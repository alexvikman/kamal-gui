//! Browse GitHub repositories through the `gh` CLI (uses its login).

use crate::env::{command, run};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhRepo {
    name: String,
    name_with_owner: String,
    ssh_url: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    pushed_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Repo {
    pub name: String,
    pub full_name: String,
    pub url: String,
    pub description: Option<String>,
    pub pushed_at: Option<String>,
    /// Has `config/deploy*.yml` according to GitHub code search. None when
    /// search was unavailable.
    pub kamal: Option<bool>,
}

/// Owners the signed-in user can browse: the user and their organizations.
pub fn owners() -> Result<Vec<String>> {
    let user = run(command("gh").args(["api", "user", "--jq", ".login"])).context("gh is not signed in (run `gh auth login`)")?;
    let orgs = run(command("gh").args(["api", "user/orgs", "--jq", ".[].login"])).unwrap_or_default();
    let mut out: Vec<String> = orgs.lines().map(String::from).collect();
    out.sort();
    out.push(user);
    Ok(out)
}

pub fn repos(owner: &str) -> Result<Vec<Repo>> {
    let out = run(command("gh").args([
        "repo",
        "list",
        owner,
        "--limit",
        "1000",
        "--no-archived",
        "--json",
        "name,nameWithOwner,sshUrl,description,pushedAt",
    ]))
    .with_context(|| format!("could not list repositories for {owner}"))?;
    let list: Vec<GhRepo> = serde_json::from_str(&out).context("unexpected output from gh")?;
    let kamal = kamal_repos(owner).ok();
    Ok(list
        .into_iter()
        .map(|r| Repo {
            kamal: kamal.as_ref().map(|k| k.contains(&r.name_with_owner)),
            name: r.name,
            full_name: r.name_with_owner,
            url: r.ssh_url,
            description: r.description.filter(|d| !d.is_empty()),
            pushed_at: r.pushed_at,
        })
        .collect())
}

/// Repositories with a Kamal deploy config, via one code search request.
fn kamal_repos(owner: &str) -> Result<HashSet<String>> {
    #[derive(Deserialize)]
    struct Hit {
        path: String,
        repository: HitRepo,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct HitRepo {
        name_with_owner: String,
    }
    let out = run(command("gh").args([
        "search", "code", "service", "--owner", owner, "--filename", "deploy.yml", "--json", "path,repository",
        "--limit", "1000",
    ]))?;
    let hits: Vec<Hit> = serde_json::from_str(&out)?;
    Ok(hits
        .into_iter()
        .filter(|h| h.path.starts_with("config/deploy"))
        .map(|h| h.repository.name_with_owner)
        .collect())
}
