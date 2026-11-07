//! Allowing lints with attributes in comments, like in Rust (see
//! [`papyrus_parser::attrs`]):
//!
//! ```papyrus
//! ; #![allow(unchecked_cast)]            ; the whole file: before the code
//! ScriptName T
//!
//! ; #[allow(unused_local)]               ; the next declaration or statement
//! Function F()
//!     Int unused = 1
//!     ; #[allow(all)]                    ; this statement
//!     Int other = 2
//! EndFunction
//! ```
//!
//! The arguments are lints (`unused_local` or `unused-local`), groups or
//! `all`; without any, every lint. Unknown names are reported (`unknown-lint`):
//! compiler errors such as `invalid_cast` cannot be allowed. So are
//! misplaced attributes (`misplaced-attribute`): after code on their line, or
//! `#![...]` below the code, with a quick fix moving them.
//!
//! Inside an attribute, completion offers the lints, groups and attributes.

use papyrus_ide::{
    Diagnostic, FileId, NavFile, NavTarget, TextRange, TextSize, Workspace,
    assists::{Assist, AssistKind, TextEdit},
    index::{Flags, Member},
};
use papyrus_parser::attrs::{Misplacement, attributes, misplaced};

/// The code of the problem of an attribute in the wrong place.
const MISPLACED: &str = "misplaced-attribute";

use crate::{Group, LINTS, is_known_name};

/// An attribute: its completion detail, and its hover with an example.
struct AttributeDoc {
    path: &'static str,
    /// The completion detail.
    detail: &'static str,
    doc: &'static str,
    /// Papyrus code using it.
    example: &'static str,
}

/// The attributes.
const ATTRIBUTES: &[AttributeDoc] = &[
    AttributeDoc {
        path: "allow",
        detail: "allow lints: `allow(unused_local, style)`",
        doc: "Allows the listed lints (lints, groups or `all`; none: every lint) in the \
              next declaration or statement, all its lines; in the whole file as \
              `#![allow]`, before the code. Text after `]` is a reason, ignored. \
              Attributes stack: write one per line for long lists.",
        example: "; #![allow(style)] the whole file\n\
                  ScriptName MyQuest\n\n\
                  ; #[allow(unused_local)] the whole function\n\
                  Function F()\n\
                  \x20   Int unused = 1\n\
                  \x20   ; #[allow(unused_local)] this statement\n\
                  \x20   Int b = 2\n\
                  EndFunction",
    },
    AttributeDoc {
        path: "fmt::skip",
        detail: "keep the code as written by the formatter",
        doc: "Keeps the code as written: the formatter leaves the next declaration or \
              statement alone; the whole file as `#![fmt::skip]`, before the code.",
        example: "; #![fmt::skip] the whole file: not formatted\n\
                  ScriptName MyQuest\n\n\
                  ; #[fmt::skip] aligned by hand\n\
                  Float[] matrix = New Float[9]",
    },
    AttributeDoc {
        path: "test",
        detail: "a test: `papyrus_analyzer test` runs it, it fails on errors or `False`",
        doc: "A test, run by `papyrus_analyzer test`, ▶ Run Test or ⚙ Debug on a new object \
              of the script, after its `OnInit`. It fails when a `PapyrusAnalyzer.Assert*` \
              fails, when the game would log an error (a call on `None`...) or when it \
              returns `False`.",
        example: "ScriptName MyQuestTests extends MyQuest\n\
                  Import PapyrusAnalyzer\n\n\
                  ; #[test]\n\
                  Function CountsKills()\n\
                  \x20   RecordKill()\n\
                  \x20   AssertIntEq(Kills, 1, \"one kill\")\n\
                  EndFunction",
    },
    AttributeDoc {
        path: "mock",
        detail: "in a test script: this function replaces a native, `mock(Game.GetPlayer)`",
        doc: "In a test script: this function runs instead of the native \
              `mock(Script.Function)` in the tests of the script; its signature must \
              match the native's. With one more parameter than the native, the first \
              one is the object the native is called on.",
        example: "; #[mock(Utility.RandomInt)]\n\
                  Int Function FixedRoll(Int aiMin, Int aiMax)\n\
                  \x20   Return 4\n\
                  EndFunction\n\n\
                  ; #[mock(Actor.GetActorValue)] the actor first\n\
                  Float Function FakeValue(Actor akSelf, String asName)\n\
                  \x20   Return 100.0\n\
                  EndFunction",
    },
];

/// The file of the script `name`.
fn script_file(ws: &Workspace, name: &str) -> Option<FileId> {
    ws.all_scripts().find(|(n, _)| n.as_str().eq_ignore_ascii_case(name)).map(|(_, f)| f)
}

/// The natives the script of `file` declares (not inherited), which
/// `#[mock]` can replace.
fn natives(ws: &Workspace, file: FileId) -> impl Iterator<Item = &Member> {
    ws.file(file)
        .index
        .members
        .iter()
        .filter(|m| m.is_callable() && m.state.is_none() && m.flags.contains(Flags::NATIVE))
}

/// The native `function` of `script`, with its file.
fn native<'a>(ws: &'a Workspace, script: &str, function: &str) -> Option<(FileId, &'a Member)> {
    let file = script_file(ws, script)?;
    let m = natives(ws, file).find(|m| m.name.as_str().eq_ignore_ascii_case(function))?;
    Some((file, m))
}

/// Whether the line of `offset` has an attribute, to parse only then.
fn on_attribute_line(text: &str, offset: TextSize) -> bool {
    let at = usize::from(offset).min(text.len());
    let line_start = text[..at].rfind('\n').map_or(0, |i| i + 1);
    let line_end = text[at..].find('\n').map_or(text.len(), |i| at + i);
    text[line_start..line_end].contains("#[") || text[line_start..line_end].contains("#![")
}

/// Why the mock function `mock` cannot replace `native`.
fn mock_mismatch(mock: &Member, native: &Member) -> Option<String> {
    let receiver = mock.params.len() == native.params.len() + 1;
    if mock.params.len() != native.params.len() && !receiver {
        return Some(format!(
            "expected {} parameters (or {} with the object the native is called on first), found {}",
            native.params.len(),
            native.params.len() + 1,
            mock.params.len()
        ));
    }
    if receiver && mock.params[0].ty.is_none_or(|ty| ty.object().is_none()) {
        return Some(format!(
            "the extra first parameter `{}` receives the object the native is called on: it must have an object type",
            mock.params[0].name.as_str()
        ));
    }
    let params = mock.params[usize::from(receiver)..].iter().zip(&native.params);
    for (m, n) in params {
        if m.ty != n.ty {
            let ty = |t: Option<papyrus_ide::index::TypeRef>| {
                t.map_or("?".to_owned(), |t| t.to_string())
            };
            return Some(format!(
                "parameter `{}` is `{}`, expected `{}`",
                m.name.as_str(),
                ty(m.ty),
                ty(n.ty)
            ));
        }
    }
    if mock.ty != native.ty {
        let ty = |t: Option<papyrus_ide::index::TypeRef>| {
            t.map_or("nothing".to_owned(), |t| format!("`{t}`"))
        };
        return Some(format!("returns {}, expected {}", ty(mock.ty), ty(native.ty)));
    }
    None
}

/// The problems of a `#[mock(Script.Function)]` attribute: a bad argument,
/// an unknown native, or a function whose signature does not match it.
fn check_mock(
    ws: &Workspace,
    file: FileId,
    text: &str,
    a: &papyrus_parser::attrs::Attribute<'_>,
    out: &mut Vec<Diagnostic>,
) {
    let path = range_of(text, a.path);
    let [arg] = a.args.as_slice() else {
        out.push(Diagnostic::error(path, "invalid-mock", "expected `mock(Script.Function)`"));
        return;
    };
    let range = range_of(text, arg);
    let Some((script, function)) = arg.split_once('.') else {
        out.push(Diagnostic::error(range, "invalid-mock", "expected `mock(Script.Function)`"));
        return;
    };
    let (script, function) = (script.trim(), function.trim());
    let Some(script_file) = script_file(ws, script) else {
        out.push(Diagnostic::warning(range, "invalid-mock", format!("unknown script `{script}`")));
        return;
    };
    let Some(native) =
        natives(ws, script_file).find(|m| m.name.as_str().eq_ignore_ascii_case(function))
    else {
        out.push(Diagnostic::error(
            range,
            "invalid-mock",
            format!("`{script}` has no native `{function}`: only natives can be mocked"),
        ));
        return;
    };
    let mock = ws
        .file(file)
        .index
        .members
        .iter()
        .find(|m| m.is_callable() && a.target.contains(m.name_range.start()));
    let Some(mock) = mock else {
        out.push(Diagnostic::error(path, "invalid-mock", "`mock` applies to a function"));
        return;
    };
    if let Some(why) = mock_mismatch(mock, native) {
        out.push(Diagnostic::error(
            mock.name_range,
            "invalid-mock",
            format!("`{}` cannot replace `{}`: {why}", mock.name.as_str(), native.signature()),
        ));
    }
}

/// Whether `names` (`None`: every lint) cover the lint `code`.
fn covers(names: Option<&[String]>, code: &str) -> bool {
    let Some(lint) = LINTS.iter().find(|l| l.code == code) else { return false };
    names.is_none_or(|names| {
        names.iter().any(|n| {
            n.eq_ignore_ascii_case(code)
                || n.eq_ignore_ascii_case(lint.group.name())
                || n.eq_ignore_ascii_case("all")
        })
    })
}

/// A lint name of an attribute, `unused_local` as `unused-local`.
fn lint_name(arg: &str) -> String {
    arg.replace('_', "-")
}

/// The lints allowed by the attributes of a file.
#[derive(Default)]
pub(crate) struct Ignores {
    /// The code `#[allow]` attributes apply to, with their lints (`None`:
    /// every lint).
    ranges: Vec<(TextRange, Option<Vec<String>>)>,
}

impl Ignores {
    pub(crate) fn of(root: papyrus_parser::SyntaxNode<'_>) -> Ignores {
        let ranges = attributes(root)
            .into_iter()
            .filter(|a| a.path.eq_ignore_ascii_case("allow"))
            .map(|a| {
                let names = (!a.args.is_empty()).then(|| {
                    a.args.iter().map(|n| lint_name(n)).filter(|n| is_known_name(n)).collect()
                });
                (a.target, names)
            })
            .collect();
        Ignores { ranges }
    }

    /// Whether the attributes allow `d`.
    pub(crate) fn silence(&self, d: &Diagnostic) -> bool {
        self.ranges.iter().any(|(range, names)| {
            range.contains_inclusive(d.range.start()) && covers(names.as_deref(), d.code)
        })
    }
}

/// The range of `part`, a slice of the file text `text`.
fn range_of(text: &str, part: &str) -> TextRange {
    let start = part.as_ptr() as usize - text.as_ptr() as usize;
    TextRange::at(TextSize::new(start as u32), TextSize::of(part))
}

/// Problems of the attributes: unknown attributes and lints, mocks that
/// cannot replace their native.
pub(crate) fn check_attributes(
    ws: &Workspace,
    file: FileId,
    root: papyrus_parser::SyntaxNode<'_>,
) -> Vec<Diagnostic> {
    let text = root.text();
    let mut out = Vec::new();
    for m in misplaced(root) {
        out.push(match m.why {
            Misplacement::AfterCode => Diagnostic::warning(
                m.range,
                MISPLACED,
                "an attribute applies to what follows it, not to code before it on its line: \
                 move it to its own line, above",
            ),
            Misplacement::InnerBelowCode => Diagnostic::error(
                m.range,
                MISPLACED,
                "`#![...]` applies to the whole file: it must come before the code, at the top \
                 of the file",
            ),
        });
    }
    for a in attributes(root) {
        if a.path.eq_ignore_ascii_case("mock") {
            check_mock(ws, file, text, &a, &mut out);
            continue;
        }
        if !ATTRIBUTES.iter().any(|d| a.path.eq_ignore_ascii_case(d.path)) {
            let expected: Vec<String> =
                ATTRIBUTES.iter().map(|d| format!("`{}`", d.path)).collect();
            out.push(Diagnostic::warning(
                range_of(text, a.path),
                "unknown-attribute",
                format!("unknown attribute `{}`: expected {}", a.path, expected.join(" or ")),
            ));
            continue;
        }
        if !a.path.eq_ignore_ascii_case("allow") {
            continue;
        }
        for arg in &a.args {
            let name = lint_name(arg);
            if is_known_name(&name) {
                continue;
            }
            let message = if papyrus_ide::COMPILER_CODES
                .iter()
                .any(|c| c.eq_ignore_ascii_case(&name))
            {
                format!(
                    "`{arg}` is a compile error, not a lint: it cannot be allowed (the compiler rejects the script)"
                )
            } else {
                format!("unknown lint `{arg}`: see `papyrus_analyzer lint --list`")
            };
            out.push(Diagnostic::warning(range_of(text, arg), "unknown-lint", message));
        }
    }
    out
}

/// The hover of the attribute part at `offset` of `file`: the attribute (with
/// the lines it applies to), or the lint or mocked native under the cursor.
pub fn attribute_hover(
    ws: &Workspace,
    file: FileId,
    offset: TextSize,
) -> Option<(TextRange, String)> {
    let text = &*ws.text(file)?;
    // Every hover comes here first: parse only for lines with an attribute.
    if !on_attribute_line(text, offset) {
        return None;
    }
    let parse = papyrus_parser::parse(text);
    let root = parse.syntax_node();
    let at = usize::from(offset);
    let comment = root.descendant_tokens().find(|t| {
        t.kind() == papyrus_parser::SyntaxKind::LINE_COMMENT
            && t.text_range().contains_inclusive(offset)
    })?;
    let attr = attributes(root).into_iter().find(|a| {
        let start = range_of(text, a.path).start();
        comment.text_range().contains(start)
    })?;
    let lines = |r: TextRange| {
        let line = |o: TextSize| text[..usize::from(o)].matches('\n').count() + 1;
        let (first, last) = (line(r.start()), line(r.end()));
        if first == last { format!("line {first}") } else { format!("lines {first}–{last}") }
    };
    let scope = if attr.inner {
        "the whole file".to_owned()
    } else {
        format!(
            "{} (`{}`)",
            lines(attr.target),
            text[attr.target].lines().next().unwrap_or("").trim()
        )
    };
    // A lint argument.
    for arg in &attr.args {
        let range = range_of(text, arg);
        if !range.contains_inclusive(offset) {
            continue;
        }
        let name = lint_name(arg);
        let doc = if attr.path.eq_ignore_ascii_case("mock") {
            let (script, function) = arg.split_once('.')?;
            let (script, function) = (script.trim(), function.trim());
            let (_, native) = native(ws, script, function)?;
            format!(
                "```papyrus\n{}\n```\n*native of `{script}`, replaced in the tests of this script*",
                native.signature()
            )
        } else if let Some(lint) = LINTS.iter().find(|l| l.code.eq_ignore_ascii_case(&name)) {
            format!(
                "```papyrus\n{}\n```\n*lint · {} · {} by default*\n\n{}",
                lint.code.replace('-', "_"),
                lint.group.name(),
                lint.group.default_level(),
                lint.summary
            )
        } else if let Some(group) = Group::ALL.iter().find(|g| g.name().eq_ignore_ascii_case(&name))
        {
            let lints: Vec<String> = LINTS
                .iter()
                .filter(|l| l.group == *group)
                .map(|l| format!("`{}`", l.code.replace('-', "_")))
                .collect();
            format!(
                "```papyrus\n{}\n```\n*lint group · {} by default*\n\n{}",
                group.name(),
                group.default_level(),
                if lints.is_empty() { "No lints yet.".to_owned() } else { lints.join(" · ") }
            )
        } else if name.eq_ignore_ascii_case("all") {
            "```papyrus\nall\n```\nEvery lint.".to_owned()
        } else {
            return None;
        };
        return Some((range, doc));
    }
    // The attribute itself.
    let path_range = range_of(text, attr.path);
    let doc = ATTRIBUTES.iter().find(|d| attr.path.eq_ignore_ascii_case(d.path))?;
    let range = if path_range.contains_inclusive(offset) {
        path_range
    } else {
        // The brackets: the whole attribute.
        let start = text[..usize::from(path_range.start())].rfind('#')?;
        let end = text[at..].find(']').map(|i| at + i + 1)?;
        if at < start || end > usize::from(comment.text_range().end()) {
            return None;
        }
        TextRange::new(TextSize::new(start as u32), TextSize::new(end as u32))
    };
    let syntax = if attr.inner { "#![...]" } else { "#[...]" };
    Some((
        range,
        format!(
            "```papyrus\n{}\n```\n*attribute `{syntax}`*\n\n{}\n\nApplies to {scope}.\n\n# Examples\n\n```papyrus\n{}\n```",
            attr.path, doc.doc, doc.example
        ),
    ))
}

/// The declaration of the script or the native named by the
/// `mock(Script.Function)` attribute at `offset` of `file`.
pub fn attribute_definition(ws: &Workspace, file: FileId, offset: TextSize) -> Option<NavTarget> {
    let text = &*ws.text(file)?;
    if !on_attribute_line(text, offset) {
        return None;
    }
    let parse = papyrus_parser::parse(text);
    let attrs = attributes(parse.syntax_node());
    let arg = attrs
        .iter()
        .filter(|a| a.path.eq_ignore_ascii_case("mock"))
        .flat_map(|a| &a.args)
        .find(|arg| range_of(text, arg).contains_inclusive(offset))?;
    let (script, function) = arg.split_once('.')?;
    let on_script = usize::from(offset) <= usize::from(range_of(text, arg).start()) + script.len();
    let (script, function) = (script.trim(), function.trim());
    let (target, focus_range, full_range) = if on_script {
        let f = script_file(ws, script)?;
        let range = ws.file(f).index.name_range?;
        (f, range, range)
    } else {
        let (f, m) = native(ws, script, function)?;
        (f, m.name_range, m.full_range)
    };
    Some(NavTarget { file: NavFile::Script(target), focus_range, full_range })
}

/// Completion inside an attribute comment at `offset` of `file`: the range
/// to replace, and the items as `(label, detail)`.
pub fn attribute_completions(
    ws: &Workspace,
    file: FileId,
    offset: TextSize,
) -> Option<(TextRange, Vec<(String, String)>)> {
    let text = &*ws.text(file)?;
    let at = usize::from(offset).min(text.len());
    let line_start = text[..at].rfind('\n').map_or(0, |i| i + 1);
    let line = &text[line_start..at];
    // Attributes go alone on their line, not after code.
    let semicolon = line.find(';')?;
    if !line[..semicolon].trim().is_empty() {
        return None;
    }
    let comment = &line[semicolon + 1..];
    let body = comment.trim_start();
    let body = body.strip_prefix("#![").or_else(|| body.strip_prefix("#["))?;
    if body.contains(']') {
        return None;
    }
    // `mock(Script.Function`: the scripts with natives, then their natives.
    if let Some(arg) = body.get(..5).filter(|p| p.eq_ignore_ascii_case("mock(")).map(|_| &body[5..])
    {
        if arg.contains([')', ',']) {
            return None;
        }
        let arg = arg.trim_start();
        let (script, word) = match arg.split_once('.') {
            Some((script, word)) => (Some(script.trim()), word),
            None => (None, arg),
        };
        let replace = TextRange::new(TextSize::new((at - word.len()) as u32), offset);
        let items = match script {
            None => ws
                .all_scripts()
                .filter(|&(_, f)| natives(ws, f).next().is_some())
                .map(|(name, _)| (name.as_str().to_owned(), "script with natives".to_owned()))
                .collect(),
            Some(script) => (script_file(ws, script).into_iter())
                .flat_map(|f| natives(ws, f))
                .map(|m| (m.name.as_str().to_owned(), m.signature()))
                .collect(),
        };
        return Some((replace, items));
    }
    // The word being typed.
    let word_len = body.len()
        - body
            .trim_end_matches(|c: char| {
                c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == ':'
            })
            .len();
    let replace = TextRange::new(TextSize::new((at - word_len) as u32), offset);
    let before = &body[..body.len() - word_len];
    let items = if before.is_empty() {
        ATTRIBUTES.iter().map(|d| (d.path.to_owned(), d.detail.to_owned())).collect()
    } else if before.trim_start().to_ascii_lowercase().starts_with("allow(")
        && before.trim_end().ends_with(['(', ','])
    {
        let mut items: Vec<(String, String)> = LINTS
            .iter()
            .map(|l| (l.code.replace('-', "_"), format!("{} · {}", l.group.name(), l.summary)))
            .collect();
        items.extend(Group::ALL.iter().map(|g| (g.name().to_owned(), "lint group".to_owned())));
        items.push(("all".to_owned(), "every lint".to_owned()));
        items
    } else {
        return None;
    };
    Some((replace, items))
}

/// Quick fixes adding an attribute allowing a lint problem: on its
/// statement, or in the whole file.
pub(crate) fn ignore_fixes(text: &str, d: &Diagnostic) -> Vec<Assist> {
    let start = usize::from(d.range.start());
    let line_start = text[..start].rfind('\n').map_or(0, |i| i + 1);
    let line = &text[line_start..];
    let indent = &line[..line.len() - line.trim_start().len()];
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let name = d.code.replace('-', "_");
    let at = |offset: usize| TextRange::empty(TextSize::new(offset as u32));
    vec![
        Assist {
            id: "allow_lint",
            label: format!("Allow `{name}` here"),
            kind: AssistKind::QuickFix,
            target: d.range,
            edits: vec![TextEdit {
                range: at(line_start),
                new_text: format!("{indent}; #[allow({name})]{newline}"),
            }],
            fixes: Some(d.code),
        },
        Assist {
            id: "allow_lint_file",
            label: format!("Allow `{name}` in this file"),
            kind: AssistKind::QuickFix,
            target: d.range,
            edits: vec![TextEdit {
                range: at(0),
                new_text: format!("; #![allow({name})]{newline}"),
            }],
            fixes: Some(d.code),
        },
    ]
}

/// The quick fix of a misplaced attribute: moves it above its line, or to
/// the top of the file for `#![...]`.
pub(crate) fn attribute_fixes(text: &str, d: &Diagnostic) -> Option<Assist> {
    if d.code != MISPLACED {
        return None;
    }
    let (start, end) = (usize::from(d.range.start()), usize::from(d.range.end()));
    let comment = &text[start..end];
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let line_start = text[..start].rfind('\n').map_or(0, |i| i + 1);
    let before = &text[line_start..start];
    // After code: from the end of the code; alone: the whole line.
    let removed = if before.trim().is_empty() {
        let line_end = text[end..].find('\n').map_or(text.len(), |i| end + i + 1);
        TextRange::new(TextSize::new(line_start as u32), TextSize::new(line_end as u32))
    } else {
        let code_end = line_start + before.trim_end().len();
        TextRange::new(TextSize::new(code_end as u32), d.range.end())
    };
    let inner = comment.trim_start_matches(';').trim_start().starts_with("#![");
    let (label, at, new_text) = if inner {
        ("Move the attribute to the top of the file", 0, format!("{comment}{newline}"))
    } else {
        let indent = &before[..before.len() - before.trim_start().len()];
        ("Move the attribute above", line_start, format!("{indent}{comment}{newline}"))
    };
    Some(Assist {
        id: "move_attribute",
        label: label.to_owned(),
        kind: AssistKind::QuickFix,
        target: d.range,
        edits: vec![
            TextEdit { range: TextRange::empty(TextSize::new(at as u32)), new_text },
            TextEdit { range: removed, new_text: String::new() },
        ],
        fixes: Some(MISPLACED),
    })
}

#[cfg(test)]
mod tests {
    use crate::test_utils::check;

    const BODY: &str = "Function F()\n    Int a = 1\n    ; #[allow(unused_local)]\n    Int b = 2\n    ; #[allow(suspicious)] because\n    Int c = 3\n    ; #[allow(bool-literal-comparison)]\n    Int d = 4\nEndFunction\n";

    #[test]
    fn outer_attributes_allow_their_item() {
        let src = format!("ScriptName T\n{BODY}");
        assert_eq!(check("unused-local", &src), ["3: `a` is never read", "9: `d` is never read"]);
        // Before a function: the whole function.
        let func = format!("ScriptName T\n; #[allow(unused_local)]\n{BODY}");
        assert_eq!(check("unused-local", &func), Vec::<String>::new());
    }

    #[test]
    fn checks_mocks_against_their_native() {
        let src = "ScriptName T\n; #[mock(Utility.RandomInt)]\nInt Function Ok(Int a, Int b)\nEndFunction\n; #[mock(Utility.RandomInt)]\nInt Function WithSelf(Form akSelf, Int a, Int b)\nEndFunction\n; #[mock(Utility.RandomInt)]\nFloat Function BadReturn(Int a, Int b)\nEndFunction\n; #[mock(Utility.RandomInt)]\nInt Function BadParam(Int a, Float b)\nEndFunction\n; #[mock(Utility.Wait)]\nFunction TooMany(Float a, Float b, Float c)\nEndFunction\n; #[mock(Utility.Helper)]\nFloat Function NotNative()\nEndFunction\n; #[mock(Nowhere.Thing)]\nFunction Unknown()\nEndFunction\n; #[mock(Wait)]\nFunction NoScript()\nEndFunction\n";
        let (ws, file) = workspace(src);
        let found: Vec<(String, String)> = crate::lint(&ws, file, &crate::LintConfig::default())
            .into_iter()
            .filter(|d| d.code == "invalid-mock")
            .map(|d| (src[d.range].to_owned(), d.message))
            .collect();
        let at: Vec<&str> = found.iter().map(|(at, _)| at.as_str()).collect();
        assert_eq!(
            at,
            ["BadReturn", "BadParam", "TooMany", "Utility.Helper", "Nowhere.Thing", "Wait"],
            "{found:#?}"
        );
        assert!(found[0].1.contains("returns `Float`, expected `Int`"), "{}", found[0].1);
        assert!(found[1].1.contains("parameter `b` is `Float`, expected `Int`"), "{}", found[1].1);
        assert!(found[2].1.contains("expected 1 parameters (or 2"), "{}", found[2].1);
    }

    #[test]
    fn mocks_go_to_their_native() {
        let src = "ScriptName T\n; #[mock(Utility.RandomInt)]\nInt Function Roll(Int a, Int b)\nEndFunction\n";
        let (ws, file) = workspace(src);
        let goto = |needle: &str| {
            let offset = papyrus_ide::TextSize::new((src.find(needle).unwrap() + 1) as u32);
            let target = super::attribute_definition(&ws, file, offset).unwrap();
            UTILITY[target.focus_range].to_owned()
        };
        assert_eq!(goto("Utility."), "Utility");
        assert_eq!(goto("RandomInt"), "RandomInt");
    }

    #[test]
    fn reports_unknown_attributes_and_lints() {
        let src = "ScriptName T\n; #[allow(invalid_cast, unused_lokal, unused_local)]\n; #[alow]\nFunction F()\nEndFunction\n";
        let mut ws = papyrus_ide::Workspace::new();
        let file = ws.set_open(std::path::PathBuf::from("/ws/T.psc"), std::sync::Arc::from(src), 0);
        let found: Vec<(&str, String)> = crate::lint(&ws, file, &crate::LintConfig::default())
            .into_iter()
            .map(|d| (d.code, src[d.range].to_owned()))
            .collect();
        assert_eq!(
            found,
            [
                ("unknown-lint", "invalid_cast".into()),
                ("unknown-lint", "unused_lokal".into()),
                ("unknown-attribute", "alow".into())
            ]
        );
    }

    const UTILITY: &str = "ScriptName Utility Hidden\nInt Function RandomInt(Int aiMin = 0, Int aiMax = 100) Global Native\nFunction Wait(Float afSeconds) Global Native\nFloat Function Helper()\nEndFunction\n";

    /// A workspace of `src` and the Utility script.
    fn workspace(src: &str) -> (papyrus_ide::Workspace, papyrus_ide::FileId) {
        let mut ws = papyrus_ide::Workspace::new();
        ws.set_open("/ws/Utility.psc".into(), UTILITY.into(), 0);
        let file = ws.set_open("/ws/T.psc".into(), src.into(), 0);
        (ws, file)
    }

    #[test]
    fn hovers_attributes_and_lints() {
        let src = "ScriptName T\n; #[allow(unused_local, style)] why\nFunction F()\nEndFunction\n; #[test]\nFunction G()\nEndFunction\n; #[mock(Utility.RandomInt)]\nInt Function Roll(Int a, Int b)\nEndFunction\n";
        let (ws, file) = workspace(src);
        let hover = |needle: &str| {
            let offset = papyrus_ide::TextSize::new((src.find(needle).unwrap() + 1) as u32);
            super::attribute_hover(&ws, file, offset).map(|(r, md)| (src[r].to_owned(), md))
        };
        let (word, md) = hover("test").unwrap();
        assert_eq!(word, "test");
        assert!(md.contains("`papyrus_analyzer test`"), "{md}");
        assert!(md.contains("Applies to lines 6–7 (`Function G()`)"), "{md}");
        let (word, md) = hover("mock").unwrap();
        assert_eq!(word, "mock");
        assert!(md.contains("runs instead of the native"), "{md}");
        assert!(md.contains("# Examples\n\n```papyrus\n; #[mock(Utility.RandomInt)]"), "{md}");
        let (word, md) = hover("Utility.").unwrap();
        assert_eq!(word, "Utility.RandomInt");
        assert!(
            md.contains("Int Function RandomInt(Int aiMin = 0, Int aiMax = 100) Global Native"),
            "{md}"
        );
        let (word, md) = hover("unused_local").unwrap();
        assert_eq!(word, "unused_local");
        assert!(md.contains("suspicious · warn by default"), "{md}");
        let (_, md) = hover("style").unwrap();
        assert!(md.contains("lint group"), "{md}");
        let (word, md) = hover("allow").unwrap();
        assert_eq!(word, "allow");
        assert!(md.contains("Applies to lines 3–4 (`Function F()`)"), "{md}");
        let (word, _) = hover("#[").unwrap();
        assert_eq!(word, "#[allow(unused_local, style)]");
        assert!(hover("why").is_none());
    }

    #[test]
    fn completes_attributes_and_lints() {
        let names = |src: &str| {
            let (ws, file) = workspace(src);
            let offset = papyrus_ide::TextSize::of(src);
            super::attribute_completions(&ws, file, offset)
                .map(|(_, items)| items.into_iter().map(|i| i.0).collect::<Vec<_>>())
        };
        assert_eq!(names("; #[mock(Uti").unwrap(), ["Utility"]);
        assert_eq!(names("; #[mock(Utility.R").unwrap(), ["RandomInt", "Wait"]);
        assert_eq!(names("; #[mock(Utility.Wait)"), None);
        assert_eq!(names("; #[").unwrap(), ["allow", "fmt::skip", "test", "mock"]);
        let lints = names("    ; #[allow(unused_local, un").unwrap();
        assert!(
            lints.contains(&"unchecked_cast".to_owned()) && lints.contains(&"style".to_owned())
        );
        assert_eq!(names("; just a comment"), None);
        assert_eq!(names("; #[allow(x)] "), None);
        assert_eq!(names("Int x ; #[allow("), None);
    }

    #[test]
    fn inner_attributes_allow_the_file() {
        let src = format!("; #![allow(unused_local)]\nScriptName T\n{BODY}");
        assert_eq!(check("unused-local", &src), Vec::<String>::new());
        let all = format!("; a comment first\n; #![allow(all)]\nScriptName T\n{BODY}");
        assert_eq!(check("unused-local", &all), Vec::<String>::new());
        let other = format!("; #![allow(unreachable_code)]\nScriptName T\n{BODY}");
        assert_eq!(check("unused-local", &other).len(), 2);
        // Stacked, one per line.
        let stacked =
            format!("; #![allow(style)]\n; #![allow(unused_local)]\nScriptName T\n{BODY}");
        assert_eq!(check("unused-local", &stacked), Vec::<String>::new());
        let outer = "ScriptName T\n; #[allow(style)]\n; #[allow(unused_local)]\nFunction F()\n    Int a = 1\nEndFunction\n";
        assert_eq!(check("unused-local", outer), Vec::<String>::new());
    }

    #[test]
    fn misplaced_attributes_apply_to_nothing() {
        let src = "ScriptName T\n; #![allow(all)]\nFunction F()\n    Int a = 1 ; #[allow(unused_local)]\nEndFunction\n";
        assert_eq!(check("unused-local", src), ["4: `a` is never read"]);
        assert_eq!(
            check("misplaced-attribute", src),
            [
                "2: `#![...]` applies to the whole file: it must come before the code, at the top of the file",
                "4: an attribute applies to what follows it, not to code before it on its line: move it to its own line, above",
            ]
        );
    }

    #[test]
    fn fragments_are_not_fixed() {
        let fragment = ";BEGIN FRAGMENT CODE\nScriptName T\nFunction Fragment_0(Bool b)\n;BEGIN CODE\nIf b == True\nEndIf\n;END CODE\nEndFunction\n;END FRAGMENT CODE\n";
        let src =
            format!("{fragment}Function G(Bool b)\n    If b == True\n    EndIf\nEndFunction\n");
        let (mut ws, _) = workspace("ScriptName Other\n");
        let file = ws.set_open("/ws/F.psc".into(), src.as_str().into(), 0);
        let config = crate::LintConfig::default();

        let at = |from: usize| {
            let start = src[from..].find("b == True").unwrap() + from;
            papyrus_ide::TextRange::at((start as u32).into(), 9.into())
        };
        let inside = at(0);
        let outside = at(fragment.len());
        let ids = |assists: Vec<papyrus_ide::assists::Assist>| {
            assists.into_iter().map(|a| a.id).collect::<Vec<_>>()
        };
        assert_eq!(ids(crate::fixes(&ws, file, inside, &config)), Vec::<&str>::new());
        assert!(!crate::fixes(&ws, file, outside, &config).is_empty());
        // Above the fragment, the attribute of the file stays possible.
        assert_eq!(ids(crate::ignore_fixes(&ws, file, inside, &config)), ["allow_lint_file"]);
        assert_eq!(
            ids(crate::ignore_fixes(&ws, file, outside, &config)),
            ["allow_lint", "allow_lint_file"]
        );

        let (fixed, applied) = crate::fix_all(&mut ws, file, &config, false);
        assert_eq!(applied, 1);
        assert_eq!(
            fixed,
            format!("{fragment}Function G(Bool b)\n    If b\n    EndIf\nEndFunction\n")
        );
    }

    #[test]
    fn misplaced_attributes_move_above() {
        let src = "ScriptName T\n; #![allow(all)] why\nFunction F()\n    Int a = 1  ; #[allow(unused_local)]\nEndFunction\n";
        let mut ws = papyrus_ide::Workspace::new();
        let file = ws.set_open("/ws/T.psc".into(), src.into(), 0);
        let range = papyrus_ide::TextRange::up_to(papyrus_ide::TextSize::of(src));
        let mut fixes = crate::ignore_fixes(&ws, file, range, &crate::LintConfig::default());
        fixes.retain(|a| a.id == "move_attribute");
        let applied: Vec<String> = fixes
            .iter()
            .map(|a| {
                let mut edits = a.edits.clone();
                edits.sort_by_key(|e| std::cmp::Reverse(e.range.start()));
                let mut out = src.to_owned();
                for e in edits {
                    out.replace_range(std::ops::Range::<usize>::from(e.range), &e.new_text);
                }
                out
            })
            .collect();
        assert_eq!(
            applied,
            [
                "; #![allow(all)] why\nScriptName T\nFunction F()\n    Int a = 1  ; #[allow(unused_local)]\nEndFunction\n",
                "ScriptName T\n; #![allow(all)] why\nFunction F()\n    ; #[allow(unused_local)]\n    Int a = 1\nEndFunction\n",
            ]
        );
    }
}
