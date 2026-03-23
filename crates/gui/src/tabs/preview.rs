use crate::i18n::{t, Lang};
use crate::state::PreviewState;
use crate::widgets::*;
use csv_splitter_core::preview::load_preview;

pub fn ui_preview(
    ui: &mut egui::Ui,
    s: &mut PreviewState,
    lang: Lang,
    dialogs: &mut Vec<PendingDialog>,
) {
    // ── Title (centered) ──
    ui.vertical_centered(|ui| {
        ui.heading(egui::RichText::new(t!(lang, PreviewTitle)).color(TEXT_BRIGHT));
        ui.label(
            egui::RichText::new(t!(lang, PreviewDesc))
                .size(12.0)
                .color(TEXT_DIM),
        );
    });
    ui.add_space(8.0);

    // ── Two-column: Input (left) | Params + Action (right) ──
    let (col_w, left_rect, right_rect) = two_col_rects(ui, 8.0);

    let left_h = {
        let mut col = ui.new_child(
            egui::UiBuilder::new()
                .id_salt("preview_left")
                .max_rect(left_rect)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        col.set_max_width(col_w);
        col.set_width(col_w);
        egui::Frame::new()
            .fill(CARD_BG)
            .stroke(egui::Stroke::new(1.0, CARD_BORDER))
            .corner_radius(egui::CornerRadius::same(14))
            .inner_margin(14.0)
            .outer_margin(egui::Margin::symmetric(0, 4))
            .show(&mut col, |ui| {
                ui.label(
                    egui::RichText::new(t!(lang, SectionInput))
                        .size(13.0)
                        .strong()
                        .color(TEXT_BRIGHT),
                );
                ui.add_space(6.0);
                let cw = ui.available_width() - 4.0;
                if let Some(d) = file_input_card(
                    ui,
                    &mut s.input_file,
                    t!(lang, DropFileHere),
                    ACCENT,
                    cw,
                    70.0,
                    lang,
                    &[(t!(lang, FilterCsv), &["csv", "tsv", "txt"])],
                    egui::Id::new("preview_input"),
                ) {
                    dialogs.push(d);
                }
            });
        col.min_rect().height()
    };

    let right_h = {
        let mut col = ui.new_child(
            egui::UiBuilder::new()
                .id_salt("preview_right")
                .max_rect(right_rect)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        col.set_max_width(col_w);
        col.set_width(col_w);
        egui::Frame::new()
            .fill(CARD_BG)
            .stroke(egui::Stroke::new(1.0, CARD_BORDER))
            .corner_radius(egui::CornerRadius::same(14))
            .inner_margin(14.0)
            .outer_margin(egui::Margin::symmetric(0, 4))
            .show(&mut col, |ui| {
                ui.label(
                    egui::RichText::new(t!(lang, SectionParams))
                        .size(13.0)
                        .strong()
                        .color(TEXT_BRIGHT),
                );
                ui.add_space(6.0);
                delimiter_combo(ui, &mut s.delim_idx, &mut s.custom_delim, lang);
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(t!(lang, PreviewRows)).color(TEXT_BRIGHT));
                    ui.add(egui::DragValue::new(&mut s.preview_rows).range(10..=10_000));
                });
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(t!(lang, SectionAction))
                        .size(13.0)
                        .strong()
                        .color(TEXT_BRIGHT),
                );
                ui.add_space(4.0);
                let can_load = s.input_file.is_some();
                if ui
                    .add_enabled(
                        can_load,
                        egui::Button::new(egui::RichText::new(t!(lang, LoadPreview)).size(14.0))
                            .min_size([120.0, 32.0].into()),
                    )
                    .clicked()
                {
                    if let Some(ref info) = s.input_file {
                        let delim = get_delimiter(s.delim_idx, &s.custom_delim);
                        match load_preview(&info.path, s.preview_rows, Some(delim)) {
                            Ok(p) => s.preview = Some(p),
                            Err(_) => s.preview = None,
                        }
                    }
                }
            });
        col.min_rect().height()
    };

    ui.allocate_space(egui::vec2(ui.available_width(), left_h.max(right_h)));

    if let Some(ref preview) = s.preview {
        let remaining_h = ui.available_height().max(250.0);
        section_frame(ui, t!(lang, SectionResult), |ui| {
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
            ui.add_space(6.0);
            egui::ScrollArea::both()
                .auto_shrink([false, false])
                .min_scrolled_height(remaining_h - 80.0)
                .show(ui, |ui| {
                    egui::Grid::new("preview_grid")
                        .striped(true)
                        .min_col_width(80.0)
                        .spacing([12.0, 4.0])
                        .show(ui, |ui| {
                            for h in &preview.headers {
                                ui.label(
                                    egui::RichText::new(h)
                                        .strong()
                                        .size(13.0)
                                        .color(TEXT_BRIGHT),
                                );
                            }
                            ui.end_row();
                            for row in &preview.rows {
                                for (i, _) in preview.headers.iter().enumerate() {
                                    let val = row.get(i).map(|s| s.as_str()).unwrap_or("");
                                    ui.label(egui::RichText::new(val).color(TEXT_BRIGHT));
                                }
                                ui.end_row();
                            }
                        });
                });
        });
    }
}
