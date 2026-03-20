use crate::i18n::{t, Lang};
use crate::state::PreviewState;
use crate::widgets::*;
use csv_splitter_core::preview::load_preview;
use std::path::PathBuf;

pub fn ui_preview(ui: &mut egui::Ui, s: &mut PreviewState, lang: Lang) {
    ui.heading(t!(lang, PreviewTitle));
    ui.label(
        egui::RichText::new(t!(lang, PreviewDesc))
            .size(12.0)
            .color(TEXT_DIM),
    );
    ui.add_space(8.0);

    // File input
    file_path_card(
        ui,
        &mut s.path,
        "📄",
        t!(lang, CsvFile),
        t!(lang, FileHint),
        ACCENT,
        lang,
        &[(t!(lang, FilterCsv), &["csv", "tsv", "txt"])],
    );
    ui.add_space(4.0);

    // Params in a card
    egui::Frame::new()
        .fill(CARD_BG)
        .stroke(egui::Stroke::new(1.0, CARD_BORDER))
        .corner_radius(egui::CornerRadius::same(8))
        .inner_margin(10.0)
        .show(ui, |ui| {
            delimiter_combo(ui, &mut s.delim_idx, &mut s.custom_delim, lang);
        });

    ui.add_space(8.0);
    let can_load = !s.path.is_empty();
    if ui
        .add_enabled(
            can_load,
            egui::Button::new(egui::RichText::new(t!(lang, LoadPreview)).size(14.0))
                .min_size([140.0, 32.0].into()),
        )
        .clicked()
    {
        let path = PathBuf::from(&s.path);
        let delim = get_delimiter(s.delim_idx, &s.custom_delim);
        match load_preview(&path, 100, Some(delim)) {
            Ok(p) => s.preview = Some(p),
            Err(_) => s.preview = None,
        }
    }

    if let Some(ref preview) = s.preview {
        ui.add_space(8.0);

        // Stats badges
        egui::Frame::new()
            .fill(CARD_BG)
            .corner_radius(egui::CornerRadius::same(6))
            .inner_margin(10.0)
            .stroke(egui::Stroke::new(1.0, CARD_BORDER))
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    stat_badge(
                        ui,
                        "📄",
                        t!(lang, FileSize),
                        &format_bytes(preview.file_size),
                    );
                    stat_badge(
                        ui,
                        "📊",
                        t!(lang, ApproxRows),
                        &format!("~{}", preview.total_rows_estimate),
                    );
                    stat_badge(
                        ui,
                        "🔤",
                        t!(lang, DetectedDelim),
                        preview.detected_delimiter.label(),
                    );
                    stat_badge(
                        ui,
                        "📋",
                        t!(lang, ColumnCount),
                        &preview.headers.len().to_string(),
                    );
                });
            });

        ui.add_space(6.0);

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
