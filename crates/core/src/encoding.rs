//! Text encoding detection and decoding.
//!
//! CSVs exported from Chinese Windows tools are usually GBK, and Excel likes to
//! prepend a UTF-8 BOM. Reading either as UTF-8 produces mojibake in every cell,
//! which is worse than failing. This module sniffs the encoding once, then hands
//! out readers that always yield UTF-8 so the rest of the pipeline stays simple.
//!
//! Byte structure is still ASCII for UTF-8 and GBK (`0x2C` and `0x0A` never appear
//! as GBK trail bytes), so the sparse index keeps working on raw bytes for those
//! two. UTF-16 is different — its separators are two bytes — and is rejected with
//! an actionable message instead of silently producing garbage.

use encoding_rs::{Encoding, GBK, UTF_16BE, UTF_16LE, UTF_8};
use encoding_rs_io::DecodeReaderBytesBuilder;
use std::fs::File;
use std::io::{self, BufReader, Read};
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextEncoding {
    /// UTF-8, with or without a BOM.
    Utf8 {
        bom: bool,
    },
    Utf16Le,
    Utf16Be,
    /// GBK / GB18030 family (the usual "ANSI" export on Chinese Windows).
    Gbk,
}

impl TextEncoding {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Utf8 { bom: true } => "utf-8-bom",
            Self::Utf8 { bom: false } => "utf-8",
            Self::Utf16Le => "utf-16le",
            Self::Utf16Be => "utf-16be",
            Self::Gbk => "gbk",
        }
    }

    pub fn encoding(&self) -> &'static Encoding {
        match self {
            Self::Utf8 { .. } => UTF_8,
            Self::Utf16Le => UTF_16LE,
            Self::Utf16Be => UTF_16BE,
            Self::Gbk => GBK,
        }
    }

    /// True when ASCII bytes (`,` `\n` `"`) keep their meaning in the raw stream,
    /// i.e. the sparse index can seek by byte offset.
    pub fn is_ascii_compatible(&self) -> bool {
        matches!(self, Self::Utf8 { .. } | Self::Gbk)
    }
}

/// Sniff the encoding from the first bytes of a file.
pub fn detect(path: &Path) -> io::Result<TextEncoding> {
    let mut file = File::open(path)?;
    let mut sample = vec![0u8; 64 * 1024];
    let n = file.read(&mut sample)?;
    sample.truncate(n);
    Ok(detect_bytes(&sample))
}

pub fn detect_bytes(sample: &[u8]) -> TextEncoding {
    if sample.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return TextEncoding::Utf8 { bom: true };
    }
    if sample.starts_with(&[0xFF, 0xFE]) {
        return TextEncoding::Utf16Le;
    }
    if sample.starts_with(&[0xFE, 0xFF]) {
        return TextEncoding::Utf16Be;
    }
    // UTF-16 without BOM: NULs interleaved with ASCII is the giveaway.
    if sample.len() >= 4 {
        let even_nul = sample.iter().step_by(2).filter(|b| **b == 0).count();
        let odd_nul = sample
            .iter()
            .skip(1)
            .step_by(2)
            .filter(|b| **b == 0)
            .count();
        let half = sample.len() / 2;
        if odd_nul > half * 3 / 4 && even_nul < half / 4 {
            return TextEncoding::Utf16Le;
        }
        if even_nul > half * 3 / 4 && odd_nul < half / 4 {
            return TextEncoding::Utf16Be;
        }
    }
    match std::str::from_utf8(sample) {
        Ok(_) => TextEncoding::Utf8 { bom: false },
        // Not valid UTF-8: for CSV the overwhelmingly likely alternative is the
        // Windows Chinese code page. GBK decoding never fails, it just yields
        // replacement characters for genuinely broken input.
        Err(_) => TextEncoding::Gbk,
    }
}

/// Open a file as a UTF-8 byte stream, decoding and stripping any BOM.
pub fn open_decoded(path: &Path) -> io::Result<Box<dyn Read + Send>> {
    let encoding = detect(path)?;
    Ok(decoded_reader(File::open(path)?, encoding))
}

/// Wrap any reader so it yields UTF-8, decoding according to `encoding`.
pub fn decoded_reader<R: Read + Send + 'static>(
    reader: R,
    encoding: TextEncoding,
) -> Box<dyn Read + Send> {
    Box::new(
        DecodeReaderBytesBuilder::new()
            .encoding(Some(encoding.encoding()))
            .bom_sniffing(true)
            .utf8_passthru(true)
            .build(reader),
    )
}

/// Decode a single cell/record that came from the raw byte stream.
///
/// UTF-8 is passed through untouched (the common case); GBK is converted here
/// because the index works on raw bytes.
pub fn decode_cell(bytes: &[u8], encoding: TextEncoding) -> String {
    match encoding {
        TextEncoding::Utf8 { .. } => String::from_utf8_lossy(bytes).into_owned(),
        TextEncoding::Gbk => {
            let (cow, _, _) = GBK.decode(bytes);
            cow.into_owned()
        }
        TextEncoding::Utf16Le | TextEncoding::Utf16Be => {
            let enc = encoding.encoding();
            let (cow, _, _) = enc.decode(bytes);
            cow.into_owned()
        }
    }
}

/// Decode a whole record (used by paths that already hold the bytes).
pub fn decode_record(bytes: &[u8], encoding: TextEncoding) -> String {
    decode_cell(bytes, encoding)
}

/// Convenience for small files (tests, configs).
pub fn read_to_string(path: &Path) -> io::Result<String> {
    let mut reader = BufReader::new(open_decoded(path)?);
    let mut out = Vec::new();
    reader.read_to_end(&mut out)?;
    Ok(String::from_utf8_lossy(&out).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "csvs_enc_{tag}_{}_{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
        fn join(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    #[test]
    fn detects_bom_variants() {
        assert_eq!(
            detect_bytes(b"\xEF\xBB\xBFid,name"),
            TextEncoding::Utf8 { bom: true }
        );
        assert_eq!(detect_bytes(b"\xFF\xFEi\x00d\x00"), TextEncoding::Utf16Le);
        assert_eq!(detect_bytes(b"\xFE\xFF\x00i\x00d"), TextEncoding::Utf16Be);
        assert_eq!(detect_bytes(b"id,name"), TextEncoding::Utf8 { bom: false });
    }

    #[test]
    fn detects_gbk_when_not_utf8() {
        // "城市" in GBK
        assert_eq!(detect_bytes(&[0xB3, 0xC7, 0xCA, 0xD0]), TextEncoding::Gbk);
    }

    #[test]
    fn decodes_gbk_cells() {
        let bytes = [0xB3, 0xC7, 0xCA, 0xD0]; // 城市
        assert_eq!(decode_cell(&bytes, TextEncoding::Gbk), "城市");
    }

    #[test]
    fn reads_gbk_file_as_utf8() {
        let dir = TempDir::new("gbk");
        let path = dir.join("gbk.csv");
        // header: 城市,人口  + one row
        let mut bytes = Vec::new();
        for ch in "城市,人口\n北京,2189\n".chars() {
            let text = ch.to_string();
            let (encoded, _, _) = GBK.encode(&text);
            bytes.extend_from_slice(&encoded);
        }
        std::fs::write(&path, &bytes).unwrap();

        let text = read_to_string(&path).unwrap();
        assert_eq!(detect(&path).unwrap(), TextEncoding::Gbk);
        assert_eq!(text, "城市,人口\n北京,2189\n");
    }

    #[test]
    fn strips_utf8_bom() {
        let dir = TempDir::new("bom");
        let path = dir.join("bom.csv");
        std::fs::write(&path, b"\xEF\xBB\xBFid,name\n1,a\n").unwrap();
        assert_eq!(read_to_string(&path).unwrap(), "id,name\n1,a\n");
    }
}
