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

    ui.heading(t!(lang, SplitTitle));
    ui.label(
        egui::RichText::new(t!(lang, SplitDesc))
            .size(12.0)
            .color(TEXT_DIM),
    );
    ui.add_space(8.0);

    let avail_w = ui.available_width();

    // ===== FLOW VISUALIZATION =====
    section_frame(ui, t!(lang, SectionFlow), |ui| {
        flow::draw_flow(ui, s, lang, avail_w, dialogs);
    });

    // ===== PARAMETERS =====
    section_frame(ui, t!(lang, SectionParams), |ui| {
        let half = (avail_w - 40.0) / 2.0;
        if avail_w > 600.0 {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(half);
                    params_left(ui, s, lang);
                });
                ui.vertical(|ui| {
                    ui.set_width(half);
                    params_right(ui, s, lang);
                });
            });
        } else {
            params_left(ui, s, lang);
            ui.add_space(4.0);
            params_right(ui, s, lang);
        }
    });

    // ===== ACTION + PROGRESS =====
    section_frame(ui, t!(lang, SectionAction), |ui| {
        let can_start = s.state != JobState::Running && s.input_file.is_some();
        ui.horizontal(|ui| {
            let btn = egui::Button::new(egui::RichText::new(t!(lang, StartSplit)).size(15.0))
                .min_size([160.0, 36.0].into());
            if ui.add_enabled(can_start, btn).clicked() {
                super::split_job::start_split(s);
            }
            if s.state == JobState::Done {
                if let Some(ref out) = s.output_file {
                    if ui
                        .button(egui::RichText::new(t!(lang, SaveResult)).size(14.0))
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
    });

    log_panel(ui, &s.logs, lang);
}

fn params_left(ui: &mut egui::Ui, s: &mut SplitState, lang: Lang) {
    ui.checkbox(&mut s.no_split, t!(lang, NoSplit));
    if !s.no_split {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(t!(lang, Splits)).color(TEXT_BRIGHT));
            ui.add(egui::DragValue::new(&mut s.splits).range(2..=1000));
        });
    }
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(t!(lang, BufferMb)).color(TEXT_BRIGHT));
        ui.add(egui::DragValue::new(&mut s.buf_mb).range(1..=512));
    });
    delimiter_combo(ui, &mut s.delim_idx, &mut s.custom_delim, lang);
}

fn params_right(ui: &mut egui::Ui, s: &mut SplitState, lang: Lang) {
    ui.horizontal(|ui| {
        let formats = CompressFormat::all();
        let labels: Vec<&str> = formats.iter().map(|f| f.label()).collect();
        ui.label(egui::RichText::new(t!(lang, CompressFormat)).color(TEXT_BRIGHT));
        egui::ComboBox::from_id_salt("compress_fmt")
            .selected_text(labels[s.compress_idx])
            .width(160.0)
            .show_ui(ui, |ui| {
                for (i, l) in labels.iter().enumerate() {
                    ui.selectable_value(&mut s.compress_idx, i, *l);
                }
            });
    });
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(t!(lang, CompressLevel)).color(TEXT_BRIGHT));
        ui.add(egui::DragValue::new(&mut s.compress_level).range(1..=22));
    });
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(t!(lang, SortColumn)).color(TEXT_BRIGHT));
        let display = if s.sort_column.is_empty() {
            t!(lang, SortNone)
        } else {
            &s.sort_column
        };
        egui::ComboBox::from_id_salt("sort_col")
            .selected_text(display)
            .width(160.0)
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
        ui.checkbox(&mut s.sort_desc, t!(lang, Descending));
    });
}
