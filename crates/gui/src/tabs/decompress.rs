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

    // ── Two-column: Input (left) | Result + Action (right) ──
    let (col_w, left_rect, right_rect) = two_col_rects(ui, 8.0);

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
            .stroke(egui::Stroke::new(1.0, CARD_BORDER))
            .corner_radius(egui::CornerRadius::same(14))
            .inner_margin(14.0)
            .outer_margin(egui::Margin::symmetric(0, 4))
            .show(&mut col, |ui| {
                ui.set_max_width(col_w - 30.0);
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
                        &["zst", "gz", "bz2", "xz", "zip", "7z", "tar", "tgz", "csv"],
                    )],
                    egui::Id::new("decomp_input"),
                ) {
                    dialogs.push(d);
                }
                ui.add_space(6.0);
                if let Some(d) = dir_path_card(
                    ui,
                    &mut s.output_dir,
                    "📁",
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
            .stroke(egui::Stroke::new(1.0, CARD_BORDER))
            .corner_radius(egui::CornerRadius::same(14))
            .inner_margin(14.0)
            .outer_margin(egui::Margin::symmetric(0, 4))
            .show(&mut col, |ui| {
                ui.set_max_width(col_w - 30.0);
                ui.label(
                    egui::RichText::new(t!(lang, SectionResult))
                        .size(13.0)
                        .strong()
                        .color(TEXT_BRIGHT),
                );
                ui.add_space(6.0);
                let rw = ui.available_width() - 4.0;
                draw_decomp_result(ui, s, lang, rw);
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

    log_panel(ui, &s.logs, lang);
}
fn draw_decomp_result(ui: &mut egui::Ui, s: &DecompState, lang: Lang, w: f32) {
    ui.vertical_centered(|ui| {
        ui.set_max_width(w);
        match s.state {
            JobState::Done => {
                ui.label(egui::RichText::new("✅").size(28.0));
                if let Some(ref out) = s.output_file {
                    let max_c = ((w - 20.0) / 7.0).max(6.0) as usize;
                    let name = if out.name.chars().count() > max_c {
                        let t: String = out.name.chars().take(max_c - 1).collect();
                        format!("{}…", t)
                    } else {
                        out.name.clone()
                    };
                    ui.label(egui::RichText::new(&name).size(11.0).color(TEXT_BRIGHT));
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
                        .size(11.0)
                        .color(TEXT_DIM),
                );
            }
            JobState::Error => {
                ui.label(egui::RichText::new("❌").size(28.0));
            }
            JobState::Idle => {
                ui.label(egui::RichText::new("📦").size(28.0).color(TEXT_DIM));
                ui.label(
                    egui::RichText::new(t!(lang, FlowResult))
                        .size(11.0)
                        .color(TEXT_DIM),
                );
            }
        }
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
