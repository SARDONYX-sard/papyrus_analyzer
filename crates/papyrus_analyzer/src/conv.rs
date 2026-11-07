//! Conversions between protocol types and IDE types.

use std::{
    path::{Path, PathBuf},
    str::FromStr,
    sync::Arc,
};

use lsp_types::{Position, Range, Uri};
use papyrus_ide::{FileId, LineCol, LineIndex, TextRange, TextSize, Workspace};

/// Converts a `file://` URI into a path.
pub(crate) fn uri_to_path(uri: &Uri) -> Option<PathBuf> {
    let s = uri.as_str();
    let rest = s.strip_prefix("file://")?;
    // Skip the authority (usually empty).
    let path = &rest[rest.find('/')?..];
    let decoded = percent_decode(path)?;
    // `/c:/foo` on Windows.
    let bytes = decoded.as_bytes();
    let path = if bytes.len() >= 3
        && bytes[0] == b'/'
        && bytes[1].is_ascii_alphabetic()
        && bytes[2] == b':'
    {
        decoded[1..].replace('/', "\\")
    } else {
        decoded
    };
    Some(PathBuf::from(path))
}

/// Converts an absolute path into a `file://` URI.
pub(crate) fn path_to_uri(path: &Path) -> Uri {
    let s = path.to_string_lossy().replace('\\', "/");
    let mut out = String::from("file://");
    if !s.starts_with('/') {
        out.push('/');
    }
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    Uri::from_str(&out).unwrap_or_else(|_| Uri::from_str("file:///").expect("valid uri"))
}

fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = s.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Text and line index of a file, loaded once per request.
pub(crate) struct Lines {
    pub(crate) text: Arc<str>,
    pub(crate) index: Arc<LineIndex>,
}

impl Lines {
    /// Loads the lines of `file`: the editor contents, or the file on disk.
    pub(crate) fn of(ws: &Workspace, file: FileId) -> Option<Lines> {
        if let Some(doc) = &ws.file(file).open {
            return Some(Lines { text: doc.text.clone(), index: doc.line_index.clone() });
        }
        let text = ws.text(file)?;
        let index = Arc::new(LineIndex::new(&text));
        Some(Lines { text, index })
    }

    /// The lines of a flags file.
    pub(crate) fn flags(flags: &papyrus_ide::flags::FlagTable) -> Lines {
        let text = flags.text.clone();
        let index = Arc::new(LineIndex::new(&text));
        Lines { text, index }
    }

    pub(crate) fn offset(&self, pos: Position) -> TextSize {
        self.index.offset(&self.text, LineCol { line: pos.line, col: pos.character })
    }

    pub(crate) fn position(&self, offset: TextSize) -> Position {
        let lc = self.index.line_col(&self.text, offset);
        Position::new(lc.line, lc.col)
    }

    pub(crate) fn range(&self, range: TextRange) -> Range {
        Range::new(self.position(range.start()), self.position(range.end()))
    }

    pub(crate) fn text_range(&self, range: Range) -> TextRange {
        let start = self.offset(range.start);
        let end = self.offset(range.end).max(start);
        TextRange::new(start, end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_roundtrip() {
        let uri = Uri::from_str("file:///d%3A/My%20Scripts/Foo.psc").unwrap();
        let path = uri_to_path(&uri).unwrap();
        if cfg!(windows) {
            assert_eq!(path, PathBuf::from("d:\\My Scripts\\Foo.psc"));
        }
        let back = path_to_uri(&path);
        assert_eq!(uri_to_path(&back).unwrap(), path);

        let unix = Uri::from_str("file:///home/user/a%23b.psc").unwrap();
        assert_eq!(uri_to_path(&unix).unwrap(), PathBuf::from("/home/user/a#b.psc"));
    }
}
