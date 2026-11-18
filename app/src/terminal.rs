//! A small terminal emulator for interactive kamal commands (`app exec -i`,
//! aliases like `console`, a shell in an accessory). The command runs in a
//! pseudoterminal; its output goes through a VT100/xterm parser and the
//! screen is drawn cell by cell. Keys go straight to the process.

use crate::ansi;
use crate::console::{BG, TEXT};
use egui::{Align2, Color32, CornerRadius, EventFilter, FontId, Key, Modifiers, Rect, Sense, Stroke, Ui, pos2, vec2};
use kgui_core::runner::{CommandSpec, Status};
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::io::{Read, Write};
use std::num::NonZeroU16;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

const SCROLLBACK: usize = 10_000;
const FONT_SIZE: f32 = 12.5;
const SELECTION: Color32 = Color32::from_rgba_premultiplied(0x2f, 0x45, 0x6b, 0xff);

struct Pty {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
}

struct Shared {
    parser: Mutex<vt100::Parser>,
    pty: Mutex<Option<Pty>>,
    status: Mutex<Status>,
    /// Stopped or closed by the user.
    stopped: AtomicBool,
}

impl Shared {
    fn print(&self, ctx: &egui::Context, text: &str) {
        self.parser.lock().unwrap().process(text.replace('\n', "\r\n").as_bytes());
        ctx.request_repaint();
    }
}

/// A cell on the visible screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Pos {
    row: u16,
    col: u16,
}

pub struct Terminal {
    shared: Arc<Shared>,
    /// Wheel movement not yet turned into whole scrollback rows.
    scroll_rest: f32,
    /// Mouse selection on the visible screen (anchor, end).
    selection: Option<(Pos, Pos)>,
    /// Take keyboard focus on the next frame.
    focus_next: bool,
}

impl Terminal {
    /// Run `prepare` (checkout, building the command) on a background
    /// thread, then the command in a pseudoterminal.
    pub fn start(ctx: &egui::Context, prepare: impl FnOnce() -> anyhow::Result<CommandSpec> + Send + 'static) -> Self {
        let size = NonZeroU16::new(24).unwrap();
        let cols = NonZeroU16::new(100).unwrap();
        let shared = Arc::new(Shared {
            parser: Mutex::new(vt100::Parser::new(size, cols, SCROLLBACK)),
            pty: Mutex::new(None),
            status: Mutex::new(Status::Running),
            stopped: AtomicBool::new(false),
        });
        let s = shared.clone();
        let ctx = ctx.clone();
        thread::spawn(move || {
            s.print(&ctx, "\x1b[34m› Checking out the deployed commit…\x1b[0m\n");
            let status = match prepare().and_then(|spec| run(&s, &ctx, spec)) {
                Ok(status) => status,
                Err(e) => {
                    s.print(&ctx, &format!("\x1b[31m{e:#}\x1b[0m\n"));
                    Status::Failed
                }
            };
            *s.status.lock().unwrap() = status;
            ctx.request_repaint();
        });
        Self { shared, scroll_rest: 0.0, selection: None, focus_next: true }
    }

    pub fn status(&self) -> Status {
        *self.shared.status.lock().unwrap()
    }

    /// Hang up on the process (SIGHUP, as when a terminal window closes).
    pub fn stop(&self) {
        self.shared.stopped.store(true, Ordering::SeqCst);
        if let Some(pty) = self.shared.pty.lock().unwrap().as_mut() {
            let _ = pty.killer.kill();
        }
    }

    pub fn focus(&mut self) {
        self.focus_next = true;
    }

    /// Everything in the scrollback and on screen, as plain text.
    pub fn text(&self) -> String {
        let mut parser = self.shared.parser.lock().unwrap();
        let screen = parser.screen_mut();
        let offset = screen.scrollback();
        let (rows, cols) = screen.size();
        let back = screen.scrollback_len();
        let total = back + rows.get() as usize;
        // vt100 only reads out the visible rows, so page through the
        // scrollback: at offset k the first visible line is `back - k`.
        let mut lines = Vec::with_capacity(total);
        while lines.len() < total {
            let k = back - lines.len().min(back);
            screen.set_scrollback(k);
            let first = back - k;
            let before = lines.len();
            for (j, row) in screen.rows(0, cols.get()).enumerate() {
                if first + j == lines.len() {
                    lines.push(row);
                }
            }
            if lines.len() == before {
                break;
            }
        }
        screen.set_scrollback(offset);
        let mut text = lines.join("\n");
        text.truncate(text.trim_end().len());
        text
    }

    fn send(&mut self, bytes: &[u8]) {
        if let Some(pty) = self.shared.pty.lock().unwrap().as_mut() {
            let _ = pty.writer.write_all(bytes).and_then(|()| pty.writer.flush());
        }
    }

    fn resize(&self, rows: u16, cols: u16) {
        let mut parser = self.shared.parser.lock().unwrap();
        let (r, c) = parser.screen().size();
        if (r.get(), c.get()) == (rows, cols) {
            return;
        }
        parser.set_size(NonZeroU16::new(rows).unwrap(), NonZeroU16::new(cols).unwrap());
        if let Some(pty) = self.shared.pty.lock().unwrap().as_ref() {
            let _ = pty.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
        }
    }

    pub fn ui(&mut self, ui: &mut Ui) {
        let font = FontId::monospace(FONT_SIZE);
        let (row_h, cell_w) = ui.fonts_mut(|f| (f.row_height(&font), f.glyph_width(&font, 'M')));
        let (rect, resp) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        let rows = ((rect.height() / row_h).floor() as u16).max(2);
        let cols = ((rect.width() / cell_w).floor() as u16).max(10);
        self.resize(rows, cols);

        if resp.clicked() || resp.drag_started() || std::mem::take(&mut self.focus_next) {
            resp.request_focus();
        }
        let focused = resp.has_focus();
        if focused {
            let all = EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true };
            ui.memory_mut(|m| m.set_focus_lock_filter(resp.id, all));
        }
        resp.clone().on_hover_cursor(egui::CursorIcon::Text);

        let cell_at = |p: egui::Pos2| Pos {
            row: (((p.y - rect.top()) / row_h).floor().max(0.0) as u16).min(rows - 1),
            col: (((p.x - rect.left()) / cell_w).round().max(0.0) as u16).min(cols),
        };
        if resp.drag_started()
            && let Some(p) = resp.interact_pointer_pos()
        {
            let at = cell_at(p);
            self.selection = Some((at, at));
        } else if resp.dragged()
            && let (Some(p), Some((_, end))) = (resp.interact_pointer_pos(), self.selection.as_mut())
        {
            *end = cell_at(p);
        } else if resp.clicked() {
            self.selection = None;
        }

        if resp.hovered() {
            let dy = ui.input(|i| i.smooth_scroll_delta.y) / row_h + self.scroll_rest;
            let whole = dy.trunc();
            self.scroll_rest = dy - whole;
            if whole != 0.0 {
                let mut parser = self.shared.parser.lock().unwrap();
                let screen = parser.screen_mut();
                if !screen.alternate_screen() {
                    let target = (screen.scrollback() as i64 + whole as i64).max(0) as usize;
                    screen.set_scrollback(target);
                    self.selection = None;
                }
            }
        }

        if focused {
            self.input(ui);
        }
        self.paint(ui, rect, row_h, cell_w, &font, focused);
    }

    fn input(&mut self, ui: &mut Ui) {
        let (app_cursor, bracketed) = {
            let parser = self.shared.parser.lock().unwrap();
            (parser.screen().application_cursor(), parser.screen().bracketed_paste())
        };
        let events = ui.input(|i| i.events.clone());
        let mut out: Vec<u8> = Vec::new();
        for event in events {
            match event {
                egui::Event::Text(t) => out.extend(t.as_bytes()),
                egui::Event::Paste(t) => {
                    let t = t.replace("\r\n", "\r").replace('\n', "\r");
                    if bracketed {
                        out.extend(b"\x1b[200~");
                        out.extend(t.as_bytes());
                        out.extend(b"\x1b[201~");
                    } else {
                        out.extend(t.as_bytes());
                    }
                }
                egui::Event::Copy => {
                    if let Some(text) = self.selected_text() {
                        ui.ctx().copy_text(text);
                    }
                }
                egui::Event::Key { key, pressed: true, modifiers, .. } => {
                    if let Some(bytes) = key_bytes(key, modifiers, app_cursor) {
                        out.extend(bytes);
                    }
                }
                _ => {}
            }
        }
        if !out.is_empty() {
            self.selection = None;
            self.shared.parser.lock().unwrap().screen_mut().set_scrollback(0);
            self.send(&out);
        }
    }

    fn selected_text(&self) -> Option<String> {
        let (a, b) = self.selection?;
        let (start, end) = if (a.row, a.col) <= (b.row, b.col) { (a, b) } else { (b, a) };
        if start == end {
            return None;
        }
        let parser = self.shared.parser.lock().unwrap();
        Some(parser.screen().contents_between(start.row, start.col, end.row, end.col))
    }

    fn paint(&self, ui: &Ui, rect: Rect, row_h: f32, cell_w: f32, font: &FontId, focused: bool) {
        let painter = ui.painter_at(rect);
        let parser = self.shared.parser.lock().unwrap();
        let screen = parser.screen();
        let (rows, cols) = screen.size();
        let (rows, cols) = (rows.get(), cols.get());
        let cell_rect = |row: u16, col: u16, n: u16| {
            Rect::from_min_size(
                pos2(rect.left() + col as f32 * cell_w, rect.top() + row as f32 * row_h),
                vec2(n as f32 * cell_w, row_h),
            )
        };
        let selected = self.selection.map(|(a, b)| if (a.row, a.col) <= (b.row, b.col) { (a, b) } else { (b, a) });
        let is_selected = |row: u16, col: u16| {
            selected.is_some_and(|(s, e)| (row, col) >= (s.row, s.col) && (row, col) < (e.row, e.col))
        };

        for row in 0..rows {
            let mut col = 0;
            while col < cols {
                let Some(cell) = screen.cell(row, col) else { break };
                let style = Style::of(cell);
                let start = col;
                let mut text = String::new();
                while col < cols {
                    let Some(cell) = screen.cell(row, col) else { break };
                    if Style::of(cell) != style || (cell.is_wide() && !text.is_empty()) {
                        break;
                    }
                    let c = cell.contents();
                    text.push_str(if c.is_empty() { " " } else { c });
                    col += if cell.is_wide() { 2 } else { 1 };
                    if cell.is_wide() {
                        break;
                    }
                }
                if col == start {
                    col += 1;
                    continue;
                }
                let (fg, bg) = style.colors();
                if bg != BG {
                    painter.rect_filled(cell_rect(row, start, col - start), CornerRadius::ZERO, bg);
                }
                for c in start..col {
                    if is_selected(row, c) {
                        painter.rect_filled(cell_rect(row, c, 1), CornerRadius::ZERO, SELECTION);
                    }
                }
                if !text.trim().is_empty() {
                    let at = cell_rect(row, start, 1).min;
                    painter.text(at, Align2::LEFT_TOP, &text, font.clone(), fg);
                    if style.underline {
                        let y = at.y + row_h - 2.0;
                        let x2 = at.x + (col - start) as f32 * cell_w;
                        painter.line_segment([pos2(at.x, y), pos2(x2, y)], Stroke::new(1.0, fg));
                    }
                }
            }
        }

        let running = *self.shared.status.lock().unwrap() == Status::Running;
        if running && screen.scrollback() == 0 && !screen.hide_cursor() {
            let (row, col) = screen.cursor_position();
            let r = cell_rect(row, col, 1);
            if focused {
                painter.rect_filled(r, CornerRadius::ZERO, TEXT.gamma_multiply(0.55));
            } else {
                painter.rect_stroke(
                    r.shrink(0.5),
                    CornerRadius::ZERO,
                    Stroke::new(1.0, TEXT),
                    egui::StrokeKind::Inside,
                );
            }
        }
        if screen.scrollback() > 0 {
            let label = format!("↑ {} lines", screen.scrollback());
            let at = rect.right_top() + vec2(-8.0, 4.0);
            painter.text(at, Align2::RIGHT_TOP, label, FontId::proportional(11.5), TEXT.gamma_multiply(0.6));
        }
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Spawn the command in a pseudoterminal and pump its output into the parser
/// until it exits.
fn run(s: &Shared, ctx: &egui::Context, spec: CommandSpec) -> anyhow::Result<Status> {
    if s.stopped.load(Ordering::SeqCst) {
        return Ok(Status::Cancelled);
    }
    s.print(ctx, &format!("\x1b[2m{}\x1b[0m\n\n", spec.summary));
    let (rows, cols) = s.parser.lock().unwrap().screen().size();
    let pair = native_pty_system()
        .openpty(PtySize { rows: rows.get(), cols: cols.get(), pixel_width: 0, pixel_height: 0 })
        .map_err(|e| anyhow::anyhow!("could not open a terminal: {e}"))?;
    let mut cmd = CommandBuilder::new(&spec.program);
    cmd.args(&spec.args);
    cmd.cwd(&spec.cwd);
    for (k, v) in &spec.envs {
        cmd.env(k, v);
    }
    let mut child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| anyhow::anyhow!("could not start {}: {e}", spec.program.display()))?;
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().map_err(|e| anyhow::anyhow!("{e}"))?;
    let writer = pair.master.take_writer().map_err(|e| anyhow::anyhow!("{e}"))?;
    *s.pty.lock().unwrap() = Some(Pty { master: pair.master, writer, killer: child.clone_killer() });
    if s.stopped.load(Ordering::SeqCst) {
        let _ = child.kill();
    }

    let mut buf = [0u8; 16 * 1024];
    loop {
        match reader.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                s.parser.lock().unwrap().process(&buf[..n]);
                ctx.request_repaint();
            }
        }
    }
    let exit = child.wait()?;
    *s.pty.lock().unwrap() = None;
    let stopped = s.stopped.load(Ordering::SeqCst);
    let (status, note) = match (stopped, exit.success()) {
        (true, _) => (Status::Cancelled, "Stopped".to_string()),
        (false, true) => (Status::Ok, "Exited".to_string()),
        (false, false) => (Status::Failed, format!("Exited with code {}", exit.exit_code())),
    };
    s.print(ctx, &format!("\n\x1b[2m[{note}]\x1b[0m"));
    Ok(status)
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Style {
    fg: vt100::Color,
    bg: vt100::Color,
    bold: bool,
    dim: bool,
    underline: bool,
    inverse: bool,
}

impl Style {
    fn of(cell: &vt100::Cell) -> Self {
        Self {
            fg: cell.fgcolor(),
            bg: cell.bgcolor(),
            bold: cell.bold(),
            dim: cell.dim(),
            underline: cell.underline(),
            inverse: cell.inverse(),
        }
    }

    fn colors(self) -> (Color32, Color32) {
        let mut fg = match self.fg {
            vt100::Color::Default if self.bold => Color32::WHITE,
            // Bold text in one of the first 8 colors shows the bright variant.
            vt100::Color::Idx(i) if self.bold && i < 8 => ansi::BRIGHT[i as usize],
            c => color(c, TEXT),
        };
        let mut bg = color(self.bg, BG);
        if self.inverse {
            std::mem::swap(&mut fg, &mut bg);
        }
        if self.dim {
            fg = fg.gamma_multiply(0.6);
        }
        (fg, bg)
    }
}

/// xterm's 256 colors, with the console's own palette for the first 16.
fn color(c: vt100::Color, default: Color32) -> Color32 {
    match c {
        vt100::Color::Default => default,
        vt100::Color::Rgb(r, g, b) => Color32::from_rgb(r, g, b),
        vt100::Color::Idx(i @ 0..8) => ansi::COLORS[i as usize],
        vt100::Color::Idx(i @ 8..16) => ansi::BRIGHT[i as usize - 8],
        vt100::Color::Idx(i @ 16..232) => {
            let i = i - 16;
            let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            Color32::from_rgb(level(i / 36), level((i / 6) % 6), level(i % 6))
        }
        vt100::Color::Idx(i) => {
            let v = 8 + (i - 232) * 10;
            Color32::from_rgb(v, v, v)
        }
    }
}

/// Bytes a key sends, as in xterm. Text keys arrive as `Event::Text`; this
/// covers control keys, Ctrl+letter and the navigation keys. Cmd shortcuts
/// stay with the app.
fn key_bytes(key: Key, m: Modifiers, app_cursor: bool) -> Option<Vec<u8>> {
    if m.mac_cmd {
        return None;
    }
    if m.ctrl {
        let name = key.name();
        if name.len() == 1 && name.as_bytes()[0].is_ascii_uppercase() {
            return Some(vec![name.as_bytes()[0] - b'@']);
        }
        let b = match key {
            Key::OpenBracket => 0x1b,
            Key::Backslash => 0x1c,
            Key::CloseBracket => 0x1d,
            Key::Space => 0x00,
            _ => 0,
        };
        if b != 0 || key == Key::Space {
            return Some(vec![b]);
        }
    }
    // xterm modifier parameter: 1 + shift + 2·alt + 4·ctrl.
    let modifier = 1 + m.shift as u8 + 2 * m.alt as u8 + 4 * m.ctrl as u8;
    let cursor = |c: char| {
        if modifier > 1 {
            format!("\x1b[1;{modifier}{c}").into_bytes()
        } else if app_cursor {
            format!("\x1bO{c}").into_bytes()
        } else {
            format!("\x1b[{c}").into_bytes()
        }
    };
    let tilde = |n: u8| {
        if modifier > 1 { format!("\x1b[{n};{modifier}~").into_bytes() } else { format!("\x1b[{n}~").into_bytes() }
    };
    Some(match key {
        Key::Enter => b"\r".to_vec(),
        Key::Backspace if m.alt => b"\x1b\x7f".to_vec(),
        Key::Backspace => b"\x7f".to_vec(),
        Key::Tab if m.shift => b"\x1b[Z".to_vec(),
        Key::Tab => b"\t".to_vec(),
        Key::Escape => b"\x1b".to_vec(),
        Key::ArrowUp => cursor('A'),
        Key::ArrowDown => cursor('B'),
        Key::ArrowRight => cursor('C'),
        Key::ArrowLeft => cursor('D'),
        Key::Home => cursor('H'),
        Key::End => cursor('F'),
        Key::Insert => tilde(2),
        Key::Delete => tilde(3),
        Key::PageUp => tilde(5),
        Key::PageDown => tilde(6),
        Key::F1 => b"\x1bOP".to_vec(),
        Key::F2 => b"\x1bOQ".to_vec(),
        Key::F3 => b"\x1bOR".to_vec(),
        Key::F4 => b"\x1bOS".to_vec(),
        Key::F5 => tilde(15),
        Key::F6 => tilde(17),
        Key::F7 => tilde(18),
        Key::F8 => tilde(19),
        Key::F9 => tilde(20),
        Key::F10 => tilde(21),
        Key::F11 => tilde(23),
        Key::F12 => tilde(24),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(key: Key, m: Modifiers) -> Vec<u8> {
        key_bytes(key, m, false).unwrap_or_default()
    }

    #[test]
    fn control_keys() {
        assert_eq!(keys(Key::C, Modifiers::CTRL), [3]);
        assert_eq!(keys(Key::D, Modifiers::CTRL), [4]);
        assert_eq!(keys(Key::Z, Modifiers::CTRL), [26]);
        assert_eq!(keys(Key::OpenBracket, Modifiers::CTRL), [0x1b]);
        assert_eq!(keys(Key::Enter, Modifiers::NONE), b"\r");
        assert_eq!(keys(Key::Backspace, Modifiers::NONE), [0x7f]);
        assert_eq!(keys(Key::Tab, Modifiers::SHIFT), b"\x1b[Z");
        // Plain letters come as text events, Cmd shortcuts belong to the app.
        assert!(key_bytes(Key::A, Modifiers::NONE, false).is_none());
        assert!(key_bytes(Key::V, Modifiers::MAC_CMD, false).is_none());
    }

    #[test]
    fn cursor_keys_follow_the_mode() {
        assert_eq!(keys(Key::ArrowUp, Modifiers::NONE), b"\x1b[A");
        assert_eq!(key_bytes(Key::ArrowUp, Modifiers::NONE, true).unwrap(), b"\x1bOA");
        assert_eq!(keys(Key::ArrowLeft, Modifiers::ALT), b"\x1b[1;3D");
        assert_eq!(keys(Key::Delete, Modifiers::NONE), b"\x1b[3~");
        assert_eq!(keys(Key::F5, Modifiers::NONE), b"\x1b[15~");
    }

    #[test]
    fn palette() {
        assert_eq!(color(vt100::Color::Idx(1), TEXT), ansi::COLORS[1]);
        assert_eq!(color(vt100::Color::Idx(16), TEXT), Color32::from_rgb(0, 0, 0));
        assert_eq!(color(vt100::Color::Idx(231), TEXT), Color32::from_rgb(255, 255, 255));
        assert_eq!(color(vt100::Color::Idx(232), TEXT), Color32::from_rgb(8, 8, 8));
        assert_eq!(color(vt100::Color::Default, TEXT), TEXT);
    }

    fn sh(script: &str) -> Terminal {
        let spec = CommandSpec {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), script.into()],
            cwd: std::env::temp_dir(),
            envs: vec![("TERM".into(), "xterm-256color".into())],
            summary: "sh".into(),
        };
        Terminal::start(&egui::Context::default(), move || Ok(spec))
    }

    fn wait_until(what: impl Fn() -> bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !what() && std::time::Instant::now() < deadline {
            thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    #[test]
    fn runs_a_command_in_a_pty() {
        let term = sh("test -t 0 && printf 'tty %s\\n' \"$(tput cols)\"");
        wait_until(|| term.status() != Status::Running);
        assert_eq!(term.status(), Status::Ok, "{}", term.text());
        assert!(term.text().contains("tty 100"), "{}", term.text());
    }

    #[test]
    fn sends_input_and_keeps_scrollback() {
        let mut term = sh("for i in $(seq 1 60); do echo line $i; done; read x; echo got $x; exit 3");
        wait_until(|| term.text().contains("line 60"));
        term.send(b"hello\r");
        wait_until(|| term.status() != Status::Running);
        let text = term.text();
        assert_eq!(term.status(), Status::Failed, "{text}");
        for want in ["line 1\n", "line 60", "got hello", "Exited with code 3"] {
            assert!(text.contains(want), "missing {want:?} in {text}");
        }
    }

    #[test]
    fn draws_and_resizes_to_the_panel() {
        let ctx = egui::Context::default();
        crate::theme::install(&ctx);
        let mut term = sh("printf '\\033[1;31mred\\033[0m \\033[7minverse\\033[0m 漢字 \\033[38;5;208mx\\n'; sleep 5");
        wait_until(|| term.text().contains("inverse"));
        for width in [900.0, 300.0, 40.0] {
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(width, 200.0))),
                ..Default::default()
            };
            let mut out = ctx.run_ui(input, |ui| term.ui(ui));
            out.textures_delta.clear();
            let size = term.shared.parser.lock().unwrap().screen().size();
            assert!(size.1.get() >= 10 && f32::from(size.1.get()) * 7.0 <= width.max(70.0), "{size:?} at {width}");
            if width == 900.0 {
                // Like xterm, narrowing cuts lines rather than rewrapping them.
                assert!(term.text().contains("red inverse 漢字 x"), "{}", term.text());
            }
        }
    }

    #[test]
    fn stop_hangs_up() {
        let term = sh("echo ready; sleep 30");
        wait_until(|| term.text().contains("ready"));
        term.stop();
        wait_until(|| term.status() != Status::Running);
        assert_eq!(term.status(), Status::Cancelled);
    }
}
