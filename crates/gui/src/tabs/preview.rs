use crate::state::PreviewState;
use crate::widgets::*;
use csv_splitter_core::preview::load_preview;
use std::path::PathBuf;

pub fn ui_preview(ui: &mut egui::Ui, s: &mut PreviewState) {
    ui.heading("👁 数据预览");
    ui.label(egui::RichText::new("快速预览 CSV 文件的前 100 行数据").size(12.0).weak());
    ui.add_space(6.0);

    file_picker(
        ui, "CSV 文件:", &mut s.path,
        &[("CSV", &["csv", "tsv", "txt"])],
    );
    delimiter_combo(ui, &mut s.delim_idx, &mut s.custom_delim);

    ui.add_space(4.0);
    let can_load = !s.path.is_empty();
    if ui.add_enabled(can_load, egui::Button::new("📖 加载预览").min_size([120.0, 28.0].into())).clicked() {
        let path = PathBuf::from(&s.path);
        let delim = get_delimiter(s.delim_idx, &s.custom_delim);
        match load_preview(&path, 100, Some(delim)) {
            Ok(p) => { s.preview = Some(p); }
            Err(_e) => { s.preview = None; }
        }
    }

    if let Some(ref preview) = s.preview {
        ui.add_space(6.0);

        // File info bar
        egui::Frame::new()
            .fill(ui.visuals().faint_bg_color)
            .corner_radius(egui::CornerRadius::same(6))
            .inner_margin(8.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(format!("📄 大小: {}", format_bytes(preview.file_size)));
                    ui.separator();
                    ui.label(format!("📊 约 {} 行", preview.total_rows_estimate));
                    ui.separator();
                    ui.label(format!("🔤 分隔符: {}", preview.detected_delimiter.label()));
                    ui.separator();
                    ui.label(format!("📋 {} 列", preview.headers.len()));
                });
            });

        ui.add_space(4.0);

        // Data table
        egui::ScrollArea::both()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                egui::Grid::new("preview_grid")
                    .striped(true)
                    .min_col_width(80.0)
                    .spacing([12.0, 4.0])
                    .show(ui, |ui| {
                        for h in &preview.headers {
                            ui.label(egui::RichText::new(h).strong().size(13.0));
                        }
                        ui.end_row();

                        for row in &preview.rows {
                            for (i, _) in preview.headers.iter().enumerate() {
                                let val = row.get(i).map(|s| s.as_str()).unwrap_or("");
                                ui.label(val);
                            }
                            ui.end_row();
                        }
                    });
            });
    }
}
