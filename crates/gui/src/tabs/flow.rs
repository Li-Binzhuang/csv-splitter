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
    let split_on = !s.no_split;
    let compress_on = !s.no_compress;

    check_and_read_headers(s);

    let avail_w = ui.available_width();
    let arrow_sz = 20.0_f32;
    let spacing = ui.spacing().item_spacing.x;

    // Card dimensions: narrower, taller
    let card_w = ((avail_w - arrow_sz - spacing * 3.0) / 2.0).clamp(120.0, 380.0);
    let card_h = 90.0_f32;

    let csv_filters: &[(&str, &[&str])] = &[(
        "CSV",
        &[
            "csv", "tsv", "txt", "gz", "zst", "bz2", "xz", "zip", "7z", "tar",
        ],
    )];
    let input_id = egui::Id::new("split_input");
    let n_active = split_on as usize + compress_on as usize;

    match n_active {
        2 => draw_flow_grid(
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
        ),
        1 => draw_flow_row3(
            ui,
            s,
            lang,
            dialogs,
            card_w,
            card_h,
            arrow_sz,
            n_splits,
            has_compress,
            split_on,
            csv_filters,
            input_id,
        ),
        _ => draw_flow_row2(
            ui,
            s,
            lang,
            dialogs,
            card_w,
            card_h,
            arrow_sz,
            csv_filters,
            input_id,
        ),
    }
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
    // Row 1: [Source] → [Split]
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
        draw_arrow(ui, ACCENT, arrow_sz, card_h);
        draw_split_card(ui, s, lang, card_w, card_h, n_splits);
    });

    // Down arrow on right column
    ui.horizontal(|ui| {
        ui.add_space(card_w + arrow_sz + ui.spacing().item_spacing.x * 2.0);
        draw_arrow_down(ui, WARN, card_w, arrow_sz);
    });

    // Row 2: [Result] ← [Compress]
    ui.horizontal(|ui| {
        draw_result_card(ui, s, lang, card_w, card_h);
        draw_arrow_left(ui, SUCCESS, arrow_sz, card_h);
        draw_compress_card(ui, s, lang, card_w, card_h, has_compress);
    });
}

/// Single middle step: [Source] → [Middle] → [Result]
#[allow(clippy::too_many_arguments)]
fn draw_flow_row3(
    ui: &mut egui::Ui,
    s: &mut SplitState,
    lang: Lang,
    dialogs: &mut Vec<PendingDialog>,
    card_w: f32,
    card_h: f32,
    arrow_sz: f32,
    n_splits: usize,
    has_compress: bool,
    split_on: bool,
    csv_filters: &[(&str, &[&str])],
    input_id: egui::Id,
) {
    let small_card = ((card_w * 2.0 + arrow_sz) / 3.0 - arrow_sz).clamp(90.0, 260.0);
    let small_arrow = arrow_sz.min(18.0);
    ui.horizontal(|ui| {
        if let Some(d) = file_input_card(
            ui,
            &mut s.input_file,
            t!(lang, DropFileHere),
            ACCENT,
            small_card,
            card_h,
            lang,
            csv_filters,
            input_id,
        ) {
            dialogs.push(d);
        }
        draw_arrow(ui, ACCENT, small_arrow, card_h);
        if split_on {
            draw_split_card(ui, s, lang, small_card, card_h, n_splits);
        } else {
            draw_compress_card(ui, s, lang, small_card, card_h, has_compress);
        }
        draw_arrow(ui, SUCCESS, small_arrow, card_h);
        draw_result_card(ui, s, lang, small_card, card_h);
    });
}

/// No middle steps: [Source] → [Result]
#[allow(clippy::too_many_arguments)]
fn draw_flow_row2(
    ui: &mut egui::Ui,
    s: &mut SplitState,
    lang: Lang,
    dialogs: &mut Vec<PendingDialog>,
    card_w: f32,
    card_h: f32,
    arrow_sz: f32,
    csv_filters: &[(&str, &[&str])],
    input_id: egui::Id,
) {
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
        draw_arrow(ui, SUCCESS, arrow_sz, card_h);
        draw_result_card(ui, s, lang, card_w, card_h);
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
                let len = headers.len();
                s.available_headers = headers;
                s.selected_columns = vec![true; len];
                s.column_search.clear();
            }
        }
    }
}
