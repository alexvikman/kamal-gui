//! About window: version, build and updates.

use crate::build_info;
use crate::task::{self, Task};
use crate::theme;
use crate::update::{self, Release};
use egui::{Id, Modal, RichText, Ui};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub enum AboutResult {
    Open,
    Close,
    /// The update is installed; relaunch from this bundle.
    Relaunch(PathBuf),
}

pub struct About {
    icon: Option<egui::TextureHandle>,
    check: Option<Task<Release>>,
    release: Option<Release>,
    install: Option<Task<PathBuf>>,
    step: Arc<Mutex<String>>,
    error: Option<String>,
}

impl About {
    /// Opens the window and checks for a newer nightly right away.
    pub fn new(ctx: &egui::Context, known: Option<Release>) -> Self {
        let icon = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png")).ok().map(|icon| {
            let image =
                egui::ColorImage::from_rgba_unmultiplied([icon.width as usize, icon.height as usize], &icon.rgba);
            ctx.load_texture("about-icon", image, egui::TextureOptions::LINEAR)
        });
        let mut about = Self { icon, check: None, release: known, install: None, step: Arc::default(), error: None };
        about.start_check(ctx);
        about
    }

    fn start_check(&mut self, ctx: &egui::Context) {
        self.error = None;
        self.check = Some(Task::spawn(ctx, update::check));
    }

    /// The newest release seen by this window (for the app's own notice).
    pub fn latest(&self) -> Option<&Release> {
        self.release.as_ref()
    }

    pub fn ui(&mut self, ctx: &egui::Context, runs_active: bool) -> AboutResult {
        match task::poll(&mut self.check) {
            Some(Ok(r)) => self.release = Some(r),
            Some(Err(e)) => self.error = Some(format!("Couldn't check for updates: {e}")),
            None => {}
        }
        match task::poll(&mut self.install) {
            Some(Ok(bundle)) => return AboutResult::Relaunch(bundle),
            Some(Err(e)) => self.error = Some(format!("Update failed: {e}")),
            None => {}
        }

        let mut close = false;
        let busy = self.install.is_some();
        let resp = Modal::new(Id::unique("about")).show(ctx, |ui| {
            ui.set_width(460.0);
            let p = theme::palette(ui);
            ui.horizontal(|ui| {
                if let Some(icon) = &self.icon {
                    ui.add(egui::Image::new(icon).fit_to_exact_size(egui::vec2(64.0, 64.0)));
                }
                ui.add_space(6.0);
                ui.vertical(|ui| {
                    ui.label(theme::title("Kamal GUI", 20.0));
                    ui.label(theme::caption(format!("Version {}", build_info::VERSION)));
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        ui.label(theme::caption("Build"));
                        let commit = format!("{}/commit/{}", build_info::repo_url(), build_info::SHA);
                        ui.hyperlink_to(
                            RichText::new(build_info::short_sha()).monospace().size(12.0).color(theme::ACCENT),
                            commit,
                        );
                        ui.label(theme::caption(format!("· {}", build_info::DATE)));
                    });
                    ui.label(theme::caption("Deploy, watch and debug Kamal projects."));
                });
            });

            ui.add_space(8.0);
            ui.separator();
            ui.label(RichText::new("Updates").family(theme::medium()));
            self.updates(ui, &p, runs_active);
            ui.separator();

            ui.horizontal(|ui| {
                ui.hyperlink_to(RichText::new("GitHub").color(theme::ACCENT), build_info::repo_url());
                ui.label(theme::caption("·"));
                ui.hyperlink_to(
                    RichText::new("Releases").color(theme::ACCENT),
                    format!("{}/releases", build_info::repo_url()),
                );
                ui.label(theme::caption("·"));
                ui.hyperlink_to(
                    RichText::new("MIT License").color(theme::ACCENT),
                    format!("{}/blob/main/LICENSE", build_info::repo_url()),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add_enabled(!busy, egui::Button::new("Close")).clicked() {
                        close = true;
                    }
                });
            });
        });
        if close || (resp.should_close() && !busy) { AboutResult::Close } else { AboutResult::Open }
    }

    fn updates(&mut self, ui: &mut Ui, p: &theme::Palette, runs_active: bool) {
        let ctx = ui.ctx().clone();
        if self.install.is_some() {
            let step = self.step.lock().unwrap().clone();
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(if step.is_empty() { "Starting…".to_string() } else { step });
            });
            return;
        }
        if let Some(e) = &self.error {
            ui.add(egui::Label::new(RichText::new(e).color(p.bad)).wrap());
        }
        if self.check.is_some() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(theme::caption("Checking for updates…"));
            });
            return;
        }
        let Some(release) = self.release.clone() else {
            if theme::quiet(ui, "Check for updates").clicked() {
                self.start_check(&ctx);
            }
            return;
        };

        if !release.is_newer() {
            ui.label(RichText::new("You're on the latest nightly.").color(p.ok));
            if theme::quiet_rich(ui, RichText::new("Check again").color(p.muted)).clicked() {
                self.start_check(&ctx);
            }
            return;
        }

        ui.label(format!("{} is available.", release.title));
        ui.horizontal(|ui| {
            let date = release.published_at.get(..10).unwrap_or(&release.published_at);
            ui.label(theme::caption(format!("Published {date} · commit {}", release.short_sha())));
            ui.hyperlink_to(RichText::new("What's changed").size(12.5).color(theme::ACCENT), release.compare_url());
        });
        ui.add_space(4.0);
        if update::current_bundle().is_none() {
            ui.label(theme::caption(
                "This is a development build. Download the app from the releases page to get automatic updates.",
            ));
            ui.hyperlink_to(RichText::new("Open releases").color(theme::ACCENT), &release.url);
            return;
        }
        let button = ui
            .add_enabled(!runs_active, theme::primary("Update and relaunch"))
            .on_disabled_hover_text("Wait for the running deploy to finish");
        if button.clicked() {
            self.error = None;
            let step = self.step.clone();
            let ctx2 = ctx.clone();
            self.install = Some(Task::spawn(&ctx, move || {
                update::install(&release, &|s| {
                    *step.lock().unwrap() = s.to_string();
                    ctx2.request_repaint();
                })
            }));
        }
        ui.label(theme::caption(
            "Downloads the nightly, verifies its checksum and that it was built by this repo's CI (needs `gh` signed in), replaces this app and restarts it.",
        ));
    }
}
