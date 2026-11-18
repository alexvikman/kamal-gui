//! Self-update from the rolling "nightly" GitHub release.
//!
//! Check: compare the release's commit with the one this binary was built
//! from. Install: download the zip and SHA256SUMS, verify the checksum and
//! (when `gh` is available) the signed build provenance, check the bundle id
//! and signature, then swap the app bundle in place. The caller relaunches.

use crate::build_info;
use anyhow::{Context, Result, bail};
use kgui_core::env::{command, run};
use serde::Deserialize;
use std::path::{Path, PathBuf};

const BUNDLE_ID: &str = "io.github.alexvikman.kamal-gui";
const ZIP_ASSET: &str = "KamalGUI-nightly-macos-universal.zip";
const SUMS_ASSET: &str = "SHA256SUMS";

#[derive(Debug, Clone)]
pub struct Release {
    pub title: String,
    /// Commit the release was built from.
    pub sha: String,
    pub published_at: String,
    pub url: String,
    zip_url: String,
    sums_url: String,
}

impl Release {
    pub fn short_sha(&self) -> &str {
        &self.sha[..self.sha.len().min(7)]
    }

    /// Built from a different commit than the running app.
    pub fn is_newer(&self) -> bool {
        build_info::SHA != "unknown" && self.sha != build_info::SHA
    }

    pub fn compare_url(&self) -> String {
        format!("{}/compare/{}...{}", build_info::repo_url(), build_info::SHA, self.sha)
    }
}

#[derive(Deserialize)]
struct ApiRelease {
    name: Option<String>,
    target_commitish: String,
    published_at: Option<String>,
    html_url: String,
    #[serde(default)]
    assets: Vec<ApiAsset>,
}

#[derive(Deserialize)]
struct ApiAsset {
    name: String,
    browser_download_url: String,
}

fn parse_release(json: &str) -> Result<Release> {
    let r: ApiRelease = serde_json::from_str(json).context("unexpected response from GitHub")?;
    let asset = |name: &str| {
        r.assets
            .iter()
            .find(|a| a.name == name)
            .map(|a| a.browser_download_url.clone())
            .with_context(|| format!("the nightly release has no {name}"))
    };
    Ok(Release {
        title: r.name.unwrap_or_else(|| "Nightly".into()),
        sha: r.target_commitish.clone(),
        published_at: r.published_at.unwrap_or_default(),
        url: r.html_url.clone(),
        zip_url: asset(ZIP_ASSET)?,
        sums_url: asset(SUMS_ASSET)?,
    })
}

/// The newest nightly release (public API, no login needed).
pub fn check() -> Result<Release> {
    let url = format!("https://api.github.com/repos/{}/releases/tags/nightly", build_info::REPO);
    let json = run(command("curl").args([
        "-fsSL",
        "--max-time",
        "20",
        "-H",
        "Accept: application/vnd.github+json",
        "-H",
        "User-Agent: kamal-gui",
        &url,
    ]))
    .context("could not reach GitHub")?;
    parse_release(&json)
}

/// `/Applications/Kamal GUI.app` when running from an app bundle; None for
/// `cargo run` builds, which can't update themselves.
pub fn current_bundle() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    bundle_of(&exe)
}

fn bundle_of(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let bundle = contents.parent()?;
    (macos.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && bundle.extension().is_some_and(|e| e == "app"))
    .then(|| bundle.to_path_buf())
}

/// Expected digest for `file_name` from a `shasum -a 256` listing.
fn expected_digest(sums: &str, file_name: &str) -> Option<String> {
    sums.lines().find_map(|l| {
        let (digest, name) = l.split_once(char::is_whitespace)?;
        (name.trim().trim_start_matches('*') == file_name).then(|| digest.to_lowercase())
    })
}

fn sha256(path: &Path) -> Result<String> {
    let out = run(command("shasum").args(["-a", "256"]).arg(path))?;
    out.split_whitespace().next().map(str::to_lowercase).context("shasum printed nothing")
}

fn download(url: &str, to: &Path) -> Result<()> {
    run(command("curl").args(["-fsSL", "--max-time", "600", "-H", "User-Agent: kamal-gui", "-o"]).arg(to).arg(url))
        .with_context(|| format!("download failed: {url}"))?;
    Ok(())
}

/// Download, verify and install `release` over the running app bundle.
/// `step` is told what's happening. Returns the bundle path to relaunch.
pub fn install(release: &Release, step: &dyn Fn(&str)) -> Result<PathBuf> {
    let bundle = current_bundle().context("this is a development build; updates install into Kamal GUI.app")?;
    install_into(release, &bundle, step)?;
    Ok(bundle)
}

/// Replace the app bundle at `bundle` with `release`.
pub fn install_into(release: &Release, bundle: &Path, step: &dyn Fn(&str)) -> Result<()> {
    let parent = bundle.parent().context("app bundle has no parent folder")?;
    // Work next to the bundle so the final swap is a rename on one volume.
    let work = parent.join(format!(".kamal-gui-update-{}", std::process::id()));
    if work.exists() {
        std::fs::remove_dir_all(&work)?;
    }
    std::fs::create_dir_all(&work)
        .with_context(|| format!("can't write to {} (move the app somewhere you can write to)", parent.display()))?;
    let result = install_in(release, bundle, &work, step);
    let _ = std::fs::remove_dir_all(&work);
    result
}

fn install_in(release: &Release, bundle: &Path, work: &Path, step: &dyn Fn(&str)) -> Result<()> {
    let zip = work.join(ZIP_ASSET);
    let sums = work.join(SUMS_ASSET);

    step("Downloading…");
    download(&release.zip_url, &zip)?;
    download(&release.sums_url, &sums)?;

    step("Verifying checksum…");
    let listing = std::fs::read_to_string(&sums)?;
    let expected = expected_digest(&listing, ZIP_ASSET).context("SHA256SUMS doesn't list the app")?;
    let actual = sha256(&zip)?;
    if actual != expected {
        bail!("checksum mismatch for {ZIP_ASSET}: expected {expected}, got {actual}");
    }

    // The checksum only proves the download matches the release. The signed
    // provenance proves the release was built by this repo's CI.
    if run(command("gh").args(["auth", "status"])).is_ok() {
        step("Verifying build provenance…");
        run(command("gh").args(["attestation", "verify"]).arg(&zip).args(["-R", build_info::REPO]))
            .context("build provenance could not be verified")?;
    }

    step("Unpacking…");
    let extract = work.join("extract");
    run(command("ditto").args(["-x", "-k"]).arg(&zip).arg(&extract))?;
    let new_app = extract.join("Kamal GUI.app");
    let plist = new_app.join("Contents/Info.plist");
    let id = run(command("/usr/libexec/PlistBuddy").args(["-c", "Print :CFBundleIdentifier"]).arg(&plist))?;
    if id.trim() != BUNDLE_ID {
        bail!("unexpected bundle id {id:?} in the download");
    }
    run(command("codesign").args(["--verify", "--deep", "--strict"]).arg(&new_app))
        .context("the downloaded app's signature is invalid")?;

    step("Installing…");
    let old = work.join("previous.app");
    std::fs::rename(bundle, &old).context("could not move the current app aside")?;
    if let Err(e) = std::fs::rename(&new_app, bundle) {
        let _ = std::fs::rename(&old, bundle);
        return Err(e).context("could not move the new app into place");
    }
    Ok(())
}

/// Start the (new) app and let the caller close this one.
pub fn relaunch(bundle: &Path) -> Result<()> {
    std::process::Command::new("/usr/bin/open").arg("-n").arg(bundle).spawn().context("could not relaunch")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_release() {
        let json = r#"{"name":"Nightly 2026-11-18 (1272bf9)","target_commitish":"1272bf9abc","published_at":"2026-11-18T19:20:00Z",
            "html_url":"https://github.com/alexvikman/kamal-gui/releases/tag/nightly",
            "assets":[{"name":"SHA256SUMS","browser_download_url":"https://x/SHA256SUMS"},
                      {"name":"KamalGUI-nightly-macos-universal.zip","browser_download_url":"https://x/app.zip"}]}"#;
        let r = parse_release(json).unwrap();
        assert_eq!(r.short_sha(), "1272bf9");
        assert_eq!(r.zip_url, "https://x/app.zip");
        assert!(parse_release(r#"{"target_commitish":"a","html_url":"u","assets":[]}"#).is_err());
    }

    #[test]
    fn finds_digest() {
        let sums = "ABC123  KamalGUI-nightly-macos-universal.zip\ndef456  kgui-nightly-macos-universal.tar.gz\n";
        assert_eq!(expected_digest(sums, ZIP_ASSET).as_deref(), Some("abc123"));
        assert_eq!(expected_digest(sums, "missing"), None);
    }

    /// End to end against the real nightly: download, verify, swap.
    /// `cargo test -p kamal-gui installs_latest_nightly -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn installs_latest_nightly_into_a_scratch_bundle() {
        let dir = std::env::temp_dir().join(format!("kgui-update-test-{}", std::process::id()));
        let bundle = dir.join("Kamal GUI.app");
        std::fs::create_dir_all(bundle.join("Contents/MacOS")).unwrap();
        std::fs::write(bundle.join("Contents/MacOS/kamal-gui"), b"old").unwrap();

        let release = check().expect("nightly release");
        println!("release: {} ({})", release.title, release.short_sha());
        install_into(&release, &bundle, &|s| println!("  {s}")).expect("install");

        let exe = std::fs::read(bundle.join("Contents/MacOS/kamal-gui")).unwrap();
        assert!(exe.len() > 1_000_000, "binary was not replaced");
        let id = run(command("/usr/libexec/PlistBuddy")
            .args(["-c", "Print :CFBundleIdentifier"])
            .arg(bundle.join("Contents/Info.plist")))
        .unwrap();
        assert_eq!(id, BUNDLE_ID);
        assert!(std::fs::read_dir(&dir).unwrap().count() == 1, "work dir left behind");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn detects_bundles() {
        let exe = Path::new("/Applications/Kamal GUI.app/Contents/MacOS/kamal-gui");
        assert_eq!(bundle_of(exe).as_deref(), Some(Path::new("/Applications/Kamal GUI.app")));
        assert_eq!(bundle_of(Path::new("/repo/target/debug/kamal-gui")), None);
    }
}
