mod about;
mod ansi;
mod app;
mod build_info;
mod console;
mod dialogs;
mod history_view;
mod menu;
mod project_view;
mod settings_page;
mod task;
mod terminal;
mod theme;
mod update;

use kgui_core::{env, history::History, settings};

fn main() -> eframe::Result {
    // The app binary doubles as the `kgui` CLI (`.kamal/secrets` calls `kgui secret`).
    let args: Vec<String> = std::env::args().collect();
    if let Some(code) = kgui_core::cli::dispatch(&args) {
        std::process::exit(code);
    }

    install_kgui_shim();
    if let Ok(h) = History::open() {
        let _ = h.mark_interrupted();
    }
    // Resolving the login-shell PATH takes a moment; do it before it's needed.
    std::thread::spawn(|| {
        env::login_path();
    });

    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Kamal GUI")
        .with_inner_size([1320.0, 860.0])
        .with_min_inner_size([960.0, 600.0]);
    if let Ok(icon) = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png")) {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        // winit's default macOS menu quits on Cmd+Q without asking the app,
        // which would cut a running deploy off. The app handles Cmd+Q itself.
        #[cfg(target_os = "macos")]
        event_loop_builder: Some(Box::new(|builder| {
            use winit::platform::macos::EventLoopBuilderExtMacOS;
            builder.with_default_menu(false);
        })),
        ..Default::default()
    };
    eframe::run_native("Kamal GUI", options, Box::new(|cc| Ok(Box::new(app::KamalApp::new(cc)))))
}

/// `.kamal/secrets` calls `kgui secret …`. Expose this binary as `kgui` in a
/// directory that is put first on PATH for every kamal process.
fn install_kgui_shim() {
    // Private data dir first: kamal runs whatever is called `kgui` in here.
    let _ = settings::ensure_data_dir();
    let dir = settings::data_dir().join("bin");
    if let (Ok(()), Ok(exe)) = (std::fs::create_dir_all(&dir), std::env::current_exe()) {
        let link = dir.join("kgui");
        if std::fs::read_link(&link).ok().as_deref() != Some(exe.as_path()) {
            let _ = std::fs::remove_file(&link);
            let _ = std::os::unix::fs::symlink(&exe, &link);
        }
    }
    env::set_extra_bin_dir(dir);
}
