use crate::i18n::{t, Lang};
use crate::state::FileInfo;
use csv_splitter_core::Delimiter;

// ===== Theme colors (centralized) =====
pub const CARD_BG: egui::Color32 = egui::Color32::from_rgb(38, 42, 54);
pub const CARD_BORDER: egui::Color32 = egui::Color32::from_rgb(58, 63, 80);
pub const ACCENT: egui::Color32 = egui::Color32::from_rgb(88, 166, 255);
pub const SUCCESS: egui::Color32 = egui::Color32::from_rgb(80, 200, 120);
pub const WARN: egui::Color32 = egui::Color32::from_rgb(255, 180, 60);
pub const ERROR_C: egui::Color32 = egui::Color32::from_rgb(240, 80, 80);
pub const PURPLE: egui::Color32 = egui::Color32::from_rgb(180, 140, 255);
pub const TEXT_DIM: egui::Color32 = egui::Color32::from_rgb(130, 138, 155);

/// Format bytes into human-readable string.
pub fn format_bytes(b: u64) -> String {
    if b >= 1 << 30 {
        format!("{:.2} GB", b as f64 / (1u64 << 30) as f64)
    } else if b >= 1 << 20 {
        format!("{:.1} MB", b as f64 / (1u64 << 20) as f64)
    } else if b >= 1 << 10 {
        format!("{:.1} KB", b as f64 / (1u64 << 10) as f64)
    } else {
        format!("{} B", b)
    }
}

/// Reusable file input card with click-to-browse + metadata display.
/// Works for all tabs. Returns true if file was just changed.
pub fn file_input_card(
    ui: &mut egui::Ui,
    file: &mut Option<FileInfo>,
    placeholder: &str,
    accent: egui::Color32,
    w: f32,
    min_h: f32,
    lang: Lang,
    filters: &[(&str, &[&str])],
) {
    let has_file = file.is_some();
    let border = if has_file { accent } else { CARD_BORDER };

    let resp = egui::Frame::new()
        .fill(CARD_BG)
        .stroke(egui::Stroke::new(if has_file { 1.5 } else { 1.0 }, border))
        .corner_radius(egui::CornerRadius::same(8))
        .inner_margin(10.0)
        .show(ui, |ui| {
            ui.set_width(w.min(ui.available_width()));
            ui.set_min_height(min_h);

            if let Some(ref info) = file {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("📄").size(16.0));
                    ui.vertical(|ui| {
                        ui.label(egui::RichText::new(&info.name).size(12.0).strong());
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing.x = 8.0;
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
                                    egui::RichText::new(format!(
                                        "{}: {}",
                                        t!(lang, FileType),
                                        &info.extension
                                    ))
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
                                        egui::RichText::new(format!(
                                            "{}: {}",
                                            t!(lang, FileModified),
                                            ts
                                        ))
                                        .size(10.0)
                                        .color(TEXT_DIM),
                                    );
                                }
                            }
                        });
                    });
                    // Remove button on the right
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
                        if ui
                            .small_button("✕")
                            .on_hover_text(t!(lang, RemoveFile))
                            .clicked()
                        {
                            // handled below
                        }
                    });
                });
            } else {
                ui.vertical_centered(|ui| {
                    ui.add_space((min_h - 30.0).max(0.0) / 2.0);
                    ui.label(egui::RichText::new(placeholder).size(11.0).color(TEXT_DIM));
                });
            }
        });

    // Hover cursor
    if resp.response.hovered() && !has_file {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    // Click to browse (only when empty — when file exists, use ✕ to remove)
    if resp.response.clicked() && !has_file {
        let mut dialog = rfd::FileDialog::new();
        for (name, exts) in filters {
            dialog = dialog.add_filter(*name, exts);
        }
        if let Some(path) = dialog.pick_file() {
            *file = FileInfo::from_path(path);
        }
    }
}

/// File input card using a path string (for tabs that don't use FileInfo).
pub fn file_path_card(
    ui: &mut egui::Ui,
    path: &mut String,
    icon: &str,
    label: &str,
    placeholder: &str,
    accent: egui::Color32,
    lang: Lang,
    filters: &[(&str, &[&str])],
) {
    let has_file = !path.is_empty();
    let border = if has_file { accent } else { CARD_BORDER };

    egui::Frame::new()
        .fill(CARD_BG)
        .stroke(egui::Stroke::new(if has_file { 1.5 } else { 1.0 }, border))
        .corner_radius(egui::CornerRadius::same(8))
        .inner_margin(10.0)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(icon).size(16.0));
                ui.label(label);
                ui.add(
                    egui::TextEdit::singleline(path)
                        .desired_width(ui.available_width() - 40.0)
                        .hint_text(placeholder),
                );
                if ui
                    .button("📂")
                    .on_hover_text(t!(lang, BrowseFile))
                    .clicked()
                {
                    let mut dialog = rfd::FileDialog::new();
                    for (name, exts) in filters {
                        dialog = dialog.add_filter(*name, exts);
                    }
                    if let Some(p) = dialog.pick_file() {
                        *path = p.display().to_string();
                    }
                }
            });
        });
}

/// Directory input card.
pub fn dir_path_card(
    ui: &mut egui::Ui,
    path: &mut String,
    icon: &str,
    label: &str,
    placeholder: &str,
    lang: Lang,
) {
    let has = !path.is_empty();
    let border = if has { ACCENT } else { CARD_BORDER };

    egui::Frame::new()
        .fill(CARD_BG)
        .stroke(egui::Stroke::new(if has { 1.5 } else { 1.0 }, border))
        .corner_radius(egui::CornerRadius::same(8))
        .inner_margin(10.0)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(icon).size(16.0));
                ui.label(label);
                ui.add(
                    egui::TextEdit::singleline(path)
                        .desired_width(ui.available_width() - 40.0)
                        .hint_text(placeholder),
                );
                if ui.button("📂").on_hover_text(t!(lang, BrowseDir)).clicked() {
                    if let Some(p) = rfd::FileDialog::new().pick_folder() {
                        *path = p.display().to_string();
                    }
                }
            });
        });
}

/// Delimiter combo box.
pub fn delimiter_combo(ui: &mut egui::Ui, idx: &mut usize, custom: &mut String, lang: Lang) {
    let common = Delimiter::common();
    let labels: Vec<&str> = common
        .iter()
        .map(|d| d.label())
        .chain(std::iter::once(t!(lang, CustomDelim)))
        .collect();
    ui.horizontal(|ui| {
        ui.label(t!(lang, Delimiter));
        egui::ComboBox::from_id_salt(ui.next_auto_id())
            .selected_text(labels[*idx])
            .width(160.0)
            .show_ui(ui, |ui| {
                for (i, label) in labels.iter().enumerate() {
                    ui.selectable_value(idx, i, *label);
                }
            });
        if *idx >= common.len() {
            ui.label(t!(lang, CustomChar));
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
        .corner_radius(egui::CornerRadius::same(6))
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

/// Compact stat badge: icon + label + value in a rounded pill.
pub fn stat_badge(ui: &mut egui::Ui, icon: &str, label: &str, value: &str) {
    egui::Frame::new()
        .fill(egui::Color32::from_rgb(44, 48, 62))
        .corner_radius(egui::CornerRadius::same(12))
        .inner_margin(egui::Margin::symmetric(8, 3))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 3.0;
                ui.label(egui::RichText::new(icon).size(12.0));
                ui.label(egui::RichText::new(label).size(11.0).color(TEXT_DIM));
                ui.label(egui::RichText::new(value).size(11.0).strong());
            });
        });
}

/// Get delimiter from index + custom string.
pub fn get_delimiter(idx: usize, custom: &str) -> Delimiter {
    let common = Delimiter::common();
    if idx < common.len() {
        common[idx]
    } else {
        let b = custom.as_bytes().first().copied().unwrap_or(b',');
        Delimiter::Custom(b)
    }
}
