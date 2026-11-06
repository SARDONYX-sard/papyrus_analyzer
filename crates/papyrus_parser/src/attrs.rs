//! Attributes in comments, like Rust attributes:
//!
//! ```papyrus
//! ; #![allow(unchecked_cast)]          ; inner: the whole file
//! ScriptName T
//!
//! ; #[allow(unused_local)]             ; outer: the next declaration or statement
//! Function F()
//!     Int unused = 1
//! EndFunction
//! ```
//!
//! An outer attribute, alone on its line, applies to the declaration or
//! statement starting on the next line, all its lines: a whole `Function`,
//! `State` or `If`. An inner attribute comes before the code, at the top of
//! the file. Text after the closing `]` (a reason) is ignored.
//!
//! Like in Rust, attributes go before what they apply to: one after code on
//! its line, or an inner one below code, applies to nothing
//! ([`misplaced`]).

use crate::{SyntaxKind, SyntaxNode, SyntaxToken, TextRange, TextSize};

/// An attribute of a comment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attribute<'a> {
    /// `#![...]`: the whole file.
    pub inner: bool,
    /// The path: `allow`, `fmt::skip`.
    pub path: &'a str,
    /// The arguments between parentheses: `unused_local`.
    pub args: Vec<&'a str>,
    /// The code it applies to: the whole file for inner attributes.
    pub target: TextRange,
}

/// Why an attribute applies to nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Misplacement {
    /// After code on its line: it would be unclear which line it is for.
    AfterCode,
    /// `#![...]` below the first code of the file.
    InnerBelowCode,
}

/// An attribute comment in the wrong place, see [`misplaced`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Misplaced<'a> {
    /// The comment.
    pub range: TextRange,
    /// The text of the comment.
    pub comment: &'a str,
    /// Why it applies to nothing.
    pub why: Misplacement,
}

/// The attributes of the comments of a file, the misplaced ones left out.
pub fn attributes<'a>(root: SyntaxNode<'a>) -> Vec<Attribute<'a>> {
    scan(root)
        .into_iter()
        .filter_map(|(tok, place)| {
            let (inner, path, args) = parse(tok.text())?;
            let target = match place {
                Ok(()) if inner => root.text_range(),
                Ok(()) => next_item(root, tok.text_range().end())?,
                Err(_) => return None,
            };
            Some(Attribute { inner, path, args, target })
        })
        .collect()
}

/// The attribute comments in the wrong place, which apply to nothing.
pub fn misplaced<'a>(root: SyntaxNode<'a>) -> Vec<Misplaced<'a>> {
    scan(root)
        .into_iter()
        .filter_map(|(tok, place)| {
            let why = place.err()?;
            Some(Misplaced { range: tok.text_range(), comment: tok.text(), why })
        })
        .collect()
}

/// The comments the Creation Kit writes in its fragments, the longest first.
const FRAGMENT_MARKERS: [&str; 12] = [
    "BEGIN FRAGMENT CODE",
    "END FRAGMENT CODE",
    "NEXT FRAGMENT INDEX",
    "BEGIN FRAGMENT",
    "END FRAGMENT",
    "BEGIN CODE",
    "END CODE",
    "BEGIN ALIAS PROPERTY",
    "ALIAS PROPERTY TYPE",
    "END ALIAS PROPERTY",
    "BEGIN AUTOCAST TYPE",
    "END AUTOCAST",
];

/// The marker of the Creation Kit a line comment starts with, as written:
/// `BEGIN FRAGMENT` in `;BEGIN FRAGMENT Fragment_0`.
pub fn fragment_marker(comment: &str) -> Option<&str> {
    let body = comment.strip_prefix(';')?.trim_start();
    FRAGMENT_MARKERS.iter().find_map(|marker| {
        let word = body.get(..marker.len()).filter(|b| b.eq_ignore_ascii_case(marker))?;
        let ends = !body[marker.len()..].starts_with(|c: char| c.is_alphanumeric() || c == '_');
        ends.then_some(word)
    })
}

/// The fragments the Creation Kit generates: from a comment alone on its line
/// starting with `BEGIN FRAGMENT CODE` to its `END FRAGMENT CODE` (any case),
/// or to the last token of the file without end. The Creation Kit finds its
/// code by these lines: nothing may change them.
pub fn fragments(root: SyntaxNode<'_>) -> Vec<TextRange> {
    let is_marker = |text: &str, marker: &str| {
        fragment_marker(text).is_some_and(|m| m.eq_ignore_ascii_case(marker))
    };
    let mut out = Vec::new();
    let mut start = None;
    let mut first_on_line = true;
    let mut last_end = TextSize::new(0);
    for tok in root.descendant_tokens() {
        match tok.kind() {
            SyntaxKind::NEWLINE | SyntaxKind::LINE_CONTINUATION => first_on_line = true,
            SyntaxKind::WHITESPACE => {}
            kind => {
                let alone = first_on_line && kind == SyntaxKind::LINE_COMMENT;
                first_on_line = false;
                last_end = tok.text_range().end();
                match start {
                    None if alone && is_marker(tok.text(), "BEGIN FRAGMENT CODE") => {
                        start = Some(tok.text_range().start());
                    }
                    Some(begin) if alone && is_marker(tok.text(), "END FRAGMENT CODE") => {
                        out.push(TextRange::new(begin, last_end));
                        start = None;
                    }
                    _ => {}
                }
            }
        }
    }
    if let Some(begin) = start {
        out.push(TextRange::new(begin, last_end));
    }
    out
}

/// Whether an edit of `range` changes one of `fragments`. Text inserted right
/// before or after a fragment does not.
pub fn touches_fragment(fragments: &[TextRange], range: TextRange) -> bool {
    fragments.iter().any(|f| range.start() < f.end() && f.start() < range.end())
}

/// The attribute comments of a file, with where they are.
fn scan(root: SyntaxNode<'_>) -> Vec<(SyntaxToken<'_>, Result<(), Misplacement>)> {
    let text = root.text();
    let first_code = root
        .descendant_tokens()
        .find(|t| !t.kind().is_trivia() && t.kind() != SyntaxKind::NEWLINE)
        .map_or(text.len(), |t| usize::from(t.text_range().start()));
    root.descendant_tokens()
        .filter(|t| t.kind() == SyntaxKind::LINE_COMMENT && parse(t.text()).is_some())
        .map(|tok| {
            let start = usize::from(tok.text_range().start());
            let line_start = text[..start].rfind('\n').map_or(0, |i| i + 1);
            let place = if !text[line_start..start].trim().is_empty() {
                Err(Misplacement::AfterCode)
            } else if tok.text().trim_start_matches(';').trim_start().starts_with("#![")
                && start > first_code
            {
                Err(Misplacement::InnerBelowCode)
            } else {
                Ok(())
            };
            (tok, place)
        })
        .collect()
}

/// The canonical spelling of an attribute comment, for the formatter:
/// `;  #[ allow( unused-local ,x ) ]  why` → `; #[allow(unused_local, x)] why`.
/// `None` for other comments.
pub fn normalize(comment: &str) -> Option<String> {
    let (inner, path, args) = parse(comment)?;
    let rest = comment[comment.find(']')? + 1..].trim();
    let mut out = format!("; #{}[{path}", if inner { "!" } else { "" });
    if comment[..comment.find(']')?].contains('(') {
        let args: Vec<String> = args.iter().map(|a| a.replace('-', "_")).collect();
        out.push_str(&format!("({})", args.join(", ")));
    }
    out.push(']');
    if !rest.is_empty() {
        out.push(' ');
        out.push_str(rest);
    }
    Some(out)
}

/// `; #[path(args)]` or `; #![path(args)]`.
fn parse(comment: &str) -> Option<(bool, &str, Vec<&str>)> {
    let rest = comment.strip_prefix(';')?.trim_start();
    let (inner, rest) = match rest.strip_prefix("#![") {
        Some(r) => (true, r),
        None => (false, rest.strip_prefix("#[")?),
    };
    let body = rest[..rest.find(']')?].trim();
    let (path, args) = match body.find('(') {
        Some(open) => {
            let args = body[open + 1..].strip_suffix(')')?;
            let args = args.split(',').map(str::trim).filter(|a| !a.is_empty()).collect();
            (body[..open].trim(), args)
        }
        None => (body, Vec::new()),
    };
    Some((inner, path, args))
}

/// The declaration or statement starting at the first code after `offset`.
fn next_item(root: SyntaxNode<'_>, offset: TextSize) -> Option<TextRange> {
    let tok = root.descendant_tokens().find(|t| {
        t.text_range().start() >= offset && !t.kind().is_trivia() && t.kind() != SyntaxKind::NEWLINE
    })?;
    let start = tok.text_range().start();
    // The outermost node starting there, below the file and the blocks.
    let node = tok
        .parent()?
        .ancestors()
        .take_while(|n| !matches!(n.kind(), SyntaxKind::SOURCE_FILE | SyntaxKind::BLOCK))
        .filter(|n| n.text_range().start() == start)
        .last()
        .or_else(|| tok.parent())?;
    Some(node.trimmed_range())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attrs(src: &str) -> Vec<(bool, String, Vec<String>, String)> {
        let parse = crate::parse(src);
        attributes(parse.syntax_node())
            .into_iter()
            .map(|a| {
                (
                    a.inner,
                    a.path.to_owned(),
                    a.args.iter().map(|s| s.to_string()).collect(),
                    src[a.target].lines().next().unwrap_or("").to_owned(),
                )
            })
            .collect()
    }

    #[test]
    fn inner_and_outer() {
        let src = "; #![allow(unchecked_cast)]\nScriptName T\n; #[allow(unused_local, all)] kept\nFunction F()\n    Int x = 1\nEndFunction\n";
        let found = attrs(src);
        assert!(found[0].0);
        assert_eq!(
            found[1],
            (
                false,
                "allow".into(),
                vec!["unused_local".into(), "all".into()],
                "Function F()".into()
            )
        );
        // The function, all its lines.
        let parse = crate::parse(src);
        let f = &attributes(parse.syntax_node())[1];
        assert!(src[f.target].ends_with("EndFunction"));
    }

    #[test]
    fn fragments_and_edits_touching_them() {
        let found = |src: &str| {
            let parse = crate::parse(src);
            let ranges = fragments(parse.syntax_node());
            ranges.iter().map(|r| src[*r].to_owned()).collect::<Vec<_>>()
        };
        let src = "; before\n;BEGIN FRAGMENT CODE - x\nScriptName T\n\n;BEGIN FRAGMENT Fragment_0\n;END FRAGMENT\n;end fragment code - y\nInt x\n";
        assert_eq!(
            found(src),
            [
                ";BEGIN FRAGMENT CODE - x\nScriptName T\n\n;BEGIN FRAGMENT Fragment_0\n;END FRAGMENT\n;end fragment code - y"
            ]
        );
        // Without end: until the last token. After code, or a stray end: nothing.
        assert_eq!(
            found(";BEGIN FRAGMENT CODE\nScriptName T\n\n\n"),
            [";BEGIN FRAGMENT CODE\nScriptName T"]
        );
        assert!(
            found("ScriptName T ; BEGIN FRAGMENT CODE\nInt x\n;END FRAGMENT CODE\n").is_empty()
        );

        let f = [TextRange::new(TextSize::new(10), TextSize::new(20))];
        let edit = |a: u32, b: u32| touches_fragment(&f, TextRange::new(a.into(), b.into()));
        assert!(edit(15, 15) && edit(5, 11) && edit(19, 25) && edit(0, 30));
        assert!(!edit(10, 10) && !edit(20, 20) && !edit(0, 10) && !edit(20, 30));
    }

    #[test]
    fn misplaced_attributes_apply_to_nothing() {
        let src = "; a comment\n; #![allow(all)] top: fine\nScriptName T\n; #![fmt::skip] below code\nFunction F()\n    Int x = 1 ; #[allow(unused_local)] after code\nEndFunction\n";
        let parse = crate::parse(src);
        assert_eq!(attrs(src).len(), 1);
        let found: Vec<(Misplacement, &str)> =
            misplaced(parse.syntax_node()).into_iter().map(|m| (m.why, m.comment)).collect();
        assert_eq!(
            found,
            [
                (Misplacement::InnerBelowCode, "; #![fmt::skip] below code"),
                (Misplacement::AfterCode, "; #[allow(unused_local)] after code"),
            ]
        );
    }
}
