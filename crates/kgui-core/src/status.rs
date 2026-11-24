//! Live container status straight from Docker on each host, over the
//! system `ssh` (so `~/.ssh/config`, agents and jump hosts just work).
//! Kamal labels its containers with `service`, `role` and `destination`.

use crate::config::SshTarget;
use crate::env::command;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long one host may take to answer, including the SSH agent (1Password)
/// asking for approval on first use.
pub const HOST_TIMEOUT: Duration = Duration::from_secs(45);

/// What a status check is doing right now, for the UI. Cheap to clone; all
/// clones share the same steps.
#[derive(Clone, Default)]
pub struct Progress(Arc<Mutex<Vec<Step>>>);

#[derive(Debug, Clone)]
pub struct Step {
    /// e.g. "10.0.0.28" or "GitHub releases".
    pub label: String,
    pub started: Instant,
    pub done: bool,
}

impl Progress {
    fn start(&self, label: impl Into<String>) -> usize {
        let mut steps = self.0.lock().unwrap();
        steps.push(Step { label: label.into(), started: Instant::now(), done: false });
        steps.len() - 1
    }

    fn finish(&self, i: usize) {
        if let Some(step) = self.0.lock().unwrap().get_mut(i) {
            step.done = true;
        }
    }

    /// Steps that are still going.
    pub fn active(&self) -> Vec<Step> {
        self.0.lock().unwrap().iter().filter(|s| !s.done).cloned().collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Container {
    pub id: String,
    pub name: String,
    pub image: String,
    pub version: String,
    pub service: String,
    pub role: Option<String>,
    pub destination: Option<String>,
    pub state: String,
    pub status: String,
    pub created: String,
    /// `org.opencontainers.image.version`, usually inherited from the base image.
    pub image_version: Option<String>,
    /// `org.opencontainers.image.source` (where releases are published).
    pub image_source: Option<String>,
    /// CPU architecture and layers (`RootFS.Layers`) of a running container's
    /// image, to tell whether its base image tag has moved since.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub image_arch: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub image_layers: Vec<String>,
}

impl Container {
    /// Docker's `CreatedAt` (`2026-11-18 20:20:13 +0100 CET`) as Unix ms.
    pub fn created_ms(&self) -> Option<i64> {
        let mut parts = self.created.split_whitespace();
        let stamp = format!("{} {} {}", parts.next()?, parts.next()?, parts.next()?);
        chrono::DateTime::parse_from_str(&stamp, "%Y-%m-%d %H:%M:%S %z").ok().map(|d| d.timestamp_millis())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostStatus {
    pub host: String,
    pub ok: bool,
    pub error: Option<String>,
    pub containers: Vec<Container>,
}

const MARK: &str = "@@KGUI-LABELS@@";
const LAYERS_MARK: &str = "@@KGUI-LAYERS@@";

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct PsLine {
    #[serde(rename = "ID")]
    id: String,
    names: String,
    image: String,
    state: String,
    status: String,
    created_at: String,
    labels: String,
}

/// Hosts, users and jump hosts come from deploy configs in cloned repos, so
/// treat them as untrusted: a value like `-oProxyCommand=…` would otherwise be
/// read by ssh as an option and run a local command.
fn check_ssh_value(value: &str, what: &str, extra: &str) -> Result<()> {
    let ok = !value.is_empty()
        && !value.starts_with('-')
        && value.chars().all(|c| c.is_ascii_alphanumeric() || "._-:[]".contains(c) || extra.contains(c));
    anyhow::ensure!(ok, "refusing unsafe ssh {what} from deploy config: {value:?}");
    Ok(())
}

pub fn ssh_command(host: &str, ssh: &SshTarget) -> Result<std::process::Command> {
    ssh_command_with(host, ssh, std::iter::empty::<&str>())
}

/// [`ssh_command`] with extra ssh options (`-o …` pairs).
fn ssh_command_with(
    host: &str,
    ssh: &SshTarget,
    options: impl IntoIterator<Item = impl AsRef<std::ffi::OsStr>>,
) -> Result<std::process::Command> {
    check_ssh_value(host, "host", "")?;
    check_ssh_value(&ssh.user, "user", "")?;
    if let Some(proxy) = &ssh.proxy {
        // `-J user@jump1,user@jump2:port`
        check_ssh_value(proxy, "proxy", "@,")?;
    }
    let mut c = command("ssh");
    // Keepalives make a connection that drops mid-command fail within ~15 s.
    c.args([
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=5",
        "-o",
        "ServerAliveInterval=5",
        "-o",
        "ServerAliveCountMax=3",
    ])
    .arg("-p")
    .arg(ssh.port.to_string())
    .arg("-l")
    .arg(&ssh.user);
    if let Some(proxy) = &ssh.proxy {
        c.arg("-J").arg(proxy);
    }
    c.args(options);
    // `--` ends option parsing: the host can never be taken as a flag.
    c.arg("--").arg(host);
    Ok(c)
}

pub fn host_status(host: &str, ssh: &SshTarget, progress: &Progress) -> HostStatus {
    match containers(host, ssh, progress) {
        Ok(containers) => HostStatus { host: host.into(), ok: true, error: None, containers },
        Err(e) => HostStatus { host: host.into(), ok: false, error: Some(format!("{e:#}")), containers: vec![] },
    }
}

/// Status checks per server at the same time. With many projects on one
/// server, more would pile up unauthenticated connections (sshd drops them
/// past `MaxStartups`, 10 by default) and SSH agent signing requests.
const PER_SERVER: usize = 3;
static SERVER_LIMIT: crate::env::Limit = crate::env::Limit::new(PER_SERVER);

fn containers(host: &str, ssh: &SshTarget, progress: &Progress) -> Result<Vec<Container>> {
    crate::env::with_ssh(|| {
        SERVER_LIMIT.run(host, || {
            let step = progress.start(host);
            let result = containers_inner(host, ssh);
            progress.finish(step);
            result
        })
    })
}

/// The servers the units run on, accessories' included, each once, with how
/// to reach them.
pub fn servers_of<'a>(units: impl IntoIterator<Item = &'a crate::project::Unit>) -> Vec<(String, SshTarget)> {
    let mut hosts: Vec<(String, SshTarget)> = Vec::new();
    for c in units.into_iter().map(|u| &u.config) {
        let accessory_hosts = c.accessories.iter().flat_map(|a| &a.hosts);
        for h in c.servers.iter().map(|s| &s.host).chain(accessory_hosts) {
            if !hosts.iter().any(|(x, s)| x == h && *s == c.ssh) {
                hosts.push((h.clone(), c.ssh.clone()));
            }
        }
    }
    hosts
}

/// Log in to each server over a new connection, so the SSH agent signs for
/// it now. 1Password holds back its approval prompt while the requesting app
/// isn't the frontmost one, and an approval covers the app's child processes
/// (kamal) until 1Password locks. So asking at the start of a run, while the
/// user has just clicked, saves kamal from asking minutes later (after a
/// build), when the prompt would stay hidden and the agent gives up.
///
/// The first server goes alone, so there is one prompt; the rest follow in
/// parallel. Returns the servers that failed, with the reason.
pub fn authorize(hosts: &[(String, SshTarget)]) -> Vec<(String, String)> {
    let login = |(host, ssh): &(String, SshTarget)| -> Option<(String, String)> {
        // No shared connection (ours or one from ~/.ssh/config): it would
        // skip the signature this is for.
        let fresh = ["-o", "ControlMaster=no", "-o", "ControlPath=none"];
        let result = ssh_command_with(host, ssh, fresh).and_then(|mut c| {
            c.arg("true");
            match crate::env::run_within(&mut c, HOST_TIMEOUT) {
                Err(e) if e.is::<crate::env::TimedOut>() => {
                    anyhow::bail!("no answer within {} s", HOST_TIMEOUT.as_secs())
                }
                r => r.map(drop),
            }
        });
        result.err().map(|e| (host.clone(), format!("{e:#}")))
    };
    let Some((first, rest)) = hosts.split_first() else { return Vec::new() };
    let mut failed: Vec<_> = login(first).into_iter().collect();
    std::thread::scope(|s| {
        let handles: Vec<_> = rest.iter().map(|h| s.spawn(move || login(h))).collect();
        failed.extend(handles.into_iter().filter_map(|h| h.join().ok().flatten()));
    });
    failed
}

/// Where status checks keep a shared ssh connection per server: one login
/// (one SSH agent signature) serves the checks that follow for a minute.
/// Short and in /tmp, as socket paths are limited to ~104 bytes; private to
/// the user, and refused if anything about it looks off.
fn control_dir() -> Option<std::path::PathBuf> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    let uid = std::fs::metadata(dirs::home_dir()?).ok()?.uid();
    let dir = std::path::PathBuf::from(format!("/tmp/kgui-ssh-{uid}"));
    let _ = std::fs::DirBuilder::new().mode(0o700).create(&dir);
    let meta = std::fs::symlink_metadata(&dir).ok()?;
    (meta.is_dir() && meta.uid() == uid && meta.permissions().mode() & 0o077 == 0).then_some(dir)
}

/// The shell command run on a server for its status.
fn status_script() -> String {
    format!(
        "docker ps -a --no-trunc --filter label=service --format '{{{{json .}}}}'; echo {MARK}; \
             docker ps -aq --filter label=service | xargs -r docker inspect --format '{{{{json .Name}}}}{{{{\"\\t\"}}}}{{{{json .Config.Labels}}}}'; \
             echo {LAYERS_MARK}; \
             docker ps --filter label=service --format '{{{{.Image}}}}' | sort -u | while read -r i; do \
               printf '%s\\t%s\\n' \"$i\" \"$(docker image inspect --format '{{{{.Architecture}}}} {{{{join .RootFS.Layers \" \"}}}}' \"$i\" 2>/dev/null)\"; \
             done"
    )
}

fn containers_inner(host: &str, ssh: &SshTarget) -> Result<Vec<Container>> {
    // One round trip: the container list, then each container's full label map
    // (`docker ps` flattens labels into a comma-separated string, which breaks
    // on values that contain commas), then the layers of each running image.
    let control = control_dir().map(|dir| {
        [
            "-o".to_string(),
            "ControlMaster=auto".into(),
            "-o".into(),
            format!("ControlPath={}/%C", dir.display()),
            "-o".into(),
            "ControlPersist=60".into(),
        ]
    });
    let mut output = ssh_command_with(host, ssh, control.into_iter().flatten())?;
    output.arg(status_script());
    let output = match crate::env::output_within(&mut output, HOST_TIMEOUT) {
        Ok(o) => o,
        Err(e) if e.is::<crate::env::TimedOut>() => anyhow::bail!(
            "no answer within {} s (is the VPN on, or is 1Password waiting for you to approve the SSH key?)",
            HOST_TIMEOUT.as_secs()
        ),
        Err(e) => return Err(e),
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let msg = stderr.trim().trim_start_matches("ssh: ");
        anyhow::bail!("{}", if msg.is_empty() { output.status.to_string() } else { msg.to_string() });
    }
    let out = String::from_utf8_lossy(&output.stdout);
    let (ps, inspect) = out.split_once(MARK).unwrap_or((&out, ""));
    let (inspect, layers) = inspect.split_once(LAYERS_MARK).unwrap_or((inspect, ""));
    // Image → (architecture, layers).
    let layers: HashMap<&str, (&str, Vec<String>)> = layers
        .lines()
        .filter_map(|l| {
            let (image, rest) = l.split_once('\t')?;
            let mut words = rest.split_whitespace();
            let arch = words.next()?;
            Some((image, (arch, words.map(String::from).collect())))
        })
        .collect();
    let full_labels: HashMap<String, HashMap<String, String>> = inspect
        .lines()
        .filter_map(|l| {
            let (name, labels) = l.split_once('\t')?;
            let name: String = serde_json::from_str(name).ok()?;
            let labels: HashMap<String, String> = serde_json::from_str(labels).unwrap_or_default();
            Some((name.trim_start_matches('/').to_string(), labels))
        })
        .collect();
    Ok(ps
        .lines()
        .filter_map(|l| serde_json::from_str::<PsLine>(l).ok())
        .map(|p| {
            let labels: HashMap<&str, &str> = p.labels.split(',').filter_map(|kv| kv.split_once('=')).collect();
            let label = |k: &str| labels.get(k).filter(|v| !v.is_empty()).map(|v| v.to_string());
            let full = full_labels.get(&p.names);
            let full_label = |k: &str| full.and_then(|l| l.get(k)).filter(|v| !v.is_empty()).cloned();
            let (image_arch, image_layers) =
                layers.get(p.image.as_str()).map(|(a, l)| (a.to_string(), l.clone())).unwrap_or_default();
            Container {
                image_arch,
                image_layers,
                image_version: full_label(crate::images::LABEL_VERSION),
                image_source: full_label(crate::images::LABEL_SOURCE),
                version: p.image.rsplit_once(':').map(|(_, t)| t.to_string()).unwrap_or_default(),
                service: label("service").unwrap_or_default(),
                role: label("role"),
                destination: label("destination"),
                id: p.id.chars().take(12).collect(),
                name: p.names,
                image: p.image,
                state: p.state,
                status: p.status,
                created: p.created_at,
            }
        })
        .collect())
}

/// Status of several hosts in parallel.
pub fn hosts_status(hosts: &[(String, SshTarget)], progress: &Progress) -> Vec<HostStatus> {
    std::thread::scope(|s| {
        let handles: Vec<_> = hosts.iter().map(|(h, ssh)| s.spawn(move || host_status(h, ssh, progress))).collect();
        handles.into_iter().map(|h| h.join().expect("ssh thread panicked")).collect()
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionInfo {
    pub subject: Option<String>,
    /// Commits on the default branch that this version lacks.
    pub behind: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectStatus {
    pub hosts: Vec<HostStatus>,
    pub versions: HashMap<String, VersionInfo>,
    /// Newest upstream release per image source URL.
    pub releases: HashMap<String, crate::images::Release>,
    pub checked_at: i64,
    /// When the newest running app container was created: the last deploy,
    /// whether it ran from the app or from the kamal CLI.
    #[serde(default)]
    pub last_deploy: Option<i64>,
    /// For running images whose base image carries no version label: whether
    /// they still start with the layers their base tag points to now (false:
    /// the tag was republished since the deploy). Keyed by container image.
    #[serde(default)]
    pub base_current: HashMap<String, bool>,
}

/// Commit subject and how far behind the followed branch each running
/// version is, from the app's clone (no network).
fn versions_of(detail: &crate::project::ProjectDetail, hosts: &[HostStatus]) -> HashMap<String, VersionInfo> {
    let rev = &detail.branch;
    let mirror = crate::git::mirror_dir(&detail.project.name);
    let shas: std::collections::BTreeSet<&str> = hosts
        .iter()
        .flat_map(|h| &h.containers)
        .map(|c| c.version.as_str())
        .filter(|v| v.len() >= 7 && v.chars().take(7).all(|ch| ch.is_ascii_hexdigit()))
        .collect();
    shas.into_iter()
        .map(|v| {
            // The tag comes from the server: look it up as a commit id only.
            let id = crate::git::commit_id(&mirror, v.split('_').next().unwrap_or(v));
            let info = VersionInfo {
                subject: id.as_deref().and_then(|id| crate::git::resolve(&mirror, id).ok()).map(|c| c.subject),
                behind: id.as_deref().and_then(|id| crate::git::behind(&mirror, id, rev)),
            };
            (v.to_string(), info)
        })
        .collect()
}

impl ProjectStatus {
    /// Recompute "latest" / "N behind" against the clone as it is now, e.g.
    /// after a fetch, for a status saved earlier.
    pub fn update_versions(&mut self, detail: &crate::project::ProjectDetail) {
        self.versions = versions_of(detail, &self.hosts);
    }
}

fn saved_path(project: &str) -> Option<std::path::PathBuf> {
    // Project names are checked when added; refuse anything path-like anyway.
    let ok = !project.is_empty() && project.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c));
    (ok && !project.starts_with('.'))
        .then(|| crate::settings::data_dir().join("status").join(format!("{project}.json")))
}

/// The last complete status check of a project, as saved by
/// [`project_status`]: shown at startup without asking the servers.
pub fn load_saved(project: &str) -> Option<ProjectStatus> {
    serde_json::from_slice(&std::fs::read(saved_path(project)?).ok()?).ok()
}

fn save(project: &str, status: &ProjectStatus) -> Result<()> {
    let path = saved_path(project).ok_or_else(|| anyhow::anyhow!("bad project name {project:?}"))?;
    crate::settings::write_private(&path, &serde_json::to_vec(status)?)?;
    Ok(())
}

/// Containers of this project's services (apps and accessories) on every
/// host the project uses, plus commit info for each running version.
pub fn project_status(detail: &crate::project::ProjectDetail, progress: &Progress) -> ProjectStatus {
    let mut services = std::collections::HashSet::new();
    let mut apps = std::collections::HashSet::new();
    let units: Vec<&crate::project::Unit> = detail.targets.iter().flat_map(|t| &t.units).collect();
    for c in units.iter().map(|u| &u.config) {
        services.extend(c.service.clone());
        apps.extend(c.service.clone());
        services.extend(c.accessories.iter().map(|a| a.service.clone()));
    }
    let hosts = servers_of(units);

    let mut result = hosts_status(&hosts, progress);
    for h in &mut result {
        h.containers.retain(|c| services.contains(&c.service));
    }

    let versions = versions_of(detail, &result);
    let sources: std::collections::BTreeSet<&str> =
        result.iter().flat_map(|h| &h.containers).filter_map(|c| c.image_source.as_deref()).collect();
    // Base images without a version label: compare layers with the registry.
    let mut unlabeled: Vec<(&str, &str, &str, &[String])> = Vec::new(); // (container image, base, arch, layers)
    for unit in detail.targets.iter().flat_map(|t| &t.units) {
        let Some(base) = unit.base_image.as_deref().filter(|b| !crate::images::is_pinned(b)) else { continue };
        for c in result.iter().flat_map(|h| &h.containers) {
            let own = unit.config.service.as_deref() == Some(c.service.as_str()) && c.destination == unit.destination;
            if own
                && c.state == "running"
                && c.image_version.is_none()
                && !c.image_layers.is_empty()
                && !unlabeled.iter().any(|(i, ..)| *i == c.image)
            {
                unlabeled.push((&c.image, base, &c.image_arch, &c.image_layers));
            }
        }
    }
    let step = (!sources.is_empty()).then(|| progress.start("GitHub releases"));
    let registry_step = (!unlabeled.is_empty()).then(|| progress.start("Image registries"));
    let (releases, base_current) = std::thread::scope(|s| {
        let handles: Vec<_> = sources
            .into_iter()
            .map(|src| s.spawn(move || (src.to_string(), crate::images::latest_release(src))))
            .collect();
        let bases: Vec<_> = unlabeled
            .iter()
            .map(|&(image, base, arch, layers)| {
                s.spawn(move || {
                    let now = crate::images::tag_layers(base, arch)?;
                    Some((image.to_string(), layers.starts_with(&now)))
                })
            })
            .collect();
        let releases: HashMap<String, crate::images::Release> =
            handles.into_iter().filter_map(|h| h.join().ok()).filter_map(|(s, r)| Some((s, r?))).collect();
        let current: HashMap<String, bool> = bases.into_iter().filter_map(|h| h.join().ok().flatten()).collect();
        (releases, current)
    });
    for step in [step, registry_step].into_iter().flatten() {
        progress.finish(step);
    }
    let last_deploy = result
        .iter()
        .flat_map(|h| &h.containers)
        .filter(|c| apps.contains(&c.service) && c.state == "running")
        .filter_map(Container::created_ms)
        .max();
    if let Some(at) = last_deploy {
        let _ = crate::history::History::open().and_then(|h| h.record_deploy(&detail.project.name, at));
    }
    let status = ProjectStatus {
        hosts: result,
        versions,
        releases,
        checked_at: crate::history::now_ms(),
        last_deploy,
        base_current,
    };
    // Keep it for the next start, unless a server didn't answer: then the
    // last complete picture is worth more.
    if status.hosts.iter().all(|h| h.ok) {
        let _ = save(&detail.project.name, &status);
    }
    status
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(user: &str, proxy: Option<&str>) -> SshTarget {
        SshTarget { user: user.into(), port: 22, proxy: proxy.map(String::from) }
    }

    #[test]
    fn reads_docker_created_at() {
        let at = |created: &str| {
            Container {
                id: String::new(),
                name: String::new(),
                image: String::new(),
                version: String::new(),
                service: String::new(),
                role: None,
                destination: None,
                state: String::new(),
                status: String::new(),
                created: created.into(),
                image_version: None,
                image_source: None,
                image_arch: String::new(),
                image_layers: Vec::new(),
            }
            .created_ms()
        };
        assert_eq!(at("2026-11-18 20:20:13 +0100 CET"), Some(1_795_029_613_000));
        assert_eq!(at("2026-11-18 19:20:13 +0000 UTC"), Some(1_795_029_613_000));
        assert_eq!(at("About an hour ago"), None);
        assert_eq!(at(""), None);
    }

    /// The status script's quoting, run against a stand-in `docker`.
    #[test]
    fn status_script_reads_layers() {
        let dir = std::env::temp_dir().join(format!("kgui-fake-docker-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let docker = dir.join("docker");
        std::fs::write(
            &docker,
            r#"#!/bin/sh
case "$1 $2" in
  "ps -a") echo '{"ID":"abc","Names":"app-web-1","Image":"app:1","State":"running","Status":"Up","CreatedAt":"2026-11-18 20:20:13 +0100 CET","Labels":"service=app"}' ;;
  "ps -aq") echo abc ;;
  "ps --filter") echo app:1; echo app:1 ;;
  "inspect --format") echo '"/app-web-1"	{"service":"app"}' ;;
  "image inspect") [ "$3" = "--format" ] && [ "$4" = '{{.Architecture}} {{join .RootFS.Layers " "}}' ] && echo "amd64 sha256:a sha256:b" ;;
esac
"#,
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&docker, std::fs::Permissions::from_mode(0o755)).unwrap();
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(status_script())
            .env("PATH", format!("{}:/usr/bin:/bin", dir.display()))
            .output()
            .unwrap();
        let out = String::from_utf8_lossy(&out.stdout);
        let layers = out.split_once(LAYERS_MARK).unwrap().1;
        assert_eq!(layers.trim(), "app:1\tamd64 sha256:a sha256:b");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A saved status reads back the same, and older files without the
    /// newer fields still load.
    #[test]
    fn saved_status_round_trips() {
        let container: Container = serde_json::from_value(serde_json::json!({
            "id": "abc", "name": "app-web-1", "image": "app:1", "version": "1fb4993", "service": "app",
            "role": "web", "destination": null, "state": "running", "status": "Up 4 seconds",
            "created": "2026-11-18 20:20:13 +0100 CET", "image_version": null, "image_source": null
        }))
        .unwrap();
        assert!(container.image_layers.is_empty());
        let status = ProjectStatus {
            hosts: vec![HostStatus { host: "10.0.0.25".into(), ok: true, error: None, containers: vec![container] }],
            versions: HashMap::from([("1fb4993".into(), VersionInfo { subject: Some("x".into()), behind: Some(0) })]),
            releases: HashMap::new(),
            checked_at: 1,
            last_deploy: Some(2),
            base_current: HashMap::from([("app:1".into(), true)]),
        };
        let back: ProjectStatus = serde_json::from_str(&serde_json::to_string(&status).unwrap()).unwrap();
        assert_eq!(back.hosts[0].containers[0].status, "Up 4 seconds");
        assert_eq!(back.base_current.get("app:1"), Some(&true));
        assert_eq!(back.last_deploy, Some(2));
        assert!(saved_path("../etc").is_none() && saved_path(".hidden").is_none());
    }

    #[test]
    fn rejects_option_injection_from_configs() {
        assert!(ssh_command("10.0.0.1", &target("root", None)).is_ok());
        assert!(ssh_command("app-1.example.com", &target("deploy", Some("ops@bastion:2222,jump2"))).is_ok());
        assert!(ssh_command("[fd00::1]", &target("root", None)).is_ok());
        assert!(ssh_command("-oProxyCommand=touch /tmp/x", &target("root", None)).is_err());
        assert!(ssh_command("host", &target("-oProxyCommand=x", None)).is_err());
        assert!(ssh_command("host", &target("root", Some("-oProxyCommand=x"))).is_err());
        assert!(ssh_command("host name", &target("root", None)).is_err());
        assert!(ssh_command("host;rm", &target("root", None)).is_err());
        let args: Vec<String> = ssh_command("10.0.0.1", &target("root", None))
            .unwrap()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(&args[args.len() - 2..], ["--", "10.0.0.1"]);
    }
}
