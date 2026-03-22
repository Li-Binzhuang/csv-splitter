//! Split job execution and progress polling.

use crate::i18n::{t, Lang};
use crate::state::{FileInfo, JobState, SplitState};
use crate::widgets::get_delimiter;
use csv_splitter_core::splitter::run_split;
use csv_splitter_core::*;
use std::path::PathBuf;
use std::sync::mpsc;

pub fn start_split(s: &mut SplitState) {
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

    let compress_format = if s.no_compress {
        *CompressFormat::all().last().unwrap()
    } else {
        CompressFormat::all()[s.compress_idx]
    };

    let columns_file = if s.columns_file.is_some() {
        s.columns_file.as_ref().map(|f| f.path.clone())
    } else if !s.available_headers.is_empty() {
        let all_selected = s.selected_columns.iter().all(|&v| v);
        if all_selected {
            None
        } else {
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
        compress_format,
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

pub fn poll_split(s: &mut SplitState, lang: Lang) {
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
