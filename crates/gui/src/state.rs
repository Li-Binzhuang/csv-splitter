use csv_splitter_core::*;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::SystemTime;

use crate::i18n::{t, Lang};

#[derive(PartialEq, Clone, Copy)]
pub enum Tab {
    Split,
    Preview,
    Stats,
    Decompress,
}

impl Tab {
    pub fn label(&self, lang: Lang) -> &'static str {
        match self {
            Tab::Split => t!(lang, TabSplit),
            Tab::Preview => t!(lang, TabPreview),
            Tab::Stats => t!(lang, TabStats),
            Tab::Decompress => t!(lang, TabDecompress),
        }
    }
    pub fn all() -> &'static [Tab] {
        &[Tab::Split, Tab::Preview, Tab::Stats, Tab::Decompress]
    }
}

#[derive(PartialEq, Clone, Copy)]
pub enum JobState {
    Idle,
    Running,
    Done,
    Error,
}

/// Metadata about a dropped/selected file.
#[derive(Clone, Debug)]
pub struct FileInfo {
    pub path: PathBuf,
    pub name: String,
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub extension: String,
}

impl FileInfo {
    pub fn from_path(path: PathBuf) -> Option<Self> {
        let meta = std::fs::metadata(&path).ok()?;
        let name = path.file_name()?.to_string_lossy().to_string();
        let extension = path
            .extension()
            .map(|e| e.to_string_lossy().to_string())
            .unwrap_or_default()
            .to_uppercase();
        Some(Self {
            path,
            name,
            size: meta.len(),
            modified: meta.modified().ok(),
            extension,
        })
    }
}

/// Split tab state
pub struct SplitState {
    pub input_file: Option<FileInfo>,
    pub columns_file: Option<FileInfo>,
    pub output_path: String,
    pub splits: usize,
    pub no_split: bool,
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
    pub output_file: Option<FileInfo>,
    pub anim_time: f64,
}

impl Default for SplitState {
    fn default() -> Self {
        Self {
            input_file: None,
            columns_file: None,
            output_path: String::new(),
            splits: 3,
            no_split: false,
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
            output_file: None,
            anim_time: 0.0,
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
