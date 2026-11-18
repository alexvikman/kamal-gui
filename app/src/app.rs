use crate::console::{Console, Kind, Session};
use crate::dialogs::{self, AddDialog, SettingsDialog};
use crate::history_view::HistoryView;
use crate::project_view::ProjectView;
use crate::theme;
use egui::{RichText, ScrollArea, Ui};
use kgui_core::config::SshTarget;
use kgui_core::history::Run;
use kgui_core::kamal::Action;
use kgui_core::project::{self, Project};
use kgui_core::runner::{self, Event, Runner};
use kgui_core::settings::data_dir;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};

/// Something the user asked for that the app carries out.
pub enum Intent {
    Run { project: String, units: Vec<String>, action: Action, git_ref: Option<String>, label: String },
    Logs { project: String, host: String, ssh: SshTarget, container: String, label: String },
    Cancel(String),
    RemoveProject(String),
}

pub struct Confirm {
    pub title: String,
    pub body: Option<String>,
    pub details: Vec<String>,
    pub button: String,
    pub danger: bool,
    pub intent: Intent,
}

/// Free-text kamal command for one unit.
pub struct Prompt {
    pub title: String,
    pub body: String,
    pub hint: String,
    pub value: String,
    pub project: String,
    pub unit_id: String,
    pub unit_kind: String,
}

pub enum Request {
    Do(Intent),
    Confirm(Confirm),
    Prompt(Prompt),
    OpenRunLog(Run),
    Notice(String),
    /// Bring a console tab to the front.
    FocusSession(String),
}

pub struct KamalApp {
    runner: Runner,
    events: Receiver<Event>,
    projects: Vec<Project>,
    query: String,
    current: Option<String>,
    /// Project views stay alive while switching, so coming back is instant.
    views: std::collections::HashMap<String, ProjectView>,
    all_history: HistoryView,
    console: Console,
    confirm: Option<Confirm>,
    prompt: Option<Prompt>,
    settings: Option<SettingsDialog>,
    add: Option<AddDialog>,
    error: Option<String>,
    /// Asking whether to quit while runs are in progress.
    quit_dialog: bool,
    /// The user chose to stop the runs and quit; close once they've ended.
    quitting: bool,
}

impl KamalApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::install(&cc.egui_ctx);
        let (tx, events) = mpsc::channel();
        let ctx = cc.egui_ctx.clone();
        let runner = Runner::new(tx, Arc::new(move || ctx.request_repaint()));
        let projects = project::list();
        let current = load_selected().filter(|n| projects.iter().any(|p| &p.name == n));
        let views = current.iter().map(|n| (n.clone(), ProjectView::new(&cc.egui_ctx, n))).collect();
        Self {
            runner,
            events,
            projects,
            query: String::new(),
            current,
            views,
            all_history: HistoryView::new(None),
            console: Console::default(),
            confirm: None,
            prompt: None,
            settings: None,
            add: None,
            error: None,
            quit_dialog: false,
            quitting: false,
        }
    }

    fn select(&mut self, ctx: &egui::Context, name: Option<String>) {
        if self.current == name {
            return;
        }
        if let Some(n) = &name {
            match self.views.get_mut(n) {
                Some(view) => view.refresh_if_stale(ctx),
                None => {
                    self.views.insert(n.clone(), ProjectView::new(ctx, n));
                }
            }
        }
        if name.is_none() {
            self.all_history.reload();
        }
        save_selected(name.as_deref());
        self.current = name;
    }

    fn reload_projects(&mut self) {
        self.projects = project::list();
    }

    fn sidebar(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let p = theme::palette(ui);
        ui.add_space(8.0);
        ui.label(theme::title("Kamal GUI", 15.0));
        ui.add_space(4.0);
        ui.add(egui::TextEdit::singleline(&mut self.query).hint_text("Search projects…").desired_width(f32::INFINITY));
        ui.add_space(2.0);
        let all = ui.with_layout(egui::Layout::top_down_justified(egui::Align::LEFT), |ui| {
            ui.selectable_label(self.current.is_none(), "All runs").clicked()
        });
        if all.inner {
            self.select(&ctx, None);
        }
        ui.separator();

        let running = self.console.running_projects();
        let running: Vec<String> = running.into_iter().map(String::from).collect();
        let q = self.query.trim().to_lowercase();
        let mut pick = None;
        let list_height = ui.available_height() - 40.0;
        ScrollArea::vertical().max_height(list_height).auto_shrink([false, true]).show(ui, |ui| {
            ui.with_layout(egui::Layout::top_down_justified(egui::Align::LEFT), |ui| {
                for proj in self.projects.iter().filter(|p| p.name.to_lowercase().contains(&q)) {
                    let selected = self.current.as_deref() == Some(proj.name.as_str());
                    let busy = running.contains(&proj.name);
                    let r = ui.selectable_label(selected, &proj.name);
                    if busy {
                        let rect = egui::Rect::from_center_size(
                            egui::pos2(r.rect.right() - 14.0, r.rect.center().y),
                            egui::vec2(14.0, 14.0),
                        );
                        egui::Spinner::new().color(theme::ACCENT).paint_at(ui, rect);
                    }
                    let hover = if busy { format!("{}\nA run is in progress", proj.url) } else { proj.url.clone() };
                    if r.on_hover_text(hover).clicked() {
                        pick = Some(proj.name.clone());
                    }
                }
            });
            if self.projects.is_empty() {
                ui.label(RichText::new("No projects yet.").color(p.muted));
            }
        });
        if let Some(name) = pick {
            self.select(&ctx, Some(name));
        }
        ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if theme::quiet(ui, "+  Add project").clicked() {
                    self.add = Some(AddDialog::new(&ctx));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if theme::quiet_rich(ui, RichText::new("Settings").color(p.muted)).clicked() {
                        self.settings = Some(SettingsDialog::new());
                    }
                });
            });
            ui.separator();
        });
    }

    fn central(&mut self, ui: &mut Ui, requests: &mut Vec<Request>) {
        let p = theme::palette(ui);
        if let Some(e) = self.error.clone()
            && theme::dismissable(ui, p.bad, p.bad_soft, &e)
        {
            self.error = None;
        }
        if let Some(view) = self.current.as_ref().and_then(|n| self.views.get_mut(n)) {
            let running = self.console.running_in(&view.name).map(|s| crate::project_view::RunningRun {
                key: s.key.clone(),
                label: s.label.clone(),
                units: s.units.clone(),
                started_at: s.started_at,
            });
            view.ui(ui, requests, running.as_ref());
            return;
        }
        if self.projects.is_empty() {
            ui.add_space(ui.available_height() * 0.2);
            ui.vertical_centered(|ui| {
                ui.set_max_width(520.0);
                ui.heading(RichText::new("Add your first project").family(theme::medium()));
                ui.label(
                    RichText::new(
                        "Kamal GUI keeps its own clone of each repository and deploys from there, so you can keep \
                         working in your own checkout while a deploy runs.",
                    )
                    .color(p.muted),
                );
                ui.add_space(8.0);
                if ui.add(theme::primary("Add project")).clicked() {
                    self.add = Some(AddDialog::new(ui.ctx()));
                }
            });
            return;
        }
        ui.heading(theme::title("All runs", 20.0));
        ui.add_space(6.0);
        if let Some(run) = self.all_history.ui(ui) {
            requests.push(Request::OpenRunLog(run));
        }
    }

    fn handle(&mut self, ctx: &egui::Context, request: Request) {
        match request {
            Request::Do(intent) => self.perform(ctx, intent),
            Request::Confirm(c) => self.confirm = Some(c),
            Request::Prompt(p) => self.prompt = Some(p),
            Request::OpenRunLog(run) => self.console.open_run_log(&run),
            Request::Notice(n) => {
                if let Some(v) = self.current.as_ref().and_then(|c| self.views.get_mut(c)) {
                    v.notice = Some(n);
                }
            }
            Request::FocusSession(key) => {
                self.console.focus(&key);
                self.console.collapsed = false;
            }
        }
    }

    fn perform(&mut self, ctx: &egui::Context, intent: Intent) {
        let result = match intent {
            Intent::Run { project, units, action, git_ref, label } => {
                self.runner.start_run(&project, units.clone(), action, git_ref).map(|run| {
                    let title = format!("#{} {label}", run.id);
                    let mut session = Session::new(runner::run_key(run.id), title, Kind::Run, Some(project));
                    session.units = units;
                    session.label = label;
                    self.console.open(session);
                    self.all_history.on_run_updated(&run);
                    for v in self.views.values_mut() {
                        v.on_run_updated(ctx, &run);
                    }
                })
            }
            Intent::Logs { project, host, ssh, container, label } => self
                .runner
                .start_logs(&host, &ssh, &container, 500)
                .map(|key| self.console.open(Session::new(key, label, Kind::Log, Some(project)))),
            Intent::Cancel(key) => {
                self.runner.cancel(&key);
                Ok(())
            }
            Intent::RemoveProject(name) => project::remove(&name).map(|()| {
                self.views.remove(&name);
                self.reload_projects();
                self.select(ctx, None);
            }),
        };
        if let Err(e) = result {
            self.error = Some(format!("{e:#}"));
        }
    }

    /// Closing the window or pressing Cmd+Q/Cmd+W while a kamal run is in
    /// progress asks first: quitting would cut the deploy off halfway.
    fn guard_quit(&mut self, ctx: &egui::Context) {
        let runs_active = !self.console.running_projects().is_empty();
        let shortcut = ctx.input_mut(|i| {
            i.consume_key(egui::Modifiers::COMMAND, egui::Key::Q) || i.consume_key(egui::Modifiers::COMMAND, egui::Key::W)
        });
        let close_requested = ctx.input(|i| i.viewport().close_requested());

        if self.quitting {
            if !runs_active {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            } else if close_requested {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            }
            return;
        }
        if shortcut || close_requested {
            if runs_active {
                if close_requested {
                    ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                }
                self.quit_dialog = true;
            } else if shortcut {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        if self.quit_dialog || self.quitting {
            let runs: Vec<String> = self
                .console
                .sessions
                .iter()
                .filter(|s| s.is_live_run())
                .map(|s| format!("{} · {}", s.project.as_deref().unwrap_or(""), s.label))
                .collect();
            match dialogs::quit(ctx, &runs, self.quitting) {
                Some(dialogs::QuitChoice::Stay) => self.quit_dialog = false,
                Some(dialogs::QuitChoice::StopAndQuit) => {
                    let keys: Vec<String> =
                        self.console.sessions.iter().filter(|s| s.is_live_run()).map(|s| s.key.clone()).collect();
                    for k in keys {
                        self.runner.cancel(&k);
                    }
                    self.quitting = true;
                }
                None => {}
            }
            return;
        }
        if let Some(c) = &self.confirm
            && let Some(ok) = dialogs::confirm(ctx, c)
        {
            let c = self.confirm.take().unwrap();
            if ok {
                self.perform(ctx, c.intent);
            }
        }
        if let Some(p) = &mut self.prompt
            && let Some(answer) = dialogs::prompt(ctx, p)
        {
            let p = self.prompt.take().unwrap();
            if let Some(text) = answer {
                let args = dialogs::split_args(text.trim().trim_start_matches("kamal "));
                if !args.is_empty() {
                    let label = format!("{} {}", p.unit_kind, args.join(" "));
                    self.perform(
                        ctx,
                        Intent::Run { project: p.project, units: vec![p.unit_id], action: Action::Custom { args }, git_ref: None, label },
                    );
                }
            }
        }
        if let Some(s) = &mut self.settings
            && s.ui(ctx)
        {
            self.settings = None;
        }
        if let Some(a) = &mut self.add
            && let Some(added) = a.ui(ctx, &self.projects)
        {
            self.add = None;
            self.reload_projects();
            if let Some(first) = added.into_iter().next() {
                self.select(ctx, Some(first));
            }
        }
    }
}

impl eframe::App for KamalApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        while let Ok(event) = self.events.try_recv() {
            if let Some(run) = self.console.handle(event) {
                self.all_history.on_run_updated(&run);
                for v in self.views.values_mut() {
                    v.on_run_updated(&ctx, &run);
                }
            }
        }

        if self.console.any_running() {
            // Keep elapsed times and spinners moving.
            ctx.request_repaint_after(std::time::Duration::from_millis(500));
        }
        self.guard_quit(&ctx);

        let mut requests = Vec::new();
        let scope = self.current.clone();
        egui::Panel::left("sidebar").resizable(false).exact_size(240.0).show(ui, |ui| self.sidebar(ui));
        // The terminal only appears once this project has something to show.
        if self.console.has_tabs(scope.as_deref()) {
            if self.console.collapsed {
                egui::Panel::bottom("console-collapsed")
                    .exact_size(36.0)
                    .frame(egui::Frame::new().fill(crate::console::BG))
                    .show(ui, |ui| self.console.ui(ui, &self.runner, scope.as_deref()));
            } else {
                egui::Panel::bottom("console")
                    .resizable(true)
                    .default_size(340.0)
                    .frame(egui::Frame::new().fill(crate::console::BG))
                    .show(ui, |ui| self.console.ui(ui, &self.runner, scope.as_deref()));
            }
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::central_panel(ui.style()).inner_margin(egui::Margin::symmetric(24, 16)))
            .show(ui, |ui| self.central(ui, &mut requests));

        for r in requests {
            self.handle(&ctx, r);
        }
        self.dialogs(&ctx);
    }
}

fn ui_state_path() -> std::path::PathBuf {
    data_dir().join("ui.json")
}

fn load_selected() -> Option<String> {
    let text = std::fs::read_to_string(ui_state_path()).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    v.get("project")?.as_str().map(String::from)
}

fn save_selected(name: Option<&str>) {
    let _ = std::fs::create_dir_all(data_dir());
    let _ = std::fs::write(ui_state_path(), serde_json::json!({ "project": name }).to_string());
}
