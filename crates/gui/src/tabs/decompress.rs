use crate::i18n::{t, Lang};
use crate::state::{DecompState, FileInfo, JobState};
use crate::widgets::*;
use csv_splitter_core::ProgressEvent;
use std::path::PathBuf;
use std::sync::mpsc;

pub fn ui_decompress(ui: &mut egui::Ui, s: &mut DecompState, lang: Lang) {
    poll_decomp(s, lang);

    ui.heading(t!(lang, DecompTitle));
    ui.label(
        egui::RichText::new(t!(lang, DecompDesc))
            .size(12.0)
            .color(TEXT_DIM),
    );
    ui.add_space(8.0);

    let avail_w = ui.available_width();
    let compact = avail_w < 600.0;
    let card_w = if compact {
        ((avail_w - 30.0) / 2.0).clamp(120.0, 220.0)
    } else {
        ((avail_w - 60.0) / 3.0).clamp(140.0, 220.0)
    };
    let card_h: f32 = 80.0;
    let arrow_w: f32 = if compact { 12.0 } else { 18.0 };

    // Input + Flow section
    section_frame(ui, t!(lang, SectionFlow), |ui| {
        if compact {
            file_input_card(
                ui,
                &mut s.input_file,
                t!(lang, DropFileHere),
                ACCENT,
                card_w,
                card_h,
                lang,
                &[(
                    t!(lang, FilterArchive),
                    &["zst", "gz", "bz2", "xz", "zip", "7z", "tar", "tgz", "csv"],
                )],
            );
            ui.add_space(4.0);
            draw_decomp_result_card(ui, s, lang, card_w, card_h);
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
                    &[(
                        t!(lang, FilterArchive),
                        &["zst", "gz", "bz2", "xz", "zip", "7z", "tar", "tgz", "csv"],
                    )],
                );
                draw_arrow(ui, ACCENT, arrow_w, card_h);
                draw_decomp_result_card(ui, s, lang, card_w, card_h);
            });
        }

        ui.add_space(6.0);
        dir_path_card(
            ui,
            &mut s.output_dir,
            "📁",
            t!(lang, OutputDir),
            t!(lang, DirHint),
            lang,
        );
    });

    // Action section
    section_frame(ui, t!(lang, SectionAction), |ui| {
        let can_start = s.state != JobState::Running && s.input_file.is_some();
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    can_start,
                    egui::Button::new(egui::RichText::new(t!(lang, StartExtract)).size(14.0))
                        .min_size([140.0, 32.0].into()),
                )
                .clicked()
            {
                start_decomp(s);
            }
            job_status(ui, &s.state, lang);
        });
    });

    log_panel(ui, &s.logs, lang);
}

fn draw_decomp_result_card(ui: &mut egui::Ui, s: &DecompState, lang: Lang, w: f32, h: f32) {
    let border = if s.state == JobState::Done {
        SUCCESS
    } else {
        CARD_BORDER
    };
    egui::Frame::new()
        .fill(CARD_BG)
        .stroke(egui::Stroke::new(1.0, border))
        .corner_radius(egui::CornerRadius::same(12))
        .inner_margin(8.0)
        .show(ui, |ui| {
            ui.set_width(w);
            ui.set_max_width(w);
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
                            let max_chars = ((w - 20.0) / 7.0).max(6.0) as usize;
                            let name = if out.name.chars().count() > max_chars {
                                let t: String = out.name.chars().take(max_chars - 1).collect();
                                format!("{}…", t)
                            } else {
                                out.name.clone()
                            };
                            ui.label(egui::RichText::new(&name).size(10.0).color(TEXT_BRIGHT));
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

fn start_decomp(s: &mut DecompState) {
    let Some(ref info) = s.input_file else { return };
    s.logs.clear();
    s.state = JobState::Running;
    s.output_file = None;
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
        match csv_splitter_core::decompress::decompress_to_csv(&input, &out_dir, &tx) {
            Ok(p) => {
                tx.send(ProgressEvent::Finished { output: p }).ok();
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
            ProgressEvent::Finished { output } => {
                s.output_file = FileInfo::from_path(output.clone());
                s.logs
                    .push(format!("✅ {}: {}", t!(lang, Finished), output.display()));
                s.state = JobState::Done;
            }
            ProgressEvent::Error(e) => {
                s.logs.push(format!("❌ {}: {}", t!(lang, Error), e));
                s.state = JobState::Error;
            }
            ProgressEvent::Info(msg) => s.logs.push(format!("ℹ {}", msg)),
            _ => {}
        }
    }
}
