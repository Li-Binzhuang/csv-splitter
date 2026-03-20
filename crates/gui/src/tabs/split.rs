use crate::state::{JobState, SplitState};
use crate::widgets::*;
use csv_splitter_core::splitter::run_split;
use csv_splitter_core::*;
use std::path::PathBuf;
use std::sync::mpsc;

pub fn ui_split(ui: &mut egui::Ui, s: &mut SplitState) {
    poll_split(s);

    ui.heading("🔪 拆分 & 压缩");
    ui.label(egui::RichText::new("将大型 CSV 文件拆分为多个压缩分片").size(12.0).weak());
    ui.add_space(6.0);

    // --- Input section ---
    section(ui, "📁 文件设置");

    file_picker(
        ui, "输入文件:", &mut s.input_path,
        &[("CSV / 压缩文件", &["csv", "tsv", "txt", "gz", "zst", "bz2", "xz", "zip", "7z", "tar"])],
    );
    file_picker(
        ui, "列过滤文件:", &mut s.columns_path,
        &[("CSV/TXT", &["csv", "txt"])],
    );
    ui.horizontal(|ui| {
        ui.label("输出文件:");
        ui.add(
            egui::TextEdit::singleline(&mut s.output_path)
                .desired_width(ui.available_width() - 40.0)
                .hint_text("留空则自动生成..."),
        );
        if ui.button("📂").on_hover_text("选择保存位置").clicked() {
            if let Some(p) = rfd::FileDialog::new().add_filter("TAR", &["tar"]).save_file() {
                s.output_path = p.display().to_string();
            }
        }
    });

    // --- Parameters section ---
    section(ui, "⚙️ 参数设置");

    ui.horizontal(|ui| {
        ui.label("拆分份数:");
        ui.add(egui::DragValue::new(&mut s.splits).range(1..=1000));
        ui.add_space(20.0);
        ui.label("缓冲区 (MB):");
        ui.add(egui::DragValue::new(&mut s.buf_mb).range(1..=512));
    });

    ui.horizontal(|ui| {
        let formats = CompressFormat::all();
        let labels: Vec<&str> = formats.iter().map(|f| f.label()).collect();
        ui.label("压缩格式:");
        egui::ComboBox::from_id_salt("compress_fmt")
            .selected_text(labels[s.compress_idx])
            .width(180.0)
            .show_ui(ui, |ui| {
                for (i, l) in labels.iter().enumerate() {
                    ui.selectable_value(&mut s.compress_idx, i, *l);
                }
            });
        ui.add_space(12.0);
        ui.label("压缩级别:");
        ui.add(egui::DragValue::new(&mut s.compress_level).range(1..=22));
    });

    delimiter_combo(ui, &mut s.delim_idx, &mut s.custom_delim);

    ui.horizontal(|ui| {
        ui.label("排序列名:");
        ui.add(
            egui::TextEdit::singleline(&mut s.sort_column)
                .desired_width(160.0)
                .hint_text("留空则不排序"),
        );
        ui.checkbox(&mut s.sort_desc, "降序");
    });

    // --- Action ---
    ui.add_space(10.0);
    let can_start = s.state != JobState::Running && !s.input_path.is_empty();
    ui.horizontal(|ui| {
        let btn = egui::Button::new(
            egui::RichText::new("🚀 开始拆分").size(15.0),
        ).min_size([140.0, 32.0].into());
        if ui.add_enabled(can_start, btn).clicked() {
            start_split(s);
        }
        job_status(ui, &s.state);
    });

    // --- Progress ---
    if s.state == JobState::Running && s.progress_total > 0 {
        ui.add_space(4.0);
        let frac = s.progress_bytes as f32 / s.progress_total as f32;
        ui.label(
            egui::RichText::new(&s.current_phase).size(12.0).weak(),
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

    log_panel(ui, &s.logs);
}

fn start_split(s: &mut SplitState) {
    s.logs.clear();
    s.state = JobState::Running;
    s.progress_bytes = 0;
    s.progress_total = 0;

    let delim = get_delimiter(s.delim_idx, &s.custom_delim);
    let config = SplitConfig {
        input: PathBuf::from(&s.input_path),
        output: if s.output_path.is_empty() { None } else { Some(PathBuf::from(&s.output_path)) },
        splits: s.splits,
        compress_format: CompressFormat::all()[s.compress_idx],
        compress_level: s.compress_level,
        buf_mb: s.buf_mb,
        columns_file: if s.columns_path.is_empty() { None } else { Some(PathBuf::from(&s.columns_path)) },
        delimiter: delim,
        sort_column: if s.sort_column.is_empty() { None } else { Some(s.sort_column.clone()) },
        sort_descending: s.sort_desc,
    };

    let (tx, rx) = mpsc::channel();
    s.rx = Some(rx);
    std::thread::spawn(move || run_split(config, tx));
}

fn poll_split(s: &mut SplitState) {
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
            ProgressEvent::Finished { output } => {
                s.logs.push(format!("✅ 完成: {}", output.display()));
                s.state = JobState::Done;
            }
            ProgressEvent::Error(e) => {
                s.logs.push(format!("❌ 错误: {}", e));
                s.state = JobState::Error;
            }
            _ => {}
        }
    }
}
