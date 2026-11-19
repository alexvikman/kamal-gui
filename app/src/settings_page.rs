//! Settings as a page in the main area, with General and About tabs.

use crate::about::About;
use crate::app::{Confirm, Intent, Request};
use crate::task::{self, Task};
use crate::theme;
use crate::update::Release;
use egui::{RichText, ScrollArea, Ui};
use kgui_core::kamal;
use kgui_core::link::{self, Link, tilde};
use kgui_core::settings::{Settings, data_dir};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsTab {
    General,
    About,
}

pub struct SettingsPage {
    pub tab: SettingsTab,
    general: GeneralForm,
    cli: CliTool,
    /// Created the first time the About tab is shown (it checks for updates).
    about: Option<About>,
}

/// What the app should do after drawing the page.
pub enum PageResult {
    None,
    /// An update was installed; relaunch from this bundle.
    Relaunch(PathBuf),
}

impl SettingsPage {
    pub fn new(tab: SettingsTab) -> Self {
        Self { tab, general: GeneralForm::load(), cli: CliTool::default(), about: None }
    }

    /// Newest release the About tab has seen, for the sidebar notice.
    pub fn latest_release(&self) -> Option<&Release> {
        self.about.as_ref().and_then(|a| a.latest())
    }

    /// The link in `~/.local/bin` was installed, or an attempt was made.
    pub fn recheck_cli(&mut self) {
        self.cli.stale = true;
    }

    pub fn ui(
        &mut self,
        ui: &mut Ui,
        requests: &mut Vec<Request>,
        known_release: Option<Release>,
        runs_active: bool,
    ) -> PageResult {
        ui.label(theme::title("Settings", 24.0));
        ui.add_space(14.0);
        theme::tabs(ui, &mut self.tab, &[(SettingsTab::General, "General"), (SettingsTab::About, "About")]);
        ui.add_space(18.0);

        let mut result = PageResult::None;
        ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            ui.set_max_width(680.0);
            match self.tab {
                SettingsTab::General => self.general.ui(ui, &mut self.cli, requests),
                SettingsTab::About => {
                    let about = self.about.get_or_insert_with(|| About::new(ui.ctx(), known_release));
                    if let Some(bundle) = about.ui(ui, runs_active) {
                        result = PageResult::Relaunch(bundle);
                    }
                }
            }
        });
        result
    }
}

struct GeneralForm {
    saved: Settings,
    edit: Settings,
    kamal_path: String,
    detected: String,
    error: Option<String>,
    just_saved: bool,
}

impl GeneralForm {
    fn load() -> Self {
        let saved = Settings::load();
        let detected = kamal::binary(&Settings { kamal_path: None, ..saved.clone() }).display().to_string();
        Self {
            kamal_path: saved.kamal_path.as_ref().map(|p| p.display().to_string()).unwrap_or_default(),
            edit: saved.clone(),
            saved,
            detected,
            error: None,
            just_saved: false,
        }
    }

    fn current(&self) -> Settings {
        let path = self.kamal_path.trim();
        Settings { kamal_path: (!path.is_empty()).then(|| path.into()), ..self.edit.clone() }
    }

    fn ui(&mut self, ui: &mut Ui, cli: &mut CliTool, requests: &mut Vec<Request>) {
        let p = theme::palette(ui);
        let field = |ui: &mut Ui, title: &str, help: &str, add: &mut dyn FnMut(&mut Ui)| {
            ui.label(RichText::new(title).family(theme::medium()));
            add(ui);
            ui.label(theme::caption(help));
            ui.add_space(14.0);
        };

        ui.label(theme::title("Secrets", 15.0));
        ui.add_space(6.0);
        theme::card(ui, |ui| {
            field(
                ui,
                "1Password account",
                "Sign-in address used for op:// references in .kamal/secrets. Leave empty to use op's default account.",
                &mut |ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.edit.op_account)
                            .hint_text("example.1password.com")
                            .font(egui::TextStyle::Monospace)
                            .desired_width(f32::INFINITY),
                    );
                },
            );
        });

        ui.add_space(18.0);
        ui.label(theme::title("Tools", 15.0));
        ui.add_space(6.0);
        theme::card(ui, |ui| {
            let detected = self.detected.clone();
            field(
                ui,
                "Path to kamal",
                "Leave empty to use the newest Ruby that has kamal installed (mise, rbenv, asdf or chruby).",
                &mut |ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.kamal_path)
                            .hint_text(format!("Auto-detected: {detected}"))
                            .font(egui::TextStyle::Monospace)
                            .desired_width(f32::INFINITY),
                    );
                },
            );
            field(
                ui,
                "GitHub owner",
                "Organization or user shown first when adding projects from GitHub.",
                &mut |ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.edit.github_owner)
                            .hint_text("your-org")
                            .desired_width(f32::INFINITY),
                    );
                },
            );
            ui.label(RichText::new("Command line tool").family(theme::medium()));
            cli.ui(ui, requests);
        });

        ui.add_space(18.0);
        ui.label(theme::title("Data", 15.0));
        ui.add_space(6.0);
        theme::card(ui, |ui| {
            ui.label(RichText::new("App data").family(theme::medium()));
            ui.horizontal(|ui| {
                ui.label(RichText::new(data_dir().display().to_string()).monospace().size(12.0));
                if theme::link(ui, "Show in Finder").clicked() {
                    let _ = std::process::Command::new("/usr/bin/open").arg(data_dir()).spawn();
                }
            });
            ui.label(theme::caption("Project clones, deploy worktrees, run history and logs."));
        });

        ui.add_space(18.0);
        let current = self.current();
        let dirty = serde_json::to_string(&current).ok() != serde_json::to_string(&self.saved).ok();
        if dirty {
            self.just_saved = false;
        }
        ui.horizontal(|ui| {
            if ui.add_enabled(dirty, theme::primary("Save")).clicked() {
                match current.save() {
                    Ok(()) => {
                        self.saved = current;
                        self.just_saved = true;
                        self.error = None;
                    }
                    Err(e) => self.error = Some(format!("{e:#}")),
                }
            }
            if dirty && theme::quiet(ui, "Revert").clicked() {
                *self = Self::load();
            }
            if self.just_saved {
                ui.label(RichText::new("Saved").color(p.ok));
            }
        });
        if let Some(e) = &self.error {
            ui.colored_label(p.bad, e);
        }
    }
}

/// `kgui` in the user's terminal: which one it is, and the link that makes it
/// the app's own.
struct CliTool {
    status: Option<Result<link::Status, String>>,
    check: Option<Task<link::Status>>,
    /// Look (again) at the next frame.
    stale: bool,
}

impl Default for CliTool {
    fn default() -> Self {
        Self { status: None, check: None, stale: true }
    }
}

impl CliTool {
    fn ui(&mut self, ui: &mut Ui, requests: &mut Vec<Request>) {
        if let Some(result) = task::poll(&mut self.check) {
            self.status = Some(result);
        }
        if self.stale && self.check.is_none() {
            self.stale = false;
            // Asks the login shell for its PATH, which takes a moment.
            self.check = Some(Task::spawn(ui.ctx(), link::status));
        }
        let p = theme::palette(ui);
        let status = match &self.status {
            None => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(theme::caption("Looking for kgui in your terminal…"));
                });
                return;
            }
            Some(Err(e)) => {
                ui.colored_label(p.bad, e);
                None
            }
            Some(Ok(status)) => Some(status),
        };
        let view = status.map(cli_view);
        if let Some(view) = &view {
            ui.horizontal(|ui| {
                theme::dot(ui, if view.ok { p.ok } else { p.warn });
                ui.label(&view.headline);
            });
            ui.label(theme::caption(&view.detail));
        }
        let checking = self.check.is_some();
        ui.horizontal(|ui| {
            if let (Some(action), Some(status)) = (view.and_then(|v| v.action), status) {
                let label = if action == CliAction::Install { INSTALL } else { REPLACE };
                if ui.add_enabled(!checking, theme::secondary(label)).clicked() {
                    requests.push(match action {
                        CliAction::Install => Request::Do(Intent::InstallCli { replace: false }),
                        CliAction::Replace => Request::Confirm(confirm_replace(status)),
                    });
                }
            }
            if checking {
                ui.spinner();
            } else if theme::quiet_rich(ui, RichText::new("Check again").color(p.muted)).clicked() {
                self.stale = true;
            }
        });
    }
}

const INSTALL: &str = "Install command line tool";
const REPLACE: &str = "Replace with a link to the app…";

#[derive(Debug, PartialEq)]
enum CliAction {
    /// Nothing is in the way: install the link.
    Install,
    /// Something is at `~/.local/bin/kgui`: ask before replacing it.
    Replace,
}

struct CliView {
    ok: bool,
    headline: String,
    detail: String,
    action: Option<CliAction>,
}

/// What to say about `kgui` in the terminal, and what to offer.
fn cli_view(s: &link::Status) -> CliView {
    let link = tilde(&s.path);
    let dir = s.path.parent().map(tilde).unwrap_or_default();
    let ours = s.link == Link::Ours;
    // Offered whenever the terminal's kgui isn't the app's and the link isn't there yet.
    let action = match s.link {
        Link::Ours => None,
        Link::Missing => Some(CliAction::Install),
        Link::OtherLink(_) | Link::File => Some(CliAction::Replace),
    };
    let add_to_path = format!(
        "Add {dir} to PATH in your shell's startup file, for example `export PATH=\"$HOME/.local/bin:$PATH\"` in \
         ~/.zshrc, then check again."
    );
    let Some(used) = s.used.as_deref().map(tilde) else {
        let (headline, detail) = match (ours, s.dir_on_path) {
            (true, false) => {
                (format!("{dir} isn't on your PATH"), format!("The link to the app is in place. {add_to_path}"))
            }
            (true, true) => (
                "`kgui` in your terminal doesn't lead to the app".to_string(),
                format!("{link} is in place, but the app's own link behind it is broken. Restart the app."),
            ),
            (false, _) => (
                "`kgui` isn't installed in your terminal".to_string(),
                format!("Installing puts a link to the app at {link}, so the command is updated with the app."),
            ),
        };
        return CliView { ok: false, headline, detail, action };
    };
    if s.follows_app {
        let how = if ours && s.uses_link() { "is a link to the app" } else { "leads to the app" };
        return CliView {
            ok: true,
            headline: "`kgui` in your terminal is this app".into(),
            detail: format!("{used} {how}, so it is updated with it."),
            action: None,
        };
    }
    if ours && s.uses_link() {
        let target = s.target.as_deref().map(tilde).unwrap_or_default();
        return CliView {
            ok: false,
            headline: "`kgui` in your terminal is another copy of the app".into(),
            detail: format!("The link follows the copy that was started last: {target}"),
            action: None,
        };
    }
    let mut detail = format!("{used} isn't updated with the app, so it falls behind.");
    if s.uses_link() {
        detail += " Replace it with a link to the app to keep the two in step.";
    } else if ours && s.dir_on_path {
        detail +=
            &format!(" It comes before {dir} on your PATH, where the link to the app is. Remove it to use the link.");
    } else if ours {
        detail += &format!(" The link to the app is in place, but {dir} isn't on your PATH. {add_to_path}");
    }
    CliView { ok: false, headline: "`kgui` in your terminal is a separate copy".into(), detail, action }
}

/// Replacing what is at `~/.local/bin/kgui` deletes it: ask first.
fn confirm_replace(s: &link::Status) -> Confirm {
    let link = tilde(&s.path);
    let (body, detail) = match &s.link {
        Link::OtherLink(target) => (
            "It is a link to somewhere else. It is replaced by a link to this app, which is updated with the app. \
             What it points at now is left as it is.",
            format!("{link} → {}", tilde(target)),
        ),
        _ => (
            "It is a separate copy of kgui. It is deleted and replaced by a link to this app, which is updated with \
             the app.",
            link.clone(),
        ),
    };
    Confirm {
        title: format!("Replace {link}?"),
        body: Some(body.into()),
        details: vec![detail],
        button: "Replace".into(),
        danger: true,
        intent: Intent::InstallCli { replace: true },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn status(link: Link, used: Option<&str>, follows_app: bool, dir_on_path: bool) -> link::Status {
        link::Status {
            path: "/Users/me/.local/bin/kgui".into(),
            link,
            used: used.map(PathBuf::from),
            target: used.map(PathBuf::from),
            follows_app,
            dir_on_path,
        }
    }

    #[test]
    fn offers_the_link_only_when_it_helps() {
        const LINK: &str = "/Users/me/.local/bin/kgui";
        const CARGO: &str = "/Users/me/.cargo/bin/kgui";
        let other = || Link::OtherLink(Path::new(CARGO).into());

        // The terminal runs the app: nothing to do.
        let v = cli_view(&status(Link::Ours, Some(LINK), true, true));
        assert!(v.ok && v.action.is_none());
        // Also through a link the user made themselves.
        let v = cli_view(&status(Link::Missing, Some("/usr/local/bin/kgui"), true, true));
        assert!(v.ok && v.action.is_none());

        // Nothing installed.
        let v = cli_view(&status(Link::Missing, None, false, true));
        assert!(!v.ok && v.action == Some(CliAction::Install));
        // A separate copy somewhere else, no link yet.
        let v = cli_view(&status(Link::Missing, Some(CARGO), false, true));
        assert!(!v.ok && v.action == Some(CliAction::Install) && v.detail.contains(CARGO));

        // Something of the user's is where the link goes: only after asking.
        for link in [Link::File, other()] {
            let v = cli_view(&status(link.clone(), Some(LINK), false, true));
            assert!(!v.ok && v.action == Some(CliAction::Replace));
            let v = cli_view(&status(link, None, false, false));
            assert!(!v.ok && v.action == Some(CliAction::Replace));
        }

        // The link is there but something else wins, or the folder isn't on
        // PATH: say so, there is nothing of ours left to install.
        let v = cli_view(&status(Link::Ours, Some(CARGO), false, true));
        assert!(!v.ok && v.action.is_none() && v.detail.contains("comes before"));
        let v = cli_view(&status(Link::Ours, None, false, false));
        assert!(!v.ok && v.action.is_none() && v.headline.contains("isn't on your PATH"));
        // The app's own link points at another copy of the app.
        let v = cli_view(&status(Link::Ours, Some(LINK), false, true));
        assert!(!v.ok && v.action.is_none() && v.headline.contains("another copy"));
    }

    /// Draw the row with `status` and click the button called `button`.
    fn click(status: link::Status, button: &str) -> Vec<Request> {
        use egui::{Event, PointerButton, Pos2, RawInput, Rect};
        let ctx = egui::Context::default();
        crate::theme::install(&ctx);
        let mut tool = CliTool { status: Some(Ok(status)), check: None, stale: false };
        let mut requests = Vec::new();
        let mut frame = |events: Vec<Event>| {
            let input = RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(900.0, 700.0))),
                events,
                ..Default::default()
            };
            let mut out = ctx.run_ui(input, |ui| {
                egui::CentralPanel::default().show(ui, |ui| tool.ui(ui, &mut requests));
            });
            out.textures_delta.clear();
            let drawn = |cs: &egui::epaint::ClippedShape| match &cs.shape {
                egui::Shape::Text(t) if t.galley.text() == button => Some(t.visual_bounding_rect().center()),
                _ => None,
            };
            out.shapes.iter().find_map(drawn)
        };
        let Some(pos) = frame(vec![]) else { return requests };
        let press = |pressed| Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        frame(vec![Event::PointerMoved(pos)]);
        frame(vec![press(true)]);
        frame(vec![press(false)]);
        requests
    }

    #[test]
    fn the_buttons_install_or_ask_first() {
        const LINK: &str = "/Users/me/.local/bin/kgui";
        let requests = click(status(Link::Missing, None, false, true), INSTALL);
        assert!(matches!(requests[..], [Request::Do(Intent::InstallCli { replace: false })]));

        // A copy of the user's own is in the way: a question, never the deed.
        let requests = click(status(Link::File, Some(LINK), false, true), REPLACE);
        let [Request::Confirm(c)] = &requests[..] else { panic!("no question asked") };
        assert!(matches!(c.intent, Intent::InstallCli { replace: true }));

        // Nothing is on offer once the terminal runs the app.
        for button in [INSTALL, REPLACE] {
            assert!(click(status(Link::Ours, Some(LINK), true, true), button).is_empty());
        }
    }

    #[test]
    fn replacing_says_what_is_given_up() {
        let c = confirm_replace(&status(Link::File, None, false, true));
        assert!(c.danger && matches!(c.intent, Intent::InstallCli { replace: true }));
        assert!(c.body.unwrap().contains("deleted"));
        let c = confirm_replace(&status(Link::OtherLink("/opt/kgui".into()), None, false, true));
        assert_eq!(c.details, ["/Users/me/.local/bin/kgui → /opt/kgui"]);
    }
}
