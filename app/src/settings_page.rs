//! Settings as a page in the main area, with General and About tabs.

use crate::about::About;
use crate::theme;
use crate::update::Release;
use egui::{RichText, ScrollArea, Ui};
use kgui_core::kamal;
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
        Self { tab, general: GeneralForm::load(), about: None }
    }

    /// Newest release the About tab has seen, for the sidebar notice.
    pub fn latest_release(&self) -> Option<&Release> {
        self.about.as_ref().and_then(|a| a.latest())
    }

    pub fn ui(&mut self, ui: &mut Ui, known_release: Option<Release>, runs_active: bool) -> PageResult {
        ui.label(theme::title("Settings", 24.0));
        ui.add_space(14.0);
        theme::tabs(ui, &mut self.tab, &[(SettingsTab::General, "General"), (SettingsTab::About, "About")]);
        ui.add_space(18.0);

        let mut result = PageResult::None;
        ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            ui.set_max_width(680.0);
            match self.tab {
                SettingsTab::General => self.general.ui(ui),
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

    fn ui(&mut self, ui: &mut Ui) {
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
