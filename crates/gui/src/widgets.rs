use crate::i18n::{t, Lang};
use crate::state::FileInfo;
use csv_splitter_core::Delimiter;

// ===== Theme colors (centralized) =====
pub const CARD_BG: egui::Color32 = egui::Color32::from_rgb(52, 58, 76);
pub const CARD_BORDER: egui::Color32 = egui::Color32::from_rgb(78, 86, 108);
pub const ACCENT: egui::Color32 = egui::Color32::from_rgb(88, 166, 255);
pub const SUCCESS: egui::Color32 = egui::Color32::from_rgb(80, 200, 120);
pub const WARN: egui::Color32 = egui::Color32::from_rgb(255, 180, 60);
pub const ERROR_C: egui::Color32 = egui::Color32::from_rgb(240, 80, 80);
pub const PURPLE: egui::Color32 = egui::Color32::from_rgb(180, 140, 255);
pub const TEXT_DIM: egui::Color32 = egui::Color32::from_rgb(175, 185, 205);
pub const TEXT_BRIGHT: egui::Color32 = egui::Color32::from_rgb(232, 236, 248);

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
/// Works for all tabs.
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
) {
    let has_file = file.is_some();
    let border = if has_file { accent } else { CARD_BORDER };
    let mut remove_clicked = false;

    let resp = egui::Frame::new()
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
                // Truncate long filenames to fit the card
                let max_name_chars = ((max_w - 50.0) / 7.0).max(8.0) as usize;
                let display_name = if info.name.chars().count() > max_name_chars {
                    let truncated: String = info.name.chars().take(max_name_chars - 2).collect();
                    format!("{}…", truncated)
                } else {
                    info.name.clone()
                };

                ui.horizontal(|ui| {
                    ui.set_max_width(max_w - 4.0);
                    ui.label(egui::RichText::new("📄").size(16.0));
                    ui.vertical(|ui| {
                        ui.set_max_width(max_w - 60.0);
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
                    if ui
                        .small_button("✕")
                        .on_hover_text(t!(lang, RemoveFile))
                        .clicked()
                    {
                        remove_clicked = true;
                    }
                });
            } else {
                ui.vertical_centered(|ui| {
                    ui.add_space((min_h - 30.0).max(0.0) / 2.0);
                    ui.label(egui::RichText::new(placeholder).size(11.0).color(TEXT_DIM));
                });
            }
        });

    // Handle remove
    if remove_clicked {
        *file = None;
        return;
    }

    // Make the whole card area clickable for browse
    let card_rect = resp.response.rect;
    let click_resp = ui.interact(
        card_rect,
        resp.response.id.with("click"),
        egui::Sense::click(),
    );

    if !has_file && click_resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    if click_resp.clicked() && !has_file {
        let mut dialog = rfd::FileDialog::new();
        for (name, exts) in filters {
            dialog = dialog.add_filter(*name, exts);
        }
        if let Some(path) = dialog.pick_file() {
            *file = FileInfo::from_path(path);
        }
    }
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

/// Compact stat badge: icon + label + value in a rounded pill.
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

/// Titled section frame — groups content with a header label.
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
