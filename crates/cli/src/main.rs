use clap::Parser;
use csv_splitter_core::{CompressFormat, Delimiter, ProgressEvent, SplitConfig};
use csv_splitter_core::splitter::run_split;
use indicatif::{ProgressBar, ProgressStyle};
use std::path::PathBuf;
use std::sync::mpsc;

#[derive(Parser)]
#[command(name = "csv-splitter", about = "Split large CSV into compressed parts")]
struct Args {
    #[arg(short, long)]
    input: PathBuf,
    #[arg(short = 'n', long, default_value_t = 10)]
    splits: usize,
    #[arg(short, long)]
    output: Option<PathBuf>,
    #[arg(short, long)]
    columns: Option<PathBuf>,
    #[arg(short = 'l', long, default_value_t = 3)]
    level: i32,
    #[arg(long, default_value_t = 64)]
    buf_mb: usize,
    /// Compression: zstd, gzip, bzip2, xz, zip, none
    #[arg(short = 'f', long, default_value = "zstd")]
    format: String,
    /// Delimiter: comma, tab, semicolon, pipe, or a single char
    #[arg(short, long, default_value = "comma")]
    delimiter: String,
    /// Sort by column name before splitting
    #[arg(long)]
    sort: Option<String>,
    /// Sort descending
    #[arg(long)]
    sort_desc: bool,
}

fn parse_format(s: &str) -> CompressFormat {
    match s.to_lowercase().as_str() {
        "zstd" | "zstandard" => CompressFormat::Zstd,
        "gzip" | "gz" => CompressFormat::Gzip,
        "bzip2" | "bz2" => CompressFormat::Bzip2,
        "xz" | "lzma" => CompressFormat::Xz,
        "zip" => CompressFormat::Zip,
        "none" | "raw" => CompressFormat::None,
        _ => CompressFormat::Zstd,
    }
}

fn parse_delimiter(s: &str) -> Delimiter {
    match s.to_lowercase().as_str() {
        "comma" | "," => Delimiter::Comma,
        "tab" | "\\t" | "\t" => Delimiter::Tab,
        "semicolon" | ";" => Delimiter::Semicolon,
        "pipe" | "|" => Delimiter::Pipe,
        other => Delimiter::Custom(other.as_bytes().first().copied().unwrap_or(b',')),
    }
}

fn main() {
    let args = Args::parse();
    let config = SplitConfig {
        input: args.input,
        output: args.output,
        splits: args.splits,
        compress_format: parse_format(&args.format),
        compress_level: args.level,
        buf_mb: args.buf_mb,
        columns_file: args.columns,
        delimiter: parse_delimiter(&args.delimiter),
        sort_column: args.sort,
        sort_descending: args.sort_desc,
    };

    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || run_split(config, tx));

    let mut pb: Option<ProgressBar> = None;
    for event in rx {
        match event {
            ProgressEvent::PhaseStart { phase, total_bytes } => {
                let bar = ProgressBar::new(total_bytes);
                bar.set_style(ProgressStyle::default_bar()
                    .template(&format!("{} [{{bar:40.cyan/blue}}] {{bytes}}/{{total_bytes}} ({{eta}})", phase))
                    .unwrap()
                    .progress_chars("█▓░"));
                pb = Some(bar);
            }
            ProgressEvent::Progress { bytes } => {
                if let Some(ref bar) = pb { bar.set_position(bytes); }
            }
            ProgressEvent::PhaseDone { .. } => {
                if let Some(bar) = pb.take() { bar.finish_with_message("done"); }
            }
            ProgressEvent::Info(msg) => println!("ℹ️  {}", msg),
            ProgressEvent::Finished { output } => println!("\n✅ Done! {}", output.display()),
            ProgressEvent::Error(e) => { eprintln!("❌ {}", e); std::process::exit(1); }
            _ => {}
        }
    }
    handle.join().ok();
}
