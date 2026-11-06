//! Formatter for Papyrus scripts.
//!
//! The formatter works on the lossless syntax tree of [`papyrus_parser`]: it
//! keeps every significant token and every comment, and only rewrites the
//! whitespace between them, and the spelling of keywords:
//!
//! - lines are indented by nesting: function and event bodies, `If`, `While`,
//!   `State` and full `Property` blocks;
//! - tokens are separated by one space, except around `.`, `,`, brackets,
//!   calls and unary operators;
//! - keywords get their canonical spelling (`endfunction` → `EndFunction`),
//!   and names the spelling of their definition (`actor` → `Actor`,
//!   `getvalue()` → `GetValue()`, see [`NameCases`] and [`Names`]);
//! - runs of blank lines are collapsed, and blank lines at the start or the
//!   end of a block are removed;
//! - trailing whitespace is removed and the file ends with one line break.
//!
//! Attributes in comments keep code as written, like `#[rustfmt::skip]`:
//! `; #[fmt::skip]` the next declaration or statement, `; #![fmt::skip]` the whole
//! file. See [`papyrus_parser::attrs`].
//!
//! The fragments the Creation Kit generates, from `;BEGIN FRAGMENT CODE` to
//! `;END FRAGMENT CODE`, are kept as written, blank lines included: it finds
//! its code by these exact lines. Only the line breaks follow [`Newline`].
//!
//! Scripts with syntax errors are not formatted. The result is checked before
//! being returned: it must parse without errors and contain the same
//! significant tokens as the input.
//!
//! # Example
//!
//! ```
//! let out = papyrus_fmt::format("scriptname Foo\nfunction F()\nx=1\nendfunction\n", &Default::default());
//! assert_eq!(out.unwrap(), "ScriptName Foo\nFunction F()\n    x = 1\nEndFunction\n");
//! ```

use std::fmt;

use papyrus_parser::{SyntaxKind, SyntaxToken, TextRange};

/// How lines are indented.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Indent {
    /// One tab per level.
    Tabs,
    /// The given number of spaces per level.
    Spaces(u8),
}

/// The spelling of keywords and of names, named like the `rename_all` cases
/// of serde. Names start from their definition, see [`NameCases`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Case {
    /// Keywords: `PascalCase`, except the operator-like `as` and `extends`,
    /// which most scripts write in lowercase: `ScriptName Foo extends Quest`.
    /// Names: as defined.
    Canonical,
    /// As written.
    Preserve,
    /// `EndFunction`, `AutoReadOnly`, `As`, `Extends`.
    PascalCase,
    /// `endFunction`, `autoReadOnly`.
    CamelCase,
    /// `endfunction`, `autoreadonly`.
    Lowercase,
    /// `ENDFUNCTION`, `AUTOREADONLY`.
    Uppercase,
}

impl Case {
    /// The accepted names, for configuration files.
    pub const NAMES: [&'static str; 6] =
        ["canonical", "preserve", "PascalCase", "camelCase", "lowercase", "UPPERCASE"];
}

impl std::str::FromStr for Case {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s.to_ascii_lowercase().replace(['_', '-'], "").as_str() {
            "canonical" => Case::Canonical,
            "preserve" => Case::Preserve,
            "pascalcase" => Case::PascalCase,
            "camelcase" => Case::CamelCase,
            "lowercase" => Case::Lowercase,
            "uppercase" => Case::Uppercase,
            _ => {
                return Err(format!(
                    "unknown case `{s}`, expected one of {}",
                    Case::NAMES.join(", ")
                ));
            }
        })
    }
}

/// The line break written by the formatter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Newline {
    /// The one the input uses (`\r\n` if it contains any).
    Auto,
    /// `\n`
    Lf,
    /// `\r\n`
    CrLf,
}

/// Formatting options.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormatOptions {
    /// Indentation of nested lines.
    pub indent: Indent,
    /// Spelling of keywords.
    pub keyword_case: Case,
    /// Spelling of single keywords, winning over `keyword_case`, keyed by the
    /// lowercase keyword: `{"as": "As"}`. Spellings that are not the keyword
    /// (ignoring case) are ignored.
    pub keyword_overrides: std::collections::BTreeMap<String, String>,
    /// Consecutive blank lines kept at most.
    pub max_blank_lines: usize,
    /// The line break.
    pub newline: Newline,
    /// End the file with a line break.
    pub final_newline: bool,
    /// Lines longer than this break with `\`, after commas and before `&&`,
    /// `||`, like `max_width` of rustfmt; `0` (the default) keeps lines as
    /// written.
    pub max_width: usize,
    /// Spelling of the names, by kind. `Canonical` is the spelling of the
    /// definition, which `PascalCase` and `camelCase` start from too.
    pub name_cases: NameCases,
    /// The definitions of the names of the script: the formatter cannot know
    /// them from the script alone. Names without one are kept as written.
    pub names: std::sync::Arc<Names>,
}

/// The spelling of each kind of name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NameCases {
    /// Scripts: types, `extends`, `Import`, receivers like `Debug.Trace`.
    pub scripts: Case,
    /// Functions and events.
    pub functions: Case,
    /// Properties.
    pub properties: Case,
    /// Local variables, parameters and script variables.
    pub variables: Case,
    /// User flags such as `Hidden`.
    pub flags: Case,
}

impl Default for NameCases {
    fn default() -> Self {
        let c = Case::Canonical;
        NameCases { scripts: c, functions: c, properties: c, variables: c, flags: c }
    }
}

/// The kind of a name, see [`NameCases`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameKind {
    /// A script.
    Script,
    /// A function or an event.
    Function,
    /// A property.
    Property,
    /// A local variable, a parameter or a script variable.
    Variable,
    /// A user flag.
    Flag,
}

/// The definitions of the identifiers of one script, by start offset.
/// Papyrus ignores case: `actor`, `getvalue()` work, and are written like
/// their definition: `Actor`, `GetValue()`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Names {
    defs: std::collections::BTreeMap<u32, (NameKind, String)>,
}

impl Names {
    /// The identifier starting at `offset` is a `kind` defined as `spelling`.
    pub fn insert(&mut self, offset: u32, kind: NameKind, spelling: String) {
        self.defs.insert(offset, (kind, spelling));
    }
}

impl Default for FormatOptions {
    fn default() -> Self {
        FormatOptions {
            indent: Indent::Spaces(4),
            keyword_case: Case::Canonical,
            keyword_overrides: Default::default(),
            max_blank_lines: 1,
            newline: Newline::Auto,
            final_newline: true,
            max_width: 0,
            name_cases: NameCases::default(),
            names: Default::default(),
        }
    }
}

impl FormatOptions {
    /// The spelling of a keyword token.
    fn keyword_spelling<'t>(&self, kind: SyntaxKind, text: &'t str) -> std::borrow::Cow<'t, str> {
        use std::borrow::Cow;
        if let Some(spelling) = self.keyword_overrides.get(&text.to_ascii_lowercase())
            && spelling.eq_ignore_ascii_case(text)
        {
            return Cow::Owned(spelling.clone());
        }
        let Some(pascal) = pascal_keyword(kind) else { return Cow::Borrowed(text) };
        match self.keyword_case {
            Case::Preserve => Cow::Borrowed(text),
            Case::Canonical => match kind {
                SyntaxKind::AS_KW | SyntaxKind::EXTENDS_KW => {
                    Cow::Owned(pascal.to_ascii_lowercase())
                }
                _ => Cow::Borrowed(pascal),
            },
            Case::PascalCase => Cow::Borrowed(pascal),
            Case::CamelCase => {
                let mut out = pascal.to_owned();
                out[..1].make_ascii_lowercase();
                Cow::Owned(out)
            }
            Case::Lowercase => Cow::Owned(pascal.to_ascii_lowercase()),
            Case::Uppercase => Cow::Owned(pascal.to_ascii_uppercase()),
        }
    }

    /// The spelling of an identifier, from its definition; identifiers
    /// without known definition are kept.
    fn name_spelling<'t>(&'t self, tok: SyntaxToken<'t>) -> std::borrow::Cow<'t, str> {
        use std::borrow::Cow;
        let text = tok.text();
        let Some((kind, defined)) = self.names.defs.get(&u32::from(tok.text_range().start()))
        else {
            return Cow::Borrowed(text);
        };
        // A stale definition (e.g. computed for another text) is ignored.
        if !defined.eq_ignore_ascii_case(text) {
            return Cow::Borrowed(text);
        }
        let case = match kind {
            NameKind::Script => self.name_cases.scripts,
            NameKind::Function => self.name_cases.functions,
            NameKind::Property => self.name_cases.properties,
            NameKind::Variable => self.name_cases.variables,
            NameKind::Flag => self.name_cases.flags,
        };
        let first = |upper: bool| {
            let mut out = defined.clone();
            if upper {
                out[..1].make_ascii_uppercase();
            } else {
                out[..1].make_ascii_lowercase();
            }
            Cow::Owned(out)
        };
        match case {
            Case::Preserve => Cow::Borrowed(text),
            Case::Canonical => Cow::Borrowed(defined),
            Case::PascalCase => first(true),
            Case::CamelCase => first(false),
            Case::Lowercase => Cow::Owned(defined.to_ascii_lowercase()),
            Case::Uppercase => Cow::Owned(defined.to_ascii_uppercase()),
        }
    }
}

/// Why a script was not formatted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FormatError {
    /// The script has syntax errors; formatting could make things worse.
    SyntaxErrors(usize),
    /// The formatted text would change the meaning of the script. This is a
    /// bug of the formatter; the input is left untouched.
    Unsafe(String),
}

impl fmt::Display for FormatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FormatError::SyntaxErrors(n) => {
                write!(f, "not formatted: the script has {n} syntax errors")
            }
            FormatError::Unsafe(why) => write!(f, "not formatted: the result would differ ({why})"),
        }
    }
}

impl std::error::Error for FormatError {}

/// Formats a whole script.
pub fn format(text: &str, options: &FormatOptions) -> Result<String, FormatError> {
    let parse = papyrus_parser::parse(text);
    if !parse.errors().is_empty() {
        return Err(FormatError::SyntaxErrors(parse.errors().len()));
    }
    let root = parse.syntax_node();
    let newline = match options.newline {
        Newline::Auto if text.contains("\r\n") => "\r\n",
        Newline::Auto | Newline::Lf => "\n",
        Newline::CrLf => "\r\n",
    };

    let lines = split_lines(root.descendant_tokens());
    let attrs = papyrus_parser::attrs::attributes(root);
    let skip = |a: &papyrus_parser::attrs::Attribute<'_>| a.path.eq_ignore_ascii_case("fmt::skip");
    if attrs.iter().any(|a| a.inner && skip(a)) {
        return Ok(text.to_owned());
    }
    let skipped: Vec<TextRange> = attrs.iter().filter(|a| skip(a)).map(|a| a.target).collect();
    let fragment = fragment_lines(root, &lines);
    let mut kept = kept_lines(&lines, &skipped);
    kept.iter_mut().zip(&fragment).for_each(|(k, f)| *k |= f);
    let levels = indent_levels(&lines);
    let mut out = String::with_capacity(text.len() + text.len() / 8);
    let mut pending_blank = 0usize;
    let mut written_any = false;

    let mut i = 0;
    while i < lines.len() {
        let line = &lines[i];
        if line.is_blank() && !fragment[i] {
            pending_blank += 1;
            i += 1;
            continue;
        }
        if written_any && pending_blank > 0 && keep_blank_lines(&lines, &levels, i) {
            for _ in 0..pending_blank.min(options.max_blank_lines) {
                out.push_str(newline);
            }
        }
        pending_blank = 0;
        // With `max_width`, a statement continued with `\` is laid out
        // again from one line: joined when it fits, broken where it does not.
        let end = (i + 1..lines.len()).find(|&j| !lines[j].continues()).unwrap_or(lines.len());
        let joinable = options.max_width > 0
            && end > i + 1
            && (i..end).all(|j| !kept[j])
            && lines[i..end].iter().flat_map(|l| &l.tokens).all(|t| !t.kind().is_comment());
        if joinable {
            let tokens = lines[i..end]
                .iter()
                .enumerate()
                .flat_map(|(n, l)| {
                    // The indentation of the continuations goes too.
                    let skip = if n == 0 { 0 } else { 1 };
                    l.tokens
                        .iter()
                        .skip_while(move |t| skip == 1 && t.kind() == SyntaxKind::WHITESPACE)
                })
                .filter(|t| t.kind() != SyntaxKind::LINE_CONTINUATION)
                .copied()
                .collect();
            write_line(&mut out, &Line { tokens }, levels[i], options, newline);
            out.push_str(newline);
            written_any = true;
            i = end;
            continue;
        }
        if kept[i] {
            write_verbatim(&mut out, line);
        } else {
            write_line(&mut out, line, levels[i], options, newline);
        }
        out.push_str(newline);
        written_any = true;
        i += 1;
    }

    if !options.final_newline && out.ends_with(newline) {
        out.truncate(out.len() - newline.len());
    }
    verify(text, &out)?;
    Ok(out)
}

/// The lines in the code of `#[fmt::skip]` attributes, kept as written.
fn kept_lines(lines: &[Line<'_>], skipped: &[TextRange]) -> Vec<bool> {
    lines
        .iter()
        .map(|l| {
            l.tokens.iter().find(|t| t.kind() != SyntaxKind::WHITESPACE).is_some_and(|t| {
                skipped.iter().any(|r| r.contains_inclusive(t.text_range().start()))
            })
        })
        .collect()
}

/// The lines of the fragments of the Creation Kit, the blank ones included.
fn fragment_lines(root: papyrus_parser::SyntaxNode<'_>, lines: &[Line<'_>]) -> Vec<bool> {
    let mut fragment = vec![false; lines.len()];
    for range in papyrus_parser::attrs::fragments(root) {
        let inside =
            |l: &Line<'_>| l.first().is_some_and(|t| range.contains(t.text_range().start()));
        if let (Some(first), Some(last)) =
            (lines.iter().position(inside), lines.iter().rposition(inside))
        {
            fragment[first..=last].fill(true);
        }
    }
    fragment
}

/// A line as written, but its line break.
fn write_verbatim(out: &mut String, line: &Line<'_>) {
    for tok in &line.tokens {
        match tok.kind() {
            // The token includes the line break, which the caller adds back.
            SyntaxKind::LINE_CONTINUATION => out.push('\\'),
            _ => out.push_str(tok.text()),
        }
    }
}

/// One physical line: its tokens without the line break.
struct Line<'a> {
    tokens: Vec<SyntaxToken<'a>>,
}

impl<'a> Line<'a> {
    fn significant(&self) -> impl Iterator<Item = &SyntaxToken<'a>> {
        self.tokens.iter().filter(|t| t.kind() != SyntaxKind::WHITESPACE)
    }

    fn first(&self) -> Option<SyntaxToken<'a>> {
        self.significant().next().copied()
    }

    fn is_blank(&self) -> bool {
        self.first().is_none()
    }

    /// Only comments: the line takes the indentation of its surroundings.
    fn is_comment_only(&self) -> bool {
        self.significant().all(|t| t.kind().is_comment()) && !self.is_blank()
    }

    /// The line continues a previous one after a `\`.
    fn continues(&self) -> bool {
        self.tokens.first().is_some_and(|t| {
            t.prev_token().is_some_and(|p| p.kind() == SyntaxKind::LINE_CONTINUATION)
        })
    }
}

/// Splits tokens at line breaks. A line continuation token ends its physical
/// line too, but stays on it.
fn split_lines<'a>(tokens: impl Iterator<Item = SyntaxToken<'a>>) -> Vec<Line<'a>> {
    let mut lines = vec![Line { tokens: Vec::new() }];
    for tok in tokens {
        match tok.kind() {
            SyntaxKind::NEWLINE => lines.push(Line { tokens: Vec::new() }),
            SyntaxKind::LINE_CONTINUATION => {
                lines.last_mut().expect("at least one line").tokens.push(tok);
                lines.push(Line { tokens: Vec::new() });
            }
            _ => lines.last_mut().expect("at least one line").tokens.push(tok),
        }
    }
    lines
}

/// Keywords that close a block and sit at the level of its opener.
fn is_closer(kind: SyntaxKind) -> bool {
    use SyntaxKind::*;
    matches!(
        kind,
        ENDIF_KW
            | ELSEIF_KW
            | ELSE_KW
            | ENDWHILE_KW
            | ENDFUNCTION_KW
            | ENDEVENT_KW
            | ENDPROPERTY_KW
            | ENDSTATE_KW
    )
}

/// The nesting level of a token that starts a line: the number of enclosing
/// bodies (`BLOCK`s), plus one for declarations inside a state or a full
/// property (but not for the header and end keyword of the state or property).
fn token_level(tok: SyntaxToken<'_>) -> usize {
    use SyntaxKind::*;
    let mut level = 0;
    let mut child_kind = None;
    for anc in tok.ancestors() {
        match anc.kind() {
            BLOCK => level += 1,
            STATE | PROPERTY
                if matches!(child_kind, Some(FUNCTION | EVENT | VAR_DECL | PROPERTY | IMPORT)) =>
            {
                level += 1;
            }
            _ => {}
        }
        child_kind = Some(anc.kind());
    }
    level
}

fn indent_levels(lines: &[Line<'_>]) -> Vec<usize> {
    let mut levels: Vec<usize> = lines.iter().map(|l| l.first().map_or(0, token_level)).collect();
    // Continuation lines are one level deeper than the line they continue.
    for i in 1..lines.len() {
        if lines[i].continues() {
            let mut start = i;
            while start > 0 && lines[start].continues() {
                start -= 1;
            }
            levels[i] = levels[start] + 1;
        }
    }
    // Comment lines take the level of what they document: a `{ doc }` the
    // level of the declaration above it, other comments the level of the
    // next code line (one deeper before a closing keyword: the comment is the
    // last line of the block).
    for i in (0..lines.len()).rev() {
        if !lines[i].is_comment_only() || lines[i].continues() {
            continue;
        }
        let is_doc = lines[i].first().is_some_and(|t| t.kind() == SyntaxKind::DOC_COMMENT);
        levels[i] = if is_doc {
            (0..i)
                .rev()
                .find(|&j| !lines[j].is_blank() && !lines[j].is_comment_only())
                // The first line of a declaration continued with `\`.
                .map(|mut j| {
                    while j > 0 && lines[j].continues() {
                        j -= 1;
                    }
                    j
                })
                .map_or(0, |j| levels[j])
        } else {
            match (i + 1..lines.len()).find(|&j| !lines[j].is_blank()) {
                Some(j) => {
                    let next = lines[j].first().expect("non blank");
                    if !lines[j].is_comment_only() && is_closer(next.kind()) {
                        levels[j] + 1
                    } else {
                        levels[j]
                    }
                }
                None => 0,
            }
        };
    }
    levels
}

/// Blank lines before line `i` are kept unless they open or close a block.
fn keep_blank_lines(lines: &[Line<'_>], levels: &[usize], i: usize) -> bool {
    let Some(mut prev) = (0..i).rev().find(|&j| !lines[j].is_blank()) else { return false };
    // The first line of a statement continued with `\`.
    while prev > 0 && lines[prev].continues() {
        prev -= 1;
    }
    let next_first = lines[i].first().expect("non blank");
    let closes = !lines[i].is_comment_only() && is_closer(next_first.kind());
    let opens = levels[i] > levels[prev] && !lines[prev].is_comment_only();
    !closes && !opens
}

/// The indentation of `level`.
fn indentation(level: usize, options: &FormatOptions) -> String {
    match options.indent {
        Indent::Tabs => "\t".repeat(level),
        Indent::Spaces(n) => " ".repeat(level * n as usize),
    }
}

/// The display width of a line, tabs counting as one indentation level.
fn width(s: &str, options: &FormatOptions) -> usize {
    let tab = match options.indent {
        Indent::Spaces(n) => n as usize,
        Indent::Tabs => 4,
    };
    s.chars().map(|c| if c == '\t' { tab } else { 1 }).sum()
}

fn write_line(
    out: &mut String,
    line: &Line<'_>,
    level: usize,
    options: &FormatOptions,
    newline: &str,
) {
    let mut buf = indentation(level, options);
    if let Some(split) = split_attribute(line, &buf, options, newline) {
        out.push_str(&split);
        return;
    }
    // Where the line may break when too long: after commas, before `&&`
    // and `||`.
    let mut breaks = Vec::new();
    write_tokens(&mut buf, line, options, &mut breaks);
    let breakable = !line
        .tokens
        .iter()
        .any(|t| matches!(t.kind(), SyntaxKind::LINE_COMMENT | SyntaxKind::LINE_CONTINUATION));
    if options.max_width == 0 || !breakable || width(&buf, options) <= options.max_width {
        out.push_str(&buf);
        return;
    }
    // Like rustfmt: lines over `max_width` break, here with `\`, and the
    // continuations are one level deeper.
    // A line continuing another is already one level deeper.
    let continuation = indentation(if line.continues() { level } else { level + 1 }, options);
    let mut start = 0;
    let mut prefix_width = 0;
    loop {
        let fits =
            |p: usize| prefix_width + width(&buf[start..p], options) + 2 <= options.max_width;
        if prefix_width + width(&buf[start..], options) <= options.max_width {
            break;
        }
        let candidates = || breaks.iter().copied().filter(|&p| p > start && p < buf.len());
        let Some(at) = candidates().rfind(|&p| fits(p)).or_else(|| candidates().next()) else {
            break;
        };
        out.push_str(buf[start..at].trim_end());
        out.push_str(" \\");
        out.push_str(newline);
        out.push_str(&continuation);
        start = at + (buf[at..].len() - buf[at..].trim_start().len());
        prefix_width = width(&continuation, options);
    }
    out.push_str(&buf[start..]);
}

/// The parts of a normalized attribute with arguments:
/// `; #[allow(a, b)] why` → (`; #[allow(`, [`a`, `b`], `why`).
fn attribute_parts(attr: &str) -> Option<(&str, Vec<&str>, &str)> {
    let open = attr.find('(')?;
    let close = attr.find(")]")?;
    let args = attr[open + 1..close].split(", ").filter(|a| !a.is_empty()).collect();
    Some((&attr[..=open], args, attr[close + 2..].trim()))
}

/// An attribute alone on its line over `max_width`: one attribute per line,
/// its arguments spread over them, the reason on the last one.
fn split_attribute(
    line: &Line<'_>,
    indent: &str,
    options: &FormatOptions,
    newline: &str,
) -> Option<String> {
    if options.max_width == 0 || !line.is_comment_only() {
        return None;
    }
    let comment = line.first()?;
    let attr = papyrus_parser::attrs::normalize(comment.text())?;
    if width(indent, options) + width(&attr, options) <= options.max_width {
        return None;
    }
    let (prefix, args, rest) = attribute_parts(&attr)?;
    let fits = |chunk: &[&str], last: bool| {
        let reason = if last && !rest.is_empty() { rest.len() + 1 } else { 0 };
        width(indent, options) + prefix.len() + chunk.join(", ").len() + 2 + reason
            <= options.max_width
    };
    let mut chunks: Vec<Vec<&str>> = vec![Vec::new()];
    for arg in args {
        let current = chunks.last_mut().expect("one chunk");
        current.push(arg);
        if current.len() > 1 && !fits(current, false) {
            current.pop();
            chunks.push(vec![arg]);
        }
    }
    if chunks.len() < 2 {
        return None;
    }
    let count = chunks.len();
    let lines: Vec<String> = chunks
        .iter()
        .enumerate()
        .map(|(i, chunk)| {
            let reason =
                if i + 1 == count && !rest.is_empty() { format!(" {rest}") } else { String::new() };
            format!("{indent}{prefix}{})]{reason}", chunk.join(", "))
        })
        .collect();
    Some(lines.join(newline))
}

fn write_tokens(
    out: &mut String,
    line: &Line<'_>,
    options: &FormatOptions,
    breaks: &mut Vec<usize>,
) {
    let mut prev: Option<SyntaxToken<'_>> = None;
    let mut pending_ws: Option<&str> = None;
    for tok in &line.tokens {
        let kind = tok.kind();
        if kind == SyntaxKind::WHITESPACE {
            pending_ws = Some(tok.text());
            continue;
        }
        if let Some(p) = prev {
            if kind == SyntaxKind::LINE_COMMENT || kind == SyntaxKind::LINE_CONTINUATION {
                // Keep the alignment of trailing comments.
                match pending_ws {
                    Some(ws) if !ws.is_empty() && kind == SyntaxKind::LINE_COMMENT => {
                        out.push_str(ws)
                    }
                    _ => out.push(' '),
                }
            } else {
                if matches!(kind, SyntaxKind::AMP2 | SyntaxKind::PIPE2) {
                    breaks.push(out.len());
                }
                if needs_space(p, *tok) {
                    out.push(' ');
                }
            }
        }
        pending_ws = None;
        match kind {
            // The token includes the line break, which the caller adds back.
            SyntaxKind::LINE_CONTINUATION => out.push('\\'),
            SyntaxKind::LINE_COMMENT => match papyrus_parser::attrs::normalize(tok.text()) {
                Some(attr) => out.push_str(&attr),
                None => out.push_str(tok.text()),
            },
            _ if kind.is_keyword() => out.push_str(&options.keyword_spelling(kind, tok.text())),
            SyntaxKind::IDENT => out.push_str(&options.name_spelling(*tok)),
            _ => out.push_str(tok.text()),
        }
        if kind == SyntaxKind::COMMA {
            breaks.push(out.len());
        }
        prev = Some(*tok);
    }
}

/// Whether one space separates two adjacent tokens of a line.
fn needs_space(prev: SyntaxToken<'_>, next: SyntaxToken<'_>) -> bool {
    use SyntaxKind::*;
    let (p, n) = (prev.kind(), next.kind());
    if matches!(n, COMMA | R_PAREN | R_BRACK | DOT) || matches!(p, L_PAREN | L_BRACK | DOT) {
        return false;
    }
    let is_prefix_op =
        matches!(p, MINUS | BANG) && prev.parent().is_some_and(|n| n.kind() == PREFIX_EXPR);
    if is_prefix_op {
        return false;
    }
    match n {
        // Calls and parameter lists: `Foo(`; grouping keeps the space: `If (`.
        L_PAREN => !matches!(p, IDENT | R_PAREN | R_BRACK),
        // Indexing and array types: `a[0]`, `Int[]`, `New Int[5]`.
        L_BRACK => !(matches!(p, IDENT | R_PAREN | R_BRACK) || p.is_primitive_type()),
        _ => true,
    }
}

/// The `PascalCase` spelling of a keyword.
pub fn pascal_keyword(kind: SyntaxKind) -> Option<&'static str> {
    use SyntaxKind::*;
    Some(match kind {
        AS_KW => "As",
        AUTO_KW => "Auto",
        AUTOREADONLY_KW => "AutoReadOnly",
        BOOL_KW => "Bool",
        ELSE_KW => "Else",
        ELSEIF_KW => "ElseIf",
        ENDEVENT_KW => "EndEvent",
        ENDFUNCTION_KW => "EndFunction",
        ENDIF_KW => "EndIf",
        ENDPROPERTY_KW => "EndProperty",
        ENDSTATE_KW => "EndState",
        ENDWHILE_KW => "EndWhile",
        EVENT_KW => "Event",
        EXTENDS_KW => "Extends",
        FALSE_KW => "False",
        FLOAT_KW => "Float",
        FUNCTION_KW => "Function",
        GLOBAL_KW => "Global",
        IF_KW => "If",
        IMPORT_KW => "Import",
        INT_KW => "Int",
        NATIVE_KW => "Native",
        NEW_KW => "New",
        NONE_KW => "None",
        PARENT_KW => "Parent",
        PROPERTY_KW => "Property",
        RETURN_KW => "Return",
        SCRIPTNAME_KW => "ScriptName",
        SELF_KW => "Self",
        STATE_KW => "State",
        STRING_KW => "String",
        TRUE_KW => "True",
        WHILE_KW => "While",
        _ => return None,
    })
}

/// Checks that `formatted` parses and has the same significant tokens and
/// comments as `original` (keywords and names compared ignoring case).
fn verify(original: &str, formatted: &str) -> Result<(), FormatError> {
    let after = papyrus_parser::parse(formatted);
    if let Some(e) = after.errors().first() {
        return Err(FormatError::Unsafe(format!("syntax error {}", e.message())));
    }
    let tokens = |text: &str| -> Vec<(SyntaxKind, String)> {
        papyrus_lexer::Lexer::new(text)
            .map(|t| (SyntaxKind::from_token(t.kind), t))
            .filter(|(k, _)| {
                !matches!(
                    k,
                    SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE | SyntaxKind::LINE_CONTINUATION
                )
            })
            .flat_map(|(k, t)| {
                let attr = (k == SyntaxKind::LINE_COMMENT)
                    .then(|| papyrus_parser::attrs::normalize(t.text))
                    .flatten();
                match attr {
                    // An attribute: one entry per argument, as long ones are
                    // split over several lines.
                    Some(attr) => match attribute_parts(&attr) {
                        Some((prefix, args, rest)) => args
                            .iter()
                            .map(|a| format!("{prefix}{a})]"))
                            .chain((!rest.is_empty()).then(|| rest.to_owned()))
                            .map(|s| (k, s))
                            .collect(),
                        None => vec![(k, attr)],
                    },
                    None if k.is_keyword() || k == SyntaxKind::IDENT => {
                        vec![(k, t.text.to_ascii_lowercase())]
                    }
                    None => vec![(k, t.text.to_owned())],
                }
            })
            .collect()
    };
    let (a, b) = (tokens(original), tokens(formatted));
    if a != b {
        let at = a.iter().zip(&b).position(|(x, y)| x != y).unwrap_or(a.len().min(b.len()));
        return Err(FormatError::Unsafe(format!("token {at} changed")));
    }
    // Statements must stay on their lines: compare the line structure too.
    let count = |text: &str| papyrus_parser::parse(text).syntax_node().descendants().count();
    if count(original) != count(formatted) {
        return Err(FormatError::Unsafe("the syntax tree changed".to_owned()));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
