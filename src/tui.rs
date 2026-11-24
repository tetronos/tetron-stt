//! Standalone TUI: live text as you speak, history of committed text, and
//! keys to toggle listening and switch mode. Committed text is typed into the
//! focused X11 window (or copied to the clipboard), so the TUI can sit in a
//! corner while another window has focus; a global hotkey running
//! `tetron-stt toggle` / `tetron-stt mode` drives it from anywhere.

use crate::engine::{Engine, Event, Input, Mode, Opts, State};
use crate::output;
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event as TEvent, KeyCode, KeyEventKind, KeyModifiers, MouseButton,
    MouseEventKind,
};
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
    fn short(self) -> &'static str {
        match self {
            Out::Type => "type",
            Out::Copy => "copy",
            Out::Show => "show",
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

#[derive(Clone, Copy, PartialEq, Debug)]
enum Action {
    Toggle,
    Mode,
    Output,
    Copy,
    Clear,
    Quit,
}

struct Button {
    x: u16,
    y: u16,
    label: String,
    action: Action,
}

/// The footer buttons on the last row. Drawing and hit-testing both use this,
/// so they can never disagree. Quit is always shown, pinned to the right edge.
/// When the row is too narrow, labels drop their key hints first, then the
/// least needed buttons go (Clear, Copy last, Output); keys still work.
fn buttons(ui: &Ui, w: u16, h: u16) -> Vec<Button> {
    let toggle = if ui.state == State::Listening { "■ Stop" } else { "● Start" };
    let full = [
        (Action::Toggle, format!(" {toggle} [space] ")),
        (Action::Mode, format!(" Mode: {} [m] ", ui.mode.as_str())),
        (Action::Output, format!(" Output: {} [o] ", ui.out.short())),
        (Action::Copy, " Copy last [c] ".to_string()),
        (Action::Clear, " Clear [x] ".to_string()),
    ];
    let short = [
        (Action::Toggle, format!(" {toggle} ")),
        (Action::Mode, format!(" {} ", ui.mode.as_str())),
        (Action::Output, format!(" {} ", ui.out.short())),
        (Action::Copy, " Copy ".to_string()),
        (Action::Clear, " Clear ".to_string()),
    ];
    let width = |l: &str| l.chars().count() as u16;
    let row_len = |set: &[(Action, String)], quit: &str| set.iter().map(|(_, l)| width(l) + 1).sum::<u16>() + width(quit);
    let (mut set, quit) = if row_len(&full, " Quit [q] ") <= w {
        (full.to_vec(), " Quit [q] ")
    } else {
        (short.to_vec(), " Quit ")
    };
    // drop from the end (Clear, Copy, Output) until the row fits; Toggle and
    // Mode stay as long as there is any room
    while set.len() > 2 && row_len(&set, quit) > w {
        set.pop();
    }
    let y = h.saturating_sub(1);
    let qx = w.saturating_sub(width(quit));
    let mut v = Vec::new();
    let mut x = 0u16;
    for (action, label) in set {
        if x + width(&label) >= qx {
            break;
        }
        x += width(&label) + 1;
        v.push(Button { x: x - width(&label) - 1, y, label, action });
    }
    v.push(Button { x: qx, y, label: quit.to_string(), action: Action::Quit });
    v
}

fn hit(ui: &Ui, w: u16, h: u16, col: u16, row: u16) -> Option<Action> {
    buttons(ui, w, h)
        .into_iter()
        .find(|b| row == b.y && col >= b.x && col < b.x + b.label.chars().count() as u16)
        .map(|b| b.action)
}

impl Ui {
    /// Run one action from a key or a click; true means quit.
    fn apply(&mut self, a: Action, tx: &Sender<Input>) -> bool {
        match a {
            Action::Quit => return true,
            Action::Toggle => {
                let _ = tx.send(Input::Toggle);
            }
            Action::Mode => {
                let _ = tx.send(Input::Mode(None));
            }
            Action::Output => {
                self.out = match self.out {
                    Out::Type => Out::Copy,
                    Out::Copy => Out::Show,
                    Out::Show if self.typer != output::Typer::None => Out::Type,
                    Out::Show => Out::Copy,
                };
            }
            Action::Copy => {
                if let Some(l) = self.history.last() {
                    output::osc52_copy(l.trim_end_matches(" ⏎").trim_end());
                    self.status = "last text copied to clipboard".into();
                }
            }
            Action::Clear => self.history.clear(),
        }
        false
    }
}

struct Ui {
    state: State,
    mode: Mode,
    out: Out,
    partial: String,
    history: Vec<String>,
    status: String,
    typer: output::Typer,
    own_win: Option<String>,
    /// Button under a mouse press, drawn pressed until release.
    pressed: Option<Action>,
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
        // footer: clickable buttons, each labelled with its key
        for b in buttons(self, w as u16, h as u16) {
            let active = self.pressed == Some(b.action);
            let (fg, bg) = match b.action {
                Action::Toggle if self.state == State::Listening => (Color::White, Color::Red),
                Action::Toggle => (Color::Black, Color::Green),
                _ => (Color::White, Color::DarkGrey),
            };
            queue!(o, cursor::MoveTo(b.x, b.y), SetForegroundColor(fg), SetBackgroundColor(bg))?;
            if active {
                queue!(o, SetAttribute(Attribute::Reverse))?;
            }
            queue!(o, Print(&b.label), SetAttribute(Attribute::Reset), ResetColor)?;
        }
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
            let hint = if self.typer != output::Typer::None {
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
                // tetron-wm checks the focus itself (own window: its clipboard).
                let active = if self.typer == output::Typer::X11 { output::active_window() } else { None };
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
    let how = output::typer();
    let mut ui = Ui {
        state: State::Idle,
        mode: o.mode,
        out: if how != output::Typer::None { Out::Type } else { Out::Copy },
        partial: String::new(),
        history: Vec::new(),
        status: String::new(),
        typer: how,
        own_win: if how == output::Typer::X11 { output::own_window() } else { None },
        pressed: None,
    };
    let (etx, erx) = std::sync::mpsc::channel();
    let tx = Engine::spawn(o, etx);
    let usr1 = Arc::new(AtomicBool::new(false));
    let usr2 = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGUSR1, usr1.clone()).map_err(|e| e.to_string())?;
    signal_hook::flag::register(signal_hook::consts::SIGUSR2, usr2.clone()).map_err(|e| e.to_string())?;
    let pf = pid_file();
    let _ = std::fs::write(&pf, std::process::id().to_string());
    // typing runs on its own thread so a slow typer never stalls the UI;
    // the channel keeps segments in order
    let (typer, trx) = std::sync::mpsc::channel::<(String, bool)>();
    std::thread::spawn(move || {
        for (t, enter) in trx {
            output::type_text(how, &t, enter);
        }
    });
    if autostart {
        let _ = tx.send(Input::Start);
    }

    let mut out = std::io::stdout();
    terminal::enable_raw_mode().map_err(|e| e.to_string())?;
    execute!(out, terminal::EnterAlternateScreen, cursor::Hide, EnableMouseCapture).map_err(|e| e.to_string())?;
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
                        let act = match k.code {
                            _ if ctrl_c => Some(Action::Quit),
                            KeyCode::Char('q') | KeyCode::Esc => Some(Action::Quit),
                            KeyCode::Char(' ') | KeyCode::Enter => Some(Action::Toggle),
                            KeyCode::Char('m') => Some(Action::Mode),
                            KeyCode::Char('o') => Some(Action::Output),
                            KeyCode::Char('c') => Some(Action::Copy),
                            KeyCode::Char('x') => Some(Action::Clear),
                            _ => None,
                        };
                        if let Some(a) = act
                            && ui.apply(a, &tx)
                        {
                            break;
                        }
                        dirty = true;
                    }
                    TEvent::Mouse(m) => {
                        // act on release over the same button that was pressed,
                        // like a GUI button; moving off before release cancels
                        let (w, h) = terminal::size()?;
                        let hit = hit(&ui, w, h, m.column, m.row);
                        match m.kind {
                            MouseEventKind::Down(MouseButton::Left) => {
                                ui.pressed = hit;
                                dirty = true;
                            }
                            MouseEventKind::Up(MouseButton::Left) => {
                                let p = ui.pressed.take();
                                dirty = true;
                                if let Some(a) = hit
                                    && p == Some(a)
                                    && ui.apply(a, &tx)
                                {
                                    break;
                                }
                            }
                            _ => {}
                        }
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
                        if ui.typer == output::Typer::TetronWm {
                            output::report_state(s.as_str(), m.as_str());
                        }
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
    if ui.typer == output::Typer::TetronWm {
        output::report_state("idle", ui.mode.as_str());
    }
    let _ = execute!(out, DisableMouseCapture, cursor::Show, terminal::LeaveAlternateScreen);
    let _ = terminal::disable_raw_mode();
    if std::fs::read_to_string(&pf).ok().as_deref() == Some(&std::process::id().to_string()) {
        let _ = std::fs::remove_file(&pf);
    }
    for l in &ui.history {
        println!("{l}");
    }
    res.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ui() -> Ui {
        Ui {
            state: State::Idle,
            mode: Mode::Segment,
            out: Out::Copy,
            partial: String::new(),
            history: Vec::new(),
            status: String::new(),
            typer: output::Typer::None,
            own_win: None,
            pressed: None,
        }
    }

    #[test]
    fn every_drawn_button_is_hit_at_its_cells() {
        let u = ui();
        let bs = buttons(&u, 120, 20);
        assert_eq!(bs.len(), 6);
        for b in &bs {
            let len = b.label.chars().count() as u16;
            assert_eq!(hit(&u, 120, 20, b.x, b.y), Some(b.action));
            assert_eq!(hit(&u, 120, 20, b.x + len - 1, b.y), Some(b.action));
            assert_eq!(hit(&u, 120, 20, b.x + len, b.y), None, "gap after {:?}", b.action);
            assert_eq!(hit(&u, 120, 20, b.x, b.y - 1), None);
        }
    }

    #[test]
    fn quit_always_visible_at_right_edge() {
        let u = ui();
        for w in [20u16, 40, 60, 80, 120] {
            let bs = buttons(&u, w, 20);
            let q = bs.last().unwrap();
            assert_eq!(q.action, Action::Quit, "width {w}");
            assert_eq!(q.x + q.label.chars().count() as u16, w, "width {w}");
            assert_eq!(hit(&u, w, 20, w - 2, 19), Some(Action::Quit), "width {w}");
            // no overlap between neighbours
            for p in bs.windows(2) {
                assert!(p[0].x + p[0].label.chars().count() as u16 <= p[1].x, "overlap at width {w}");
            }
        }
        assert_eq!(buttons(&u, 40, 20)[0].action, Action::Toggle);
    }
}
