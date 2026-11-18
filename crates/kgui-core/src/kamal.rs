use crate::env::{command, run};
use crate::settings::Settings;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The kamal executable.
///
/// Version-manager shims (mise, rbenv, asdf) pick a Ruby based on the current
/// directory, and the app runs kamal in its own worktrees where no project
/// pins a Ruby. So look for a real kamal binary inside the installed Rubies
/// (newest Ruby first) before falling back to whatever `kamal` is on PATH.
pub fn binary(settings: &Settings) -> PathBuf {
    if let Some(p) = &settings.kamal_path {
        return p.clone();
    }
    if let Some(p) = installed_kamal() {
        return p;
    }
    if let Ok(p) = run(command("which").arg("kamal"))
        && !p.is_empty()
    {
        return PathBuf::from(p);
    }
    PathBuf::from("kamal")
}

fn installed_kamal() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    let mise = std::env::var_os("MISE_DATA_DIR").map(PathBuf::from).unwrap_or_else(|| home.join(".local/share/mise"));
    let roots = [
        mise.join("installs/ruby"),
        home.join(".rbenv/versions"),
        home.join(".asdf/installs/ruby"),
        home.join(".rubies"),
    ];
    let mut found: Vec<(Vec<u64>, PathBuf)> = roots
        .iter()
        .filter_map(|r| std::fs::read_dir(r).ok())
        .flat_map(|rd| rd.flatten())
        .map(|e| e.path())
        .filter(|dir| !dir.is_symlink())
        .filter_map(|dir| {
            let bin = dir.join("bin/kamal");
            bin.is_file().then(|| (version_key(&dir), bin))
        })
        .collect();
    found.sort();
    found.pop().map(|(_, p)| p)
}

/// `3.4.1` → [3, 4, 1] for ordering Ruby install directories.
fn version_key(dir: &Path) -> Vec<u64> {
    dir.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .trim_start_matches("ruby-")
        .split(|c: char| !c.is_ascii_digit())
        .filter_map(|p| p.parse().ok())
        .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    Deploy,
    Redeploy,
    Rollback { version: String },
    AppBoot,
    AppDetails,
    LockStatus,
    LockRelease,
    Custom { args: Vec<String> },
}

impl Action {
    pub fn kamal_args(&self) -> Vec<String> {
        let v: Vec<&str> = match self {
            Action::Deploy => vec!["deploy"],
            Action::Redeploy => vec!["redeploy"],
            Action::Rollback { version } => return vec!["rollback".into(), version.clone()],
            Action::AppBoot => vec!["app", "boot"],
            Action::AppDetails => vec!["app", "details"],
            Action::LockStatus => vec!["lock", "status"],
            Action::LockRelease => vec!["lock", "release"],
            Action::Custom { args } => return args.clone(),
        };
        v.into_iter().map(String::from).collect()
    }

    pub fn label(&self) -> String {
        match self {
            Action::Rollback { version } => format!("rollback {}", &version[..version.len().min(7)]),
            _ => self.kamal_args().join(" "),
        }
    }

    /// Deploys check out the requested ref. Everything else runs against the
    /// commit that is already in the deploy worktree (normally what's live).
    pub fn uses_ref(&self) -> bool {
        matches!(self, Action::Deploy | Action::Redeploy)
    }

    /// Prefetch secrets for commands that boot containers. Read-only commands
    /// skip it; Kamal loads secrets lazily, and `kgui secret` falls back to op.
    pub fn needs_secrets(&self) -> bool {
        !matches!(self, Action::AppDetails | Action::LockStatus | Action::LockRelease)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ruby_versions_sort_numerically() {
        let mut v =
            [version_key(Path::new("3.10.0")), version_key(Path::new("3.4.1")), version_key(Path::new("ruby-3.2.2"))];
        v.sort();
        assert_eq!(v[2], vec![3, 10, 0]);
        assert_eq!(v[0], vec![3, 2, 2]);
    }
}
