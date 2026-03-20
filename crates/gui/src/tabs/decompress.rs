use crate::i18n::{t, Lang};
use crate::state::{DecompState, JobState};
use crate::widgets::*;
use csv_splitter_core::ProgressEvent;
use std::path::PathBuf;
use std::sync::mpsc;

pub fn ui_decompress(ui: &mut egui::Ui, s: &mut DecompState, lang: Lang) {
    poll_decomp(s, lang);

    ui.heading(t!(lang, DecompTitle));
    ui.label(egui::RichText::new(t!(lang, DecompDesc)).size(12.0).weak());
    ui.add_space(8.0);

    let card_bg = egui::Color32::from_rgb(35, 38, 48);
    let card_border = egui::Color32::from_rgb(55, 60, 75);
    let accent = egui::Color32::from_rgb(88, 166, 255);

    // Input card
    egui::Frame::new()
        .fill(card_bg)
        .stroke(egui::Stroke::new(
            1.0,
            if s.input.is_empty() {
                card_border
            } else {
                accent
            },
        ))
        .corner_radius(egui::CornerRadius::same(8))
        .inner_margin(12.0)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("📦").size(18.0));
                ui.label(t!(lang, ArchiveFile));
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut s.input)
                        .desired_width(ui.available_width() - 80.0)
                        .hint_text(t!(lang, FileHint)),
                );
                if ui
                    .button("📂")
                    .on_hover_text(t!(lang, BrowseFile))
                    .clicked()
                {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter(
                            t!(lang, FilterArchive),
                            &["zst", "gz", "bz2", "xz", "zip", "7z", "tar", "tgz", "csv"],
                        )
                        .pick_file()
                    {
                        s.input = path.display().to_string();
                        resp.request_focus();
                    }
                }
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("📁").size(18.0));
                ui.label(t!(lang, OutputDir));
                ui.add(
                    egui::TextEdit::singleline(&mut s.output_dir)
                        .desired_width(ui.available_width() - 80.0)
                        .hint_text(t!(lang, DirHint)),
                );
                if ui.button("📂").on_hover_text(t!(lang, BrowseDir)).clicked() {
                    if let Some(path) = rfd::FileDialog::new().pick_folder() {
                        s.output_dir = path.display().to_string();
                    }
                }
            });
        });

    ui.add_space(8.0);
    let can_start = s.state != JobState::Running && !s.input.is_empty();
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

    log_panel(ui, &s.logs, lang);
}

fn start_decomp(s: &mut DecompState) {
    s.logs.clear();
    s.state = JobState::Running;
    let input = PathBuf::from(&s.input);
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
