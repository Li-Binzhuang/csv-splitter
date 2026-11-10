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

use encoding_rs::{Encoding, GBK, UTF_16BE, UTF_16LE, UTF_8, WINDOWS_1252};
use encoding_rs_io::DecodeReaderBytesBuilder;
use std::fs::File;
use std::io::{self, BufReader, Read, Write};
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
    /// Windows-1252 / Latin-1: the usual "ANSI" export on Western Windows. It
    /// never fails to decode, which is what makes it a safe fallback.
    Latin1,
}

impl TextEncoding {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Utf8 { bom: true } => "utf-8-bom",
            Self::Utf8 { bom: false } => "utf-8",
            Self::Utf16Le => "utf-16le",
            Self::Utf16Be => "utf-16be",
            Self::Gbk => "gbk",
            Self::Latin1 => "windows-1252",
        }
    }

    pub fn encoding(&self) -> &'static Encoding {
        match self {
            Self::Utf8 { .. } => UTF_8,
            Self::Utf16Le => UTF_16LE,
            Self::Utf16Be => UTF_16BE,
            Self::Gbk => GBK,
            Self::Latin1 => WINDOWS_1252,
        }
    }

    /// True when ASCII bytes (`,` `\n` `"`) keep their meaning in the raw stream,
    /// i.e. the sparse index can seek by byte offset.
    pub fn is_ascii_compatible(&self) -> bool {
        matches!(self, Self::Utf8 { .. } | Self::Gbk | Self::Latin1)
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
    if std::str::from_utf8(sample).is_ok() {
        return TextEncoding::Utf8 { bom: false };
    }
    // Not UTF-8. Two candidates deserve a look: the Chinese code page (the usual
    // "ANSI" export on Chinese Windows) and Windows-1252 (the usual one
    // elsewhere). Windows-1252 never fails to decode, so it cannot be chosen by
    // "does it work"; instead GBK wins only when it decodes cleanly into CJK.
    let (gbk, _, _) = GBK.decode(sample);
    let broken = gbk.contains('\u{FFFD}');
    let cjk = gbk
        .chars()
        .filter(|c| matches!(u32::from(*c), 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF))
        .count();
    if !broken && cjk > 0 {
        TextEncoding::Gbk
    } else {
        TextEncoding::Latin1
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
        TextEncoding::Latin1 => {
            let (cow, _, _) = WINDOWS_1252.decode(bytes);
            cow.into_owned()
        }
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

/// Parse an encoding name coming from a command line / query string.
///
/// `auto` returns `Ok(None)` so the caller keeps sniffing.
pub fn parse_name(name: &str) -> Option<TextEncoding> {
    match name.trim().to_ascii_lowercase().as_str() {
        "auto" | "" => None,
        "utf-8" | "utf8" => Some(TextEncoding::Utf8 { bom: false }),
        "utf-8-bom" | "utf8-bom" | "bom" => Some(TextEncoding::Utf8 { bom: true }),
        "gbk" | "gb18030" | "ansi" => Some(TextEncoding::Gbk),
        "utf-16le" | "utf16le" => Some(TextEncoding::Utf16Le),
        "utf-16be" | "utf16be" => Some(TextEncoding::Utf16Be),
        _ => None,
    }
}

/// Stream `input` into `output` as UTF-8 (optionally with a BOM).
///
/// This is the escape hatch for files the index cannot handle directly: a
/// UTF-16 export becomes a normal UTF-8 CSV, after which random access works.
/// Constant memory — it never holds more than one read buffer.
pub fn transcode(
    input: &Path,
    output: &Path,
    from: Option<TextEncoding>,
    to: TextEncoding,
    buf: usize,
) -> io::Result<u64> {
    let encoding = match from {
        Some(e) => e,
        None => detect(input)?,
    };
    let written = match to {
        TextEncoding::Utf8 { bom } => {
            let mut src = decoded_reader(File::open(input)?, encoding);
            if let Some(parent) = output.parent() {
                if !parent.as_os_str().is_empty() {
                    std::fs::create_dir_all(parent)?;
                }
            }
            let mut out = io::BufWriter::with_capacity(1024 * 1024, File::create(output)?);
            if bom {
                out.write_all(&[0xEF, 0xBB, 0xBF])?;
            }
            let mut buf = vec![0u8; buf.clamp(64 * 1024, 8 * 1024 * 1024)];
            let mut total = 0u64;
            loop {
                let n = src.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                out.write_all(&buf[..n])?;
                total += n as u64;
            }
            out.flush()?;
            total
        }
        TextEncoding::Gbk | TextEncoding::Latin1 => {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "writing legacy 8-bit encodings is not supported; convert to UTF-8 instead",
            ))
        }
        TextEncoding::Utf16Le | TextEncoding::Utf16Be => {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "writing UTF-16 is not supported; convert to UTF-8 instead",
            ))
        }
    };
    Ok(written)
}

/// Does this file start with a UTF-8 BOM?
///
/// Excel reads a BOM-less UTF-8 file as the system code page, which turns Chinese text
/// into mojibake. Preserving the input's BOM on output is the difference between "opens
/// correctly in Excel" and "looks broken".
pub fn starts_with_bom(path: &Path) -> bool {
    use std::io::Read;
    let Ok(mut file) = File::open(path) else {
        return false;
    };
    let mut head = [0u8; 3];
    match file.read_exact(&mut head) {
        Ok(()) => head == [0xEF, 0xBB, 0xBF],
        Err(_) => false,
    }
}

/// Write a BOM when the source had one, so the output opens in Excel the way the
/// input did.
pub fn keep_bom(out: &mut impl std::io::Write, source: &Path) -> std::io::Result<()> {
    if starts_with_bom(source) {
        out.write_all(&[0xEF, 0xBB, 0xBF])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Per-process unique suffix for temp directories.
    ///
    /// `SystemTime::now()` only has microsecond resolution on macOS, so a dozen
    /// tests starting at once can read the same value and share a directory — one
    /// test's cleanup then deletes the other's files mid-run.
    fn unique_seq() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        SEQ.fetch_add(1, Ordering::Relaxed)
    }

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
                    + unique_seq() as u128
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
    fn windows_1252_is_chosen_when_gbk_would_produce_garbage() {
        // "café, résumé": 0xE9 cannot start a GBK sequence, so GBK decoding
        // yields replacement characters and the file would render as junk.
        let bytes = b"id,caf\xe9\n1,r\xe9sum\xe9\n";
        assert_eq!(detect_bytes(bytes), TextEncoding::Latin1);
        assert_eq!(decode_cell(b"caf\xe9", TextEncoding::Latin1), "café");
    }

    #[test]
    fn gbk_still_wins_for_real_chinese_text() {
        let mut bytes = b"id,city\n1,".to_vec();
        bytes.extend_from_slice(&[0xB1, 0xB1, 0xBE, 0xA9]); // 北京
        bytes.push(b'\n');
        assert_eq!(detect_bytes(&bytes), TextEncoding::Gbk);
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
    fn transcodes_gbk_to_utf8() {
        let dir = TempDir::new("tc");
        let src = dir.join("gbk.csv");
        let dst = dir.join("utf8.csv");
        let text = "城市,人口\n北京,2189\n";
        let mut bytes = Vec::new();
        for ch in text.chars() {
            let one = ch.to_string();
            let (encoded, _, _) = GBK.encode(&one);
            bytes.extend_from_slice(&encoded);
        }
        std::fs::write(&src, bytes).unwrap();

        let written = transcode(
            &src,
            &dst,
            None,
            TextEncoding::Utf8 { bom: false },
            64 * 1024,
        )
        .unwrap();
        assert_eq!(written as usize, text.len());
        assert_eq!(std::fs::read_to_string(&dst).unwrap(), text);
    }

    #[test]
    fn transcode_can_emit_bom_and_utf16_input_works() {
        let dir = TempDir::new("tc16");
        let src = dir.join("u16.csv");
        let dst = dir.join("u8.csv");
        let text = "id,name\n1,北京\n";
        // UTF-16LE with BOM
        let mut bytes = vec![0xFF, 0xFE];
        for unit in text.encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        std::fs::write(&src, bytes).unwrap();
        assert_eq!(detect(&src).unwrap(), TextEncoding::Utf16Le);

        transcode(
            &src,
            &dst,
            None,
            TextEncoding::Utf8 { bom: true },
            64 * 1024,
        )
        .unwrap();
        let out = std::fs::read(&dst).unwrap();
        assert_eq!(&out[..3], &[0xEF, 0xBB, 0xBF]);
        assert_eq!(String::from_utf8_lossy(&out[3..]), text);
    }

    #[test]
    fn parses_encoding_names() {
        assert_eq!(parse_name("auto"), None);
        assert_eq!(parse_name("GBK"), Some(TextEncoding::Gbk));
        assert_eq!(
            parse_name("utf-8-bom"),
            Some(TextEncoding::Utf8 { bom: true })
        );
        assert_eq!(parse_name("nonsense"), None);
    }

    #[test]
    fn strips_utf8_bom() {
        let dir = TempDir::new("bom");
        let path = dir.join("bom.csv");
        std::fs::write(&path, b"\xEF\xBB\xBFid,name\n1,a\n").unwrap();
        assert_eq!(read_to_string(&path).unwrap(), "id,name\n1,a\n");
    }
}
