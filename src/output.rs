//! Where the TUI's committed text goes: typed into the focused window, inside a
//! tetron-wm session with `tetron-wm type` (no X11 needed; from the TUI's own
//! window tetron-wm puts it on its clipboard instead), on X11 with xdotool, else
//! copied to the clipboard with OSC 52 (works in most terminals, also over ssh).

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Duration;

fn run(args: &[&str]) -> Option<String> {
    let mut child = Command::new(args[0]).args(&args[1..]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    // xdotool can hang on a broken display; never block the UI for long
    for _ in 0..100 {
        if let Ok(Some(_)) = child.try_wait() {
            let o = child.wait_with_output().ok()?;
            return o.status.success().then(|| String::from_utf8_lossy(&o.stdout).trim().to_string());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
    None
}

/// How the TUI can type into other windows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Typer {
    /// Inside a tetron-wm session: `tetron-wm type`.
    TetronWm,
    /// X11 with xdotool.
    X11,
    /// No way to type: copy instead.
    None,
}

/// tetron-wm first (its app windows carry TETRON_WM_SESSION), then X11.
pub fn typer() -> Typer {
    if std::env::var_os("TETRON_WM_SESSION").is_some() && crate::engine::in_path("tetron-wm") {
        Typer::TetronWm
    } else if x11_available() {
        Typer::X11
    } else {
        Typer::None
    }
}

/// Type `text` (then Enter) with the given typer.
pub fn type_text(t: Typer, text: &str, enter: bool) -> bool {
    match t {
        Typer::TetronWm => wmtype("tetron-wm", text, enter),
        Typer::X11 => xtype(text, enter),
        Typer::None => false,
    }
}

/// `PROG type [--enter]` with the text on stdin (never parsed as options).
fn wmtype(prog: &str, text: &str, enter: bool) -> bool {
    let mut cmd = Command::new(prog);
    cmd.arg("type");
    if enter {
        cmd.arg("--enter");
    }
    let Ok(mut child) = cmd.stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() else { return false };
    if let Some(mut i) = child.stdin.take() {
        let _ = i.write_all(text.as_bytes());
    }
    for _ in 0..100 {
        if let Ok(Some(st)) = child.try_wait() {
            return st.success();
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    let _ = child.wait();
    false
}

pub fn x11_available() -> bool {
    std::env::var_os("DISPLAY").is_some() && crate::engine::in_path("xdotool")
}

pub fn active_window() -> Option<String> {
    run(&["xdotool", "getactivewindow"])
}

/// The window of the terminal running the TUI, so its own key presses are
/// never typed back into itself. WINDOWID when the terminal sets it, else the
/// active window at startup (the TUI was just started there).
pub fn own_window() -> Option<String> {
    std::env::var("WINDOWID").ok().filter(|s| !s.is_empty()).or_else(active_window)
}

pub fn xtype(text: &str, enter: bool) -> bool {
    let mut ok = true;
    if !text.is_empty() {
        ok &= run(&["xdotool", "type", "--clearmodifiers", "--delay", "1", "--", text]).is_some();
    }
    if enter {
        ok &= run(&["xdotool", "key", "--clearmodifiers", "Return"]).is_some();
    }
    ok
}

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::new();
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                s.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}

pub fn osc52_copy(text: &str) {
    let mut o = std::io::stdout();
    let _ = write!(o, "\x1b]52;c;{}\x07", base64(text.as_bytes()));
    let _ = o.flush();
}

#[cfg(test)]
mod tests {
    #[test]
    fn wmtype_sends_text_on_stdin_and_enter_as_a_flag() {
        use std::os::unix::fs::PermissionsExt;
        let d = std::env::temp_dir().join(format!("stt-wmtype-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let (prog, out) = (d.join("fake-tetron-wm"), d.join("out"));
        std::fs::write(&prog, format!("#!/bin/sh\necho \"$*\" > '{0}'\ncat >> '{0}'\n", out.display())).unwrap();
        std::fs::set_permissions(&prog, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(super::wmtype(prog.to_str().unwrap(), "--enter is text", true));
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "type --enter\n--enter is text");
        assert!(!super::wmtype("/nonexistent/tetron-wm", "x", false));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn base64_padding() {
        assert_eq!(super::base64(b"a"), "YQ==");
        assert_eq!(super::base64(b"ab"), "YWI=");
        assert_eq!(super::base64(b"abc"), "YWJj");
    }
}
