//! Bakes version, commit and build date into the binary (see `build_info.rs`).
//!
//! CI sets `KGUI_VERSION` (e.g. `0.1.0-nightly.42`) and `GITHUB_SHA`; local
//! builds fall back to `<crate version>-dev` and `git rev-parse HEAD`.

use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    println!("cargo:rerun-if-env-changed=KGUI_VERSION");
    println!("cargo:rerun-if-env-changed=GITHUB_SHA");
    println!("cargo:rerun-if-env-changed=GITHUB_RUN_NUMBER");
    // Pick up new commits in local builds.
    for path in ["../.git/HEAD", "../.git/refs/heads"] {
        if std::path::Path::new(path).exists() {
            println!("cargo:rerun-if-changed={path}");
        }
    }

    let crate_version = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
    // GitHub Actions always sets GITHUB_RUN_NUMBER; the nightly job builds there.
    let version = std::env::var("KGUI_VERSION")
        .ok()
        .filter(|v| !v.is_empty())
        .or_else(|| std::env::var("GITHUB_RUN_NUMBER").ok().map(|n| format!("{crate_version}-nightly.{n}")))
        .unwrap_or_else(|| format!("{crate_version}-dev"));
    let sha = std::env::var("GITHUB_SHA")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            let out = Command::new("git").args(["rev-parse", "HEAD"]).output().ok()?;
            out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
        })
        .unwrap_or_else(|| "unknown".into());
    let days = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() / 86_400).unwrap_or(0) as i64;
    let (y, m, d) = civil_from_days(days);

    println!("cargo:rustc-env=KGUI_BUILD_VERSION={version}");
    println!("cargo:rustc-env=KGUI_BUILD_SHA={sha}");
    println!("cargo:rustc-env=KGUI_BUILD_DATE={y:04}-{m:02}-{d:02}");
}

/// Days since 1970-01-01 → (year, month, day). Howard Hinnant's algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}
