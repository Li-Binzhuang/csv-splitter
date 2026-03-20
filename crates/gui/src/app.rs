use csv_splitter_core::preview::{compute_column_stats, load_preview};
use csv_splitter_core::splitter::run_split;
use csv_splitter_core::*;
use std::path::PathBuf;
use std::sync::mpsc;

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Split,
    Preview,
    Stats,
    Decompress,
}

#[derive(PartialEq)]
enum JobState {
    Idle,
    Running,
    Done,
    Error,
}

pub struct App {
    tab: Tab,
    // Split config
    input_path: String,
    output_path: String,
    columns_path: String,
    splits: usize,
    compress_idx: usize,
    compress_level: i32,
    buf_mb: usize,
    delim_idx: usize,
    custom_delim: String,
    sort_column: String,
    sort_desc: bool,
    // Split job state
    state: JobState,
    rx: Option<mpsc::Receiver<ProgressEvent>>,
    logs: Vec<String>,
    current_phase: String,
    progress_bytes: u64,
    progress_total: u64,
    // Preview
    preview: Option<CsvPreview>,
    preview_path: String,
    preview_delim_idx: usize,
    // Stats
    stats: Option<Vec<ColumnStats>>,
    stats_path: String,
    stats_delim_idx: usize,
    stats_selected_col: usize,
    stats_sample_size: usize,
    stats_state: JobState,
    stats_rx: Option<mpsc::Receiver<ProgressEvent>>,
    stats_logs: Vec<String>,
    // Decompress
    decomp_input: String,
    decomp_output_dir: String,
    decomp_state: JobState,
    decomp_rx: Option<mpsc::Receiver<ProgressEvent>>,
    decomp_logs: Vec<String>,
}

impl App {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        Self {
            tab: Tab::Split,
            input_path: String::new(),
            output_path: String::new(),
            columns_path: String::new(),
            splits: 10,
            compress_idx: 0,
            compress_level: 3,
            buf_mb: 64,
            delim_idx: 0,
            custom_delim: String::new(),
            sort_column: String::new(),
            sort_desc: false,
            state: JobState::Idle,
            rx: None,
            logs: Vec::new(),
            current_phase: String::new(),
            progress_bytes: 0,
            progress_total: 0,
            preview: None,
            preview_path: String::new(),
            preview_delim_idx: 0,
            stats: None,
            stats_path: String::new(),
            stats_delim_idx: 0,
            stats_selected_col: 0,
            stats_sample_size: 100_000,
            stats_state: JobState::Idle,
            stats_rx: None,
            stats_logs: Vec::new(),
            decomp_input: String::new(),
            decomp_output_dir: String::new(),
            decomp_state: JobState::Idle,
            decomp_rx: None,
            decomp_logs: Vec::new(),
        }
    }

    fn get_delimiter(&self, idx: usize) -> Delimiter {
        let common = Delimiter::common();
        if idx < common.len() {
            common[idx]
        } else {
            let b = self.custom_delim.as_bytes().first().copied().unwrap_or(b',');
            Delimiter::Custom(b)
        }
    }

    fn get_compress_format(&self) -> CompressFormat {
        CompressFormat::all()[self.compress_idx]
    }
}

// --- Actions ---
impl App {
    fn start_split(&mut self) {
        self.logs.clear();
        self.state = JobState::Running;
        self.progress_bytes = 0;
        self.progress_total = 0;

        let config = SplitConfig {
            input: PathBuf::from(&self.input_path),
            output: if self.output_path.is_empty() { None } else { Some(PathBuf::from(&self.output_path)) },
            splits: self.splits,
            compress_format: self.get_compress_format(),
            compress_level: self.compress_level,
            buf_mb: self.buf_mb,
            columns_file: if self.columns_path.is_empty() { None } else { Some(PathBuf::from(&self.columns_path)) },
            delimiter: self.get_delimiter(self.delim_idx),
            sort_column: if self.sort_column.is_empty() { None } else { Some(self.sort_column.clone()) },
            sort_descending: self.sort_desc,
        };

        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        std::thread::spawn(move || run_split(config, tx));
    }

    fn load_preview(&mut self) {
        let path = PathBuf::from(&self.preview_path);
        let delim = self.get_delimiter(self.preview_delim_idx);
        match load_preview(&path, 100, Some(delim)) {
            Ok(p) => { self.preview = Some(p); }
            Err(e) => { self.logs.push(format!("❌ Preview: {}", e)); }
        }
    }

    fn start_stats(&mut self) {
        self.stats_logs.clear();
        self.stats_state = JobState::Running;
        self.stats = None;
        let path = PathBuf::from(&self.stats_path);
        let delim = self.get_delimiter(self.stats_delim_idx);
        let sample = self.stats_sample_size;
        let (tx, rx) = mpsc::channel();
        self.stats_rx = Some(rx);
        std::thread::spawn(move || {
            match compute_column_stats(&path, delim, sample, 10_000, &tx) {
                Ok(s) => { tx.send(ProgressEvent::StatsReady(s)).ok(); }
                Err(e) => { tx.send(ProgressEvent::Error(e.to_string())).ok(); }
            }
        });
    }

    fn start_decompress(&mut self) {
        self.decomp_logs.clear();
        self.decomp_state = JobState::Running;
        let input = PathBuf::from(&self.decomp_input);
        let out_dir = if self.decomp_output_dir.is_empty() {
            input.parent().unwrap_or(std::path::Path::new(".")).to_path_buf()
        } else {
            PathBuf::from(&self.decomp_output_dir)
        };
        let (tx, rx) = mpsc::channel();
        self.decomp_rx = Some(rx);
        std::thread::spawn(move || {
            std::fs::create_dir_all(&out_dir).ok();
            match csv_splitter_core::decompress::decompress_to_csv(&input, &out_dir, &tx) {
                Ok(p) => { tx.send(ProgressEvent::Finished { output: p }).ok(); }
                Err(e) => { tx.send(ProgressEvent::Error(e.to_string())).ok(); }
            }
        });
    }

    fn poll_events(&mut self) {
        if let Some(rx) = &self.rx {
            while let Ok(ev) = rx.try_recv() {
                match ev {
                    ProgressEvent::PhaseStart { phase, total_bytes } => {
                        self.current_phase = phase.clone();
                        self.progress_total = total_bytes;
                        self.progress_bytes = 0;
                        self.logs.push(format!("▶ {}", phase));
                    }
                    ProgressEvent::Progress { bytes } => self.progress_bytes = bytes,
                    ProgressEvent::PhaseDone { phase } => self.logs.push(format!("✓ {}", phase)),
                    ProgressEvent::Info(msg) => self.logs.push(format!("ℹ {}", msg)),
                    ProgressEvent::Finished { output } => {
                        self.logs.push(format!("✅ {}", output.display()));
                        self.state = JobState::Done;
                    }
                    ProgressEvent::Error(e) => {
                        self.logs.push(format!("❌ {}", e));
                        self.state = JobState::Error;
                    }
                    _ => {}
                }
            }
        }
        if let Some(rx) = &self.stats_rx {
            while let Ok(ev) = rx.try_recv() {
                match ev {
                    ProgressEvent::StatsReady(s) => { self.stats = Some(s); self.stats_state = JobState::Done; }
                    ProgressEvent::Error(e) => { self.stats_logs.push(format!("❌ {}", e)); self.stats_state = JobState::Error; }
                    ProgressEvent::Info(msg) => self.stats_logs.push(msg),
                    _ => {}
                }
            }
        }
        if let Some(rx) = &self.decomp_rx {
            while let Ok(ev) = rx.try_recv() {
                match ev {
                    ProgressEvent::Finished { output } => {
                        self.decomp_logs.push(format!("✅ {}", output.display()));
                        self.decomp_state = JobState::Done;
                    }
                    ProgressEvent::Error(e) => { self.decomp_logs.push(format!("❌ {}", e)); self.decomp_state = JobState::Error; }
                    ProgressEvent::Info(msg) => self.decomp_logs.push(format!("ℹ {}", msg)),
                    _ => {}
                }
            }
        }
    }
}

fn format_bytes(b: u64) -> String {
    if b >= 1 << 30 { format!("{:.2} GB", b as f64 / (1u64 << 30) as f64) }
    else if b >= 1 << 20 { format!("{:.1} MB", b as f64 / (1u64 << 20) as f64) }
    else if b >= 1 << 10 { format!("{:.1} KB", b as f64 / (1u64 << 10) as f64) }
    else { format!("{} B", b) }
}

fn file_picker(ui: &mut egui::Ui, label: &str, value: &mut String, filters: &[(&str, &[&str])]) {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.text_edit_singleline(value);
        if ui.button("📂").clicked() {
            let mut dialog = rfd::FileDialog::new();
            for (name, exts) in filters {
                dialog = dialog.add_filter(*name, exts);
            }
            if let Some(path) = dialog.pick_file() {
                *value = path.display().to_string();
            }
        }
    });
}

fn dir_picker(ui: &mut egui::Ui, label: &str, value: &mut String) {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.text_edit_singleline(value);
        if ui.button("📂").clicked() {
            if let Some(path) = rfd::FileDialog::new().pick_folder() {
                *value = path.display().to_string();
            }
        }
    });
}

fn delimiter_combo(ui: &mut egui::Ui, idx: &mut usize, custom: &mut String) {
    let common = Delimiter::common();
    let labels: Vec<&str> = common.iter().map(|d| d.label()).chain(std::iter::once("Custom")).collect();
    egui::ComboBox::from_label("Delimiter")
        .selected_text(labels[*idx])
        .show_ui(ui, |ui| {
            for (i, label) in labels.iter().enumerate() {
                ui.selectable_value(idx, i, *label);
            }
        });
    if *idx >= common.len() {
        ui.horizontal(|ui| {
            ui.label("Custom char:");
            ui.text_edit_singleline(custom);
        });
    }
}

fn show_log(ui: &mut egui::Ui, logs: &[String]) {
    egui::ScrollArea::vertical()
        .max_height(120.0)
        .stick_to_bottom(true)
        .show(ui, |ui| {
            for line in logs {
                ui.label(line);
            }
        });
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_events();
        if self.state == JobState::Running || self.stats_state == JobState::Running || self.decomp_state == JobState::Running {
            ctx.request_repaint();
        }

        egui::TopBottomPanel::top("tabs").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.tab, Tab::Split, "🔪 Split & Compress");
                ui.selectable_value(&mut self.tab, Tab::Preview, "👁 Preview");
                ui.selectable_value(&mut self.tab, Tab::Stats, "📊 Column Stats");
                ui.selectable_value(&mut self.tab, Tab::Decompress, "📦 Decompress");
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            match self.tab {
                Tab::Split => self.ui_split(ui),
                Tab::Preview => self.ui_preview(ui),
                Tab::Stats => self.ui_stats(ui),
                Tab::Decompress => self.ui_decompress(ui),
            }
        });
    }
}

impl App {
    fn ui_split(&mut self, ui: &mut egui::Ui) {
        ui.heading("Split & Compress");
        ui.add_space(4.0);

        file_picker(ui, "Input CSV:", &mut self.input_path,
            &[("CSV", &["csv", "tsv", "txt", "gz", "zst", "bz2", "xz", "zip", "7z", "tar"])]);
        file_picker(ui, "Columns filter:", &mut self.columns_path, &[("CSV/TXT", &["csv", "txt"])]);

        ui.horizontal(|ui| {
            ui.label("Output:");
            ui.text_edit_singleline(&mut self.output_path);
            if ui.button("📂").clicked() {
                if let Some(p) = rfd::FileDialog::new().add_filter("TAR", &["tar"]).save_file() {
                    self.output_path = p.display().to_string();
                }
            }
        });

        ui.add_space(4.0);
        ui.separator();

        ui.horizontal(|ui| {
            ui.label("Splits:");
            ui.add(egui::DragValue::new(&mut self.splits).range(1..=1000));
            ui.add_space(12.0);
            ui.label("Buffer MB:");
            ui.add(egui::DragValue::new(&mut self.buf_mb).range(1..=512));
        });

        ui.horizontal(|ui| {
            let formats = CompressFormat::all();
            let labels: Vec<&str> = formats.iter().map(|f| f.label()).collect();
            egui::ComboBox::from_label("Compression")
                .selected_text(labels[self.compress_idx])
                .show_ui(ui, |ui| {
                    for (i, l) in labels.iter().enumerate() {
                        ui.selectable_value(&mut self.compress_idx, i, *l);
                    }
                });
            ui.label("Level:");
            ui.add(egui::DragValue::new(&mut self.compress_level).range(1..=22));
        });

        delimiter_combo(ui, &mut self.delim_idx, &mut self.custom_delim);

        ui.horizontal(|ui| {
            ui.label("Sort by column:");
            ui.text_edit_singleline(&mut self.sort_column);
            ui.checkbox(&mut self.sort_desc, "Descending");
        });

        ui.add_space(8.0);

        let can_start = self.state != JobState::Running && !self.input_path.is_empty();
        ui.horizontal(|ui| {
            if ui.add_enabled(can_start, egui::Button::new("🚀 Start")).clicked() {
                self.start_split();
            }
            match self.state {
                JobState::Running => { ui.spinner(); }
                JobState::Done => { ui.label("✅ Complete"); }
                JobState::Error => { ui.colored_label(egui::Color32::RED, "❌ Failed"); }
                JobState::Idle => {}
            }
        });

        if self.state == JobState::Running && self.progress_total > 0 {
            let frac = self.progress_bytes as f32 / self.progress_total as f32;
            ui.label(&self.current_phase);
            ui.add(egui::ProgressBar::new(frac).text(
                format!("{} / {}", format_bytes(self.progress_bytes), format_bytes(self.progress_total))));
        }

        ui.add_space(4.0);
        ui.separator();
        ui.label("Log:");
        show_log(ui, &self.logs);
    }

    fn ui_preview(&mut self, ui: &mut egui::Ui) {
        ui.heading("CSV Preview");
        ui.add_space(4.0);

        file_picker(ui, "CSV file:", &mut self.preview_path, &[("CSV", &["csv", "tsv", "txt"])]);
        delimiter_combo(ui, &mut self.preview_delim_idx, &mut self.custom_delim);

        if ui.button("Load Preview").clicked() && !self.preview_path.is_empty() {
            self.load_preview();
        }

        if let Some(ref preview) = self.preview {
            ui.add_space(4.0);
            ui.label(format!("File: {} | Rows: ~{} | Delimiter: {}",
                format_bytes(preview.file_size),
                preview.total_rows_estimate,
                preview.detected_delimiter.label()));
            ui.add_space(4.0);

            // Table
            egui::ScrollArea::both().max_height(400.0).show(ui, |ui| {
                egui::Grid::new("preview_grid")
                    .striped(true)
                    .min_col_width(60.0)
                    .show(ui, |ui| {
                        // Header row
                        for h in &preview.headers {
                            ui.strong(h);
                        }
                        ui.end_row();
                        // Data rows
                        for row in &preview.rows {
                            for (i, _) in preview.headers.iter().enumerate() {
                                let val = row.get(i).map(|s| s.as_str()).unwrap_or("");
                                ui.label(val);
                            }
                            ui.end_row();
                        }
                    });
            });
        }
    }

    fn ui_stats(&mut self, ui: &mut egui::Ui) {
        ui.heading("Column Statistics");
        ui.add_space(4.0);

        file_picker(ui, "CSV file:", &mut self.stats_path, &[("CSV", &["csv", "tsv", "txt"])]);
        delimiter_combo(ui, &mut self.stats_delim_idx, &mut self.custom_delim);

        ui.horizontal(|ui| {
            ui.label("Sample size:");
            ui.add(egui::DragValue::new(&mut self.stats_sample_size).range(1000..=10_000_000));
        });

        let can_start = self.stats_state != JobState::Running && !self.stats_path.is_empty();
        ui.horizontal(|ui| {
            if ui.add_enabled(can_start, egui::Button::new("📊 Compute Stats")).clicked() {
                self.start_stats();
            }
            if self.stats_state == JobState::Running { ui.spinner(); }
        });

        if let Some(ref stats) = self.stats {
            ui.add_space(4.0);
            ui.separator();

            // Column selector
            let col_names: Vec<&str> = stats.iter().map(|s| s.name.as_str()).collect();
            if !col_names.is_empty() {
                ui.horizontal(|ui| {
                    ui.label("Column:");
                    egui::ComboBox::from_id_salt("stats_col")
                        .selected_text(*col_names.get(self.stats_selected_col).unwrap_or(&""))
                        .show_ui(ui, |ui| {
                            for (i, name) in col_names.iter().enumerate() {
                                ui.selectable_value(&mut self.stats_selected_col, i, *name);
                            }
                        });
                });

                if let Some(col) = stats.get(self.stats_selected_col) {
                    ui.label(format!("Unique: {} | Empty: {} | Sampled: {}",
                        col.unique_count, col.empty_count, col.total_sampled));
                    ui.add_space(4.0);

                    // Value frequency table
                    egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                        egui::Grid::new("stats_grid").striped(true).show(ui, |ui| {
                            ui.strong("Value");
                            ui.strong("Count");
                            ui.strong("Percentage");
                            ui.end_row();

                            for (val, count) in &col.top_values {
                                let pct = if col.total_sampled > 0 {
                                    *count as f64 / col.total_sampled as f64 * 100.0
                                } else { 0.0 };
                                ui.label(if val.len() > 50 { &val[..50] } else { val });
                                ui.label(count.to_string());
                                ui.label(format!("{:.2}%", pct));
                                ui.end_row();
                            }
                        });
                    });
                }
            }
        }

        if !self.stats_logs.is_empty() {
            ui.separator();
            show_log(ui, &self.stats_logs);
        }
    }

    fn ui_decompress(&mut self, ui: &mut egui::Ui) {
        ui.heading("Decompress");
        ui.add_space(4.0);
        ui.label("Supports: zstd, gzip, bzip2, xz, zip, 7z, tar, tar.gz, tar.zst, tar.bz2, tar.xz");
        ui.add_space(4.0);

        file_picker(ui, "Archive:", &mut self.decomp_input,
            &[("Archives", &["zst", "gz", "bz2", "xz", "zip", "7z", "tar", "tgz", "csv"])]);
        dir_picker(ui, "Output dir:", &mut self.decomp_output_dir);

        let can_start = self.decomp_state != JobState::Running && !self.decomp_input.is_empty();
        ui.horizontal(|ui| {
            if ui.add_enabled(can_start, egui::Button::new("📦 Extract")).clicked() {
                self.start_decompress();
            }
            match self.decomp_state {
                JobState::Running => { ui.spinner(); }
                JobState::Done => { ui.label("✅ Done"); }
                JobState::Error => { ui.colored_label(egui::Color32::RED, "❌ Failed"); }
                JobState::Idle => {}
            }
        });

        if !self.decomp_logs.is_empty() {
            ui.add_space(4.0);
            ui.separator();
            show_log(ui, &self.decomp_logs);
        }
    }
}
