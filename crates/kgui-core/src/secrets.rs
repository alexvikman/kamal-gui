//! 1Password secrets for Kamal.
//!
//! `.kamal/secrets` lines look like
//! `APP_KEY=$(kgui secret op://deploy/my-app/APP_KEY)`.
//!
//! Before running kamal, the app scans those references, fetches every
//! 1Password item once (one Touch ID prompt, one `op` call per item), and
//! hands the values to kamal's process as `KGUI_SECRET_<hex(ref)>` env vars.
//! `kgui secret` then answers from the environment. Run from a terminal
//! without the app, `kgui secret` falls back to calling `op` itself.
//! Values are only ever held in memory.

use crate::env::{command, run};
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SecretRef {
    pub vault: String,
    pub item: String,
    pub section: Option<String>,
    pub field: String,
}

impl SecretRef {
    /// `op://vault/item/field` or `op://vault/item/section/field`.
    pub fn parse(s: &str) -> Result<Self> {
        let rest = s.strip_prefix("op://").with_context(|| format!("not an op:// reference: {s}"))?;
        let parts: Vec<&str> = rest.split('/').collect();
        match parts.as_slice() {
            [v, i, f] if ![v, i, f].iter().any(|p| p.is_empty()) => {
                Ok(Self { vault: v.to_string(), item: i.to_string(), section: None, field: f.to_string() })
            }
            [v, i, s, f] if ![v, i, s, f].iter().any(|p| p.is_empty()) => Ok(Self {
                vault: v.to_string(),
                item: i.to_string(),
                section: Some(s.to_string()),
                field: f.to_string(),
            }),
            _ => bail!("invalid op:// reference: {s} (expected op://vault/item/field)"),
        }
    }

    pub fn env_key(&self) -> String {
        let hex: String = self.to_string().bytes().map(|b| format!("{b:02X}")).collect();
        format!("KGUI_SECRET_{hex}")
    }
}

impl std::fmt::Display for SecretRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.section {
            Some(s) => write!(f, "op://{}/{}/{}/{}", self.vault, self.item, s, self.field),
            None => write!(f, "op://{}/{}/{}", self.vault, self.item, self.field),
        }
    }
}

/// All `kgui secret op://…` references in a secrets file.
pub fn scan(text: &str) -> Vec<SecretRef> {
    static RE: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r#"kgui\s+secret\s+["']?(op://[^\s"')]+)"#).unwrap());
    RE.captures_iter(text).filter_map(|c| SecretRef::parse(&c[1]).ok()).collect()
}

/// Secrets files Kamal may read: `.kamal/secrets`, `.kamal/secrets.<dest>`,
/// `.kamal/secrets-common`, and anything a config points at with `secrets_path`
/// as long as it lives in `.kamal/`.
fn secret_files(dir: &Path) -> Vec<std::path::PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir.join(".kamal")) else { return vec![] };
    rd.flatten()
        .map(|e| e.path())
        .filter(|p| {
            let n = p.file_name().unwrap_or_default().to_string_lossy();
            p.is_file() && n.starts_with("secrets") && !n.ends_with(".example")
        })
        .collect()
}

/// Whether any secrets file still calls the old bash adapter.
pub fn uses_legacy_adapter(dir: &Path) -> bool {
    secret_files(dir).iter().any(|p| std::fs::read_to_string(p).is_ok_and(|t| t.contains("op-read-item-field")))
}

/// Files Kamal reads secrets from for one config: `<path>-common` and
/// `<path>` (or `<path>.<destination>`), `<path>` defaulting to `.kamal/secrets`.
pub fn kamal_secret_files(secrets_path: Option<&str>, destination: Option<&str>) -> [String; 2] {
    let base = secrets_path.unwrap_or(".kamal/secrets");
    let main = match destination {
        Some(d) => format!("{base}.{d}"),
        None => base.to_string(),
    };
    [main, format!("{base}-common")]
}

/// Explain why Kamal won't find secrets for a config in `dir`, when the
/// checkout clearly expects a secrets file (an `.example` is committed or
/// `.gitignore` excludes it) but none is there. None when a file exists or
/// nothing suggests the config uses secrets.
pub fn missing_files_problem(dir: &Path, secrets_path: Option<&str>, destination: Option<&str>) -> Option<String> {
    let files = kamal_secret_files(secrets_path, destination);
    if files.iter().any(|f| dir.join(f).is_file()) {
        return None;
    }
    let main = &files[0];
    let example = dir.join(format!("{main}.example")).is_file();
    let ignored =
        command("git").arg("-C").arg(dir).args(["check-ignore", "-q", main]).status().is_ok_and(|s| s.success());
    if !example && !ignored {
        return None;
    }
    let mut msg = format!("{main} is not in this commit");
    if example {
        msg.push_str(&format!(" (only {main}.example is)"));
    }
    msg.push('.');
    if ignored {
        msg.push_str(&format!(" The repo's .gitignore still excludes {main}."));
    }
    msg.push_str(&format!(
        " Kamal can't read secrets without it. Commit {main} with `$(kgui secret op://…)` references (no values) and deploy again."
    ));
    Some(msg)
}

pub fn scan_dir(dir: &Path) -> Vec<SecretRef> {
    let set: BTreeSet<SecretRef> =
        secret_files(dir).iter().filter_map(|p| std::fs::read_to_string(p).ok()).flat_map(|t| scan(&t)).collect();
    set.into_iter().collect()
}

#[derive(Deserialize)]
struct OpItem {
    #[serde(default)]
    fields: Vec<OpField>,
}

#[derive(Deserialize)]
struct OpField {
    #[serde(default)]
    label: String,
    #[serde(default)]
    value: Option<serde_json::Value>,
    #[serde(default)]
    section: Option<OpSection>,
}

#[derive(Deserialize)]
struct OpSection {
    #[serde(default)]
    label: Option<String>,
}

pub struct Item {
    fields: Vec<OpField>,
}

impl Item {
    /// Value of a field. Missing field is an error; an empty field is "".
    pub fn get(&self, r: &SecretRef) -> Result<String> {
        let f = self
            .fields
            .iter()
            .find(|f| {
                f.label == r.field
                    && match &r.section {
                        Some(s) => f.section.as_ref().and_then(|x| x.label.as_deref()) == Some(s),
                        None => true,
                    }
            })
            .with_context(|| format!("field not found in 1Password: {r}"))?;
        Ok(match &f.value {
            None | Some(serde_json::Value::Null) => String::new(),
            Some(serde_json::Value::String(s)) => s.clone(),
            Some(other) => other.to_string(),
        })
    }
}

pub fn fetch_item(account: &str, vault: &str, item: &str) -> Result<Item> {
    let mut cmd = command("op");
    cmd.args(["item", "get", item, "--vault", vault, "--format", "json"]);
    if !account.is_empty() {
        cmd.args(["--account", account]);
    }
    let out = run(&mut cmd).with_context(|| format!("op could not read {vault}/{item}"))?;
    let parsed: OpItem = serde_json::from_str(&out).context("unexpected output from op")?;
    Ok(Item { fields: parsed.fields })
}

/// Resolve references, fetching each item once. Returns `(env_key, value)`.
///
/// Items are fetched one after another: 1Password asks to authorize `op`
/// once, and calls made in parallel before that answer each get their own
/// prompt.
pub fn resolve_all(account: &str, refs: &[SecretRef]) -> Result<Vec<(String, String)>> {
    let mut by_item: BTreeMap<(String, String), Vec<&SecretRef>> = BTreeMap::new();
    for r in refs {
        by_item.entry((r.vault.clone(), r.item.clone())).or_default().push(r);
    }
    let items: HashMap<(String, String), Result<Item>> =
        by_item.keys().map(|key| (key.clone(), fetch_item(account, &key.0, &key.1))).collect();

    let mut out = Vec::new();
    let mut errors = Vec::new();
    for ((vault, item), rs) in &by_item {
        match &items[&(vault.clone(), item.clone())] {
            Ok(it) => {
                for r in rs {
                    match it.get(r) {
                        Ok(v) => out.push((r.env_key(), v)),
                        Err(e) => errors.push(format!("{e:#}")),
                    }
                }
            }
            Err(e) => errors.push(format!("{e:#}")),
        }
    }
    if !errors.is_empty() {
        bail!("{}", errors.join("\n"));
    }
    Ok(out)
}

/// `kgui secret <ref>`: from the environment if the app provided it,
/// otherwise straight from 1Password.
pub fn lookup(reference: &str, account: &str) -> Result<String> {
    let r = SecretRef::parse(reference)?;
    if let Ok(v) = std::env::var(r.env_key()) {
        return Ok(v);
    }
    fetch_item(account, &r.vault, &r.item)?.get(&r)
}

/// Rewrite calls to the old bash adapter to `$(kgui secret op://VAULT/ITEM/FIELD)`.
///
/// Handles both styles in use:
/// - `$(bash ~/…/op-read-item-field ITEM FIELD VAULT)`
/// - `$(OP_ACCOUNT=$OP_ACCOUNT bash $OP_HELPER $OP_ITEM FIELD $OP_VAULT)` with
///   `OP_HELPER=…`, `OP_ITEM=…` etc. assigned earlier in the same file. Those
///   helper assignments are dropped once nothing references them.
///
/// Returns None if nothing changed.
pub fn migrate_legacy(text: &str) -> Option<String> {
    // `NAME=value` or `export NAME=value` (literal values only).
    let assign = regex::Regex::new(r"^(?:export\s+)?([A-Z_][A-Z0-9_]*)=([^\s$()]+)\s*$").unwrap();
    let vars: HashMap<&str, &str> = text
        .lines()
        .filter_map(|l| assign.captures(l.trim()))
        .map(|c| (c.get(1).unwrap().as_str(), c.get(2).unwrap().as_str()))
        .collect();
    let expand = |s: &str| -> Option<String> {
        let name = s.strip_prefix('$')?;
        let name = name.strip_prefix('{').and_then(|n| n.strip_suffix('}')).unwrap_or(name);
        vars.get(name).map(|v| v.to_string())
    };
    let value = |s: &str| if s.starts_with('$') { expand(s) } else { Some(s.to_string()) };

    let call = regex::Regex::new(
        r"\$\(\s*(?:[A-Z_]+=\S+\s+)*(?:bash\s+)?(\S*op-read-item-field|\$\{?OP_HELPER\}?)\s+(\S+)\s+(\S+)\s+([^\s)]+)\s*\)",
    )
    .unwrap();
    let mut changed = false;
    let replaced = call.replace_all(text, |c: &regex::Captures| {
        let helper_ok = !c[1].starts_with('$') || expand(&c[1]).is_some_and(|h| h.ends_with("op-read-item-field"));
        match (helper_ok, value(&c[2]), value(&c[3]), value(&c[4])) {
            (true, Some(item), Some(field), Some(vault)) => {
                changed = true;
                format!("$(kgui secret op://{vault}/{item}/{field})")
            }
            _ => c[0].to_string(),
        }
    });
    if !changed {
        return None;
    }

    // Drop helper variables nothing refers to any more.
    let helpers = ["OP_HELPER", "OP_ACCOUNT", "OP_VAULT", "OP_ITEM"];
    let unused: Vec<&str> = helpers
        .into_iter()
        .filter(|h| !replaced.contains(&format!("${h}")) && !replaced.contains(&format!("${{{h}}}")))
        .collect();
    let mut out: String = replaced
        .lines()
        .filter(|l| assign.captures(l.trim()).is_none_or(|c| !unused.contains(&c.get(1).unwrap().as_str())))
        .collect::<Vec<_>>()
        .join("\n");
    if text.ends_with('\n') {
        out.push('\n');
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_scan() {
        let t =
            "# c\nA=$(kgui secret op://deploy/app/APP_KEY)\nB=$(kgui secret \"op://deploy/docker-hub/PW\")\nC=$OTHER\n";
        let refs = scan(t);
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].item, "app");
        assert_eq!(refs[1].to_string(), "op://deploy/docker-hub/PW");
        assert!(refs[0].env_key().starts_with("KGUI_SECRET_6F703A2F2F"));
    }

    #[test]
    fn migrate_drops_unused_exported_helper_vars() {
        let t = "export OP_ACCOUNT=example.1password.com\nX=$(bash ~/scripts/op-read-item-field app KEY deploy)\n";
        assert_eq!(migrate_legacy(t).unwrap(), "X=$(kgui secret op://deploy/app/KEY)\n");
    }

    #[test]
    fn missing_secrets_file() {
        let dir = std::env::temp_dir().join(format!("kgui-secrets-test-{}", std::process::id()));
        std::fs::create_dir_all(dir.join(".kamal")).unwrap();
        assert!(missing_files_problem(&dir, None, None).is_none(), "no hint that secrets are used");
        std::fs::write(dir.join(".kamal/secrets.ff.example"), "").unwrap();
        let msg = missing_files_problem(&dir, None, Some("ff")).unwrap();
        assert!(msg.contains(".kamal/secrets.ff is not in this commit (only .kamal/secrets.ff.example is)"), "{msg}");
        std::fs::write(dir.join(".kamal/secrets-common"), "").unwrap();
        assert!(missing_files_problem(&dir, None, Some("ff")).is_none(), "common file is enough");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn migrate() {
        let t = "X=$(bash ~/scripts/kamal-1password-adapter/op-read-item-field my-app APP_KEY deploy)\nY=1\n";
        assert_eq!(migrate_legacy(t).unwrap(), "X=$(kgui secret op://deploy/my-app/APP_KEY)\nY=1\n");
        assert!(migrate_legacy("Y=1\n").is_none());
    }

    #[test]
    fn migrate_with_variables() {
        let t = "OP_ACCOUNT=example.1password.com\nOP_VAULT=deploy\nOP_ITEM=acme\nOP_HELPER=~/scripts/kamal-1password-adapter/op-read-item-field\n\nPW=$(OP_ACCOUNT=$OP_ACCOUNT bash $OP_HELPER docker-hub KAMAL_REGISTRY_PASSWORD $OP_VAULT)\nAPP_KEY=$(OP_ACCOUNT=$OP_ACCOUNT bash $OP_HELPER $OP_ITEM APP_KEY $OP_VAULT)\n";
        assert_eq!(
            migrate_legacy(t).unwrap(),
            "\nPW=$(kgui secret op://deploy/docker-hub/KAMAL_REGISTRY_PASSWORD)\nAPP_KEY=$(kgui secret op://deploy/acme/APP_KEY)\n"
        );
    }

    #[test]
    fn field_lookup() {
        let item: OpItem = serde_json::from_str(
            r#"{"fields":[{"label":"A","value":"x"},{"label":"B"},{"label":"C","value":"y","section":{"label":"s"}}]}"#,
        )
        .unwrap();
        let item = Item { fields: item.fields };
        let r = |s| SecretRef::parse(s).unwrap();
        assert_eq!(item.get(&r("op://v/i/A")).unwrap(), "x");
        assert_eq!(item.get(&r("op://v/i/B")).unwrap(), "");
        assert_eq!(item.get(&r("op://v/i/s/C")).unwrap(), "y");
        assert!(item.get(&r("op://v/i/Z")).is_err());
    }
}
