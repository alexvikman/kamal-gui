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
        // Absolute entries only: kamal runs inside a checkout of the project,
        // where a relative entry (`.`, `bin`, `node_modules/.bin`) would let
        // the repository supply its own `git`, `ssh` or `docker`.
        let mut seen = HashSet::new();
        parts.retain(|p| p.starts_with('/') && seen.insert(p.clone()));
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

static SSH_APPROVED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static SSH_GATE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Run something that uses the SSH agent (git over ssh, ssh to servers).
///
/// Agents like 1Password ask for approval per application. If the first
/// connections start at the same moment, each one triggers its own prompt.
/// So the first SSH use runs alone; once it's done (and approved), the rest
/// run in parallel and reuse the approval.
pub fn with_ssh<T>(f: impl FnOnce() -> T) -> T {
    use std::sync::atomic::Ordering;
    if SSH_APPROVED.load(Ordering::Acquire) {
        return f();
    }
    let guard = SSH_GATE.lock().unwrap_or_else(|e| e.into_inner());
    if SSH_APPROVED.load(Ordering::Acquire) {
        drop(guard);
        return f();
    }
    let result = f();
    SSH_APPROVED.store(true, Ordering::Release);
    result
}

/// At most `max` jobs at a time per key (a server, say); the rest wait.
pub struct Limit {
    max: usize,
    running: std::sync::Mutex<Vec<(String, usize)>>,
    freed: std::sync::Condvar,
}

impl Limit {
    pub const fn new(max: usize) -> Self {
        Self { max, running: std::sync::Mutex::new(Vec::new()), freed: std::sync::Condvar::new() }
    }

    pub fn run<T>(&self, key: &str, f: impl FnOnce() -> T) -> T {
        let _permit = self.acquire(key, &|| false);
        f()
    }

    /// A slot if one is free right now.
    pub fn try_acquire<'a>(&'a self, key: &'a str) -> Option<Permit<'a>> {
        let mut running = self.running.lock().unwrap_or_else(|e| e.into_inner());
        self.take(&mut running, key)
    }

    /// Wait for a slot; gives up (None) once `stop` returns true.
    pub fn acquire<'a>(&'a self, key: &'a str, stop: &dyn Fn() -> bool) -> Option<Permit<'a>> {
        let mut running = self.running.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if let Some(p) = self.take(&mut running, key) {
                return Some(p);
            }
            if stop() {
                return None;
            }
            running = self.freed.wait_timeout(running, Duration::from_millis(250)).unwrap_or_else(|e| e.into_inner()).0;
        }
    }

    fn take<'a>(&'a self, running: &mut Vec<(String, usize)>, key: &'a str) -> Option<Permit<'a>> {
        match running.iter_mut().find(|(k, _)| k == key) {
            Some((_, n)) if *n >= self.max => None,
            Some((_, n)) => {
                *n += 1;
                Some(Permit(self, key))
            }
            None => {
                running.push((key.to_string(), 1));
                Some(Permit(self, key))
            }
        }
    }
}

/// A slot taken from a [`Limit`]; freed when dropped (also on panic).
pub struct Permit<'a>(&'a Limit, &'a str);

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        let mut running = self.0.running.lock().unwrap_or_else(|e| e.into_inner());
        running.retain_mut(|(k, n)| {
            if k == self.1 {
                *n -= 1;
            }
            *n > 0
        });
        self.0.freed.notify_all();
    }
}

/// Run to completion and return trimmed stdout, or an error carrying stderr.
pub fn run(cmd: &mut Command) -> Result<String> {
    let desc = describe(cmd);
    let out = cmd.stdin(Stdio::null()).output().with_context(|| format!("could not start `{desc}`"))?;
    check_output(&desc, out)
}

/// [`run`] with a time limit (see [`output_within`]).
pub fn run_within(cmd: &mut Command, timeout: Duration) -> Result<String> {
    let desc = describe(cmd);
    let out = output_within(cmd, timeout)?;
    check_output(&desc, out)
}

/// A command gave no answer in time and was stopped.
#[derive(Debug)]
pub struct TimedOut(pub Duration);

impl std::fmt::Display for TimedOut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "no answer within {} s", self.0.as_secs())
    }
}

impl std::error::Error for TimedOut {}

/// Like `Command::output`, but stops waiting after `timeout`: the command and
/// everything it started (ssh under git, say) run in their own process group,
/// which is killed. The error is then a [`TimedOut`].
pub fn output_within(cmd: &mut Command, timeout: Duration) -> Result<std::process::Output> {
    use std::os::unix::process::CommandExt;
    let desc = describe(cmd);
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .with_context(|| format!("could not start `{desc}`"))?;
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_end(&mut buf);
            }
            buf
        })
    };
    let stdout = drain(child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let stderr = drain(child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = Command::new("kill").args(["-KILL", "--", &format!("-{}", child.id())]).status();
            let _ = child.wait();
            let _ = (stdout.join(), stderr.join());
            return Err(TimedOut(timeout).into());
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    Ok(std::process::Output {
        status,
        stdout: stdout.join().unwrap_or_default(),
        stderr: stderr.join().unwrap_or_default(),
    })
}

fn check_output(desc: &str, out: std::process::Output) -> Result<String> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    /// The first SSH use runs alone; nothing overlaps it.
    #[test]
    fn output_within_stops_the_whole_group() {
        let started = Instant::now();
        let err = output_within(Command::new("sh").args(["-c", "sleep 30 & sleep 30"]), Duration::from_millis(300))
            .unwrap_err();
        assert!(err.downcast_ref::<TimedOut>().is_some(), "{err:#}");
        assert!(started.elapsed() < Duration::from_secs(5));
        let out = run_within(Command::new("sh").args(["-c", "echo hi"]), Duration::from_secs(5)).unwrap();
        assert_eq!(out, "hi");
    }

    #[test]
    fn limit_caps_jobs_per_key() {
        static LIMIT: Limit = Limit::new(2);
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let other_ran = Arc::new(AtomicUsize::new(0));
        let mut handles: Vec<_> = (0..6)
            .map(|_| {
                let (active, peak) = (active.clone(), peak.clone());
                std::thread::spawn(move || {
                    LIMIT.run("host-a", || {
                        let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                        peak.fetch_max(now, Ordering::SeqCst);
                        std::thread::sleep(Duration::from_millis(40));
                        active.fetch_sub(1, Ordering::SeqCst);
                    })
                })
            })
            .collect();
        // Another key isn't held up by host-a's queue.
        let other = other_ran.clone();
        handles.push(std::thread::spawn(move || {
            LIMIT.run("host-b", || {
                other.fetch_add(1, Ordering::SeqCst);
            })
        }));
        handles.into_iter().for_each(|h| {
            h.join().unwrap();
        });
        assert_eq!(peak.load(Ordering::SeqCst), 2);
        assert_eq!(other_ran.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn waiting_for_a_slot_can_be_given_up() {
        let limit = Limit::new(1);
        let held = limit.try_acquire("deploy").unwrap();
        assert!(limit.try_acquire("deploy").is_none());
        let started = Instant::now();
        assert!(limit.acquire("deploy", &|| started.elapsed() > Duration::from_millis(300)).is_none());
        drop(held);
        assert!(limit.acquire("deploy", &|| true).is_some());
    }

    #[test]
    fn first_ssh_use_runs_alone() {
        let active = Arc::new(AtomicUsize::new(0));
        let max_during_first = Arc::new(AtomicUsize::new(0));
        let handles: Vec<_> = (0..4)
            .map(|i| {
                let (active, max) = (active.clone(), max_during_first.clone());
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(i * 5));
                    with_ssh(|| {
                        let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                        if !SSH_APPROVED.load(Ordering::SeqCst) {
                            max.fetch_max(now, Ordering::SeqCst);
                        }
                        std::thread::sleep(Duration::from_millis(50));
                        active.fetch_sub(1, Ordering::SeqCst);
                    })
                })
            })
            .collect();
        handles.into_iter().for_each(|h| h.join().unwrap());
        assert_eq!(max_during_first.load(Ordering::SeqCst), 1);
        assert!(SSH_APPROVED.load(Ordering::SeqCst));
    }
}
