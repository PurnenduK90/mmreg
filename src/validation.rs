use crate::error::MmregError;

/// One register assertion of a `SafetyProfile`: `(read(addr) & mask) == (expected & mask)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidationCheck {
    /// Human-readable name of the check (e.g. `"PCAP_DONE"`)
    pub name: String,
    /// Physical address of the status/control register to read
    pub addr: u64,
    /// Bits of the read value that are compared
    pub mask: u32,
    /// Expected value of the masked bits
    pub expected: u32,
}

/// Prerequisite register checks run before a peripheral is mapped (`--validate <profile.json>`).
///
/// Profile files need the `profile` feature (on by default). Numbers are JSON integers or
/// strings (`"0xFF41_0000"`, `"0d42"`, decimal otherwise):
/// ```json
/// {
///   "name": "ZynqMP_AXI_Safety",
///   "checks": [
///     {
///       "name": "PCAP_DONE",
///       "addr": "0xFFCA_3010",
///       "mask": "0x4",
///       "expected": "0x4"
///     }
///   ]
/// }
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SafetyProfile {
    /// Profile name
    pub name: String,
    /// Checks, run in order; the first failure stops the run
    pub checks: Vec<ValidationCheck>,
}

impl SafetyProfile {
    /// Loads a profile from a JSON file.
    #[cfg(feature = "profile")]
    pub fn from_file(path: &std::path::Path) -> Result<Self, MmregError> {
        let content =
            std::fs::read_to_string(path).map_err(|e| MmregError::ValidationProfileParse {
                details: format!("failed to read '{}': {}", path.display(), e),
            })?;
        Self::parse(&content)
    }

    /// Parses a profile from JSON. Unknown keys (except `x-*` extensions) and an empty `checks`
    /// list are errors.
    #[cfg(feature = "profile")]
    pub fn parse(json_str: &str) -> Result<Self, MmregError> {
        json::parse_profile(json_str)
            .map_err(|details| MmregError::ValidationProfileParse { details })
    }

    /// Runs every check with `read_fn` as the register reader.
    pub fn execute_with<F>(&self, mut read_fn: F) -> Result<(), MmregError>
    where
        F: FnMut(u64) -> Result<u32, MmregError>,
    {
        for check in &self.checks {
            let actual = read_fn(check.addr)?;
            if actual & check.mask != check.expected & check.mask {
                return Err(MmregError::ValidationHookFailed {
                    profile_name: self.name.clone(),
                    check_name: check.name.clone(),
                    address: check.addr,
                    mask: check.mask,
                    expected: check.expected,
                    actual,
                });
            }
        }
        Ok(())
    }
}

#[cfg(feature = "profile")]
mod json {
    use super::{SafetyProfile, ValidationCheck};
    use crate::parse::parse_u64_radix;

    struct Parser<'a> {
        text: &'a str,
        src: &'a [u8],
        pos: usize,
    }

    impl<'a> Parser<'a> {
        fn new(input: &'a str) -> Self {
            let text = input.strip_prefix('\u{FEFF}').unwrap_or(input);
            Parser {
                text,
                src: text.as_bytes(),
                pos: 0,
            }
        }

        fn skip_ws(&mut self) {
            while let Some(&b) = self.src.get(self.pos) {
                if matches!(b, b' ' | b'\t' | b'\n' | b'\r') {
                    self.pos += 1;
                } else {
                    break;
                }
            }
        }

        fn peek(&mut self) -> Option<u8> {
            self.skip_ws();
            self.src.get(self.pos).copied()
        }

        fn expect(&mut self, expected: u8) -> Result<(), String> {
            match self.peek() {
                Some(b) if b == expected => {
                    self.pos += 1;
                    Ok(())
                }
                Some(b) => Err(format!(
                    "expected '{}' at byte {}, found '{}'",
                    expected as char, self.pos, b as char
                )),
                None => Err(format!(
                    "unexpected end of JSON, expected '{}'",
                    expected as char
                )),
            }
        }

        fn parse_string(&mut self) -> Result<String, String> {
            self.expect(b'"')?;
            let mut out = String::new();
            while let Some(&b) = self.src.get(self.pos) {
                self.pos += 1;
                match b {
                    b'"' => return Ok(out),
                    b'\\' => {
                        let esc = *self
                            .src
                            .get(self.pos)
                            .ok_or_else(|| "unterminated escape".to_string())?;
                        self.pos += 1;
                        match esc {
                            b'"' => out.push('"'),
                            b'\\' => out.push('\\'),
                            b'/' => out.push('/'),
                            b'b' => out.push('\u{0008}'),
                            b'f' => out.push('\u{000C}'),
                            b'n' => out.push('\n'),
                            b'r' => out.push('\r'),
                            b't' => out.push('\t'),
                            b'u' => {
                                let code = self.parse_hex4()?;
                                let ch = if (0xD800..=0xDBFF).contains(&code) {
                                    if self.src.get(self.pos..self.pos + 2) != Some(b"\\u") {
                                        return Err("unpaired UTF-16 high surrogate".to_string());
                                    }
                                    self.pos += 2;
                                    let low = self.parse_hex4()?;
                                    if !(0xDC00..=0xDFFF).contains(&low) {
                                        return Err("invalid UTF-16 low surrogate".to_string());
                                    }
                                    let combined =
                                        0x1_0000 + (((code - 0xD800) << 10) | (low - 0xDC00));
                                    char::from_u32(combined).unwrap()
                                } else {
                                    char::from_u32(code).ok_or_else(|| {
                                        format!("invalid unicode scalar \\u{code:04X}")
                                    })?
                                };
                                out.push(ch);
                            }
                            other => return Err(format!("invalid escape '\\{}'", other as char)),
                        }
                    }
                    0..=0x1F => return Err("unescaped control character in string".to_string()),
                    _ => {
                        let ch = self.text[self.pos - 1..].chars().next().unwrap();
                        self.pos += ch.len_utf8() - 1;
                        out.push(ch);
                    }
                }
            }
            Err("unterminated string".to_string())
        }

        fn parse_hex4(&mut self) -> Result<u32, String> {
            let bytes = self
                .src
                .get(self.pos..self.pos + 4)
                .ok_or_else(|| "short \\u escape".to_string())?;
            if !bytes.iter().all(u8::is_ascii_hexdigit) {
                return Err("invalid \\u escape".to_string());
            }
            let hex = std::str::from_utf8(bytes).unwrap();
            self.pos += 4;
            u32::from_str_radix(hex, 16).map_err(|_| format!("invalid \\u{hex} escape"))
        }

        fn parse_u64(&mut self) -> Result<u64, String> {
            match self.peek() {
                Some(b'"') => {
                    let s = self.parse_string()?;
                    parse_u64_radix(&s, 10)
                }
                Some(b'0'..=b'9') => {
                    let start = self.pos;
                    while self.src.get(self.pos).is_some_and(|b| {
                        b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'+' | b'-')
                    }) {
                        self.pos += 1;
                    }
                    let token = std::str::from_utf8(&self.src[start..self.pos])
                        .map_err(|_| "invalid number".to_string())?;
                    if token.len() > 1 && token.starts_with('0') {
                        return Err(format!("invalid JSON number '{token}' (quote hex strings)"));
                    }
                    token
                        .parse::<u64>()
                        .map_err(|_| format!("invalid integer '{token}'"))
                }
                _ => Err(format!(
                    "expected integer or number string at byte {}",
                    self.pos
                )),
            }
        }

        fn parse_u32(&mut self) -> Result<u32, String> {
            let v = self.parse_u64()?;
            u32::try_from(v).map_err(|_| format!("0x{v:X} does not fit in 32 bits"))
        }

        fn skip_value(&mut self, depth: u8) -> Result<(), String> {
            if depth > 32 {
                return Err("JSON nesting too deep".to_string());
            }
            match self
                .peek()
                .ok_or_else(|| "unexpected end of JSON".to_string())?
            {
                b'"' => {
                    self.parse_string()?;
                    Ok(())
                }
                b'{' => self.parse_object(|_, p| p.skip_value(depth + 1)),
                b'[' => {
                    self.expect(b'[')?;
                    if self.peek() == Some(b']') {
                        self.pos += 1;
                        return Ok(());
                    }
                    loop {
                        self.skip_value(depth + 1)?;
                        match self.peek() {
                            Some(b',') => self.pos += 1,
                            Some(b']') => {
                                self.pos += 1;
                                return Ok(());
                            }
                            _ => return Err("expected ',' or ']' in array".to_string()),
                        }
                    }
                }
                b't' if self.src[self.pos..].starts_with(b"true") => {
                    self.pos += 4;
                    Ok(())
                }
                b'f' if self.src[self.pos..].starts_with(b"false") => {
                    self.pos += 5;
                    Ok(())
                }
                b'n' if self.src[self.pos..].starts_with(b"null") => {
                    self.pos += 4;
                    Ok(())
                }
                b'-' | b'0'..=b'9' => self.skip_number(),
                b => Err(format!("unexpected '{}' at byte {}", b as char, self.pos)),
            }
        }

        fn skip_number(&mut self) -> Result<(), String> {
            if self.src.get(self.pos) == Some(&b'-') {
                self.pos += 1;
            }
            match self.src.get(self.pos) {
                Some(b'0') => self.pos += 1,
                Some(b'1'..=b'9') => {
                    while self.src.get(self.pos).is_some_and(u8::is_ascii_digit) {
                        self.pos += 1;
                    }
                }
                _ => return Err("invalid number in ignored field".to_string()),
            }
            if self.src.get(self.pos) == Some(&b'.') {
                self.pos += 1;
                if !self.src.get(self.pos).is_some_and(u8::is_ascii_digit) {
                    return Err("invalid fraction in ignored number".to_string());
                }
                while self.src.get(self.pos).is_some_and(u8::is_ascii_digit) {
                    self.pos += 1;
                }
            }
            if matches!(self.src.get(self.pos), Some(b'e' | b'E')) {
                self.pos += 1;
                if matches!(self.src.get(self.pos), Some(b'+' | b'-')) {
                    self.pos += 1;
                }
                if !self.src.get(self.pos).is_some_and(u8::is_ascii_digit) {
                    return Err("invalid exponent in ignored number".to_string());
                }
                while self.src.get(self.pos).is_some_and(u8::is_ascii_digit) {
                    self.pos += 1;
                }
            }
            Ok(())
        }

        fn parse_object(
            &mut self,
            mut on_field: impl FnMut(String, &mut Self) -> Result<(), String>,
        ) -> Result<(), String> {
            self.expect(b'{')?;
            if self.peek() == Some(b'}') {
                self.pos += 1;
                return Ok(());
            }
            loop {
                let key = self.parse_string()?;
                self.expect(b':')?;
                on_field(key, self)?;
                match self.peek() {
                    Some(b',') => self.pos += 1,
                    Some(b'}') => {
                        self.pos += 1;
                        return Ok(());
                    }
                    _ => return Err(format!("expected ',' or '}}' at byte {}", self.pos)),
                }
            }
        }

        fn parse_check(&mut self) -> Result<ValidationCheck, String> {
            let mut name = None;
            let mut addr = None;
            let mut mask = None;
            let mut expected = None;

            self.parse_object(|key, p| {
                match key.as_str() {
                    "name" if name.is_none() => name = Some(p.parse_string()?),
                    "addr" if addr.is_none() => addr = Some(p.parse_u64()?),
                    "mask" if mask.is_none() => mask = Some(p.parse_u32()?),
                    "expected" if expected.is_none() => expected = Some(p.parse_u32()?),
                    k if k.starts_with("x-") => p.skip_value(0)?,
                    "name" | "addr" | "mask" | "expected" => {
                        return Err(format!("duplicate key '{key}' in check"));
                    }
                    other => return Err(format!("unknown key '{other}' in check")),
                }
                Ok(())
            })?;

            Ok(ValidationCheck {
                name: name.ok_or_else(|| "missing 'name' in check".to_string())?,
                addr: addr.ok_or_else(|| "missing 'addr' in check".to_string())?,
                mask: mask.ok_or_else(|| "missing 'mask' in check".to_string())?,
                expected: expected.ok_or_else(|| "missing 'expected' in check".to_string())?,
            })
        }
    }

    pub(super) fn parse_profile(input: &str) -> Result<SafetyProfile, String> {
        let mut parser = Parser::new(input);
        let mut name = None;
        let mut checks = None;

        parser.parse_object(|key, p| {
            match key.as_str() {
                "name" if name.is_none() => name = Some(p.parse_string()?),
                "checks" if checks.is_none() => {
                    p.expect(b'[')?;
                    let mut list = Vec::new();
                    if p.peek() == Some(b']') {
                        p.pos += 1;
                    } else {
                        loop {
                            list.push(p.parse_check()?);
                            match p.peek() {
                                Some(b',') => p.pos += 1,
                                Some(b']') => {
                                    p.pos += 1;
                                    break;
                                }
                                _ => return Err("expected ',' or ']' in 'checks'".to_string()),
                            }
                        }
                    }
                    checks = Some(list);
                }
                k if k.starts_with("x-") => p.skip_value(0)?,
                "name" | "checks" => return Err(format!("duplicate key '{key}'")),
                other => return Err(format!("unknown key '{other}'")),
            }
            Ok(())
        })?;

        if parser.peek().is_some() {
            return Err(format!("trailing characters at byte {}", parser.pos));
        }
        let checks = checks.ok_or_else(|| "missing 'checks' array".to_string())?;
        if checks.is_empty() {
            return Err("no 'checks' entries".to_string());
        }
        Ok(SafetyProfile {
            name: name.ok_or_else(|| "missing 'name'".to_string())?,
            checks,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_execute_checks() {
        let profile = SafetyProfile {
            name: "ZynqMP".into(),
            checks: vec![ValidationCheck {
                name: "PCAP_DONE".into(),
                addr: 0xFFCA_3010,
                mask: 0x4,
                expected: 0x4,
            }],
        };
        assert!(profile.execute_with(|_| Ok(0xFFFF_FFF4)).is_ok());
        let err = profile.execute_with(|_| Ok(0)).unwrap_err();
        assert_eq!(err.code(), "E_VALIDATION_HOOK_FAILED");
        assert!(err.to_string().contains("PCAP_DONE"));
        let err = profile
            .execute_with(|address| Err(MmregError::BlueprintUnmapped { address, size: 4 }))
            .unwrap_err();
        assert_eq!(err.code(), "E_BLUEPRINT_UNMAPPED");
    }

    #[cfg(feature = "profile")]
    #[test]
    fn test_parse_json_profile() {
        let profile = SafetyProfile::parse(
            "\u{FEFF}{\n  \
  \"name\": \"ZynqMP_AXI_Safety #1\",\n  \
  \"x-comment\": { \"board\": \"zcu102\", \"tags\": [true, false, null, -1.5] },\n  \
  \"checks\": [\n    \
    {\n      \
      \"name\": \"PCAP_DONE #bit2\",\n      \
      \"addr\": \"0xFFCA_3010\",\n      \
      \"mask\": \"0x4\",\n      \
      \"expected\": \"0x4\",\n      \
      \"x-note\": \"FPGA programmed\"\n    \
    },\n    \
    {\n      \
      \"name\": \"ISOLATION_REMOVED\\t\\\"ok\\\"\\u0021\\uD83D\\uDE80\",\n      \
      \"addr\": \"0xFF41_0000\",\n      \
      \"mask\": 15,\n      \
      \"expected\": \"0d0\"\n    \
    }\n  \
  ]\n}",
        )
        .unwrap();
        assert_eq!(profile.name, "ZynqMP_AXI_Safety #1");
        assert_eq!(profile.checks.len(), 2);
        assert_eq!(profile.checks[0].name, "PCAP_DONE #bit2");
        assert_eq!(
            profile.checks[1].name,
            "ISOLATION_REMOVED\t\"ok\"!\u{1F680}"
        );
        assert_eq!(profile.checks[1].addr, 0xFF41_0000);
        assert_eq!(profile.checks[1].mask, 0xF);
        assert_eq!(profile.checks[1].expected, 0);

        let deep = format!(
            r#"{{"name": "Bad", "x-deep": {}, "checks": [{{"name": "c", "addr": 0, "mask": 1, "expected": 1}}]}}"#,
            "[".repeat(40) + &"]".repeat(40)
        );
        let check = r#"{"name": "c", "addr": "0x1000", "mask": "0x1", "expected": "0x1"}"#;
        for bad in [
            format!(r#"{{"name": "Bad", "unknown_key": 42, "checks": [{check}]}}"#),
            r#"{"name": "Bad", "checks": [{"name": "c", "addr": 4096, "mask": 1, "expected": 1, "typo": true}]}"#
                .to_string(),
            r#"{"name": "Bad", "checks": []}"#.to_string(),
            r#"{"name": "Bad"}"#.to_string(),
            format!(r#"{{"checks": [{check}]}}"#),
            format!(r#"{{"name": "Bad", "name": "Dup", "checks": [{check}]}}"#),
            format!(r#"{{"name": "Bad", "checks": [{check}]}} trailing"#),
            format!(r#"{{"name": "\uD800", "checks": [{check}]}}"#),
            format!(r#"{{"name": "\u+041", "checks": [{check}]}}"#),
            deep,
            check
                .replace(r#""mask": "0x1""#, r#""mask": "0x1_0000_0000""#)
                .pipe(|c| format!(r#"{{"name": "Bad", "checks": [{c}]}}"#)),
            check
                .replace(r#""addr": "0x1000""#, r#""addr": "0x0x10""#)
                .pipe(|c| format!(r#"{{"name": "Bad", "checks": [{c}]}}"#)),
            check
                .replace(r#""addr": "0x1000""#, r#""addr": 010"#)
                .pipe(|c| format!(r#"{{"name": "Bad", "checks": [{c}]}}"#)),
            check
                .replace(r#""addr": "0x1000""#, r#""addr": 0x1000"#)
                .pipe(|c| format!(r#"{{"name": "Bad", "checks": [{c}]}}"#)),
        ] {
            assert_eq!(
                SafetyProfile::parse(&bad).unwrap_err().code(),
                "E_VALIDATION_PROFILE_PARSE",
                "{bad}"
            );
        }
        assert!(SafetyProfile::from_file(std::path::Path::new("/nonexistent/p.json")).is_err());
    }

    #[cfg(feature = "profile")]
    trait Pipe: Sized {
        fn pipe<R>(self, f: impl FnOnce(Self) -> R) -> R {
            f(self)
        }
    }
    #[cfg(feature = "profile")]
    impl Pipe for String {}
}
