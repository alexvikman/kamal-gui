//! Process environment helpers.
//!
//! A macOS app started from Finder gets a minimal PATH (`/usr/bin:/bin:…`)
//! that lacks `op`, `docker`, `mise`, credential helpers and so on. Every
//! child process we spawn therefore gets the PATH of the user's login shell.

use anyhow::{Context, Result, bail};
use std::collections::HashSet;
use std::ffi::OsStr;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

static LOGIN_PATH: OnceLock<String> = OnceLock::new();
static EXTRA_BIN: OnceLock<PathBuf> = OnceLock::new();

/// Directory prepended to PATH for children (the app puts a `kgui` symlink there).
pub fn set_extra_bin_dir(dir: PathBuf) {
    let _ = EXTRA_BIN.set(dir);
}

pub fn path_env() -> String {
    match EXTRA_BIN.get() {
        Some(dir) => format!("{}:{}", dir.display(), login_path()),
        None => login_path().to_string(),
    }
}

pub fn login_path() -> &'static str {
    LOGIN_PATH.get_or_init(|| {
        let mut parts: Vec<String> = Vec::new();
        if let Some(p) = shell_path() {
            parts.extend(p.split(':').map(String::from));
        }
        if let Ok(p) = std::env::var("PATH") {
            parts.extend(p.split(':').map(String::from));
        }
        if let Some(home) = dirs::home_dir() {
            for d in [".local/bin", ".cargo/bin", ".local/share/mise/shims"] {
                parts.push(home.join(d).to_string_lossy().into_owned());
            }
        }
        for d in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin", "/usr/sbin", "/sbin"] {
            parts.push(d.into());
        }
        let mut seen = HashSet::new();
        parts.retain(|p| !p.is_empty() && seen.insert(p.clone()));
        parts.join(":")
    })
}

/// Ask the login shell for its PATH (interactive, so `.zshrc` additions such
/// as `mise activate` are included). Gives up after a few seconds.
fn shell_path() -> Option<String> {
    const MARK: &str = "__KGUI_PATH__";
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    let script = format!("printf '\\n{MARK}%s{MARK}\\n' \"$PATH\"");
    let mut child = Command::new(shell)
        .args(["-l", "-i", "-c", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(25)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let mut out = String::new();
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    let start = out.find(MARK)? + MARK.len();
    let end = start + out[start..].find(MARK)?;
    Some(out[start..end].to_string())
}

/// `Command` with the login-shell PATH. Rust resolves a relative program name
/// against the PATH set on the command, so `command("op")` finds Homebrew's op.
pub fn command(program: impl AsRef<OsStr>) -> Command {
    let mut c = Command::new(program);
    c.env("PATH", path_env());
    c
}

/// Run to completion and return trimmed stdout, or an error carrying stderr.
pub fn run(cmd: &mut Command) -> Result<String> {
    let desc = describe(cmd);
    let out = cmd
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("could not start `{desc}`"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("`{desc}` failed ({}): {}", out.status, stderr.trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
}

fn describe(cmd: &Command) -> String {
    let mut s = cmd.get_program().to_string_lossy().into_owned();
    for a in cmd.get_args().take(4) {
        s.push(' ');
        s.push_str(&a.to_string_lossy());
    }
    s
}
