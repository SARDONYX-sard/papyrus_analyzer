//! Values of literals, as written in the source.

use crate::ir::Operand;

/// Parses a literal: an integer (decimal or `0x` hexadecimal), a float, a
/// string, `True`, `False` or `None`, optionally negated.
pub(crate) fn parse(text: &str) -> Option<Operand> {
    let text = text.trim();
    if let Some(rest) = text.strip_prefix('-') {
        return match parse(rest)? {
            Operand::Int(i) => Some(Operand::Int(i.wrapping_neg())),
            Operand::Float(f) => Some(Operand::Float(-f)),
            _ => None,
        };
    }
    if let Some(body) = text.strip_prefix('"') {
        return Some(Operand::Str(unescape(body.strip_suffix('"').unwrap_or(body))));
    }
    if text.eq_ignore_ascii_case("true") {
        return Some(Operand::Bool(true));
    }
    if text.eq_ignore_ascii_case("false") {
        return Some(Operand::Bool(false));
    }
    if text.eq_ignore_ascii_case("none") {
        return Some(Operand::None);
    }
    if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        // Hexadecimal literals cover the whole 32 bits: 0xFFFFFFFF is -1.
        return u32::from_str_radix(hex, 16).ok().map(|v| Operand::Int(v as i32));
    }
    if text.contains(['.', 'e', 'E']) {
        return text.parse::<f32>().ok().map(Operand::Float);
    }
    text.parse::<i64>().ok().map(|v| Operand::Int(v as i32))
}

/// Resolves the escapes of a string literal: `\n`, `\t`, `\\` and `\"`.
fn unescape(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literals() {
        assert_eq!(parse("42"), Some(Operand::Int(42)));
        assert_eq!(parse("-1"), Some(Operand::Int(-1)));
        assert_eq!(parse("0xFFFFFFFF"), Some(Operand::Int(-1)));
        assert_eq!(parse("1.5"), Some(Operand::Float(1.5)));
        assert_eq!(parse("-2.0"), Some(Operand::Float(-2.0)));
        assert_eq!(parse("\"a\\\"b\\n\""), Some(Operand::Str("a\"b\n".to_owned())));
        assert_eq!(parse("TRUE"), Some(Operand::Bool(true)));
        assert_eq!(parse("none"), Some(Operand::None));
        assert_eq!(parse("foo"), None);
    }
}
