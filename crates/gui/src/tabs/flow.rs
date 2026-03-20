//! Flow visualization cards and arrows for the split tab.

use crate::i18n::{t, Lang};
use crate::state::{JobState, SplitState};
use crate::widgets::*;
use csv_splitter_core::CompressFormat;

/// Draw the full flow: Source → Split → Compress → Result + column filter below.
pub fn draw_flow(ui: &mut egui::Ui, s: &mut SplitState, lang: Lang, avail_w: f32) {
    let n_splits = if s.no_split {
        1
    } else {
        s.splits.max(1).min(6)
    };
    let has_compress = s.compress_idx < CompressFormat::all().len() - 1;

    let compact = avail_w < 700.0;
    let card_w = if compact {
        ((avail_w - 40.0) / 2.0).min(180.0).max(100.0)
    } else {
        ((avail_w - 100.0) / 4.0).min(180.0).max(120.0)
    };
    let card_h = if compact { 66.0 } else { 82.0 };
    let arrow_w = if compact { 12.0 } else { 18.0 };
    let csv_filters: &[(&str, &[&str])] = &[(
        "CSV",
        &[
            "csv", "tsv", "txt", "gz", "zst", "bz2", "xz", "zip", "7z", "tar",
        ],
    )];

    // Auto-read headers when input file changes
    check_and_read_headers(s);

    if compact {
        ui.horizontal(|ui| {
            file_input_card(
                ui,
                &mut s.input_file,
                t!(lang, DropFileHere),
                ACCENT,
                card_w,
                card_h,
                lang,
                csv_filters,
            );
            draw_arrow(ui, ACCENT, arrow_w, card_h);
            draw_split_card(ui, s, lang, card_w, card_h, n_splits);
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            draw_compress_card(ui, s, lang, card_w, card_h, has_compress);
            draw_arrow(ui, SUCCESS, arrow_w, card_h);
            draw_result_card(ui, s, lang, card_w, card_h);
        });
    } else {
        ui.horizontal(|ui| {
            file_input_card(
                ui,
                &mut s.input_file,
                t!(lang, DropFileHere),
                ACCENT,
                card_w,
                card_h,
                lang,
                csv_filters,
            );
            draw_arrow(ui, ACCENT, arrow_w, card_h);
            draw_split_card(ui, s, lang, card_w, card_h, n_splits);
            draw_arrow(
                ui,
                if has_compress { WARN } else { ACCENT },
                arrow_w,
                card_h,
            );
            draw_compress_card(ui, s, lang, card_w, card_h, has_compress);
            draw_arrow(ui, SUCCESS, arrow_w, card_h);
            draw_result_card(ui, s, lang, card_w, card_h);
        });
    }

    // Column filter section
    ui.add_space(6.0);

    // Column filter file card — same width as the input card
    file_input_card(
        ui,
        &mut s.columns_file,
        t!(lang, DropColFileHere),
        PURPLE,
        card_w,
        46.0,
        lang,
        &[("CSV/TXT", &["csv", "txt"])],
    );

    // Column selector from headers (only when headers are available)
    if !s.available_headers.is_empty() {
        ui.add_space(4.0);
        draw_column_selector(ui, s, lang, card_w);
    }
}

/// Check if input file changed and read headers if needed.
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
        (Some(_), true) => false, // have file but no headers yet
        (Some(_), false) => true, // already loaded
    };
    if !headers_match {
        if let Some(ref info) = s.input_file {
            let delim = crate::widgets::get_delimiter(s.delim_idx, &s.custom_delim);
            if let Ok(headers) =
                csv_splitter_core::csv_reader::read_headers(&info.path, Some(delim))
            {
                let len = headers.len();
                s.available_headers = headers;
                s.selected_columns = vec![true; len]; // all selected by default
                s.column_search.clear();
            }
        }
    }
}

/// Draw the searchable multi-select column selector.
fn draw_column_selector(ui: &mut egui::Ui, s: &mut SplitState, lang: Lang, w: f32) {
    egui::Frame::new()
        .fill(CARD_BG)
        .stroke(egui::Stroke::new(1.0, PURPLE.linear_multiply(0.5)))
        .corner_radius(egui::CornerRadius::same(8))
        .inner_margin(8.0)
        .show(ui, |ui| {
            ui.set_width(w.min(ui.available_width()));

            // Header row: label + select all / deselect all
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

            // Search box
            ui.add_space(2.0);
            ui.add(
                egui::TextEdit::singleline(&mut s.column_search)
                    .desired_width(w.min(ui.available_width()) - 16.0)
                    .hint_text(t!(lang, SearchColumns)),
            );

            // Scrollable checkbox list
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
                            ui.checkbox(sel, egui::RichText::new(header).size(11.0));
                        }
                    }
                });
        });
}

fn draw_split_card(ui: &mut egui::Ui, s: &SplitState, lang: Lang, w: f32, h: f32, n_splits: usize) {
    let border = if s.no_split { CARD_BORDER } else { ACCENT };
    egui::Frame::new()
        .fill(CARD_BG)
        .stroke(egui::Stroke::new(1.0, border))
        .corner_radius(egui::CornerRadius::same(8))
        .inner_margin(8.0)
        .show(ui, |ui| {
            ui.set_width(w);
            ui.set_min_height(h);
            ui.vertical_centered(|ui| {
                ui.label(
                    egui::RichText::new(t!(lang, FlowSplit))
                        .size(12.0)
                        .strong()
                        .color(TEXT_BRIGHT),
                );
                ui.add_space(4.0);
                if s.no_split {
                    ui.label(egui::RichText::new("📄 1:1").size(18.0).color(TEXT_DIM));
                } else {
                    let anim = s.anim_time;
                    for i in 0..n_splits {
                        let phase = (anim * 2.0 + i as f64 * 0.3).sin() as f32 * 0.15 + 0.85;
                        let bar_w = (w - 20.0) * phase;
                        let (rect, _) = ui
                            .allocate_exact_size(egui::vec2(w - 20.0, 14.0), egui::Sense::hover());
                        let bar_rect = egui::Rect::from_min_size(rect.min, egui::vec2(bar_w, 12.0));
                        ui.painter().rect_filled(
                            bar_rect,
                            3.0,
                            ACCENT.linear_multiply(0.3 + 0.1 * (i as f32)),
                        );
                        ui.painter().text(
                            bar_rect.center(),
                            egui::Align2::CENTER_CENTER,
                            &format!("#{}", i + 1),
                            egui::FontId::proportional(10.0),
                            egui::Color32::from_rgb(200, 210, 225),
                        );
                    }
                }
            });
        });
}

fn draw_compress_card(
    ui: &mut egui::Ui,
    s: &SplitState,
    lang: Lang,
    w: f32,
    h: f32,
    has_compress: bool,
) {
    let border = if has_compress { WARN } else { CARD_BORDER };
    egui::Frame::new()
        .fill(CARD_BG)
        .stroke(egui::Stroke::new(1.0, border))
        .corner_radius(egui::CornerRadius::same(8))
        .inner_margin(8.0)
        .show(ui, |ui| {
            ui.set_width(w);
            ui.set_min_height(h);
            ui.vertical_centered(|ui| {
                ui.label(
                    egui::RichText::new(t!(lang, FlowCompress))
                        .size(12.0)
                        .strong()
                        .color(TEXT_BRIGHT),
                );
                ui.add_space(4.0);
                if has_compress {
                    let fmt = CompressFormat::all()[s.compress_idx];
                    ui.label(egui::RichText::new(fmt.label()).size(11.0));
                    let scale = 0.9 + (s.anim_time * 3.0).sin() as f32 * 0.1;
                    ui.label(egui::RichText::new("🗜️").size(28.0 * scale));
                    ui.label(
                        egui::RichText::new(format!("Lv.{}", s.compress_level))
                            .size(10.0)
                            .color(TEXT_DIM),
                    );
                } else {
                    ui.label(egui::RichText::new("—").size(18.0).color(TEXT_DIM));
                    ui.label(
                        egui::RichText::new("No compress")
                            .size(10.0)
                            .color(TEXT_DIM),
                    );
                }
            });
        });
}

fn draw_result_card(ui: &mut egui::Ui, s: &SplitState, lang: Lang, w: f32, h: f32) {
    let border = if s.state == JobState::Done {
        SUCCESS
    } else {
        CARD_BORDER
    };
    egui::Frame::new()
        .fill(CARD_BG)
        .stroke(egui::Stroke::new(1.0, border))
        .corner_radius(egui::CornerRadius::same(8))
        .inner_margin(8.0)
        .show(ui, |ui| {
            ui.set_width(w);
            ui.set_min_height(h);
            ui.vertical_centered(|ui| {
                ui.label(
                    egui::RichText::new(t!(lang, FlowResult))
                        .size(12.0)
                        .strong()
                        .color(TEXT_BRIGHT),
                );
                ui.add_space(4.0);
                match s.state {
                    JobState::Done => {
                        ui.label(egui::RichText::new("✅").size(22.0));
                        if let Some(ref out) = s.output_file {
                            ui.label(egui::RichText::new(&out.name).size(10.0));
                            ui.label(
                                egui::RichText::new(format_bytes(out.size))
                                    .size(10.0)
                                    .color(TEXT_DIM),
                            );
                        }
                    }
                    JobState::Running => {
                        ui.spinner();
                        ui.label(
                            egui::RichText::new(t!(lang, Processing))
                                .size(10.0)
                                .color(TEXT_DIM),
                        );
                    }
                    JobState::Error => {
                        ui.label(egui::RichText::new("❌").size(22.0));
                    }
                    JobState::Idle => {
                        ui.label(egui::RichText::new("📦").size(22.0).color(TEXT_DIM));
                        ui.label(egui::RichText::new("…").size(10.0).color(TEXT_DIM));
                    }
                }
            });
        });
}

fn draw_arrow(ui: &mut egui::Ui, color: egui::Color32, w: f32, h: f32) {
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
