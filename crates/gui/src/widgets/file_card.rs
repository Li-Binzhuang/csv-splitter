//! File input card and directory path card widgets.

use super::*;
use crate::i18n::{t, Lang};
use crate::state::FileInfo;

/// Reusable file input card with click-to-browse + metadata display.
/// Links ("移除 · 重新选择") appear BELOW the card vertically.
/// Returns Some(PendingDialog) if a file dialog was spawned.
#[allow(clippy::too_many_arguments)]
pub fn file_input_card(
    ui: &mut egui::Ui,
    file: &mut Option<FileInfo>,
    placeholder: &str,
    accent: egui::Color32,
    w: f32,
    min_h: f32,
    lang: Lang,
    filters: &[(&str, &[&str])],
    dialog_id: egui::Id,
) -> Option<PendingDialog> {
    let mut result: Option<PendingDialog> = None;
    let has_file = file.is_some();

    // Wrap everything in a vertical so links always appear below the card
    ui.vertical(|ui| {
        let border = if has_file { accent } else { CARD_BORDER };

        let frame_resp = egui::Frame::new()
            .fill(CARD_BG)
            .stroke(egui::Stroke::new(if has_file { 1.5 } else { 1.0 }, border))
            .corner_radius(egui::CornerRadius::same(12))
            .inner_margin(12.0)
            .show(ui, |ui| {
                let max_w = w.min(ui.available_width());
                ui.set_width(max_w);
                ui.set_max_width(max_w);
                ui.set_min_height(min_h);

                if let Some(ref info) = file {
                    draw_file_meta(ui, info, max_w, lang);
                } else {
                    ui.vertical_centered(|ui| {
                        ui.add_space((min_h - 30.0).max(0.0) / 2.0);
                        ui.label(egui::RichText::new(placeholder).size(11.0).color(TEXT_DIM));
                    });
                }
            });

        // Clear + re-select links below the card (only when file is loaded)
        if has_file {
            ui.horizontal(|ui| {
                if ui
                    .link(
                        egui::RichText::new(t!(lang, RemoveFile))
                            .size(11.0)
                            .color(TEXT_DIM),
                    )
                    .clicked()
                {
                    *file = None;
                }
                ui.label(egui::RichText::new("·").size(11.0).color(TEXT_DIM));
                if ui
                    .link(
                        egui::RichText::new(t!(lang, ReSelect))
                            .size(11.0)
                            .color(ACCENT),
                    )
                    .clicked()
                {
                    let owned_filters: Vec<(String, Vec<String>)> = filters
                        .iter()
                        .map(|(n, exts)| {
                            (n.to_string(), exts.iter().map(|e| e.to_string()).collect())
                        })
                        .collect();
                    result = Some(spawn_file_dialog(
                        DialogTarget::File(dialog_id),
                        owned_filters,
                    ));
                }
            });
            // If file was just cleared, don't process card click
            if file.is_none() {
                return;
            }
        }

        // Card-level click to open file browser (only when empty)
        if !has_file {
            let card_resp = frame_resp.response.interact(egui::Sense::click());
            if card_resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if card_resp.clicked() {
                let owned_filters: Vec<(String, Vec<String>)> = filters
                    .iter()
                    .map(|(n, exts)| (n.to_string(), exts.iter().map(|e| e.to_string()).collect()))
                    .collect();
                result = Some(spawn_file_dialog(
                    DialogTarget::File(dialog_id),
                    owned_filters,
                ));
            }
        }
    });

    result
}

fn draw_file_meta(ui: &mut egui::Ui, info: &FileInfo, max_w: f32, lang: Lang) {
    let max_name_chars = ((max_w - 50.0) / 7.0).max(8.0) as usize;
    let display_name = if info.name.chars().count() > max_name_chars {
        let truncated: String = info.name.chars().take(max_name_chars - 2).collect();
        format!("{}…", truncated)
    } else {
        info.name.clone()
    };
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("📄").size(16.0));
        ui.vertical(|ui| {
            ui.set_max_width(max_w - 40.0);
            ui.label(
                egui::RichText::new(&display_name)
                    .size(12.0)
                    .strong()
                    .color(TEXT_BRIGHT),
            );
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.label(
                    egui::RichText::new(format!(
                        "{}: {}",
                        t!(lang, FileSizeLabel),
                        format_bytes(info.size)
                    ))
                    .size(10.0)
                    .color(TEXT_DIM),
                );
                if !info.extension.is_empty() {
                    ui.label(
                        egui::RichText::new(format!("{}: {}", t!(lang, FileType), &info.extension))
                            .size(10.0)
                            .color(TEXT_DIM),
                    );
                }
                if let Some(modified) = info.modified {
                    if let Ok(dur) = modified.elapsed() {
                        let s = dur.as_secs();
                        let ts = if s < 60 {
                            format!("{}s", s)
                        } else if s < 3600 {
                            format!("{}m", s / 60)
                        } else if s < 86400 {
                            format!("{}h", s / 3600)
                        } else {
                            format!("{}d", s / 86400)
                        };
                        ui.label(
                            egui::RichText::new(format!("{}: {}", t!(lang, FileModified), ts))
                                .size(10.0)
                                .color(TEXT_DIM),
                        );
                    }
                }
            });
        });
    });
}

/// Directory input card with async browse. Returns Some(PendingDialog) if spawned.
pub fn dir_path_card(
    ui: &mut egui::Ui,
    path: &mut String,
    icon: &str,
    label: &str,
    placeholder: &str,
    lang: Lang,
    dialog_id: egui::Id,
) -> Option<PendingDialog> {
    let has = !path.is_empty();
    let border = if has { ACCENT } else { CARD_BORDER };
    let mut spawned = false;

    egui::Frame::new()
        .fill(CARD_BG)
        .stroke(egui::Stroke::new(if has { 1.5 } else { 1.0 }, border))
        .corner_radius(egui::CornerRadius::same(12))
        .inner_margin(12.0)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(icon).size(16.0));
                ui.label(egui::RichText::new(label).color(TEXT_BRIGHT));
                ui.add(
                    egui::TextEdit::singleline(path)
                        .desired_width(ui.available_width() - 40.0)
                        .hint_text(placeholder),
                );
                if ui.button("📂").on_hover_text(t!(lang, BrowseDir)).clicked() {
                    spawned = true;
                }
            });
        });

    if spawned {
        Some(spawn_dir_dialog(DialogTarget::Dir(dialog_id)))
    } else {
        None
    }
}
