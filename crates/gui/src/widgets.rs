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
) {
    ui.horizontal(|ui| {
        ui.label(label);
        let response = ui.add(
            egui::TextEdit::singleline(value)
                .desired_width(ui.available_width() - 40.0)
                .hint_text("点击右侧按钮选择文件..."),
        );
        if ui.button("📂").on_hover_text("浏览文件").clicked() {
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
pub fn dir_picker(ui: &mut egui::Ui, label: &str, value: &mut String) {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.add(
            egui::TextEdit::singleline(value)
                .desired_width(ui.available_width() - 40.0)
                .hint_text("点击右侧按钮选择目录..."),
        );
        if ui.button("📂").on_hover_text("浏览目录").clicked() {
            if let Some(path) = rfd::FileDialog::new().pick_folder() {
                *value = path.display().to_string();
            }
        }
    });
}

/// Delimiter combo box.
pub fn delimiter_combo(ui: &mut egui::Ui, idx: &mut usize, custom: &mut String) {
    let common = Delimiter::common();
    let labels: Vec<&str> = common
        .iter()
        .map(|d| d.label())
        .chain(std::iter::once("自定义"))
        .collect();
    ui.horizontal(|ui| {
        ui.label("分隔符:");
        egui::ComboBox::from_id_salt(ui.next_auto_id())
            .selected_text(labels[*idx])
            .width(160.0)
            .show_ui(ui, |ui| {
                for (i, label) in labels.iter().enumerate() {
                    ui.selectable_value(idx, i, *label);
                }
            });
        if *idx >= common.len() {
            ui.label("自定义字符:");
            ui.add(egui::TextEdit::singleline(custom).desired_width(40.0));
        }
    });
}

/// Scrollable log area.
pub fn log_panel(ui: &mut egui::Ui, logs: &[String]) {
    if logs.is_empty() {
        return;
    }
    ui.separator();
    ui.label("📋 日志:");
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
                        ui.label(egui::RichText::new(line).size(12.0).family(egui::FontFamily::Monospace));
                    }
                });
        });
}

/// Job status indicator.
pub fn job_status(ui: &mut egui::Ui, state: &crate::state::JobState) {
    match state {
        crate::state::JobState::Running => {
            ui.spinner();
            ui.label("处理中...");
        }
        crate::state::JobState::Done => {
            ui.label(egui::RichText::new("✅ 完成").color(egui::Color32::from_rgb(80, 200, 120)));
        }
        crate::state::JobState::Error => {
            ui.label(egui::RichText::new("❌ 失败").color(egui::Color32::from_rgb(240, 80, 80)));
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
