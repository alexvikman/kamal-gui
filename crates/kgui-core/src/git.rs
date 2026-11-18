//! The app never deploys from your working copy. Each project gets a bare
//! clone of its GitHub origin under the data dir, and deploys run in a
//! worktree checked out at the chosen commit.

use crate::env::{command, run};
use crate::settings::data_dir;
use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Branch the deploy worktree sits on. Kamal clones the worktree for its
/// build and fetches from it, so HEAD must be on a branch, not detached.
const DEPLOY_BRANCH: &str = "kgui-deploy";

pub fn mirror_dir(project: &str) -> PathBuf {
    data_dir().join("repos").join(format!("{project}.git"))
}

pub fn worktree_dir(project: &str) -> PathBuf {
    data_dir().join("worktrees").join(project)
}

fn git(mirror: &Path) -> Command {
    let mut c = command("git");
    c.arg("--git-dir").arg(mirror);
    c
}

fn git_in(dir: &Path) -> Command {
    let mut c = command("git");
    c.arg("-C").arg(dir);
    c
}

/// Transports git may use for origin. What is cloned gets deployed, so only
/// encrypted, authenticated ones: no `http://`, `git://`, local paths or
/// remote helpers (`ext::…` runs a command).
const ALLOWED_PROTOCOLS: &str = "https:ssh";

/// Accepts `https://…`, `ssh://…` and scp-style `user@host:path`.
pub fn check_origin(url: &str) -> Result<()> {
    let host_ok = |h: &str| {
        !h.is_empty() && !h.starts_with('-') && h.chars().all(|c| c.is_ascii_alphanumeric() || "._-@[]:".contains(c))
    };
    let ok = !url.starts_with('-')
        && !url.chars().any(|c| c.is_whitespace() || c.is_control())
        && match url.split_once("://") {
            Some(("https" | "ssh", rest)) => host_ok(rest.split('/').next().unwrap_or("")),
            Some(_) => false,
            // scp-style: a host before the first colon, then a path.
            None => url
                .split_once(':')
                .is_some_and(|(host, path)| host_ok(host) && !path.is_empty() && !path.starts_with(':')),
        };
    anyhow::ensure!(ok, "unsupported repository URL {url:?} (use https://… or git@host:org/repo.git)");
    Ok(())
}

/// Create the bare clone if needed, then fetch. Returns the mirror path.
pub fn ensure_mirror(project: &str, origin: &str) -> Result<PathBuf> {
    check_origin(origin)?;
    crate::env::with_ssh(|| ensure_mirror_inner(project, origin))
}

fn ensure_mirror_inner(project: &str, origin: &str) -> Result<PathBuf> {
    let dir = mirror_dir(project);
    if !dir.join("HEAD").exists() {
        std::fs::create_dir_all(dir.parent().unwrap())?;
        if dir.exists() {
            std::fs::remove_dir_all(&dir)?;
        }
        // `--` ends option parsing: the URL can never be taken as a flag.
        run(command("git")
            .env("GIT_ALLOW_PROTOCOL", ALLOWED_PROTOCOLS)
            .args(["clone", "--bare", "--quiet", "--", origin])
            .arg(&dir))
        .with_context(|| format!("could not clone {origin}"))?;
        run(git(&dir).args(["config", "remote.origin.fetch", "+refs/heads/*:refs/remotes/origin/*"]))?;
        // Never let stray secrets files make Kamal see an "uncommitted" tree.
        std::fs::create_dir_all(dir.join("info"))?;
        std::fs::write(dir.join("info/exclude"), "/.kamal/secrets*\n")?;
    } else if run(git(&dir).args(["remote", "get-url", "origin"])).ok().as_deref() != Some(origin) {
        run(git(&dir).args(["remote", "set-url", "origin", origin]))?;
    }
    fetch(&dir)?;
    Ok(dir)
}

pub fn fetch(mirror: &Path) -> Result<()> {
    run(git(mirror)
        .env("GIT_ALLOW_PROTOCOL", ALLOWED_PROTOCOLS)
        .args(["fetch", "--quiet", "--prune", "--tags", "origin"]))?;
    let _ = run(git(mirror).args(["remote", "set-head", "origin", "--auto"]));
    Ok(())
}

/// `origin/main` (or whatever origin's HEAD points at).
pub fn default_branch(mirror: &Path) -> String {
    if let Ok(r) = run(git(mirror).args(["symbolic-ref", "--short", "refs/remotes/origin/HEAD"]))
        && !r.is_empty()
    {
        return r;
    }
    for b in ["origin/main", "origin/master"] {
        if run(git(mirror).args(["rev-parse", "--verify", "--quiet", b])).is_ok() {
            return b.into();
        }
    }
    "origin/main".into()
}

#[derive(Debug, Clone, Serialize)]
pub struct Commit {
    pub sha: String,
    pub short: String,
    pub author: String,
    pub time: i64,
    pub subject: String,
}

const LOG_FORMAT: &str = "--format=%H%x1f%an%x1f%ct%x1f%s";

fn parse_commits(out: &str) -> Vec<Commit> {
    out.lines()
        .filter_map(|l| {
            let mut p = l.split('\x1f');
            let sha = p.next()?.to_string();
            Some(Commit {
                short: sha.chars().take(7).collect(),
                sha,
                author: p.next()?.into(),
                time: p.next()?.parse().unwrap_or(0),
                subject: p.next().unwrap_or("").into(),
            })
        })
        .collect()
}

pub fn resolve(mirror: &Path, rev: &str) -> Result<Commit> {
    let rev = format!("{rev}^{{commit}}");
    let out = run(git(mirror).args(["log", "-1", LOG_FORMAT, "--end-of-options", &rev, "--"]))
        .with_context(|| format!("unknown revision {rev}"))?;
    parse_commits(&out).into_iter().next().context("empty commit")
}

pub fn log(mirror: &Path, rev: &str, n: usize) -> Result<Vec<Commit>> {
    let out = run(git(mirror).args(["log", &format!("-{n}"), LOG_FORMAT, "--end-of-options", rev, "--"]))?;
    Ok(parse_commits(&out))
}

/// Number of commits on `rev` that `sha` lacks. None if `sha` is unknown.
pub fn behind(mirror: &Path, sha: &str, rev: &str) -> Option<u32> {
    run(git(mirror).args(["rev-list", "--count", "--end-of-options", &format!("{sha}..{rev}")])).ok()?.parse().ok()
}

#[derive(Debug, Clone, Serialize)]
pub struct GitRef {
    pub name: String,
    pub kind: String,
    pub sha: String,
    pub time: i64,
    pub subject: String,
}

pub fn refs(mirror: &Path) -> Result<Vec<GitRef>> {
    let out = run(git(mirror).args([
        "for-each-ref",
        "--sort=-committerdate",
        "--count=200",
        "--format=%(refname)%1f%(objectname)%1f%(committerdate:unix)%1f%(subject)",
        "refs/remotes/origin",
        "refs/tags",
    ]))?;
    Ok(out
        .lines()
        .filter_map(|l| {
            let mut p = l.split('\x1f');
            let full = p.next()?;
            let (kind, name) = if let Some(n) = full.strip_prefix("refs/remotes/") {
                ("branch", n)
            } else {
                ("tag", full.strip_prefix("refs/tags/")?)
            };
            if name.ends_with("/HEAD") {
                return None;
            }
            Some(GitRef {
                name: name.into(),
                kind: kind.into(),
                sha: p.next()?.into(),
                time: p.next()?.parse().unwrap_or(0),
                subject: p.next().unwrap_or("").into(),
            })
        })
        .collect())
}

pub fn list_config_files(mirror: &Path, rev: &str) -> Result<Vec<String>> {
    let out = run(git(mirror).args(["ls-tree", "--name-only", "--end-of-options", rev, "config/"]))?;
    Ok(out.lines().filter_map(|l| l.strip_prefix("config/")).map(String::from).collect())
}

pub fn show(mirror: &Path, rev: &str, path: &str) -> Option<String> {
    run(git(mirror).args(["show", "--end-of-options", &format!("{rev}:{path}")])).ok()
}

/// Check out `sha` in the project's deploy worktree (created on first use)
/// and wipe anything left from earlier runs.
pub fn prepare_worktree(project: &str, mirror: &Path, sha: &str) -> Result<PathBuf> {
    let wt = worktree_dir(project);
    if wt.join(".git").exists() {
        run(git_in(&wt).args(["checkout", "--quiet", "--force", "-B", DEPLOY_BRANCH, sha]))?;
        run(git_in(&wt).args(["clean", "-ffdxq"]))?;
    } else {
        if wt.exists() {
            std::fs::remove_dir_all(&wt)?;
        }
        std::fs::create_dir_all(wt.parent().unwrap())?;
        let _ = run(git(mirror).args(["worktree", "prune"]));
        run(git(mirror).args(["worktree", "add", "--quiet", "--force", "-B", DEPLOY_BRANCH]).arg(&wt).arg(sha))?;
    }
    let head = run(git_in(&wt).args(["rev-parse", "HEAD"]))?;
    if head != sha {
        bail!("worktree is at {head}, expected {sha}");
    }
    Ok(wt)
}

/// Commit currently checked out in the deploy worktree, if any.
pub fn worktree_head(project: &str) -> Option<String> {
    let wt = worktree_dir(project);
    if !wt.join(".git").exists() {
        return None;
    }
    run(git_in(&wt).args(["rev-parse", "HEAD"])).ok()
}

/// Everything the deploy picker needs for one project.
#[derive(Debug, Clone, Serialize)]
pub struct RefList {
    pub default_branch: String,
    pub refs: Vec<GitRef>,
    pub commits: Vec<Commit>,
    /// Commit in the deploy worktree (the last thing deployed from the app).
    pub worktree_head: Option<String>,
}

pub fn ref_list(project: &str) -> Result<RefList> {
    let mirror = mirror_dir(project);
    anyhow::ensure!(mirror.join("HEAD").exists(), "project has not been cloned yet");
    let default_branch = default_branch(&mirror);
    Ok(RefList {
        refs: refs(&mirror)?,
        commits: log(&mirror, &default_branch, 30)?,
        worktree_head: worktree_head(project),
        default_branch,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_urls() {
        for ok in [
            "git@github.com:org/app.git",
            "https://github.com/org/app",
            "ssh://git@github.com/org/app.git",
            "ssh://git@[fd00::1]:2222/org/app.git",
            "deploy@git.example.com:app",
        ] {
            assert!(check_origin(ok).is_ok(), "{ok}");
        }
        for bad in [
            "--upload-pack=touch /tmp/x",
            "-oProxyCommand=x:repo",
            "ext::sh -c 'touch /tmp/x'",
            "ext::sh",
            "http://github.com/org/app",
            "git://github.com/org/app",
            "file:///tmp/repo",
            "/tmp/repo",
            "ssh://-oProxyCommand=x/repo",
            "git@github.com:org/app.git\n--flag",
            "",
        ] {
            assert!(check_origin(bad).is_err(), "{bad}");
        }
    }
}
