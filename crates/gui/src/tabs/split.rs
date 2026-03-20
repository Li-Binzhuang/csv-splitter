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
    // Top row: [Source file card] ---> [Split chunks] ---> [Compress] ---> [Result]
    draw_flow(ui, s, lang, avail_w);

    ui.add_space(12.0);
    ui.separator();
    ui.add_space(4.0);

    // ===== PARAMETERS (responsive 2-column) =====
    ui.label(
        egui::RichText::new(t!(lang, SectionParams))
            .size(14.0)
            .strong(),
    );
    ui.add_space(4.0);

    let half = (avail_w - 20.0) / 2.0;
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.set_width(half);
            ui.horizontal(|ui| {
                ui.checkbox(&mut s.no_split, t!(lang, NoSplit));
            });
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
        });
        ui.vertical(|ui| {
            ui.set_width(half);
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
        });
    });

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

// ===== FLOW DRAWING =====

fn draw_flow(ui: &mut egui::Ui, s: &mut SplitState, lang: Lang, avail_w: f32) {
    let accent = egui::Color32::from_rgb(88, 166, 255);
    let card_bg = egui::Color32::from_rgb(35, 38, 48);
    let card_border = egui::Color32::from_rgb(55, 60, 75);
    let success_color = egui::Color32::from_rgb(80, 200, 120);
    let compress_color = egui::Color32::from_rgb(255, 180, 60);

    let n_splits = if s.no_split {
        1
    } else {
        s.splits.max(1).min(6)
    };
    let has_compress = s.compress_idx < CompressFormat::all().len() - 1; // not "None"

    // Calculate card widths based on available space
    let card_w = ((avail_w - 80.0) / 4.0).min(200.0).max(120.0);
    let card_h = 100.0;
    let arrow_w = 20.0;

    // Flow row
    ui.horizontal(|ui| {
        // 1. SOURCE FILE CARD (with drop zone)
        draw_file_drop_card(
            ui,
            &mut s.input_file,
            t!(lang, DropFileHere),
            t!(lang, FlowSource),
            accent,
            card_bg,
            card_border,
            card_w,
            card_h,
            lang,
            &[(
                "CSV",
                &[
                    "csv", "tsv", "txt", "gz", "zst", "bz2", "xz", "zip", "7z", "tar",
                ],
            )],
        );

        // Arrow
        draw_arrow(ui, accent, arrow_w, card_h);

        // 2. SPLIT STAGE
        let _split_h = if n_splits <= 1 {
            card_h
        } else {
            (n_splits as f32 * 22.0).max(card_h)
        };
        egui::Frame::new()
            .fill(card_bg)
            .stroke(egui::Stroke::new(
                1.0,
                if s.no_split { card_border } else { accent },
            ))
            .corner_radius(egui::CornerRadius::same(8))
            .inner_margin(8.0)
            .show(ui, |ui| {
                ui.set_width(card_w);
                ui.set_min_height(card_h);
                ui.vertical_centered(|ui| {
                    ui.label(egui::RichText::new(t!(lang, FlowSplit)).size(12.0).strong());
                    ui.add_space(4.0);
                    if s.no_split {
                        ui.label(egui::RichText::new("📄 1:1").size(20.0).weak());
                    } else {
                        // Draw mini split visualization
                        let anim = s.anim_time;
                        for i in 0..n_splits {
                            let phase = (anim * 2.0 + i as f64 * 0.3).sin() as f32 * 0.15 + 0.85;
                            let w = (card_w - 20.0) * phase;
                            let (rect, _) = ui.allocate_exact_size(
                                egui::vec2(card_w - 20.0, 14.0),
                                egui::Sense::hover(),
                            );
                            let bar_rect = egui::Rect::from_min_size(rect.min, egui::vec2(w, 12.0));
                            ui.painter().rect_filled(
                                bar_rect,
                                3.0,
                                accent.linear_multiply(0.3 + 0.1 * (i as f32)),
                            );
                            let label = format!("#{}", i + 1);
                            ui.painter().text(
                                bar_rect.center(),
                                egui::Align2::CENTER_CENTER,
                                &label,
                                egui::FontId::proportional(10.0),
                                egui::Color32::from_rgb(200, 210, 225),
                            );
                        }
                    }
                });
            });

        // Arrow
        draw_arrow(
            ui,
            if has_compress { compress_color } else { accent },
            arrow_w,
            card_h,
        );

        // 3. COMPRESS STAGE
        egui::Frame::new()
            .fill(card_bg)
            .stroke(egui::Stroke::new(
                1.0,
                if has_compress {
                    compress_color
                } else {
                    card_border
                },
            ))
            .corner_radius(egui::CornerRadius::same(8))
            .inner_margin(8.0)
            .show(ui, |ui| {
                ui.set_width(card_w);
                ui.set_min_height(card_h);
                ui.vertical_centered(|ui| {
                    ui.label(
                        egui::RichText::new(t!(lang, FlowCompress))
                            .size(12.0)
                            .strong(),
                    );
                    ui.add_space(4.0);
                    if has_compress {
                        let fmt = CompressFormat::all()[s.compress_idx];
                        ui.label(egui::RichText::new(fmt.label()).size(11.0));
                        // Animated compress icon
                        let anim = s.anim_time;
                        let scale = 0.9 + (anim * 3.0).sin() as f32 * 0.1;
                        let icon_size = 28.0 * scale;
                        ui.label(egui::RichText::new("🗜️").size(icon_size));
                        ui.label(
                            egui::RichText::new(format!("Lv.{}", s.compress_level))
                                .size(10.0)
                                .weak(),
                        );
                    } else {
                        ui.label(egui::RichText::new("—").size(20.0).weak());
                        ui.label(egui::RichText::new("No compression").size(10.0).weak());
                    }
                });
            });

        // Arrow
        draw_arrow(ui, success_color, arrow_w, card_h);

        // 4. RESULT CARD
        egui::Frame::new()
            .fill(card_bg)
            .stroke(egui::Stroke::new(
                1.0,
                if s.state == JobState::Done {
                    success_color
                } else {
                    card_border
                },
            ))
            .corner_radius(egui::CornerRadius::same(8))
            .inner_margin(8.0)
            .show(ui, |ui| {
                ui.set_width(card_w);
                ui.set_min_height(card_h);
                ui.vertical_centered(|ui| {
                    ui.label(
                        egui::RichText::new(t!(lang, FlowResult))
                            .size(12.0)
                            .strong(),
                    );
                    ui.add_space(4.0);
                    match s.state {
                        JobState::Done => {
                            ui.label(egui::RichText::new("✅").size(24.0));
                            if let Some(ref out) = s.output_file {
                                ui.label(egui::RichText::new(&out.name).size(10.0));
                                ui.label(
                                    egui::RichText::new(format_bytes(out.size))
                                        .size(10.0)
                                        .weak(),
                                );
                            }
                        }
                        JobState::Running => {
                            ui.spinner();
                            ui.label(egui::RichText::new(t!(lang, Processing)).size(10.0).weak());
                        }
                        JobState::Error => {
                            ui.label(egui::RichText::new("❌").size(24.0));
                        }
                        JobState::Idle => {
                            ui.label(egui::RichText::new("📦").size(24.0).weak());
                            ui.label(egui::RichText::new(".tar").size(10.0).weak());
                        }
                    }
                });
            });
    });

    // Column filter card below source
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        draw_file_drop_card(
            ui,
            &mut s.columns_file,
            t!(lang, DropColFileHere),
            t!(lang, ColumnFilter),
            egui::Color32::from_rgb(180, 140, 255),
            card_bg,
            card_border,
            card_w,
            70.0,
            lang,
            &[("CSV/TXT", &["csv", "txt"])],
        );
    });
}

fn draw_arrow(ui: &mut egui::Ui, color: egui::Color32, w: f32, h: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::hover());
    let mid_y = rect.center().y;
    let painter = ui.painter();
    // Line
    painter.line_segment(
        [
            egui::pos2(rect.left() + 2.0, mid_y),
            egui::pos2(rect.right() - 6.0, mid_y),
        ],
        egui::Stroke::new(2.0, color),
    );
    // Arrowhead
    let tip = egui::pos2(rect.right() - 2.0, mid_y);
    painter.line_segment(
        [egui::pos2(tip.x - 6.0, mid_y - 4.0), tip],
        egui::Stroke::new(2.0, color),
    );
    painter.line_segment(
        [egui::pos2(tip.x - 6.0, mid_y + 4.0), tip],
        egui::Stroke::new(2.0, color),
    );
}

fn draw_file_drop_card(
    ui: &mut egui::Ui,
    file: &mut Option<FileInfo>,
    placeholder: &str,
    _title: &str,
    accent: egui::Color32,
    bg: egui::Color32,
    border: egui::Color32,
    w: f32,
    h: f32,
    lang: Lang,
    filters: &[(&str, &[&str])],
) {
    let has_file = file.is_some();
    let stroke_color = if has_file { accent } else { border };

    let frame_resp = egui::Frame::new()
        .fill(bg)
        .stroke(egui::Stroke::new(
            if has_file { 1.5 } else { 1.0 },
            stroke_color,
        ))
        .corner_radius(egui::CornerRadius::same(8))
        .inner_margin(8.0)
        .show(ui, |ui| {
            ui.set_width(w);
            ui.set_min_height(h);

            if let Some(ref info) = file {
                // Show file info
                let name = info.name.clone();
                let size = info.size;
                let ext = info.extension.clone();
                let modified = info.modified;
                let mut remove = false;
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("📄").size(16.0));
                        ui.label(egui::RichText::new(&name).size(12.0).strong());
                    });
                    ui.add_space(2.0);
                    ui.label(
                        egui::RichText::new(format!(
                            "{}: {}",
                            t!(lang, FileSizeLabel),
                            format_bytes(size)
                        ))
                        .size(10.0)
                        .weak(),
                    );
                    ui.label(
                        egui::RichText::new(format!(
                            "{}: {}",
                            t!(lang, FileType),
                            if ext.is_empty() {
                                "—".to_string()
                            } else {
                                ext
                            }
                        ))
                        .size(10.0)
                        .weak(),
                    );
                    if let Some(modified) = modified {
                        if let Ok(dur) = modified.elapsed() {
                            let secs = dur.as_secs();
                            let time_str = if secs < 60 {
                                format!("{}s ago", secs)
                            } else if secs < 3600 {
                                format!("{}m ago", secs / 60)
                            } else if secs < 86400 {
                                format!("{}h ago", secs / 3600)
                            } else {
                                format!("{}d ago", secs / 86400)
                            };
                            ui.label(
                                egui::RichText::new(format!(
                                    "{}: {}",
                                    t!(lang, FileModified),
                                    time_str
                                ))
                                .size(10.0)
                                .weak(),
                            );
                        }
                    }
                    if ui.small_button(t!(lang, RemoveFile)).clicked() {
                        remove = true;
                    }
                });
                if remove {
                    *file = None;
                }
            } else {
                // Empty drop zone
                ui.vertical_centered(|ui| {
                    ui.add_space((h - 40.0).max(0.0) / 2.0);
                    ui.label(
                        egui::RichText::new(placeholder)
                            .size(11.0)
                            .weak()
                            .color(egui::Color32::from_rgb(120, 128, 145)),
                    );
                });
            }
        });

    // Click to browse
    if frame_resp.response.clicked() {
        let mut dialog = rfd::FileDialog::new();
        for (name, exts) in filters {
            dialog = dialog.add_filter(*name, exts);
        }
        if let Some(path) = dialog.pick_file() {
            *file = FileInfo::from_path(path);
        }
    }
    // Hover effect
    if frame_resp.response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
}

// ===== SPLIT JOB =====

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
        columns_file: s.columns_file.as_ref().map(|f| f.path.clone()),
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
