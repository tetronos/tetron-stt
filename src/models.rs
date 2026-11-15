//! Model catalog, location, status and download.
//!
//! Models are never fetched implicitly: only `tetron-stt models download`
//! touches the network. Archives come from the sherpa-onnx GitHub releases,
//! download into `<models>/.download/` (resumable with curl -C -), are checked
//! against the pinned sha256, unpacked with tar, then the archive is removed.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

const BASE_URL: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models";

pub struct Model {
    pub role: &'static str,
    pub name: &'static str,
    pub bytes: u64,
    pub sha256: &'static str,
}

/// Default tier: Nemotron 3.5 streaming (live partials, segment mode) and
/// Parakeet TDT 0.6B v2 (final pass). Both are int8 transducers with the same
/// file layout, and both run faster than real time on a laptop CPU.
pub const MODELS: &[Model] = &[
    Model {
        role: "live",
        name: "sherpa-onnx-nemotron-3.5-asr-streaming-0.6b-560ms-int8-2026-06-11",
        bytes: 475_271_763,
        sha256: "c6bf5e0df765f9d5b43bc9e0536d4b4b3e7d40bdf5ecf13e45f134c51c05ae3a",
    },
    Model {
        role: "final",
        name: "sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8",
        bytes: 482_440_461,
        sha256: "6fc4426b54aa549a0019b8a75b8dea59164e71790a37ac56a045ad57f915f714",
    },
];

pub const FILES: &[&str] = &["encoder.int8.onnx", "decoder.int8.onnx", "joiner.int8.onnx", "tokens.txt"];

/// Models a system image ships (the TetronOS Desktop ISO pre-seeds them here).
pub const SYSTEM_MODELS_DIR: &str = "/usr/share/tetron-stt/models";

/// The directory models are read from: TETRON_STT_MODELS if set, else the
/// system directory when it holds every model, else the user's own.
pub fn models_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("TETRON_STT_MODELS") {
        return d.into();
    }
    pick_dir(Path::new(SYSTEM_MODELS_DIR), user_models_dir())
}

fn pick_dir(system: &Path, user: PathBuf) -> PathBuf {
    if all_installed(system) { system.to_path_buf() } else { user }
}

fn user_models_dir() -> PathBuf {
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share"));
    data.join("tetron-stt/models")
}

pub fn model_path(dir: &Path, role: &str) -> PathBuf {
    let m = MODELS.iter().find(|m| m.role == role).expect("known role");
    dir.join(m.name)
}

pub fn installed(dir: &Path, m: &Model) -> bool {
    FILES.iter().all(|f| dir.join(m.name).join(f).is_file())
}

pub fn all_installed(dir: &Path) -> bool {
    MODELS.iter().all(|m| installed(dir, m))
}

pub fn status(json: bool) {
    let dir = models_dir();
    if json {
        let list: Vec<_> = MODELS
            .iter()
            .map(|m| serde_json::json!({"role": m.role, "name": m.name, "bytes": m.bytes, "installed": installed(&dir, m)}))
            .collect();
        let v = serde_json::json!({"dir": dir, "installed": all_installed(&dir), "models": list});
        println!("{v}");
        return;
    }
    println!("models dir: {}", dir.display());
    for m in MODELS {
        let st = if installed(&dir, m) { "installed" } else { "missing" };
        println!("  {:5}  {:9}  {:4} MB  {}", m.role, st, m.bytes / 1_000_000, m.name);
    }
    if !all_installed(&dir) {
        println!("run: tetron-stt models download");
    }
}

fn emit(json: bool, v: serde_json::Value, human: String) {
    if json {
        println!("{v}");
    } else {
        eprintln!("{human}");
    }
}

fn sha256(path: &Path) -> Result<String, String> {
    let tries: [(&str, &[&str]); 2] = [("sha256sum", &[]), ("shasum", &["-a", "256"])];
    for (cmd, args) in tries {
        if let Ok(o) = Command::new(cmd).args(args).arg(path).stderr(Stdio::null()).output()
            && o.status.success()
        {
            let s = String::from_utf8_lossy(&o.stdout);
            return Ok(s.split_whitespace().next().unwrap_or("").to_string());
        }
    }
    Err("neither sha256sum nor shasum found".into())
}

fn size(p: &Path) -> u64 {
    std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)
}

/// Download, verify and unpack every missing model. JSON mode prints one
/// object per line on stdout ({"model","bytes","total"} progress, then
/// {"installed":name} per model and {"done":true}); errors are {"error":...}.
pub fn download(json: bool) -> Result<(), String> {
    let dir = models_dir();
    let dl = dir.join(".download");
    std::fs::create_dir_all(&dl).map_err(|e| format!("{}: {e}", dl.display()))?;
    for m in MODELS {
        if installed(&dir, m) {
            emit(json, serde_json::json!({"installed": m.name}), format!("{}: already installed", m.name));
            continue;
        }
        let arc = dl.join(format!("{}.tar.bz2", m.name));
        if size(&arc) != m.bytes {
            let url = format!("{BASE_URL}/{}.tar.bz2", m.name);
            emit(json, serde_json::json!({"model": m.name, "url": url}), format!("downloading {url}"));
            let mut child = Command::new("curl")
                .args(["-fsSL", "--retry", "3", "-C", "-", "-o"])
                .arg(&arc)
                .arg(&url)
                .stdin(Stdio::null())
                .spawn()
                .map_err(|e| format!("curl: {e}"))?;
            let mut last = u64::MAX;
            loop {
                let done = child.try_wait().map_err(|e| e.to_string())?;
                let b = size(&arc);
                if b != last || done.is_some() {
                    last = b;
                    emit(
                        json,
                        serde_json::json!({"model": m.name, "bytes": b, "total": m.bytes}),
                        format!("  {} / {} MB", b / 1_000_000, m.bytes / 1_000_000),
                    );
                }
                if let Some(st) = done {
                    // 33 = server does not support resume, 416 range error: start over next time
                    if !st.success() {
                        return Err(format!("curl failed ({st}) for {url}; run again to resume"));
                    }
                    break;
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        }
        emit(json, serde_json::json!({"model": m.name, "verify": true}), format!("verifying {}", m.name));
        let sum = sha256(&arc)?;
        if sum != m.sha256 {
            let _ = std::fs::remove_file(&arc);
            return Err(format!("{}: sha256 mismatch (got {sum}), archive removed; run again", m.name));
        }
        emit(json, serde_json::json!({"model": m.name, "unpack": true}), format!("unpacking {}", m.name));
        let st = Command::new("tar").arg("xjf").arg(&arc).arg("-C").arg(&dir).status().map_err(|e| format!("tar: {e}"))?;
        if !st.success() || !installed(&dir, m) {
            return Err(format!("{}: unpack failed", m.name));
        }
        let _ = std::fs::remove_file(&arc);
        emit(json, serde_json::json!({"installed": m.name}), format!("{}: installed", m.name));
    }
    emit(json, serde_json::json!({"done": true}), "all models installed".into());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_dir_wins_only_when_it_holds_every_model() {
        let base = std::env::temp_dir().join(format!("tetron-stt-models-test-{}", std::process::id()));
        let user = PathBuf::from("/home/u/.local/share/tetron-stt/models");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        assert_eq!(pick_dir(&base, user.clone()), user, "empty system dir");
        for (i, m) in MODELS.iter().enumerate() {
            for f in FILES {
                let p = base.join(m.name).join(f);
                std::fs::create_dir_all(p.parent().unwrap()).unwrap();
                std::fs::write(&p, b"x").unwrap();
            }
            let want = if i + 1 == MODELS.len() { base.clone() } else { user.clone() };
            assert_eq!(pick_dir(&base, user.clone()), want, "after {} model(s)", i + 1);
        }
        std::fs::remove_dir_all(&base).unwrap();
    }
}
