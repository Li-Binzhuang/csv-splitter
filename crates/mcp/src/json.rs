/// Minimal JSON value for the MCP envelope. Arguments are small and the protocol
/// needs one object per line, so this avoids a new dependency in the hot CLI.
#[derive(Clone, Debug)]
pub enum McpJson {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    #[allow(dead_code)]
    Array(Vec<McpJson>),
    Object(Vec<(String, McpJson)>),
}

impl McpJson {
    pub fn get(&self, key: &str) -> Option<&McpJson> {
        match self {
            Self::Object(pairs) => pairs
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(value) => Some(value),
            _ => None,
        }
    }
    pub(crate) fn as_u64(&self) -> Option<u64> {
        match self {
            Self::Number(value) if *value >= 0.0 => Some(*value as u64),
            _ => None,
        }
    }
}

pub struct McpParser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> McpParser<'a> {
    pub fn parse(text: &'a str) -> Result<McpJson, String> {
        let mut parser = Self {
            bytes: text.as_bytes(),
            pos: 0,
        };
        let value = parser.value()?;
        parser.whitespace();
        if parser.pos != parser.bytes.len() {
            return Err(format!("trailing input at byte {}", parser.pos));
        }
        Ok(value)
    }
    fn whitespace(&mut self) {
        while self
            .bytes
            .get(self.pos)
            .is_some_and(|byte| byte.is_ascii_whitespace())
        {
            self.pos += 1;
        }
    }
    fn value(&mut self) -> Result<McpJson, String> {
        self.whitespace();
        match self.bytes.get(self.pos).copied() {
            Some(b'n') => {
                self.literal(b"null")?;
                Ok(McpJson::Null)
            }
            Some(b't') => {
                self.literal(b"true")?;
                Ok(McpJson::Bool(true))
            }
            Some(b'f') => {
                self.literal(b"false")?;
                Ok(McpJson::Bool(false))
            }
            Some(b'"') => Ok(McpJson::String(self.string()?)),
            Some(b'[') => {
                self.pos += 1;
                let mut items = Vec::new();
                self.whitespace();
                if self.bytes.get(self.pos) == Some(&b']') {
                    self.pos += 1;
                    return Ok(McpJson::Array(items));
                }
                loop {
                    items.push(self.value()?);
                    self.whitespace();
                    match self.bytes.get(self.pos) {
                        Some(b',') => self.pos += 1,
                        Some(b']') => {
                            self.pos += 1;
                            break;
                        }
                        _ => return Err(format!("expected , or ] at byte {}", self.pos)),
                    }
                }
                Ok(McpJson::Array(items))
            }
            Some(b'{') => {
                self.pos += 1;
                let mut pairs = Vec::new();
                self.whitespace();
                if self.bytes.get(self.pos) == Some(&b'}') {
                    self.pos += 1;
                    return Ok(McpJson::Object(pairs));
                }
                loop {
                    self.whitespace();
                    let key = self.string()?;
                    self.whitespace();
                    if self.bytes.get(self.pos) != Some(&b':') {
                        return Err(format!("expected : at byte {}", self.pos));
                    }
                    self.pos += 1;
                    let value = self.value()?;
                    pairs.push((key, value));
                    self.whitespace();
                    match self.bytes.get(self.pos) {
                        Some(b',') => self.pos += 1,
                        Some(b'}') => {
                            self.pos += 1;
                            break;
                        }
                        _ => return Err(format!("expected , or }} at byte {}", self.pos)),
                    }
                }
                Ok(McpJson::Object(pairs))
            }
            Some(byte) if byte == b'-' || byte.is_ascii_digit() => {
                let start = self.pos;
                if self.bytes[self.pos] == b'-' {
                    self.pos += 1;
                }
                while self.bytes.get(self.pos).is_some_and(|byte| {
                    byte.is_ascii_digit()
                        || *byte == b'.'
                        || *byte == b'e'
                        || *byte == b'E'
                        || *byte == b'-'
                        || *byte == b'+'
                }) {
                    self.pos += 1;
                }
                let raw =
                    std::str::from_utf8(&self.bytes[start..self.pos]).map_err(|e| e.to_string())?;
                raw.parse::<f64>()
                    .map(McpJson::Number)
                    .map_err(|_| format!("bad number {raw:?}"))
            }
            _ => Err(format!("unexpected value at byte {}", self.pos)),
        }
    }
    fn literal(&mut self, word: &[u8]) -> Result<(), String> {
        if self.bytes.get(self.pos..self.pos + word.len()) == Some(word) {
            self.pos += word.len();
            Ok(())
        } else {
            Err(format!(
                "expected {} at byte {}",
                String::from_utf8_lossy(word),
                self.pos
            ))
        }
    }
    fn string(&mut self) -> Result<String, String> {
        if self.bytes.get(self.pos) != Some(&b'"') {
            return Err(format!("expected string at byte {}", self.pos));
        }
        self.pos += 1;
        let mut out: Vec<u8> = Vec::new();
        while let Some(byte) = self.bytes.get(self.pos).copied() {
            self.pos += 1;
            match byte {
                b'"' => {
                    return String::from_utf8(out)
                        .map_err(|_| "invalid UTF-8 in string".to_string())
                }
                b'\\' => {
                    let escape = self
                        .bytes
                        .get(self.pos)
                        .copied()
                        .ok_or("unterminated escape")?;
                    self.pos += 1;
                    let ch = match escape {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{0008}',
                        b'f' => '\u{000c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => {
                            let hex = self
                                .bytes
                                .get(self.pos..self.pos + 4)
                                .ok_or("short unicode escape")?;
                            self.pos += 4;
                            let code =
                                u16::from_str_radix(std::str::from_utf8(hex).unwrap_or(""), 16)
                                    .map_err(|_| "bad unicode escape")?;
                            char::from_u32(code as u32).unwrap_or('\u{fffd}')
                        }
                        _ => return Err(format!("bad escape \\{escape}")),
                    };
                    out.extend_from_slice(ch.to_string().as_bytes());
                }
                _ => out.push(byte),
            }
        }
        Err("unterminated string".into())
    }
}
