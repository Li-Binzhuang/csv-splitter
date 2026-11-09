use crate::types::{ProgressEvent, ProgressSender};
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter};
use std::path::{Path, PathBuf};

/// Pack chunk files into a container. Tar keeps the file names; zip also
/// compresses, so callers usually pass raw `.csv` chunks for zip and compressed
/// chunks for tar.
pub fn pack(
    files: &[PathBuf],
    output: &Path,
    zip: bool,
    buf_size: usize,
    tx: &ProgressSender,
) -> io::Result<()> {
    if zip {
        pack_zip(files, output, buf_size)
    } else {
        pack_tar(files, output, buf_size, tx)
    }
}

pub fn pack_tar(
    files: &[PathBuf],
    output: &Path,
    buf_size: usize,
    tx: &ProgressSender,
) -> io::Result<()> {
    let io_buf = buf_size.clamp(256 * 1024, 8 * 1024 * 1024);
    let tar_file = BufWriter::with_capacity(io_buf, File::create(output)?);
    let mut archive = tar::Builder::new(tar_file);
    let mut bytes_done: u64 = 0;

    for path in files {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "chunk.csv".to_string());
        let fsize = fs::metadata(path)?.len();

        let mut header = tar::Header::new_gnu();
        header.set_size(fsize);
        header.set_mode(0o644);
        header.set_cksum();

        let r = BufReader::with_capacity(io_buf, File::open(path)?);
        archive.append_data(&mut header, &name, r)?;

        bytes_done += fsize;
        tx.send(ProgressEvent::Progress { bytes: bytes_done }).ok();
    }

    archive.finish()?;
    Ok(())
}

/// Pack the already-written CSV chunks into a single real zip archive.
pub fn pack_zip(files: &[PathBuf], output: &Path, buf_size: usize) -> io::Result<()> {
    use zip::write::SimpleFileOptions;
    use zip::{CompressionMethod, ZipWriter};

    let io_buf = buf_size.clamp(256 * 1024, 8 * 1024 * 1024);
    let file = File::create(output)?;
    let mut zip = ZipWriter::new(BufWriter::with_capacity(io_buf, file));
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .large_file(true);

    for path in files {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "chunk.csv".to_string());
        zip.start_file(name, options)
            .map_err(|e| io::Error::other(e.to_string()))?;
        let mut f = BufReader::with_capacity(io_buf, File::open(path)?);
        io::copy(&mut f, &mut zip)?;
    }
    zip.finish().map_err(|e| io::Error::other(e.to_string()))?;
    Ok(())
}
