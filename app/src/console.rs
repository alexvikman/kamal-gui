//! Bottom panel: a terminal-style view with one tab per kamal run or log
//! stream. Tabs belong to a project and only show while that project is open
//! ("All runs" shows every tab).

use crate::{ansi, theme};
use egui::{Color32, CornerRadius, FontId, Margin, RichText, ScrollArea, Stroke, Ui, Visuals};
use kgui_core::history::Run;
use kgui_core::runner::{self, Event, Runner, Status, Stream};
use std::collections::HashMap;

const MAX_LINES: usize = 50_000;
/// Vertical gap between output lines.
const LINE_GAP: f32 = 1.0;

pub const BG: Color32 = Color32::from_rgb(0x0e, 0x0f, 0x11);
const BAR: Color32 = Color32::from_rgb(0x17, 0x18, 0x1b);
const TAB_ACTIVE: Color32 = Color32::from_rgb(0x2a, 0x2c, 0x31);
pub const TEXT: Color32 = Color32::from_rgb(0xd4, 0xd6, 0xda);
const DIM: Color32 = Color32::from_rgb(0x7f, 0x84, 0x8e);
const INFO: Color32 = Color32::from_rgb(0x61, 0xaf, 0xef);
const ERROR: Color32 = Color32::from_rgb(0xe0, 0x6c, 0x75);
const WARN: Color32 = Color32::from_rgb(0xe5, 0xc0, 0x7b);
const OK: Color32 = Color32::from_rgb(0x98, 0xc3, 0x79);

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Run,
    Log,
}

pub struct Session {
    pub key: String,
    pub title: String,
    pub kind: Kind,
    pub project: Option<String>,
    /// Unit ids a run covers (empty for log streams).
    pub units: Vec<String>,
    /// Short description, e.g. "deploy default".
    pub label: String,
    pub started_at: i64,
    lines: Vec<(Stream, String)>,
    /// Display width of each line in characters (ANSI codes removed).
    widths: Vec<u32>,
    /// Lines dropped from the front so far (keeps memory bounded).
    dropped: u64,
    /// Row positions for the current filter and width (see `update_rows`).
    rows: Option<Rows>,
    pub status: Status,
    /// A finished run read back from history; nothing to cancel.
    pub replay: bool,
}

/// Where each visible output line sits. Built incrementally as lines arrive,
/// rebuilt only when the filter, the width or the font changes. The console
/// font is monospaced and lines wrap at any character, so a line's height
/// follows from its length and nothing has to be laid out to know it.
struct Rows {
    filter: String,
    cols: usize,
    row_height: f32,
    dropped: u64,
    /// Lines considered so far.
    scanned: usize,
    /// Indices into `lines` that match the filter.
    lines: Vec<u32>,
    /// Top of each entry in `lines`, plus the total height at the end.
    tops: Vec<f32>,
}

/// Characters a line occupies on screen: ANSI escapes take none, tabs four.
fn display_width(line: &str) -> u32 {
    let mut n = 0u32;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => {
                // Skip a CSI sequence: ESC [ params final-byte.
                if chars.next() == Some('[') {
                    for c in chars.by_ref() {
                        if c.is_ascii_alphabetic() {
                            break;
                        }
                    }
                }
            }
            '\t' => n += 4,
            c if c.is_control() => {}
            _ => n += 1,
        }
    }
    n
}

impl Session {
    pub fn new(key: String, title: String, kind: Kind, project: Option<String>) -> Self {
        Self {
            key,
            title,
            kind,
            project,
            units: Vec::new(),
            label: String::new(),
            started_at: chrono::Utc::now().timestamp_millis(),
            lines: Vec::new(),
            widths: Vec::new(),
            dropped: 0,
            rows: None,
            status: Status::Running,
            replay: false,
        }
    }

    pub fn is_live_run(&self) -> bool {
        self.kind == Kind::Run && self.status == Status::Running && !self.replay
    }

    pub fn lines(&self) -> &[(Stream, String)] {
        &self.lines
    }

    pub fn set_lines(&mut self, lines: Vec<(Stream, String)>) {
        self.lines.clear();
        self.widths.clear();
        self.rows = None;
        self.append(lines);
    }

    fn append(&mut self, batch: impl IntoIterator<Item = (Stream, String)>) {
        for (stream, line) in batch {
            self.widths.push(display_width(&line));
            self.lines.push((stream, line));
        }
        if self.lines.len() > MAX_LINES {
            let drop = self.lines.len() - MAX_LINES;
            self.lines.drain(..drop);
            self.widths.drain(..drop);
            self.dropped += drop as u64;
        }
    }

    fn update_rows(&mut self, filter: &str, cols: usize, row_height: f32) -> &Rows {
        let stale = self.rows.as_ref().is_none_or(|r| {
            r.filter != filter || r.cols != cols || r.row_height != row_height || r.dropped != self.dropped
        });
        if stale {
            self.rows = Some(Rows {
                filter: filter.to_string(),
                cols,
                row_height,
                dropped: self.dropped,
                scanned: 0,
                lines: Vec::new(),
                tops: vec![0.0],
            });
        }
        let rows = self.rows.as_mut().unwrap();
        for i in rows.scanned..self.lines.len() {
            if !filter.is_empty() && !self.lines[i].1.to_lowercase().contains(filter) {
                continue;
            }
            let wrapped = (self.widths[i] as usize).div_ceil(cols).max(1);
            let top = *rows.tops.last().unwrap();
            rows.lines.push(i as u32);
            rows.tops.push(top + wrapped as f32 * row_height + LINE_GAP);
        }
        rows.scanned = self.lines.len();
        rows
    }
}

#[derive(Default)]
pub struct Console {
    pub sessions: Vec<Session>,
    /// Minimized to just the tab bar (tabs and runs keep going).
    pub collapsed: bool,
    /// Active tab per project (None = the "All runs" view).
    active: HashMap<Option<String>, String>,
    filter: String,
    /// Lines for sessions that aren't open (yet).
    pending: HashMap<String, Vec<(Stream, String)>>,
    pending_end: HashMap<String, Status>,
}

fn in_scope(s: &Session, scope: Option<&str>) -> bool {
    scope.is_none() || s.project.as_deref() == scope
}

impl Console {
    pub fn open(&mut self, mut session: Session) {
        if self.sessions.iter().any(|s| s.key == session.key) {
            self.focus(&session.key);
            return;
        }
        if let Some(early) = self.pending.remove(&session.key) {
            session.append(early);
        }
        if let Some(status) = self.pending_end.remove(&session.key) {
            session.status = status;
        }
        let key = session.key.clone();
        self.sessions.push(session);
        self.focus(&key);
        self.collapsed = false;
    }

    /// Make `key` the active tab in its project and in "All runs".
    pub fn focus(&mut self, key: &str) -> bool {
        let Some(s) = self.sessions.iter().find(|s| s.key == key) else { return false };
        self.active.insert(s.project.clone(), key.to_string());
        self.active.insert(None, key.to_string());
        true
    }

    pub fn has_tabs(&self, scope: Option<&str>) -> bool {
        self.sessions.iter().any(|s| in_scope(s, scope))
    }

    /// Apply a runner event. Returns the run when its history row changed.
    pub fn handle(&mut self, event: Event) -> Option<Run> {
        match event {
            Event::Line { key, stream, line } => match self.sessions.iter_mut().find(|s| s.key == key) {
                Some(s) => s.append([(stream, line)]),
                None => self.pending.entry(key).or_default().push((stream, line)),
            },
            Event::End { key, status } => match self.sessions.iter_mut().find(|s| s.key == key) {
                Some(s) => s.status = status,
                None => {
                    self.pending_end.insert(key, status);
                }
            },
            Event::RunUpdated(run) => return Some(run),
        }
        None
    }

    pub fn running_projects(&self) -> Vec<&str> {
        self.sessions.iter().filter(|s| s.is_live_run()).filter_map(|s| s.project.as_deref()).collect()
    }

    /// The kamal run currently going on in a project, if any.
    pub fn running_in(&self, project: &str) -> Option<&Session> {
        self.sessions.iter().find(|s| s.is_live_run() && s.project.as_deref() == Some(project))
    }

    pub fn any_running(&self) -> bool {
        self.sessions.iter().any(|s| s.status == Status::Running && !s.replay)
    }

    pub fn open_run_log(&mut self, run: &Run) {
        if self.focus(&runner::run_key(run.id)) {
            return;
        }
        let mut s = Session::new(
            format!("history:{}", run.id),
            format!("#{} {}", run.id, run.action),
            Kind::Run,
            Some(run.project.clone()),
        );
        s.replay = true;
        s.started_at = run.started_at;
        s.set_lines(runner::read_log(run.id).unwrap_or_else(|e| vec![(Stream::Err, format!("{e:#}"))]));
        s.status = match Status::from_run(&run.status) {
            Status::Running => Status::Interrupted,
            other => other,
        };
        self.open(s);
    }

    pub fn ui(&mut self, ui: &mut Ui, runner: &Runner, scope: Option<&str>) {
        *ui.visuals_mut() = terminal_visuals();
        egui::Frame::new().fill(BG).show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            egui::Frame::new().fill(BAR).inner_margin(Margin::symmetric(8, 5)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                self.toolbar(ui, runner, scope);
            });
            if !self.has_tabs(scope) || self.collapsed {
                return;
            }
            egui::Frame::new().inner_margin(Margin { left: 12, right: 8, top: 6, bottom: 4 }).show(ui, |ui| {
                self.output(ui, scope);
            });
        });
    }

    fn active_key(&self, scope: Option<&str>) -> Option<String> {
        let wanted = self.active.get(&scope.map(String::from));
        let visible = |k: &String| self.sessions.iter().any(|s| &s.key == k && in_scope(s, scope));
        wanted
            .filter(|k| visible(k))
            .cloned()
            .or_else(|| self.sessions.iter().rev().find(|s| in_scope(s, scope)).map(|s| s.key.clone()))
    }

    fn toolbar(&mut self, ui: &mut Ui, runner: &Runner, scope: Option<&str>) {
        let active = self.active_key(scope);
        let mut close = None;
        let mut select = None;
        ui.horizontal(|ui| {
            if !self.has_tabs(scope) {
                ui.label(RichText::new("Deploys and logs for this project show up here.").color(DIM));
                return;
            }
            let tools_width = 300.0;
            ScrollArea::horizontal()
                .max_width((ui.available_width() - tools_width).max(120.0))
                .id_salt("console-tabs")
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        for s in self.sessions.iter().filter(|s| in_scope(s, scope)) {
                            let is_active = active.as_deref() == Some(&s.key);
                            let title = if scope.is_none() {
                                format!("{} · {}", s.project.as_deref().unwrap_or(""), s.title)
                            } else {
                                s.title.clone()
                            };
                            match tab(ui, &title, s.status, is_active) {
                                TabClick::Select => select = Some(s.key.clone()),
                                TabClick::Close => close = Some(s.key.clone()),
                                TabClick::None => {}
                            }
                        }
                    });
                });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let hint = if self.collapsed { "Restore terminal" } else { "Minimize terminal" };
                if chevron_button(ui, self.collapsed).on_hover_text(hint).clicked() {
                    self.collapsed = !self.collapsed;
                }
                ui.add_space(2.0);
                let Some(s) = active.as_ref().and_then(|k| self.sessions.iter().find(|s| &s.key == k)) else { return };
                if s.status == Status::Running && !s.replay {
                    let label = if s.kind == Kind::Log { "Stop" } else { "Cancel" };
                    if ui.add(egui::Button::new(RichText::new(label).color(ERROR))).clicked() {
                        runner.cancel(&s.key);
                    }
                }
                if ui.button("Copy").on_hover_text("Copy the whole output").clicked() {
                    let text = s.lines().iter().map(|(_, l)| l.as_str()).collect::<Vec<_>>().join("\n");
                    ui.ctx().copy_text(text);
                }
                ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("Filter…").desired_width(150.0));
            });
        });
        if let Some(key) = select {
            self.focus(&key);
            self.collapsed = false;
        }
        if let Some(key) = close {
            if let Some(s) = self.sessions.iter().find(|s| s.key == key)
                && s.kind == Kind::Log
                && s.status == Status::Running
            {
                runner.cancel(&key);
            }
            self.sessions.retain(|s| s.key != key);
            self.active.retain(|_, k| k != &key);
        }
    }

    fn output(&mut self, ui: &mut Ui, scope: Option<&str>) {
        let Some(key) = self.active_key(scope) else { return };
        let filter = self.filter.trim().to_lowercase();
        let Some(s) = self.sessions.iter_mut().find(|s| s.key == key) else { return };
        let font = FontId::monospace(12.5);
        let (row_height, char_width) = ui.fonts_mut(|f| (f.row_height(&font), f.glyph_width(&font, 'M')));

        // Only the lines inside the viewport are laid out and drawn; the rest
        // are just numbers in `Rows`.
        ScrollArea::vertical()
            .id_salt(("console-output", &s.key))
            .auto_shrink(false)
            .stick_to_bottom(true)
            .show_viewport(ui, |ui, viewport| {
                let width = ui.available_width().max(char_width * 20.0);
                let cols = ((width / char_width).floor() as usize).max(1);
                s.update_rows(&filter, cols, row_height);
                let rows = s.rows.as_ref().unwrap();
                let total = *rows.tops.last().unwrap();
                ui.set_height(total);
                let origin = ui.max_rect().min;
                let first = rows.tops.partition_point(|&t| t <= viewport.min.y).saturating_sub(1);
                for r in first..rows.lines.len() {
                    let top = rows.tops[r];
                    if top > viewport.max.y {
                        break;
                    }
                    let (stream, line) = &s.lines[rows.lines[r] as usize];
                    let mut job = ansi::layout(line, line_color(*stream, line), font.clone());
                    job.wrap.max_width = width;
                    job.wrap.break_anywhere = true;
                    let galley = ui.fonts_mut(|f| f.layout_job(job));
                    let rect = egui::Rect::from_min_size(origin + egui::vec2(0.0, top), egui::vec2(width, rows.tops[r + 1] - top));
                    ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                        ui.add(egui::Label::new(galley));
                    });
                }
            });
    }
}

/// Base color for a line: the app's own messages in blue; kamal's log levels
/// dimmed (DEBUG), highlighted (ERROR/WARN) or plain.
fn line_color(stream: Stream, line: &str) -> Color32 {
    if stream == Stream::Info {
        return if line.starts_with('✓') {
            OK
        } else if line.starts_with('✗') {
            ERROR
        } else {
            INFO
        };
    }
    let t = line.trim_start();
    if t.starts_with("DEBUG") {
        DIM
    } else if t.starts_with("ERROR") || t.starts_with("FATAL") || t.contains("Error:") {
        ERROR
    } else if t.starts_with("WARN") {
        WARN
    } else {
        TEXT
    }
}

enum TabClick {
    None,
    Select,
    Close,
}

/// A tab: status dot, title and a close button inside one rounded shape.
fn tab(ui: &mut Ui, title: &str, status: Status, active: bool) -> TabClick {
    let mut click = TabClick::None;
    // No click handler on the whole tab: in egui the widget registered last
    // wins the hit test, so a tab-wide one would swallow the close button.
    egui::Frame::new()
        .fill(if active { TAB_ACTIVE } else { Color32::TRANSPARENT })
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin { left: 9, right: 4, top: 3, bottom: 3 })
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                theme::dot(ui, status_dot(status));
                let text = RichText::new(title).color(if active { Color32::WHITE } else { DIM });
                if ui.add(egui::Label::new(text).sense(egui::Sense::click())).clicked() {
                    click = TabClick::Select;
                }
                let (rect, x) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::click());
                if x.hovered() {
                    ui.painter().rect_filled(rect, CornerRadius::same(4), Color32::from_white_alpha(24));
                }
                let c = rect.center();
                let d = 3.5;
                let stroke = Stroke::new(1.3, if x.hovered() || active { TEXT } else { DIM });
                ui.painter().line_segment([c + egui::vec2(-d, -d), c + egui::vec2(d, d)], stroke);
                ui.painter().line_segment([c + egui::vec2(-d, d), c + egui::vec2(d, -d)], stroke);
                if x.on_hover_text("Close").clicked() {
                    click = TabClick::Close;
                }
            });
        })
        ;
    click
}

/// Chevron pointing down (minimize) or up (restore).
fn chevron_button(ui: &mut Ui, up: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(26.0, 24.0), egui::Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(rect, CornerRadius::same(6), Color32::from_white_alpha(20));
    }
    let c = rect.center();
    let (w, h) = (5.0, 2.5);
    let dy = if up { -h } else { h };
    let stroke = Stroke::new(1.5, if resp.hovered() { Color32::WHITE } else { TEXT });
    ui.painter().line_segment([c + egui::vec2(-w, -dy), c + egui::vec2(0.0, dy)], stroke);
    ui.painter().line_segment([c + egui::vec2(0.0, dy), c + egui::vec2(w, -dy)], stroke);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn status_dot(status: Status) -> Color32 {
    match status {
        Status::Running => INFO,
        Status::Ok => OK,
        Status::Failed => ERROR,
        Status::Cancelled | Status::Interrupted => WARN,
    }
}

fn terminal_visuals() -> Visuals {
    let mut v = Visuals::dark();
    v.override_text_color = Some(TEXT);
    v.extreme_bg_color = Color32::from_rgb(0x1c, 0x1d, 0x21);
    v.selection.bg_fill = Color32::from_rgb(0x2f, 0x45, 0x6b);
    v.selection.stroke = Stroke::new(1.0, Color32::WHITE);
    for w in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
        w.corner_radius = CornerRadius::same(6);
        w.bg_stroke = Stroke::NONE;
    }
    v.widgets.inactive.weak_bg_fill = Color32::from_rgb(0x26, 0x28, 0x2d);
    v.widgets.inactive.bg_fill = Color32::from_rgb(0x26, 0x28, 0x2d);
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(0x31, 0x33, 0x39);
    v.widgets.hovered.bg_fill = Color32::from_rgb(0x31, 0x33, 0x39);
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(0x2e, 0x30, 0x36));
    v
}

/// Status colors for light UI elements (history table, etc.).
pub fn status_color(p: &theme::Palette, status: Status) -> Color32 {
    match status {
        Status::Running => theme::ACCENT,
        Status::Ok => p.ok,
        Status::Failed => p.bad,
        Status::Cancelled | Status::Interrupted => p.warn,
    }
}

#[cfg(test)]
mod bench {
    //! `KGUI_BENCH_RUN=2 cargo test --release -p kamal-gui bench -- --ignored --nocapture`
    use super::*;
    use std::sync::{Arc, mpsc};
    use std::time::Instant;

    #[test]
    #[ignore]
    fn console_frame_cost() {
        let id: i64 = std::env::var("KGUI_BENCH_RUN").ok().and_then(|v| v.parse().ok()).unwrap_or(2);
        let lines = runner::read_log(id).expect("run log");
        let n_lines = lines.len();
        let mut console = Console::default();
        let mut s = Session::new("k".into(), "#bench".into(), Kind::Run, Some("p".into()));
        s.set_lines(lines);
        s.status = Status::Ok;
        console.open(s);
        let ctx = egui::Context::default();
        crate::theme::install(&ctx);
        let runner = Runner::new(mpsc::channel().0, Arc::new(|| {}));
        let input = || egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1400.0, 420.0))),
            ..Default::default()
        };
        let frame = |console: &mut Console| {
            let mut out = ctx.run_ui(input(), |ui| console.ui(ui, &runner, Some("p")));
            out.textures_delta.clear();
            let _ = ctx.tessellate(out.shapes, out.pixels_per_point);
        };
        let t = Instant::now();
        frame(&mut console);
        let first = t.elapsed();
        let n = 30;
        let t = Instant::now();
        for _ in 0..n {
            frame(&mut console);
        }
        println!("run #{id}: {n_lines} lines  first frame {first:?}  steady frame {:?}", t.elapsed() / n);
        console.filter = "docker".into();
        let t = Instant::now();
        for _ in 0..n {
            frame(&mut console);
        }
        println!("  with filter: steady frame {:?}", t.elapsed() / n);
    }
}
