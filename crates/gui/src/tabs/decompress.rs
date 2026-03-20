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
    ui.add_space(6.0);

    file_picker(
        ui, t!(lang, ArchiveFile), &mut s.input,
        &[(t!(lang, FilterArchive), &["zst", "gz", "bz2", "xz", "zip", "7z", "tar", "tgz", "csv"])],
        lang,
    );
    dir_picker(ui, t!(lang, OutputDir), &mut s.output_dir, lang);

    ui.add_space(6.0);
    let can_start = s.state != JobState::Running && !s.input.is_empty();
    ui.horizontal(|ui| {
        let btn = egui::Button::new(t!(lang, StartExtract)).min_size([120.0, 28.0].into());
        if ui.add_enabled(can_start, btn).clicked() {
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
        input.parent().unwrap_or(std::path::Path::new(".")).to_path_buf()
    } else {
        PathBuf::from(&s.output_dir)
    };
    let (tx, rx) = mpsc::channel();
    s.rx = Some(rx);
    std::thread::spawn(move || {
        std::fs::create_dir_all(&out_dir).ok();
        match csv_splitter_core::decompress::decompress_to_csv(&input, &out_dir, &tx) {
            Ok(p) => { tx.send(ProgressEvent::Finished { output: p }).ok(); }
            Err(e) => { tx.send(ProgressEvent::Error(e.to_string())).ok(); }
        }
    });
}

fn poll_decomp(s: &mut DecompState, lang: Lang) {
    let Some(rx) = &s.rx else { return };
    while let Ok(ev) = rx.try_recv() {
        match ev {
            ProgressEvent::Finished { output } => {
                s.logs.push(format!("✅ {}: {}", t!(lang, Finished), output.display()));
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
