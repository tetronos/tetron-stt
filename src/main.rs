//! tetron-stt: local CPU speech-to-text dictation.

mod engine;
mod filter;
mod models;
mod output;
mod serve;
mod tui;

use engine::{Event, Input, Mode, Opts, State};

const HELP: &str = "\
tetron-stt: local speech-to-text dictation on the CPU (no GPU, no cloud)

Usage:
  tetron-stt [options]            TUI: live text, toggle with space, m switches mode
  tetron-stt serve [options]      JSON-lines provider on stdin/stdout (used by tetron-wm)
  tetron-stt toggle               toggle listening in the running TUI (bind to a global hotkey)
  tetron-stt mode                 switch segment/final in the running TUI
  tetron-stt models status [--json]
  tetron-stt models download [--json]   explicit download, ~0.96 GB, resumable, sha256 checked
  tetron-stt models path          print the models directory
  tetron-stt doctor               check recorder, models, typing tool, threads
  tetron-stt transcribe FILE      final-model transcript of an audio file (needs ffmpeg)

Modes:
  segment   text is committed at each pause while you speak (default)
  final     text is shown live, committed once on stop after a more accurate final pass

Options:
  --mode segment|final   starting mode
  --threads N            CPU threads per model (default: min(4, cores))
  --pause SECONDS        silence that ends a segment (default 1.2)
  --enter-word WORDS     trailing phrase that presses Enter (default \"make it so\", \"\" disables)
  --idle-unload SECONDS  free the models after this long idle (default 600, 0 = never)
  --file WAV             play a file in real time instead of the mic; starts listening
  --start                start listening immediately
  --json                 JSON output for models status/download

Spoken symbols: \"slash\" /, \"underscore\" _, \"dot\" ., \"dash\"/\"hyphen\" -.

Environment:
  TETRON_STT_MODELS      models directory (default ~/.local/share/tetron-stt/models)
  TETRON_STT_AUDIO_CMD   recorder writing raw s16le mono 16 kHz to stdout
                         (default: first of arecord, pw-record, rec)
";

fn die(msg: &str) -> ! {
    eprintln!("tetron-stt: {msg}");
    std::process::exit(2)
}

struct Args {
    cmd: Vec<String>,
    opts: Opts,
    start: bool,
    json: bool,
}

fn parse() -> Args {
    let mut opts = Opts {
        models_dir: models::models_dir(),
        threads: engine::default_threads(),
        pause: 1.2,
        enter_word: "make it so".into(),
        audio_cmd: engine::default_audio_cmd(),
        mode: Mode::Segment,
        idle_unload: 600,
    };
    let (mut cmd, mut start, mut json) = (Vec::new(), false, false);
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        let mut val = || it.next().unwrap_or_else(|| die(&format!("{a} needs a value")));
        match a.as_str() {
            "-h" | "--help" | "help" => {
                print!("{HELP}");
                std::process::exit(0)
            }
            "-V" | "--version" => {
                println!("tetron-stt {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0)
            }
            "--mode" => opts.mode = Mode::parse(&val()).unwrap_or_else(|| die("--mode is segment or final")),
            "--threads" => opts.threads = val().parse().unwrap_or_else(|_| die("--threads needs a number")),
            "--pause" => opts.pause = val().parse().unwrap_or_else(|_| die("--pause needs seconds")),
            "--enter-word" => opts.enter_word = val(),
            "--idle-unload" => opts.idle_unload = val().parse().unwrap_or_else(|_| die("--idle-unload needs seconds")),
            "--file" => {
                opts.audio_cmd = engine::file_audio_cmd(&val());
                start = true;
            }
            "--start" => start = true,
            "--json" => json = true,
            s if s.starts_with('-') => die(&format!("unknown option {s} (see --help)")),
            _ => cmd.push(a),
        }
    }
    Args { cmd, opts, start, json }
}

fn doctor(o: &Opts) -> bool {
    let mut ok = true;
    let mut check = |good: bool, what: String| {
        println!("{}  {what}", if good { " ok " } else { "FAIL" });
        ok &= good;
    };
    check(engine::in_path(&o.audio_cmd[0]), format!("recorder: {}", o.audio_cmd.join(" ")));
    check(models::all_installed(&o.models_dir), format!("models in {}", o.models_dir.display()));
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    check(o.threads >= 1, format!("threads: {} per model ({cores} cores)", o.threads));
    println!(
        "info  typing: {}",
        if output::x11_available() { "xdotool on X11" } else { "no X11 + xdotool: the TUI copies to the clipboard instead" }
    );
    println!("info  ffmpeg (--file, transcribe): {}", if engine::in_path("ffmpeg") { "found" } else { "missing" });
    if !engine::in_path(&o.audio_cmd[0]) {
        println!("hint: install alsa-utils (arecord), pipewire (pw-record) or sox (rec)");
    }
    if !models::all_installed(&o.models_dir) {
        println!("hint: tetron-stt models download");
    }
    ok
}

/// Final-model transcript of a whole file, decoded as fast as the CPU allows
/// (no real-time pacing). Prints the text, or nothing for silence.
fn transcribe(mut o: Opts, file: &str) -> Result<(), String> {
    o.audio_cmd = engine::file_audio_cmd(file);
    o.audio_cmd.retain(|a| a != "-re");
    o.mode = Mode::Final;
    o.enter_word = String::new();
    let (etx, erx) = std::sync::mpsc::channel();
    let tx = engine::Engine::spawn(o, etx);
    let _ = tx.send(Input::Start);
    let mut started = false;
    for e in erx {
        match e {
            Event::Commit { text, .. } => println!("{text}"),
            Event::Error(t) => return Err(t),
            Event::State(State::Listening, _) => started = true,
            Event::State(State::Idle, _) if started => break,
            _ => {}
        }
    }
    Ok(())
}

fn main() {
    let a = parse();
    let c: Vec<&str> = a.cmd.iter().map(String::as_str).collect();
    let r = match c.as_slice() {
        [] | ["tui"] => tui::run(a.opts, a.start),
        ["serve"] => {
            serve::run(a.opts, a.start);
            Ok(())
        }
        ["toggle"] => tui::signal_running(libc::SIGUSR1),
        ["mode"] => tui::signal_running(libc::SIGUSR2),
        ["models"] | ["models", "status"] => {
            models::status(a.json);
            Ok(())
        }
        ["models", "download"] => models::download(a.json),
        ["models", "path"] => {
            println!("{}", a.opts.models_dir.display());
            Ok(())
        }
        ["doctor"] => {
            if doctor(&a.opts) {
                Ok(())
            } else {
                std::process::exit(1)
            }
        }
        ["transcribe", f] => transcribe(a.opts, f),
        _ => Err(format!("unknown command: {} (see --help)", a.cmd.join(" "))),
    };
    if let Err(e) = r {
        if a.json {
            println!("{}", serde_json::json!({"error": e}));
        }
        die(&e);
    }
}
