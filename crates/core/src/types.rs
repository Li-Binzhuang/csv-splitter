use std::path::PathBuf;
use std::sync::mpsc;

/// Supported compression formats for output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompressFormat {
    Zstd,
    Gzip,
    Bzip2,
    Xz,
    Zip,
    None,
}

impl CompressFormat {
    pub fn extension(&self) -> &str {
        match self {
            Self::Zstd => ".zst",
            Self::Gzip => ".gz",
            Self::Bzip2 => ".bz2",
            Self::Xz => ".xz",
            Self::Zip => ".zip",
            Self::None => "",
        }
    }

    pub fn all() -> &'static [CompressFormat] {
        &[
            Self::Zstd,
            Self::Gzip,
            Self::Bzip2,
            Self::Xz,
            Self::Zip,
            Self::None,
        ]
    }

    pub fn label(&self) -> &str {
        match self {
            Self::Zstd => "Zstandard (.zst)",
            Self::Gzip => "Gzip (.gz)",
            Self::Bzip2 => "Bzip2 (.bz2)",
            Self::Xz => "XZ/LZMA (.xz)",
            Self::Zip => "Zip (.zip)",
            Self::None => "No compression",
        }
    }
}

/// CSV delimiter options.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Delimiter {
    #[default]
    Comma,
    Tab,
    Semicolon,
    Pipe,
    Custom(u8),
}

impl Delimiter {
    pub fn byte(&self) -> u8 {
        match self {
            Self::Comma => b',',
            Self::Tab => b'\t',
            Self::Semicolon => b';',
            Self::Pipe => b'|',
            Self::Custom(b) => *b,
        }
    }

    pub fn label(&self) -> &str {
        match self {
            Self::Comma => "Comma (,)",
            Self::Tab => "Tab (\\t)",
            Self::Semicolon => "Semicolon (;)",
            Self::Pipe => "Pipe (|)",
            Self::Custom(_) => "Custom",
        }
    }

    pub fn common() -> &'static [Delimiter] {
        &[Self::Comma, Self::Tab, Self::Semicolon, Self::Pipe]
    }
}

/// Configuration for a split job.
#[derive(Clone, Debug)]
pub struct SplitConfig {
    pub input: PathBuf,
    pub output: Option<PathBuf>,
    pub splits: usize,
    /// Split by target chunk size instead of chunk count. When set, `splits` is
    /// ignored and the chunk count is derived from the data size.
    pub max_bytes: Option<u64>,
    pub compress_format: CompressFormat,
    pub compress_level: i32,
    pub buf_mb: usize,
    pub columns_file: Option<PathBuf>,
    pub delimiter: Delimiter,
    pub sort_column: Option<String>,
    pub sort_descending: bool,
}

impl Default for SplitConfig {
    fn default() -> Self {
        Self {
            input: PathBuf::new(),
            output: None,
            splits: 10,
            max_bytes: None,
            compress_format: CompressFormat::Zstd,
            compress_level: 3,
            buf_mb: 64,
            columns_file: None,
            delimiter: Delimiter::Comma,
            sort_column: None,
            sort_descending: false,
        }
    }
}

/// Column statistics from sampling.
#[derive(Clone, Debug)]
pub struct ColumnStats {
    pub name: String,
    pub total_sampled: u64,
    /// Top values by frequency: (value, count)
    pub top_values: Vec<(String, u64)>,
    pub unique_count: u64,
    pub empty_count: u64,
}

/// Preview data from a CSV file.
#[derive(Clone, Debug, Default)]
pub struct CsvPreview {
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub total_rows_estimate: u64,
    pub file_size: u64,
    pub detected_delimiter: Delimiter,
}

/// Progress events sent from core to UI/CLI.
#[derive(Clone, Debug)]
pub enum ProgressEvent {
    PhaseStart {
        phase: String,
        total_bytes: u64,
    },
    Progress {
        bytes: u64,
    },
    Info(String),
    PhaseDone {
        phase: String,
    },
    Finished {
        output: PathBuf,
    },
    Error(String),
    /// Preview data ready
    PreviewReady(CsvPreview),
    /// Column stats ready
    StatsReady(Vec<ColumnStats>),
    /// Decompression done, extracted CSV path
    DecompressDone {
        csv_path: PathBuf,
    },
    /// Decompression done, multiple extracted files
    DecompressFiles {
        paths: Vec<PathBuf>,
    },
}

pub type ProgressSender = mpsc::Sender<ProgressEvent>;
