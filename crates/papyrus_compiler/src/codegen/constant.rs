//! Constants: the values of literals as written in the source, and their
//! conversions at compile time.

use papyrus_ide::index::{BaseType, TypeRef};

use crate::codegen::ir::Operand;

/// Converts a constant to `to` at compile time, like the official compiler
/// does for default values. `None` when the conversion is not exact.
pub(crate) fn convert(value: &Operand, to: TypeRef) -> Option<Operand> {
    // Arrays and objects only take `None`.
    let scalar = if to.array { None } else { Some(to.base) };
    Some(match (value, scalar) {
        (Operand::Var(_), _) => return None,
        (Operand::Int(i), Some(BaseType::Float)) => Operand::Float(*i as f32),
        (Operand::Float(f), Some(BaseType::Int)) => Operand::Int(*f as i32),
        (Operand::Int(i), Some(BaseType::String)) => Operand::Str(i.to_string()),
        (Operand::Bool(b), Some(BaseType::String)) => {
            Operand::Str(if *b { "True" } else { "False" }.to_owned())
        }
        (Operand::None, Some(BaseType::String)) => Operand::Str("None".to_owned()),
        (Operand::Int(i), Some(BaseType::Bool)) => Operand::Bool(*i != 0),
        (Operand::Float(f), Some(BaseType::Bool)) => Operand::Bool(*f != 0.0),
        (Operand::Str(s), Some(BaseType::Bool)) => Operand::Bool(!s.is_empty()),
        (Operand::None, Some(BaseType::Bool)) => Operand::Bool(false),
        (Operand::Bool(b), Some(BaseType::Int)) => Operand::Int(*b as i32),
        (Operand::Bool(b), Some(BaseType::Float)) => Operand::Float(*b as i32 as f32),
        (v @ Operand::Int(_), Some(BaseType::Int))
        | (v @ Operand::Float(_), Some(BaseType::Float))
        | (v @ Operand::Str(_), Some(BaseType::String))
        | (v @ Operand::Bool(_), Some(BaseType::Bool)) => v.clone(),
        (Operand::None, Some(BaseType::Int | BaseType::Float)) => return None,
        (Operand::None, _) => Operand::None,
        _ => return None,
    })
}

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
