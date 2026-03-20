use csv_splitter_core::*;
use std::sync::mpsc;

#[derive(PartialEq, Clone, Copy)]
pub enum Tab {
    Split,
    Preview,
    Stats,
    Decompress,
}

impl Tab {
    pub fn label(&self) -> &str {
        match self {
            Tab::Split => "🔪 拆分压缩",
            Tab::Preview => "👁 数据预览",
            Tab::Stats => "📊 列统计",
            Tab::Decompress => "📦 解压缩",
        }
    }

    pub fn all() -> &'static [Tab] {
        &[Tab::Split, Tab::Preview, Tab::Stats, Tab::Decompress]
    }
}

#[derive(PartialEq)]
pub enum JobState {
    Idle,
    Running,
    Done,
    Error,
}

/// Split tab state
pub struct SplitState {
    pub input_path: String,
    pub output_path: String,
    pub columns_path: String,
    pub splits: usize,
    pub compress_idx: usize,
    pub compress_level: i32,
    pub buf_mb: usize,
    pub delim_idx: usize,
    pub custom_delim: String,
    pub sort_column: String,
    pub sort_desc: bool,
    pub state: JobState,
    pub rx: Option<mpsc::Receiver<ProgressEvent>>,
    pub logs: Vec<String>,
    pub current_phase: String,
    pub progress_bytes: u64,
    pub progress_total: u64,
}

impl Default for SplitState {
    fn default() -> Self {
        Self {
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
        }
    }
}

/// Preview tab state
pub struct PreviewState {
    pub path: String,
    pub delim_idx: usize,
    pub custom_delim: String,
    pub preview: Option<CsvPreview>,
}

impl Default for PreviewState {
    fn default() -> Self {
        Self {
            path: String::new(),
            delim_idx: 0,
            custom_delim: String::new(),
            preview: None,
        }
    }
}

/// Stats tab state
pub struct StatsState {
    pub path: String,
    pub delim_idx: usize,
    pub custom_delim: String,
    pub selected_col: usize,
    pub sample_size: usize,
    pub state: JobState,
    pub rx: Option<mpsc::Receiver<ProgressEvent>>,
    pub logs: Vec<String>,
    pub stats: Option<Vec<ColumnStats>>,
}

impl Default for StatsState {
    fn default() -> Self {
        Self {
            path: String::new(),
            delim_idx: 0,
            custom_delim: String::new(),
            selected_col: 0,
            sample_size: 100_000,
            state: JobState::Idle,
            rx: None,
            logs: Vec::new(),
            stats: None,
        }
    }
}

/// Decompress tab state
pub struct DecompState {
    pub input: String,
    pub output_dir: String,
    pub state: JobState,
    pub rx: Option<mpsc::Receiver<ProgressEvent>>,
    pub logs: Vec<String>,
}

impl Default for DecompState {
    fn default() -> Self {
        Self {
            input: String::new(),
            output_dir: String::new(),
            state: JobState::Idle,
            rx: None,
            logs: Vec::new(),
        }
    }
}
