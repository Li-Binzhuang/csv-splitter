use super::flow;
use crate::i18n::{t, Lang};
use crate::state::{JobState, SplitState};
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
        ui.heading(t!(lang, SplitTitle));
        ui.label(
            egui::RichText::new(t!(lang, SplitDesc))
                .size(12.0)
                .color(TEXT_DIM),
        );
    });
    ui.add_space(8.0);

    // ===== FLOW — title row has toggles inline =====
    egui::Frame::new()
        .fill(CARD_BG)
        .stroke(egui::Stroke::new(1.0, CARD_BORDER))
        .corner_radius(egui::CornerRadius::same(14))
        .inner_margin(14.0)
        .outer_margin(egui::Margin::symmetric(0, 4))
        .show(ui, |ui| {
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

    // ===== Two-column: (Columns + Action) left 60% | Params right 40% =====
    let (left_w, right_w, left_rect, right_rect) = two_col_rects_ratio(ui, 8.0, 0.55);

    // -- Left column: columns + action --
    let left_h = {
        let mut left_ui = ui.new_child(
            egui::UiBuilder::new()
                .id_salt("split_left")
                .max_rect(left_rect)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        left_ui.set_max_width(left_w);
        left_ui.set_width(left_w);

        egui::Frame::new()
            .fill(CARD_BG)
            .stroke(egui::Stroke::new(1.0, CARD_BORDER))
            .corner_radius(egui::CornerRadius::same(14))
            .inner_margin(14.0)
            .outer_margin(egui::Margin::symmetric(0, 4))
            .show(&mut left_ui, |ui| {
                ui.label(
                    egui::RichText::new(t!(lang, SectionColumns))
                        .size(13.0)
                        .strong()
                        .color(TEXT_BRIGHT),
                );
                ui.add_space(6.0);
                let w = ui.available_width();
                draw_columns_panel(ui, s, lang, w);
            });

        egui::Frame::new()
            .fill(CARD_BG)
            .stroke(egui::Stroke::new(1.0, CARD_BORDER))
            .corner_radius(egui::CornerRadius::same(14))
            .inner_margin(14.0)
            .outer_margin(egui::Margin::symmetric(0, 4))
            .show(&mut left_ui, |ui| {
                ui.label(
                    egui::RichText::new(t!(lang, SectionAction))
                        .size(13.0)
                        .strong()
                        .color(TEXT_BRIGHT),
                );
                ui.add_space(6.0);
                draw_action(ui, s, lang, dialogs);
            });

        left_ui.min_rect().height()
    };

    // -- Right column: params --
    let right_h = {
        let mut right_ui = ui.new_child(
            egui::UiBuilder::new()
                .id_salt("split_right")
                .max_rect(right_rect)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        right_ui.set_max_width(right_w);
        right_ui.set_width(right_w);

        egui::Frame::new()
            .fill(CARD_BG)
            .stroke(egui::Stroke::new(1.0, CARD_BORDER))
            .corner_radius(egui::CornerRadius::same(14))
            .inner_margin(14.0)
            .outer_margin(egui::Margin::symmetric(0, 4))
            .show(&mut right_ui, |ui| {
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

        right_ui.min_rect().height()
    };

    // Advance parent cursor past the taller column
    let max_h = left_h.max(right_h);
    ui.allocate_space(egui::vec2(ui.available_width(), max_h));

    log_panel(ui, &s.logs, lang);
}
fn draw_action(
    ui: &mut egui::Ui,
    s: &mut SplitState,
    lang: Lang,
    dialogs: &mut Vec<PendingDialog>,
) {
    let can_start = s.state != JobState::Running && s.input_file.is_some();
    ui.horizontal(|ui| {
        let btn = egui::Button::new(egui::RichText::new(t!(lang, StartSplit)).size(14.0))
            .min_size([120.0, 32.0].into());
        if ui.add_enabled(can_start, btn).clicked() {
            super::split_job::start_split(s);
        }
        if s.state == JobState::Done {
            if let Some(ref out) = s.output_file {
                if ui
                    .button(egui::RichText::new(t!(lang, SaveResult)).size(13.0))
                    .clicked()
                {
                    dialogs.push(spawn_save_file_dialog(out.name.clone(), out.path.clone()));
                }
            }
        }
        job_status(ui, &s.state, lang);
    });

    if s.state == JobState::Running && s.progress_total > 0 {
        ui.add_space(4.0);
        let frac = s.progress_bytes as f32 / s.progress_total as f32;
        ui.label(
            egui::RichText::new(&s.current_phase)
                .size(12.0)
                .color(TEXT_DIM),
        );
        ui.add(
            egui::ProgressBar::new(frac)
                .text(format!(
                    "{} / {}",
                    format_bytes(s.progress_bytes),
                    format_bytes(s.progress_total)
                ))
                .animate(true),
        );
    }
}

/// Left panel: column filter file + column checkboxes.
fn draw_columns_panel(ui: &mut egui::Ui, s: &mut SplitState, lang: Lang, w: f32) {
    if s.available_headers.is_empty() {
        ui.label(
            egui::RichText::new(t!(lang, DropColFileHere))
                .size(11.0)
                .color(TEXT_DIM),
        );
    } else {
        let selected_count = s.selected_columns.iter().filter(|&&v| v).count();
        ui.horizontal(|ui| {
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
                .desired_width(w - 30.0)
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
    }
}

fn params_left(ui: &mut egui::Ui, s: &mut SplitState, lang: Lang) {
    if !s.no_split {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(t!(lang, Splits)).color(TEXT_BRIGHT));
            ui.add(egui::DragValue::new(&mut s.splits).range(2..=1000));
        });
        ui.add_space(2.0);
    }
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(t!(lang, BufferMb)).color(TEXT_BRIGHT));
        ui.add(egui::DragValue::new(&mut s.buf_mb).range(1..=512));
    });
    ui.add_space(2.0);
    delimiter_combo(ui, &mut s.delim_idx, &mut s.custom_delim, lang);
}

fn params_right(ui: &mut egui::Ui, s: &mut SplitState, lang: Lang) {
    let combo_w = (ui.available_width() - 90.0).clamp(60.0, 120.0);
    if !s.no_compress {
        ui.horizontal(|ui| {
            let formats = CompressFormat::all();
            let labels: Vec<&str> = formats.iter().map(|f| f.label()).collect();
            ui.label(egui::RichText::new(t!(lang, CompressFormat)).color(TEXT_BRIGHT));
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
            ui.label(egui::RichText::new(t!(lang, CompressLevel)).color(TEXT_BRIGHT));
            ui.add(egui::DragValue::new(&mut s.compress_level).range(1..=22));
        });
        ui.add_space(2.0);
    }
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
    ui.checkbox(&mut s.sort_desc, t!(lang, Descending));
}
