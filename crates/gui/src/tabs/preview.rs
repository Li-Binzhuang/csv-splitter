use crate::i18n::{t, Lang};
use crate::state::PreviewState;
use crate::widgets::*;
use csv_splitter_core::preview::load_preview;
use std::path::PathBuf;

pub fn ui_preview(ui: &mut egui::Ui, s: &mut PreviewState, lang: Lang) {
    ui.heading(t!(lang, PreviewTitle));
    ui.label(egui::RichText::new(t!(lang, PreviewDesc)).size(12.0).weak());
    ui.add_space(6.0);

    file_picker(
        ui, t!(lang, CsvFile), &mut s.path,
        &[(t!(lang, FilterCsv), &["csv", "tsv", "txt"])],
        lang,
    );
    delimiter_combo(ui, &mut s.delim_idx, &mut s.custom_delim, lang);

    ui.add_space(4.0);
    let can_load = !s.path.is_empty();
    if ui.add_enabled(can_load, egui::Button::new(t!(lang, LoadPreview)).min_size([120.0, 28.0].into())).clicked() {
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
                    ui.label(format!("📄 {}: {}", t!(lang, FileSize), format_bytes(preview.file_size)));
                    ui.separator();
                    ui.label(format!("📊 ~{} {}", preview.total_rows_estimate, t!(lang, ApproxRows)));
                    ui.separator();
                    ui.label(format!("🔤 {}: {}", t!(lang, DetectedDelim), preview.detected_delimiter.label()));
                    ui.separator();
                    ui.label(format!("📋 {} {}", preview.headers.len(), t!(lang, ColumnCount)));
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
