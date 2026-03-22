use crate::types::CompressFormat;
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::Path;

const BUF_SIZE: usize = 64 * 1024 * 1024;

/// Create a compressing writer for the given format.
/// Returns a boxed Write that compresses on the fly.
pub fn make_compressor(
    path: &Path,
    format: CompressFormat,
    level: i32,
) -> io::Result<Box<dyn Write + Send>> {
    let file = BufWriter::with_capacity(BUF_SIZE, File::create(path)?);
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
        CompressFormat::Zip | CompressFormat::None => {
            // For Zip we handle it at the archive level; None = raw file
            Ok(Box::new(file))
        }
    }
}

/// Wrapper to ensure zstd encoder is finished on drop.
struct ZstdFinisher(Option<zstd::Encoder<'static, BufWriter<File>>>);

impl Write for ZstdFinisher {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.as_mut().unwrap().write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.as_mut().unwrap().flush()
    }
}

impl Drop for ZstdFinisher {
    fn drop(&mut self) {
        if let Some(enc) = self.0.take() {
            enc.finish().ok();
        }
    }
}

/// Finish the compressor (important for zstd to flush final frame).
pub fn finish_compressor(mut writer: Box<dyn Write + Send>) -> io::Result<()> {
    writer.flush()?;
    drop(writer);
    Ok(())
}
