//! Flow visualization with 2×2 grid layout (田字).
//!
//! Full (split+compress):
//!   [Source] → [Split]
//!                 ↓
//!   [Result] ← [Compress]
//!
//! Only split:    [Source] → [Split] → [Result]
//! Only compress: [Source] → [Compress] → [Result]
//! Neither:       [Source] → [Result]

use super::flow_cards::{draw_compress_card, draw_result_card, draw_split_card};
use crate::i18n::{t, Lang};
use crate::state::SplitState;
use crate::widgets::*;
use csv_splitter_core::CompressFormat;

/// Draw the flow pipeline.
pub fn draw_flow(
    ui: &mut egui::Ui,
    s: &mut SplitState,
    lang: Lang,
    dialogs: &mut Vec<PendingDialog>,
) {
    let n_splits = if s.no_split { 1 } else { s.splits.clamp(1, 6) };
    let has_compress = !s.no_compress && s.compress_idx < CompressFormat::all().len() - 1;

    check_and_read_headers(s);
    check_filter_file(s);

    let avail_w = ui.available_width();
    let arrow_sz = 20.0_f32;
    let spacing = ui.spacing().item_spacing.x;

    // card_w = desired OUTER width of each card (including frame margins + stroke).
    // Row layout: [card_outer] <spacing> [arrow] <spacing> [card_outer]
    // => 2*card_w + arrow_sz + 2*spacing = avail_w
    let card_w = ((avail_w - arrow_sz - spacing * 2.0) / 2.0)
        .floor()
        .max(120.0);
    let card_h = 90.0_f32;

    let csv_filters: &[(&str, &[&str])] = &[(
        "CSV",
        &[
            "csv", "tsv", "txt", "gz", "zst", "bz2", "xz", "zip", "7z", "tar",
        ],
    )];
    let input_id = egui::Id::new("split_input");

    // Always draw the 2×2 grid to keep height stable
    draw_flow_grid(
        ui,
        s,
        lang,
        dialogs,
        card_w,
        card_h,
        arrow_sz,
        n_splits,
        has_compress,
        csv_filters,
        input_id,
    );
}

/// Toggle switches inline with section title.
pub fn draw_toggles(ui: &mut egui::Ui, s: &mut SplitState, lang: Lang) {
    ui.spacing_mut().item_spacing.x = 12.0;
    let split_on = !s.no_split;
    let split_label = if split_on {
        egui::RichText::new(format!("✅ {}", t!(lang, EnableSplit)))
            .size(11.0)
            .color(ACCENT)
    } else {
        egui::RichText::new(format!("⬜ {}", t!(lang, EnableSplit)))
            .size(11.0)
            .color(TEXT_DIM)
    };
    if ui.selectable_label(split_on, split_label).clicked() {
        s.no_split = !s.no_split;
    }
    let compress_on = !s.no_compress;
    let compress_label = if compress_on {
        egui::RichText::new(format!("✅ {}", t!(lang, EnableCompress)))
            .size(11.0)
            .color(WARN)
    } else {
        egui::RichText::new(format!("⬜ {}", t!(lang, EnableCompress)))
            .size(11.0)
            .color(TEXT_DIM)
    };
    if ui.selectable_label(compress_on, compress_label).clicked() {
        s.no_compress = !s.no_compress;
    }
}

/// 2×2 grid: [Source]→[Split] / [Result]←[Compress], with ↓ on right column.
#[allow(clippy::too_many_arguments)]
fn draw_flow_grid(
    ui: &mut egui::Ui,
    s: &mut SplitState,
    lang: Lang,
    dialogs: &mut Vec<PendingDialog>,
    card_w: f32,
    card_h: f32,
    arrow_sz: f32,
    n_splits: usize,
    has_compress: bool,
    csv_filters: &[(&str, &[&str])],
    input_id: egui::Id,
) {
    let row_w = ui.available_width();

    // Row 1: [Source] → [Split]
    ui.horizontal(|ui| {
        ui.set_max_width(row_w);
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
        draw_arrow(ui, ACCENT, arrow_sz, card_h);
        draw_split_card(ui, s, lang, card_w, card_h, n_splits);
    });

    // Down arrow on right column
    ui.horizontal(|ui| {
        ui.add_space(card_w + arrow_sz + ui.spacing().item_spacing.x * 2.0);
        draw_arrow_down(ui, WARN, card_w, arrow_sz);
    });

    // Row 2: [Action] ← [Compress]
    ui.horizontal(|ui| {
        ui.set_max_width(row_w);
        draw_result_card(ui, s, lang, card_w, card_h, dialogs);
        draw_arrow_left(ui, SUCCESS, arrow_sz, card_h);
        draw_compress_card(ui, s, lang, card_w, card_h, has_compress);
    });
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
                apply_headers(s, headers);
            }
        }
    }
}

/// Check if the filter file changed and re-read its headers.
fn check_filter_file(s: &mut SplitState) {
    let current = s.columns_file.as_ref().map(|f| f.path.clone());
    if current != s.last_filter_path {
        s.last_filter_path = current.clone();
        if current.is_some() {
            read_filter_headers(s);
        } else {
            s.filter_headers.clear();
            s.selected_columns.iter_mut().for_each(|v| *v = true);
        }
    }
}

/// Read headers from the column filter file into `filter_headers`.
fn read_filter_headers(s: &mut SplitState) {
    if let Some(ref info) = s.columns_file {
        let delim = crate::widgets::get_delimiter(s.delim_idx, &s.custom_delim);
        if let Ok(headers) = csv_splitter_core::csv_reader::read_headers(&info.path, Some(delim)) {
            s.filter_headers = headers;
            // If main CSV headers already loaded, apply intersection
            if !s.available_headers.is_empty() {
                let filter_set: std::collections::HashSet<&str> =
                    s.filter_headers.iter().map(|h| h.as_str()).collect();
                for (i, h) in s.available_headers.iter().enumerate() {
                    if let Some(sel) = s.selected_columns.get_mut(i) {
                        *sel = filter_set.contains(h.as_str());
                    }
                }
            }
        }
    } else {
        s.filter_headers.clear();
    }
}

/// Apply headers to state, auto-selecting based on filter_headers if present.
fn apply_headers(s: &mut SplitState, headers: Vec<String>) {
    let len = headers.len();
    s.available_headers = headers;
    if s.filter_headers.is_empty() {
        s.selected_columns = vec![true; len];
    } else {
        let filter_set: std::collections::HashSet<&str> =
            s.filter_headers.iter().map(|h| h.as_str()).collect();
        s.selected_columns = s
            .available_headers
            .iter()
            .map(|h| filter_set.contains(h.as_str()))
            .collect();
    }
    s.column_search.clear();
}
