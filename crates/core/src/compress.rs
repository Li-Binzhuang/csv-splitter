use crate::types::CompressFormat;
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::Path;

/// Create a compressing writer for the given format.
///
/// `buf_size` is the *per-chunk* buffer the caller has budgeted for, not a
/// hard-coded constant: the previous version gave every chunk 64 MB, so a
/// 10-way split allocated ~640 MB of write buffers before doing any work.
///
/// `Zip` is not handled here — a zip file has to be written as one archive, so
/// the splitter writes raw CSV chunks and packs them afterwards.
pub fn make_compressor(
    path: &Path,
    format: CompressFormat,
    level: i32,
    buf_size: usize,
) -> io::Result<Box<dyn Write + Send>> {
    let capacity = buf_size.clamp(64 * 1024, 8 * 1024 * 1024);
    let file = BufWriter::with_capacity(capacity, File::create(path)?);
    match format {
        CompressFormat::Zstd => {
            let enc = zstd::Encoder::new(file, level).map_err(io::Error::other)?;
            Ok(Box::new(ZstdFinisher(Some(enc))))
        }
        CompressFormat::Gzip => {
            let enc = flate2::write::GzEncoder::new(file, flate2::Compression::new(level as u32));
            Ok(Box::new(enc))
        }
        CompressFormat::Bzip2 => {
            let enc = bzip2::write::BzEncoder::new(file, bzip2::Compression::new(level as u32));
            Ok(Box::new(enc))
        }
        CompressFormat::Xz => {
            let enc = xz2::write::XzEncoder::new(file, level as u32);
            Ok(Box::new(enc))
        }
        CompressFormat::Zip | CompressFormat::None => Ok(Box::new(file)),
    }
}

/// Wrapper to ensure the zstd encoder is finished (final frame flushed) on drop.
struct ZstdFinisher(Option<zstd::Encoder<'static, BufWriter<File>>>);

impl Write for ZstdFinisher {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self.0.as_mut() {
            Some(enc) => enc.write(buf),
            None => Ok(buf.len()),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self.0.as_mut() {
            Some(enc) => enc.flush(),
            None => Ok(()),
        }
    }
}

impl Drop for ZstdFinisher {
    fn drop(&mut self) {
        if let Some(enc) = self.0.take() {
            enc.finish().ok();
        }
    }
}

/// Finish the compressor (important for zstd to flush its final frame).
pub fn finish_compressor(mut writer: Box<dyn Write + Send>) -> io::Result<()> {
    writer.flush()?;
    drop(writer);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_readable_zstd_frame() {
        let dir = std::env::temp_dir().join(format!("csvs_zstd_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("x.zst");

        let mut w = make_compressor(&path, CompressFormat::Zstd, 3, 64 * 1024).unwrap();
        w.write_all(b"hello csv\n").unwrap();
        finish_compressor(w).unwrap();

        let text = zstd::decode_all(File::open(&path).unwrap()).unwrap();
        assert_eq!(text, b"hello csv\n");
        std::fs::remove_dir_all(&dir).ok();
    }
}
