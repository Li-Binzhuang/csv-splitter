use super::flow;
use crate::i18n::{t, Lang};
use crate::state::{FileInfo, JobState, SplitState};
use crate::widgets::*;
use csv_splitter_core::splitter::run_split;
use csv_splitter_core::*;
use std::path::PathBuf;
use std::sync::mpsc;

pub fn ui_split(ui: &mut egui::Ui, s: &mut SplitState, lang: Lang) {
    poll_split(s, lang);

    ui.heading(t!(lang, SplitTitle));
    ui.label(egui::RichText::new(t!(lang, SplitDesc)).size(12.0).weak());
    ui.add_space(8.0);

    let avail_w = ui.available_width();

    // ===== FLOW VISUALIZATION =====
    flow::draw_flow(ui, s, lang, avail_w);

    ui.add_space(12.0);
    ui.separator();
    ui.add_space(4.0);

    // ===== PARAMETERS =====
    ui.label(
        egui::RichText::new(t!(lang, SectionParams))
            .size(14.0)
            .strong(),
    );
    ui.add_space(4.0);

    let half = (avail_w - 20.0) / 2.0;
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

    // ===== ACTION + PROGRESS =====
    ui.add_space(8.0);
    let can_start = s.state != JobState::Running && s.input_file.is_some();
    ui.horizontal(|ui| {
        let btn = egui::Button::new(egui::RichText::new(t!(lang, StartSplit)).size(15.0))
            .min_size([160.0, 36.0].into());
        if ui.add_enabled(can_start, btn).clicked() {
            start_split(s);
        }
        if s.state == JobState::Done {
            if let Some(ref out) = s.output_file {
                if ui
                    .button(egui::RichText::new(t!(lang, SaveResult)).size(14.0))
                    .clicked()
                {
                    if let Some(dest) = rfd::FileDialog::new().set_file_name(&out.name).save_file()
                    {
                        std::fs::copy(&out.path, &dest).ok();
                    }
                }
            }
        }
        job_status(ui, &s.state, lang);
    });

    if s.state == JobState::Running && s.progress_total > 0 {
        ui.add_space(4.0);
        let frac = s.progress_bytes as f32 / s.progress_total as f32;
        ui.label(egui::RichText::new(&s.current_phase).size(12.0).weak());
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

    log_panel(ui, &s.logs, lang);
}

fn params_left(ui: &mut egui::Ui, s: &mut SplitState, lang: Lang) {
    ui.checkbox(&mut s.no_split, t!(lang, NoSplit));
    if !s.no_split {
        ui.horizontal(|ui| {
            ui.label(t!(lang, Splits));
            ui.add(egui::DragValue::new(&mut s.splits).range(2..=1000));
        });
    }
    ui.horizontal(|ui| {
        ui.label(t!(lang, BufferMb));
        ui.add(egui::DragValue::new(&mut s.buf_mb).range(1..=512));
    });
    delimiter_combo(ui, &mut s.delim_idx, &mut s.custom_delim, lang);
}

fn params_right(ui: &mut egui::Ui, s: &mut SplitState, lang: Lang) {
    ui.horizontal(|ui| {
        let formats = CompressFormat::all();
        let labels: Vec<&str> = formats.iter().map(|f| f.label()).collect();
        ui.label(t!(lang, CompressFormat));
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
        ui.label(t!(lang, CompressLevel));
        ui.add(egui::DragValue::new(&mut s.compress_level).range(1..=22));
    });
    ui.horizontal(|ui| {
        ui.label(t!(lang, SortColumn));
        ui.add(
            egui::TextEdit::singleline(&mut s.sort_column)
                .desired_width(120.0)
                .hint_text(t!(lang, SortColumnHint)),
        );
        ui.checkbox(&mut s.sort_desc, t!(lang, Descending));
    });
}

fn start_split(s: &mut SplitState) {
    let Some(ref input) = s.input_file else {
        return;
    };
    s.logs.clear();
    s.state = JobState::Running;
    s.progress_bytes = 0;
    s.progress_total = 0;
    s.output_file = None;

    let delim = get_delimiter(s.delim_idx, &s.custom_delim);
    let splits = if s.no_split { 1 } else { s.splits };

    // Determine columns_file: explicit file takes priority, otherwise generate from selection
    let columns_file = if s.columns_file.is_some() {
        s.columns_file.as_ref().map(|f| f.path.clone())
    } else if !s.available_headers.is_empty() {
        // Check if any columns are deselected
        let all_selected = s.selected_columns.iter().all(|&v| v);
        if all_selected {
            None
        } else {
            // Write selected column names to a temp file
            let selected: Vec<&str> = s
                .available_headers
                .iter()
                .zip(s.selected_columns.iter())
                .filter(|(_, &sel)| sel)
                .map(|(h, _)| h.as_str())
                .collect();
            if selected.is_empty() {
                None
            } else {
                let tmp = std::env::temp_dir().join("csv_splitter_columns.txt");
                if std::fs::write(&tmp, selected.join("\n")).is_ok() {
                    Some(tmp)
                } else {
                    None
                }
            }
        }
    } else {
        None
    };

    let config = SplitConfig {
        input: input.path.clone(),
        output: if s.output_path.is_empty() {
            None
        } else {
            Some(PathBuf::from(&s.output_path))
        },
        splits,
        compress_format: CompressFormat::all()[s.compress_idx],
        compress_level: s.compress_level,
        buf_mb: s.buf_mb,
        columns_file,
        delimiter: delim,
        sort_column: if s.sort_column.is_empty() {
            None
        } else {
            Some(s.sort_column.clone())
        },
        sort_descending: s.sort_desc,
    };

    let (tx, rx) = mpsc::channel();
    s.rx = Some(rx);
    std::thread::spawn(move || run_split(config, tx));
}

fn poll_split(s: &mut SplitState, lang: Lang) {
    let Some(rx) = &s.rx else { return };
    while let Ok(ev) = rx.try_recv() {
        match ev {
            ProgressEvent::PhaseStart { phase, total_bytes } => {
                s.current_phase = phase.clone();
                s.progress_total = total_bytes;
                s.progress_bytes = 0;
                s.logs.push(format!("▶ {}", phase));
            }
            ProgressEvent::Progress { bytes } => s.progress_bytes = bytes,
            ProgressEvent::PhaseDone { phase } => s.logs.push(format!("✓ {}", phase)),
            ProgressEvent::Info(msg) => s.logs.push(format!("ℹ {}", msg)),
            ProgressEvent::Finished { ref output } => {
                s.output_file = FileInfo::from_path(output.clone());
                s.logs
                    .push(format!("✅ {}: {}", t!(lang, Finished), output.display()));
                s.state = JobState::Done;
            }
            ProgressEvent::Error(e) => {
                s.logs.push(format!("❌ {}: {}", t!(lang, Error), e));
                s.state = JobState::Error;
            }
            _ => {}
        }
    }
}
