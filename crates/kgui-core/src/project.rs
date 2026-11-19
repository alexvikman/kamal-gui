//! Projects are git repositories added to the app by URL. The app keeps its
//! own bare clone of each one and reads deploy configs from origin's default
//! branch, so nothing depends on a working copy on disk.
//!
//! Each `config/deploy*.yml` becomes a deploy unit, grouped into targets:
//!
//! - `deploy.yml` + `deploy.workers.yml` → target `default` (web + workers)
//! - `deploy.ff.yml` without `servers`/`registry`/`builder` → destination
//!   overlay of `deploy.yml`, run as `-c config/deploy.yml -d ff` (target `ff`)
//! - `deploy.acme.yml` with a full config → standalone
//!   `-c config/deploy.acme.yml` (target `acme`)

use crate::config::{self, UnitConfig};
use crate::git;
use crate::settings::data_dir;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

// ── Registry ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub name: String,
    pub url: String,
    pub added_at: i64,
}

fn registry_path() -> PathBuf {
    data_dir().join("projects.json")
}

pub fn list() -> Vec<Project> {
    let mut projects: Vec<Project> =
        std::fs::read_to_string(registry_path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    projects.sort_by(|a, b| a.name.cmp(&b.name));
    projects
}

fn save(projects: &[Project]) -> Result<()> {
    let path = registry_path();
    std::fs::create_dir_all(path.parent().unwrap())?;
    std::fs::write(&path, serde_json::to_string_pretty(projects)?)
        .with_context(|| format!("could not write {}", path.display()))
}

pub fn find(name: &str) -> Result<Project> {
    list().into_iter().find(|p| p.name == name).with_context(|| format!("no project named {name}"))
}

/// `git@github.com:org/app.git`, `https://github.com/org/app` → `app`.
pub fn name_from_url(url: &str) -> Option<String> {
    let last = url.trim().trim_end_matches('/').rsplit(['/', ':']).next()?;
    let name = last.strip_suffix(".git").unwrap_or(last);
    let ok = !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c));
    (ok && name != "." && name != "..").then(|| name.to_string())
}

/// Guards read-modify-write of `projects.json`, so several projects can be
/// added at once.
static REGISTRY: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn registry_lock() -> std::sync::MutexGuard<'static, ()> {
    REGISTRY.lock().unwrap_or_else(|e| e.into_inner())
}

fn ensure_new(projects: &[Project], name: &str) -> Result<()> {
    if let Some(existing) = projects.iter().find(|p| p.name == name) {
        bail!("a project named {name} already exists ({})", existing.url);
    }
    Ok(())
}

/// Clone the repository and add it to the registry. Safe to call for
/// several repositories in parallel: the clone runs outside the registry lock.
pub fn add(url: &str) -> Result<Project> {
    let url = url.trim();
    let name = name_from_url(url).with_context(|| format!("can't derive a project name from {url}"))?;
    ensure_new(&list(), &name)?;
    git::ensure_mirror(&name, url)?;
    let _guard = registry_lock();
    let mut projects = list();
    ensure_new(&projects, &name)?;
    let project = Project { name, url: url.to_string(), added_at: crate::history::now_ms() };
    projects.push(project.clone());
    save(&projects)?;
    Ok(project)
}

/// Remove from the registry and delete the app's clone and worktree.
/// Run history is kept.
pub fn remove(name: &str) -> Result<()> {
    let guard = registry_lock();
    let mut projects = list();
    let before = projects.len();
    projects.retain(|p| p.name != name);
    if projects.len() == before {
        bail!("no project named {name}");
    }
    save(&projects)?;
    drop(guard);
    for dir in [git::worktree_dir(name), git::mirror_dir(name)] {
        if dir.exists() {
            std::fs::remove_dir_all(&dir).with_context(|| format!("could not delete {}", dir.display()))?;
        }
    }
    Ok(())
}

pub fn fetch(name: &str) -> Result<()> {
    let p = find(name)?;
    git::ensure_mirror(&p.name, &p.url)?;
    Ok(())
}

// ── Deploy units ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct Unit {
    /// `config/deploy.workers.yml` or `config/deploy.yml@ff`.
    pub id: String,
    /// `web` or `workers`.
    pub kind: String,
    pub config_file: String,
    pub destination: Option<String>,
    pub config: UnitConfig,
    /// External image the built stage starts from (from the Dockerfile).
    pub base_image: Option<String>,
}

impl Unit {
    pub fn kamal_args(&self) -> Vec<String> {
        let mut a = vec!["-c".to_string(), self.config_file.clone()];
        if let Some(d) = &self.destination {
            a.push("-d".into());
            a.push(d.clone());
        }
        a
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Target {
    pub id: String,
    pub units: Vec<Unit>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectDetail {
    pub project: Project,
    /// `origin/main` (whatever origin's HEAD points at).
    pub default_branch: String,
    pub commit: Option<git::Commit>,
    pub targets: Vec<Target>,
    pub errors: Vec<String>,
}

fn is_deploy_file(name: &str) -> bool {
    name.starts_with("deploy") && name.ends_with(".yml")
}

fn kind_of(stem: &str) -> &'static str {
    if stem.split('.').any(|p| p == "workers") { "workers" } else { "web" }
}

fn group_of(stem: &str) -> String {
    let g: Vec<&str> = stem.split('.').filter(|p| *p != "deploy" && *p != "workers").collect();
    if g.is_empty() { "default".into() } else { g.join(".") }
}

/// Build targets from the file names in `config/` and a reader for
/// `config/<name>` contents.
pub fn build_targets(files: &[String], read: impl Fn(&str) -> Option<String>) -> (Vec<Target>, Vec<String>) {
    let names: BTreeSet<&str> = files.iter().map(|s| s.as_str()).filter(|n| is_deploy_file(n)).collect();
    let mut groups: BTreeMap<String, Vec<Unit>> = BTreeMap::new();
    let mut errors = Vec::new();

    for name in &names {
        let stem = name.trim_end_matches(".yml");
        let parsed = match read(&format!("config/{name}")).map(|t| config::parse(&t)) {
            Some(Ok(v)) => v,
            Some(Err(e)) => {
                errors.push(format!("config/{name}: {e:#}"));
                continue;
            }
            None => continue,
        };

        let full = config::has_key(&parsed, "servers")
            && (config::has_key(&parsed, "registry") || config::has_key(&parsed, "builder"));
        let overlay_of = stem
            .rsplit_once('.')
            .filter(|(base, dest)| *dest != "workers" && names.contains(format!("{base}.yml").as_str()));

        let (unit, group) = match overlay_of {
            Some((base_stem, dest)) if !full => {
                let base_file = format!("{base_stem}.yml");
                let mut merged = match read(&format!("config/{base_file}")).map(|t| config::parse(&t)) {
                    Some(Ok(v)) => v,
                    _ => {
                        errors.push(format!("config/{name}: can't read base file {base_file}"));
                        continue;
                    }
                };
                config::deep_merge(&mut merged, parsed);
                let config_file = format!("config/{base_file}");
                (
                    Unit {
                        id: format!("{config_file}@{dest}"),
                        kind: kind_of(base_stem).into(),
                        config_file,
                        destination: Some(dest.into()),
                        config: config::extract(&merged),
                        base_image: None,
                    },
                    dest.to_string(),
                )
            }
            _ => {
                let config_file = format!("config/{name}");
                (
                    Unit {
                        id: config_file.clone(),
                        kind: kind_of(stem).into(),
                        config_file,
                        destination: None,
                        config: config::extract(&parsed),
                        base_image: None,
                    },
                    group_of(stem),
                )
            }
        };
        groups.entry(group).or_default().push(unit);
    }

    let mut dockerfiles: HashMap<String, Option<String>> = HashMap::new();
    for unit in groups.values_mut().flatten() {
        let path = unit.config.dockerfile.clone().unwrap_or_else(|| "Dockerfile".into());
        // The path comes from the config; only ever read inside the repository.
        let inside = Path::new(&path).components().all(|c| matches!(c, std::path::Component::Normal(_)));
        let text = dockerfiles.entry(path.clone()).or_insert_with(|| if inside { read(&path) } else { None });
        unit.base_image =
            text.as_deref().and_then(|t| crate::images::base_image(t, unit.config.build_target.as_deref()));
    }

    let mut targets: Vec<Target> = groups
        .into_iter()
        .map(|(id, mut units)| {
            units.sort_by_key(|u| (u.kind != "web", u.id.clone()));
            Target { id, units }
        })
        .collect();
    targets.sort_by_key(|t| (t.id != "default", t.id.clone()));
    (targets, errors)
}

/// Targets as defined on origin's default branch, read from the app's clone.
pub fn load_detail(project: Project) -> Result<ProjectDetail> {
    let mirror = git::mirror_dir(&project.name);
    if !mirror.join("HEAD").exists() {
        git::ensure_mirror(&project.name, &project.url)?;
    }
    let rev = git::default_branch(&mirror);
    let files = git::list_config_files(&mirror, &rev).unwrap_or_default();
    let (targets, mut errors) = build_targets(&files, |p| git::show(&mirror, &rev, p));
    if targets.is_empty() && errors.is_empty() {
        errors.push(format!("no config/deploy*.yml on {rev}"));
    }
    Ok(ProjectDetail { commit: git::resolve(&mirror, &rev).ok(), default_branch: rev, targets, errors, project })
}

/// Targets from config files on disk (the deploy worktree).
pub fn targets_in_dir(dir: &Path) -> (Vec<Target>, Vec<String>) {
    let files: Vec<String> = std::fs::read_dir(dir.join("config"))
        .map(|rd| rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    build_targets(&files, |p| std::fs::read_to_string(dir.join(p)).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str =
        "service: s\nimage: i\nservers:\n  web:\n    - 10.0.0.1\nregistry:\n  username: u\nbuilder:\n  arch: amd64\n";
    const WORKERS: &str =
        "service: s-workers\nservers:\n  horizon:\n    hosts:\n      - 10.0.0.1\nbuilder:\n  arch: amd64\n";

    fn targets(files: &[(&str, &str)]) -> Vec<Target> {
        let names: Vec<String> = files.iter().map(|(n, _)| n.to_string()).collect();
        let (t, errors) = build_targets(&names, |p| {
            files.iter().find(|(n, _)| format!("config/{n}") == p).map(|(_, c)| c.to_string())
        });
        assert!(errors.is_empty(), "{errors:?}");
        t
    }

    #[test]
    fn names_from_urls() {
        assert_eq!(name_from_url("git@github.com:org/my-app.git").as_deref(), Some("my-app"));
        assert_eq!(name_from_url("https://github.com/org/app/").as_deref(), Some("app"));
        assert_eq!(name_from_url("https://github.com/org/app.git").as_deref(), Some("app"));
        assert_eq!(name_from_url("../x/.."), None);
    }

    #[test]
    fn dockerfile_outside_the_repo_is_not_read() {
        for path in ["/etc/hosts", "../../secret/Dockerfile"] {
            let config = format!("{FULL}  dockerfile: {path}\n");
            let (t, _) = build_targets(&["deploy.yml".to_string()], |p| match p {
                "config/deploy.yml" => Some(config.clone()),
                other => panic!("read {other}"),
            });
            assert_eq!(t[0].units[0].base_image, None);
        }
    }

    #[test]
    fn web_and_workers() {
        let t = targets(&[("deploy.yml", FULL), ("deploy.workers.yml", WORKERS)]);
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].id, "default");
        assert_eq!(t[0].units[0].kind, "web");
        assert_eq!(t[0].units[1].kind, "workers");
        assert_eq!(t[0].units[1].kamal_args(), vec!["-c", "config/deploy.workers.yml"]);
    }

    #[test]
    fn destinations() {
        let t = targets(&[
            ("deploy.yml", FULL),
            ("deploy.workers.yml", WORKERS),
            ("deploy.ff.yml", "service: ff\n"),
            ("deploy.workers.ff.yml", "service: ff-workers\n"),
        ]);
        let ff = t.iter().find(|t| t.id == "ff").unwrap();
        assert_eq!(ff.units.len(), 2);
        assert_eq!(ff.units[0].kamal_args(), vec!["-c", "config/deploy.yml", "-d", "ff"]);
        assert_eq!(ff.units[0].config.service.as_deref(), Some("ff"));
        assert_eq!(ff.units[0].config.servers[0].host, "10.0.0.1");
        assert_eq!(ff.units[1].kind, "workers");
    }

    #[test]
    fn standalone_tenants() {
        let t = targets(&[("deploy.yml", FULL), ("deploy.acme.yml", FULL), ("deploy.acme.workers.yml", WORKERS)]);
        let acme = t.iter().find(|t| t.id == "acme").unwrap();
        assert_eq!(acme.units[0].kamal_args(), vec!["-c", "config/deploy.acme.yml"]);
        assert_eq!(acme.units[1].kamal_args(), vec!["-c", "config/deploy.acme.workers.yml"]);
    }
}
