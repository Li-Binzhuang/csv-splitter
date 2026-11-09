//! Common UI widgets: section_frame, stat_badge, log_panel, job_status, etc.

use super::*;
use crate::i18n::{t, Lang};

/// Delimiter combo box.
pub fn delimiter_combo(ui: &mut egui::Ui, idx: &mut usize, custom: &mut String, lang: Lang) {
    let common = csv_splitter_core::Delimiter::common();
    let labels: Vec<&str> = common
        .iter()
        .map(|d| d.label())
        .chain(std::iter::once(t!(lang, CustomDelim)))
        .collect();
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(t!(lang, Delimiter)).color(TEXT_BRIGHT));
        egui::ComboBox::from_id_salt(ui.next_auto_id())
            .selected_text(labels[*idx])
            .width((ui.available_width() - 80.0).clamp(60.0, 160.0))
            .show_ui(ui, |ui| {
                for (i, label) in labels.iter().enumerate() {
                    ui.selectable_value(idx, i, *label);
                }
            });
        if *idx >= common.len() {
            ui.label(egui::RichText::new(t!(lang, CustomChar)).color(TEXT_BRIGHT));
            ui.add(egui::TextEdit::singleline(custom).desired_width(40.0));
        }
    });
}

/// Scrollable log area.
pub fn log_panel(ui: &mut egui::Ui, logs: &[String], lang: Lang) {
    if logs.is_empty() {
        return;
    }
    ui.add_space(8.0);
    ui.separator();
    ui.label(t!(lang, LogTitle));
    let log_h = ui.available_height().max(120.0) - 8.0;
    egui::Frame::new()
        .fill(CARD_BG)
        .stroke(egui::Stroke::new(1.0_f32, CARD_BORDER))
        .corner_radius(egui::CornerRadius::same(10))
        .inner_margin(8.0)
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .max_height(log_h)
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    for line in logs {
                        ui.label(
                            egui::RichText::new(line)
                                .size(12.0)
                                .color(TEXT_BRIGHT)
                                .family(egui::FontFamily::Monospace),
                        );
                    }
                });
        });
}

/// Job status indicator.
pub fn job_status(ui: &mut egui::Ui, state: &crate::state::JobState, lang: Lang) {
    match state {
        crate::state::JobState::Running => {
            ui.spinner();
            ui.label(t!(lang, Processing));
        }
        crate::state::JobState::Done => {
            ui.label(egui::RichText::new(t!(lang, Done)).color(SUCCESS));
        }
        crate::state::JobState::Error => {
            ui.label(egui::RichText::new(t!(lang, Failed)).color(ERROR_C));
        }
        crate::state::JobState::Idle => {}
    }
}

/// Compact stat badge.
pub fn stat_badge(ui: &mut egui::Ui, icon: &str, label: &str, value: &str) {
    egui::Frame::new()
        .fill(egui::Color32::from_rgb(60, 66, 84))
        .corner_radius(egui::CornerRadius::same(12))
        .inner_margin(egui::Margin::symmetric(8, 3))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 3.0;
                ui.label(egui::RichText::new(icon).size(12.0));
                ui.label(egui::RichText::new(label).size(11.0).color(TEXT_DIM));
                ui.label(
                    egui::RichText::new(value)
                        .size(11.0)
                        .strong()
                        .color(TEXT_BRIGHT),
                );
            });
        });
}

/// Titled section frame.
pub fn section_frame(ui: &mut egui::Ui, title: &str, add_contents: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .fill(CARD_BG)
        .stroke(egui::Stroke::new(1.0_f32, CARD_BORDER))
        .corner_radius(egui::CornerRadius::same(14))
        .inner_margin(14.0)
        .outer_margin(egui::Margin::symmetric(0, 4))
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new(title)
                    .size(13.0)
                    .strong()
                    .color(TEXT_BRIGHT),
            );
            ui.add_space(6.0);
            add_contents(ui);
        });
}

/// Two-column layout helper. Returns (left_w, right_w, left_rect, right_rect).
/// `ratio` is the fraction for the left column (e.g. 0.5 = equal, 0.6 = left wider).
/// Caller should use `ui.allocate_space()` after drawing both columns
/// to advance the parent cursor past the taller column.
pub fn two_col_rects_ratio(
    ui: &egui::Ui,
    gap: f32,
    ratio: f32,
) -> (f32, f32, egui::Rect, egui::Rect) {
    let avail = ui.available_width();
    let usable = (avail - gap).max(200.0);
    let left_w = (usable * ratio).max(100.0);
    let right_w = (usable - left_w).max(100.0);
    let top = ui.cursor().min;
    let left = egui::Rect::from_min_size(top, egui::vec2(left_w, 10.0));
    let right = egui::Rect::from_min_size(
        egui::pos2(top.x + left_w + gap, top.y),
        egui::vec2(right_w, 10.0),
    );
    (left_w, right_w, left, right)
}

/// Convenience: equal 50/50 two-column rects.
pub fn two_col_rects(ui: &egui::Ui, gap: f32) -> (f32, egui::Rect, egui::Rect) {
    let (lw, _rw, l, r) = two_col_rects_ratio(ui, gap, 0.5);
    (lw, l, r)
}

/// Draw a horizontal right-pointing arrow (→).
pub fn draw_arrow(ui: &mut egui::Ui, color: egui::Color32, w: f32, h: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::hover());
    let mid_y = rect.center().y;
    let p = ui.painter();
    p.line_segment(
        [
            egui::pos2(rect.left() + 2.0, mid_y),
            egui::pos2(rect.right() - 6.0, mid_y),
        ],
        egui::Stroke::new(2.0_f32, color),
    );
    let tip = egui::pos2(rect.right() - 2.0, mid_y);
    p.line_segment(
        [egui::pos2(tip.x - 6.0, mid_y - 4.0), tip],
        egui::Stroke::new(2.0_f32, color),
    );
    p.line_segment(
        [egui::pos2(tip.x - 6.0, mid_y + 4.0), tip],
        egui::Stroke::new(2.0_f32, color),
    );
}

/// Draw a horizontal left-pointing arrow (←).
pub fn draw_arrow_left(ui: &mut egui::Ui, color: egui::Color32, w: f32, h: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::hover());
    let mid_y = rect.center().y;
    let p = ui.painter();
    p.line_segment(
        [
            egui::pos2(rect.left() + 6.0, mid_y),
            egui::pos2(rect.right() - 2.0, mid_y),
        ],
        egui::Stroke::new(2.0_f32, color),
    );
    let tip = egui::pos2(rect.left() + 2.0, mid_y);
    p.line_segment(
        [egui::pos2(tip.x + 6.0, mid_y - 4.0), tip],
        egui::Stroke::new(2.0_f32, color),
    );
    p.line_segment(
        [egui::pos2(tip.x + 6.0, mid_y + 4.0), tip],
        egui::Stroke::new(2.0_f32, color),
    );
}

/// Draw a vertical down-pointing arrow (↓), centered in given width.
pub fn draw_arrow_down(ui: &mut egui::Ui, color: egui::Color32, w: f32, h: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::hover());
    let mid_x = rect.center().x;
    let p = ui.painter();
    p.line_segment(
        [
            egui::pos2(mid_x, rect.top() + 2.0),
            egui::pos2(mid_x, rect.bottom() - 6.0),
        ],
        egui::Stroke::new(2.0_f32, color),
    );
    let tip = egui::pos2(mid_x, rect.bottom() - 2.0);
    p.line_segment(
        [egui::pos2(mid_x - 4.0, tip.y - 6.0), tip],
        egui::Stroke::new(2.0_f32, color),
    );
    p.line_segment(
        [egui::pos2(mid_x + 4.0, tip.y - 6.0), tip],
        egui::Stroke::new(2.0_f32, color),
    );
}
