//! Base images: which external image a unit's Dockerfile builds on, which
//! version of it is running, and the newest release upstream.
//!
//! Version info comes from OCI labels (`org.opencontainers.image.version`,
//! `org.opencontainers.image.source`) that images like serversideup/php set
//! and that the app image inherits.

use crate::env::{command, run_within};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, OnceLock};
use std::time::{Duration, Instant};

pub const LABEL_VERSION: &str = "org.opencontainers.image.version";
pub const LABEL_SOURCE: &str = "org.opencontainers.image.source";

/// The external image the given build stage ultimately starts from, following
/// `FROM <earlier-stage>` chains and expanding `ARG` defaults. `target` is
/// Kamal's `builder.target`; without it the last stage is used.
pub fn base_image(dockerfile: &str, target: Option<&str>) -> Option<String> {
    let mut args: HashMap<String, String> = HashMap::new();
    let mut stages: Vec<(Option<String>, String)> = Vec::new();
    for raw in dockerfile.lines() {
        let line = raw.trim();
        let mut words = line.split_whitespace();
        let Some(instr) = words.next() else { continue };
        match instr.to_ascii_uppercase().as_str() {
            "ARG" => {
                if let Some((k, v)) = words.next().and_then(|a| a.split_once('=')) {
                    args.entry(k.to_string()).or_insert_with(|| v.trim_matches('"').to_string());
                }
            }
            "FROM" => {
                let rest: Vec<&str> = words.filter(|w| !w.starts_with("--")).collect();
                let Some(image) = rest.first() else { continue };
                let name = match rest.as_slice() {
                    [_, as_kw, name, ..] if as_kw.eq_ignore_ascii_case("as") => Some(name.to_string()),
                    _ => None,
                };
                stages.push((name, expand(image, &args)));
            }
            _ => {}
        }
    }
    let mut idx = match target {
        Some(t) => stages.iter().position(|(n, _)| n.as_deref().is_some_and(|n| n.eq_ignore_ascii_case(t)))?,
        None => stages.len().checked_sub(1)?,
    };
    // Follow references to earlier stages.
    for _ in 0..stages.len() {
        let from = &stages[idx].1;
        match stages[..idx].iter().rposition(|(n, _)| n.as_deref().is_some_and(|n| n.eq_ignore_ascii_case(from))) {
            Some(prev) => idx = prev,
            None => return Some(from.clone()),
        }
    }
    None
}

/// `${NAME}`, `${NAME:-default}` and `$NAME` from ARG defaults.
fn expand(s: &str, args: &HashMap<String, String>) -> String {
    static RE: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"\$\{([A-Za-z_][A-Za-z0-9_]*)(?::-([^}]*))?\}|\$([A-Za-z_][A-Za-z0-9_]*)").unwrap()
    });
    RE.replace_all(s, |c: &regex::Captures| {
        let name = c.get(1).or_else(|| c.get(3)).unwrap().as_str();
        args.get(name).cloned().or_else(|| c.get(2).map(|d| d.as_str().to_string())).unwrap_or_default()
    })
    .into_owned()
}

/// A tag like `8.4-fpm-nginx-v5.1.4` or a digest pins the version; a tag like
/// `8.4-fpm-nginx` floats to whatever was published last.
pub fn is_pinned(image: &str) -> bool {
    if image.contains("@sha256:") {
        return true;
    }
    let tag = image.rsplit_once(':').map(|(_, t)| t).unwrap_or("");
    static RE: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"(^|-)v?\d+\.\d+\.\d+$").unwrap());
    RE.is_match(tag)
}

/// `v5.1.4-44343912330` (version plus CI build id) → `v5.1.4`.
pub fn release_of(version: &str) -> &str {
    match version.rsplit_once('-') {
        Some((head, build)) if build.len() >= 6 && build.chars().all(|c| c.is_ascii_digit()) => head,
        _ => version,
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Release {
    pub tag: String,
    pub published_at: String,
    pub url: String,
}

type ReleaseCache = Mutex<HashMap<String, (Instant, Option<Release>)>>;

/// Newest GitHub release for an `org.opencontainers.image.source` URL, via
/// `gh` (cached for 15 minutes per process). None for non-GitHub sources.
pub fn latest_release(source: &str) -> Option<Release> {
    static CACHE: OnceLock<ReleaseCache> = OnceLock::new();
    let cache = CACHE.get_or_init(Mutex::default);
    if let Some((at, r)) = cache.lock().unwrap().get(source)
        && at.elapsed() < Duration::from_secs(15 * 60)
    {
        return r.clone();
    }
    let repo = github_repo(source)?;
    let result = run_within(
        command("gh").args([
            "api",
            &format!("repos/{repo}/releases/latest"),
            "--jq",
            "[.tag_name, .published_at, .html_url] | @tsv",
        ]),
        Duration::from_secs(20),
    )
    .ok()
    .and_then(|out| {
        let mut p = out.split('\t');
        Some(Release { tag: p.next()?.to_string(), published_at: p.next()?.to_string(), url: p.next()?.to_string() })
    })
    // The URL is opened in the browser.
    .filter(|r| r.url.starts_with("https://github.com/"));
    cache.lock().unwrap().insert(source.to_string(), (Instant::now(), result.clone()));
    result
}

/// `owner/repo` from a GitHub URL. The source is an image label, written by
/// whoever built the image, and becomes part of an authenticated API path:
/// only a plain `owner/repo` gets through (no `..`, `?`, `{…}` or extra
/// segments that would point the request somewhere else).
fn github_repo(source: &str) -> Option<String> {
    let rest = source.trim_end_matches('/').trim_end_matches(".git").strip_prefix("https://github.com/")?;
    let (owner, repo) = rest.split_once('/')?;
    let ok = |s: &str| {
        !s.is_empty()
            && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
            && s.chars().any(|c| c != '.')
            && !s.starts_with('-')
    };
    (ok(owner) && ok(repo)).then(|| format!("{owner}/{repo}"))
}

/// A `host/repo:tag` image reference, checked so it can go into a registry URL.
#[derive(Debug, PartialEq, Eq)]
pub struct ImageRef {
    /// Registry host (`registry-1.docker.io` for Docker Hub).
    pub host: String,
    pub repo: String,
    pub tag: String,
}

/// Parse a `FROM` image. Digests are pinned (nothing to compare), and
/// anything unusual is refused: the reference comes from a cloned repo's
/// Dockerfile and ends up in an HTTPS request.
pub fn parse_ref(image: &str) -> Option<ImageRef> {
    if image.contains('@') {
        return None;
    }
    let (first, rest) = image.split_once('/').unwrap_or(("", image));
    let explicit_host = first.contains('.') || first.contains(':') || first == "localhost";
    let (host, path) = if explicit_host { (first, rest) } else { ("docker.io", image) };
    let (repo, tag) = match path.rsplit_once(':') {
        Some((r, t)) => (r, t),
        None => (path, "latest"),
    };
    let host = if host == "docker.io" { "registry-1.docker.io" } else { host };
    let repo = if host == "registry-1.docker.io" && !repo.contains('/') {
        format!("library/{repo}")
    } else {
        repo.to_string()
    };
    let host_ok =
        !host.is_empty() && host.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || ".-:".contains(c));
    let repo_ok = !repo.is_empty()
        && !repo.contains("..")
        && !repo.starts_with('/')
        && !repo.ends_with('/')
        && repo.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "._/-".contains(c));
    let tag_ok =
        !tag.is_empty() && tag.len() <= 128 && tag.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c));
    (host_ok && repo_ok && tag_ok).then(|| ImageRef { host: host.to_string(), repo, tag: tag.to_string() })
}

const MANIFEST_TYPES: &str = "application/vnd.oci.image.index.v1+json, \
    application/vnd.docker.distribution.manifest.list.v2+json, \
    application/vnd.oci.image.manifest.v1+json, \
    application/vnd.docker.distribution.manifest.v2+json";

/// Whether an address is on the public internet (not loopback, private,
/// link-local, CGNAT, multicast, documentation or unspecified).
fn is_public(ip: std::net::IpAddr) -> bool {
    use std::net::IpAddr;
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            !(v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_multicast()
                || v4.is_documentation()
                || o[0] == 0
                || (o[0] == 100 && (64..128).contains(&o[1]))
                || o[0] >= 240)
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public(IpAddr::V4(v4));
            }
            let first = v6.segments()[0];
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (first & 0xfe00) == 0xfc00
                || (first & 0xffc0) == 0xfe80)
        }
    }
}

/// `host[:port]` of an HTTPS URL, resolved to a public address. Registry and
/// token hosts come from Dockerfiles and registry responses, so a request
/// must never reach the local machine or the internal network: names that
/// are IP literals, local-only, or resolve to a non-public address are
/// refused. Returns curl's `--resolve` pin for the checked address.
fn public_pin(url: &str) -> Option<String> {
    use std::net::ToSocketAddrs;
    let authority = url.strip_prefix("https://")?.split(['/', '?', '#']).next()?;
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().ok()?),
        None => (authority, 443),
    };
    let host_ok = host.contains('.')
        && host.parse::<std::net::IpAddr>().is_err()
        && host.chars().all(|c| c.is_ascii_alphanumeric() || ".-".contains(c))
        && ![".local", ".localhost", ".internal", ".lan", ".home.arpa"].iter().any(|s| host.ends_with(s));
    if !host_ok {
        return None;
    }
    let addrs: Vec<_> = (host, port).to_socket_addrs().ok()?.collect();
    if addrs.is_empty() || !addrs.iter().all(|a| is_public(a.ip())) {
        return None;
    }
    let ip = match addrs[0].ip() {
        std::net::IpAddr::V6(v6) => format!("[{v6}]"),
        v4 => v4.to_string(),
    };
    Some(format!("{host}:{port}:{ip}"))
}

/// One HTTPS GET with curl: status, response headers, body.
fn http_get(url: &str, accept: &str, token: Option<&str>) -> Option<(u16, String, String)> {
    let pin = public_pin(url)?;
    let mut c = command("curl");
    // -q: ignore ~/.curlrc. -g: no URL globbing. HTTPS only, also across
    // redirects (blobs redirect to a CDN; curl drops the Authorization header
    // there). The host is pinned to the address checked above.
    c.args(["-q", "-g", "-s", "-L", "--max-redirs", "3", "--proto", "=https", "--proto-redir", "=https"])
        .args(["--max-time", "20", "--max-filesize", "4000000", "--resolve", &pin])
        .args(["-D", "/dev/stderr", "-w", "\n%{http_code}", "-H"])
        .arg(format!("Accept: {accept}"));
    if let Some(t) = token {
        c.arg("-H").arg(format!("Authorization: Bearer {t}"));
    }
    c.arg(url);
    let out = crate::env::output_within(&mut c, Duration::from_secs(25)).ok()?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let (body, code) = stdout.rsplit_once('\n')?;
    Some((code.trim().parse().ok()?, String::from_utf8_lossy(&out.stderr).into_owned(), body.to_string()))
}

/// An anonymous pull token, from the `WWW-Authenticate: Bearer realm=…`
/// challenge the registry answered with.
fn pull_token(headers: &str) -> Option<String> {
    let line = headers.lines().rev().find(|l| l.to_ascii_lowercase().starts_with("www-authenticate: bearer "))?;
    let params = &line["www-authenticate: bearer ".len()..];
    let get = |key: &str| {
        let start = params.find(&format!("{key}=\""))? + key.len() + 2;
        let end = start + params[start..].find('"')?;
        Some(params[start..end].to_string())
    };
    let realm = get("realm")
        .filter(|r| r.starts_with("https://") && r.chars().all(|c| c.is_ascii_alphanumeric() || ".-_/:".contains(c)))?;
    let mut url = realm;
    let mut sep = '?';
    for key in ["service", "scope"] {
        if let Some(v) = get(key) {
            let safe = v.chars().all(|c| c.is_ascii_alphanumeric() || "._-:/".contains(c));
            if !safe {
                return None;
            }
            url.push_str(&format!("{sep}{key}={}", v.replace(':', "%3A").replace('/', "%2F")));
            sep = '&';
        }
    }
    let (code, _, body) = http_get(&url, "application/json", None)?;
    if code != 200 {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(&body).ok()?;
    v.get("token").or_else(|| v.get("access_token"))?.as_str().map(String::from)
}

/// GET from the registry's v2 API, fetching a pull token when asked for one.
fn registry_get(r: &ImageRef, path: &str, accept: &str, token: &mut Option<String>) -> Option<String> {
    let url = format!("https://{}/v2/{}/{path}", r.host, r.repo);
    let (code, headers, body) = http_get(&url, accept, token.as_deref())?;
    match code {
        200 => Some(body),
        401 if token.is_none() => {
            *token = Some(pull_token(&headers)?);
            let (code, _, body) = http_get(&url, accept, token.as_deref())?;
            (code == 200).then_some(body)
        }
        _ => None,
    }
}

type LayerCache = Mutex<HashMap<String, (Instant, Option<Vec<String>>)>>;

/// The layers (`rootfs.diff_ids`) the tag points to right now, for the given
/// CPU architecture. An image built `FROM` it starts with exactly these while
/// the tag hasn't moved. Public images only; cached for 15 minutes.
pub fn tag_layers(image: &str, arch: &str) -> Option<Vec<String>> {
    static CACHE: OnceLock<LayerCache> = OnceLock::new();
    let cache = CACHE.get_or_init(Mutex::default);
    let key = format!("{image} {arch}");
    if let Some((at, r)) = cache.lock().unwrap().get(&key)
        && at.elapsed() < Duration::from_secs(15 * 60)
    {
        return r.clone();
    }
    let result = fetch_tag_layers(image, arch);
    cache.lock().unwrap().insert(key, (Instant::now(), result.clone()));
    result
}

fn fetch_tag_layers(image: &str, arch: &str) -> Option<Vec<String>> {
    let r = parse_ref(image)?;
    let digest_ok =
        |d: &str| d.starts_with("sha256:") && d.len() == 71 && d[7..].chars().all(|c| c.is_ascii_hexdigit());
    let mut token = None;
    let json = |s: String| serde_json::from_str::<serde_json::Value>(&s).ok();
    let mut manifest = json(registry_get(&r, &format!("manifests/{}", r.tag), MANIFEST_TYPES, &mut token)?)?;
    if let Some(list) = manifest.get("manifests").and_then(|m| m.as_array()) {
        let digest = list.iter().find_map(|m| {
            let p = m.get("platform")?;
            (p.get("os")?.as_str()? == "linux" && p.get("architecture")?.as_str()? == arch)
                .then(|| m.get("digest")?.as_str().map(String::from))?
        })?;
        if !digest_ok(&digest) {
            return None;
        }
        manifest = json(registry_get(&r, &format!("manifests/{digest}"), MANIFEST_TYPES, &mut token)?)?;
    }
    let config = manifest.get("config")?.get("digest")?.as_str()?.to_string();
    if !digest_ok(&config) {
        return None;
    }
    let config = json(registry_get(&r, &format!("blobs/{config}"), "*/*", &mut token)?)?;
    let ids = config.get("rootfs")?.get("diff_ids")?.as_array()?;
    let ids: Vec<String> = ids.iter().filter_map(|v| v.as_str().map(String::from)).collect();
    (!ids.is_empty()).then_some(ids)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOCKERFILE: &str = "ARG PHP_VERSION=8.4\n\
        FROM serversideup/php:${PHP_VERSION}-cli AS php-base\n\
        FROM php-base AS composer\n\
        FROM node:22-bookworm-slim AS node\n\
        FROM php-base AS application\n\
        FROM serversideup/php:${PHP_VERSION}-fpm-nginx AS web\n\
        COPY --from=application /app /app\n\
        FROM application AS worker\n";

    #[test]
    fn follows_stages_and_args() {
        assert_eq!(base_image(DOCKERFILE, Some("web")).as_deref(), Some("serversideup/php:8.4-fpm-nginx"));
        assert_eq!(base_image(DOCKERFILE, Some("worker")).as_deref(), Some("serversideup/php:8.4-cli"));
        assert_eq!(base_image(DOCKERFILE, None).as_deref(), Some("serversideup/php:8.4-cli"));
        assert_eq!(base_image("FROM --platform=linux/amd64 ruby:3.4 AS base\n", None).as_deref(), Some("ruby:3.4"));
        assert_eq!(base_image(DOCKERFILE, Some("missing")), None);
    }

    #[test]
    fn image_sources() {
        assert_eq!(
            github_repo("https://github.com/serversideup/docker-php").as_deref(),
            Some("serversideup/docker-php")
        );
        assert_eq!(github_repo("https://github.com/org/app.git/").as_deref(), Some("org/app"));
        for bad in [
            "https://gitlab.com/org/app",
            "https://github.com/org",
            "https://github.com/../user",
            "https://github.com/org/app/../../user",
            "https://github.com/org/app?x=",
            "https://github.com/{owner}/{repo}",
            "https://github.com/-x/app",
        ] {
            assert_eq!(github_repo(bad), None, "{bad}");
        }
    }

    #[test]
    fn image_refs() {
        let r = |s: &str| parse_ref(s).map(|r| format!("{} {} {}", r.host, r.repo, r.tag));
        assert_eq!(r("ruby:3.4").as_deref(), Some("registry-1.docker.io library/ruby 3.4"));
        assert_eq!(
            r("serversideup/php:8.5-fpm-nginx").as_deref(),
            Some("registry-1.docker.io serversideup/php 8.5-fpm-nginx")
        );
        assert_eq!(
            r("docker.umami.is/umami-software/umami:postgresql-latest").as_deref(),
            Some("docker.umami.is umami-software/umami postgresql-latest")
        );
        assert_eq!(r("ghcr.io/org/app").as_deref(), Some("ghcr.io org/app latest"));
        assert_eq!(r("localhost:5000/app:dev").as_deref(), Some("localhost:5000 app dev"));
        for bad in ["ruby@sha256:abc", "evil.com/../x:1", "Ruby:3", "host.com/app:a?b", "ghcr.io/:x", "a.io/x:$(id)"] {
            assert_eq!(parse_ref(bad), None, "{bad}");
        }
    }

    /// `cargo test -p kgui-core registry_layers -- --ignored` (needs network).
    #[test]
    #[ignore]
    fn registry_layers() {
        for image in ["docker.umami.is/umami-software/umami:postgresql-latest", "serversideup/php:8.5-fpm-nginx"] {
            let layers = tag_layers(image, "amd64");
            assert!(layers.as_ref().is_some_and(|l| !l.is_empty()), "{image}: {layers:?}");
        }
    }

    #[test]
    fn only_public_hosts() {
        use std::net::IpAddr;
        for ip in [
            "10.0.0.25",
            "192.168.1.1",
            "127.0.0.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:10.0.0.1",
        ] {
            assert!(!is_public(ip.parse::<IpAddr>().unwrap()), "{ip}");
        }
        for ip in ["140.82.112.33", "2606:4700::6810:84e5"] {
            assert!(is_public(ip.parse::<IpAddr>().unwrap()), "{ip}");
        }
        for url in [
            "https://10.0.0.25/v2/x",
            "https://localhost:5000/v2/x",
            "https://[::1]/v2/x",
            "https://registry.local/v2/x",
            "https://intranet/v2/x",
            "http://ghcr.io/v2/x",
        ] {
            assert_eq!(public_pin(url), None, "{url}");
        }
    }

    #[test]
    fn pinned_and_release() {
        assert!(!is_pinned("serversideup/php:8.4-fpm-nginx"));
        assert!(is_pinned("serversideup/php:8.4-fpm-nginx-v5.1.4"));
        assert!(is_pinned("ruby@sha256:abc"));
        assert_eq!(release_of("v5.1.4-44343912330"), "v5.1.4");
        assert_eq!(release_of("v5.1.4"), "v5.1.4");
        assert_eq!(release_of("1.2.3-rc1"), "1.2.3-rc1");
    }
}
