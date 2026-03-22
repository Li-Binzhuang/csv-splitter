//! Flow visualization and column selection for the split tab.
//! V-shaped adaptive layout: Source → Split on top, Compress → Result on bottom.

use super::flow_cards::{draw_compress_card, draw_result_card, draw_split_card};
use crate::i18n::{t, Lang};
use crate::state::SplitState;
use crate::widgets::*;
use csv_splitter_core::CompressFormat;

/// Draw the V-shaped flow + column filter below.
pub fn draw_flow(
    ui: &mut egui::Ui,
    s: &mut SplitState,
    lang: Lang,
    avail_w: f32,
    dialogs: &mut Vec<PendingDialog>,
) {
    let n_splits = if s.no_split { 1 } else { s.splits.clamp(1, 6) };
    let has_compress = s.compress_idx < CompressFormat::all().len() - 1;

    // Adaptive card sizing: 2 cards + 1 arrow per row
    let card_w = ((avail_w - 50.0) / 2.0).clamp(120.0, 220.0);
    let card_h = 78.0_f32;
    let arrow_w = 20.0_f32;

    let csv_filters: &[(&str, &[&str])] = &[(
        "CSV",
        &[
            "csv", "tsv", "txt", "gz", "zst", "bz2", "xz", "zip", "7z", "tar",
        ],
    )];

    check_and_read_headers(s);

    let input_id = egui::Id::new("split_input");
    let col_id = egui::Id::new("split_columns");

    // Row 1: Source → Split
    ui.horizontal(|ui| {
        if let Some(d) = file_input_card(
            ui,
            &mut s.input_file,
            t!(lang, DropFileHere),
            ACCENT,
            card_w,
            card_h,
            lang,
            csv_filters,
            input_id,
        ) {
            dialogs.push(d);
        }
        draw_arrow(ui, ACCENT, arrow_w, card_h);
        draw_split_card(ui, s, lang, card_w, card_h, n_splits);
    });

    // Down arrow (centered)
    draw_down_arrow(ui, if has_compress { WARN } else { ACCENT }, avail_w);

    // Row 2: Compress → Result
    ui.horizontal(|ui| {
        draw_compress_card(ui, s, lang, card_w, card_h, has_compress);
        draw_arrow(ui, SUCCESS, arrow_w, card_h);
        draw_result_card(ui, s, lang, card_w, card_h);
    });

    ui.add_space(6.0);
    if let Some(d) = file_input_card(
        ui,
        &mut s.columns_file,
        t!(lang, DropColFileHere),
        PURPLE,
        card_w,
        46.0,
        lang,
        &[("CSV/TXT", &["csv", "txt"])],
        col_id,
    ) {
        dialogs.push(d);
    }

    if !s.available_headers.is_empty() {
        ui.add_space(4.0);
        draw_column_selector(ui, s, lang, card_w);
    }
}

/// Draw a vertical down-arrow, centered in the available width.
fn draw_down_arrow(ui: &mut egui::Ui, color: egui::Color32, avail_w: f32) {
    let h = 20.0;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(avail_w, h), egui::Sense::hover());
    let mid_x = rect.center().x;
    let p = ui.painter();
    p.line_segment(
        [
            egui::pos2(mid_x, rect.top() + 2.0),
            egui::pos2(mid_x, rect.bottom() - 6.0),
        ],
        egui::Stroke::new(2.0, color),
    );
    let tip = egui::pos2(mid_x, rect.bottom() - 2.0);
    p.line_segment(
        [egui::pos2(mid_x - 4.0, tip.y - 6.0), tip],
        egui::Stroke::new(2.0, color),
    );
    p.line_segment(
        [egui::pos2(mid_x + 4.0, tip.y - 6.0), tip],
        egui::Stroke::new(2.0, color),
    );
}

fn check_and_read_headers(s: &mut SplitState) {
    let current_path = s.input_file.as_ref().map(|f| f.path.clone());
    let headers_match = match (&current_path, s.available_headers.is_empty()) {
        (None, _) => {
            if !s.available_headers.is_empty() {
                s.available_headers.clear();
                s.selected_columns.clear();
                s.column_search.clear();
            }
            true
        }
        (Some(_), true) => false,
        (Some(_), false) => true,
    };
    if !headers_match {
        if let Some(ref info) = s.input_file {
            let delim = crate::widgets::get_delimiter(s.delim_idx, &s.custom_delim);
            if let Ok(headers) =
                csv_splitter_core::csv_reader::read_headers(&info.path, Some(delim))
            {
                let len = headers.len();
                s.available_headers = headers;
                s.selected_columns = vec![true; len];
                s.column_search.clear();
            }
        }
    }
}

fn draw_column_selector(ui: &mut egui::Ui, s: &mut SplitState, lang: Lang, w: f32) {
    egui::Frame::new()
        .fill(CARD_BG)
        .stroke(egui::Stroke::new(1.0, PURPLE.linear_multiply(0.5)))
        .corner_radius(egui::CornerRadius::same(12))
        .inner_margin(10.0)
        .show(ui, |ui| {
            ui.set_width(w.min(ui.available_width()));

            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(t!(lang, ColumnSelect))
                        .size(12.0)
                        .strong()
                        .color(TEXT_BRIGHT),
                );
                let selected_count = s.selected_columns.iter().filter(|&&v| v).count();
                ui.label(
                    egui::RichText::new(format!(
                        "{}: {}/{}",
                        t!(lang, SelectedCount),
                        selected_count,
                        s.available_headers.len()
                    ))
                    .size(10.0)
                    .color(TEXT_DIM),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button(t!(lang, DeselectAll)).clicked() {
                        s.selected_columns.iter_mut().for_each(|v| *v = false);
                    }
                    if ui.small_button(t!(lang, SelectAll)).clicked() {
                        s.selected_columns.iter_mut().for_each(|v| *v = true);
                    }
                });
            });

            ui.add_space(2.0);
            ui.add(
                egui::TextEdit::singleline(&mut s.column_search)
                    .desired_width(w.min(ui.available_width()) - 16.0)
                    .hint_text(t!(lang, SearchColumns)),
            );

            ui.add_space(2.0);
            egui::ScrollArea::vertical()
                .max_height(150.0)
                .show(ui, |ui| {
                    let search = s.column_search.to_lowercase();
                    for (i, header) in s.available_headers.iter().enumerate() {
                        if !search.is_empty() && !header.to_lowercase().contains(&search) {
                            continue;
                        }
                        if let Some(sel) = s.selected_columns.get_mut(i) {
                            ui.checkbox(
                                sel,
                                egui::RichText::new(header).size(11.0).color(TEXT_BRIGHT),
                            );
                        }
                    }
                });
        });
}
