//! ANSI SGR colors (16 colors, bold, dim) → egui text layout.

use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontId};

// One Dark-style colors for the black terminal background.
pub const COLORS: [Color32; 8] = [
    Color32::from_rgb(0x5c, 0x63, 0x70),
    Color32::from_rgb(0xe0, 0x6c, 0x75),
    Color32::from_rgb(0x98, 0xc3, 0x79),
    Color32::from_rgb(0xe5, 0xc0, 0x7b),
    Color32::from_rgb(0x61, 0xaf, 0xef),
    Color32::from_rgb(0xc6, 0x78, 0xdd),
    Color32::from_rgb(0x56, 0xb6, 0xc2),
    Color32::from_rgb(0xdc, 0xdf, 0xe4),
];
pub const BRIGHT: [Color32; 8] = [
    Color32::from_rgb(0x7f, 0x84, 0x8e),
    Color32::from_rgb(0xf4, 0x84, 0x8c),
    Color32::from_rgb(0xb5, 0xdc, 0x96),
    Color32::from_rgb(0xf2, 0xd2, 0x96),
    Color32::from_rgb(0x82, 0xc4, 0xf8),
    Color32::from_rgb(0xd9, 0x96, 0xea),
    Color32::from_rgb(0x7c, 0xcc, 0xd6),
    Color32::WHITE,
];

pub fn layout(line: &str, base: Color32, font: FontId) -> LayoutJob {
    let mut job = LayoutJob::default();
    let mut color = base;
    let mut bold = false;
    let mut dim = false;
    let push = |job: &mut LayoutJob, text: &str, color: Color32, bold: bool, dim: bool| {
        if text.is_empty() {
            return;
        }
        let mut c = if bold && color == base { Color32::WHITE } else { color };
        if dim {
            c = c.gamma_multiply(0.65);
        }
        job.append(text, 0.0, TextFormat { font_id: font.clone(), color: c, ..Default::default() });
    };

    let mut rest = line;
    while let Some(start) = rest.find('\x1b') {
        push(&mut job, &rest[..start], color, bold, dim);
        let seq = &rest[start..];
        // CSI: ESC [ params final-byte
        let Some(body) = seq.strip_prefix("\x1b[") else {
            rest = &seq[1..];
            continue;
        };
        let Some(end) = body.find(|c: char| c.is_ascii_alphabetic()) else {
            rest = "";
            break;
        };
        if &body[end..end + 1] == "m" {
            let params = &body[..end];
            let codes: Vec<u32> =
                if params.is_empty() { vec![0] } else { params.split(';').filter_map(|p| p.parse().ok()).collect() };
            for c in codes {
                match c {
                    0 => (color, bold, dim) = (base, false, false),
                    1 => bold = true,
                    2 => dim = true,
                    22 => (bold, dim) = (false, false),
                    39 => color = base,
                    30..=37 => color = COLORS[(c - 30) as usize],
                    90..=97 => color = BRIGHT[(c - 90) as usize],
                    _ => {}
                }
            }
        }
        rest = &body[end + 1..];
    }
    push(&mut job, rest, color, bold, dim);
    job
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_codes() {
        let job = layout("\x1b[32mok\x1b[0m done \x1b[2K", Color32::GRAY, FontId::monospace(12.0));
        assert_eq!(job.text, "ok done ");
        assert_eq!(job.sections[0].format.color, COLORS[2]);
    }
}
