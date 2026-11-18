//! Version information baked in by `build.rs`.

pub const VERSION: &str = env!("KGUI_BUILD_VERSION");
pub const SHA: &str = env!("KGUI_BUILD_SHA");
pub const DATE: &str = env!("KGUI_BUILD_DATE");
pub const REPO: &str = "alexvikman/kamal-gui";

pub fn short_sha() -> &'static str {
    &SHA[..SHA.len().min(7)]
}

pub fn repo_url() -> String {
    format!("https://github.com/{REPO}")
}
