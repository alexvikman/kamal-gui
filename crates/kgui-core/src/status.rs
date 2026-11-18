//! Live container status straight from Docker on each host, over the
//! system `ssh` (so `~/.ssh/config`, agents and jump hosts just work).
//! Kamal labels its containers with `service`, `role` and `destination`.

use crate::config::SshTarget;
use crate::env::command;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize)]
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
}

#[derive(Debug, Clone, Serialize)]
pub struct HostStatus {
    pub host: String,
    pub ok: bool,
    pub error: Option<String>,
    pub containers: Vec<Container>,
}

const MARK: &str = "@@KGUI-LABELS@@";

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

pub fn ssh_command(host: &str, ssh: &SshTarget) -> std::process::Command {
    let mut c = command("ssh");
    c.args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=5", "-p"])
        .arg(ssh.port.to_string())
        .arg("-l")
        .arg(&ssh.user);
    if let Some(proxy) = &ssh.proxy {
        c.arg("-J").arg(proxy);
    }
    c.arg(host);
    c
}

pub fn host_status(host: &str, ssh: &SshTarget) -> HostStatus {
    match containers(host, ssh) {
        Ok(containers) => HostStatus { host: host.into(), ok: true, error: None, containers },
        Err(e) => HostStatus {
            host: host.into(),
            ok: false,
            error: Some(format!("{e:#}")),
            containers: vec![],
        },
    }
}

fn containers(host: &str, ssh: &SshTarget) -> Result<Vec<Container>> {
    // One round trip: the container list, then each container's full label map
    // (`docker ps` flattens labels into a comma-separated string, which breaks
    // on values that contain commas).
    let output = ssh_command(host, ssh)
        .arg(format!(
            "docker ps -a --no-trunc --filter label=service --format '{{{{json .}}}}'; echo {MARK}; \
             docker ps -aq --filter label=service | xargs -r docker inspect --format '{{{{json .Name}}}}{{{{\"\\t\"}}}}{{{{json .Config.Labels}}}}'"
        ))
        .stdin(std::process::Stdio::null())
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let msg = stderr.trim().trim_start_matches("ssh: ");
        anyhow::bail!("{}", if msg.is_empty() { output.status.to_string() } else { msg.to_string() });
    }
    let out = String::from_utf8_lossy(&output.stdout);
    let (ps, inspect) = out.split_once(MARK).unwrap_or((&out, ""));
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
            let labels: HashMap<&str, &str> =
                p.labels.split(',').filter_map(|kv| kv.split_once('=')).collect();
            let label = |k: &str| labels.get(k).filter(|v| !v.is_empty()).map(|v| v.to_string());
            let full = full_labels.get(&p.names);
            let full_label = |k: &str| full.and_then(|l| l.get(k)).filter(|v| !v.is_empty()).cloned();
            Container {
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
pub fn hosts_status(hosts: &[(String, SshTarget)]) -> Vec<HostStatus> {
    std::thread::scope(|s| {
        let handles: Vec<_> = hosts.iter().map(|(h, ssh)| s.spawn(move || host_status(h, ssh))).collect();
        handles.into_iter().map(|h| h.join().expect("ssh thread panicked")).collect()
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct VersionInfo {
    pub subject: Option<String>,
    /// Commits on the default branch that this version lacks.
    pub behind: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectStatus {
    pub hosts: Vec<HostStatus>,
    pub versions: HashMap<String, VersionInfo>,
    /// Newest upstream release per image source URL.
    pub releases: HashMap<String, crate::images::Release>,
    pub checked_at: i64,
}

/// Containers of this project's services (apps and accessories) on every
/// host the project uses, plus commit info for each running version.
pub fn project_status(detail: &crate::project::ProjectDetail) -> ProjectStatus {
    let mut services = std::collections::HashSet::new();
    let mut hosts: Vec<(String, SshTarget)> = Vec::new();
    for unit in detail.targets.iter().flat_map(|t| &t.units) {
        let c = &unit.config;
        services.extend(c.service.clone());
        let mut add = |h: &String| {
            if !hosts.iter().any(|(x, s)| x == h && *s == c.ssh) {
                hosts.push((h.clone(), c.ssh.clone()));
            }
        };
        c.servers.iter().for_each(|s| add(&s.host));
        for a in &c.accessories {
            services.insert(a.service.clone());
            a.hosts.iter().for_each(&mut add);
        }
    }

    let mut result = hosts_status(&hosts);
    for h in &mut result {
        h.containers.retain(|c| services.contains(&c.service));
    }

    let mut versions = HashMap::new();
    {
        let rev = &detail.default_branch;
        let mirror = crate::git::mirror_dir(&detail.project.name);
        let shas: std::collections::BTreeSet<&str> = result
            .iter()
            .flat_map(|h| &h.containers)
            .map(|c| c.version.as_str())
            .filter(|v| v.len() >= 7 && v.chars().take(7).all(|ch| ch.is_ascii_hexdigit()))
            .collect();
        for v in shas {
            let sha = v.split('_').next().unwrap_or(v);
            versions.insert(
                v.to_string(),
                VersionInfo {
                    subject: crate::git::resolve(&mirror, sha).ok().map(|c| c.subject),
                    behind: crate::git::behind(&mirror, sha, rev),
                },
            );
        }
    }
    let sources: std::collections::BTreeSet<&str> =
        result.iter().flat_map(|h| &h.containers).filter_map(|c| c.image_source.as_deref()).collect();
    let releases = std::thread::scope(|s| {
        let handles: Vec<_> = sources
            .into_iter()
            .map(|src| s.spawn(move || (src.to_string(), crate::images::latest_release(src))))
            .collect();
        handles.into_iter().filter_map(|h| h.join().ok()).filter_map(|(s, r)| Some((s, r?))).collect()
    });
    ProjectStatus { hosts: result, versions, releases, checked_at: crate::history::now_ms() }
}
