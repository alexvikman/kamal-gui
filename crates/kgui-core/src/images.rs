//! Base images: which external image a unit's Dockerfile builds on, which
//! version of it is running, and the newest release upstream.
//!
//! Version info comes from OCI labels (`org.opencontainers.image.version`,
//! `org.opencontainers.image.source`) that images like serversideup/php set
//! and that the app image inherits.

use crate::env::{command, run};
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
    let repo = source.trim_end_matches('/').trim_end_matches(".git").strip_prefix("https://github.com/")?.to_string();
    let result = run(command("gh").args([
        "api",
        &format!("repos/{repo}/releases/latest"),
        "--jq",
        "[.tag_name, .published_at, .html_url] | @tsv",
    ]))
    .ok()
    .and_then(|out| {
        let mut p = out.split('\t');
        Some(Release { tag: p.next()?.to_string(), published_at: p.next()?.to_string(), url: p.next()?.to_string() })
    });
    cache.lock().unwrap().insert(source.to_string(), (Instant::now(), result.clone()));
    result
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
    fn pinned_and_release() {
        assert!(!is_pinned("serversideup/php:8.4-fpm-nginx"));
        assert!(is_pinned("serversideup/php:8.4-fpm-nginx-v5.1.4"));
        assert!(is_pinned("ruby@sha256:abc"));
        assert_eq!(release_of("v5.1.4-44343912330"), "v5.1.4");
        assert_eq!(release_of("v5.1.4"), "v5.1.4");
        assert_eq!(release_of("1.2.3-rc1"), "1.2.3-rc1");
    }
}
