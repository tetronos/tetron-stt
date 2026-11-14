//! `tetron-stt serve`: the provider protocol, one JSON object per line.
//!
//! stdin:  {"cmd":"start"} {"cmd":"stop"} {"cmd":"toggle"} {"cmd":"mode"}
//!         {"cmd":"mode","mode":"final"|"segment"} {"cmd":"quit"}
//! stdout: {"state":"idle"|"loading"|"listening"|"finishing","mode":"segment"}
//!         {"partial":"text so far"} {"commit":"text","enter":false}
//!         {"error":"..."} {"info":"..."}
//! The consumer (tetron-wm) pastes each commit into the focused app and
//! presses Enter when "enter" is true. stdin EOF quits.

use crate::engine::{Engine, Event, Input, Mode, Opts};
use serde::Deserialize;
use std::io::{BufRead, Write};

#[derive(Deserialize)]
struct Cmd {
    cmd: String,
    #[serde(default)]
    mode: Option<String>,
}

pub fn event_json(e: &Event) -> serde_json::Value {
    use serde_json::json;
    match e {
        Event::State(s, m) => json!({"state": s.as_str(), "mode": m.as_str()}),
        Event::Partial(t) => json!({"partial": t}),
        Event::Commit { text, enter } => json!({"commit": text, "enter": enter}),
        Event::Error(t) => json!({"error": t}),
        Event::Info(t) => json!({"info": t}),
    }
}

pub fn run(o: Opts, autostart: bool) {
    let (etx, erx) = std::sync::mpsc::channel();
    let tx = Engine::spawn(o, etx);
    if autostart {
        let _ = tx.send(Input::Start);
    }
    let itx = tx.clone();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if line.trim().is_empty() {
                continue;
            }
            let input = match serde_json::from_str::<Cmd>(&line) {
                Ok(c) => match c.cmd.as_str() {
                    "start" => Input::Start,
                    "stop" => Input::Stop,
                    "toggle" => Input::Toggle,
                    "mode" => Input::Mode(c.mode.as_deref().and_then(Mode::parse)),
                    "quit" => Input::Quit,
                    other => {
                        println!("{}", serde_json::json!({"error": format!("unknown cmd {other}")}));
                        continue;
                    }
                },
                Err(e) => {
                    println!("{}", serde_json::json!({"error": format!("bad command: {e}")}));
                    continue;
                }
            };
            if itx.send(input).is_err() {
                return;
            }
        }
        let _ = itx.send(Input::Quit);
    });
    let mut out = std::io::stdout().lock();
    let mut last_partial = String::new();
    for e in erx {
        if let Event::Partial(t) = &e {
            if *t == last_partial {
                continue; // the engine reports every 100 ms; send changes only
            }
            last_partial = t.clone();
        }
        if writeln!(out, "{}", event_json(&e)).and_then(|_| out.flush()).is_err() {
            break;
        }
    }
}
