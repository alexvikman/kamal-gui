//! The macOS menu bar. winit's default menu is turned off (its Quit item
//! terminates without asking), so the app builds its own: Quit and Close go
//! through the app, which asks first when a deploy is running.

use std::sync::mpsc::{self, Receiver};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuAction {
    About,
    CheckForUpdates,
    Settings,
    AddProject,
    CloseWindow,
    Quit,
    OpenGithub,
    OpenKamalDocs,
}

/// Keeps the native menu alive for as long as the app runs.
pub struct AppMenu {
    #[cfg(target_os = "macos")]
    _menu: muda::Menu,
    pub actions: Receiver<MenuAction>,
}

#[cfg(target_os = "macos")]
pub fn install(ctx: &egui::Context) -> AppMenu {
    use muda::{Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu, accelerator::Accelerator};

    let accel = |s: &str| s.parse::<Accelerator>().ok();
    let item = |id: &str, text: &str, key: Option<&str>| MenuItem::with_id(id, text, true, key.and_then(accel));

    let app = Submenu::with_items(
        "Kamal GUI",
        true,
        &[
            &item("about", "About Kamal GUI", None),
            &item("updates", "Check for Updates…", None),
            &PredefinedMenuItem::separator(),
            &item("settings", "Settings…", Some("CmdOrCtrl+Comma")),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::hide(None),
            &PredefinedMenuItem::hide_others(None),
            &PredefinedMenuItem::show_all(None),
            &PredefinedMenuItem::separator(),
            &item("quit", "Quit Kamal GUI", Some("CmdOrCtrl+Q")),
        ],
    )
    .expect("app menu");
    let file = Submenu::with_items(
        "File",
        true,
        &[
            &item("add-project", "Add Project…", Some("CmdOrCtrl+N")),
            &PredefinedMenuItem::separator(),
            &item("close", "Close Window", Some("CmdOrCtrl+W")),
        ],
    )
    .expect("file menu");
    let window = Submenu::with_items(
        "Window",
        true,
        &[
            &PredefinedMenuItem::minimize(None),
            &PredefinedMenuItem::maximize(Some("Zoom")),
            &PredefinedMenuItem::fullscreen(None),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::bring_all_to_front(None),
        ],
    )
    .expect("window menu");
    let help = Submenu::with_items(
        "Help",
        true,
        &[&item("github", "Kamal GUI on GitHub", None), &item("kamal-docs", "Kamal Documentation", None)],
    )
    .expect("help menu");

    let menu = Menu::new();
    menu.append_items(&[&app, &file, &window, &help]).expect("menu bar");
    menu.init_for_nsapp();
    window.set_as_windows_menu_for_nsapp();
    help.set_as_help_menu_for_nsapp();

    let (tx, actions) = mpsc::channel();
    let ctx = ctx.clone();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        let action = match event.id().0.as_str() {
            "about" => MenuAction::About,
            "updates" => MenuAction::CheckForUpdates,
            "settings" => MenuAction::Settings,
            "add-project" => MenuAction::AddProject,
            "close" => MenuAction::CloseWindow,
            "quit" => MenuAction::Quit,
            "github" => MenuAction::OpenGithub,
            "kamal-docs" => MenuAction::OpenKamalDocs,
            _ => return,
        };
        let _ = tx.send(action);
        ctx.request_repaint();
    }));
    AppMenu { _menu: menu, actions }
}

#[cfg(not(target_os = "macos"))]
pub fn install(_ctx: &egui::Context) -> AppMenu {
    let (_tx, actions) = mpsc::channel();
    AppMenu { actions }
}
