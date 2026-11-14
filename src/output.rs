//! Where committed text goes in standalone (TUI) use: typed into the focused
//! X11 window with xdotool, or copied to the clipboard with OSC 52 (works in
//! most terminals, also over ssh). Inside tetron-wm, `serve` is used instead
//! and tetron-wm pastes into the focused app itself.

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
    fn base64_padding() {
        assert_eq!(super::base64(b"a"), "YQ==");
        assert_eq!(super::base64(b"ab"), "YWI=");
        assert_eq!(super::base64(b"abc"), "YWJj");
    }
}
