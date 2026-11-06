//! Small text parsers shared by the data files of both backends.

/// `int(text, 0)` of Python: `0x`, `0o`, `0b` prefixes or a decimal without
/// leading zeros; signs, surrounding whitespace and single underscores
/// between digits are accepted.
pub fn parse_int_auto(text: &str) -> Option<i128> {
    let trimmed = text.trim();
    let (sign, body) = match trimmed.as_bytes().first()? {
        b'-' => ("-", &trimmed[1..]),
        b'+' => ("", &trimmed[1..]),
        _ => ("", trimmed),
    };
    let lower = body.get(..2).map(str::to_ascii_lowercase);
    let (radix, digits) = match lower.as_deref() {
        Some("0x") => (16, &body[2..]),
        Some("0o") => (8, &body[2..]),
        Some("0b") => (2, &body[2..]),
        _ => (10, body),
    };
    let digits = if radix != 10 {
        digits.strip_prefix('_').unwrap_or(digits)
    } else {
        digits
    };
    if digits.is_empty()
        || digits.starts_with('_')
        || digits.ends_with('_')
        || digits.contains("__")
        || digits.starts_with(['+', '-'])
    {
        return None;
    }
    let cleaned: String = digits.chars().filter(|&c| c != '_').collect();
    if radix == 10
        && cleaned.len() > 1
        && cleaned.starts_with('0')
        && cleaned.bytes().any(|b| b != b'0')
    {
        return None;
    }
    let value = i128::from_str_radix(&cleaned, radix).ok()?;
    Some(if sign == "-" { -value } else { value })
}

/// A frequency: `40MHz`, `32.768 kHz`, `8e6` or a plain number of Hz.
pub fn parse_frequency(text: &str) -> std::result::Result<f64, String> {
    let trimmed = text.trim();
    let lower = trimmed.to_ascii_lowercase();
    let (number, scale) = [("ghz", 1e9), ("mhz", 1e6), ("khz", 1e3), ("hz", 1.0)]
        .into_iter()
        .find_map(|(unit, scale)| Some((lower.strip_suffix(unit)?, scale)))
        .unwrap_or((lower.as_str(), 1.0));
    match number.trim_end().parse::<f64>() {
        Ok(value) if value.is_finite() && value > 0.0 => Ok(value * scale),
        _ => Err(format!("{text:?} is not a frequency such as \"40MHz\"")),
    }
}

/// Case-insensitive glob match supporting `*` and `?`.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.to_ascii_uppercase().chars().collect();
    let text: Vec<char> = text.to_ascii_uppercase().chars().collect();
    let (mut p, mut t) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while t < text.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some(p);
            mark = t;
            p += 1;
        } else if let Some(position) = star {
            p = position + 1;
            mark += 1;
            t = mark;
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|&c| c == '*')
}

/// How specific a matching pattern is: exact names first, then the number of
/// literal characters.
pub fn specificity(pattern: &str) -> (bool, usize) {
    let literal = pattern.chars().filter(|c| !matches!(c, '*' | '?')).count();
    (literal == pattern.chars().count(), literal)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integers_follow_python_int_with_base_0() {
        assert_eq!(parse_int_auto(" 0x1F "), Some(31));
        assert_eq!(parse_int_auto("-0b1_0"), Some(-2));
        assert_eq!(parse_int_auto("1_000"), Some(1000));
        assert_eq!(parse_int_auto("010"), None);
        assert_eq!(parse_int_auto("0x"), None);
    }

    #[test]
    fn frequencies_take_a_unit() {
        assert_eq!(parse_frequency("40MHz"), Ok(40e6));
        assert_eq!(parse_frequency("32.768 kHz"), Ok(32_768.0));
        assert_eq!(parse_frequency("8e6"), Ok(8e6));
        assert!(parse_frequency("0Hz").is_err());
    }

    #[test]
    fn chip_patterns() {
        assert!(glob_match("SR6P6*", "sr6p6c4"));
        assert!(glob_match("STM32F4?5", "STM32F405"));
        assert!(!glob_match("SR6P6", "SR6P7"));
        assert!(specificity("SR6P6") > specificity("SR6P*"));
        assert!(specificity("SR6P6*") > specificity("SR6*"));
    }
}
