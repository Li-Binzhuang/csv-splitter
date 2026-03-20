use crate::types::*;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

/// Detect archive format by magic bytes and extension, then extract CSV files.
/// Returns path to extracted CSV (or original path if not compressed).
pub fn decompress_to_csv(input: &Path, output_dir: &Path, tx: &ProgressSender) -> io::Result<PathBuf> {
    let ext = input.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    let stem = input.file_stem().unwrap().to_string_lossy().to_string();

    // Check magic bytes
    let mut magic = [0u8; 8];
    let mut f = File::open(input)?;
    let n = f.read(&mut magic)?;
    drop(f);

    let format = detect_format(&magic[..n], &ext);
    tx.send(ProgressEvent::Info(format!("Detected format: {}", format_name(&format)))).ok();

    match format {
        ArchiveFormat::Csv => Ok(input.to_path_buf()),
        ArchiveFormat::Zstd => decompress_zstd(input, output_dir, &stem, tx),
        ArchiveFormat::Gzip => decompress_gzip(input, output_dir, &stem, tx),
        ArchiveFormat::Bzip2 => decompress_bzip2(input, output_dir, &stem, tx),
        ArchiveFormat::Xz => decompress_xz(input, output_dir, &stem, tx),
        ArchiveFormat::Zip => decompress_zip(input, output_dir, tx),
        ArchiveFormat::SevenZ => decompress_7z(input, output_dir, tx),
        ArchiveFormat::Tar => decompress_tar(input, output_dir, tx),
        ArchiveFormat::TarGz => decompress_tar_gz(input, output_dir, tx),
        ArchiveFormat::TarZst => decompress_tar_zst(input, output_dir, tx),
        ArchiveFormat::TarBz2 => decompress_tar_bz2(input, output_dir, tx),
        ArchiveFormat::TarXz => decompress_tar_xz(input, output_dir, tx),
        ArchiveFormat::Unknown => Err(io::Error::new(io::ErrorKind::InvalidData, "Unknown archive format")),
    }
}

#[derive(Debug)]
enum ArchiveFormat {
    Csv, Zstd, Gzip, Bzip2, Xz, Zip, SevenZ, Tar,
    TarGz, TarZst, TarBz2, TarXz, Unknown,
}

fn detect_format(magic: &[u8], ext: &str) -> ArchiveFormat {
    // Compound extensions first
    if ext == "tar.gz" || ext == "tgz" || ext.ends_with(".tar.gz") { return ArchiveFormat::TarGz; }
    if ext.ends_with(".tar.zst") { return ArchiveFormat::TarZst; }
    if ext.ends_with(".tar.bz2") || ext.ends_with(".tar.bz") { return ArchiveFormat::TarBz2; }
    if ext.ends_with(".tar.xz") { return ArchiveFormat::TarXz; }

    // Magic bytes
    if magic.len() >= 4 && magic[..4] == [0x28, 0xb5, 0x2f, 0xfd] {
        return if ext.contains("tar") { ArchiveFormat::TarZst } else { ArchiveFormat::Zstd };
    }
    if magic.len() >= 2 && magic[..2] == [0x1f, 0x8b] {
        return if ext.contains("tar") || ext == "tgz" { ArchiveFormat::TarGz } else { ArchiveFormat::Gzip };
    }
    if magic.len() >= 3 && magic[..3] == [b'B', b'Z', b'h'] {
        return if ext.contains("tar") { ArchiveFormat::TarBz2 } else { ArchiveFormat::Bzip2 };
    }
    if magic.len() >= 6 && magic[..6] == [0xfd, 0x37, 0x7a, 0x58, 0x5a, 0x00] {
        return if ext.contains("tar") { ArchiveFormat::TarXz } else { ArchiveFormat::Xz };
    }
    if magic.len() >= 4 && magic[..4] == [0x50, 0x4b, 0x03, 0x04] { return ArchiveFormat::Zip; }
    if magic.len() >= 6 && magic[..6] == [0x37, 0x7a, 0xbc, 0xaf, 0x27, 0x1c] { return ArchiveFormat::SevenZ; }

    // Extension fallback
    match ext {
        "csv" | "tsv" | "txt" => ArchiveFormat::Csv,
        "zst" | "zstd" => ArchiveFormat::Zstd,
        "gz" | "gzip" => ArchiveFormat::Gzip,
        "bz2" | "bz" => ArchiveFormat::Bzip2,
        "xz" | "lzma" => ArchiveFormat::Xz,
        "zip" => ArchiveFormat::Zip,
        "7z" => ArchiveFormat::SevenZ,
        "tar" => ArchiveFormat::Tar,
        "tgz" => ArchiveFormat::TarGz,
        _ => ArchiveFormat::Unknown,
    }
}

fn format_name(f: &ArchiveFormat) -> &str {
    match f {
        ArchiveFormat::Csv => "CSV (plain)",
        ArchiveFormat::Zstd => "Zstandard",
        ArchiveFormat::Gzip => "Gzip",
        ArchiveFormat::Bzip2 => "Bzip2",
        ArchiveFormat::Xz => "XZ/LZMA",
        ArchiveFormat::Zip => "Zip",
        ArchiveFormat::SevenZ => "7-Zip",
        ArchiveFormat::Tar => "Tar",
        ArchiveFormat::TarGz => "Tar+Gzip",
        ArchiveFormat::TarZst => "Tar+Zstd",
        ArchiveFormat::TarBz2 => "Tar+Bzip2",
        ArchiveFormat::TarXz => "Tar+XZ",
        ArchiveFormat::Unknown => "Unknown",
    }
}

const BUF_SIZE: usize = 64 * 1024 * 1024;

fn stream_decompress(reader: &mut dyn Read, out_path: &Path) -> io::Result<()> {
    let mut writer = BufWriter::with_capacity(BUF_SIZE, File::create(out_path)?);
    let mut buf = vec![0u8; BUF_SIZE];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 { break; }
        writer.write_all(&buf[..n])?;
    }
    writer.flush()?;
    Ok(())
}

fn decompress_zstd(input: &Path, out_dir: &Path, stem: &str, tx: &ProgressSender) -> io::Result<PathBuf> {
    tx.send(ProgressEvent::Info("Decompressing zstd...".into())).ok();
    let out = out_dir.join(format!("{}.csv", stem.trim_end_matches(".csv")));
    let file = BufReader::with_capacity(BUF_SIZE, File::open(input)?);
    let mut decoder = zstd::Decoder::new(file)?;
    stream_decompress(&mut decoder, &out)?;
    Ok(out)
}

fn decompress_gzip(input: &Path, out_dir: &Path, stem: &str, tx: &ProgressSender) -> io::Result<PathBuf> {
    tx.send(ProgressEvent::Info("Decompressing gzip...".into())).ok();
    let out = out_dir.join(format!("{}.csv", stem.trim_end_matches(".csv")));
    let file = BufReader::with_capacity(BUF_SIZE, File::open(input)?);
    let mut decoder = flate2::read::GzDecoder::new(file);
    stream_decompress(&mut decoder, &out)?;
    Ok(out)
}

fn decompress_bzip2(input: &Path, out_dir: &Path, stem: &str, tx: &ProgressSender) -> io::Result<PathBuf> {
    tx.send(ProgressEvent::Info("Decompressing bzip2...".into())).ok();
    let out = out_dir.join(format!("{}.csv", stem.trim_end_matches(".csv")));
    let file = BufReader::with_capacity(BUF_SIZE, File::open(input)?);
    let mut decoder = bzip2::read::BzDecoder::new(file);
    stream_decompress(&mut decoder, &out)?;
    Ok(out)
}

fn decompress_xz(input: &Path, out_dir: &Path, stem: &str, tx: &ProgressSender) -> io::Result<PathBuf> {
    tx.send(ProgressEvent::Info("Decompressing xz...".into())).ok();
    let out = out_dir.join(format!("{}.csv", stem.trim_end_matches(".csv")));
    let file = BufReader::with_capacity(BUF_SIZE, File::open(input)?);
    let mut decoder = xz2::read::XzDecoder::new(file);
    stream_decompress(&mut decoder, &out)?;
    Ok(out)
}

fn find_csv_in_dir(dir: &Path) -> io::Result<PathBuf> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let p = entry.path();
        if p.is_file() {
            let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
            if ext == "csv" || ext == "tsv" || ext == "txt" {
                return Ok(p);
            }
        }
    }
    // If no CSV found, return first file
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if entry.path().is_file() { return Ok(entry.path()); }
    }
    Err(io::Error::new(io::ErrorKind::NotFound, "No CSV found in archive"))
}

fn decompress_zip(input: &Path, out_dir: &Path, tx: &ProgressSender) -> io::Result<PathBuf> {
    tx.send(ProgressEvent::Info("Extracting zip...".into())).ok();
    let file = File::open(input)?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        if entry.is_file() {
            let name = entry.name().to_string();
            let out_path = out_dir.join(&name);
            if let Some(parent) = out_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut out_file = File::create(&out_path)?;
            io::copy(&mut entry, &mut out_file)?;
        }
    }
    find_csv_in_dir(out_dir)
}

fn decompress_7z(input: &Path, out_dir: &Path, tx: &ProgressSender) -> io::Result<PathBuf> {
    tx.send(ProgressEvent::Info("Extracting 7z...".into())).ok();
    sevenz_rust::decompress_file(input, out_dir)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    find_csv_in_dir(out_dir)
}

fn extract_tar_from_reader(reader: impl Read, out_dir: &Path) -> io::Result<PathBuf> {
    let mut archive = tar::Archive::new(reader);
    archive.unpack(out_dir)?;
    find_csv_in_dir(out_dir)
}

fn decompress_tar(input: &Path, out_dir: &Path, tx: &ProgressSender) -> io::Result<PathBuf> {
    tx.send(ProgressEvent::Info("Extracting tar...".into())).ok();
    let file = BufReader::with_capacity(BUF_SIZE, File::open(input)?);
    extract_tar_from_reader(file, out_dir)
}

fn decompress_tar_gz(input: &Path, out_dir: &Path, tx: &ProgressSender) -> io::Result<PathBuf> {
    tx.send(ProgressEvent::Info("Extracting tar.gz...".into())).ok();
    let file = BufReader::with_capacity(BUF_SIZE, File::open(input)?);
    let gz = flate2::read::GzDecoder::new(file);
    extract_tar_from_reader(gz, out_dir)
}

fn decompress_tar_zst(input: &Path, out_dir: &Path, tx: &ProgressSender) -> io::Result<PathBuf> {
    tx.send(ProgressEvent::Info("Extracting tar.zst...".into())).ok();
    let file = BufReader::with_capacity(BUF_SIZE, File::open(input)?);
    let zst = zstd::Decoder::new(file)?;
    extract_tar_from_reader(zst, out_dir)
}

fn decompress_tar_bz2(input: &Path, out_dir: &Path, tx: &ProgressSender) -> io::Result<PathBuf> {
    tx.send(ProgressEvent::Info("Extracting tar.bz2...".into())).ok();
    let file = BufReader::with_capacity(BUF_SIZE, File::open(input)?);
    let bz = bzip2::read::BzDecoder::new(file);
    extract_tar_from_reader(bz, out_dir)
}

fn decompress_tar_xz(input: &Path, out_dir: &Path, tx: &ProgressSender) -> io::Result<PathBuf> {
    tx.send(ProgressEvent::Info("Extracting tar.xz...".into())).ok();
    let file = BufReader::with_capacity(BUF_SIZE, File::open(input)?);
    let xz = xz2::read::XzDecoder::new(file);
    extract_tar_from_reader(xz, out_dir)
}
