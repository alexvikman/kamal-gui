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

    /// Returns a run the user clicked.
    pub fn ui(&mut self, ui: &mut Ui) -> Option<Run> {
        let p = theme::palette(ui);
        if let Some(e) = &self.error {
            ui.colored_label(p.bad, e);
        }
        if self.runs.is_empty() {
            ui.label(RichText::new("No runs yet.").color(p.muted));
            return None;
        }
        use egui_extras::{Column, TableBuilder};
        let mut clicked = None;
        let show_project = self.project.is_none();
        ui.label(theme::caption("Click a run to see its full output in the terminal below."));
        ui.add_space(6.0);
        theme::card(ui, |ui| {
            let max_height = (ui.available_height() - 40.0).max(120.0);
            let mut table = TableBuilder::new(ui)
                .id_salt(("history", &self.project))
                .striped(true)
                // The table scrolls itself and only lays out visible rows.
                .vscroll(true)
                .auto_shrink([false, true])
                .min_scrolled_height(0.0)
                .max_scroll_height(max_height)
                .sense(egui::Sense::click())
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .column(Column::exact(150.0))
                .column(Column::exact(if show_project { 150.0 } else { 0.0 }))
                .column(Column::auto().at_least(110.0))
                .column(Column::auto().at_least(160.0))
                .column(Column::remainder().at_least(200.0).clip(true))
                .column(Column::exact(90.0))
                .column(Column::exact(80.0))
                .column(Column::exact(60.0));
            table = table.resizable(false);
            table
                .header(26.0, |mut h| {
                    for title in [
                        "Started",
                        if show_project { "Project" } else { "" },
                        "Command",
                        "Units",
                        "Commit",
                        "Status",
                        "Duration",
                        "",
                    ] {
                        h.col(|ui| {
                            ui.label(RichText::new(title).family(theme::medium()).color(p.muted));
                        });
                    }
                })
                .body(|body| {
                    let runs = &self.runs;
                    body.rows(30.0, runs.len(), |mut row| {
                        let r = &runs[row.index()];
                        {
                            row.col(|ui| {
                                ui.label(theme::date_time(r.started_at));
                            });
                            row.col(|ui| {
                                if show_project {
                                    ui.label(&r.project);
                                }
                            });
                            row.col(|ui| {
                                ui.label(RichText::new(&r.action).monospace());
                            });
                            row.col(|ui| {
                                let units: Vec<String> = r
                                    .units
                                    .split(", ")
                                    .map(|u| u.trim_start_matches("config/").replace(".yml", ""))
                                    .collect();
                                ui.label(theme::caption(units.join(", ")));
                            });
                            row.col(|ui| {
                                if let Some(sha) = &r.sha {
                                    ui.label(RichText::new(&sha[..sha.len().min(7)]).monospace());
                                    ui.add(
                                        egui::Label::new(theme::caption(r.subject.as_deref().unwrap_or(""))).truncate(),
                                    );
                                }
                            });
                            row.col(|ui| {
                                let status = Status::from_run(&r.status);
                                let code =
                                    r.exit_code.filter(|c| *c != 0).map(|c| format!(" ({c})")).unwrap_or_default();
                                let color = console::status_color(&p, status);
                                ui.label(RichText::new(format!("{}{code}", r.status)).color(color));
                            });
                            row.col(|ui| {
                                ui.label(theme::caption(duration(r)));
                            });
                            let mut open = false;
                            row.col(|ui| {
                                open = theme::link(ui, "Output").clicked();
                            });
                            if open || row.response().clicked() {
                                clicked = Some(r.clone());
                            }
                        }
                    });
                });
        });
        clicked
    }
}

fn duration(r: &Run) -> String {
    let Some(end) = r.finished_at else { return String::new() };
    let s = (end - r.started_at) / 1000;
    if s < 60 { format!("{s}s") } else { format!("{}m {}s", s / 60, s % 60) }
}

pub fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n { s.to_string() } else { format!("{}…", s.chars().take(n).collect::<String>()) }
}
