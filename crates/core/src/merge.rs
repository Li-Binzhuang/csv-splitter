//! Re-assemble the chunks produced by [`crate::splitter::run_split`].
//!
//! The splitter writes `stem_0001.csv[.zst|.gz|.bz2|.xz]` into a tar (or a zip),
//! each chunk carrying its own copy of the header. Merging is therefore:
//! keep the first header, drop the rest, concatenate in file-name order.
//!
//! This is the inverse operation the tool never had — and the cheapest way to
//! prove a split is lossless (`split` then `merge` must reproduce the input).

use crate::csv_reader::RecordReader;
use crate::types::ProgressEvent;
use crate::types::ProgressSender;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

const IO_BUF: usize = 1024 * 1024;

/// Summary of a merge run.
#[derive(Clone, Debug)]
pub struct MergeReport {
    pub output: PathBuf,
    pub chunks: usize,
    pub records: u64,
    pub bytes: u64,
}

/// Merge a split archive (`.tar`, `.tar.gz`, `.tar.zst`, `.tar.bz2`, `.tar.xz`,
/// `.zip`) or a directory of chunk files back into a single CSV.
pub fn merge_archive(input: &Path, output: &Path, tx: &ProgressSender) -> io::Result<MergeReport> {
    let readers: Vec<(String, Box<dyn Read>)> = if input.is_dir() {
        collect_from_dir(input)?
    } else {
        match sniff_container(input)? {
            Container::Zip => collect_from_zip(input)?,
            Container::Tar => collect_from_tar(input, Container::Tar)?,
            Container::TarGz => collect_from_tar(input, Container::TarGz)?,
            Container::TarZst => collect_from_tar(input, Container::TarZst)?,
            Container::TarBz2 => collect_from_tar(input, Container::TarBz2)?,
            Container::TarXz => collect_from_tar(input, Container::TarXz)?,
            Container::Unknown => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("unsupported merge input: {}", input.display()),
                ))
            }
        }
    };

    if readers.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "no chunks found in archive",
        ));
    }

    write_merged(readers, output, tx)
}

fn write_merged(
    mut chunks: Vec<(String, Box<dyn Read>)>,
    output: &Path,
    tx: &ProgressSender,
) -> io::Result<MergeReport> {
    // Chunk names are zero-padded, so lexicographic order is the split order.
    chunks.sort_by(|a, b| a.0.cmp(&b.0));

    if let Some(parent) = output.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }

    let mut out = BufWriter::with_capacity(IO_BUF, File::create(output)?);
    let mut report = MergeReport {
        output: output.to_path_buf(),
        chunks: chunks.len(),
        records: 0,
        bytes: 0,
    };

    let mut scratch = Vec::with_capacity(256 * 1024);
    for (idx, (name, reader)) in chunks.into_iter().enumerate() {
        let mut rr = RecordReader::new(reader, 256 * 1024);
        let mut first = true;
        while rr.next_record(&mut scratch)? {
            if first {
                first = false;
                // Only the first chunk contributes a header; the others merely
                // repeat it.
                if idx > 0 {
                    continue;
                }
            }
            out.write_all(&scratch)?;
            report.records += 1;
            report.bytes += scratch.len() as u64;
        }
        tx.send(ProgressEvent::Info(format!("merged chunk {name}")))
            .ok();
    }

    out.flush()?;
    tx.send(ProgressEvent::Progress {
        bytes: report.bytes,
    })
    .ok();
    Ok(report)
}

fn collect_from_zip(input: &Path) -> io::Result<Vec<(String, Box<dyn Read>)>> {
    let file = File::open(input)?;
    let mut zip = zip::ZipArchive::new(BufReader::new(file)).map_err(io::Error::other)?;
    let mut names: Vec<String> = (0..zip.len())
        .filter_map(|i| zip.by_index(i).ok().map(|f| f.name().to_string()))
        .filter(|n| is_chunk(n))
        .collect();
    names.sort();

    // `by_index` borrows the archive mutably, so extract the bytes up front.
    let mut out: Vec<(String, Box<dyn Read>)> = Vec::with_capacity(names.len());
    for name in names {
        let mut entry = zip.by_name(&name).map_err(io::Error::other)?;
        let mut buf = Vec::new();
        entry.read_to_end(&mut buf)?;
        out.push((
            name.clone(),
            decompress_by_name(name, Box::new(io::Cursor::new(buf)))?,
        ));
    }
    Ok(out)
}

/// Container detection by magic bytes first, extension second: a split output
/// named `.tar` may actually be a zip if the caller picked a custom name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Container {
    Zip,
    Tar,
    TarGz,
    TarZst,
    TarBz2,
    TarXz,
    Unknown,
}

fn sniff_container(input: &Path) -> io::Result<Container> {
    let mut f = File::open(input)?;
    let mut magic = [0u8; 6];
    let n = f.read(&mut magic)?;
    let magic = &magic[..n];
    if magic.starts_with(b"PK") {
        return Ok(Container::Zip);
    }
    if magic.starts_with(&[0x1f, 0x8b]) {
        return Ok(Container::TarGz);
    }
    if magic.starts_with(&[0x28, 0xB5, 0x2F, 0xFD]) {
        return Ok(Container::TarZst);
    }
    if magic.starts_with(b"BZh") {
        return Ok(Container::TarBz2);
    }
    if magic.starts_with(&[0xFD, b'7', b'z', b'X', b'Z', 0x00]) {
        return Ok(Container::TarXz);
    }
    // tar has no magic; a valid archive has a name right at offset 0.
    if magic.is_empty() {
        return Ok(Container::Unknown);
    }
    Ok(Container::Tar)
}

fn collect_from_tar(
    input: &Path,
    container: Container,
) -> io::Result<Vec<(String, Box<dyn Read>)>> {
    let file = BufReader::with_capacity(IO_BUF, File::open(input)?);
    let reader: Box<dyn Read> = match container {
        Container::TarGz => Box::new(flate2::read::GzDecoder::new(file)),
        Container::TarZst => Box::new(zstd::Decoder::new(file)?),
        Container::TarBz2 => Box::new(bzip2::read::BzDecoder::new(file)),
        Container::TarXz => Box::new(xz2::read::XzDecoder::new(file)),
        _ => Box::new(file),
    };

    let mut archive = tar::Archive::new(reader);
    let mut out = Vec::new();
    for entry in archive.entries()? {
        let mut entry = entry?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let name = entry
            .path()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        if !is_chunk(&name) {
            continue;
        }
        let mut buf = Vec::new();
        entry.read_to_end(&mut buf)?;
        out.push((
            name.clone(),
            decompress_by_name(name, Box::new(io::Cursor::new(buf)))?,
        ));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

fn collect_from_dir(input: &Path) -> io::Result<Vec<(String, Box<dyn Read>)>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(input)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if !is_chunk(&name) {
            continue;
        }
        let reader: Box<dyn Read> = decompress_by_name(name.clone(), Box::new(File::open(&path)?))?;
        out.push((name, reader));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

fn is_chunk(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower.ends_with(".csv")
        || lower.ends_with(".csv.zst")
        || lower.ends_with(".csv.gz")
        || lower.ends_with(".csv.bz2")
        || lower.ends_with(".csv.xz")
}

fn decompress_by_name(name: String, reader: Box<dyn Read>) -> io::Result<Box<dyn Read>> {
    let lower = name.to_lowercase();
    if lower.ends_with(".csv.zst") {
        Ok(Box::new(zstd::Decoder::new(reader)?))
    } else if lower.ends_with(".csv.gz") {
        Ok(Box::new(flate2::read::GzDecoder::new(reader)))
    } else if lower.ends_with(".csv.bz2") {
        Ok(Box::new(bzip2::read::BzDecoder::new(reader)))
    } else if lower.ends_with(".csv.xz") {
        Ok(Box::new(xz2::read::XzDecoder::new(reader)))
    } else {
        Ok(reader)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel() -> (ProgressSender, std::sync::mpsc::Receiver<ProgressEvent>) {
        std::sync::mpsc::channel()
    }

    #[test]
    fn merges_directory_of_chunks() {
        let dir = std::env::temp_dir().join(format!("csvs_merge_{}", std::process::id()));
        let chunks = dir.join("chunks");
        std::fs::create_dir_all(&chunks).unwrap();
        std::fs::write(chunks.join("s_0001.csv"), "id,v\n1,a\n2,b\n").unwrap();
        std::fs::write(chunks.join("s_0002.csv"), "id,v\n3,c\n").unwrap();

        let (tx, _rx) = channel();
        let out = dir.join("merged.csv");
        let report = merge_archive(&chunks, &out, &tx).unwrap();

        assert_eq!(report.chunks, 2);
        assert_eq!(
            std::fs::read_to_string(&out).unwrap(),
            "id,v\n1,a\n2,b\n3,c\n"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn keeps_header_of_first_chunk_only() {
        let dir = std::env::temp_dir().join(format!("csvs_merge2_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a_0001.csv"), "h\n1\n").unwrap();
        std::fs::write(dir.join("a_0002.csv"), "h\n2\n").unwrap();

        let (tx, _rx) = channel();
        let out = dir.join("m.csv");
        merge_archive(&dir, &out, &tx).unwrap();
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "h\n1\n2\n");
        std::fs::remove_dir_all(&dir).ok();
    }
}
