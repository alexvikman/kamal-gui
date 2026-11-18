use egui::{Color32, CornerRadius, FontFamily, Margin, RichText, Stroke, TextStyle, ThemePreference, Ui, Visuals, vec2};

pub const ACCENT: Color32 = Color32::from_rgb(0x25, 0x63, 0xeb);
pub const TEXT: Color32 = Color32::from_rgb(0x0d, 0x0d, 0x0d);

/// The app is light-only: white surfaces, near-black text.
pub struct Palette {
    pub ok: Color32,
    pub bad: Color32,
    pub bad_soft: Color32,
    pub warn: Color32,
    pub warn_soft: Color32,
    pub muted: Color32,
    pub card: Color32,
    pub border: Color32,
    pub subtle: Color32,
}

pub fn palette(_ui: &Ui) -> Palette {
    palette_static()
}

/// Geist for the UI (bundled, SIL OFL; see assets/fonts/Geist-OFL.txt), with
/// a medium weight for titles. SF Mono for code when present (macOS).
pub fn medium() -> FontFamily {
    FontFamily::Name("medium".into())
}

fn install_fonts(ctx: &egui::Context) {
    use egui::{FontData, FontDefinitions};
    use std::sync::Arc;
    let mut fonts = FontDefinitions::default();
    fonts
        .font_data
        .insert("geist".into(), Arc::new(FontData::from_static(include_bytes!("../assets/fonts/Geist-Regular.ttf"))));
    fonts
        .font_data
        .insert("geist-medium".into(), Arc::new(FontData::from_static(include_bytes!("../assets/fonts/Geist-Medium.ttf"))));
    fonts.families.entry(FontFamily::Proportional).or_default().insert(0, "geist".into());
    // Medium falls back to the regular stack for glyphs Geist lacks.
    let mut medium_stack = vec!["geist-medium".to_string()];
    medium_stack.extend(fonts.families[&FontFamily::Proportional].iter().skip(1).cloned());
    fonts.families.insert(medium(), medium_stack);
    if let Ok(bytes) = std::fs::read("/System/Library/Fonts/SFNSMono.ttf") {
        fonts.font_data.insert("sf-mono".into(), Arc::new(FontData::from_owned(bytes)));
        fonts.families.entry(FontFamily::Monospace).or_default().insert(0, "sf-mono".into());
    }
    ctx.set_fonts(fonts);
}

pub fn install(ctx: &egui::Context) {
    install_fonts(ctx);
    ctx.set_theme(ThemePreference::Light);

    let mut v = Visuals::light();
    let p = palette_static();
    v.panel_fill = Color32::WHITE;
    v.window_fill = Color32::WHITE;
    v.extreme_bg_color = Color32::WHITE; // text inputs, scroll areas
    v.faint_bg_color = Color32::from_rgb(0xfa, 0xfa, 0xfa); // striped rows
    v.code_bg_color = p.subtle;
    v.override_text_color = Some(TEXT);
    v.window_stroke = Stroke::new(1.0, p.border);
    v.window_shadow.color = Color32::from_black_alpha(28);
    v.popup_shadow.color = Color32::from_black_alpha(20);
    v.selection.bg_fill = Color32::from_rgb(0xec, 0xec, 0xee);
    v.selection.stroke = Stroke::new(1.0, TEXT);
    v.hyperlink_color = ACCENT;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
    // Every state has a 1px border of the same width, so nothing shifts or
    // grows on hover; only colors change. Text inputs use the same border.
    for (w, fill, border) in [
        (&mut v.widgets.inactive, Color32::from_rgb(0xf4, 0xf4, 0xf5), p.border),
        (&mut v.widgets.hovered, Color32::from_rgb(0xea, 0xea, 0xec), Color32::from_rgb(0xd6, 0xd6, 0xda)),
        (&mut v.widgets.active, Color32::from_rgb(0xe0, 0xe0, 0xe3), Color32::from_rgb(0xc8, 0xc8, 0xcd)),
        (&mut v.widgets.open, Color32::from_rgb(0xea, 0xea, 0xec), Color32::from_rgb(0xd6, 0xd6, 0xda)),
    ] {
        w.bg_fill = fill;
        w.weak_bg_fill = fill;
        w.bg_stroke = Stroke::new(1.0, border);
        w.fg_stroke = Stroke::new(1.0, TEXT);
        w.corner_radius = CornerRadius::same(7);
        w.expansion = 0.0;
    }

    ctx.all_styles_mut(|style| {
        style.visuals = v.clone();
        style.spacing.item_spacing = vec2(8.0, 6.0);
        // A slim grey scrollbar with room between it and the content.
        // Floating, because egui only applies the handle opacities below to
        // floating bars; `floating_allocated_width` still reserves a lane so
        // the bar never covers content.
        let mut scroll = egui::style::ScrollStyle::floating();
        scroll.bar_width = 6.0;
        scroll.floating_width = 6.0;
        scroll.floating_allocated_width = 18.0;
        scroll.handle_min_length = 24.0;
        scroll.bar_inner_margin = 6.0;
        scroll.bar_outer_margin = 3.0;
        scroll.foreground_color = true;
        scroll.dormant_background_opacity = 0.0;
        scroll.active_background_opacity = 0.0;
        scroll.interact_background_opacity = 0.0;
        scroll.dormant_handle_opacity = 0.22;
        scroll.active_handle_opacity = 0.32;
        scroll.interact_handle_opacity = 0.45;
        style.spacing.scroll = scroll;
        style.spacing.button_padding = vec2(11.0, 5.0);
        style.spacing.interact_size.y = 26.0;
        style.text_styles.insert(TextStyle::Body, egui::FontId::proportional(14.0));
        style.text_styles.insert(TextStyle::Button, egui::FontId::proportional(14.0));
        style.text_styles.insert(TextStyle::Small, egui::FontId::proportional(12.0));
        style.text_styles.insert(TextStyle::Monospace, egui::FontId::monospace(12.5));
        style.text_styles.insert(TextStyle::Heading, egui::FontId::new(22.0, medium()));
    });
}

fn palette_static() -> Palette {
    Palette {
        ok: Color32::from_rgb(0x1a, 0x7f, 0x45),
        bad: Color32::from_rgb(0xc4, 0x3a, 0x31),
        bad_soft: Color32::from_rgb(0xfd, 0xec, 0xea),
        warn: Color32::from_rgb(0x8f, 0x5b, 0x00),
        warn_soft: Color32::from_rgb(0xfd, 0xf4, 0xe0),
        muted: Color32::from_rgb(0x6e, 0x6e, 0x73),
        card: Color32::WHITE,
        border: Color32::from_rgb(0xe8, 0xe8, 0xea),
        subtle: Color32::from_rgb(0xf4, 0xf4, 0xf5),
    }
}

/// Title text in the medium weight.
pub fn title(text: impl Into<String>, size: f32) -> RichText {
    RichText::new(text.into()).family(medium()).size(size)
}

pub fn primary(text: &str) -> egui::Button<'static> {
    filled(text, ACCENT)
}

/// A solid button. Its border matches the fill so no grey edge shows.
pub fn filled(text: &str, color: Color32) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.to_string()).family(medium()).color(Color32::WHITE))
        .fill(color)
        .stroke(Stroke::new(1.0, color))
}

pub fn danger(ui: &Ui, text: &str) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.to_string()).color(palette(ui).bad))
}

pub fn dot(ui: &mut Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(vec2(10.0, 10.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.0, color);
}

/// A soft-colored banner (errors, warnings).
/// A short, readable headline for an error, plus the full text when it adds
/// anything (git and ssh errors are long and mostly noise).
pub fn summarize_error(msg: &str) -> (String, Option<String>) {
    let known = [
        ("Permission denied (publickey)", "GitHub refused the SSH key. Is your SSH agent (1Password) unlocked and allowed to sign for this app?"),
        ("communication with agent failed", "Couldn't talk to the SSH agent (1Password). Unlock it and try again."),
        ("Could not resolve host", "Can't reach the server. Check your internet connection."),
        ("Operation timed out", "The connection timed out. Is the VPN up?"),
        ("Connection refused", "The connection was refused."),
    ];
    let headline_prefix = msg.split(": `").next().filter(|p| p.len() < msg.len()).map(|p| format!("{p}: "));
    for (needle, text) in known {
        if msg.contains(needle) {
            return (format!("{}{text}", headline_prefix.unwrap_or_default()), Some(msg.to_string()));
        }
    }
    let first = msg.lines().next().unwrap_or(msg);
    if first.len() > 180 || msg.lines().count() > 1 {
        let short: String = first.chars().take(180).collect();
        let ellipsis = if first.chars().count() > 180 { "…" } else { "" };
        (format!("{short}{ellipsis}"), Some(msg.to_string()))
    } else {
        (msg.to_string(), None)
    }
}

/// A colored message with a Dismiss button on the right and, for long
/// errors, the full text under "Details". Returns true when dismissed.
pub fn dismissable(ui: &mut Ui, fg: Color32, bg: Color32, msg: &str) -> bool {
    let (headline, details) = summarize_error(msg);
    let mut dismissed = false;
    banner(ui, fg, bg, |ui| {
        ui.horizontal_top(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                dismissed = quiet_rich(ui, RichText::new("Dismiss").color(fg)).clicked();
                ui.add_space(12.0);
                ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                    ui.add(egui::Label::new(RichText::new(headline).color(fg)).wrap());
                    if let Some(details) = details {
                        egui::CollapsingHeader::new(RichText::new("Details").size(12.5).color(fg))
                            .id_salt(("error-details", msg.len(), msg.chars().take(40).collect::<String>()))
                            .show(ui, |ui| {
                                ui.add(egui::Label::new(RichText::new(details).monospace().size(11.5).color(fg)).wrap());
                            });
                    }
                });
            });
        });
    });
    dismissed
}

pub fn banner(ui: &mut Ui, fg: Color32, bg: Color32, add: impl FnOnce(&mut Ui)) {
    egui::Frame::new()
        .fill(bg)
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.visuals_mut().override_text_color = Some(fg);
            add(ui);
        });
}

/// Small muted secondary text.
pub fn caption(text: impl Into<String>) -> RichText {
    RichText::new(text.into()).size(12.5).color(palette_static().muted)
}

/// Frameless text button in the accent color.
pub fn link(ui: &mut Ui, text: &str) -> egui::Response {
    let r = ui
        .add(egui::Button::new(RichText::new(text).color(ACCENT)).frame(false))
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    if r.hovered() {
        let y = r.rect.bottom() - 2.0;
        ui.painter().hline(r.rect.x_range(), y, Stroke::new(1.0, ACCENT));
    }
    r
}

/// Text button without a visible frame until hovered.
pub fn quiet(ui: &mut Ui, text: &str) -> egui::Response {
    quiet_rich(ui, RichText::new(text))
}

pub fn quiet_rich(ui: &mut Ui, text: RichText) -> egui::Response {
    ui.add(egui::Button::new(text).frame_when_inactive(false)).on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// White button with a hairline border.
pub fn secondary(text: &str) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.to_string())).fill(Color32::WHITE).stroke(Stroke::new(1.0, palette_static().border))
}

/// A "⋯" button that opens `menu` below it.
pub fn more_menu(ui: &mut Ui, hover: &str, menu: impl FnOnce(&mut Ui)) {
    let (rect, resp) = ui.allocate_exact_size(vec2(28.0, 26.0), egui::Sense::click());
    let open = egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&resp));
    if resp.hovered() || open {
        ui.painter().rect_filled(rect, CornerRadius::same(7), Color32::from_rgb(0xf0, 0xf0, 0xf2));
    }
    for dx in [-6.0, 0.0, 6.0] {
        ui.painter().circle_filled(rect.center() + vec2(dx, 0.0), 1.6, TEXT);
    }
    let resp = resp.on_hover_text(hover);
    egui::Popup::menu(&resp).align(egui::RectAlign::BOTTOM_END).show(|ui| {
        ui.set_min_width(220.0);
        menu(ui);
    });
}

/// Text tabs with an underline under the active one.
pub fn tabs<T: PartialEq + Copy>(ui: &mut Ui, current: &mut T, items: &[(T, &str)]) {
    let p = palette_static();
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 20.0;
        for (value, label) in items {
            let active = *current == *value;
            let text = if active { RichText::new(*label).family(medium()) } else { RichText::new(*label).color(p.muted) };
            let r = ui.add(egui::Label::new(text).sense(egui::Sense::click()));
            if active {
                let y = r.rect.bottom() + 6.0;
                ui.painter().line_segment([egui::pos2(r.rect.left(), y), egui::pos2(r.rect.right(), y)], Stroke::new(2.0, TEXT));
            }
            if r.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                *current = *value;
            }
        }
    });
    ui.add_space(6.0);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), egui::Sense::hover());
    ui.painter().hline(rect.x_range(), rect.center().y, Stroke::new(1.0, p.border));
}

/// `git@github.com:org/app.git` → `org/app`.
pub fn short_url(url: &str) -> String {
    let s = url.trim_end_matches(".git");
    for prefix in ["git@github.com:", "https://github.com/", "ssh://git@github.com/"] {
        if let Some(rest) = s.strip_prefix(prefix) {
            return rest.to_string();
        }
    }
    s.to_string()
}

pub fn card(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    let p = palette(ui);
    egui::Frame::new()
        .fill(p.card)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui);
        });
}

pub fn ago(ms: i64) -> String {
    let s = (chrono::Utc::now().timestamp_millis() - ms).max(0) / 1000;
    match s {
        0..60 => format!("{s}s ago"),
        60..3600 => format!("{}m ago", s / 60),
        3600..86400 => format!("{}h ago", s / 3600),
        _ => date_time(ms),
    }
}

pub fn date_time(ms: i64) -> String {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|d| d.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_default()
}

pub fn short_version(v: &str) -> String {
    if v.len() >= 40 && v[..40].chars().all(|c| c.is_ascii_hexdigit()) {
        format!("{}{}", &v[..7], &v[40..])
    } else {
        v.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Event, Pos2, RawInput, Rect, Shape};

    /// Paint a primary button and return its blue rect and that rect's stroke.
    fn painted(ctx: &egui::Context, hover: bool) -> (Rect, Stroke) {
        let mut found = None;
        for _ in 0..3 {
            let mut input = RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(400.0, 200.0))),
                ..Default::default()
            };
            let pos = if hover { egui::pos2(30.0, 12.0) } else { egui::pos2(390.0, 190.0) };
            input.events.push(Event::PointerMoved(pos));
            let mut out = ctx.run_ui(input, |ui| {
                ui.add(primary("Deploy all"));
            });
            out.textures_delta.clear();
            found = out.shapes.iter().find_map(|cs| match &cs.shape {
                Shape::Rect(r) if r.fill == ACCENT => Some((r.rect, r.stroke)),
                _ => None,
            });
        }
        found.expect("no primary button painted")
    }

    #[test]
    fn git_ssh_errors_get_a_readable_headline() {
        let msg = "Fetching from origin failed: `git --git-dir /Users/x/Library/Application Support/kgui/repos/my-app.git fetch --quiet` failed (exit status: 128): sign_and_send_pubkey: signing failed for RSA \"id_rsa\" from agent: communication with agent failed\ngit@github.com: Permission denied (publickey).\nfatal: Could not read from remote repository.";
        let (headline, details) = summarize_error(msg);
        assert!(headline.starts_with("Fetching from origin failed: GitHub refused the SSH key"), "{headline}");
        assert_eq!(details.as_deref(), Some(msg));
        assert_eq!(summarize_error("short"), ("short".to_string(), None));
    }

    #[test]
    fn primary_button_looks_the_same_on_hover() {
        let ctx = egui::Context::default();
        install(&ctx);
        let (idle_rect, idle_stroke) = painted(&ctx, false);
        let (hover_rect, hover_stroke) = painted(&ctx, true);
        assert_eq!(idle_rect, hover_rect, "button changes size on hover");
        for s in [idle_stroke, hover_stroke] {
            assert!(s.width == 0.0 || s.color == ACCENT, "border {s:?} drawn over the blue fill");
        }
    }
}
