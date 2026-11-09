use super::flow;
use crate::i18n::{t, Lang};
use crate::state::SplitState;
use crate::widgets::*;
use csv_splitter_core::*;

pub fn ui_split(
    ui: &mut egui::Ui,
    s: &mut SplitState,
    lang: Lang,
    dialogs: &mut Vec<PendingDialog>,
) {
    super::split_job::poll_split(s, lang);

    // ── Title (centered) ──
    ui.vertical_centered(|ui| {
        ui.heading(egui::RichText::new(t!(lang, SplitTitle)).color(TEXT_BRIGHT));
        ui.label(
            egui::RichText::new(t!(lang, SplitDesc))
                .size(12.0)
                .color(TEXT_DIM),
        );
    });
    ui.add_space(8.0);

    // Capture the total available width ONCE — everything aligns to this.
    let total_w = ui.available_width();

    // ===== FLOW — title row has toggles inline =====
    let flow_resp = egui::Frame::new()
        .fill(CARD_BG)
        .stroke(egui::Stroke::new(1.0_f32, CARD_BORDER))
        .corner_radius(egui::CornerRadius::same(14))
        .inner_margin(14.0)
        .outer_margin(egui::Margin::symmetric(0, 4))
        .show(ui, |ui| {
            ui.set_max_width(total_w - 30.0); // account for frame margins + stroke
            ui.set_min_width(total_w - 30.0); // force full width so it aligns with columns below
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(t!(lang, SectionFlow))
                        .size(13.0)
                        .strong()
                        .color(TEXT_BRIGHT),
                );
                ui.add_space(12.0);
                flow::draw_toggles(ui, s, lang);
            });
            ui.add_space(6.0);
            flow::draw_flow(ui, s, lang, dialogs);
        });

    // Use the flow frame's actual rendered rect as the single source of truth
    let flow_rect = flow_resp.response.rect;
    let flow_w = flow_rect.width();
    let flow_left = flow_rect.left();

    // ===== Two-column: Columns (left, wider) | Params (right) =====
    // Params panel has a natural width ~250px; columns gets the rest.
    let gap = 8.0_f32;
    let right_w = 250.0_f32.min((flow_w - gap) * 0.38);
    let left_w = (flow_w - gap - right_w).max(100.0);
    let top_y = ui.cursor().min.y;
    let left_rect =
        egui::Rect::from_min_size(egui::pos2(flow_left, top_y), egui::vec2(left_w, 10.0));
    let right_rect = egui::Rect::from_min_size(
        egui::pos2(flow_left + left_w + gap, top_y),
        egui::vec2(right_w, 10.0),
    );

    // First pass: draw right (params) to measure its natural height
    let right_h = {
        let mut col = ui.new_child(
            egui::UiBuilder::new()
                .id_salt("split_right")
                .max_rect(right_rect)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        col.set_max_width(right_w);
        col.set_width(right_w);
        egui::Frame::new()
            .fill(CARD_BG)
            .stroke(egui::Stroke::new(1.0_f32, CARD_BORDER))
            .corner_radius(egui::CornerRadius::same(14))
            .inner_margin(14.0)
            .outer_margin(egui::Margin::symmetric(0, 4))
            .show(&mut col, |ui| {
                // Force content to fill the frame so right edge aligns with flow
                let inner_w = right_w - 30.0; // 14*2 margin + 1*2 stroke
                ui.set_min_width(inner_w);
                ui.label(
                    egui::RichText::new(t!(lang, SectionParams))
                        .size(13.0)
                        .strong()
                        .color(TEXT_BRIGHT),
                );
                ui.add_space(6.0);
                params_left(ui, s, lang);
                params_right(ui, s, lang);
            });
        col.min_rect().height()
    };

    // Second pass: draw left (columns) with min_height matching params
    let left_h = {
        let mut col = ui.new_child(
            egui::UiBuilder::new()
                .id_salt("split_left")
                .max_rect(left_rect)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        col.set_max_width(left_w);
        col.set_width(left_w);
        egui::Frame::new()
            .fill(CARD_BG)
            .stroke(egui::Stroke::new(1.0_f32, CARD_BORDER))
            .corner_radius(egui::CornerRadius::same(14))
            .inner_margin(14.0)
            .outer_margin(egui::Margin::symmetric(0, 4))
            .show(&mut col, |ui| {
                let inner_w = left_w - 30.0;
                ui.set_min_width(inner_w);
                ui.set_min_height(right_h - 40.0);
                ui.label(
                    egui::RichText::new(t!(lang, SectionColumns))
                        .size(13.0)
                        .strong()
                        .color(TEXT_BRIGHT),
                );
                ui.add_space(6.0);
                draw_columns_panel(ui, s, lang, dialogs);
            });
        col.min_rect().height()
    };

    ui.allocate_space(egui::vec2(ui.available_width(), left_h.max(right_h)));

    log_panel(ui, &s.logs, lang);
}

/// Column selector panel with optional filter file support.
fn draw_columns_panel(
    ui: &mut egui::Ui,
    s: &mut SplitState,
    lang: Lang,
    dialogs: &mut Vec<PendingDialog>,
) {
    let csv_filters: &[(&str, &[&str])] = &[("CSV", &["csv", "tsv", "txt"])];
    let col_id = egui::Id::new("split_columns");

    // Filter file sub-card
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(t!(lang, ColumnFilter))
                .size(11.0)
                .color(TEXT_DIM),
        );
        if let Some(ref info) = s.columns_file {
            ui.label(
                egui::RichText::new(&info.name)
                    .size(11.0)
                    .color(TEXT_BRIGHT),
            );
            if ui
                .link(
                    egui::RichText::new(t!(lang, RemoveFile))
                        .size(10.0)
                        .color(TEXT_DIM),
                )
                .clicked()
            {
                s.columns_file = None;
                s.filter_headers.clear();
                // Reset all to selected
                s.selected_columns.iter_mut().for_each(|v| *v = true);
            }
        } else if ui
            .link(
                egui::RichText::new(t!(lang, DropColFileHere))
                    .size(10.0)
                    .color(ACCENT),
            )
            .clicked()
        {
            let owned_filters: Vec<(String, Vec<String>)> = csv_filters
                .iter()
                .map(|(n, exts)| (n.to_string(), exts.iter().map(|e| e.to_string()).collect()))
                .collect();
            dialogs.push(spawn_file_dialog(DialogTarget::File(col_id), owned_filters));
        }
    });
    ui.add_space(4.0);

    if s.available_headers.is_empty() {
        ui.vertical_centered(|ui| {
            ui.add_space(20.0);
            ui.label(
                egui::RichText::new(t!(lang, DropFileHere))
                    .size(11.0)
                    .color(TEXT_DIM),
            );
        });
    } else {
        let sel = s.selected_columns.iter().filter(|&&v| v).count();
        let total = s.available_headers.len();
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(format!("{}: {}/{}", t!(lang, SelectedCount), sel, total))
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
                if ui.small_button(t!(lang, InvertSelect)).clicked() {
                    s.selected_columns.iter_mut().for_each(|v| *v = !*v);
                }
            });
        });
        ui.add_space(2.0);
        ui.add(
            egui::TextEdit::singleline(&mut s.column_search)
                .desired_width(ui.available_width() - 8.0)
                .hint_text(t!(lang, SearchColumns)),
        );
        ui.add_space(2.0);
        egui::ScrollArea::vertical()
            .max_height(150.0)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
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
    }
}

fn params_left(ui: &mut egui::Ui, s: &mut SplitState, lang: Lang) {
    let split_dim = if s.no_split { TEXT_DIM } else { TEXT_BRIGHT };
    ui.add_enabled_ui(!s.no_split, |ui| {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(t!(lang, Splits)).color(split_dim));
            ui.add(egui::DragValue::new(&mut s.splits).range(2..=1000));
        });
    });
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(t!(lang, BufferMb)).color(TEXT_BRIGHT));
        ui.add(egui::DragValue::new(&mut s.buf_mb).range(1..=512));
    });
    ui.add_space(2.0);
    delimiter_combo(ui, &mut s.delim_idx, &mut s.custom_delim, lang);
}

fn params_right(ui: &mut egui::Ui, s: &mut SplitState, lang: Lang) {
    let combo_w = (ui.available_width() - 90.0).clamp(60.0, 120.0);
    let compress_dim = if s.no_compress { TEXT_DIM } else { TEXT_BRIGHT };
    ui.add_enabled_ui(!s.no_compress, |ui| {
        ui.horizontal(|ui| {
            let formats = CompressFormat::all();
            let labels: Vec<&str> = formats.iter().map(|f| f.label()).collect();
            ui.label(egui::RichText::new(t!(lang, CompressFormat)).color(compress_dim));
            egui::ComboBox::from_id_salt("compress_fmt")
                .selected_text(labels[s.compress_idx])
                .width(combo_w)
                .show_ui(ui, |ui| {
                    for (i, l) in labels.iter().enumerate() {
                        ui.selectable_value(&mut s.compress_idx, i, *l);
                    }
                });
        });
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(t!(lang, CompressLevel)).color(compress_dim));
            ui.add(egui::DragValue::new(&mut s.compress_level).range(1..=22));
        });
    });
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(t!(lang, SortColumn)).color(TEXT_BRIGHT));
        let display = if s.sort_column.is_empty() {
            t!(lang, SortNone)
        } else {
            &s.sort_column
        };
        egui::ComboBox::from_id_salt("sort_col")
            .selected_text(display)
            .width(combo_w)
            .show_ui(ui, |ui| {
                let is_none = s.sort_column.is_empty();
                if ui.selectable_label(is_none, t!(lang, SortNone)).clicked() {
                    s.sort_column.clear();
                }
                for h in &s.available_headers {
                    let selected = s.sort_column == *h;
                    if ui.selectable_label(selected, h).clicked() {
                        s.sort_column = h.clone();
                    }
                }
            });
    });
    ui.add_space(2.0);
    ui.checkbox(
        &mut s.sort_desc,
        egui::RichText::new(t!(lang, Descending)).color(TEXT_BRIGHT),
    );
}
