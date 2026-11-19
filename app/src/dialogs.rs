use crate::app::{Confirm, Prompt};
use crate::task::{self, Task};
use crate::theme;
use egui::{Id, Key, Modal, RichText, ScrollArea, Ui};
use kgui_core::github::{self, Repo};
use kgui_core::project::{self, Project};
use kgui_core::settings::Settings;
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn buttons(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    ui.add_space(6.0);
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), add);
}

pub enum QuitChoice {
    Stay,
    StopAndQuit,
}

/// Shown when quitting while kamal runs are in progress.
pub fn quit(ctx: &egui::Context, runs: &[String], stopping: bool) -> Option<QuitChoice> {
    let mut choice = None;
    let resp = Modal::new(Id::unique("quit")).show(ctx, |ui| {
        ui.set_width(480.0);
        let p = theme::palette(ui);
        let title = if runs.len() == 1 { "A deploy is still running" } else { "Deploys are still running" };
        ui.label(theme::title(title, 15.0));
        theme::banner(ui, ui.visuals().text_color(), p.subtle, |ui| {
            for r in runs {
                ui.label(RichText::new(r).monospace());
            }
        });
        if stopping {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Stopping kamal, then quitting…");
            });
            return;
        }
        ui.add(
            egui::Label::new(
                RichText::new(
                    "Quitting stops it partway. That can leave servers on a mix of versions and the deploy lock held \
                     (release it later from the service's actions menu).",
                )
                .color(p.muted),
            )
            .wrap(),
        );
        buttons(ui, |ui| {
            if ui.add(theme::primary("Keep running")).clicked() || ui.input(|i| i.key_pressed(Key::Enter)) {
                choice = Some(QuitChoice::Stay);
            }
            let stop = theme::filled("Stop and quit", p.bad);
            if ui.add(stop).clicked() {
                choice = Some(QuitChoice::StopAndQuit);
            }
        });
    });
    if resp.should_close() && choice.is_none() && !stopping {
        choice = Some(QuitChoice::Stay);
    }
    choice
}

/// Some(true) = confirmed, Some(false) = dismissed, None = still open.
pub fn confirm(ctx: &egui::Context, c: &Confirm) -> Option<bool> {
    let mut answer = None;
    let resp = Modal::new(Id::unique("confirm")).show(ctx, |ui| {
        ui.set_width(480.0);
        let p = theme::palette(ui);
        ui.label(theme::title(&c.title, 15.0));
        if let Some(b) = &c.body {
            ui.add(egui::Label::new(RichText::new(b).color(p.muted)).wrap());
        }
        if !c.details.is_empty() {
            theme::banner(ui, ui.visuals().text_color(), p.border, |ui| {
                for d in &c.details {
                    ui.label(RichText::new(d).monospace());
                }
            });
        }
        buttons(ui, |ui| {
            let ok = if c.danger { theme::filled(&c.button, p.bad) } else { theme::primary(&c.button) };
            if ui.add(ok).clicked() || ui.input(|i| i.key_pressed(Key::Enter)) {
                answer = Some(true);
            }
            if ui.button("Cancel").clicked() {
                answer = Some(false);
            }
        });
    });
    if resp.should_close() && answer.is_none() {
        answer = Some(false);
    }
    answer
}

/// Some(Some(text)) = submitted, Some(None) = dismissed, None = still open.
pub fn prompt(ctx: &egui::Context, p: &mut Prompt) -> Option<Option<String>> {
    let mut answer = None;
    let resp = Modal::new(Id::unique("prompt")).show(ctx, |ui| {
        ui.set_width(520.0);
        let pal = theme::palette(ui);
        ui.label(theme::title(&p.title, 15.0));
        ui.add(egui::Label::new(RichText::new(&p.body).color(pal.muted)).wrap());
        let r = ui.add(
            egui::TextEdit::singleline(&mut p.value)
                .hint_text(&p.hint)
                .font(egui::TextStyle::Monospace)
                .desired_width(f32::INFINITY),
        );
        r.request_focus();
        let submit = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
        buttons(ui, |ui| {
            if (ui.add_enabled(!p.value.trim().is_empty(), theme::primary("Run")).clicked() || submit)
                && !p.value.trim().is_empty()
            {
                answer = Some(Some(p.value.clone()));
            }
            if ui.button("Cancel").clicked() {
                answer = Some(None);
            }
        });
    });
    if resp.should_close() && answer.is_none() {
        answer = Some(None);
    }
    answer
}

/// Split a command line on whitespace, honoring '…' and "…" quotes.
pub fn split_args(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut has = false;
    for c in s.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => cur.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                has = true;
            }
            (None, c) if c.is_whitespace() => {
                if has || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            (None, c) => cur.push(c),
        }
    }
    if has || !cur.is_empty() {
        out.push(cur);
    }
    out
}

// ── Add projects ────────────────────────────────────────────────────────

#[derive(PartialEq, Eq, Clone, Copy)]
enum Mode {
    Github,
    Url,
}

pub struct AddDialog {
    mode: Mode,
    url: String,
    owners: Vec<String>,
    owner: String,
    owners_task: Option<Task<(Vec<String>, String)>>,
    repos: Vec<Repo>,
    repos_owner: String,
    repos_task: Option<Task<Vec<Repo>>>,
    query: String,
    kamal_only: bool,
    picked: BTreeSet<String>,
    adding: Option<Adding>,
    added: Vec<String>,
    error: Option<String>,
}

impl AddDialog {
    pub fn new(ctx: &egui::Context) -> Self {
        Self {
            mode: Mode::Github,
            url: String::new(),
            owners: Vec::new(),
            owner: String::new(),
            owners_task: Some(Task::spawn(ctx, || Ok((github::owners()?, Settings::load().github_owner)))),
            repos: Vec::new(),
            repos_owner: String::new(),
            repos_task: None,
            query: String::new(),
            kamal_only: true,
            picked: BTreeSet::new(),
            adding: None,
            added: Vec::new(),
            error: None,
        }
    }

    fn poll(&mut self, ctx: &egui::Context) -> Option<Vec<String>> {
        match task::poll(&mut self.owners_task) {
            Some(Ok((owners, preferred))) => {
                self.owner =
                    if owners.contains(&preferred) { preferred } else { owners.first().cloned().unwrap_or_default() };
                self.owners = owners;
            }
            Some(Err(e)) => {
                self.error = Some(e);
                self.mode = Mode::Url;
            }
            None => {}
        }
        if !self.owner.is_empty() && self.owner != self.repos_owner {
            self.repos_owner = self.owner.clone();
            self.repos.clear();
            self.picked.clear();
            let owner = self.owner.clone();
            self.repos_task = Some(Task::spawn(ctx, move || {
                let repos = github::repos(&owner)?;
                let mut s = Settings::load();
                if s.github_owner != owner {
                    s.github_owner = owner;
                    let _ = s.save();
                }
                Ok(repos)
            }));
        }
        match task::poll(&mut self.repos_task) {
            Some(Ok(r)) => self.repos = r,
            Some(Err(e)) => self.error = Some(e),
            None => {}
        }
        // All went well: close on its own. Otherwise the list stays up with
        // what failed until Close.
        if let Some(a) = &self.adding
            && a.finished()
            && a.all_added()
        {
            self.added.extend(a.added());
            self.adding = None;
            return Some(std::mem::take(&mut self.added));
        }
        None
    }

    fn start_adding(&mut self, ctx: &egui::Context, urls: Vec<String>) {
        self.error = None;
        self.adding = Some(Adding::start(ctx, urls));
    }

    /// Returns the names of added projects once the dialog closes.
    pub fn ui(&mut self, ctx: &egui::Context, existing: &[Project]) -> Option<Vec<String>> {
        if let Some(done) = self.poll(ctx) {
            return Some(done);
        }
        let busy = self.adding.as_ref().is_some_and(|a| !a.finished());
        if busy {
            // Elapsed times keep moving.
            ctx.request_repaint_after(Duration::from_millis(500));
        }
        let mut close = false;
        let resp = Modal::new(Id::unique("add-project")).show(ctx, |ui| {
            ui.set_width(680.0);
            let p = theme::palette(ui);
            ui.label(theme::title("Add projects", 15.0));
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.mode, Mode::Github, "From GitHub");
                ui.selectable_value(&mut self.mode, Mode::Url, "Git URL");
            });
            ui.separator();

            if let Some(adding) = &self.adding {
                adding.ui(ui, &p);
            } else {
                match self.mode {
                    Mode::Url => {
                        ui.label(RichText::new("Repository URL").family(theme::medium()));
                        ui.add_enabled(
                            !busy,
                            egui::TextEdit::singleline(&mut self.url)
                                .hint_text("git@github.com:org/app.git")
                                .font(egui::TextStyle::Monospace)
                                .desired_width(f32::INFINITY),
                        );
                        ui.label(
                        RichText::new(
                            "The app keeps its own clone and deploys from it. Your working copies are never touched.",
                        )
                        .size(12.0)
                        .color(p.muted),
                    );
                    }
                    Mode::Github => self.github_list(ui, existing, busy),
                }
            }
            if let Some(e) = &self.error {
                ui.add(egui::Label::new(RichText::new(e).color(p.bad)).wrap());
            }

            if let Some(adding) = &self.adding {
                buttons(ui, |ui| {
                    if busy {
                        let stopping = adding.stop.load(Ordering::Relaxed);
                        let label = if stopping { "Stopping…" } else { "Stop" };
                        if ui
                            .add_enabled(!stopping, egui::Button::new(label))
                            .on_hover_text("Finish the clones under way and skip the rest")
                            .clicked()
                        {
                            adding.stop.store(true, Ordering::Relaxed);
                        }
                    } else if ui.add(theme::primary("Close")).clicked() {
                        close = true;
                    }
                });
                return;
            }
            buttons(ui, |ui| {
                let (label, urls): (String, Vec<String>) = match self.mode {
                    Mode::Url => (
                        "Add project".into(),
                        if self.url.trim().is_empty() { vec![] } else { vec![self.url.trim().to_string()] },
                    ),
                    Mode::Github => {
                        let n = self.picked.len();
                        (
                            format!(
                                "Add {} project{}",
                                if n > 0 { n.to_string() } else { String::new() },
                                if n == 1 { "" } else { "s" }
                            ),
                            self.picked.iter().cloned().collect(),
                        )
                    }
                };
                if ui.add_enabled(!busy && !urls.is_empty(), theme::primary(label.trim())).clicked() {
                    self.start_adding(ctx, urls);
                }
                if ui.add_enabled(!busy, egui::Button::new("Close")).clicked() {
                    close = true;
                }
            });
        });
        if close || (resp.should_close() && !busy) {
            if let Some(a) = self.adding.take() {
                self.added.extend(a.added());
            }
            Some(std::mem::take(&mut self.added))
        } else {
            None
        }
    }

    fn github_list(&mut self, ui: &mut Ui, existing: &[Project], busy: bool) {
        let p = theme::palette(ui);
        let search_worked = self.repos.iter().any(|r| r.kamal.is_some());
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("owner").selected_text(&self.owner).width(200.0).show_ui(ui, |ui| {
                for o in &self.owners {
                    ui.selectable_value(&mut self.owner, o.clone(), o);
                }
            });
            ui.add(egui::TextEdit::singleline(&mut self.query).hint_text("Filter…").desired_width(220.0));
            ui.add_enabled(search_worked, egui::Checkbox::new(&mut self.kamal_only, "Kamal projects only"))
                .on_disabled_hover_text("GitHub code search was unavailable");
        });
        let q = self.query.trim().to_lowercase();
        let visible: Vec<&Repo> = self
            .repos
            .iter()
            .filter(|r| !self.kamal_only || !search_worked || r.kamal == Some(true))
            .filter(|r| r.full_name.to_lowercase().contains(&q))
            .collect();
        egui::Frame::new().stroke(egui::Stroke::new(1.0, p.border)).corner_radius(6).inner_margin(6).show(ui, |ui| {
            ScrollArea::vertical().min_scrolled_height(320.0).max_height(420.0).auto_shrink([false, false]).show(
                ui,
                |ui| {
                    if self.owners_task.is_some() || self.repos_task.is_some() {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(RichText::new("Loading repositories…").color(p.muted));
                        });
                        return;
                    }
                    if visible.is_empty() {
                        ui.label(RichText::new("No repositories match.").color(p.muted));
                    }
                    for r in visible {
                        let done = existing.iter().any(|e| e.url == r.url || e.name == r.name);
                        ui.horizontal(|ui| {
                            let mut checked = done || self.picked.contains(&r.url);
                            if ui
                                .add_enabled(
                                    !done && !busy,
                                    egui::Checkbox::new(&mut checked, RichText::new(&r.name).family(theme::medium())),
                                )
                                .changed()
                            {
                                if checked {
                                    self.picked.insert(r.url.clone());
                                } else {
                                    self.picked.remove(&r.url);
                                }
                            }
                            if r.kamal == Some(true) {
                                ui.label(RichText::new("Kamal").size(12.5).color(theme::ACCENT));
                            }
                            if done {
                                ui.label(theme::caption("Already added"));
                            }
                            if let Some(d) = &r.description {
                                ui.add(egui::Label::new(RichText::new(d).color(p.muted)).truncate());
                            }
                        });
                    }
                },
            );
        });
    }
}

/// How many repositories are cloned at the same time.
const PARALLEL_CLONES: usize = 4;

#[derive(Clone)]
enum CloneState {
    Waiting,
    Cloning(Instant),
    Added(Duration),
    Failed(String),
    /// Stopped before it started.
    Skipped,
}

/// Repositories being cloned and added, a few at a time, each with its own
/// state so the dialog can show what's going on.
struct Adding {
    names: Vec<String>,
    states: Arc<Mutex<Vec<CloneState>>>,
    stop: Arc<AtomicBool>,
    started: Instant,
}

impl Adding {
    fn start(ctx: &egui::Context, urls: Vec<String>) -> Self {
        let names: Vec<String> = urls.iter().map(|u| project::name_from_url(u).unwrap_or_else(|| u.clone())).collect();
        let states = Arc::new(Mutex::new(vec![CloneState::Waiting; urls.len()]));
        let stop = Arc::new(AtomicBool::new(false));
        let next = Arc::new(AtomicUsize::new(0));
        let urls = Arc::new(urls);
        for _ in 0..PARALLEL_CLONES.min(urls.len()) {
            let (states, stop, next, urls, ctx) =
                (states.clone(), stop.clone(), next.clone(), urls.clone(), ctx.clone());
            std::thread::spawn(move || {
                let set = |i: usize, s: CloneState| {
                    states.lock().unwrap_or_else(|e| e.into_inner())[i] = s;
                    ctx.request_repaint();
                };
                loop {
                    let i = next.fetch_add(1, Ordering::SeqCst);
                    let Some(url) = urls.get(i) else { break };
                    if stop.load(Ordering::Relaxed) {
                        set(i, CloneState::Skipped);
                        continue;
                    }
                    let t = Instant::now();
                    set(i, CloneState::Cloning(t));
                    match project::add(url) {
                        Ok(_) => set(i, CloneState::Added(t.elapsed())),
                        Err(e) => set(i, CloneState::Failed(format!("{e:#}"))),
                    }
                }
            });
        }
        Self { names, states, stop, started: Instant::now() }
    }

    fn snapshot(&self) -> Vec<CloneState> {
        self.states.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn finished(&self) -> bool {
        self.snapshot().iter().all(|s| !matches!(s, CloneState::Waiting | CloneState::Cloning(_)))
    }

    fn all_added(&self) -> bool {
        self.snapshot().iter().all(|s| matches!(s, CloneState::Added(_)))
    }

    fn added(&self) -> Vec<String> {
        self.snapshot()
            .iter()
            .zip(&self.names)
            .filter(|(s, _)| matches!(s, CloneState::Added(_)))
            .map(|(_, n)| n.clone())
            .collect()
    }

    fn ui(&self, ui: &mut Ui, p: &theme::Palette) {
        let states = self.snapshot();
        let count = |f: fn(&CloneState) -> bool| states.iter().filter(|s| f(s)).count();
        let added = count(|s| matches!(s, CloneState::Added(_)));
        let failed = count(|s| matches!(s, CloneState::Failed(_)));
        let skipped = count(|s| matches!(s, CloneState::Skipped));
        let running = count(|s| matches!(s, CloneState::Cloning(_)));
        let done = added + failed + skipped;
        let total = states.len();

        let mut summary = if done < total {
            format!("Cloning {} of {total}  ·  {done} done", (done + running).min(total))
        } else {
            format!("Done: {added} added")
        };
        if failed > 0 {
            summary += &format!(", {failed} failed");
        }
        if skipped > 0 {
            summary += &format!(", {skipped} skipped");
        }
        summary += &format!("  ·  {} s", self.started.elapsed().as_secs());
        ui.horizontal(|ui| {
            if done < total {
                ui.spinner();
            }
            ui.label(RichText::new(summary).family(theme::medium()));
        });
        ui.add(egui::ProgressBar::new(done as f32 / total.max(1) as f32).desired_height(4.0));
        ui.add_space(4.0);

        let slow = states.iter().any(|s| matches!(s, CloneState::Cloning(t) if t.elapsed() > Duration::from_secs(30)));
        egui::Frame::new().stroke(egui::Stroke::new(1.0, p.border)).corner_radius(6).inner_margin(6).show(ui, |ui| {
            ScrollArea::vertical().min_scrolled_height(320.0).max_height(420.0).auto_shrink([false, false]).show(
                ui,
                |ui| {
                    for (name, state) in self.names.iter().zip(&states) {
                        ui.horizontal(|ui| {
                            ui.set_min_height(24.0);
                            let (rect, _) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
                            match state {
                                CloneState::Cloning(_) => {
                                    egui::Spinner::new().size(12.0).color(theme::ACCENT).paint_at(ui, rect)
                                }
                                CloneState::Added(_) => {
                                    ui.painter().circle_filled(rect.center(), 4.0, p.ok);
                                }
                                CloneState::Failed(_) => {
                                    ui.painter().circle_filled(rect.center(), 4.0, p.bad);
                                }
                                CloneState::Waiting | CloneState::Skipped => {
                                    ui.painter().circle_stroke(rect.center(), 3.5, egui::Stroke::new(1.0, p.muted));
                                }
                            }
                            ui.label(RichText::new(name).family(theme::medium()));
                            let (text, color) = match state {
                                CloneState::Waiting => ("waiting".to_string(), p.muted),
                                CloneState::Cloning(t) => {
                                    (format!("cloning…  {} s", t.elapsed().as_secs()), theme::ACCENT)
                                }
                                CloneState::Added(d) => (format!("added in {} s", d.as_secs()), p.ok),
                                CloneState::Failed(e) => (e.clone(), p.bad),
                                CloneState::Skipped => ("skipped".to_string(), p.muted),
                            };
                            let r = ui.add(egui::Label::new(RichText::new(&text).size(12.5).color(color)).truncate());
                            if matches!(state, CloneState::Failed(_)) {
                                r.on_hover_text(text);
                            }
                        });
                    }
                },
            );
        });
        if slow {
            ui.label(theme::caption(
                "Big repositories take a while. If nothing finishes, check whether 1Password is waiting for you \
                 to approve the SSH key, and that you can reach GitHub.",
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::split_args;

    /// Each repository gets its own outcome; nothing here reaches the network
    /// (the URLs are rejected before cloning).
    #[test]
    fn adding_reports_each_repository() {
        use super::{Adding, CloneState};
        let urls = vec!["not a url".to_string(), "also not one".to_string(), "ftp://x/".to_string()];
        let adding = Adding::start(&egui::Context::default(), urls);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !adding.finished() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(adding.finished());
        assert!(adding.snapshot().iter().all(|s| matches!(s, CloneState::Failed(_))));
        assert!(!adding.all_added());
        assert!(adding.added().is_empty());
    }

    #[test]
    fn splits_quoted_args() {
        assert_eq!(
            split_args(r#"app exec "php artisan about" -r web"#),
            vec!["app", "exec", "php artisan about", "-r", "web"]
        );
        assert_eq!(split_args("lock  status"), vec!["lock", "status"]);
        assert_eq!(split_args("x ''"), vec!["x", ""]);
    }
}
