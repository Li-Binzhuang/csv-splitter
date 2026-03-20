use crate::i18n::{t, Lang};
use csv_splitter_core::Delimiter;

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

/// File picker: label + text input + browse button.
pub fn file_picker(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut String,
    filters: &[(&str, &[&str])],
    lang: Lang,
) {
    ui.horizontal(|ui| {
        ui.label(label);
        let response = ui.add(
            egui::TextEdit::singleline(value)
                .desired_width(ui.available_width() - 40.0)
                .hint_text(t!(lang, FileHint)),
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
            if let Some(path) = dialog.pick_file() {
                *value = path.display().to_string();
                response.request_focus();
            }
        }
    });
}

/// Directory picker: label + text input + browse button.
#[allow(dead_code)]
pub fn dir_picker(ui: &mut egui::Ui, label: &str, value: &mut String, lang: Lang) {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.add(
            egui::TextEdit::singleline(value)
                .desired_width(ui.available_width() - 40.0)
                .hint_text(t!(lang, DirHint)),
        );
        if ui.button("📂").on_hover_text(t!(lang, BrowseDir)).clicked() {
            if let Some(path) = rfd::FileDialog::new().pick_folder() {
                *value = path.display().to_string();
            }
        }
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
            ui.label(
                egui::RichText::new(t!(lang, Done)).color(egui::Color32::from_rgb(80, 200, 120)),
            );
        }
        crate::state::JobState::Error => {
            ui.label(
                egui::RichText::new(t!(lang, Failed)).color(egui::Color32::from_rgb(240, 80, 80)),
            );
        }
        crate::state::JobState::Idle => {}
    }
}

/// Section header with separator.
pub fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(8.0);
    ui.separator();
    ui.add_space(4.0);
    ui.label(egui::RichText::new(title).size(14.0).strong());
    ui.add_space(2.0);
}

/// Compact stat badge: icon + label + value in a rounded pill.
pub fn stat_badge(ui: &mut egui::Ui, icon: &str, label: &str, value: &str) {
    egui::Frame::new()
        .fill(egui::Color32::from_rgb(40, 44, 55))
        .corner_radius(egui::CornerRadius::same(12))
        .inner_margin(egui::Margin::symmetric(8, 3))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 3.0;
                ui.label(egui::RichText::new(icon).size(12.0));
                ui.label(egui::RichText::new(label).size(11.0).weak());
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
