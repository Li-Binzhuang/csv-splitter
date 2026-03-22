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
            .width(160.0)
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
    egui::Frame::new()
        .fill(ui.visuals().extreme_bg_color)
        .corner_radius(egui::CornerRadius::same(10))
        .inner_margin(8.0)
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .max_height(140.0)
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    for line in logs {
                        ui.label(
                            egui::RichText::new(line)
                                .size(12.0)
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
        .stroke(egui::Stroke::new(1.0, CARD_BORDER))
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

/// Draw a horizontal arrow between flow cards.
pub fn draw_arrow(ui: &mut egui::Ui, color: egui::Color32, w: f32, h: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::hover());
    let mid_y = rect.center().y;
    let p = ui.painter();
    p.line_segment(
        [
            egui::pos2(rect.left() + 2.0, mid_y),
            egui::pos2(rect.right() - 6.0, mid_y),
        ],
        egui::Stroke::new(2.0, color),
    );
    let tip = egui::pos2(rect.right() - 2.0, mid_y);
    p.line_segment(
        [egui::pos2(tip.x - 6.0, mid_y - 4.0), tip],
        egui::Stroke::new(2.0, color),
    );
    p.line_segment(
        [egui::pos2(tip.x - 6.0, mid_y + 4.0), tip],
        egui::Stroke::new(2.0, color),
    );
}
