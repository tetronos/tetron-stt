//! The dictation engine: one thread that owns the recognizers and the audio
//! recorder, fed by an `Input` channel and reporting through an `Event` channel.
//!
//! Live: Nemotron streaming decodes 100 ms chunks and reports the partial text.
//! Segment mode commits each segment at a pause (endpoint). Final mode commits
//! nothing while speaking; on stop, Parakeet re-transcribes the whole utterance
//! (more accurate, about 1 s after stop). The mode is fixed per utterance, so a
//! switch while listening applies from the next one.
//!
//! The recorder runs only while listening (the mic is closed otherwise), and
//! the models load on first start and unload after `idle_unload` seconds idle.

use crate::filter::postfilter;
use crate::models;
use sherpa_onnx::{
    OfflineRecognizer, OfflineRecognizerConfig, OnlineRecognizer, OnlineRecognizerConfig, OnlineStream,
};
use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

pub const SR: i32 = 16000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Segment,
    Final,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Segment => "segment",
            Mode::Final => "final",
        }
    }
    pub fn parse(s: &str) -> Option<Mode> {
        match s {
            "segment" | "live" => Some(Mode::Segment),
            "final" => Some(Mode::Final),
            _ => None,
        }
    }
    pub fn other(self) -> Mode {
        match self {
            Mode::Segment => Mode::Final,
            Mode::Final => Mode::Segment,
        }
    }
}

pub enum Input {
    Start,
    Stop,
    Toggle,
    /// None switches to the other mode.
    Mode(Option<Mode>),
    Quit,
    Audio(Vec<f32>),
    AudioEnd(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum State {
    Idle,
    Loading,
    Listening,
    /// Final pass running after stop.
    Finishing,
}

impl State {
    pub fn as_str(&self) -> &'static str {
        match self {
            State::Idle => "idle",
            State::Loading => "loading",
            State::Listening => "listening",
            State::Finishing => "finishing",
        }
    }
}

#[derive(Clone, Debug)]
pub enum Event {
    State(State, Mode),
    Partial(String),
    Commit { text: String, enter: bool },
    Error(String),
    Info(String),
}

#[derive(Clone)]
pub struct Opts {
    pub models_dir: PathBuf,
    pub threads: i32,
    pub pause: f32,
    pub enter_word: String,
    pub audio_cmd: Vec<String>,
    pub mode: Mode,
    pub idle_unload: u64,
}

pub fn default_threads() -> i32 {
    std::thread::available_parallelism().map(|n| n.get().min(4) as i32).unwrap_or(2)
}

pub fn in_path(cmd: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(cmd).is_file()))
        .unwrap_or(false)
}

/// First available recorder producing raw s16le mono 16 kHz on stdout.
/// TETRON_STT_AUDIO_CMD overrides (split on whitespace).
pub fn default_audio_cmd() -> Vec<String> {
    if let Ok(c) = std::env::var("TETRON_STT_AUDIO_CMD") {
        return c.split_whitespace().map(str::to_string).collect();
    }
    let cands: [&[&str]; 3] = [
        &["arecord", "-q", "-t", "raw", "-f", "S16_LE", "-r", "16000", "-c", "1"],
        &["pw-record", "--rate", "16000", "--channels", "1", "--format", "s16", "-"],
        &["rec", "-q", "-t", "raw", "-r", "16000", "-c", "1", "-b", "16", "-e", "signed-integer", "-"],
    ];
    let pick = cands.iter().find(|c| in_path(c[0])).unwrap_or(&cands[0]);
    pick.iter().map(|s| s.to_string()).collect()
}

/// Replays a file in real time through the same path as the mic (tests, demos).
pub fn file_audio_cmd(file: &str) -> Vec<String> {
    ["ffmpeg", "-v", "error", "-re", "-i", file, "-f", "s16le", "-ar", "16000", "-ac", "1", "-"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

struct Models {
    live: OnlineRecognizer,
    fin: OfflineRecognizer,
}

fn load(o: &Opts) -> Result<Models, String> {
    if !models::all_installed(&o.models_dir) {
        return Err(format!("models missing in {}; run: tetron-stt models download", o.models_dir.display()));
    }
    let p = |role: &str, f: &str| Some(models::model_path(&o.models_dir, role).join(f).to_string_lossy().into_owned());
    let mut c = OnlineRecognizerConfig::default();
    c.model_config.transducer.encoder = p("live", "encoder.int8.onnx");
    c.model_config.transducer.decoder = p("live", "decoder.int8.onnx");
    c.model_config.transducer.joiner = p("live", "joiner.int8.onnx");
    c.model_config.tokens = p("live", "tokens.txt");
    c.model_config.num_threads = o.threads;
    c.model_config.model_type = Some("nemo_transducer".into());
    c.decoding_method = Some("greedy_search".into());
    let live = OnlineRecognizer::create(&c).ok_or("could not load the live model")?;
    let mut f = OfflineRecognizerConfig::default();
    f.model_config.transducer.encoder = p("final", "encoder.int8.onnx");
    f.model_config.transducer.decoder = p("final", "decoder.int8.onnx");
    f.model_config.transducer.joiner = p("final", "joiner.int8.onnx");
    f.model_config.tokens = p("final", "tokens.txt");
    f.model_config.num_threads = o.threads;
    f.model_config.model_type = Some("nemo_transducer".into());
    f.decoding_method = Some("greedy_search".into());
    let fin = OfflineRecognizer::create(&f).ok_or("could not load the final model")?;
    Ok(Models { live, fin })
}

fn spawn_recorder(cmd: &[String], tx: Sender<Input>) -> Result<Child, String> {
    let mut child = Command::new(&cmd[0])
        .args(&cmd[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("{}: {e}", cmd[0]))?;
    let mut out = child.stdout.take().unwrap();
    let name = cmd[0].clone();
    std::thread::spawn(move || {
        let mut buf = vec![0u8; (SR as usize) * 2 / 10]; // 100 ms of s16
        loop {
            match out.read_exact(&mut buf) {
                Ok(()) => {
                    let a = buf.as_chunks::<2>().0.iter().map(|b| i16::from_le_bytes(*b) as f32 / 32768.0).collect();
                    if tx.send(Input::Audio(a)).is_err() {
                        return;
                    }
                }
                Err(_) => {
                    let _ = tx.send(Input::AudioEnd(name));
                    return;
                }
            }
        }
    });
    Ok(child)
}

struct Utt {
    stream: OnlineStream,
    audio: Vec<f32>,
    mode: Mode,
    rec: Child,
    /// Bumped per recorder so a late AudioEnd from a killed one is ignored.
    epoch: u64,
    /// Bytes of the stream's text already committed (segment mode).
    committed: usize,
    /// Text length and the audio time (s) it last changed, for pause detection.
    last_len: usize,
    changed_at: f32,
}

/// Silence after which a fully committed stream is reset to keep it short.
const RESET_AFTER: f32 = 4.0;

pub struct Engine {
    o: Opts,
    tx: Sender<Input>,
    ev: Sender<Event>,
    models: Option<Models>,
    utt: Option<Utt>,
    mode: Mode,
    last_active: Instant,
    epoch: u64,
}

impl Engine {
    /// Spawns the engine thread; returns the input sender (clone it for the
    /// UI, signals and tests).
    pub fn spawn(o: Opts, ev: Sender<Event>) -> Sender<Input> {
        let (tx, rx) = std::sync::mpsc::channel();
        let e = Engine { mode: o.mode, o, tx: tx.clone(), ev, models: None, utt: None, last_active: Instant::now(), epoch: 0 };
        std::thread::spawn(move || e.run(rx));
        tx
    }

    fn state(&self, s: State) {
        let _ = self.ev.send(Event::State(s, self.mode));
    }

    fn run(mut self, rx: Receiver<Input>) {
        self.state(State::Idle);
        loop {
            let msg = match rx.recv_timeout(Duration::from_secs(1)) {
                Ok(m) => m,
                Err(RecvTimeoutError::Timeout) => {
                    if self.utt.is_none()
                        && self.models.is_some()
                        && self.o.idle_unload > 0
                        && self.last_active.elapsed().as_secs() >= self.o.idle_unload
                    {
                        self.models = None;
                        let _ = self.ev.send(Event::Info("models unloaded (idle)".into()));
                    }
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => break,
            };
            match msg {
                Input::Start => self.start(),
                Input::Stop => self.stop(),
                Input::Toggle => {
                    if self.utt.is_some() {
                        self.stop()
                    } else {
                        self.start()
                    }
                }
                Input::Mode(m) => {
                    self.mode = m.unwrap_or(self.mode.other());
                    let s = if self.utt.is_some() { State::Listening } else { State::Idle };
                    self.state(s);
                }
                Input::Quit => {
                    self.stop();
                    break;
                }
                Input::Audio(a) => self.audio(a),
                Input::AudioEnd(name) => {
                    // a recorder that ends on its own: end of a test file, or a mic error
                    if let Some(u) = &self.utt
                        && u.epoch == self.epoch
                    {
                        let _ = self.ev.send(Event::Info(format!("{name} ended")));
                        self.stop();
                    }
                }
            }
        }
    }

    fn start(&mut self) {
        if self.utt.is_some() {
            return;
        }
        if self.models.is_none() {
            self.state(State::Loading);
            let t = Instant::now();
            match load(&self.o) {
                Ok(m) => {
                    self.models = Some(m);
                    let _ = self.ev.send(Event::Info(format!("models loaded in {:.1}s", t.elapsed().as_secs_f32())));
                }
                Err(e) => {
                    let _ = self.ev.send(Event::Error(e));
                    self.state(State::Idle);
                    return;
                }
            }
        }
        self.epoch += 1;
        let rec = match spawn_recorder(&self.o.audio_cmd, self.tx.clone()) {
            Ok(r) => r,
            Err(e) => {
                let _ = self.ev.send(Event::Error(format!("recorder: {e}")));
                self.state(State::Idle);
                return;
            }
        };
        let stream = self.models.as_ref().unwrap().live.create_stream();
        self.utt = Some(Utt { stream, audio: Vec::new(), mode: self.mode, rec, epoch: self.epoch, committed: 0, last_len: 0, changed_at: 0.0 });
        self.last_active = Instant::now();
        self.state(State::Listening);
    }

    fn commit(&self, raw: &str, segment: bool) {
        let (mut text, enter) = postfilter(raw, &self.o.enter_word);
        if text.is_empty() && !enter {
            return;
        }
        // the next segment continues the sentence; after a path or identifier
        // symbol it continues the word ("tests/" + "main.rs")
        if segment && !enter && !text.is_empty() && !text.ends_with(['/', '_', '.', '-']) {
            text.push(' ');
        }
        let _ = self.ev.send(Event::Commit { text, enter });
    }

    fn audio(&mut self, a: Vec<f32>) {
        let (Some(u), Some(m)) = (&mut self.utt, &self.models) else { return };
        if u.epoch != self.epoch {
            return;
        }
        u.audio.extend_from_slice(&a);
        u.stream.accept_waveform(SR, &a);
        while m.live.is_ready(&u.stream) {
            m.live.decode(&u.stream);
        }
        // Segments are cut here, not with sherpa's endpoint + reset: the model
        // emits words up to ~1 s after they are spoken, so its endpoint can fire
        // mid-word and the reset drops the pending audio. The transducer's text
        // only grows, so a segment is the text added since the last commit,
        // committed once the text has not changed for `pause` seconds.
        let raw = m.live.get_result(&u.stream).map(|r| r.text).unwrap_or_default();
        let now = u.audio.len() as f32 / SR as f32;
        if raw.len() != u.last_len {
            u.last_len = raw.len();
            u.changed_at = now;
        }
        let new = raw.get(u.committed..).unwrap_or("").trim().to_string();
        if std::env::var_os("TETRON_STT_DEBUG").is_some() {
            eprintln!("{now:6.1}s quiet={:.1} [{new}]", now - u.changed_at);
        }
        let quiet = now - u.changed_at;
        let mut commit = None;
        if u.mode == Mode::Segment && quiet >= self.o.pause && !new.is_empty() {
            u.committed = raw.len();
            commit = Some(new.clone());
        }
        if u.mode == Mode::Segment && quiet >= RESET_AFTER && !raw.is_empty() && u.committed == raw.len() {
            m.live.reset(&u.stream);
            (u.committed, u.last_len) = (0, 0);
        }
        let partial = if commit.is_some() { String::new() } else { new };
        let _ = self.ev.send(Event::Partial(partial));
        if let Some(t) = commit {
            self.commit(&t, true);
        }
    }

    fn stop(&mut self) {
        let Some(mut u) = self.utt.take() else { return };
        let _ = u.rec.kill();
        let _ = u.rec.wait();
        self.epoch += 1;
        let m = self.models.as_ref().unwrap();
        self.state(State::Finishing);
        match u.mode {
            Mode::Segment => {
                // flush what is left of the current segment
                // 1.5 s of silence: the 560 ms chunk plus lookahead must see
                // past the last word, or it is dropped
                u.stream.accept_waveform(SR, &vec![0.0; SR as usize * 3 / 2]);
                u.stream.input_finished();
                while m.live.is_ready(&u.stream) {
                    m.live.decode(&u.stream);
                }
                let raw = m.live.get_result(&u.stream).map(|r| r.text).unwrap_or_default();
                let tail = raw.get(u.committed..).unwrap_or("").trim().to_string();
                self.commit(&tail, false);
            }
            Mode::Final => {
                if !u.audio.is_empty() {
                    let s = m.fin.create_stream();
                    s.accept_waveform(SR, &u.audio);
                    m.fin.decode(&s);
                    let text = s.get_result().map(|r| r.text).unwrap_or_default();
                    self.commit(&text, false);
                }
            }
        }
        let _ = self.ev.send(Event::Partial(String::new()));
        self.last_active = Instant::now();
        self.state(State::Idle);
    }
}
