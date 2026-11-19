use crate::console::{Console, Kind, Session};
use crate::dialogs::{self, AddDialog};
use crate::history_view::HistoryView;
use crate::project_view::ProjectView;
use crate::settings_page::{PageResult, SettingsPage, SettingsTab};
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
    Run {
        project: String,
        units: Vec<String>,
        action: Action,
        git_ref: Option<String>,
        label: String,
    },
    Logs {
        project: String,
        host: String,
        ssh: SshTarget,
        container: String,
        label: String,
    },
    /// An interactive kamal command (exec -i, an alias) in a terminal tab.
    Terminal {
        project: String,
        unit_id: String,
        args: Vec<String>,
        title: String,
    },
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
    /// The Settings page, when it replaces the main area.
    settings: Option<SettingsPage>,
    add: Option<AddDialog>,
    error: Option<String>,
    /// Asking whether to quit while runs are in progress.
    quit_dialog: bool,
    /// The user chose to stop the runs and quit; close once they've ended.
    quitting: bool,
    menu: crate::menu::AppMenu,
    /// Background check for a newer nightly (on start and every few hours).
    update_check: Option<crate::task::Task<crate::update::Release>>,
    last_update_check: Option<std::time::Instant>,
    update_available: Option<crate::update::Release>,
    /// How the sidebar orders projects, and when each was last deployed.
    sort: ProjectSort,
    last_deploy: std::collections::HashMap<String, i64>,
}

#[derive(Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ProjectSort {
    #[default]
    Name,
    LastDeploy,
}

/// What the app remembers between launches (`ui.json`).
#[derive(serde::Serialize, serde::Deserialize, Default)]
struct UiState {
    project: Option<String>,
    #[serde(default)]
    sort: ProjectSort,
}

impl KamalApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::install(&cc.egui_ctx);
        let (tx, events) = mpsc::channel();
        let ctx = cc.egui_ctx.clone();
        let runner = Runner::new(tx, Arc::new(move || ctx.request_repaint()));
        let projects = project::list();
        let state = load_ui_state();
        let current = state.project.filter(|n| projects.iter().any(|p| &p.name == n));
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
            // `--settings` / `--about` open straight on that page
            // (e.g. `open -a "Kamal GUI" --args --about`).
            settings: std::env::args().find_map(|a| match a.as_str() {
                "--settings" => Some(SettingsPage::new(SettingsTab::General)),
                "--about" => Some(SettingsPage::new(SettingsTab::About)),
                _ => None,
            }),
            add: None,
            error: None,
            quit_dialog: false,
            menu: crate::menu::install(&cc.egui_ctx),
            update_check: None,
            last_update_check: None,
            update_available: None,
            quitting: false,
            sort: state.sort,
            last_deploy: kgui_core::history::History::open().and_then(|h| h.last_deploys()).unwrap_or_default(),
        }
    }

    fn open_settings(&mut self, tab: SettingsTab) {
        match &mut self.settings {
            Some(page) => page.tab = tab,
            None => self.settings = Some(SettingsPage::new(tab)),
        }
    }

    fn select(&mut self, ctx: &egui::Context, name: Option<String>) {
        // Picking a project or "All runs" leaves the Settings page.
        self.settings = None;
        if self.current == name {
            return;
        }
        if let Some(n) = &name {
            match self.views.get_mut(n) {
                Some(view) => view.on_shown(),
                None => {
                    self.views.insert(n.clone(), ProjectView::new(ctx, n));
                }
            }
        }
        if name.is_none() {
            self.all_history.reload();
        }
        self.current = name;
        self.save_ui_state();
    }

    fn save_ui_state(&self) {
        save_ui_state(&UiState { project: self.current.clone(), sort: self.sort });
    }

    /// Projects in sidebar order: by name, or most recently deployed first
    /// (never-deployed ones last, by name).
    fn sorted_projects(&self) -> Vec<&Project> {
        let mut list: Vec<&Project> = self.projects.iter().collect();
        list.sort_by_key(|p| p.name.to_lowercase());
        if self.sort == ProjectSort::LastDeploy {
            list.sort_by_key(|p| std::cmp::Reverse(self.last_deploy.get(&p.name).copied()));
        }
        list
    }

    fn reload_projects(&mut self) {
        self.projects = project::list();
    }

    fn sidebar(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let p = theme::palette(ui);
        // The window title already says "Kamal GUI"; start with search.
        ui.add_space(10.0);
        ui.add(egui::TextEdit::singleline(&mut self.query).hint_text("Search projects…").desired_width(f32::INFINITY));
        ui.add_space(2.0);
        let all = ui.with_layout(egui::Layout::top_down_justified(egui::Align::LEFT), |ui| {
            ui.selectable_label(self.settings.is_none() && self.current.is_none(), "All runs").clicked()
        });
        if all.inner {
            self.select(&ctx, None);
        }
        ui.separator();
        ui.horizontal(|ui| {
            // Line up with the project names' text.
            ui.add_space(2.0);
            ui.label(RichText::new("Projects").size(12.5).color(p.muted));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let mut sort = self.sort;
                theme::sort_menu(ui, |ui| {
                    ui.label(theme::caption("Sort by"));
                    for (value, label) in [(ProjectSort::Name, "Name"), (ProjectSort::LastDeploy, "Last deploy")] {
                        if theme::check_item(ui, label, sort == value).clicked() {
                            sort = value;
                            ui.close();
                        }
                    }
                });
                if sort != self.sort {
                    self.sort = sort;
                    self.save_ui_state();
                }
            });
        });

        let running = self.console.running_projects();
        let running: Vec<String> = running.into_iter().map(String::from).collect();
        let q = self.query.trim().to_lowercase();
        let mut pick = None;
        // The list ends where the footer starts; its height is measured each
        // frame (it grows when an update is available).
        let footer_id = egui::Id::unique("sidebar-footer-height");
        let footer = ctx.data(|d| d.get_temp::<f32>(footer_id)).unwrap_or(48.0);
        let list_height = (ui.available_height() - footer - 6.0).max(0.0);
        ScrollArea::vertical().max_height(list_height).auto_shrink([false, true]).show(ui, |ui| {
            ui.with_layout(egui::Layout::top_down_justified(egui::Align::LEFT), |ui| {
                for proj in self.sorted_projects().into_iter().filter(|p| p.name.to_lowercase().contains(&q)) {
                    let selected = self.settings.is_none() && self.current.as_deref() == Some(proj.name.as_str());
                    let busy = running.contains(&proj.name);
                    let deployed = self.last_deploy.get(&proj.name).copied();
                    let age = deployed.filter(|_| self.sort == ProjectSort::LastDeploy).map(theme::ago_short);
                    let r = project_row(ui, &proj.name, selected, busy, age.as_deref(), p.muted);
                    let mut hover = proj.url.clone();
                    if let Some(t) = deployed {
                        hover += &format!("\nDeployed {} ({})", theme::ago(t), theme::date_time_short(t));
                    }
                    if busy {
                        let queued = self.console.running_in(&proj.name).is_some_and(|s| s.queued);
                        hover += if queued { "\nA deploy is queued" } else { "\nA run is in progress" };
                    }
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
        let footer = ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
            ui.add_space(8.0);
            if let Some(r) = self.update_available.clone().filter(|r| r.is_newer()) {
                ui.horizontal(|ui| {
                    let label = RichText::new("Update available").color(theme::ACCENT);
                    if theme::quiet_rich(ui, label).on_hover_text(format!("{} is out", r.title)).clicked() {
                        self.open_settings(SettingsTab::About);
                    }
                });
            }
            ui.horizontal(|ui| {
                if theme::quiet(ui, "+  Add project").clicked() {
                    self.add = Some(AddDialog::new(&ctx));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let color = if self.settings.is_some() { theme::TEXT } else { p.muted };
                    if theme::quiet_rich(ui, RichText::new("Settings").color(color)).clicked() {
                        self.open_settings(SettingsTab::General);
                    }
                });
            });
            ui.separator();
            ui.min_rect().height()
        });
        if (footer.inner - ctx.data(|d| d.get_temp::<f32>(footer_id)).unwrap_or(0.0)).abs() > 0.5 {
            ctx.data_mut(|d| d.insert_temp(footer_id, footer.inner));
            ctx.request_repaint();
        }
    }

    fn central(&mut self, ui: &mut Ui, requests: &mut Vec<Request>) {
        let p = theme::palette(ui);
        if let Some(e) = self.error.clone()
            && theme::dismissable(ui, p.bad, p.bad_soft, &e)
        {
            self.error = None;
        }
        if let Some(page) = &mut self.settings {
            let runs_active = !self.console.running_projects().is_empty();
            let result = page.ui(ui, self.update_available.clone(), runs_active);
            if let Some(r) = page.latest_release() {
                self.update_available = Some(r.clone());
            }
            if let PageResult::Relaunch(bundle) = result {
                match crate::update::relaunch(&bundle) {
                    Ok(()) => ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close),
                    Err(e) => self.error = Some(format!("Update installed, but {e:#}. Restart the app manually.")),
                }
            }
            return;
        }
        if let Some(view) = self.current.as_ref().and_then(|n| self.views.get_mut(n)) {
            let running = self.console.running_in(&view.name).map(|s| crate::project_view::RunningRun {
                key: s.key.clone(),
                label: s.label.clone(),
                units: s.units.clone(),
                started_at: s.started_at,
                queued: s.queued,
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
        let projects = &self.projects;
        let repo_of = |name: &str| projects.iter().find(|p| p.name == name).and_then(|p| theme::github_web(&p.url));
        if let Some(run) = self.all_history.ui(ui, &repo_of) {
            requests.push(Request::OpenRunLog(run));
        }
    }

    fn handle(&mut self, ctx: &egui::Context, request: Request) {
        match request {
            Request::Do(intent) => self.perform(ctx, intent),
            Request::Confirm(c) => self.confirm = Some(c),
            Request::Prompt(p) => self.prompt = Some(p),
            Request::OpenRunLog(run) => self.console.open_run_log(&run),
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
            Intent::Terminal { project, unit_id, args, title } => {
                self.console.open_terminal(ctx, &self.runner, &project, &unit_id, args, title);
                Ok(())
            }
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
    fn guard_quit(&mut self, ctx: &egui::Context, menu_quit: bool) {
        let runs_active = !self.console.running_projects().is_empty();
        let shortcut = menu_quit
            || ctx.input_mut(|i| {
                i.consume_key(egui::Modifiers::COMMAND, egui::Key::Q)
                    || i.consume_key(egui::Modifiers::COMMAND, egui::Key::W)
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

    /// Handle clicks in the macOS menu bar. Returns true for Quit/Close.
    fn menu_actions(&mut self, ctx: &egui::Context) -> bool {
        use crate::menu::MenuAction;
        let mut quit = false;
        while let Ok(action) = self.menu.actions.try_recv() {
            match action {
                MenuAction::About | MenuAction::CheckForUpdates => self.open_settings(SettingsTab::About),
                MenuAction::Settings => self.open_settings(SettingsTab::General),
                MenuAction::AddProject => self.add = Some(AddDialog::new(ctx)),
                MenuAction::CloseWindow | MenuAction::Quit => quit = true,
                MenuAction::OpenGithub => ctx.open_url(egui::OpenUrl::new_tab(crate::build_info::repo_url())),
                MenuAction::OpenKamalDocs => ctx.open_url(egui::OpenUrl::new_tab("https://kamal-deploy.org/docs/")),
            }
        }
        quit
    }

    /// Look for a newer nightly when the app starts and every six hours.
    fn poll_updates(&mut self, ctx: &egui::Context) {
        const EVERY: std::time::Duration = std::time::Duration::from_secs(6 * 60 * 60);
        // Offline or rate-limited: try again next round, quietly.
        if let Some(Ok(release)) = crate::task::poll(&mut self.update_check) {
            self.update_available = Some(release);
        }
        if self.update_check.is_none() && self.last_update_check.is_none_or(|t| t.elapsed() > EVERY) {
            self.last_update_check = Some(std::time::Instant::now());
            self.update_check = Some(crate::task::Task::spawn(ctx, crate::update::check));
        }
        ctx.request_repaint_after(EVERY);
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
                    let intent = if args.iter().any(|a| a == "-i" || a == "--interactive") {
                        Intent::Terminal { project: p.project, unit_id: p.unit_id, args, title: label }
                    } else {
                        Intent::Run {
                            project: p.project,
                            units: vec![p.unit_id],
                            action: Action::Custom { args },
                            git_ref: None,
                            label,
                        }
                    };
                    self.perform(ctx, intent);
                }
            }
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
                if run.action == "deploy"
                    && run.status == "ok"
                    && let Some(t) = run.finished_at
                {
                    note_deploy(&mut self.last_deploy, &run.project, t);
                }
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
        for v in self.views.values_mut() {
            v.poll(&ctx);
        }
        let menu_quit = self.menu_actions(&ctx);
        self.poll_updates(&ctx);
        self.guard_quit(&ctx, menu_quit);

        let mut requests = Vec::new();
        let scope = self.current.clone();
        egui::Panel::left("sidebar").resizable(false).exact_size(240.0).show(ui, |ui| self.sidebar(ui));
        let margin = egui::Margin::symmetric(24, 16);
        let content = egui::CentralPanel::default()
            .frame(egui::Frame::central_panel(ui.style()).inner_margin(margin))
            .show(ui, |ui| self.central(ui, &mut requests))
            .response
            .rect
            - margin;
        // The terminal floats over the main area once this project has
        // something to show.
        self.console.overlay(&ctx, content, &self.runner, scope.as_deref());

        for r in requests {
            self.handle(&ctx, r);
        }
        // Status checks see deploys made outside the app too.
        for v in self.views.values() {
            if let Some(t) = v.last_deploy() {
                note_deploy(&mut self.last_deploy, &v.name, t);
            }
        }
        self.dialogs(&ctx);
    }
}

fn ui_state_path() -> std::path::PathBuf {
    data_dir().join("ui.json")
}

fn load_ui_state() -> UiState {
    std::fs::read_to_string(ui_state_path()).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
}

fn save_ui_state(state: &UiState) {
    if let Ok(text) = serde_json::to_string(state) {
        let _ = kgui_core::settings::write_private(&ui_state_path(), text.as_bytes());
    }
}

/// A project in the sidebar: its name, wrapped to leave room on the right
/// for a spinner (a run is in progress) or how long ago it was deployed.
fn project_row(
    ui: &mut Ui,
    name: &str,
    selected: bool,
    busy: bool,
    age: Option<&str>,
    muted: egui::Color32,
) -> egui::Response {
    let pad = egui::vec2(11.0, 5.0);
    let width = ui.available_width();
    let painter = ui.painter().clone();
    let age =
        age.filter(|_| !busy).map(|a| painter.layout_no_wrap(a.to_string(), egui::FontId::proportional(12.0), muted));
    let right = if busy { 22.0 } else { age.as_ref().map_or(0.0, |g| g.size().x + 10.0) };
    let text = painter.layout(
        name.to_string(),
        egui::TextStyle::Body.resolve(ui.style()),
        theme::TEXT,
        (width - 2.0 * pad.x - right).max(40.0),
    );
    let height = (text.size().y + 2.0 * pad.y).max(ui.spacing().interact_size.y);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::click());
    let visuals = ui.visuals();
    let fill = if selected {
        Some(visuals.selection.bg_fill)
    } else if resp.hovered() {
        Some(visuals.widgets.hovered.weak_bg_fill)
    } else {
        None
    };
    if let Some(fill) = fill {
        painter.rect_filled(rect, 7, fill);
    }
    let first_line = rect.top() + pad.y + text.rows.first().map_or(text.size().y, |r| r.height()) / 2.0;
    painter.galley(rect.min + pad, text, theme::TEXT);
    if busy {
        let spinner = egui::Rect::from_center_size(egui::pos2(rect.right() - 14.0, first_line), egui::vec2(14.0, 14.0));
        egui::Spinner::new().color(theme::ACCENT).paint_at(ui, spinner);
    } else if let Some(age) = age {
        let pos = egui::pos2(rect.right() - pad.x + 3.0 - age.size().x, first_line - age.size().y / 2.0);
        painter.galley(pos, age, muted);
    }
    resp
}

fn note_deploy(map: &mut std::collections::HashMap<String, i64>, project: &str, at: i64) {
    let t = map.entry(project.to_string()).or_insert(at);
    *t = (*t).max(at);
}
