//! Runs kamal commands and log streams in background threads and reports
//! their output as [`Event`]s over a channel. UI-agnostic: the app drains the
//! channel each frame, and `notify` wakes the UI when something arrives.

use crate::config::SshTarget;
use crate::history::{self, History, Run};
use crate::kamal::{self, Action};
use crate::project::{self, Unit};
use crate::settings::Settings;
use crate::{env, git, secrets, status};
use anyhow::Context;
use std::collections::{BTreeSet, HashMap};
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::process::CommandExt;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, TryLockError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    /// Messages from the app itself.
    Info,
    Out,
    Err,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Running,
    Ok,
    Failed,
    Cancelled,
    Interrupted,
}

impl Status {
    pub fn from_run(s: &str) -> Self {
        match s {
            "running" => Status::Running,
            "ok" => Status::Ok,
            "failed" => Status::Failed,
            "cancelled" => Status::Cancelled,
            _ => Status::Interrupted,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Status::Running => "running",
            Status::Ok => "ok",
            Status::Failed => "failed",
            Status::Cancelled => "cancelled",
            Status::Interrupted => "interrupted",
        }
    }
}

#[derive(Debug, Clone)]
pub enum Event {
    Line { key: String, stream: Stream, line: String },
    End { key: String, status: Status },
    RunUpdated(Run),
}

pub type Notify = Arc<dyn Fn() + Send + Sync>;

/// Handle to the background runner. Cheap to clone.
#[derive(Clone)]
pub struct Runner {
    inner: Arc<Inner>,
}

struct Inner {
    tx: Sender<Event>,
    notify: Notify,
    /// Cancel flags for running sessions, keyed `run:<id>` / `log:<n>`.
    cancels: Mutex<HashMap<String, Arc<AtomicBool>>>,
    /// One kamal run per project at a time (they share the deploy worktree).
    locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    next_log: AtomicU64,
}

pub fn run_key(id: i64) -> String {
    format!("run:{id}")
}

impl Runner {
    pub fn new(tx: Sender<Event>, notify: Notify) -> Self {
        Self {
            inner: Arc::new(Inner {
                tx,
                notify,
                cancels: Mutex::default(),
                locks: Mutex::default(),
                next_log: AtomicU64::new(0),
            }),
        }
    }

    fn send(&self, event: Event) {
        let _ = self.inner.tx.send(event);
        (self.inner.notify)();
    }

    fn register(&self, key: &str) -> Arc<AtomicBool> {
        let flag = Arc::new(AtomicBool::new(false));
        self.inner.cancels.lock().unwrap().insert(key.to_string(), flag.clone());
        flag
    }

    fn unregister(&self, key: &str) {
        self.inner.cancels.lock().unwrap().remove(key);
    }

    pub fn cancel(&self, key: &str) {
        if let Some(flag) = self.inner.cancels.lock().unwrap().get(key) {
            flag.store(true, Ordering::SeqCst);
        }
    }

    fn project_lock(&self, name: &str) -> Arc<Mutex<()>> {
        self.inner.locks.lock().unwrap().entry(name.to_string()).or_default().clone()
    }

    /// Start a kamal run. Returns the history row right away; output and the
    /// final status arrive as events keyed [`run_key`].
    pub fn start_run(
        &self,
        project: &str,
        unit_ids: Vec<String>,
        action: Action,
        git_ref: Option<String>,
    ) -> anyhow::Result<Run> {
        anyhow::ensure!(!unit_ids.is_empty(), "no units selected");
        let history = History::open()?;
        let run_id = history.start(project, &unit_ids.join(", "), &action.label())?;
        let run = history.get(run_id)?.context("run disappeared")?;
        let key = run_key(run_id);
        let cancel = self.register(&key);
        let runner = self.clone();
        let project = project.to_string();

        thread::spawn(move || {
            let out = Output::new(runner.clone(), key.clone(), Some(history::log_path(run_id)));
            let lock = runner.project_lock(&project);
            let guard = match lock.try_lock() {
                Ok(g) => g,
                Err(TryLockError::WouldBlock) => {
                    out.info("Waiting for another run in this project to finish…");
                    lock.lock().unwrap_or_else(|e| e.into_inner())
                }
                Err(TryLockError::Poisoned(e)) => e.into_inner(),
            };
            let result = execute(&out, run_id, &project, &unit_ids, &action, git_ref, &cancel);
            drop(guard);
            runner.unregister(&key);

            let (status, code) = match result {
                Ok(()) => (Status::Ok, Some(0)),
                Err(RunError::Exit(code)) => (Status::Failed, code),
                Err(RunError::Cancelled) => (Status::Cancelled, None),
                Err(RunError::Other(msg)) => {
                    for l in msg.lines() {
                        out.error(l);
                    }
                    (Status::Failed, None)
                }
            };
            out.info(match status {
                Status::Ok => "✓ Done".to_string(),
                Status::Cancelled => "Cancelled".to_string(),
                _ => format!("✗ Failed{}", code.map(|c| format!(" (exit {c})")).unwrap_or_default()),
            });
            if let Ok(h) = History::open() {
                let _ = h.finish(run_id, status.as_str(), code);
                if let Ok(Some(run)) = h.get(run_id) {
                    runner.send(Event::RunUpdated(run));
                }
            }
            runner.send(Event::End { key, status });
        });
        Ok(run)
    }

    /// Follow `docker logs` of a container over ssh. Returns the session key.
    pub fn start_logs(&self, host: &str, ssh: &SshTarget, container: &str, tail: u32) -> anyhow::Result<String> {
        anyhow::ensure!(
            !container.is_empty() && container.chars().all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c)),
            "invalid container name: {container}"
        );
        let key = format!("log:{}", self.inner.next_log.fetch_add(1, Ordering::Relaxed) + 1);
        let mut cmd = status::ssh_command(host, ssh);
        cmd.arg(format!("docker logs --follow --timestamps --tail {tail} {container}"));
        let cancel = self.register(&key);
        let runner = self.clone();
        let k = key.clone();
        thread::spawn(move || {
            let out = Output::new(runner.clone(), k.clone(), None);
            let status = match run_process(&out, cmd, &cancel) {
                Ok(s) if s.success() => Status::Ok,
                Ok(_) => Status::Failed,
                Err(RunError::Cancelled) => Status::Cancelled,
                Err(RunError::Other(msg)) => {
                    out.error(msg);
                    Status::Failed
                }
                Err(RunError::Exit(_)) => Status::Failed,
            };
            runner.unregister(&k);
            runner.send(Event::End { key: k, status });
        });
        Ok(key)
    }
}

/// Emits output lines for one session, and for runs also appends them to the
/// run's log file (`I`/`O`/`E` prefix = info/stdout/stderr).
struct Output {
    runner: Runner,
    key: String,
    file: Mutex<Option<File>>,
}

impl Output {
    fn new(runner: Runner, key: String, log_file: Option<std::path::PathBuf>) -> Arc<Self> {
        let file = log_file.and_then(|p| std::fs::OpenOptions::new().create(true).append(true).open(p).ok());
        Arc::new(Self { runner, key, file: Mutex::new(file) })
    }

    fn emit(&self, stream: Stream, line: impl Into<String>) {
        let line = line.into();
        if let Some(f) = self.file.lock().unwrap().as_mut() {
            let tag = match stream {
                Stream::Out => 'O',
                Stream::Err => 'E',
                Stream::Info => 'I',
            };
            let _ = writeln!(f, "{tag} {line}");
        }
        self.runner.send(Event::Line { key: self.key.clone(), stream, line });
    }

    fn info(&self, line: impl Into<String>) {
        self.emit(Stream::Info, line)
    }

    fn error(&self, line: impl Into<String>) {
        self.emit(Stream::Err, line)
    }
}

/// Read a run's stored log back as `(stream, line)` pairs.
pub fn read_log(id: i64) -> anyhow::Result<Vec<(Stream, String)>> {
    let f = File::open(history::log_path(id)).context("log file not found")?;
    Ok(BufReader::new(f)
        .lines()
        .map_while(Result::ok)
        .map(|l| {
            let (tag, rest) = l.split_at(l.len().min(2));
            let stream = match tag.trim() {
                "O" => Stream::Out,
                "E" => Stream::Err,
                _ => Stream::Info,
            };
            (stream, rest.to_string())
        })
        .collect())
}

enum RunError {
    Exit(Option<i32>),
    Cancelled,
    Other(String),
}

impl From<anyhow::Error> for RunError {
    fn from(e: anyhow::Error) -> Self {
        RunError::Other(format!("{e:#}"))
    }
}

impl From<String> for RunError {
    fn from(s: String) -> Self {
        RunError::Other(s)
    }
}

fn execute(
    out: &Arc<Output>,
    run_id: i64,
    project_name: &str,
    unit_ids: &[String],
    action: &Action,
    git_ref: Option<String>,
    cancel: &AtomicBool,
) -> Result<(), RunError> {
    let settings = Settings::load();
    out.info(format!("› Fetching {project_name} from origin…"));

    let p = project::find(project_name)?;
    let mirror = git::ensure_mirror(&p.name, &p.url)?;
    let default = git::default_branch(&mirror);
    let (rev, from_worktree) = match (action.uses_ref(), git::worktree_head(&p.name)) {
        (true, _) => (git_ref.unwrap_or(default), false),
        (false, Some(head)) => (head, true),
        (false, None) => (default, false),
    };
    let commit = git::resolve(&mirror, &rev)?;
    if let Ok(h) = History::open() {
        let _ = h.set_commit(run_id, &rev, &commit.sha, &commit.subject);
    }
    if from_worktree {
        out.info(format!("› Last deployed commit: {} {}", commit.short, commit.subject));
    } else {
        out.info(format!("› {rev} → {} {} ({})", commit.short, commit.subject, commit.author));
    }

    let wt = git::prepare_worktree(&p.name, &mirror, &commit.sha)?;
    let (targets, _) = project::targets_in_dir(&wt);
    let all: Vec<&Unit> = targets.iter().flat_map(|t| &t.units).collect();
    let units: Vec<Unit> = unit_ids
        .iter()
        .map(|id| {
            all.iter()
                .find(|u| &u.id == id)
                .map(|u| (*u).clone())
                .ok_or_else(|| format!("{id} does not exist in commit {}", commit.short))
        })
        .collect::<Result<_, _>>()?;

    if action.needs_secrets() {
        let mut problems: Vec<String> = units
            .iter()
            .filter_map(|u| secrets::missing_files_problem(&wt, u.config.secrets_path.as_deref(), u.destination.as_deref()))
            .collect();
        problems.dedup();
        if !problems.is_empty() {
            return Err(RunError::Other(problems.join("\n")));
        }
    }

    let mut envs: Vec<(String, String)> = vec![("KGUI_OP_ACCOUNT".into(), settings.op_account.clone())];
    if action.needs_secrets() {
        let refs = secrets::scan_dir(&wt);
        if !refs.is_empty() {
            let items: BTreeSet<_> = refs.iter().map(|r| (&r.vault, &r.item)).collect();
            out.info(format!("› Reading {} secrets ({} items) from 1Password…", refs.len(), items.len()));
            envs.extend(secrets::resolve_all(&settings.op_account, &refs)?);
        } else if secrets::uses_legacy_adapter(&wt) {
            out.info("› .kamal/secrets uses op-read-item-field (one op call per field). `kgui migrate-secrets` converts it to kgui secret.");
        }
    }

    let kamal_bin = kamal::binary(&settings);
    // `.kamal/secrets` may call `kamal secrets fetch` itself. Version-manager
    // shims can't resolve kamal in the app's worktrees, so put its dir first.
    let path = match kamal_bin.parent().filter(|p| !p.as_os_str().is_empty()) {
        Some(dir) => format!("{}:{}", dir.display(), env::path_env()),
        None => env::path_env(),
    };

    for unit in &units {
        if cancel.load(Ordering::SeqCst) {
            return Err(RunError::Cancelled);
        }
        let mut args = action.kamal_args();
        args.extend(unit.kamal_args());
        out.info(format!("━━ kamal {} ━━", args.join(" ")));

        let mut cmd = Command::new(&kamal_bin);
        cmd.args(&args).current_dir(&wt).env("PATH", &path).envs(envs.iter().map(|(k, v)| (k, v)));
        match run_process(out, cmd, cancel) {
            Ok(status) if status.success() => {}
            Ok(status) => return Err(RunError::Exit(status.code())),
            Err(RunError::Cancelled) => {
                out.error("Cancelled mid-run. Check `kamal lock status`; the deploy lock may need releasing.");
                return Err(RunError::Cancelled);
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Run a command, streaming its output, until it exits or `cancel` is set.
/// The child gets its own process group so cancelling also stops whatever it
/// spawned (ssh, docker).
fn run_process(out: &Arc<Output>, mut cmd: Command, cancel: &AtomicBool) -> Result<ExitStatus, RunError> {
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).process_group(0);
    let program = cmd.get_program().to_string_lossy().into_owned();
    let mut child = cmd.spawn().map_err(|e| format!("could not start {program}: {e}"))?;
    let readers = [
        pump(out.clone(), child.stdout.take().unwrap(), Stream::Out),
        pump(out.clone(), child.stderr.take().unwrap(), Stream::Err),
    ];
    let join = |readers: [JoinHandle<()>; 2]| readers.into_iter().for_each(|r| drop(r.join()));

    loop {
        if let Some(status) = child.try_wait().map_err(|e| format!("{program}: {e}"))? {
            join(readers);
            return Ok(status);
        }
        if cancel.load(Ordering::SeqCst) {
            let group = format!("-{}", child.id());
            let _ = Command::new("kill").args(["-TERM", "--", &group]).status();
            let deadline = Instant::now() + Duration::from_secs(3);
            while Instant::now() < deadline && matches!(child.try_wait(), Ok(None)) {
                thread::sleep(Duration::from_millis(50));
            }
            let _ = Command::new("kill").args(["-KILL", "--", &group]).status();
            let _ = child.wait();
            join(readers);
            return Err(RunError::Cancelled);
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn pump(out: Arc<Output>, reader: impl Read + Send + 'static, stream: Stream) -> JoinHandle<()> {
    thread::spawn(move || {
        let mut reader = BufReader::new(reader);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match reader.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let line = String::from_utf8_lossy(&buf);
                    out.emit(stream, line.trim_end_matches(['\n', '\r']));
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn collect(rx: &mpsc::Receiver<Event>, key: &str) -> (Vec<(Stream, String)>, Status) {
        let mut lines = Vec::new();
        loop {
            match rx.recv_timeout(Duration::from_secs(10)).expect("timed out") {
                Event::Line { key: k, stream, line } if k == key => lines.push((stream, line)),
                Event::End { key: k, status } if k == key => return (lines, status),
                _ => {}
            }
        }
    }

    #[test]
    fn streams_output_and_exit() {
        let (tx, rx) = mpsc::channel();
        let runner = Runner::new(tx, Arc::new(|| {}));
        let key = "t:1".to_string();
        let cancel = runner.register(&key);
        let out = Output::new(runner.clone(), key.clone(), None);
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "echo one; echo two >&2; exit 3"]);
        let status = run_process(&out, cmd, &cancel).ok().unwrap();
        assert_eq!(status.code(), Some(3));
        runner.send(Event::End { key: key.clone(), status: Status::Failed });
        let (lines, _) = collect(&rx, &key);
        assert!(lines.contains(&(Stream::Out, "one".into())));
        assert!(lines.contains(&(Stream::Err, "two".into())));
    }

    #[test]
    fn cancel_kills_process_group() {
        let (tx, _rx) = mpsc::channel();
        let runner = Runner::new(tx, Arc::new(|| {}));
        let key = "t:2".to_string();
        let cancel = runner.register(&key);
        let out = Output::new(runner.clone(), key.clone(), None);
        let mut cmd = Command::new("sh");
        // The grandchild `sleep` must die too.
        cmd.args(["-c", "sleep 30 & wait"]);
        let r2 = runner.clone();
        let k2 = key.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(300));
            r2.cancel(&k2);
        });
        let started = Instant::now();
        assert!(matches!(run_process(&out, cmd, &cancel), Err(RunError::Cancelled)));
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
