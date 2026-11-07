//! Per-file summaries of declarations.
//!
//! A [`ScriptIndex`] records everything other files need to know about a script
//! (its name, parent, properties, functions, ...) without keeping its text or
//! syntax tree alive. All names are [`Name`]s, so a fully indexed workspace of
//! thousands of scripts stays small.

use std::fmt::{self, Write as _};

use papyrus_intern::{Name, Symbol};
use papyrus_parser::{
    SyntaxKind, SyntaxNode, SyntaxToken, TextRange,
    ast::{self, AstNode},
};

/// A primitive or object base type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BaseType {
    /// `Bool`
    Bool,
    /// `Int`
    Int,
    /// `Float`
    Float,
    /// `String`
    String,
    /// A script type, e.g. `Actor`.
    Object(Name),
}

/// A type as written in a declaration: a base type, possibly an array.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TypeRef {
    /// The element type.
    pub base: BaseType,
    /// `true` for `Base[]`.
    pub array: bool,
}

impl TypeRef {
    /// A non-array type.
    pub fn scalar(base: BaseType) -> TypeRef {
        TypeRef { base, array: false }
    }

    /// The `Int` type.
    pub const INT: TypeRef = TypeRef { base: BaseType::Int, array: false };
    /// The `Float` type.
    pub const FLOAT: TypeRef = TypeRef { base: BaseType::Float, array: false };
    /// The `Bool` type.
    pub const BOOL: TypeRef = TypeRef { base: BaseType::Bool, array: false };
    /// The `String` type.
    pub const STRING: TypeRef = TypeRef { base: BaseType::String, array: false };

    /// Converts a syntax type.
    pub fn from_ast(ty: ast::Type<'_>) -> Option<TypeRef> {
        let base = ty.base()?;
        let base = match base.kind() {
            SyntaxKind::BOOL_KW => BaseType::Bool,
            SyntaxKind::INT_KW => BaseType::Int,
            SyntaxKind::FLOAT_KW => BaseType::Float,
            SyntaxKind::STRING_KW => BaseType::String,
            _ => BaseType::Object(Name::new(base.text())),
        };
        Some(TypeRef { base, array: ty.is_array() })
    }

    /// The script name of an object (non-array) type.
    pub fn object(self) -> Option<Name> {
        match self.base {
            BaseType::Object(name) if !self.array => Some(name),
            _ => None,
        }
    }

    /// The element type of an array type.
    pub fn element(self) -> Option<TypeRef> {
        self.array.then_some(TypeRef { base: self.base, array: false })
    }
}

impl fmt::Display for TypeRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.base {
            BaseType::Bool => f.write_str("Bool")?,
            BaseType::Int => f.write_str("Int")?,
            BaseType::Float => f.write_str("Float")?,
            BaseType::String => f.write_str("String")?,
            BaseType::Object(name) => f.write_str(name.as_str())?,
        }
        if self.array {
            f.write_str("[]")?;
        }
        Ok(())
    }
}

/// What kind of declaration a [`Member`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MemberKind {
    /// `Function`
    Function,
    /// `Event`
    Event,
    /// `Property`
    Property,
    /// A script variable.
    Variable,
    /// `State`
    State,
}

/// The language flags (keywords) of a declaration, as a bit set.
///
/// User flags such as `Hidden` come from the flags file and are kept by name,
/// see [`Member::user_flags`] and [`crate::flags`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Flags(u8);

impl Flags {
    /// `Global`
    pub const GLOBAL: Flags = Flags(1);
    /// `Native`
    pub const NATIVE: Flags = Flags(1 << 1);
    /// `Auto` (properties and states)
    pub const AUTO: Flags = Flags(1 << 2);
    /// `AutoReadOnly`
    pub const AUTO_READONLY: Flags = Flags(1 << 3);

    const NAMES: [(Flags, &'static str); 4] = [
        (Flags::AUTO, "Auto"),
        (Flags::AUTO_READONLY, "AutoReadOnly"),
        (Flags::GLOBAL, "Global"),
        (Flags::NATIVE, "Native"),
    ];

    /// Returns `true` if all flags of `other` are set.
    pub fn contains(self, other: Flags) -> bool {
        self.0 & other.0 == other.0
    }

    /// Parses a known flag name, ignoring case.
    pub fn from_name(name: &str) -> Option<Flags> {
        Flags::NAMES.iter().find(|(_, n)| n.eq_ignore_ascii_case(name)).map(|&(f, _)| f)
    }

    fn insert(&mut self, other: Flags) {
        self.0 |= other.0;
    }

    /// Splits flags into language flags and user flag names.
    fn collect<'a>(flags: impl Iterator<Item = ast::Flag<'a>>) -> (Flags, Box<[Name]>) {
        let mut out = Flags::default();
        let mut user = Vec::new();
        for flag in flags {
            match Flags::from_name(flag.text()) {
                Some(f) => out.insert(f),
                None => user.push(Name::new(flag.text())),
            }
        }
        (out, user.into_boxed_slice())
    }
}

/// Renders language and user flags as Papyrus source, e.g. `" Global Native"`.
pub fn render_flags(out: &mut String, flags: Flags, user: &[Name]) {
    for (flag, name) in Flags::NAMES {
        if flags.contains(flag) {
            out.push(' ');
            out.push_str(name);
        }
    }
    for name in user {
        out.push(' ');
        out.push_str(name.as_str());
    }
}

/// A parameter of a function or event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParamInfo {
    /// The parameter type.
    pub ty: Option<TypeRef>,
    /// The parameter name.
    pub name: Name,
    /// The default value, as written in the source.
    pub default: Option<Box<str>>,
}

/// A script level declaration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    /// The declaration kind.
    pub kind: MemberKind,
    /// The declared name.
    pub name: Name,
    /// Range of the name identifier.
    pub name_range: TextRange,
    /// Range of the whole declaration.
    pub full_range: TextRange,
    /// Return type, property type or variable type.
    pub ty: Option<TypeRef>,
    /// Parameters of functions and events.
    pub params: Box<[ParamInfo]>,
    /// Language flags.
    pub flags: Flags,
    /// User flags, from the flags file.
    pub user_flags: Box<[Name]>,
    /// Initial value of properties and variables, as written in the source.
    pub default: Option<Box<str>>,
    /// The state declaring this function or event, `None` for the empty state.
    pub state: Option<Name>,
    /// For property accessors: the property name.
    pub property: Option<Name>,
    /// Ranges of the documentation comments.
    pub docs: Box<[TextRange]>,
}

impl Member {
    /// Renders the declaration header as Papyrus source.
    pub fn signature(&self) -> String {
        let mut out = String::new();
        match self.kind {
            MemberKind::Function | MemberKind::Event => {
                if let Some(ty) = self.ty {
                    let _ = write!(out, "{ty} ");
                }
                out.push_str(if self.kind == MemberKind::Function {
                    "Function "
                } else {
                    "Event "
                });
                out.push_str(self.name.as_str());
                out.push('(');
                for (i, p) in self.params.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    render_param(&mut out, p);
                }
                out.push(')');
                render_flags(&mut out, self.flags, &self.user_flags);
            }
            MemberKind::Property | MemberKind::Variable => {
                if let Some(ty) = self.ty {
                    let _ = write!(out, "{ty} ");
                }
                if self.kind == MemberKind::Property {
                    out.push_str("Property ");
                }
                out.push_str(self.name.as_str());
                if let Some(default) = &self.default {
                    let _ = write!(out, " = {default}");
                }
                render_flags(&mut out, self.flags, &self.user_flags);
            }
            MemberKind::State => {
                if self.flags.contains(Flags::AUTO) {
                    out.push_str("Auto ");
                }
                out.push_str("State ");
                out.push_str(self.name.as_str());
            }
        }
        out
    }

    /// Returns `true` for callable members.
    pub fn is_callable(&self) -> bool {
        matches!(self.kind, MemberKind::Function | MemberKind::Event)
    }

    /// Returns `true` for `Global` functions.
    pub fn is_global(&self) -> bool {
        self.flags.contains(Flags::GLOBAL)
    }
}

/// Renders a parameter as Papyrus source.
pub fn render_param(out: &mut String, p: &ParamInfo) {
    if let Some(ty) = p.ty {
        let _ = write!(out, "{ty} ");
    }
    out.push_str(p.name.as_str());
    if let Some(default) = &p.default {
        let _ = write!(out, " = {default}");
    }
}

/// The summary of one script file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScriptIndex {
    /// The declared script name.
    pub name: Option<Name>,
    /// Range of the script name identifier.
    pub name_range: Option<TextRange>,
    /// The parent script.
    pub extends: Option<Name>,
    /// Range of the parent script name.
    pub extends_range: Option<TextRange>,
    /// Language flags of the header (Native).
    pub flags: Flags,
    /// User flags of the header, e.g. Hidden.
    pub user_flags: Box<[Name]>,
    /// Imported scripts.
    pub imports: Box<[Name]>,
    /// All declarations, in source order, including functions of states and
    /// property accessors.
    pub members: Box<[Member]>,
    /// Ranges of the script documentation comments.
    pub docs: Box<[TextRange]>,
    /// The identifiers, keywords and identifier-like strings of the text, as
    /// sorted [`Name::key`]s:
    /// see [`ScriptIndex::mentions`].
    pub words: Box<[Symbol]>,
}

impl ScriptIndex {
    /// Summarizes a parsed file.
    pub fn build(file: ast::SourceFile<'_>) -> ScriptIndex {
        let mut index = ScriptIndex::default();
        let mut imports = Vec::new();
        let mut members = Vec::new();

        for item in file.items() {
            collect_item(&mut index, &mut imports, &mut members, item, None);
        }

        index.imports = imports.into_boxed_slice();
        index.members = members.into_boxed_slice();
        let mut words: Vec<Symbol> = file
            .syntax()
            .descendant_tokens()
            .filter_map(|t| {
                let text = match t.kind() {
                    SyntaxKind::IDENT => t.text(),
                    k if k.is_keyword() => t.text(),
                    // Strings name states: `GoToState("Busy")`.
                    SyntaxKind::STRING => {
                        let inner = t.text().trim_matches('"');
                        let word = !inner.is_empty()
                            && inner.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
                        word.then_some(inner)?
                    }
                    _ => return None,
                };
                Some(Name::new(text).key())
            })
            .collect();
        words.sort_unstable();
        words.dedup();
        index.words = words.into_boxed_slice();
        index
    }

    /// Whether the file may refer to `name`: an identifier, keyword or string
    /// of its text is spelled so. Searches skip the other files without
    /// reading them.
    pub fn mentions(&self, name: Name) -> bool {
        self.words.binary_search(&name.key()).is_ok()
    }

    /// Script name as written, or an empty string.
    pub fn name_str(&self) -> &'static str {
        self.name.map_or("", |n| n.as_str())
    }

    /// Members declared outside of any state, in source order.
    pub fn empty_state_members(&self) -> impl Iterator<Item = (usize, &Member)> {
        self.members.iter().enumerate().filter(|(_, m)| m.state.is_none() && m.property.is_none())
    }

    /// Finds a member by name, preferring declarations outside of states.
    pub fn find(&self, name: Name, filter: impl Fn(&Member) -> bool) -> Option<usize> {
        let mut fallback = None;
        for (i, m) in self.members.iter().enumerate() {
            if m.name == name && m.property.is_none() && filter(m) {
                if m.state.is_none() {
                    return Some(i);
                }
                fallback.get_or_insert(i);
            }
        }
        fallback
    }
}

fn collect_item(
    index: &mut ScriptIndex,
    imports: &mut Vec<Name>,
    members: &mut Vec<Member>,
    item: ast::Item<'_>,
    state: Option<Name>,
) {
    match item {
        ast::Item::Header(header) => {
            if index.name.is_some() {
                return;
            }
            if let Some(name) = header.name() {
                index.name = Some(Name::new(name.text()));
                index.name_range = Some(name.syntax().text_range());
            }
            if let Some(ext) = header.extends() {
                index.extends = Some(Name::new(ext.text()));
                index.extends_range = Some(ext.syntax().text_range());
            }
            (index.flags, index.user_flags) = Flags::collect(header.flags());
            index.docs = doc_ranges(header.syntax()).into_boxed_slice();
        }
        ast::Item::Import(import) => {
            if let Some(name) = import.name_ref() {
                imports.push(Name::new(name.text()));
            }
        }
        ast::Item::VarDecl(var) => {
            let Some(name) = var.name() else { return };
            let (flags, user_flags) = Flags::collect(var.flags());
            members.push(Member {
                kind: MemberKind::Variable,
                name: Name::new(name.text()),
                name_range: name.syntax().text_range(),
                full_range: var.syntax().trimmed_range(),
                ty: var.ty().and_then(TypeRef::from_ast),
                params: Box::new([]),
                flags,
                user_flags,
                default: var.initializer().map(|e| expr_text(e.syntax())),
                state,
                property: None,
                docs: doc_ranges(var.syntax()).into_boxed_slice(),
            });
        }
        ast::Item::Property(prop) => {
            let Some(name) = prop.name() else { return };
            let (flags, user_flags) = Flags::collect(prop.flags());
            let prop_name = Name::new(name.text());
            members.push(Member {
                kind: MemberKind::Property,
                name: prop_name,
                name_range: name.syntax().text_range(),
                full_range: prop.syntax().trimmed_range(),
                ty: prop.ty().and_then(TypeRef::from_ast),
                params: Box::new([]),
                flags,
                user_flags,
                default: prop.initializer().map(|e| expr_text(e.syntax())),
                state,
                property: None,
                docs: doc_ranges(prop.syntax()).into_boxed_slice(),
            });
            for f in prop.functions() {
                if let Some(mut m) = function_member(f, state) {
                    m.property = Some(prop_name);
                    members.push(m);
                }
            }
        }
        ast::Item::State(st) => {
            let Some(name) = st.name() else { return };
            let state_name = Name::new(name.text());
            let mut flags = Flags::default();
            if st.is_auto() {
                flags.insert(Flags::AUTO);
            }
            members.push(Member {
                kind: MemberKind::State,
                name: state_name,
                name_range: name.syntax().text_range(),
                full_range: st.syntax().trimmed_range(),
                ty: None,
                params: Box::new([]),
                flags,
                user_flags: Box::new([]),
                default: None,
                state: None,
                property: None,
                docs: doc_ranges(st.syntax()).into_boxed_slice(),
            });
            for inner in st.items() {
                collect_item(index, imports, members, inner, Some(state_name));
            }
        }
        ast::Item::Function(f) => members.extend(function_member(f, state)),
        ast::Item::Event(e) => {
            let Some(name) = e.name() else { return };
            let (flags, user_flags) = Flags::collect(e.flags());
            members.push(Member {
                kind: MemberKind::Event,
                name: Name::new(name.text()),
                name_range: name.syntax().text_range(),
                full_range: e.syntax().trimmed_range(),
                ty: None,
                params: params(e.param_list()),
                flags,
                user_flags,
                default: None,
                state,
                property: None,
                docs: doc_ranges(e.syntax()).into_boxed_slice(),
            });
        }
    }
}

fn function_member(f: ast::Function<'_>, state: Option<Name>) -> Option<Member> {
    let name = f.name()?;
    let (flags, user_flags) = Flags::collect(f.flags());
    Some(Member {
        kind: MemberKind::Function,
        name: Name::new(name.text()),
        name_range: name.syntax().text_range(),
        full_range: f.syntax().trimmed_range(),
        ty: f.return_type().and_then(TypeRef::from_ast),
        params: params(f.param_list()),
        flags,
        user_flags,
        default: None,
        state,
        property: None,
        docs: doc_ranges(f.syntax()).into_boxed_slice(),
    })
}

/// Converts a parameter list.
pub fn params(list: Option<ast::ParamList<'_>>) -> Box<[ParamInfo]> {
    let Some(list) = list else { return Box::new([]) };
    list.params()
        .filter_map(|p| {
            Some(ParamInfo {
                ty: p.ty().and_then(TypeRef::from_ast),
                name: Name::new(p.name()?.text()),
                default: p.default_value().map(|e| expr_text(e.syntax())),
            })
        })
        .collect()
}

/// The text of an expression without surrounding trivia, line continuations
/// collapsed.
fn expr_text(node: SyntaxNode<'_>) -> Box<str> {
    let range = node.trimmed_range();
    let text = &node.source()[range];
    if text.contains('\n') {
        text.split_whitespace().collect::<Vec<_>>().join(" ").replace("\\ ", "").into()
    } else {
        text.into()
    }
}

/// Finds the documentation comments of a declaration, in this order:
///
/// - a `{ ... }` comment on the line right after its header line, which is the
///   official Papyrus documentation syntax, shown in the Creation Kit: the
///   summary;
/// - the comment lines directly above it (`;` or `;/ /;` comments, without a
///   blank line in between): the rest, like examples.
pub fn doc_ranges(node: SyntaxNode<'_>) -> Vec<TextRange> {
    let mut out = Vec::new();
    let Some(first) = node.first_token() else { return out };

    // Leading comments.
    let mut leading = Vec::new();
    let mut newlines = 0;
    let mut cur = first.prev_token();
    while let Some(tok) = cur {
        match tok.kind() {
            SyntaxKind::WHITESPACE => {}
            SyntaxKind::NEWLINE => {
                newlines += 1;
                if newlines > 1 {
                    break;
                }
            }
            SyntaxKind::LINE_COMMENT | SyntaxKind::BLOCK_COMMENT | SyntaxKind::DOC_COMMENT => {
                // A comment trailing code on the previous line is not ours.
                if !starts_line(tok) {
                    break;
                }
                newlines = 0;
                leading.push(tok.text_range());
            }
            _ => break,
        }
        cur = tok.prev_token();
    }
    leading.reverse();

    // `{ doc }` after the header line, or at the end of it: the summary, first.
    let mut cur = Some(first);
    while let Some(tok) = cur {
        if tok.kind() == SyntaxKind::NEWLINE {
            break;
        }
        if tok.kind() == SyntaxKind::DOC_COMMENT {
            out.push(tok.text_range());
            out.extend(leading);
            return out;
        }
        cur = tok.next_token();
    }
    let mut cur = cur.and_then(|t| t.next_token());
    while let Some(tok) = cur {
        match tok.kind() {
            SyntaxKind::WHITESPACE => {}
            SyntaxKind::DOC_COMMENT => {
                out.push(tok.text_range());
                break;
            }
            _ => break,
        }
        cur = tok.next_token();
    }
    out.extend(leading);
    out
}

fn starts_line(tok: SyntaxToken<'_>) -> bool {
    let mut cur = tok.prev_token();
    while let Some(t) = cur {
        match t.kind() {
            SyntaxKind::WHITESPACE => cur = t.prev_token(),
            SyntaxKind::NEWLINE => return true,
            _ => return false,
        }
    }
    true
}

/// Turns comment tokens into plain documentation text.
pub fn render_docs(text: &str, ranges: &[TextRange]) -> String {
    hide_test_lines(&doc_comments_markdown(text, ranges))
}

/// The Markdown of documentation comments, as written: with the hidden lines of
/// their examples, for doc tests.
pub fn doc_comments_markdown(text: &str, ranges: &[TextRange]) -> String {
    let lines = doc_comment_lines(text, ranges);
    let lines: Vec<&str> = lines.iter().map(|(_, l)| l.as_str()).collect();
    keep_line_breaks(&lines.join("\n"))
}

/// The lines of the Markdown of documentation comments, each with the line of
/// the script it comes from (from 0).
///
/// `; text` lines are like `/// text` in Rust: one space after `;` is removed,
/// the rest of the indentation is kept, and `;` alone is a blank line.
pub fn doc_comment_lines(text: &str, ranges: &[TextRange]) -> Vec<(usize, String)> {
    let mut out: Vec<(usize, String)> = Vec::new();
    for &range in ranges {
        let start = usize::from(range.start());
        let Some(raw) = text.get(start..usize::from(range.end())) else { continue };
        let first_line = text[..start].matches('\n').count();
        let block = if let Some(inner) = raw.strip_prefix(";/") {
            inner.strip_suffix("/;").unwrap_or(inner)
        } else if let Some(inner) = raw.strip_prefix('{') {
            inner.strip_suffix('}').unwrap_or(inner)
        } else {
            let line = raw.trim_start_matches(';');
            let line = line.strip_prefix(' ').unwrap_or(line).trim_end();
            out.push((first_line, line.to_owned()));
            continue;
        };
        // A paragraph of its own: `{Summary.}` then `; Text.` lines.
        if out.last().is_some_and(|(_, l)| !l.is_empty()) {
            out.push((first_line, String::new()));
        }
        let lines: Vec<&str> = block.lines().collect();
        // Remove the common indentation of multi-line comments.
        let indent = lines
            .iter()
            .skip(1)
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.len() - l.trim_start().len())
            .min()
            .unwrap_or(0);
        for (i, line) in lines.iter().enumerate() {
            let line = if i == 0 {
                line.trim()
            } else {
                line.get(indent..).unwrap_or(line.trim_start()).trim_end()
            };
            if i == 0 && line.is_empty() {
                continue;
            }
            out.push((first_line + i, line.to_owned()));
        }
        out.push((first_line, String::new()));
    }
    // Like `trim`: no blank lines around.
    let first = out.iter().position(|(_, l)| !l.is_empty()).unwrap_or(out.len());
    let last = out.iter().rposition(|(_, l)| !l.is_empty()).map_or(first, |i| i + 1);
    out.truncate(last);
    out.drain(..first);
    out
}

/// Documentation as readers see it, like rustdoc: the lines of `papyrus`
/// examples starting with `# ` are for doc tests only (declarations the example
/// needs), and the attributes of the examples (`papyrus,no_run`) are dropped.
pub fn hide_test_lines(markdown: &str) -> String {
    let mut out = Vec::new();
    let (mut in_papyrus, mut in_other) = (false, false);
    for line in markdown.lines() {
        let trimmed = line.trim_start();
        if let Some(info) = trimmed.strip_prefix("```") {
            if in_papyrus || in_other {
                (in_papyrus, in_other) = (false, false);
                out.push(line.to_owned());
            } else if info
                .split(',')
                .next()
                .is_some_and(|l| l.trim().eq_ignore_ascii_case("papyrus"))
            {
                in_papyrus = true;
                let indent = &line[..line.len() - trimmed.len()];
                out.push(format!("{indent}```papyrus"));
            } else {
                in_other = true;
                out.push(line.to_owned());
            }
            continue;
        }
        if in_papyrus && (trimmed == "#" || trimmed.starts_with("# ")) {
            continue;
        }
        out.push(line.to_owned());
    }
    out.join("\n")
}

/// Markdown joins the lines of a paragraph, which most comments of scripts do
/// not expect: `a\nb` becomes `a  \nb`, a line break. Not in code blocks, where
/// lines are kept and spaces would be text.
fn keep_line_breaks(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_code = false;
    let mut lines = text.lines().peekable();
    while let Some(line) = lines.next() {
        out.push_str(line);
        let Some(next) = lines.peek() else { break };
        let fence = line.trim_start().starts_with("```");
        if fence {
            in_code = !in_code;
        }
        let next_fence = next.trim_start().starts_with("```");
        if !in_code && !fence && !next_fence && !line.is_empty() && !next.trim().is_empty() {
            out.push_str("  ");
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index(src: &str) -> (ScriptIndex, String) {
        let parse = papyrus_parser::parse(src);
        (ScriptIndex::build(parse.source_file()), src.to_owned())
    }

    #[test]
    fn summarizes_declarations() {
        let (idx, _) = index(
            "ScriptName Foo extends Quest Hidden\nImport Utility\nInt Property Count = 5 Auto\nInt[] values\nInt Function Add(Int a, Int b = 2) Global\n  Return a + b\nEndFunction\nAuto State Busy\n  Event OnInit()\n  EndEvent\nEndState\n",
        );
        assert_eq!(idx.name_str(), "Foo");
        assert_eq!(idx.extends.unwrap().as_str(), "Quest");
        assert_eq!(idx.user_flags[0].as_str(), "Hidden");
        assert_eq!(idx.imports.len(), 1);
        let sigs: Vec<_> = idx.members.iter().map(|m| m.signature()).collect();
        assert_eq!(
            sigs,
            [
                "Int Property Count = 5 Auto",
                "Int[] values",
                "Int Function Add(Int a, Int b = 2) Global",
                "Auto State Busy",
                "Event OnInit()",
            ]
        );
        assert_eq!(idx.members[4].state.unwrap().as_str(), "Busy");
    }

    #[test]
    fn docs() {
        let src = "ScriptName Foo\n\n; Adds numbers.\n; Really.\nInt Function Add(Int a) Global Native\n{ Official docs }\n\n; not a doc\n\nFunction Bar()\nEndFunction\n";
        let (idx, text) = index(src);
        assert_eq!(
            render_docs(&text, &idx.members[0].docs),
            "Official docs\n\nAdds numbers.  \nReally."
        );
        assert_eq!(render_docs(&text, &idx.members[1].docs), "");
        // Like `///` in Rust: one space after `;` is removed, not the rest.
        let src = "ScriptName Foo\n; Text.\n;\n; ```papyrus\n; If a\n;     F()\n; EndIf\n; ```\nFunction F()\nEndFunction\n";
        let (idx, text) = index(src);
        assert_eq!(
            render_docs(&text, &idx.members[0].docs),
            "Text.\n\n```papyrus\nIf a\n    F()\nEndIf\n```"
        );
    }

    #[test]
    fn block_comment_docs() {
        let src = ";/ script docs\n   second line /;\nScriptName Foo\n";
        let (idx, text) = index(src);
        assert_eq!(render_docs(&text, &idx.docs), "script docs  \nsecond line");
    }

    #[test]
    fn docs_keep_their_line_breaks() {
        // Paragraphs, lists and code blocks as written; no space before a blank
        // line, at the end, or next to a fence.
        let src = "ScriptName Foo\n{first\nsecond\n\n- a\n- b\ntext\n```\ncode\nmore\n```\nlast}\n";
        let (idx, text) = index(src);
        assert_eq!(
            render_docs(&text, &idx.docs),
            "first  \nsecond\n\n- a  \n- b  \ntext\n```\ncode\nmore\n```\nlast"
        );
    }
}
