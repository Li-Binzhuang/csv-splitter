use crate::types::*;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

/// Decompress/extract an archive into the output directory.
/// Returns a list of all extracted file paths.
pub fn decompress_file(
    input: &Path,
    output_dir: &Path,
    tx: &ProgressSender,
) -> io::Result<Vec<PathBuf>> {
    let ext = extension_lower(input);
    let stem = input.file_stem().unwrap().to_string_lossy().to_string();

    let mut magic = [0u8; 8];
    let mut f = File::open(input)?;
    let n = f.read(&mut magic)?;
    drop(f);

    let format = detect_format(&magic[..n], &ext);
    tx.send(ProgressEvent::Info(format!(
        "Detected format: {}",
        format_name(&format)
    )))
    .ok();

    match format {
        ArchiveFormat::Plain => {
            // Not compressed — just copy to output dir
            let dest = output_dir.join(input.file_name().unwrap());
            if dest != input {
                std::fs::copy(input, &dest)?;
            }
            Ok(vec![dest])
        }
        ArchiveFormat::Zstd => decompress_single(input, output_dir, &stem, tx, |r| {
            Ok(Box::new(zstd::Decoder::new(r)?))
        }),
        ArchiveFormat::Gzip => decompress_single(input, output_dir, &stem, tx, |r| {
            Ok(Box::new(flate2::read::GzDecoder::new(r)))
        }),
        ArchiveFormat::Bzip2 => decompress_single(input, output_dir, &stem, tx, |r| {
            Ok(Box::new(bzip2::read::BzDecoder::new(r)))
        }),
        ArchiveFormat::Xz => decompress_single(input, output_dir, &stem, tx, |r| {
            Ok(Box::new(xz2::read::XzDecoder::new(r)))
        }),
        ArchiveFormat::Zip => decompress_zip(input, output_dir, tx),
        ArchiveFormat::SevenZ => decompress_7z(input, output_dir, tx),
        ArchiveFormat::Tar => decompress_tar_reader(
            BufReader::with_capacity(BUF_SIZE, File::open(input)?),
            output_dir,
            tx,
        ),
        ArchiveFormat::TarGz => {
            let r = flate2::read::GzDecoder::new(BufReader::with_capacity(
                BUF_SIZE,
                File::open(input)?,
            ));
            decompress_tar_reader(r, output_dir, tx)
        }
        ArchiveFormat::TarZst => {
            let r = zstd::Decoder::new(BufReader::with_capacity(BUF_SIZE, File::open(input)?))?;
            decompress_tar_reader(r, output_dir, tx)
        }
        ArchiveFormat::TarBz2 => {
            let r =
                bzip2::read::BzDecoder::new(BufReader::with_capacity(BUF_SIZE, File::open(input)?));
            decompress_tar_reader(r, output_dir, tx)
        }
        ArchiveFormat::TarXz => {
            let r =
                xz2::read::XzDecoder::new(BufReader::with_capacity(BUF_SIZE, File::open(input)?));
            decompress_tar_reader(r, output_dir, tx)
        }
        ArchiveFormat::Unknown => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Unknown archive format",
        )),
    }
}

/// Backward-compatible wrapper that returns the first extracted file.
pub fn decompress_to_csv(
    input: &Path,
    output_dir: &Path,
    tx: &ProgressSender,
) -> io::Result<PathBuf> {
    let files = decompress_file(input, output_dir, tx)?;
    files
        .into_iter()
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "No files extracted"))
}

#[derive(Debug)]
enum ArchiveFormat {
    Plain,
    Zstd,
    Gzip,
    Bzip2,
    Xz,
    Zip,
    SevenZ,
    Tar,
    TarGz,
    TarZst,
    TarBz2,
    TarXz,
    Unknown,
}

fn extension_lower(p: &Path) -> String {
    let name = p
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase();
    // Handle compound extensions like .tar.gz
    if name.ends_with(".tar.gz") || name.ends_with(".tar.gzip") {
        return "tar.gz".into();
    }
    if name.ends_with(".tar.zst") || name.ends_with(".tar.zstd") {
        return "tar.zst".into();
    }
    if name.ends_with(".tar.bz2") || name.ends_with(".tar.bz") {
        return "tar.bz2".into();
    }
    if name.ends_with(".tar.xz") || name.ends_with(".tar.lzma") {
        return "tar.xz".into();
    }
    p.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase()
}

fn detect_format(magic: &[u8], ext: &str) -> ArchiveFormat {
    // Compound extensions first
    match ext {
        "tar.gz" | "tgz" => return ArchiveFormat::TarGz,
        "tar.zst" | "tar.zstd" => return ArchiveFormat::TarZst,
        "tar.bz2" | "tar.bz" => return ArchiveFormat::TarBz2,
        "tar.xz" | "tar.lzma" => return ArchiveFormat::TarXz,
        _ => {}
    }

    // Magic bytes
    if magic.len() >= 4 && magic[..4] == [0x28, 0xb5, 0x2f, 0xfd] {
        return if ext.contains("tar") {
            ArchiveFormat::TarZst
        } else {
            ArchiveFormat::Zstd
        };
    }
    if magic.len() >= 2 && magic[..2] == [0x1f, 0x8b] {
        return if ext.contains("tar") || ext == "tgz" {
            ArchiveFormat::TarGz
        } else {
            ArchiveFormat::Gzip
        };
    }
    if magic.len() >= 3 && magic[..3] == [b'B', b'Z', b'h'] {
        return if ext.contains("tar") {
            ArchiveFormat::TarBz2
        } else {
            ArchiveFormat::Bzip2
        };
    }
    if magic.len() >= 6 && magic[..6] == [0xfd, 0x37, 0x7a, 0x58, 0x5a, 0x00] {
        return if ext.contains("tar") {
            ArchiveFormat::TarXz
        } else {
            ArchiveFormat::Xz
        };
    }
    if magic.len() >= 4 && magic[..4] == [0x50, 0x4b, 0x03, 0x04] {
        return ArchiveFormat::Zip;
    }
    if magic.len() >= 6 && magic[..6] == [0x37, 0x7a, 0xbc, 0xaf, 0x27, 0x1c] {
        return ArchiveFormat::SevenZ;
    }

    // Extension fallback
    match ext {
        "csv" | "tsv" | "txt" | "json" | "xml" | "log" => ArchiveFormat::Plain,
        "zst" | "zstd" => ArchiveFormat::Zstd,
        "gz" | "gzip" => ArchiveFormat::Gzip,
        "bz2" | "bz" => ArchiveFormat::Bzip2,
        "xz" | "lzma" => ArchiveFormat::Xz,
        "zip" | "jar" | "war" | "apk" => ArchiveFormat::Zip,
        "7z" => ArchiveFormat::SevenZ,
        "tar" => ArchiveFormat::Tar,
        "tgz" => ArchiveFormat::TarGz,
        _ => ArchiveFormat::Unknown,
    }
}

fn format_name(f: &ArchiveFormat) -> &str {
    match f {
        ArchiveFormat::Plain => "Plain file",
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

/// Decompress a single-stream compressed file (zstd, gzip, bzip2, xz).
/// The output filename is the stem (original name without the compression extension).
fn decompress_single<F>(
    input: &Path,
    out_dir: &Path,
    stem: &str,
    tx: &ProgressSender,
    make_decoder: F,
) -> io::Result<Vec<PathBuf>>
where
    F: FnOnce(BufReader<File>) -> io::Result<Box<dyn Read>>,
{
    tx.send(ProgressEvent::Info(format!(
        "Decompressing {}...",
        input.file_name().unwrap().to_string_lossy()
    )))
    .ok();
    // Use stem as output name; if stem still has an extension, keep it, otherwise add nothing
    let out = out_dir.join(stem);
    let file = BufReader::with_capacity(BUF_SIZE, File::open(input)?);
    let mut decoder = make_decoder(file)?;
    stream_to_file(&mut decoder, &out)?;
    let size = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
    tx.send(ProgressEvent::Info(format!(
        "  -> {} ({} bytes)",
        out.file_name().unwrap().to_string_lossy(),
        size
    )))
    .ok();
    Ok(vec![out])
}

fn stream_to_file(reader: &mut dyn Read, out_path: &Path) -> io::Result<()> {
    let mut writer = BufWriter::with_capacity(BUF_SIZE, File::create(out_path)?);
    let mut buf = vec![0u8; BUF_SIZE];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        writer.write_all(&buf[..n])?;
    }
    writer.flush()?;
    Ok(())
}

fn decompress_zip(input: &Path, out_dir: &Path, tx: &ProgressSender) -> io::Result<Vec<PathBuf>> {
    tx.send(ProgressEvent::Info("Extracting zip...".into()))
        .ok();
    let file = File::open(input)?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let mut extracted = Vec::new();
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let name = entry.name().to_string();
        if entry.is_dir() {
            std::fs::create_dir_all(out_dir.join(&name))?;
            continue;
        }
        let out_path = out_dir.join(&name);
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut out_file = File::create(&out_path)?;
        io::copy(&mut entry, &mut out_file)?;
        let size = std::fs::metadata(&out_path).map(|m| m.len()).unwrap_or(0);
        tx.send(ProgressEvent::Info(format!(
            "  -> {} ({} bytes)",
            name, size
        )))
        .ok();
        extracted.push(out_path);
    }
    Ok(extracted)
}

fn decompress_7z(input: &Path, out_dir: &Path, tx: &ProgressSender) -> io::Result<Vec<PathBuf>> {
    tx.send(ProgressEvent::Info("Extracting 7z...".into())).ok();
    sevenz_rust::decompress_file(input, out_dir)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    collect_files_recursive(out_dir, tx)
}

fn decompress_tar_reader(
    reader: impl Read,
    out_dir: &Path,
    tx: &ProgressSender,
) -> io::Result<Vec<PathBuf>> {
    tx.send(ProgressEvent::Info("Extracting tar archive...".into()))
        .ok();
    let mut archive = tar::Archive::new(reader);
    archive.unpack(out_dir)?;
    collect_files_recursive(out_dir, tx)
}

/// Walk a directory recursively and collect all files.
fn collect_files_recursive(dir: &Path, tx: &ProgressSender) -> io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    walk_dir(dir, &mut files)?;
    files.sort();
    for f in &files {
        let size = std::fs::metadata(f).map(|m| m.len()).unwrap_or(0);
        let name = f.strip_prefix(dir).unwrap_or(f);
        tx.send(ProgressEvent::Info(format!(
            "  -> {} ({} bytes)",
            name.display(),
            size
        )))
        .ok();
    }
    Ok(files)
}

fn walk_dir(dir: &Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let p = entry.path();
        if p.is_dir() {
            walk_dir(&p, out)?;
        } else {
            out.push(p);
        }
    }
    Ok(())
}
