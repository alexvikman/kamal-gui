//! The `kgui` command in the user's own terminal.
//!
//! The app binary is also the CLI, and at every start the app points its own
//! link (`<data dir>/bin/kgui`) at the binary that is running. A symlink
//! `~/.local/bin/kgui` to that link therefore follows the app: through
//! updates, and to wherever the app is moved. A separate copy of the CLI
//! (`cargo install`, the nightly tarball) stays at the version it was
//! installed at, and falls behind the app.

use crate::{env, settings};
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

/// The app's own link. Its directory is first on PATH for the kamal processes
/// the app starts, and the terminal's link points here.
pub fn shim_path() -> PathBuf {
    settings::data_dir().join("bin").join("kgui")
}

/// `~/.local/bin/kgui`: the only place a link is ever installed.
pub fn link_path() -> Result<PathBuf> {
    Ok(dirs::home_dir().context("no home folder")?.join(".local/bin/kgui"))
}

/// A path the way it is typed in a terminal: `~/.local/bin/kgui`.
pub fn tilde(path: &Path) -> String {
    match dirs::home_dir().and_then(|home| path.strip_prefix(home).ok().map(Path::to_path_buf)) {
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

/// Point the app's own link at `exe`, the app that is running.
pub fn point_shim(shim: &Path, exe: &Path) -> Result<()> {
    if std::fs::read_link(shim).ok().as_deref() == Some(exe) {
        return Ok(());
    }
    symlink_over(exe, shim)
}

/// What is at `~/.local/bin/kgui`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Link {
    Missing,
    /// The symlink to the app's own link.
    Ours,
    /// A symlink to somewhere else.
    OtherLink(PathBuf),
    /// A file of its own: a separate copy of the CLI.
    File,
}

pub fn inspect(link: &Path, shim: &Path) -> Link {
    match std::fs::read_link(link) {
        Ok(target) if target == shim => Link::Ours,
        Ok(target) => Link::OtherLink(target),
        Err(_) if link.symlink_metadata().is_ok() => Link::File,
        Err(_) => Link::Missing,
    }
}

/// Install the link at `~/.local/bin/kgui`. What is already there (a copy of
/// the CLI, a link to somewhere else) is only replaced with `replace`, which
/// the user has confirmed.
pub fn install(replace: bool) -> Result<()> {
    install_at(&link_path()?, &shim_path(), replace)
}

pub fn install_at(link: &Path, shim: &Path, replace: bool) -> Result<()> {
    if inspect(link, shim) == Link::Ours {
        return Ok(());
    }
    // Made when the app starts. Without it the link would lead nowhere.
    if shim.metadata().is_err() {
        bail!("the app's own link {} is missing; restart the app and try again", shim.display());
    }
    if replace {
        return symlink_over(shim, link);
    }
    if let Some(dir) = link.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("could not create {}", dir.display()))?;
    }
    // Fails when something is there by now: nothing is replaced unasked.
    std::os::unix::fs::symlink(shim, link).with_context(|| format!("could not create {}", link.display()))
}

/// Make `at` a symlink to `target`, in place of the file or link that is
/// there. One rename, so there is a working `kgui` at every moment. A folder
/// is never replaced (the rename fails).
fn symlink_over(target: &Path, at: &Path) -> Result<()> {
    let dir = at.parent().context("no parent folder")?;
    std::fs::create_dir_all(dir).with_context(|| format!("could not create {}", dir.display()))?;
    let tmp = dir.join(format!(".kgui-link-{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let placed = std::os::unix::fs::symlink(target, &tmp).and_then(|()| std::fs::rename(&tmp, at));
    if placed.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    placed.with_context(|| format!("could not replace {}", at.display()))
}

/// How `kgui` stands in the user's terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    /// `~/.local/bin/kgui`, and what is there.
    pub path: PathBuf,
    pub link: Link,
    /// The `kgui` a terminal runs: the first one on the shell's PATH.
    pub used: Option<PathBuf>,
    /// The binary that one leads to, with links followed.
    pub target: Option<PathBuf>,
    /// It is this app, so it follows the app's updates.
    pub follows_app: bool,
    /// `~/.local/bin` is on the shell's PATH.
    pub dir_on_path: bool,
}

impl Status {
    /// The terminal runs `~/.local/bin/kgui` rather than one found earlier.
    pub fn uses_link(&self) -> bool {
        self.used.as_deref() == Some(self.path.as_path())
    }
}

/// Asks the login shell for its PATH, which takes a moment: not for the UI
/// thread.
pub fn status() -> Result<Status> {
    let app = std::env::current_exe().and_then(std::fs::canonicalize).context("can't tell where the app is")?;
    // The shell's PATH as it is. `login_path` adds folders the shell may lack.
    let path = env::shell_path().unwrap_or_else(|| env::login_path().to_string());
    Ok(status_in(&link_path()?, &shim_path(), &app, &path))
}

/// `app` is the running binary with links followed, `path` the PATH of the
/// user's shell.
pub fn status_in(link: &Path, shim: &Path, app: &Path, path: &str) -> Status {
    let used = on_path(path).into_iter().next();
    let target = used.as_ref().and_then(|p| std::fs::canonicalize(p).ok());
    Status {
        path: link.to_path_buf(),
        link: inspect(link, shim),
        follows_app: target.as_deref() == Some(app),
        dir_on_path: path.split(':').any(|dir| Some(Path::new(dir)) == link.parent()),
        used,
        target,
    }
}

/// Every `kgui` on `path`, in the order a shell finds them. Relative entries
/// depend on the folder the terminal is in and are left out.
pub fn on_path(path: &str) -> Vec<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let mut found: Vec<PathBuf> = Vec::new();
    for dir in path.split(':').filter(|d| d.starts_with('/')) {
        let candidate = Path::new(dir).join("kgui");
        // Follows links, as running it does: a link that leads nowhere is skipped.
        let runs = candidate.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0);
        if runs && !found.contains(&candidate) {
            found.push(candidate);
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    /// A stand-in for the home folder, the data dir and the app.
    struct World {
        root: PathBuf,
        /// `<home>/.local/bin/kgui`
        link: PathBuf,
        /// `<data dir>/bin/kgui`, pointing at `app`
        shim: PathBuf,
        app: PathBuf,
    }

    impl World {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!("kgui-link-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            // macOS's temp dir is itself behind a link (/var → /private/var).
            let root = std::fs::canonicalize(root).unwrap();
            let app = executable(&root.join("Kamal GUI.app/Contents/MacOS/kamal-gui"));
            let shim = root.join("data/bin/kgui");
            point_shim(&shim, &app).unwrap();
            Self { link: root.join("home/.local/bin/kgui"), shim, app, root }
        }

        fn state(&self) -> Link {
            inspect(&self.link, &self.shim)
        }

        fn install(&self, replace: bool) -> Result<()> {
            install_at(&self.link, &self.shim, replace)
        }

        /// The terminal's PATH: these folders under the root, in this order.
        fn status(&self, dirs: &[&str]) -> Status {
            let path: Vec<String> = dirs.iter().map(|d| self.root.join(d).display().to_string()).collect();
            status_in(&self.link, &self.shim, &self.app, &path.join(":"))
        }
    }

    impl Drop for World {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn executable(path: &Path) -> PathBuf {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.to_path_buf()
    }

    #[test]
    fn installs_where_nothing_is_and_makes_the_folder() {
        let w = World::new("missing");
        assert!(!w.link.parent().unwrap().exists());
        assert_eq!(w.state(), Link::Missing);
        w.install(false).unwrap();
        assert_eq!(w.state(), Link::Ours);
        assert_eq!(std::fs::read_link(&w.link).unwrap(), w.shim);
        assert_eq!(std::fs::canonicalize(&w.link).unwrap(), w.app);
    }

    #[test]
    fn installing_twice_changes_nothing() {
        let w = World::new("ours");
        w.install(false).unwrap();
        w.install(false).unwrap();
        w.install(true).unwrap();
        assert_eq!(w.state(), Link::Ours);
        let names: Vec<_> =
            std::fs::read_dir(w.link.parent().unwrap()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names, ["kgui"]);
    }

    #[test]
    fn a_separate_copy_is_only_replaced_when_confirmed() {
        let w = World::new("file");
        executable(&w.link);
        assert_eq!(w.state(), Link::File);
        assert!(w.install(false).is_err());
        assert_eq!(std::fs::read_to_string(&w.link).unwrap(), "#!/bin/sh\n");
        w.install(true).unwrap();
        assert_eq!(w.state(), Link::Ours);
    }

    #[test]
    fn someone_elses_link_is_only_replaced_when_confirmed() {
        let w = World::new("other-link");
        let other = executable(&w.root.join("cargo/bin/kgui"));
        std::fs::create_dir_all(w.link.parent().unwrap()).unwrap();
        symlink(&other, &w.link).unwrap();
        assert_eq!(w.state(), Link::OtherLink(other.clone()));
        assert!(w.install(false).is_err());
        assert_eq!(std::fs::read_link(&w.link).unwrap(), other);
        w.install(true).unwrap();
        assert_eq!(w.state(), Link::Ours);
        // Only the link went; what it pointed at is not ours to remove.
        assert!(other.exists());
    }

    #[test]
    fn a_folder_is_never_replaced() {
        let w = World::new("folder");
        std::fs::create_dir_all(w.link.join("inside")).unwrap();
        assert!(w.install(false).is_err());
        assert!(w.install(true).is_err());
        assert!(w.link.join("inside").is_dir());
        assert_eq!(std::fs::read_dir(w.link.parent().unwrap()).unwrap().count(), 1);
    }

    #[test]
    fn nothing_is_installed_without_the_apps_own_link() {
        let w = World::new("no-shim");
        std::fs::remove_file(&w.shim).unwrap();
        assert!(w.install(false).is_err());
        assert_eq!(w.state(), Link::Missing);
    }

    /// What an update or a moved app comes down to: the app's own link is
    /// pointed at the new binary, and the terminal's link follows.
    #[test]
    fn the_link_follows_the_app() {
        let w = World::new("follows");
        w.install(false).unwrap();
        let moved = executable(&w.root.join("elsewhere/Kamal GUI.app/Contents/MacOS/kamal-gui"));
        point_shim(&w.shim, &moved).unwrap();
        assert_eq!(std::fs::canonicalize(&w.link).unwrap(), moved);
        assert_eq!(w.state(), Link::Ours);
        point_shim(&w.shim, &moved).unwrap();
        assert_eq!(std::fs::read_link(&w.shim).unwrap(), moved);
    }

    #[test]
    fn tells_the_apps_kgui_from_a_separate_copy() {
        let w = World::new("status");
        let cargo = executable(&w.root.join("home/.cargo/bin/kgui"));

        // Only a separate copy, and no link yet.
        let s = w.status(&["home/.local/bin", "home/.cargo/bin"]);
        assert_eq!((s.link.clone(), s.used.clone(), s.follows_app), (Link::Missing, Some(cargo.clone()), false));
        assert!(s.dir_on_path && !s.uses_link());

        // With the link first on PATH, the terminal runs the app.
        w.install(false).unwrap();
        let s = w.status(&["home/.local/bin", "home/.cargo/bin"]);
        assert_eq!(
            (s.used.clone(), s.target.clone(), s.follows_app),
            (Some(w.link.clone()), Some(w.app.clone()), true)
        );
        assert!(s.uses_link());

        // A copy earlier on PATH still wins.
        let s = w.status(&["home/.cargo/bin", "home/.local/bin"]);
        assert_eq!((s.link.clone(), s.used.clone(), s.follows_app), (Link::Ours, Some(cargo), false));
        assert!(s.dir_on_path && !s.uses_link());

        // Installed, but the folder isn't on PATH: the terminal finds nothing.
        let s = w.status(&["usr/bin"]);
        assert_eq!((s.link, s.used, s.follows_app, s.dir_on_path), (Link::Ours, None, false, false));
    }

    #[test]
    fn a_link_to_another_copy_of_the_app_does_not_follow_this_one() {
        let w = World::new("other-app");
        w.install(false).unwrap();
        let other = executable(&w.root.join("target/debug/kamal-gui"));
        point_shim(&w.shim, &other).unwrap();
        let s = w.status(&["home/.local/bin"]);
        assert_eq!((s.link.clone(), s.target.clone(), s.follows_app), (Link::Ours, Some(other), false));
        assert!(s.uses_link());
    }

    #[test]
    fn finds_what_a_shell_would_run() {
        let w = World::new("on-path");
        let a = executable(&w.root.join("a/kgui"));
        let b = executable(&w.root.join("b/kgui"));
        // Not executable, a folder, and a link that leads nowhere: all skipped.
        std::fs::create_dir_all(w.root.join("c")).unwrap();
        std::fs::write(w.root.join("c/kgui"), "").unwrap();
        std::fs::create_dir_all(w.root.join("d/kgui")).unwrap();
        std::fs::create_dir_all(w.root.join("e")).unwrap();
        symlink(w.root.join("gone"), w.root.join("e/kgui")).unwrap();
        let dir = |d: &str| w.root.join(d).display().to_string();
        let path = [dir("c"), dir("d"), dir("e"), "bin".into(), dir("b"), dir("nowhere"), dir("a"), dir("b")].join(":");
        assert_eq!(on_path(&path), [b, a]);
        assert!(on_path("").is_empty());
    }
}
