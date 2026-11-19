//! Minimal reading of Kamal deploy configs: enough to know services, hosts,
//! roles, ssh settings, aliases and accessories without running Ruby.
//! Kamal itself remains the authority for everything it executes.

use anyhow::{Context, Result};
use serde::Serialize;
use serde_yaml::Value;

pub fn parse(text: &str) -> Result<Value> {
    // Kamal configs may contain ERB. Strip tags so the YAML still parses.
    static ERB: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(r"<%.*?%>").unwrap());
    let cleaned = ERB.replace_all(text, "");
    serde_yaml::from_str(&cleaned).context("invalid YAML")
}

/// Kamal merges a destination file (`deploy.<dest>.yml`) deeply into its base.
pub fn deep_merge(base: &mut Value, overlay: Value) {
    match (base, overlay) {
        (Value::Mapping(b), Value::Mapping(o)) => {
            for (k, v) in o {
                match b.get_mut(&k) {
                    Some(bv) => deep_merge(bv, v),
                    None => {
                        b.insert(k, v);
                    }
                }
            }
        }
        (b, o) => *b = o,
    }
}

pub fn has_key(v: &Value, key: &str) -> bool {
    v.get(key).is_some()
}

/// Whether a name read from a deploy config (an alias, an accessory, a
/// destination) or from a server (an image tag) can be handed to kamal as an
/// argument: it must not look like an option, as `--hosts=…` or `-y` would.
pub fn is_name(s: &str) -> bool {
    s.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_')
        && s.chars().all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c))
}

#[derive(Debug, Clone, Serialize)]
pub struct Server {
    pub role: String,
    pub host: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Alias {
    pub name: String,
    pub command: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Accessory {
    pub name: String,
    pub service: String,
    pub hosts: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UnitConfig {
    pub service: Option<String>,
    pub image: Option<String>,
    pub primary_role: Option<String>,
    pub servers: Vec<Server>,
    pub ssh: SshTarget,
    pub proxy_hosts: Vec<String>,
    pub aliases: Vec<Alias>,
    pub accessories: Vec<Accessory>,
    /// `secrets_path` override (Kamal's default is `.kamal/secrets`).
    pub secrets_path: Option<String>,
    /// `builder.dockerfile` (Kamal's default is `Dockerfile`).
    pub dockerfile: Option<String>,
    /// `builder.target`: the Dockerfile stage that is built.
    pub build_target: Option<String>,
}

/// How to reach a host the way Kamal does.
#[derive(Debug, Clone, Serialize, serde::Deserialize, PartialEq, Eq, Hash)]
pub struct SshTarget {
    pub user: String,
    pub port: u16,
    pub proxy: Option<String>,
}

fn s(v: &Value) -> Option<String> {
    match v {
        Value::String(x) => Some(x.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// A host entry is either `"1.2.3.4"` or `{"1.2.3.4": tag}`.
fn host_of(v: &Value) -> Option<String> {
    match v {
        Value::Mapping(m) => m.keys().next().and_then(s),
        other => s(other),
    }
}

fn hosts_of(v: &Value) -> Vec<String> {
    match v {
        Value::Sequence(seq) => seq.iter().filter_map(host_of).collect(),
        Value::Mapping(_) => v.get("hosts").map(hosts_of).unwrap_or_default(),
        other => host_of(other).into_iter().collect(),
    }
}

pub fn extract(v: &Value) -> UnitConfig {
    let mut servers = Vec::new();
    match v.get("servers") {
        Some(Value::Sequence(seq)) => {
            for h in seq.iter().filter_map(host_of) {
                servers.push(Server { role: "web".into(), host: h });
            }
        }
        Some(Value::Mapping(roles)) => {
            for (role, rv) in roles {
                let role = s(role).unwrap_or_default();
                for h in hosts_of(rv) {
                    servers.push(Server { role: role.clone(), host: h });
                }
            }
        }
        _ => {}
    }

    let ssh = v.get("ssh");
    let ssh = SshTarget {
        user: ssh.and_then(|x| x.get("user")).and_then(s).unwrap_or_else(|| "root".into()),
        port: ssh.and_then(|x| x.get("port")).and_then(s).and_then(|p| p.parse().ok()).unwrap_or(22),
        proxy: ssh.and_then(|x| x.get("proxy")).and_then(s),
    };

    let mut proxy_hosts = Vec::new();
    if let Some(p) = v.get("proxy") {
        if let Some(h) = p.get("host").and_then(s) {
            proxy_hosts.extend(h.split(',').map(|x| x.trim().to_string()));
        }
        if let Some(Value::Sequence(hs)) = p.get("hosts") {
            proxy_hosts.extend(hs.iter().filter_map(s));
        }
    }

    // Alias and accessory names are run as `kamal <alias>` and
    // `kamal accessory <verb> <name>`. Ones that aren't plain names are left
    // out (Kamal itself only accepts `[a-z0-9_-]` for aliases).
    let aliases = match v.get("aliases") {
        Some(Value::Mapping(m)) => {
            m.iter().filter_map(|(k, v)| Some(Alias { name: s(k).filter(|n| is_name(n))?, command: s(v)? })).collect()
        }
        _ => vec![],
    };

    let service = v.get("service").and_then(s);
    let accessories = match v.get("accessories") {
        Some(Value::Mapping(m)) => m
            .iter()
            .filter_map(|(k, av)| {
                let name = s(k).filter(|n| is_name(n))?;
                let svc = av
                    .get("service")
                    .and_then(s)
                    .unwrap_or_else(|| format!("{}-{}", service.clone().unwrap_or_default(), name));
                let mut hosts = Vec::new();
                if let Some(h) = av.get("host").and_then(s) {
                    hosts.push(h);
                }
                if let Some(hs) = av.get("hosts") {
                    hosts.extend(hosts_of(hs));
                }
                Some(Accessory { name, service: svc, hosts })
            })
            .collect(),
        _ => vec![],
    };

    UnitConfig {
        service,
        image: v.get("image").and_then(s),
        primary_role: v.get("primary_role").and_then(s),
        servers,
        ssh,
        proxy_hosts,
        aliases,
        accessories,
        secrets_path: v.get("secrets_path").and_then(s),
        dockerfile: v.get("builder").and_then(|b| b.get("dockerfile")).and_then(s),
        build_target: v.get("builder").and_then(|b| b.get("target")).and_then(s),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_and_hosts() {
        let v = parse("service: a\nservers:\n  web:\n    - 10.0.0.1\n  horizon:\n    hosts:\n      - 10.0.0.2: tag\n")
            .unwrap();
        let c = extract(&v);
        assert_eq!(c.service.as_deref(), Some("a"));
        assert_eq!(c.servers.len(), 2);
        assert_eq!(c.servers[1].role, "horizon");
        assert_eq!(c.servers[1].host, "10.0.0.2");
        assert_eq!(c.ssh.user, "root");
    }

    #[test]
    fn overlay_merges() {
        let mut base = parse("service: a\nenv:\n  clear:\n    A: 1\n    B: 2\n").unwrap();
        deep_merge(&mut base, parse("service: b\nenv:\n  clear:\n    B: 3\n").unwrap());
        let c = extract(&base);
        assert_eq!(c.service.as_deref(), Some("b"));
        assert_eq!(base["env"]["clear"]["A"], Value::from(1));
        assert_eq!(base["env"]["clear"]["B"], Value::from(3));
    }
}
