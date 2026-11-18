use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Directory for everything the app owns: settings, the project registry,
/// git mirrors, deploy worktrees, run history and run logs.
/// `~/Library/Application Support/kgui` on macOS.
pub fn data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from("/")).join(".local/share"))
        .join("kgui")
}

/// Create the data dir if needed and keep it private to the user: run logs,
/// deploy checkouts and the `kgui` shim that kamal runs all live in it.
pub fn ensure_data_dir() -> std::io::Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let dir = data_dir();
    std::fs::create_dir_all(&dir)?;
    let mut perms = std::fs::metadata(&dir)?.permissions();
    if perms.mode() & 0o077 != 0 {
        perms.set_mode(0o700);
        std::fs::set_permissions(&dir, perms)?;
    }
    Ok(dir)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// 1Password account used for `op://` references (sign-in address, e.g.
    /// `example.1password.com`). Empty uses op's default account.
    pub op_account: String,
    /// Absolute path to the kamal executable. Auto-detected when unset.
    pub kamal_path: Option<PathBuf>,
    /// GitHub owner (org or user) shown first when browsing repositories.
    pub github_owner: String,
}

impl Settings {
    pub fn path() -> PathBuf {
        data_dir().join("settings.json")
    }

    pub fn load() -> Self {
        std::fs::read_to_string(Self::path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path();
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(&path, serde_json::to_string_pretty(self)?)
            .with_context(|| format!("could not write {}", path.display()))
    }
}
