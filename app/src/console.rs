//! Bottom panel: a terminal-style view with one tab per kamal run, log
//! stream or interactive session. Tabs belong to a project and only show while that project is open
//! ("All runs" shows every tab).

use crate::terminal::Terminal;
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
    /// An interactive command in a pseudoterminal.
    Terminal,
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
    /// A deploy waiting for a free slot (see `runner::MAX_PARALLEL_DEPLOYS`).
    pub queued: bool,
    /// A finished run read back from history; nothing to cancel.
    pub replay: bool,
    /// The screen of an interactive session (`Kind::Terminal`).
    terminal: Option<Terminal>,
    /// Selected output: where the drag started and where it is now.
    selection: Option<(Pos, Pos)>,
}

/// A place in the output: a line (counted from the very first one, so it
/// survives old lines being dropped) and a character in its plain text.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
struct Pos {
    line: u64,
    col: usize,
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
            queued: false,
            replay: false,
            terminal: None,
            selection: None,
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

    /// The selected text, one output line per line, in the order shown (so
    /// only lines that match the filter).
    fn selected_text(&self, (a, b): (Pos, Pos)) -> String {
        let (a, b) = if a <= b { (a, b) } else { (b, a) };
        let Some(rows) = &self.rows else { return String::new() };
        let mut out = Vec::new();
        for &i in &rows.lines {
            let abs = self.dropped + i as u64;
            if abs < a.line || abs > b.line {
                continue;
            }
            let text = plain(&self.lines[i as usize].1);
            let from = if abs == a.line { a.col } else { 0 };
            let to = if abs == b.line { b.col } else { usize::MAX };
            out.push(text.chars().skip(from).take(to.saturating_sub(from)).collect::<String>());
        }
        out.join("\n")
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
    terminals_opened: u64,
    /// Height of the open terminal, as dragged (0: not dragged yet).
    height: f32,
}

/// Room the floating terminal takes at the bottom of the main area, so
/// scrolling content can make space to come out from under it.
pub fn overlay_space(ctx: &egui::Context) -> f32 {
    ctx.data(|d| d.get_temp::<f32>(overlay_id())).unwrap_or(0.0)
}

fn overlay_id() -> egui::Id {
    egui::Id::unique("console-overlay-space")
}

const RADIUS: u8 = 10;

fn in_scope(s: &Session, scope: Option<&str>) -> bool {
    scope.is_none() || s.project.as_deref() == scope
}

impl Console {
    pub fn open(&mut self, mut session: Session) {
        if self.sessions.iter().any(|s| s.key == session.key) {
            self.focus(&session.key);
            self.collapsed = false;
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

    /// Run an interactive kamal command (`args`, without the unit's `-c`/`-d`)
    /// for one unit in a new terminal tab.
    pub fn open_terminal(
        &mut self,
        ctx: &egui::Context,
        runner: &Runner,
        project: &str,
        unit_id: &str,
        args: Vec<String>,
        title: String,
    ) {
        self.terminals_opened += 1;
        let key = format!("term:{}", self.terminals_opened);
        let mut session = Session::new(key, title, Kind::Terminal, Some(project.to_string()));
        session.units = vec![unit_id.to_string()];
        session.label = args.join(" ");
        let (runner, project, unit_id) = (runner.clone(), project.to_string(), unit_id.to_string());
        session.terminal = Some(Terminal::start(ctx, move || runner.prepare_interactive(&project, &unit_id, &args)));
        self.open(session);
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
            Event::Queued { key, queued } => {
                if let Some(s) = self.sessions.iter_mut().find(|s| s.key == key) {
                    s.queued = queued;
                }
            }
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
            self.collapsed = false;
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

    /// The terminal floats over the bottom of `content` (the main area,
    /// inside its margins, so it lines up with what's on the page) instead
    /// of taking room from it. Drag its top edge to resize it.
    pub fn overlay(&mut self, ctx: &egui::Context, content: egui::Rect, runner: &Runner, scope: Option<&str>) {
        // Gap between the terminal and the content that ends above it.
        const GAP: f32 = 12.0;
        if !self.has_tabs(scope) {
            ctx.data_mut(|d| d.insert_temp(overlay_id(), 0.0f32));
            return;
        }
        let max = (content.height() - GAP - 80.0).max(120.0);
        let height = if self.collapsed {
            36.0
        } else {
            let h = if self.height > 0.0 { self.height } else { 340.0 };
            h.clamp(120.0f32.min(max), max)
        };
        ctx.data_mut(|d| d.insert_temp(overlay_id(), height + GAP));
        let rect =
            egui::Rect::from_min_max(egui::pos2(content.left(), content.bottom() - height), content.right_bottom());
        egui::Area::new(egui::Id::unique("console-overlay"))
            .order(egui::Order::Middle)
            .fixed_pos(rect.min)
            .constrain(false)
            .show(ctx, |ui| {
                let shadow = egui::Shadow { offset: [0, 6], blur: 28, spread: 0, color: Color32::from_black_alpha(70) };
                ui.painter().add(shadow.as_shape(rect, RADIUS));
                ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                    ui.set_min_size(rect.size());
                    ui.set_max_size(rect.size());
                    self.ui(ui, runner, scope);
                });
                if !self.collapsed {
                    let edge = egui::Rect::from_min_max(
                        rect.left_top() - egui::vec2(0.0, 3.0),
                        rect.right_top() + egui::vec2(0.0, 4.0),
                    );
                    let r = ui
                        .interact(edge, ui.scope_id().with("resize"), egui::Sense::drag())
                        .on_hover_cursor(egui::CursorIcon::ResizeVertical);
                    if r.dragged() {
                        self.height = (height - r.drag_delta().y).clamp(120.0f32.min(max), max);
                    }
                }
            });
    }

    pub fn ui(&mut self, ui: &mut Ui, runner: &Runner, scope: Option<&str>) {
        for s in &mut self.sessions {
            if let Some(t) = &s.terminal {
                s.status = t.status();
            }
        }
        *ui.visuals_mut() = terminal_visuals();
        let top = egui::CornerRadius { nw: RADIUS, ne: RADIUS, sw: 0, se: 0 };
        egui::Frame::new().fill(BG).corner_radius(RADIUS).show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            let bar = if self.collapsed { egui::CornerRadius::same(RADIUS) } else { top };
            egui::Frame::new().fill(BAR).corner_radius(bar).inner_margin(Margin::symmetric(8, 5)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                self.toolbar(ui, runner, scope);
            });
            if !self.has_tabs(scope) || self.collapsed {
                return;
            }
            egui::Frame::new().inner_margin(Margin { left: 14, right: 10, top: 8, bottom: 16 }).show(ui, |ui| {
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
            let tools_width = 360.0;
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
                    // Stopping a log stream leaves the container alone: say so, and don't paint it red.
                    let button = match s.kind {
                        Kind::Run => egui::Button::new(RichText::new("Cancel").color(ERROR)),
                        Kind::Terminal => egui::Button::new(RichText::new("Stop").color(ERROR)),
                        Kind::Log => egui::Button::new("Stop following"),
                    };
                    let mut response = ui.add(button);
                    if s.kind == Kind::Log {
                        response = response.on_hover_text("Stop streaming new lines. The container keeps running.");
                    }
                    if response.clicked() {
                        match &s.terminal {
                            Some(t) => t.stop(),
                            None => runner.cancel(&s.key),
                        }
                    }
                }
                if ui.button("Copy").on_hover_text("Copy the whole output").clicked() {
                    let text = match &s.terminal {
                        Some(t) => t.text(),
                        None => s.lines().iter().map(|(_, l)| l.as_str()).collect::<Vec<_>>().join("\n"),
                    };
                    ui.ctx().copy_text(text);
                }
                if s.terminal.is_none() {
                    ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("Filter…").desired_width(150.0));
                }
            });
        });
        if let Some(key) = select {
            self.focus(&key);
            self.collapsed = false;
            if let Some(t) = self.sessions.iter_mut().find(|s| s.key == key).and_then(|s| s.terminal.as_mut()) {
                t.focus();
            }
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
        if let Some(t) = &mut s.terminal {
            t.ui(ui);
            return;
        }
        let font = FontId::monospace(12.5);
        let (row_height, char_width) = ui.fonts_mut(|f| (f.row_height(&font), f.glyph_width(&font, 'M')));
        let row_height = row_pitch(row_height, ui.ctx().pixels_per_point());
        // A filter change reorders what's shown: start the selection over.
        if s.rows.as_ref().is_some_and(|r| r.filter != filter) {
            s.selection = None;
        }
        let mut selection = s.selection;
        let mut copy: Option<String> = None;

        // Only the lines inside the viewport are laid out and drawn; the rest
        // are just numbers in `Rows`. While something is selected the view
        // stops following new output, so the selection stays put.
        ScrollArea::vertical()
            .id_salt(("console-output", &s.key))
            .auto_shrink(false)
            .stick_to_bottom(selection.is_none())
            .show_viewport(ui, |ui, viewport| {
                let width = ui.available_width().max(char_width * 20.0);
                let cols = ((width / char_width).floor() as usize).max(1);
                s.update_rows(&filter, cols, row_height);
                let s = &*s;
                let rows = s.rows.as_ref().unwrap();
                let total = *rows.tops.last().unwrap();
                ui.set_height(total);
                let origin = ui.max_rect().min;
                // A line's galley, and where each of its characters ended up in it.
                let galley_of = |r: usize| {
                    let (stream, line) = &s.lines[rows.lines[r] as usize];
                    let (job, at) = wrap(ansi::layout(line, line_color(*stream, line), font.clone()), cols);
                    (ui.fonts_mut(|f| f.layout_job(job)), at)
                };
                // Pointer position → place in the output.
                let hit = |pos: egui::Pos2| -> Option<Pos> {
                    if rows.lines.is_empty() {
                        return None;
                    }
                    let y = (pos.y - origin.y).clamp(0.0, (total - 1.0).max(0.0));
                    let r = rows.tops.partition_point(|&t| t <= y).saturating_sub(1).min(rows.lines.len() - 1);
                    let (galley, at) = galley_of(r);
                    let local = pos - (origin + egui::vec2(0.0, rows.tops[r]));
                    let col = if pos.y - origin.y >= total {
                        at.len() - 1
                    } else {
                        let g = galley.cursor_from_pos(local).index.0;
                        at.partition_point(|&a| a < g)
                    };
                    Some(Pos { line: s.dropped + rows.lines[r] as u64, col })
                };

                let visible = egui::Rect::from_min_size(origin + viewport.min.to_vec2(), viewport.size());
                let resp = ui.interact(visible, ui.scope_id().with("select"), egui::Sense::click_and_drag());
                let resp = resp.on_hover_cursor(egui::CursorIcon::Text);
                if resp.drag_started_by(egui::PointerButton::Primary)
                    && let Some(p) = ui.input(|i| i.pointer.press_origin()).and_then(hit)
                {
                    selection = Some((p, p));
                    resp.request_focus();
                }
                if resp.dragged_by(egui::PointerButton::Primary)
                    && let Some(ptr) = resp.interact_pointer_pos()
                {
                    if let (Some((anchor, _)), Some(p)) = (selection, hit(ptr)) {
                        selection = Some((anchor, p));
                    }
                    // Past the top or bottom edge: scroll, faster the further out.
                    let out = if ptr.y < visible.top() {
                        visible.top() - ptr.y
                    } else if ptr.y > visible.bottom() {
                        visible.bottom() - ptr.y
                    } else {
                        0.0
                    };
                    if out != 0.0 {
                        ui.scroll_with_delta(egui::vec2(0.0, out.clamp(-48.0, 48.0) * 0.7));
                        ui.ctx().request_repaint();
                    }
                }
                if resp.clicked_by(egui::PointerButton::Primary) {
                    selection = None;
                    resp.request_focus();
                }
                let all = || {
                    let first = *rows.lines.first()?;
                    let last = *rows.lines.last()?;
                    let end = plain(&s.lines[last as usize].1).chars().count();
                    Some((
                        Pos { line: s.dropped + first as u64, col: 0 },
                        Pos { line: s.dropped + last as u64, col: end },
                    ))
                };
                resp.context_menu(|ui| {
                    if ui.add_enabled(selection.is_some_and(|(a, b)| a != b), egui::Button::new("Copy")).clicked() {
                        copy = selection.map(|sel| s.selected_text(sel));
                        ui.close();
                    }
                    if ui.button("Select all").clicked() {
                        selection = all();
                        ui.close();
                    }
                    if ui.add_enabled(selection.is_some(), egui::Button::new("Clear selection")).clicked() {
                        selection = None;
                        ui.close();
                    }
                });
                if resp.has_focus() {
                    ui.input(|i| {
                        for e in &i.events {
                            match e {
                                egui::Event::Copy => {
                                    if let Some(sel) = selection.filter(|(a, b)| a != b) {
                                        copy = Some(s.selected_text(sel));
                                    }
                                }
                                egui::Event::Key { key: egui::Key::A, pressed: true, modifiers, .. }
                                    if modifiers.command =>
                                {
                                    selection = all();
                                }
                                egui::Event::Key { key: egui::Key::Escape, pressed: true, .. } => selection = None,
                                _ => {}
                            }
                        }
                    });
                }

                let sel = selection.map(|(a, b)| if a <= b { (a, b) } else { (b, a) }).filter(|(a, b)| a != b);
                let first = rows.tops.partition_point(|&t| t <= viewport.min.y).saturating_sub(1);
                for r in first..rows.lines.len() {
                    let top = rows.tops[r];
                    if top > viewport.max.y {
                        break;
                    }
                    let (galley, chars) = galley_of(r);
                    let at = origin + egui::vec2(0.0, top);
                    let abs = s.dropped + rows.lines[r] as u64;
                    if let Some((a, b)) = sel.filter(|(a, b)| a.line <= abs && abs <= b.line) {
                        let n = chars.len() - 1;
                        let from = if abs == a.line { a.col.min(n) } else { 0 };
                        let to = if abs == b.line { b.col.min(n) } else { n };
                        // Every cell the selected characters take, without the
                        // breaks `wrap` put in.
                        let text: Vec<char> = galley.text().chars().collect();
                        for g in (chars[from]..chars[to]).filter(|&g| text[g] != '\n') {
                            let c = galley.pos_from_cursor(egui::text::CCursor::new(g));
                            let cell =
                                egui::Rect::from_min_size(at + c.min.to_vec2(), egui::vec2(char_width, c.height()));
                            ui.painter().rect_filled(cell, 0.0, SELECTION);
                        }
                        // A selection that goes on to the next line covers the line end too.
                        if abs < b.line {
                            let c = galley.pos_from_cursor(egui::text::CCursor::new(chars[n]));
                            let cell =
                                egui::Rect::from_min_size(at + c.min.to_vec2(), egui::vec2(char_width, c.height()));
                            ui.painter().rect_filled(cell, 0.0, SELECTION);
                        }
                    }
                    ui.painter().galley(at, galley, TEXT);
                }
            });
        s.selection = selection;
        if let Some(text) = copy.filter(|t| !t.is_empty()) {
            ui.ctx().copy_text(text);
        }
    }
}

/// Distance between wrapped rows as egui lays them out: each row's height is
/// snapped to whole physical pixels, so it isn't the font's row height. Line
/// positions are worked out from this without laying lines out, and a
/// mismatch adds up over a long line until it runs into the next one.
fn row_pitch(row_height: f32, pixels_per_point: f32) -> f32 {
    (row_height * pixels_per_point).round() / pixels_per_point
}

/// Breaks a line into rows of `cols` characters itself, rather than having
/// egui wrap it. egui snaps each glyph to whole pixels along the whole line,
/// so its rows hold one character more or less here and there, and a long
/// line would take more rows than `update_rows` reserved for it and run into
/// the next one. Each row here starts afresh, so each holds exactly `cols`.
///
/// Tabs become four spaces and control characters nothing, as
/// `display_width` counts them. Also returns where each character of the
/// line's plain text ended up in the laid-out text, plus where it ends.
fn wrap(job: egui::text::LayoutJob, cols: usize) -> (egui::text::LayoutJob, Vec<usize>) {
    let mut out = egui::text::LayoutJob::default();
    let mut at = Vec::with_capacity(job.text.len() + 1);
    let (mut g, mut col) = (0, 0);
    for section in &job.sections {
        let mut text = String::new();
        for c in job.text[section.byte_range.start.0..section.byte_range.end.0].chars() {
            let cells = match c {
                '\t' => 4,
                c if c.is_control() => 0,
                _ => 1,
            };
            if cells == 0 {
                at.push(g);
            }
            for cell in 0..cells {
                if col == cols {
                    text.push('\n');
                    (g, col) = (g + 1, 0);
                }
                if cell == 0 {
                    at.push(g);
                }
                text.push(if c == '\t' { ' ' } else { c });
                (g, col) = (g + 1, col + 1);
            }
        }
        out.append(&text, 0.0, section.format.clone());
    }
    at.push(g);
    (out, at)
}

/// Selected output's background.
const SELECTION: Color32 = Color32::from_rgb(0x2f, 0x45, 0x6b);

/// A line's text without ANSI escapes, as laid out (what selections count in).
fn plain(line: &str) -> String {
    ansi::layout(line, TEXT, FontId::monospace(12.5)).text
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
                if ui.add(egui::Label::new(text).selectable(false).sense(egui::Sense::click())).clicked() {
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
        });
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

#[cfg(test)]
mod tests {
    use super::*;

    fn session(lines: &[&str]) -> Session {
        let mut s = Session::new("run:1".into(), "t".into(), Kind::Run, None);
        s.set_lines(lines.iter().map(|l| (Stream::Out, l.to_string())).collect());
        s
    }

    #[test]
    fn copies_the_selected_part_of_each_line() {
        let mut s = session(&["first line", "\x1b[31msecond\x1b[0m line", "third line"]);
        s.update_rows("", 80, 16.0);
        let sel = (Pos { line: 0, col: 6 }, Pos { line: 2, col: 5 });
        assert_eq!(s.selected_text(sel), "line\nsecond line\nthird");
        // Dragging upwards selects the same.
        assert_eq!(s.selected_text((sel.1, sel.0)), "line\nsecond line\nthird");
    }

    #[test]
    fn copies_only_what_the_filter_shows() {
        let mut s = session(&["ERROR one", "info two", "ERROR three"]);
        s.update_rows("error", 80, 16.0);
        let sel = (Pos { line: 0, col: 0 }, Pos { line: 2, col: 11 });
        assert_eq!(s.selected_text(sel), "ERROR one\nERROR three");
    }

    #[test]
    fn opening_a_tab_again_restores_a_minimized_console() {
        let mut c = Console::default();
        c.open(session(&["a"]));
        c.collapsed = true;
        c.open(session(&["a"]));
        assert!(!c.collapsed);
        assert_eq!(c.sessions.len(), 1);
    }

    #[test]
    fn selection_counts_lines_from_the_very_first() {
        let mut s = session(&["a"; MAX_LINES]);
        s.append([(Stream::Out, "last".to_string())]);
        assert_eq!(s.dropped, 1);
        s.update_rows("", 80, 16.0);
        let last = MAX_LINES as u64;
        assert_eq!(s.selected_text((Pos { line: last, col: 0 }, Pos { line: last, col: 4 })), "last");
    }
}

#[cfg(test)]
mod drag_tests {
    use super::*;
    use egui::{Event, PointerButton, Pos2, RawInput, Rect, vec2};
    use std::sync::{Arc, mpsc};

    /// Dragging selects; holding the pointer above the output scrolls up and
    /// extends the selection; Cmd+C copies it.
    #[test]
    fn drag_selects_scrolls_and_copies() {
        let lines: Vec<(Stream, String)> = (0..300).map(|i| (Stream::Out, format!("line {i:03}"))).collect();
        let mut console = Console::default();
        let mut s = Session::new("k".into(), "t".into(), Kind::Run, Some("p".into()));
        s.set_lines(lines);
        s.status = Status::Ok;
        console.open(s);
        let ctx = egui::Context::default();
        crate::theme::install(&ctx);
        let runner = Runner::new(mpsc::channel().0, Arc::new(|| {}));
        let mut copied = Vec::new();
        let mut frame = |console: &mut Console, events: Vec<Event>| {
            let input = RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(900.0, 420.0))),
                events,
                ..Default::default()
            };
            let mut out = ctx.run_ui(input, |ui| console.ui(ui, &runner, Some("p")));
            out.textures_delta.clear();
            for c in out.platform_output.commands {
                if let egui::OutputCommand::CopyText(t) = c {
                    copied.push(t);
                }
            }
        };
        let press = |pos: Pos2, pressed: bool| Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        for _ in 0..3 {
            frame(&mut console, vec![]);
        }
        let (from, to) = (Pos2::new(60.0, 300.0), Pos2::new(120.0, 360.0));
        frame(&mut console, vec![Event::PointerMoved(from), press(from, true)]);
        for i in 1..=5 {
            frame(&mut console, vec![Event::PointerMoved(from + (to - from) * (i as f32 / 5.0))]);
        }
        let sel = console.sessions[0].selection.expect("dragging selects");
        assert!(sel.0 != sel.1);
        let start = sel.0.min(sel.1).line;

        // Above the output: it scrolls up and the selection follows.
        let above = Pos2::new(60.0, 5.0);
        for _ in 0..30 {
            frame(&mut console, vec![Event::PointerMoved(above)]);
        }
        frame(&mut console, vec![press(above, false)]);
        let sel = console.sessions[0].selection.unwrap();
        assert!(sel.0.min(sel.1).line < start, "scrolled up while dragging: {start} → {:?}", sel);

        frame(&mut console, vec![Event::Copy]);
        let text = copied.last().expect("Cmd+C copies");
        // The first line is cut where the drag ended (mid-line); the rest are whole.
        assert!(text.lines().count() > 3 && text.lines().skip(1).all(|l| l.starts_with("line ")), "{text}");
    }
}

#[cfg(test)]
mod layout {
    use super::*;

    /// A long line, as laid out, takes exactly the rows and the height
    /// `update_rows` reserves for it, and fits the width: otherwise it runs
    /// into the next line (or leaves a gap), more so the longer it is.
    #[test]
    fn wrapped_lines_take_the_room_reserved_for_them() {
        let ctx = egui::Context::default();
        crate::theme::install(&ctx);
        let line = "INFO [21b171be] Running docker run\t--detach --name mi-web-workers --env A=\"b\" ".repeat(30);
        for ppp in [1.0, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0] {
            ctx.set_pixels_per_point(ppp);
            let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
                let font = FontId::monospace(12.5);
                let (row_height, char_width) = ui.fonts_mut(|f| (f.row_height(&font), f.glyph_width(&font, 'M')));
                let pitch = row_pitch(row_height, ppp);
                for tenths in 2000..12000 {
                    let width = tenths as f32 / 10.0;
                    let cols = ((width / char_width).floor() as usize).max(1);
                    let rows = (display_width(&line) as usize).div_ceil(cols);
                    let (job, _) = wrap(ansi::layout(&line, TEXT, font.clone()), cols);
                    let galley = ui.fonts_mut(|f| f.layout_job(job));
                    assert_eq!(galley.rows.len(), rows, "rows at {ppp}x, width {width}");
                    let last = galley.rows.last().unwrap().pos.y;
                    assert!((last - (rows - 1) as f32 * pitch).abs() < 0.01, "height at {ppp}x, width {width}");
                    assert!(galley.size().x <= width + 1.0, "too wide at {ppp}x, width {width}");
                }
            });
            out.textures_delta.clear();
        }
    }

    #[test]
    fn wraps_at_the_column_count() {
        let font = FontId::monospace(12.5);
        let (job, at) = wrap(ansi::layout("ab\x1b[31mcd\x1b[0me\tf\rg", TEXT, font), 3);
        assert_eq!(job.text, "abc\nde \n   \nfg");
        // a b c d e tab f \r g, then the end.
        assert_eq!(at, [0, 1, 2, 4, 5, 6, 12, 13, 13, 14]);
    }
}
