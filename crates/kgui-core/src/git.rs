//! The app never deploys from your working copy. Each project gets a bare
//! clone of its GitHub origin under the data dir, and deploys run in a
//! worktree checked out at the chosen commit.

use crate::env::{command, run, run_within};
use crate::settings::data_dir;
use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// Branch the deploy worktree sits on. Kamal clones the worktree for its
/// build and fetches from it, so HEAD must be on a branch, not detached.
const DEPLOY_BRANCH: &str = "kgui-deploy";

pub fn mirror_dir(project: &str) -> PathBuf {
    data_dir().join("repos").join(format!("{project}.git"))
}

/// When the app's clone of a project was last fetched (Unix ms).
pub fn last_fetch(project: &str) -> Option<i64> {
    let dir = mirror_dir(project);
    let modified = ["FETCH_HEAD", "HEAD"].iter().find_map(|f| std::fs::metadata(dir.join(f)).ok()?.modified().ok())?;
    Some(modified.duration_since(std::time::UNIX_EPOCH).ok()?.as_millis() as i64)
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

/// Refuses a URL that carries a password or token (`https://user:token@…`).
/// The app stores project URLs as they are and shows them in the window, in
/// error messages and to `git` as an argument, so a secret in one would end
/// up in all of those. Checked when a project is added.
pub fn check_no_credentials(url: &str) -> Result<()> {
    let authority = url.split_once("://").map(|(_, rest)| rest.split('/').next().unwrap_or(""));
    let has_secret = authority.and_then(|a| a.rsplit_once('@')).is_some_and(|(userinfo, _)| userinfo.contains(':'));
    // Never repeat the URL here: the message is shown and may be logged.
    anyhow::ensure!(
        !has_secret,
        "this URL contains a password or token. Remove it and let git's credential helper sign in, or use an ssh URL (git@host:org/repo.git)"
    );
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
        ORIGIN_LIMIT
            .run("origin", || {
                run_within(
                    command("git")
                        .env("GIT_ALLOW_PROTOCOL", ALLOWED_PROTOCOLS)
                        .args(["clone", "--bare", "--quiet", "--", origin])
                        .arg(&dir),
                    CLONE_TIMEOUT,
                )
            })
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

/// Time limits for talking to origin. They include waiting for the SSH agent
/// (1Password) to approve the key, so they're generous; they're there so a
/// dead connection fails instead of hanging the app.
const CLONE_TIMEOUT: Duration = Duration::from_secs(600);
const FETCH_TIMEOUT: Duration = Duration::from_secs(120);

/// Clones and fetches at the same time (refreshing many projects at once
/// would otherwise ask the SSH agent to sign for all of them together).
static ORIGIN_LIMIT: crate::env::Limit = crate::env::Limit::new(4);

/// What a fetch brings in: origin's branches and tags, exactly as they are
/// there now. Tags are forced like branches: with plain `--tags`, a tag that
/// was moved on origin makes every fetch fail ("would clobber existing tag"),
/// so one moved tag would stop all deploys of the project, and the clone
/// would keep deploying the commit the tag used to point at.
const FETCH_REFSPECS: [&str; 2] = ["+refs/heads/*:refs/remotes/origin/*", "+refs/tags/*:refs/tags/*"];

pub fn fetch(mirror: &Path) -> Result<()> {
    ORIGIN_LIMIT
        .run("origin", || {
            run_within(
                git(mirror)
                    .env("GIT_ALLOW_PROTOCOL", ALLOWED_PROTOCOLS)
                    .args(["fetch", "--quiet", "--prune", "--no-tags", "origin"])
                    .args(FETCH_REFSPECS),
                FETCH_TIMEOUT,
            )
        })
        .context("git fetch from origin")?;
    let _ = run(git(mirror).args(["remote", "set-head", "origin", "--auto"]));
    Ok(())
}

/// `origin/main` (or whatever origin's HEAD points at).
pub fn default_branch(mirror: &Path) -> String {
    // Not `--short`: git shortens to `remotes/origin/main` when a tag named
    // `origin/main` exists, and the rest of the app expects `origin/<branch>`.
    if let Ok(r) = run(git(mirror).args(["symbolic-ref", "refs/remotes/origin/HEAD"]))
        && let Some(branch) = r.strip_prefix("refs/remotes/origin/")
        && !branch.is_empty()
    {
        return format!("origin/{branch}");
    }
    ["main", "master"]
        .into_iter()
        .find(|b| has_branch(mirror, b))
        .map_or("origin/main".into(), |b| format!("origin/{b}"))
}

/// The branch the app follows for a project: `origin/<branch>` when one is
/// chosen and origin has it, otherwise origin's default branch.
pub fn tracked_branch(mirror: &Path, branch: Option<&str>) -> String {
    match branch {
        Some(b) if has_branch(mirror, b) => format!("origin/{b}"),
        _ => default_branch(mirror),
    }
}

/// Whether origin has a branch named `branch` (as of the last fetch).
pub fn has_branch(mirror: &Path, branch: &str) -> bool {
    !branch.is_empty()
        && !branch.starts_with('-')
        && run(git(mirror).args([
            "rev-parse",
            "--verify",
            "--quiet",
            "--end-of-options",
            &format!("refs/remotes/origin/{branch}^{{commit}}"),
        ]))
        .is_ok()
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

/// A full object id (SHA-1 or SHA-256).
fn is_object_id(s: &str) -> bool {
    matches!(s.len(), 40 | 64) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The one thing a revision name means in this app, spelled so git can't
/// read it any other way: `origin/<branch>` is that branch on origin, a full
/// object id is that object, and any other name is a tag.
///
/// Git's own lookup of a short name tries tags (and local branches) before
/// branches on origin. Handed `origin/main` as it is, git would pick a tag
/// that someone named `origin/main`, and the app would show and deploy that
/// commit as the head of main. So git only ever gets what this returns.
pub fn full_rev(rev: &str) -> String {
    if rev.starts_with("refs/remotes/origin/") || rev.starts_with("refs/tags/") || is_object_id(rev) {
        rev.to_string()
    } else if rev.starts_with("origin/") {
        format!("refs/remotes/{rev}")
    } else {
        format!("refs/tags/{rev}")
    }
}

/// [`full_rev`] the way it's shown: `origin/main`, `v1.2`, or the id.
pub fn short_rev(rev: &str) -> &str {
    rev.strip_prefix("refs/remotes/").or_else(|| rev.strip_prefix("refs/tags/")).unwrap_or(rev)
}

/// The full id of the commit an id or abbreviation (7 characters or more)
/// stands for, as image tags on the servers carry it. Only ever an object:
/// git would otherwise look for a tag or branch with that name first.
pub fn commit_id(mirror: &Path, id: &str) -> Option<String> {
    if is_object_id(id) {
        return Some(id.to_string());
    }
    if id.len() < 7 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    // Every object that starts with the abbreviation; keep the commits.
    let candidates = run(git(mirror).args(["rev-parse", &format!("--disambiguate={id}")])).ok()?;
    let mut commits = candidates.lines().filter(|c| is_object_id(c)).take(16).filter(|c| {
        run(git(mirror).args(["rev-parse", "--verify", "--quiet", "--end-of-options", &format!("{c}^{{commit}}")]))
            .is_ok_and(|commit| commit == *c)
    });
    let commit = commits.next()?;
    commits.next().is_none().then(|| commit.to_string())
}

pub fn resolve(mirror: &Path, rev: &str) -> Result<Commit> {
    let commit = format!("{}^{{commit}}", full_rev(rev));
    let out = run(git(mirror).args(["log", "-1", LOG_FORMAT, "--end-of-options", &commit, "--"]))
        .with_context(|| format!("unknown revision {}", short_rev(rev)))?;
    parse_commits(&out).into_iter().next().context("empty commit")
}

pub fn log(mirror: &Path, rev: &str, n: usize) -> Result<Vec<Commit>> {
    let out = run(git(mirror).args(["log", &format!("-{n}"), LOG_FORMAT, "--end-of-options", &full_rev(rev), "--"]))?;
    Ok(parse_commits(&out))
}

/// Number of commits on `rev` that the commit `id` (see [`commit_id`]) lacks.
/// None if the commit is unknown.
pub fn behind(mirror: &Path, id: &str, rev: &str) -> Option<u32> {
    if !is_object_id(id) {
        return None;
    }
    run(git(mirror).args(["rev-list", "--count", "--end-of-options", &format!("{id}..{}", full_rev(rev))]))
        .ok()?
        .parse()
        .ok()
}

#[derive(Debug, Clone, Serialize)]
pub struct GitRef {
    pub name: String,
    pub kind: String,
    pub sha: String,
    pub time: i64,
    pub subject: String,
}

impl GitRef {
    /// The revision to deploy this branch or tag by: its full ref name, so a
    /// tag and a branch with the same short name can't be mixed up.
    pub fn rev(&self) -> String {
        match self.kind.as_str() {
            "branch" => format!("refs/remotes/{}", self.name),
            _ => format!("refs/tags/{}", self.name),
        }
    }
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
    let out = run(git(mirror).args(["ls-tree", "--name-only", "--end-of-options", &full_rev(rev), "config/"]))?;
    Ok(out.lines().filter_map(|l| l.strip_prefix("config/")).map(String::from).collect())
}

pub fn show(mirror: &Path, rev: &str, path: &str) -> Option<String> {
    run(git(mirror).args(["show", "--end-of-options", &format!("{}:{path}", full_rev(rev))])).ok()
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
    /// The branch the project follows (`origin/develop`).
    pub branch: String,
    pub refs: Vec<GitRef>,
    pub commits: Vec<Commit>,
    /// Commit in the deploy worktree (the last thing deployed from the app).
    pub worktree_head: Option<String>,
}

pub fn ref_list(project: &str, branch: Option<&str>) -> Result<RefList> {
    let mirror = mirror_dir(project);
    anyhow::ensure!(mirror.join("HEAD").exists(), "project has not been cloned yet");
    let branch = tracked_branch(&mirror, branch);
    Ok(RefList {
        refs: refs(&mirror)?,
        commits: log(&mirror, &branch, 30)?,
        worktree_head: worktree_head(project),
        branch,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follows_a_chosen_branch_while_origin_has_it() {
        let dir = std::env::temp_dir().join(format!("kgui-branch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mirror = dir.join("m.git");
        let sh = |args: &[&str]| assert!(std::process::Command::new("git").args(args).status().unwrap().success());
        let m = mirror.to_str().unwrap();
        sh(&["init", "-q", "--bare", m]);
        let tree = run(git(&mirror).args(["hash-object", "-w", "-t", "tree", "/dev/null"])).unwrap();
        let commit = run(git(&mirror)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .args(["commit-tree", "-m", "c", &tree]))
        .unwrap();
        for r in ["refs/remotes/origin/main", "refs/remotes/origin/develop"] {
            sh(&["--git-dir", m, "update-ref", r, &commit]);
        }
        assert_eq!(tracked_branch(&mirror, None), "origin/main");
        assert_eq!(tracked_branch(&mirror, Some("develop")), "origin/develop");
        // Gone from origin (or never there): back to the default branch.
        assert_eq!(tracked_branch(&mirror, Some("feature")), "origin/main");
        assert!(!has_branch(&mirror, "--help"));
        assert!(!has_branch(&mirror, ""));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// An origin with `main`, a second commit on `evil`, and a mirror of it.
    fn origin_and_mirror(name: &str) -> (PathBuf, PathBuf, String, String) {
        let dir = std::env::temp_dir().join(format!("kgui-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (origin, mirror) = (dir.join("origin"), dir.join("m.git"));
        std::fs::create_dir_all(&origin).unwrap();
        let commit = |msg: &str| {
            in_origin(
                &origin,
                &["-c", "user.name=t", "-c", "user.email=t@example.com", "commit", "-q", "--allow-empty", "-m", msg],
            )
        };
        in_origin(&origin, &["init", "-q", "-b", "main"]);
        commit("good");
        let good = in_origin(&origin, &["rev-parse", "HEAD"]);
        in_origin(&origin, &["checkout", "-q", "-b", "evil"]);
        commit("evil");
        let evil = in_origin(&origin, &["rev-parse", "HEAD"]);
        in_origin(&origin, &["checkout", "-q", "main"]);
        let out = std::process::Command::new("git").args(["clone", "-q", "--bare"]).arg(&origin).arg(&mirror).output();
        assert!(out.unwrap().status.success());
        run(git(&mirror).args(["config", "remote.origin.fetch", "+refs/heads/*:refs/remotes/origin/*"])).unwrap();
        fetch_local(&mirror);
        (origin, mirror, good, evil)
    }

    fn in_origin(origin: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git").arg("-C").arg(origin).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// [`fetch`] from an origin on disk (the app itself only allows https and ssh).
    fn fetch_local(mirror: &Path) {
        run(git(mirror).args(["fetch", "--quiet", "--prune", "--no-tags", "origin"]).args(FETCH_REFSPECS)).unwrap();
        let _ = run(git(mirror).args(["remote", "set-head", "origin", "--auto"]));
    }

    /// Anyone who can push a tag could otherwise point "main" at any commit:
    /// git looks a short name up among tags before branches on origin.
    #[test]
    fn a_tag_cannot_stand_in_for_a_branch() {
        let (origin, mirror, good, evil) = origin_and_mirror("ref-confusion");
        let short = &good[..7];
        in_origin(&origin, &["tag", "origin/main", "evil"]);
        in_origin(&origin, &["tag", "v1", "main"]);
        // A tag named like the abbreviated id of main's commit.
        in_origin(&origin, &["tag", short, "evil"]);
        fetch_local(&mirror);

        assert_eq!(default_branch(&mirror), "origin/main");
        assert_eq!(tracked_branch(&mirror, None), "origin/main");
        assert_eq!(resolve(&mirror, "origin/main").unwrap().sha, good);
        assert_eq!(resolve(&mirror, "origin/main").unwrap().subject, "good");
        assert_eq!(log(&mirror, "origin/main", 5).unwrap().len(), 1);
        assert_eq!(resolve(&mirror, "origin/evil").unwrap().sha, evil);
        assert_eq!(resolve(&mirror, "v1").unwrap().sha, good);
        // The tag is still there to pick, under its own full name.
        let tag = refs(&mirror).unwrap().into_iter().find(|r| r.kind == "tag" && r.name == "origin/main").unwrap();
        assert_eq!(tag.rev(), "refs/tags/origin/main");
        assert_eq!(resolve(&mirror, &tag.rev()).unwrap().sha, evil);
        let branch =
            refs(&mirror).unwrap().into_iter().find(|r| r.kind == "branch" && r.name == "origin/main").unwrap();
        assert_eq!(resolve(&mirror, &branch.rev()).unwrap().sha, good);
        // Neither a local branch nor anything else git would guess at.
        assert!(resolve(&mirror, "evil").is_err());
        assert!(resolve(&mirror, "HEAD").is_err());
        assert!(resolve(&mirror, "--help").is_err());

        // Image tags name commits, never refs.
        assert_eq!(commit_id(&mirror, short).as_deref(), Some(good.as_str()));
        assert_eq!(commit_id(&mirror, &good).as_deref(), Some(good.as_str()));
        assert_eq!(commit_id(&mirror, "v1"), None);
        assert_eq!(commit_id(&mirror, "abc"), None);
        assert_eq!(behind(&mirror, &good, "origin/main"), Some(0));
        assert_eq!(behind(&mirror, &good, "origin/evil"), Some(1));
        assert_eq!(behind(&mirror, short, "origin/main"), None);
        std::fs::remove_dir_all(origin.parent().unwrap()).unwrap();
    }

    /// A tag moved or deleted on origin must not make fetching fail, and the
    /// clone follows origin.
    #[test]
    fn fetch_follows_moved_and_deleted_tags() {
        let (origin, mirror, good, evil) = origin_and_mirror("moved-tags");
        in_origin(&origin, &["tag", "stable", "main"]);
        in_origin(&origin, &["tag", "old", "main"]);
        fetch_local(&mirror);
        assert_eq!(resolve(&mirror, "stable").unwrap().sha, good);
        in_origin(&origin, &["tag", "-f", "stable", "evil"]);
        in_origin(&origin, &["tag", "-d", "old"]);
        in_origin(&origin, &["branch", "-D", "evil"]);
        fetch_local(&mirror);
        assert_eq!(resolve(&mirror, "stable").unwrap().sha, evil);
        assert!(resolve(&mirror, "old").is_err());
        assert!(resolve(&mirror, "origin/evil").is_err());
        assert_eq!(default_branch(&mirror), "origin/main");
        std::fs::remove_dir_all(origin.parent().unwrap()).unwrap();
    }

    #[test]
    fn revisions_are_spelled_out() {
        let id = "0f3a9c1d2e4b5a69788796a5b4c3d2e1f0a9b8c7";
        assert_eq!(full_rev("origin/main"), "refs/remotes/origin/main");
        assert_eq!(full_rev("origin/feature/x"), "refs/remotes/origin/feature/x");
        assert_eq!(full_rev("v1.2"), "refs/tags/v1.2");
        assert_eq!(full_rev("main"), "refs/tags/main");
        assert_eq!(full_rev("refs/heads/main"), "refs/tags/refs/heads/main");
        assert_eq!(full_rev("refs/tags/origin/main"), "refs/tags/origin/main");
        assert_eq!(full_rev(id), id);
        assert_eq!(short_rev("refs/remotes/origin/main"), "origin/main");
        assert_eq!(short_rev("refs/tags/v1.2"), "v1.2");
        assert_eq!(short_rev(id), id);
    }

    #[test]
    fn urls_with_secrets_are_refused() {
        for ok in [
            "git@github.com:org/app.git",
            "https://github.com/org/app",
            "https://user@bitbucket.org/team/app.git",
            "ssh://git@[fd00::1]:2222/org/app.git",
            "https://git.example.com:8443/org/app.git",
        ] {
            assert!(check_no_credentials(ok).is_ok(), "{ok}");
        }
        for bad in ["https://user:ghp_secret@github.com/org/app.git", "https://x-access-token:abc@github.com/org/app"] {
            let err = check_no_credentials(bad).unwrap_err().to_string();
            assert!(!err.contains("secret") && !err.contains("abc@"), "{err}");
        }
    }

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
