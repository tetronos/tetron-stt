//! Standalone TUI: live text as you speak, history of committed text, and
//! keys to toggle listening and switch mode. Committed text is typed into the
//! focused X11 window (or copied to the clipboard), so the TUI can sit in a
//! corner while another window has focus; a global hotkey running
//! `tetron-stt toggle` / `tetron-stt mode` drives it from anywhere.

use crate::engine::{Engine, Event, Input, Mode, Opts, State};
use crate::output;
use crossterm::event::{self, Event as TEvent, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::style::{Attribute, Color, Print, ResetColor, SetAttribute, SetBackgroundColor, SetForegroundColor};
use crossterm::terminal::{self, ClearType};
use crossterm::{cursor, execute, queue};
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::time::Duration;

#[derive(Clone, Copy, PartialEq)]
enum Out {
    Type,
    Copy,
    Show,
}

impl Out {
    fn label(self) -> &'static str {
        match self {
            Out::Type => "type into focused window",
            Out::Copy => "copy to clipboard",
            Out::Show => "show only",
        }
    }
}

pub fn pid_file() -> std::path::PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(d) => std::path::PathBuf::from(d).join("tetron-stt.pid"),
        None => std::env::temp_dir().join(format!("tetron-stt-{}.pid", unsafe { libc::getuid() })),
    }
}

/// `tetron-stt toggle` / `tetron-stt mode`: signal the running TUI.
pub fn signal_running(sig: i32) -> Result<(), String> {
    let p = pid_file();
    let pid: i32 = std::fs::read_to_string(&p)
        .map_err(|_| format!("no running tetron-stt TUI ({} missing)", p.display()))?
        .trim()
        .parse()
        .map_err(|_| format!("bad pid in {}", p.display()))?;
    if unsafe { libc::kill(pid, sig) } != 0 {
        return Err(format!("tetron-stt pid {pid} is not running"));
    }
    Ok(())
}

struct Ui {
    state: State,
    mode: Mode,
    out: Out,
    partial: String,
    history: Vec<String>,
    status: String,
    x11: bool,
    own_win: Option<String>,
}

fn wrap(s: &str, w: usize) -> Vec<String> {
    let w = w.max(1);
    let chars: Vec<char> = s.chars().collect();
    if chars.is_empty() {
        return vec![String::new()];
    }
    chars.chunks(w).map(|c| c.iter().collect()).collect()
}

impl Ui {
    fn draw(&self, o: &mut impl Write) -> std::io::Result<()> {
        let (w, h) = terminal::size()?;
        let (w, h) = (w as usize, h as usize);
        queue!(o, terminal::Clear(ClearType::All), cursor::MoveTo(0, 0))?;
        let (badge, bg) = match self.state {
            State::Listening => (" ● LISTENING ", Color::Red),
            State::Loading => (" LOADING ", Color::DarkYellow),
            State::Finishing => (" FINISHING ", Color::DarkYellow),
            State::Idle => (" ○ off ", Color::DarkGrey),
        };
        queue!(
            o,
            SetAttribute(Attribute::Bold),
            Print(" tetron-stt "),
            SetBackgroundColor(bg),
            SetForegroundColor(Color::White),
            Print(badge),
            ResetColor,
            SetAttribute(Attribute::Reset),
            Print(format!("  mode: {}  output: {}", self.mode.as_str(), self.out.label())),
        )?;
        // footer: keys + status
        let keys = " space toggle  m mode  o output  c copy last  x clear  q quit";
        queue!(o, cursor::MoveTo(0, (h - 1) as u16), SetAttribute(Attribute::Reverse), Print(format!("{keys:w$}").chars().take(w).collect::<String>()), SetAttribute(Attribute::Reset))?;
        if !self.status.is_empty() {
            queue!(o, cursor::MoveTo(0, (h - 2) as u16), SetForegroundColor(Color::DarkGrey), Print(self.status.chars().take(w).collect::<String>()), ResetColor)?;
        }
        // live partial, up to 3 lines above the status line
        let mut plines = wrap(&format!("> {}", self.partial), w);
        if plines.len() > 3 {
            plines.drain(..plines.len() - 3);
        }
        let ptop = h.saturating_sub(3 + plines.len());
        queue!(o, cursor::MoveTo(0, (ptop.saturating_sub(1)) as u16), SetForegroundColor(Color::DarkGrey), Print("─".repeat(w)), ResetColor)?;
        for (i, l) in plines.iter().enumerate() {
            queue!(o, cursor::MoveTo(0, (ptop + i) as u16), SetForegroundColor(Color::Cyan), Print(l), ResetColor)?;
        }
        // history fills rows 2 .. separator, newest at the bottom
        let room = ptop.saturating_sub(3);
        let mut hl: Vec<String> = self.history.iter().flat_map(|s| wrap(s, w)).collect();
        if hl.len() > room {
            hl.drain(..hl.len() - room);
        }
        for (i, l) in hl.iter().enumerate() {
            queue!(o, cursor::MoveTo(0, (2 + i) as u16), Print(l))?;
        }
        if self.history.is_empty() && room > 2 {
            let hint = if self.x11 {
                "Start, then focus the target window: text is typed there. Global hotkey: tetron-stt toggle / tetron-stt mode"
            } else {
                "Start and speak; committed text is copied to the clipboard. Global hotkey: tetron-stt toggle / tetron-stt mode"
            };
            for (i, l) in wrap(hint, w).iter().enumerate() {
                queue!(o, cursor::MoveTo(0, (2 + i) as u16), SetForegroundColor(Color::DarkGrey), Print(l), ResetColor)?;
            }
        }
        o.flush()
    }

    /// Deliver a commit; returns a status note.
    fn deliver(&mut self, text: &str, enter: bool, typer: &Sender<(String, bool)>) -> String {
        let shown = format!("{text}{}", if enter { " ⏎" } else { "" });
        self.history.push(shown);
        match self.out {
            Out::Show => String::new(),
            Out::Copy => {
                output::osc52_copy(text.trim_end());
                "copied to clipboard".into()
            }
            Out::Type => {
                let active = output::active_window();
                if active.is_some() && active == self.own_win {
                    output::osc52_copy(text.trim_end());
                    "this window has focus: copied to clipboard instead of typing".into()
                } else {
                    let _ = typer.send((text.to_string(), enter));
                    String::new()
                }
            }
        }
    }
}

pub fn run(o: Opts, autostart: bool) -> Result<(), String> {
    let x11 = output::x11_available();
    let mut ui = Ui {
        state: State::Idle,
        mode: o.mode,
        out: if x11 { Out::Type } else { Out::Copy },
        partial: String::new(),
        history: Vec::new(),
        status: String::new(),
        x11,
        own_win: if x11 { output::own_window() } else { None },
    };
    let (etx, erx) = std::sync::mpsc::channel();
    let tx = Engine::spawn(o, etx);
    let usr1 = Arc::new(AtomicBool::new(false));
    let usr2 = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGUSR1, usr1.clone()).map_err(|e| e.to_string())?;
    signal_hook::flag::register(signal_hook::consts::SIGUSR2, usr2.clone()).map_err(|e| e.to_string())?;
    let pf = pid_file();
    let _ = std::fs::write(&pf, std::process::id().to_string());
    // typing runs on its own thread so a slow xdotool never stalls the UI;
    // the channel keeps segments in order
    let (typer, trx) = std::sync::mpsc::channel::<(String, bool)>();
    std::thread::spawn(move || {
        for (t, enter) in trx {
            output::xtype(&t, enter);
        }
    });
    if autostart {
        let _ = tx.send(Input::Start);
    }

    let mut out = std::io::stdout();
    terminal::enable_raw_mode().map_err(|e| e.to_string())?;
    execute!(out, terminal::EnterAlternateScreen, cursor::Hide).map_err(|e| e.to_string())?;
    let res = (|| -> std::io::Result<()> {
        let mut dirty = true;
        loop {
            if dirty {
                ui.draw(&mut out)?;
                dirty = false;
            }
            if event::poll(Duration::from_millis(50))? {
                match event::read()? {
                    TEvent::Key(k) if k.kind != KeyEventKind::Release => {
                        let ctrl_c = k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c');
                        match k.code {
                            _ if ctrl_c => break,
                            KeyCode::Char('q') | KeyCode::Esc => break,
                            KeyCode::Char(' ') | KeyCode::Enter => {
                                let _ = tx.send(Input::Toggle);
                            }
                            KeyCode::Char('m') => {
                                let _ = tx.send(Input::Mode(None));
                            }
                            KeyCode::Char('o') => {
                                ui.out = match ui.out {
                                    Out::Type => Out::Copy,
                                    Out::Copy => Out::Show,
                                    Out::Show if ui.x11 => Out::Type,
                                    Out::Show => Out::Copy,
                                };
                            }
                            KeyCode::Char('c') => {
                                if let Some(l) = ui.history.last() {
                                    output::osc52_copy(l.trim_end_matches(" ⏎").trim_end());
                                    ui.status = "last text copied to clipboard".into();
                                }
                            }
                            KeyCode::Char('x') => ui.history.clear(),
                            _ => {}
                        }
                        dirty = true;
                    }
                    TEvent::Resize(..) => dirty = true,
                    _ => {}
                }
            }
            if usr1.swap(false, Ordering::Relaxed) {
                let _ = tx.send(Input::Toggle);
            }
            if usr2.swap(false, Ordering::Relaxed) {
                let _ = tx.send(Input::Mode(None));
            }
            while let Ok(e) = erx.try_recv() {
                dirty = true;
                match e {
                    Event::State(s, m) => {
                        ui.state = s;
                        ui.mode = m;
                    }
                    Event::Partial(t) => {
                        if t == ui.partial {
                            continue;
                        }
                        ui.partial = t;
                    }
                    Event::Commit { text, enter } => ui.status = ui.deliver(&text, enter, &typer),
                    Event::Error(t) => ui.status = format!("error: {t}"),
                    Event::Info(t) => ui.status = t,
                }
            }
        }
        Ok(())
    })();
    let _ = tx.send(Input::Quit);
    let _ = execute!(out, cursor::Show, terminal::LeaveAlternateScreen);
    let _ = terminal::disable_raw_mode();
    if std::fs::read_to_string(&pf).ok().as_deref() == Some(&std::process::id().to_string()) {
        let _ = std::fs::remove_file(&pf);
    }
    for l in &ui.history {
        println!("{l}");
    }
    res.map_err(|e| e.to_string())
}
