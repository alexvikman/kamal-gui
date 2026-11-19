use crate::{console, theme};
use egui::{RichText, Ui};
use kgui_core::history::{History, Run};
use kgui_core::runner::Status;

pub struct HistoryView {
    project: Option<String>,
    runs: Vec<Run>,
    error: Option<String>,
}

impl HistoryView {
    pub fn new(project: Option<String>) -> Self {
        let mut v = Self { project, runs: Vec::new(), error: None };
        v.reload();
        v
    }

    pub fn reload(&mut self) {
        match History::open().and_then(|h| h.list(self.project.as_deref(), 300)) {
            Ok(runs) => {
                self.runs = runs;
                self.error = None;
            }
            Err(e) => self.error = Some(format!("{e:#}")),
        }
    }

    pub fn on_run_updated(&mut self, run: &Run) {
        if self.project.as_ref().is_some_and(|p| p != &run.project) {
            return;
        }
        match self.runs.iter_mut().find(|r| r.id == run.id) {
            Some(r) => *r = run.clone(),
            None => self.runs.insert(0, run.clone()),
        }
    }

    /// Returns a run the user clicked. `repo_of` gives a project's GitHub
    /// URL, for commit links.
    pub fn ui(&mut self, ui: &mut Ui, repo_of: &dyn Fn(&str) -> Option<String>) -> Option<Run> {
        let p = theme::palette(ui);
        if let Some(e) = &self.error {
            ui.colored_label(p.bad, e);
        }
        if self.runs.is_empty() {
            ui.label(RichText::new("No runs yet.").color(p.muted));
            return None;
        }
        use egui_extras::{Column, TableBuilder};
        const T: f32 = theme::ROW_TEXT;
        const HEADER: f32 = 36.0;
        let text = |s: &str| RichText::new(s).size(T);
        let muted = |s: &str| RichText::new(s).size(T).color(p.muted);
        let mut clicked = None;
        let show_project = self.project.is_none();
        let overlay = console::overlay_space(ui.ctx());
        ui.label(theme::caption("Click a run to see its full output in the terminal."));
        ui.add_space(6.0);
        // Framed like a service block: a header, a divider, striped rows.
        egui::Frame::new().fill(p.card).stroke(egui::Stroke::new(1.0, p.border)).corner_radius(8).inner_margin(1).show(
            ui,
            |ui| {
                let block = ui.max_rect();
                // Rows touch, like a block's rows (their height is all there is).
                ui.spacing_mut().item_spacing.y = 0.0;
                // The table ends above the floating terminal, so its last
                // rows and bottom edge stay in sight.
                let max_height = (ui.available_height() - 40.0 - overlay).max(120.0);
                let mut table = TableBuilder::new(ui)
                    .id_salt(("history", &self.project))
                    .striped(false)
                    // The table scrolls itself and only lays out visible rows.
                    .vscroll(true)
                    .auto_shrink([false, true])
                    .min_scrolled_height(0.0)
                    .max_scroll_height(max_height)
                    .sense(egui::Sense::click())
                    .resizable(false)
                    .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                    .column(Column::exact(132.0));
                // Fixed columns stay small; the commit column takes (and gives up)
                // whatever room is left, so Output always stays visible.
                if show_project {
                    table = table.column(Column::exact(150.0).clip(true));
                }
                table = table
                    .column(Column::exact(72.0).clip(true))
                    .column(Column::exact(150.0).clip(true))
                    .column(Column::remainder().at_least(40.0).clip(true))
                    .column(Column::exact(96.0))
                    .column(Column::exact(64.0))
                    .column(Column::exact(56.0));

                let mut titles = vec!["Started"];
                if show_project {
                    titles.push("Project");
                }
                titles.extend(["Command", "Target", "Commit", "Status", "Duration", ""]);
                table
                    .header(HEADER, |mut h| {
                        for (i, title) in titles.iter().enumerate() {
                            h.col(|ui| {
                                if i == 0 {
                                    ui.add_space(13.0);
                                }
                                ui.label(muted(title));
                            });
                        }
                    })
                    .body(|body| {
                        let runs = &self.runs;
                        body.rows(34.0, runs.len(), |mut row| {
                            let r = &runs[row.index()];
                            let index = row.index();
                            row.col(|ui| {
                                // Every other row is tinted across the whole
                                // block, starting with the second, as in a
                                // service block.
                                if index % 2 == 1 {
                                    let stripe = egui::Rect::from_x_y_ranges(
                                        block.x_range().shrink(1.0),
                                        ui.max_rect().y_range(),
                                    );
                                    // The cell clips to itself; the stripe spans the
                                    // row but stays within the scrolled body.
                                    let clip = egui::Rect::from_x_y_ranges(block.x_range(), ui.clip_rect().y_range());
                                    ui.ctx().layer_painter(ui.layer_id()).with_clip_rect(clip).rect_filled(
                                        stripe,
                                        0.0,
                                        theme::STRIPE,
                                    );
                                }
                                ui.add_space(13.0);
                                ui.label(text(&theme::date_time_short(r.started_at)));
                            });
                            if show_project {
                                row.col(|ui| {
                                    ui.add(egui::Label::new(text(&r.project)).truncate());
                                });
                            }
                            row.col(|ui| {
                                ui.add(egui::Label::new(text(&r.action)).truncate());
                            });
                            row.col(|ui| {
                                ui.add(egui::Label::new(muted(&targets_label(&r.units))).truncate())
                                    .on_hover_text(&r.units);
                            });
                            row.col(|ui| {
                                if let Some(sha) = &r.sha {
                                    let url = repo_of(&r.project).map(|repo| format!("{repo}/commit/{sha}"));
                                    theme::subtle_link(ui, text(&sha[..sha.len().min(7)]), url.as_deref());
                                    ui.add_space(8.0);
                                    ui.add(egui::Label::new(muted(r.subject.as_deref().unwrap_or(""))).truncate());
                                }
                            });
                            row.col(|ui| {
                                let status = Status::from_run(&r.status);
                                let code =
                                    r.exit_code.filter(|c| *c != 0).map(|c| format!(" ({c})")).unwrap_or_default();
                                let label = format!("{}{code}", r.status);
                                match status {
                                    Status::Running => {
                                        ui.add(egui::Spinner::new().size(10.0).color(theme::ACCENT));
                                        ui.label(text(&label).color(theme::ACCENT));
                                    }
                                    Status::Failed | Status::Interrupted => {
                                        theme::dot(ui, p.bad);
                                        ui.label(text(&label).color(p.bad));
                                    }
                                    _ => {
                                        theme::dot(ui, console::status_color(&p, status));
                                        ui.label(muted(&label));
                                    }
                                }
                            });
                            row.col(|ui| {
                                ui.label(muted(&duration(r)));
                            });
                            let mut open = false;
                            row.col(|ui| {
                                open = theme::row_link(ui, "Output").clicked();
                            });
                            if open || row.response().clicked() {
                                clicked = Some(r.clone());
                            }
                        });
                    });
                // The divider under the header spans the block, drawn last so
                // nothing in the table covers it.
                let y = (block.top() + HEADER).round() - 1.0;
                ui.painter().rect_filled(egui::Rect::from_x_y_ranges(block.x_range(), y..=y + 1.0), 0.0, p.border);
            },
        );
        clicked
    }
}

/// `config/deploy.yml@ff, config/deploy.workers.yml@ff` → `ff · web + workers`.
fn targets_label(units: &str) -> String {
    let mut targets: Vec<(String, Vec<&str>)> = Vec::new();
    for unit in units.split(", ").filter(|u| !u.is_empty()) {
        let (file, dest) = unit.split_once('@').map_or((unit, None), |(f, d)| (f, Some(d)));
        let stem = file.trim_start_matches("config/").trim_end_matches(".yml");
        let kind = if stem.split('.').any(|p| p == "workers") { "workers" } else { "web" };
        let target = dest.map(String::from).unwrap_or_else(|| {
            let rest: Vec<&str> = stem.split('.').filter(|p| *p != "deploy" && *p != "workers").collect();
            if rest.is_empty() { "default".into() } else { rest.join(".") }
        });
        match targets.iter_mut().find(|(t, _)| *t == target) {
            Some((_, kinds)) => kinds.push(kind),
            None => targets.push((target, vec![kind])),
        }
    }
    targets.into_iter().map(|(t, k)| format!("{t} · {}", k.join(" + "))).collect::<Vec<_>>().join(", ")
}

fn duration(r: &Run) -> String {
    let Some(end) = r.finished_at else { return String::new() };
    let s = (end - r.started_at) / 1000;
    if s < 60 { format!("{s}s") } else { format!("{}m {}s", s / 60, s % 60) }
}

pub fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n { s.to_string() } else { format!("{}…", s.chars().take(n).collect::<String>()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(id: i64, project: &str, units: &str) -> Run {
        Run {
            id,
            project: project.into(),
            units: units.into(),
            action: "deploy".into(),
            git_ref: Some("origin/master".into()),
            sha: Some("b4064df0000000000000000000000000000000000".into()),
            subject: Some("Commit .kamal/secrets with kgui secret references and a long subject line".into()),
            started_at: 1_794_000_000_000,
            finished_at: Some(1_794_000_100_000),
            status: "ok".into(),
            exit_code: Some(0),
        }
    }

    /// Every column, including the Output link, fits inside the window.
    #[test]
    fn table_fits_the_window() {
        for width in [900.0, 1400.0] {
            let ctx = egui::Context::default();
            crate::theme::install(&ctx);
            let mut view = HistoryView { project: None, runs: Vec::new(), error: None };
            view.runs = (0..20)
                .map(|i| {
                    run(i, "ifg-operator-tool-household-web", "config/deploy.yml@tsi, config/deploy.workers.yml@tsi")
                })
                .collect();
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 700.0))),
                ..Default::default()
            };
            let mut rightmost = 0.0f32;
            let mut found = false;
            for _ in 0..3 {
                let mut out = ctx.run_ui(input.clone(), |ui| {
                    egui::CentralPanel::default().show(ui, |ui| {
                        view.ui(ui, &|_| None);
                    });
                });
                out.textures_delta.clear();
                for cs in &out.shapes {
                    if let egui::Shape::Text(t) = &cs.shape
                        && t.galley.text() == "Output"
                    {
                        found = true;
                        rightmost = rightmost.max(t.visual_bounding_rect().right());
                    }
                }
            }
            assert!(found, "no Output link drawn at width {width}");
            assert!(rightmost <= width, "Output link ends at {rightmost}, past the window ({width})");
        }
    }

    #[test]
    fn readable_targets() {
        assert_eq!(targets_label("config/deploy.yml, config/deploy.workers.yml"), "default · web + workers");
        assert_eq!(targets_label("config/deploy.yml@ff, config/deploy.workers.yml@ff"), "ff · web + workers");
        assert_eq!(targets_label("config/deploy.acme.workers.yml"), "acme · workers");
    }
}
