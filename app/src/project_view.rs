use crate::app::{Confirm, Intent, Prompt, Request};
use crate::history_view::{HistoryView, truncate};
use crate::task::{self, Task};
use crate::theme::{self, Palette};
use egui::{Align, Layout, RichText, ScrollArea, Ui, vec2};
use kgui_core::config::UnitConfig;
use kgui_core::git::{self, RefList};
use kgui_core::history::Run;
use kgui_core::images;
use kgui_core::kamal::{AccessoryVerb, Action};
use kgui_core::project::{self, ProjectDetail, Target, Unit};
use kgui_core::status::{self, Container, ProjectStatus};
use std::collections::{BTreeSet, HashMap};

#[derive(PartialEq, Eq, Clone, Copy)]
enum Tab {
    Overview,
    History,
}

/// A kamal run in progress in this project (from the console).
pub struct RunningRun {
    pub key: String,
    pub label: String,
    pub units: Vec<String>,
    pub started_at: i64,
}

fn elapsed(started_at: i64) -> String {
    let s = ((chrono::Utc::now().timestamp_millis() - started_at) / 1000).max(0);
    if s < 60 { format!("{s}s") } else { format!("{}m {:02}s", s / 60, s % 60) }
}

/// Width of the service-name column in unit rows.
const NAME_COL: f32 = 250.0;

pub struct ProjectView {
    pub name: String,
    detail: Option<ProjectDetail>,
    refs: Option<RefList>,
    status: Option<ProjectStatus>,
    load: Option<Task<ProjectDetail>>,
    fetch: Option<Task<ProjectDetail>>,
    refs_task: Option<Task<RefList>>,
    status_task: Option<Task<ProjectStatus>>,
    fetched_at: Option<i64>,
    error: Option<String>,
    /// Selected ref per target id.
    selected: HashMap<String, String>,
    tab: Tab,
    history: HistoryView,
}

impl ProjectView {
    pub fn new(ctx: &egui::Context, name: &str) -> Self {
        let mut v = Self {
            name: name.to_string(),
            detail: None,
            refs: None,
            status: None,
            load: None,
            fetch: None,
            refs_task: None,
            status_task: None,
            fetched_at: None,
            error: None,
            selected: HashMap::new(),
            tab: Tab::Overview,
            history: HistoryView::new(Some(name.to_string())),
        };
        // Show what the local clone has right away, then refresh from origin.
        let n = v.name.clone();
        v.load = Some(Task::spawn(ctx, move || project::load_detail(project::find(&n)?)));
        v.load_refs(ctx);
        // GitHub first; the server check starts when the fetch is done (see
        // `poll`). The SSH agent then asks for its keys in a fixed order,
        // one at a time.
        v.fetch_origin(ctx);
        v
    }

    fn load_refs(&mut self, ctx: &egui::Context) {
        let n = self.name.clone();
        self.refs_task = Some(Task::spawn(ctx, move || git::ref_list(&n)));
    }

    fn fetch_origin(&mut self, ctx: &egui::Context) {
        let n = self.name.clone();
        self.fetch = Some(Task::spawn(ctx, move || {
            project::fetch(&n)?;
            project::load_detail(project::find(&n)?)
        }));
    }

    fn refresh_status(&mut self, ctx: &egui::Context) {
        let n = self.name.clone();
        self.status_task = Some(Task::spawn(ctx, move || {
            let detail = project::load_detail(project::find(&n)?)?;
            Ok(status::project_status(&detail))
        }));
    }

    /// Coming back to an open project: refresh in the background when the
    /// data is more than a minute old, keep showing what's there meanwhile.
    pub fn refresh_if_stale(&mut self, ctx: &egui::Context) {
        let now = chrono::Utc::now().timestamp_millis();
        let stale = |t: Option<i64>| t.is_none_or(|t| now - t > 60_000);
        if self.fetch.is_none() && stale(self.fetched_at) {
            self.fetch_origin(ctx);
        }
        if self.status_task.is_none() && stale(self.status.as_ref().map(|s| s.checked_at)) {
            self.refresh_status(ctx);
        }
        self.history.reload();
    }

    pub fn on_run_updated(&mut self, ctx: &egui::Context, run: &Run) {
        self.history.on_run_updated(run);
        if run.project == self.name && run.status != "running" {
            self.refresh_status(ctx);
            self.load_refs(ctx);
        }
    }

    fn poll(&mut self, ctx: &egui::Context) {
        match task::poll(&mut self.load) {
            Some(Ok(d)) => {
                if self.detail.is_none() {
                    self.detail = Some(d);
                }
            }
            Some(Err(e)) => self.error = Some(e),
            None => {}
        }
        let fetched = task::poll(&mut self.fetch);
        let fetch_finished = fetched.is_some();
        match fetched {
            Some(Ok(d)) => {
                self.detail = Some(d);
                self.fetched_at = Some(chrono::Utc::now().timestamp_millis());
                self.load_refs(ctx);
            }
            Some(Err(e)) => self.error = Some(format!("Fetching from origin failed: {e}")),
            None => {}
        }
        if fetch_finished && self.status.is_none() && self.status_task.is_none() {
            self.refresh_status(ctx);
        }
        match task::poll(&mut self.refs_task) {
            Some(Ok(r)) => self.refs = Some(r),
            // While the first fetch is still cloning, a missing clone is expected.
            Some(Err(e)) if self.fetch.is_none() => self.error = Some(e),
            Some(Err(_)) | None => {}
        }
        match task::poll(&mut self.status_task) {
            Some(Ok(s)) => self.status = Some(s),
            Some(Err(e)) => self.error = Some(e),
            None => {}
        }
    }

    pub fn ui(&mut self, ui: &mut Ui, requests: &mut Vec<Request>, running: Option<&RunningRun>) {
        let ctx = ui.ctx().clone();
        self.poll(&ctx);
        let p = theme::palette(ui);

        self.header(ui, &ctx, &p, requests);
        ui.add_space(14.0);
        theme::tabs(ui, &mut self.tab, &[(Tab::Overview, "Overview"), (Tab::History, "History")]);
        ui.add_space(14.0);

        match self.tab {
            // The history table scrolls (and virtualizes) on its own.
            // Errors are about the project's state, not its
            // history; only an in-progress run is shown here.
            Tab::History => {
                if let Some(run) = running {
                    running_banner(ui, &p, run, requests);
                    ui.add_space(12.0);
                }
                let repo = self.detail.as_ref().and_then(|d| theme::github_web(&d.project.url));
                if let Some(run) = self.history.ui(ui, &|_| repo.clone()) {
                    requests.push(Request::OpenRunLog(run));
                }
            }
            // Banners live inside the scroll area so they line up with (and
            // scroll with) the content instead of spanning the scrollbar lane.
            Tab::Overview => {
                ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                    self.banners(ui, &p, running, requests);
                    self.overview(ui, &p, requests, running);
                });
            }
        }
    }

    /// Run in progress and errors, above the tab's content.
    fn banners(&mut self, ui: &mut Ui, p: &Palette, running: Option<&RunningRun>, requests: &mut Vec<Request>) {
        if let Some(run) = running {
            running_banner(ui, p, run, requests);
            ui.add_space(12.0);
        }
        if let Some(msg) = self.error.clone() {
            if theme::dismissable(ui, p.bad, p.bad_soft, &msg) {
                self.error = None;
            }
            ui.add_space(8.0);
        }
    }

    fn header(&mut self, ui: &mut Ui, ctx: &egui::Context, p: &Palette, requests: &mut Vec<Request>) {
        ui.horizontal(|ui| {
            ui.label(theme::title(&self.name, 24.0));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                theme::more_menu(ui, "Project actions", |ui| {
                    if let Some(web) = self.detail.as_ref().and_then(|d| theme::github_web(&d.project.url))
                        && ui.button("Open on GitHub").clicked()
                    {
                        ctx.open_url(egui::OpenUrl::new_tab(web));
                        ui.close();
                    }
                    if ui.button("Fetch from origin").clicked() {
                        self.fetch_origin(ctx);
                        ui.close();
                    }
                    if ui.button("Check server status").clicked() {
                        self.refresh_status(ctx);
                        ui.close();
                    }
                    ui.separator();
                    if ui.add(theme::danger(ui, "Remove project…")).clicked() {
                        requests.push(Request::Confirm(Confirm {
                            title: format!("Remove {}?", self.name),
                            body: Some("Deletes the app's clone and deploy worktree. Nothing on GitHub or on the servers is touched, and run history is kept.".into()),
                            details: vec![],
                            button: "Remove".into(),
                            danger: true,
                            intent: Intent::RemoveProject(self.name.clone()),
                        }));
                        ui.close();
                    }
                });
                let busy = self.fetch.is_some() || self.status_task.is_some();
                let label = if busy { "Refreshing…" } else { "Refresh" };
                if ui.add_enabled(!busy, theme::secondary(label)).on_hover_text("Fetch from origin and check the servers").clicked() {
                    self.fetch_origin(ctx);
                    self.refresh_status(ctx);
                }
            });
        });

        let muted = |s: String| RichText::new(s).size(12.5).color(p.muted);
        let mut parts = Vec::new();
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            if let Some(d) = &self.detail {
                let web = theme::github_web(&d.project.url);
                theme::subtle_link(ui, muted(theme::short_url(&d.project.url)), web.as_deref());
                ui.label(muted("  ·  ".into()));
                let branch = d.default_branch.trim_start_matches("origin/").to_string();
                let branch_url = web.as_ref().map(|w| format!("{w}/tree/{branch}"));
                theme::subtle_link(ui, muted(branch), branch_url.as_deref());
                if let Some(c) = &d.commit {
                    ui.label(muted(" @ ".into()));
                    let commit_url = web.as_ref().map(|w| format!("{w}/commit/{}", c.sha));
                    theme::subtle_link(
                        ui,
                        muted(format!("{} {}", c.short, truncate(&c.subject, 60))),
                        commit_url.as_deref(),
                    );
                }
            }
            self.header_status(&mut parts);
            for part in parts {
                ui.label(muted(format!("  ·  {part}")));
            }
        });
    }

    fn header_status(&self, parts: &mut Vec<String>) {
        if self.fetch.is_some() {
            parts.push("fetching…".into());
        } else if let Some(t) = self.fetched_at {
            parts.push(format!("fetched {}", theme::ago(t)));
        }
        if let Some(s) = &self.status {
            parts.push(format!("status checked {}", theme::ago(s.checked_at)));
        }
    }

    fn overview(&mut self, ui: &mut Ui, p: &Palette, requests: &mut Vec<Request>, running: Option<&RunningRun>) {
        if let Some(s) = &self.status {
            let failed: Vec<_> = s.hosts.iter().filter(|h| !h.ok).collect();
            if !failed.is_empty() {
                theme::banner(ui, p.warn, p.warn_soft, |ui| {
                    let hosts: Vec<&str> = failed.iter().map(|h| h.host.as_str()).collect();
                    ui.label(
                        RichText::new(format!("Can't reach {}. Is the VPN up?", hosts.join(", ")))
                            .family(theme::medium()),
                    );
                    for h in failed {
                        ui.label(RichText::new(format!("{}: {}", h.host, h.error.as_deref().unwrap_or(""))).size(12.0));
                    }
                });
                ui.add_space(16.0);
            }
        }

        let Some(detail) = &self.detail else {
            ui.label(RichText::new("Reading config…").color(p.muted));
            return;
        };
        for e in &detail.errors {
            theme::banner(ui, p.bad, p.bad_soft, |ui| {
                ui.label(e);
            });
            ui.add_space(8.0);
        }
        for t in &detail.targets {
            let current = self
                .refs
                .as_ref()
                .map(|r| self.selected.get(&t.id).cloned().unwrap_or_else(|| r.default_branch.clone()));
            let mut new_selection = None;
            target_header(
                ui,
                p,
                &self.name,
                t,
                self.refs.as_ref(),
                current.as_deref(),
                running,
                &mut new_selection,
                requests,
            );
            ui.add_space(8.0);
            theme::card(ui, |ui| {
                for (i, unit) in t.units.iter().enumerate() {
                    if i > 0 {
                        ui.add_space(4.0);
                        ui.separator();
                        ui.add_space(4.0);
                    }
                    let cx = UnitCtx {
                        project: &self.name,
                        status: self.status.as_ref(),
                        git_ref: current.clone(),
                        ref_label: ref_label(self.refs.as_ref(), current.as_deref()),
                        can_deploy: self.refs.is_some() && running.is_none(),
                        busy: running.is_some(),
                        deploying: running.is_some_and(|r| r.units.contains(&unit.id)),
                        repo: theme::github_web(&detail.project.url),
                        branch: detail.default_branch.trim_start_matches("origin/").to_string(),
                    };
                    unit_row(ui, p, unit, &cx, requests);
                }
            });
            if let Some(sel) = new_selection {
                self.selected.insert(t.id.clone(), sel);
            }
            ui.add_space(28.0);
        }
    }
}

fn ref_label(refs: Option<&RefList>, r: Option<&str>) -> String {
    let (Some(refs), Some(r)) = (refs, r) else { return "the default branch".into() };
    if let Some(c) = refs.commits.iter().find(|c| c.sha == r) {
        return format!("{} {}", c.short, c.subject);
    }
    if let Some(g) = refs.refs.iter().find(|g| g.name == r) {
        return format!("{} ({} {})", g.name, &g.sha[..7], g.subject);
    }
    r.to_string()
}

#[allow(clippy::too_many_arguments)]
fn target_header(
    ui: &mut Ui,
    p: &Palette,
    project: &str,
    t: &Target,
    refs: Option<&RefList>,
    current: Option<&str>,
    running: Option<&RunningRun>,
    new_selection: &mut Option<String>,
    requests: &mut Vec<Request>,
) {
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.label(theme::title(if t.id == "default" { "Production" } else { &t.id }, 17.0));
            let caption = if t.id == "default" {
                "Default deploy configuration".to_string()
            } else if t.units.iter().any(|u| u.destination.is_some()) {
                format!("Kamal destination  ·  -d {}", t.id)
            } else {
                "Separate deploy configuration".to_string()
            };
            ui.label(theme::caption(caption));
        });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let this_target = running.is_some_and(|r| t.units.iter().any(|u| r.units.contains(&u.id)));
            let label = if this_target {
                "Deploying…"
            } else if t.units.len() > 1 {
                "Deploy all"
            } else {
                "Deploy"
            };
            let target_name = if t.id == "default" { project.to_string() } else { format!("{project} · {}", t.id) };
            let services: Vec<String> =
                t.units.iter().map(|u| u.config.service.clone().unwrap_or_else(|| u.id.clone())).collect();
            let disabled_reason = if running.is_some() {
                "A run is already in progress in this project"
            } else {
                "Waiting for the project to load"
            };
            let r = ui
                .add_enabled(refs.is_some() && running.is_none(), theme::primary(label))
                .on_hover_text(format!("Deploys {}", services.join(" and ")))
                .on_disabled_hover_text(disabled_reason);
            if this_target {
                ui.add(egui::Spinner::new().color(theme::ACCENT));
            }
            if r.clicked() {
                requests.push(Request::Confirm(Confirm {
                    title: format!("Deploy {target_name}?"),
                    body: Some(format!(
                        "From {}.{}",
                        ref_label(refs, current),
                        if t.units.len() > 1 { " Runs one after another and stops at the first failure." } else { "" }
                    )),
                    details: t.units.iter().map(|u| format!("kamal deploy {}", u.kamal_args().join(" "))).collect(),
                    button: "Deploy".into(),
                    danger: false,
                    intent: Intent::Run {
                        project: project.to_string(),
                        units: t.units.iter().map(|u| u.id.clone()).collect(),
                        action: Action::Deploy,
                        git_ref: current.map(String::from),
                        label: format!("deploy {}", t.id),
                    },
                }));
            }
            if let (Some(refs), Some(cur)) = (refs, current) {
                ref_picker(ui, &t.id, refs, cur, new_selection);
            }
            let _ = p;
        });
    });
}

fn ref_picker(ui: &mut Ui, target: &str, refs: &RefList, current: &str, new_selection: &mut Option<String>) {
    let mut sel = current.to_string();
    let short = |s: &str| truncate(s, 56);
    let latest = format!("{} (latest)", refs.default_branch.trim_start_matches("origin/"));
    egui::ComboBox::from_id_salt(("ref", target))
        .width(280.0)
        .height(480.0)
        .selected_text(if sel == refs.default_branch {
            latest.clone()
        } else {
            short(&ref_label(Some(refs), Some(&sel)))
        })
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut sel, refs.default_branch.clone(), latest);
            ui.separator();
            ui.label(theme::caption(format!(
                "Recent commits on {}",
                refs.default_branch.trim_start_matches("origin/")
            )));
            for c in &refs.commits {
                ui.selectable_value(
                    &mut sel,
                    c.sha.clone(),
                    format!("{}  {}  ·  {}", c.short, short(&c.subject), c.author),
                );
            }
            let branches: Vec<_> = refs.refs.iter().filter(|r| r.kind == "branch").collect();
            if !branches.is_empty() {
                ui.separator();
                ui.label(theme::caption("Branches"));
                for b in branches {
                    ui.selectable_value(&mut sel, b.name.clone(), b.name.trim_start_matches("origin/"));
                }
            }
            let tags: Vec<_> = refs.refs.iter().filter(|r| r.kind == "tag").take(30).collect();
            if !tags.is_empty() {
                ui.separator();
                ui.label(theme::caption("Tags"));
                for t in tags {
                    ui.selectable_value(&mut sel, t.name.clone(), &t.name);
                }
            }
        });
    if sel != current {
        *new_selection = Some(sel);
    }
}

struct UnitCtx<'a> {
    project: &'a str,
    status: Option<&'a ProjectStatus>,
    git_ref: Option<String>,
    ref_label: String,
    can_deploy: bool,
    /// Some run is in progress in this project.
    busy: bool,
    /// This unit is part of the run in progress.
    deploying: bool,
    /// `https://github.com/org/app`, for links.
    repo: Option<String>,
    /// Default branch (`master`), for compare links.
    branch: String,
}

/// Kamal labels app containers with `service` and, when deployed with -d,
/// `destination`. Match both, so destinations that keep the base service name
/// don't show each other's containers.
fn own_container(unit: &Unit, c: &Container) -> bool {
    unit.config.service.as_deref() == Some(c.service.as_str()) && c.destination == unit.destination
}

/// The shell "Open shell" starts in a container: bash when the image has it
/// (line editing, tab completion), else sh. Kamal passes the command on to the
/// remote shell as-is, so the inner quotes survive. `exec bash || exec sh`
/// wouldn't do: sh exits when an exec fails.
const SHELL: &str = "sh -c 'command -v bash >/dev/null && exec bash || exec sh'";

fn interactive(cmd: &str) -> bool {
    let words: Vec<&str> = cmd.split_whitespace().collect();
    words.contains(&"exec") && (words.contains(&"-i") || words.contains(&"--interactive"))
}

fn kind_label(kind: &str) -> String {
    let mut c = kind.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

/// One service: name and config on the left, live containers in the middle,
/// actions on the right.
fn unit_row(ui: &mut Ui, p: &Palette, unit: &Unit, cx: &UnitCtx, requests: &mut Vec<Request>) {
    let cfg = &unit.config;
    let service = cfg.service.clone().unwrap_or_else(|| "(no service)".into());
    let file = unit.config_file.trim_start_matches("config/");
    let caption = match &unit.destination {
        Some(d) => format!("{}  ·  {file} -d {d}", kind_label(&unit.kind)),
        None => format!("{}  ·  {file}", kind_label(&unit.kind)),
    };

    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(vec2(NAME_COL, 0.0), Layout::top_down(Align::Min), |ui| {
            ui.set_width(NAME_COL);
            ui.label(RichText::new(&service).family(theme::medium()).size(14.5));
            ui.label(theme::caption(caption));
            if cx.deploying {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().size(12.0).color(theme::ACCENT));
                    ui.label(RichText::new("Deploying…").size(12.5).color(theme::ACCENT));
                });
            }
        });
        ui.with_layout(Layout::top_down(Align::Min), |ui| {
            // The service's ⋯ sits at the end of its first line, in the same
            // column as each accessory's ⋯, so all menus and links line up.
            let mut menu_requests = Vec::new();
            {
                let mut menu = |ui: &mut Ui| unit_menu(ui, unit, cx, &service, &mut menu_requests);
                containers(ui, p, unit, cfg, cx, requests, &mut menu);
            }
            requests.extend(menu_requests);
            base_image_line(ui, p, unit, cx.status);
        });
    });
}

/// A line of text with the trailing menu slot on the right.
fn line_with_menu(ui: &mut Ui, text: RichText, menu: &mut Option<&mut dyn FnMut(&mut Ui)>) {
    ui.horizontal(|ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            trailing_slot(ui, menu.take());
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                ui.add(egui::Label::new(text).truncate());
            });
        });
    });
}

/// Width every line reserves on the right for a ⋯ menu.
const MENU_SLOT: f32 = 28.0;

fn trailing_slot(ui: &mut Ui, menu: Option<&mut dyn FnMut(&mut Ui)>) {
    match menu {
        Some(menu) => menu(ui),
        None => {
            ui.add_space(MENU_SLOT);
        }
    }
}

fn containers(
    ui: &mut Ui,
    p: &Palette,
    unit: &Unit,
    cfg: &UnitConfig,
    cx: &UnitCtx,
    requests: &mut Vec<Request>,
    unit_menu: &mut dyn FnMut(&mut Ui),
) {
    // Handed to the first line drawn; every other line gets an empty slot.
    let mut unit_menu: Option<&mut dyn FnMut(&mut Ui)> = Some(unit_menu);
    let Some(status) = cx.status else {
        let hosts: Vec<String> = cfg.servers.iter().map(|s| format!("{} on {}", s.role, s.host)).collect();
        line_with_menu(ui, theme::caption(format!("Checking {}…", hosts.join(", "))), &mut unit_menu);
        return;
    };
    let unit_hosts: BTreeSet<&str> = cfg.servers.iter().map(|s| s.host.as_str()).collect();
    let unreachable = status.hosts.iter().filter(|h| !h.ok && unit_hosts.contains(h.host.as_str())).count();
    let all_unreachable = !unit_hosts.is_empty() && unreachable == unit_hosts.len();

    let running: Vec<(&str, &Container)> = status
        .hosts
        .iter()
        .flat_map(|h| {
            h.containers
                .iter()
                .filter(|c| own_container(unit, c) && c.state == "running")
                .map(move |c| (h.host.as_str(), c))
        })
        .collect();
    let accessories: Vec<(&str, &str, Option<&Container>)> = cfg
        .accessories
        .iter()
        .flat_map(|a| {
            let found: Vec<(&str, &str, Option<&Container>)> = status
                .hosts
                .iter()
                .flat_map(|h| {
                    h.containers
                        .iter()
                        .filter(|c| c.service == a.service)
                        .map(move |c| (a.name.as_str(), h.host.as_str(), Some(c)))
                })
                .collect();
            if found.is_empty() { vec![(a.name.as_str(), "", None)] } else { found }
        })
        .collect();

    if running.is_empty() {
        let text = if all_unreachable {
            let hosts: Vec<&str> = unit_hosts.iter().copied().collect();
            format!("Status unknown  ·  can't reach {}", hosts.join(", "))
        } else {
            "Not running".to_string()
        };
        line_with_menu(ui, RichText::new(text).color(p.muted), &mut unit_menu);
        ui.add_space(6.0);
        // Fall through: accessories still get their rows (and Boot).
    }

    let logs = |requests: &mut Vec<Request>, host: &str, c: &Container| {
        requests.push(Request::Do(Intent::Logs {
            host: host.to_string(),
            ssh: cfg.ssh.clone(),
            container: c.name.clone(),
            label: format!("{} {}@{host}", c.service, c.role.as_deref().unwrap_or("")),
            project: cx.project.to_string(),
        }));
    };

    for (host, c) in &running {
        let info = status.versions.get(&c.version);
        let sha = c.version.split('_').next().unwrap_or(&c.version);
        let freshness = match info.and_then(|i| i.behind) {
            Some(0) => Some(("latest".to_string(), p.ok, None)),
            // What's on the default branch but not on the server.
            Some(n) => Some((
                format!("{n} behind"),
                p.warn,
                cx.repo.as_ref().map(|r| format!("{r}/compare/{sha}...{}", cx.branch)),
            )),
            None => None,
        };
        let open = container_block(
            ui,
            p,
            ContainerLine {
                dot: p.ok,
                name: c.role.as_deref().unwrap_or("app"),
                hover: &c.name,
                version: Some(&c.version),
                version_url: cx.repo.as_ref().map(|r| format!("{r}/commit/{sha}")),
                note: info.and_then(|i| i.subject.as_deref()).unwrap_or(""),
                freshness,
                detail: format!("{host}  ·  {}", c.status),
                logs: true,
                accessory: None,
            },
            unit_menu.take(),
        );
        if open.logs {
            logs(requests, host, c);
        }
    }
    for (name, host, c) in &accessories {
        let (exists, running) = match c {
            Some(c) => (true, c.state == "running"),
            None => (false, false),
        };
        let menu =
            Some(AccessoryMenu { exists: exists || all_unreachable, running, busy: cx.busy, unknown: all_unreachable });
        let line = match c {
            Some(c) => ContainerLine {
                dot: if running { p.ok } else { p.bad },
                name,
                hover: &c.name,
                version: None,
                version_url: None,
                note: "accessory",
                freshness: None,
                detail: format!("{host}  ·  {}", c.status),
                logs: true,
                accessory: menu,
            },
            None => ContainerLine {
                dot: p.border,
                name,
                hover: "",
                version: None,
                version_url: None,
                note: "accessory",
                freshness: None,
                detail: if all_unreachable { "status unknown" } else { "not running" }.to_string(),
                logs: false,
                accessory: menu,
            },
        };
        let click = container_block(ui, p, line, None);
        if click.logs
            && let Some(c) = c
        {
            logs(requests, host, c);
        }
        if let Some(verb) = click.accessory {
            requests.push(accessory_request(unit, cx, name, verb));
        }
        if click.shell {
            requests.push(Request::Do(Intent::Terminal {
                project: cx.project.to_string(),
                unit_id: unit.id.clone(),
                args: ["accessory", "exec", "-i", "--reuse", name, SHELL].map(String::from).to_vec(),
                title: format!("{name} shell"),
            }));
        }
    }
}

/// What a container line's accessory menu may offer.
struct AccessoryMenu {
    exists: bool,
    running: bool,
    busy: bool,
    /// Servers unreachable: offer everything, kamal will tell.
    unknown: bool,
}

#[derive(Default)]
struct BlockClick {
    logs: bool,
    accessory: Option<AccessoryVerb>,
    /// Open a shell in the accessory's container.
    shell: bool,
}

fn accessory_request(unit: &Unit, cx: &UnitCtx, name: &str, verb: AccessoryVerb) -> Request {
    let args = unit.kamal_args().join(" ");
    let intent = Intent::Run {
        project: cx.project.to_string(),
        units: vec![unit.id.clone()],
        action: Action::Accessory { name: name.to_string(), verb },
        git_ref: None,
        label: format!("{name} {}", verb.as_str()),
    };
    let (title, body, button, danger) = match verb {
        AccessoryVerb::Details => return Request::Do(intent),
        AccessoryVerb::Boot => (
            format!("Boot {name}?"),
            "Creates and starts the container from the deploy config, with its secrets.",
            "Boot",
            false,
        ),
        AccessoryVerb::Reboot => (
            format!("Reboot {name}?"),
            "Removes and recreates the container with the current config. Data in its volumes is kept, but it is briefly unavailable.",
            "Reboot",
            true,
        ),
        AccessoryVerb::Restart => {
            (format!("Restart {name}?"), "Restarts the existing container; briefly unavailable.", "Restart", false)
        }
        AccessoryVerb::Start => (format!("Start {name}?"), "Starts the stopped container.", "Start", false),
        AccessoryVerb::Stop => (format!("Stop {name}?"), "The app may fail while it's stopped.", "Stop", true),
        AccessoryVerb::Remove => (
            format!("Remove {name}?"),
            "Deletes the container, its image AND its data directory on the server. This can't be undone.",
            "Remove",
            true,
        ),
    };
    let cmd = Action::Accessory { name: name.to_string(), verb }.kamal_args().join(" ");
    Request::Confirm(Confirm {
        title,
        body: Some(body.into()),
        details: vec![format!("kamal {cmd} {args}")],
        button: button.into(),
        danger,
        intent,
    })
}

struct ContainerLine<'a> {
    dot: egui::Color32,
    /// Role (web, scheduler…) or accessory name.
    name: &'a str,
    hover: &'a str,
    version: Option<&'a str>,
    version_url: Option<String>,
    /// Commit subject, or "accessory".
    note: &'a str,
    /// "latest" / "3 behind", its color and an optional compare link.
    freshness: Option<(String, egui::Color32, Option<String>)>,
    /// Host and uptime, shown underneath in grey.
    detail: String,
    logs: bool,
    /// Accessories get their own actions menu.
    accessory: Option<AccessoryMenu>,
}

/// One container as two lines: what runs (with actions on the right), then
/// where and for how long.
fn container_block(
    ui: &mut Ui,
    p: &Palette,
    line: ContainerLine,
    unit_menu: Option<&mut dyn FnMut(&mut Ui)>,
) -> BlockClick {
    let mut click = BlockClick::default();
    ui.spacing_mut().item_spacing.y = 2.0;
    ui.horizontal(|ui| {
        // Right side first so the commit subject can truncate to what's left.
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if line.accessory.is_none() {
                trailing_slot(ui, unit_menu);
            }
            if let Some(menu) = &line.accessory {
                theme::more_menu(ui, &format!("Actions for {}", line.name), |ui| {
                    if menu.busy {
                        ui.label(theme::caption("A run is in progress. Actions are available when it finishes."));
                        ui.separator();
                    }
                    ui.add_enabled_ui(!menu.busy, |ui| {
                        let mut item = |ui: &mut Ui, text: &str, verb: AccessoryVerb, show: bool| {
                            if show && ui.button(text).clicked() {
                                click.accessory = Some(verb);
                                ui.close();
                            }
                        };
                        item(ui, "Boot", AccessoryVerb::Boot, !menu.exists || menu.unknown);
                        item(ui, "Start", AccessoryVerb::Start, menu.exists && (!menu.running || menu.unknown));
                        item(ui, "Restart", AccessoryVerb::Restart, menu.exists && (menu.running || menu.unknown));
                        item(ui, "Reboot (recreate container)", AccessoryVerb::Reboot, menu.exists);
                        item(ui, "Stop", AccessoryVerb::Stop, menu.exists && (menu.running || menu.unknown));
                        ui.separator();
                        if (menu.running || menu.unknown) && ui.button("Open shell").clicked() {
                            click.shell = true;
                            ui.close();
                        }
                        item(ui, "Details", AccessoryVerb::Details, true);
                        if menu.exists && ui.add(theme::danger(ui, "Remove…")).clicked() {
                            click.accessory = Some(AccessoryVerb::Remove);
                            ui.close();
                        }
                    });
                });
            }
            if line.logs && theme::link(ui, "Logs").clicked() {
                click.logs = true;
            }
            if let Some((text, color, url)) = &line.freshness {
                theme::subtle_link(ui, RichText::new(text).size(12.5).color(*color), url.as_deref());
            }
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                theme::dot(ui, line.dot);
                let name = ui.label(RichText::new(line.name).family(theme::medium()));
                if !line.hover.is_empty() {
                    name.on_hover_text(line.hover);
                }
                if let Some(v) = line.version {
                    theme::subtle_link(
                        ui,
                        RichText::new(theme::short_version(v)).monospace(),
                        line.version_url.as_deref(),
                    );
                }
                ui.add(egui::Label::new(theme::caption(line.note)).truncate());
            });
        });
    });
    ui.horizontal(|ui| {
        ui.add_space(18.0);
        ui.label(RichText::new(line.detail).size(12.0).color(p.muted));
    });
    ui.add_space(10.0);
    click
}

fn unit_menu(ui: &mut Ui, unit: &Unit, cx: &UnitCtx, service: &str, requests: &mut Vec<Request>) {
    // Versions that exist as stopped containers can be rolled back to.
    let mut rollback: Vec<(String, String)> = Vec::new();
    if let Some(status) = cx.status {
        let live: BTreeSet<&str> = status
            .hosts
            .iter()
            .flat_map(|h| &h.containers)
            .filter(|c| own_container(unit, c) && c.state == "running")
            .map(|c| c.version.as_str())
            .collect();
        for c in
            status.hosts.iter().flat_map(|h| &h.containers).filter(|c| own_container(unit, c) && c.state != "running")
        {
            if !live.contains(c.version.as_str()) && !rollback.iter().any(|(v, _)| v == &c.version) {
                let subject = status.versions.get(&c.version).and_then(|v| v.subject.clone()).unwrap_or_default();
                rollback.push((c.version.clone(), subject));
            }
        }
    }

    theme::more_menu(ui, &format!("Actions for {service}"), |ui| {
        if cx.busy {
            ui.label(theme::caption("A run is in progress. Actions are available when it finishes."));
            ui.separator();
        }
        ui.add_enabled_ui(!cx.busy, |ui| unit_menu_items(ui, unit, cx, service, &rollback, requests));
    });
}

#[allow(clippy::too_many_arguments)]
fn unit_menu_items(
    ui: &mut Ui,
    unit: &Unit,
    cx: &UnitCtx,
    service: &str,
    rollback: &[(String, String)],
    requests: &mut Vec<Request>,
) {
    let cfg = &unit.config;
    let args = unit.kamal_args().join(" ");
    let run = |action: Action, label: &str| Intent::Run {
        project: cx.project.to_string(),
        units: vec![unit.id.clone()],
        git_ref: if action.uses_ref() { cx.git_ref.clone() } else { None },
        action,
        label: format!("{} {label}", unit.kind),
    };
    let confirm = |title: String, body: String, button: &str, danger: bool, action: Action, label: &str| {
        Request::Confirm(Confirm {
            title,
            body: (!body.is_empty()).then_some(body),
            details: vec![format!("kamal {} {args}", action.kamal_args().join(" "))],
            button: button.into(),
            danger,
            intent: run(action, label),
        })
    };
    {
        if let Some(repo) = &cx.repo {
            // The destination file when there is one, else the config itself.
            let file = match &unit.destination {
                Some(d) => format!("{}.{d}.yml", unit.config_file.trim_end_matches(".yml")),
                None => unit.config_file.clone(),
            };
            if ui.button("Open config on GitHub").clicked() {
                ui.ctx().open_url(egui::OpenUrl::new_tab(format!("{repo}/blob/{}/{file}", cx.branch)));
                ui.close();
            }
            ui.separator();
        }
        if ui.add_enabled(cx.can_deploy, egui::Button::new(format!("Deploy {service} only"))).clicked() {
            requests.push(confirm(
                format!("Deploy {service}?"),
                format!("From {}.", cx.ref_label),
                "Deploy",
                false,
                Action::Deploy,
                "deploy",
            ));
            ui.close();
        }
        if ui.add_enabled(cx.can_deploy, egui::Button::new("Redeploy (skip bootstrap)")).clicked() {
            requests.push(confirm(
                format!("Redeploy {service}?"),
                format!("From {}.", cx.ref_label),
                "Redeploy",
                false,
                Action::Redeploy,
                "redeploy",
            ));
            ui.close();
        }
        if rollback.is_empty() {
            ui.add_enabled(false, egui::Button::new("Roll back to…"))
                .on_disabled_hover_text("No previous versions on the servers");
        } else {
            ui.menu_button("Roll back to", |ui| {
                for (version, subject) in rollback {
                    let label = format!("{}  {}", theme::short_version(version), truncate(subject, 50));
                    if ui.button(label).clicked() {
                        let mut req = confirm(
                            format!("Roll {service} back to {}?", theme::short_version(version)),
                            subject.clone(),
                            "Roll back",
                            true,
                            Action::Rollback { version: version.clone() },
                            "rollback",
                        );
                        if let Request::Confirm(c) = &mut req {
                            c.details = vec![format!("kamal rollback {version} {args}")];
                        }
                        requests.push(req);
                        ui.close();
                    }
                }
            });
        }
        if ui.button("Reboot (app boot)").clicked() {
            requests.push(confirm(
                format!("Reboot {service}?"),
                "Boots the current version again.".into(),
                "Reboot",
                false,
                Action::AppBoot,
                "app boot",
            ));
            ui.close();
        }
        ui.separator();
        if ui.button("App details").clicked() {
            requests.push(Request::Do(run(Action::AppDetails, "app details")));
            ui.close();
        }
        if ui.button("Lock status").clicked() {
            requests.push(Request::Do(run(Action::LockStatus, "lock status")));
            ui.close();
        }
        if ui.add(theme::danger(ui, "Release lock…")).clicked() {
            requests.push(confirm(
                "Release the deploy lock?".into(),
                "Only do this when no deploy is running.".into(),
                "Release",
                true,
                Action::LockRelease,
                "lock release",
            ));
            ui.close();
        }
        ui.separator();
        if ui.button("Open shell").on_hover_text("bash (or sh) in the running container on the primary host").clicked()
        {
            requests.push(Request::Do(Intent::Terminal {
                project: cx.project.to_string(),
                unit_id: unit.id.clone(),
                args: ["app", "exec", "-i", "--reuse", SHELL].map(String::from).to_vec(),
                title: format!("{} shell", unit.kind),
            }));
            ui.close();
        }
        if !cfg.aliases.is_empty() {
            ui.menu_button("Aliases", |ui| {
                for a in &cfg.aliases {
                    let is_interactive = interactive(&a.command);
                    let label = if is_interactive { format!("{}  ›_", a.name) } else { a.name.clone() };
                    let hover =
                        if is_interactive { format!("{}\nOpens in a terminal.", a.command) } else { a.command.clone() };
                    if ui.button(label).on_hover_text(hover).clicked() {
                        if is_interactive {
                            requests.push(Request::Do(Intent::Terminal {
                                project: cx.project.to_string(),
                                unit_id: unit.id.clone(),
                                args: vec![a.name.clone()],
                                title: format!("{} {}", unit.kind, a.name),
                            }));
                        } else {
                            requests.push(Request::Confirm(Confirm {
                                title: format!("Run alias {}?", a.name),
                                body: Some(a.command.clone()),
                                details: vec![format!("kamal {} {args}", a.name)],
                                button: "Run".into(),
                                danger: false,
                                intent: run(Action::Custom { args: vec![a.name.clone()] }, &a.name),
                            }));
                        }
                        ui.close();
                    }
                }
            });
        }
        if ui.button("Run kamal command…").clicked() {
            requests.push(Request::Prompt(Prompt {
                title: "Run a kamal command".into(),
                body: format!(
                    "Runs in the deploy worktree with {args}. Interactive commands (exec -i) open in a terminal."
                ),
                hint: "app exec \"php artisan about\"".into(),
                value: String::new(),
                project: cx.project.to_string(),
                unit_id: unit.id.clone(),
                unit_kind: unit.kind.clone(),
            }));
            ui.close();
        }
    }
}

/// The external image this unit builds on, which release of it is running and
/// whether a newer one is out.
fn base_image_line(ui: &mut Ui, p: &Palette, unit: &Unit, status: Option<&ProjectStatus>) {
    let Some(base) = &unit.base_image else { return };
    let mut running: Vec<(String, Option<String>)> = Vec::new();
    if let Some(status) = status {
        for c in
            status.hosts.iter().flat_map(|h| &h.containers).filter(|c| own_container(unit, c) && c.state == "running")
        {
            if let Some(v) = &c.image_version {
                let entry = (images::release_of(v).to_string(), c.image_source.clone());
                if !running.contains(&entry) {
                    running.push(entry);
                }
            }
        }
    }
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let kind = if images::is_pinned(base) {
            "Pinned to this version."
        } else {
            "Floating tag: each deploy builds on whatever was last published under it."
        };
        ui.label(theme::caption("Image")).on_hover_text(kind);
        ui.label(RichText::new(base).monospace().size(12.0).color(p.muted)).on_hover_text(kind);
        for (version, source) in &running {
            let latest = source.as_ref().and_then(|s| status.and_then(|st| st.releases.get(s)));
            match latest {
                Some(r) if r.tag == *version => {
                    ui.label(theme::caption(format!("· {version}")));
                    ui.label(RichText::new("latest").size(12.5).color(p.ok));
                }
                Some(r) => {
                    let date = r.published_at.get(..10).unwrap_or(&r.published_at);
                    ui.label(theme::caption(format!("· {version} →")));
                    ui.label(RichText::new(format!("{} available", r.tag)).size(12.5).color(p.warn))
                        .on_hover_text(format!("Released {date}. Deploy to pick it up."));
                    ui.hyperlink_to(RichText::new("Release notes").size(12.5).color(theme::ACCENT), &r.url);
                }
                None => {
                    ui.label(theme::caption(format!("· {version}")));
                }
            }
        }
    });
}

/// Shown at the top of the project while a kamal run is in progress.
fn running_banner(ui: &mut Ui, p: &Palette, run: &RunningRun, requests: &mut Vec<Request>) {
    let _ = p;
    egui::Frame::new()
        .fill(egui::Color32::from_rgb(0xea, 0xf1, 0xfe))
        .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(0xc7, 0xd9, 0xfb)))
        .corner_radius(egui::CornerRadius::same(8))
        .inner_margin(egui::Margin::symmetric(14, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new().size(16.0).color(theme::ACCENT));
                ui.add_space(4.0);
                ui.vertical(|ui| {
                    let what = if run.label.is_empty() { "Running".to_string() } else { run.label.clone() };
                    ui.label(RichText::new(format!("{what} in progress")).family(theme::medium()).color(theme::ACCENT));
                    let units: Vec<String> =
                        run.units.iter().map(|u| u.trim_start_matches("config/").replace(".yml", "")).collect();
                    ui.label(theme::caption(format!("{}  ·  {}", units.join(", "), elapsed(run.started_at))));
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.add(theme::danger(ui, "Cancel")).clicked() {
                        requests.push(Request::Confirm(Confirm {
                            title: "Cancel the run?".into(),
                            body: Some(
                                "Stops kamal and everything it started. A deploy stopped halfway can leave the deploy lock held; release it from the service’s actions menu if so."
                                    .into(),
                            ),
                            details: vec![],
                            button: "Cancel run".into(),
                            danger: true,
                            intent: Intent::Cancel(run.key.clone()),
                        }));
                    }
                    if ui.add(theme::secondary("Show output")).clicked() {
                        requests.push(Request::FocusSession(run.key.clone()));
                    }
                });
            });
        });
}

#[cfg(test)]
mod bench {
    //! `KGUI_BENCH_PROJECT=my-app cargo test --release -p kamal-gui project_frame_cost -- --ignored --nocapture`
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    #[ignore]
    fn project_frame_cost() {
        let name = std::env::var("KGUI_BENCH_PROJECT")
            .ok()
            .or_else(|| project::list().into_iter().next().map(|p| p.name))
            .expect("add a project or set KGUI_BENCH_PROJECT");
        let ctx = egui::Context::default();
        crate::theme::install(&ctx);
        let mut view = ProjectView::new(&ctx, &name);
        let input = || egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1100.0, 800.0))),
            ..Default::default()
        };
        let frame = |view: &mut ProjectView| {
            let mut requests = Vec::new();
            let mut out = ctx.run_ui(input(), |ui| {
                egui::CentralPanel::default().show(ui, |ui| view.ui(ui, &mut requests, None));
            });
            out.textures_delta.clear();
            let _ = ctx.tessellate(out.shapes, out.pixels_per_point);
        };
        let deadline = Instant::now() + Duration::from_secs(15);
        while (view.detail.is_none() || view.refs.is_none()) && Instant::now() < deadline {
            frame(&mut view);
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(view.detail.is_some(), "project did not load");
        for _ in 0..3 {
            frame(&mut view);
        }
        let n = 60;
        let t = Instant::now();
        for _ in 0..n {
            frame(&mut view);
        }
        let targets = view.detail.as_ref().map(|d| d.targets.len()).unwrap_or(0);
        println!("{name}: {targets} targets  steady frame {:?}", t.elapsed() / n);
    }
}
