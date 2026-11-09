use crate::i18n::{t, Lang};
use crate::state::{DecompState, FileInfo, JobState};
use crate::widgets::*;
use csv_splitter_core::ProgressEvent;
use std::path::PathBuf;
use std::sync::mpsc;

pub fn ui_decompress(
    ui: &mut egui::Ui,
    s: &mut DecompState,
    lang: Lang,
    dialogs: &mut Vec<PendingDialog>,
) {
    poll_decomp(s, lang);

    ui.vertical_centered(|ui| {
        ui.heading(egui::RichText::new(t!(lang, DecompTitle)).color(TEXT_BRIGHT));
        ui.label(
            egui::RichText::new(t!(lang, DecompDesc))
                .size(12.0)
                .color(TEXT_DIM),
        );
    });
    ui.add_space(8.0);

    let (col_w, left_rect, right_rect) = two_col_rects(ui, 8.0);
    let inner_w = col_w - 30.0;
    let total_w = col_w * 2.0 + 8.0;

    // Draw left (input) first
    let left_h = {
        let mut col = ui.new_child(
            egui::UiBuilder::new()
                .id_salt("decomp_left")
                .max_rect(left_rect)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        col.set_max_width(col_w);
        col.set_width(col_w);
        egui::Frame::new()
            .fill(CARD_BG)
            .stroke(egui::Stroke::new(1.0_f32, CARD_BORDER))
            .corner_radius(egui::CornerRadius::same(14))
            .inner_margin(14.0)
            .outer_margin(egui::Margin::symmetric(0, 4))
            .show(&mut col, |ui| {
                ui.set_min_width(inner_w);
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
                    &[(
                        t!(lang, FilterArchive),
                        &[
                            "zst", "gz", "bz2", "xz", "zip", "7z", "tar", "tgz", "jar", "war",
                            "apk", "csv", "tsv", "txt",
                        ],
                    )],
                    egui::Id::new("decomp_input"),
                ) {
                    dialogs.push(d);
                }
                ui.add_space(6.0);
                if let Some(d) = dir_path_card(
                    ui,
                    &mut s.output_dir,
                    "\u{1F4C1}",
                    t!(lang, OutputDir),
                    t!(lang, DirHint),
                    lang,
                    egui::Id::new("decomp_output_dir"),
                ) {
                    dialogs.push(d);
                }
            });
        col.min_rect().height()
    };

    // Draw right (result + action), matching left height
    let right_h = {
        let mut col = ui.new_child(
            egui::UiBuilder::new()
                .id_salt("decomp_right")
                .max_rect(right_rect)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        col.set_max_width(col_w);
        col.set_width(col_w);
        egui::Frame::new()
            .fill(CARD_BG)
            .stroke(egui::Stroke::new(1.0_f32, CARD_BORDER))
            .corner_radius(egui::CornerRadius::same(14))
            .inner_margin(14.0)
            .outer_margin(egui::Margin::symmetric(0, 4))
            .show(&mut col, |ui| {
                ui.set_min_width(inner_w);
                ui.set_min_height((left_h - 40.0).max(60.0));
                ui.label(
                    egui::RichText::new(t!(lang, SectionResult))
                        .size(13.0)
                        .strong()
                        .color(TEXT_BRIGHT),
                );
                ui.add_space(6.0);
                draw_decomp_result(ui, s, lang);
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
                let can = s.state != JobState::Running && s.input_file.is_some();
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(
                            can,
                            egui::Button::new(
                                egui::RichText::new(t!(lang, StartExtract)).size(14.0),
                            )
                            .min_size([120.0, 32.0].into()),
                        )
                        .clicked()
                    {
                        start_decomp(s);
                    }
                    job_status(ui, &s.state, lang);
                });
            });
        col.min_rect().height()
    };

    ui.allocate_space(egui::vec2(ui.available_width(), left_h.max(right_h)));

    // Log section: width = both columns, min height like stats result (~250)
    let log_h = ui.available_height().max(250.0);
    let log_inner_w = total_w - 30.0;
    egui::Frame::new()
        .fill(CARD_BG)
        .stroke(egui::Stroke::new(1.0_f32, CARD_BORDER))
        .corner_radius(egui::CornerRadius::same(14))
        .inner_margin(14.0)
        .outer_margin(egui::Margin::symmetric(0, 4))
        .show(ui, |ui| {
            ui.set_min_width(log_inner_w);
            ui.set_min_height((log_h - 60.0).max(180.0));
            ui.label(
                egui::RichText::new(t!(lang, LogTitle))
                    .size(13.0)
                    .strong()
                    .color(TEXT_BRIGHT),
            );
            ui.add_space(6.0);
            egui::ScrollArea::vertical()
                .max_height((log_h - 80.0).max(160.0))
                .stick_to_bottom(true)
                .id_salt("decomp_log")
                .show(ui, |ui| {
                    if s.logs.is_empty() {
                        ui.label(egui::RichText::new("\u{2014}").size(12.0).color(TEXT_DIM));
                    } else {
                        for line in &s.logs {
                            ui.label(
                                egui::RichText::new(line)
                                    .size(12.0)
                                    .color(TEXT_BRIGHT)
                                    .family(egui::FontFamily::Monospace),
                            );
                        }
                    }
                });
        });
}

fn draw_decomp_result(ui: &mut egui::Ui, s: &DecompState, lang: Lang) {
    match s.state {
        JobState::Done if !s.output_files.is_empty() => {
            ui.horizontal_wrapped(|ui| {
                stat_badge(
                    ui,
                    "\u{2705}",
                    t!(lang, Finished),
                    &format!("{}", s.output_files.len()),
                );
            });
            ui.add_space(4.0);
            let list_h = (s.output_files.len() as f32 * 20.0).clamp(40.0, 120.0);
            egui::ScrollArea::vertical()
                .max_height(list_h)
                .id_salt("decomp_file_list")
                .show(ui, |ui| {
                    for f in &s.output_files {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            ui.label(egui::RichText::new("\u{1F4C4}").size(11.0));
                            ui.label(egui::RichText::new(&f.name).size(11.0).color(TEXT_BRIGHT));
                            ui.label(
                                egui::RichText::new(format_bytes(f.size))
                                    .size(10.0)
                                    .color(TEXT_DIM),
                            );
                        });
                    }
                });
        }
        JobState::Running => {
            ui.vertical_centered(|ui| {
                ui.spinner();
                ui.label(
                    egui::RichText::new(t!(lang, Processing))
                        .size(11.0)
                        .color(TEXT_DIM),
                );
            });
        }
        JobState::Error => {
            ui.vertical_centered(|ui| {
                ui.label(egui::RichText::new("\u{274C}").size(28.0));
            });
        }
        _ => {
            ui.vertical_centered(|ui| {
                ui.label(egui::RichText::new("\u{1F4E6}").size(28.0).color(TEXT_DIM));
                ui.label(
                    egui::RichText::new(t!(lang, FlowResult))
                        .size(11.0)
                        .color(TEXT_DIM),
                );
            });
        }
    }
}

fn start_decomp(s: &mut DecompState) {
    let Some(ref info) = s.input_file else { return };
    s.logs.clear();
    s.state = JobState::Running;
    s.output_files.clear();
    let input = info.path.clone();
    let out_dir = if s.output_dir.is_empty() {
        input
            .parent()
            .unwrap_or(std::path::Path::new("."))
            .to_path_buf()
    } else {
        PathBuf::from(&s.output_dir)
    };
    let (tx, rx) = mpsc::channel();
    s.rx = Some(rx);
    std::thread::spawn(move || {
        std::fs::create_dir_all(&out_dir).ok();
        match csv_splitter_core::decompress::decompress_file(&input, &out_dir, &tx) {
            Ok(paths) => {
                tx.send(ProgressEvent::DecompressFiles { paths }).ok();
            }
            Err(e) => {
                tx.send(ProgressEvent::Error(e.to_string())).ok();
            }
        }
    });
}

fn poll_decomp(s: &mut DecompState, lang: Lang) {
    let Some(rx) = &s.rx else { return };
    while let Ok(ev) = rx.try_recv() {
        match ev {
            ProgressEvent::DecompressFiles { paths } => {
                s.output_files = paths
                    .iter()
                    .filter_map(|p| FileInfo::from_path(p.clone()))
                    .collect();
                let count = s.output_files.len();
                s.logs.push(format!(
                    "\u{2705} {}: {} file(s)",
                    t!(lang, Finished),
                    count
                ));
                s.state = JobState::Done;
            }
            ProgressEvent::Finished { output } => {
                if let Some(fi) = FileInfo::from_path(output.clone()) {
                    s.output_files.push(fi);
                }
                s.logs.push(format!(
                    "\u{2705} {}: {}",
                    t!(lang, Finished),
                    output.display()
                ));
                s.state = JobState::Done;
            }
            ProgressEvent::Error(e) => {
                s.logs.push(format!("\u{274C} {}: {}", t!(lang, Error), e));
                s.state = JobState::Error;
            }
            ProgressEvent::Info(msg) => s.logs.push(format!("\u{2139}\u{FE0F} {}", msg)),
            _ => {}
        }
    }
}
