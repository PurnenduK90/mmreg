//! Integer parsing for CLI arguments, safety profiles and sysfs attributes (allocates only on error).

/// Parses an unsigned 64-bit integer from `input`.
///
/// Rules:
/// - Accepts at most one optional prefix: `0x` / `0X` (base 16) or `0d` / `0D` (base 10).
/// - If unprefixed, uses `default_radix` (typically `16` for CLI hex mode or `10` for `--dec` / profile strings).
/// - Rejects duplicate prefixes (such as `0x0x10`), signs (`+` / `-`), empty strings, or strings with no digits.
/// - Allows visual `_` separators between digits without allocating a temporary `String`.
pub fn parse_u64_radix(input: &str, default_radix: u32) -> Result<u64, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() || trimmed.starts_with('+') || trimmed.starts_with('-') {
        return Err(format!("invalid integer '{}'", input));
    }

    let (digits, radix) = if let Some(hex) = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
    {
        (hex, 16)
    } else if let Some(dec) = trimmed
        .strip_prefix("0d")
        .or_else(|| trimmed.strip_prefix("0D"))
    {
        (dec, 10)
    } else {
        (trimmed, default_radix)
    };

    let mut acc: u64 = 0;
    let mut saw_digit = false;

    for b in digits.bytes() {
        if b == b'_' {
            continue;
        }
        let digit = match (b as char).to_digit(radix) {
            Some(d) => d as u64,
            None => return Err(format!("invalid digit '{}' in '{}'", b as char, input)),
        };
        acc = acc
            .checked_mul(radix as u64)
            .and_then(|v| v.checked_add(digit))
            .ok_or_else(|| format!("integer overflow in '{}'", input))?;
        saw_digit = true;
    }

    if !saw_digit {
        return Err(format!("no digits found in '{}'", input));
    }

    Ok(acc)
}

/// Parses an unsigned 32-bit integer from `input` (same rules as [`parse_u64_radix`]).
pub fn parse_u32_radix(input: &str, default_radix: u32) -> Result<u32, String> {
    let val = parse_u64_radix(input, default_radix)?;
    u32::try_from(val).map_err(|_| format!("value '{}' exceeds 32-bit unsigned integer", input))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_zero_alloc_integer_parsers() {
        assert_eq!(parse_u64_radix("0x4000_0000", 16).unwrap(), 0x4000_0000);
        assert_eq!(parse_u64_radix("0X4000_0000", 10).unwrap(), 0x4000_0000);
        assert_eq!(parse_u64_radix("4000_0000", 16).unwrap(), 0x4000_0000);
        assert_eq!(parse_u64_radix("0d1_024", 16).unwrap(), 1024);
        assert_eq!(parse_u64_radix("0D42", 16).unwrap(), 42);
        assert_eq!(parse_u32_radix("0xCAFE_BABE", 16).unwrap(), 0xCAFE_BABE);

        // Rejects duplicate prefixes, empty strings, underscore-only strings, signs, and overflow
        assert!(parse_u64_radix("0x0x10", 16).is_err());
        assert!(parse_u64_radix("0d0d10", 16).is_err());
        assert!(parse_u64_radix("", 16).is_err());
        assert!(parse_u64_radix("0x___", 16).is_err());
        assert!(parse_u64_radix("-1", 10).is_err());
        assert!(parse_u64_radix("+1", 10).is_err());
        assert!(parse_u64_radix("0x1_0000_0000_0000_0000", 16).is_err());
        assert!(parse_u32_radix("0x1_0000_0000", 16).is_err());
        assert_eq!(parse_u64_radix("0xa0000100\n", 16).unwrap(), 0xA000_0100);
    }
}
