//! Linter for the Papyrus scripting language: warnings about code that
//! compiles but is probably wrong or can be simpler, with automatic fixes.
//!
//! Like clippy, every lint belongs to a [`Group`] and has a [`Level`]: allowed
//! (silent), warned, or denied (reported as an error). Levels are set by
//! lint name, by group, or for `all`, in `[lints]` of `papyrus.toml` and on
//! the command line, see [`LintConfig`].
//!
//! Each lint lives in its own module below `lints/`: its documentation, the
//! check, the fix when there is one, and the tests. To add a lint:
//!
//! 1. create `lints/my_lint.rs` with a `pub(crate) const LINT: Lint`;
//! 2. add `mod my_lint;` to `lints.rs` and `my_lint::LINT` to [`LINTS`].
//!
//! Fixes are assists ([`papyrus_ide::assists`]) of kind quick fix naming the
//! lint code in [`Assist::fixes`].

pub mod ignore;
mod lints;

use std::{fmt, str::FromStr};

use papyrus_ide::{
    Diagnostic, FileId, Severity, TextRange, Workspace,
    assists::{Assist, Handler},
    hir::Body,
    sema::{FileSource, Sema},
};

/// What happens to the problems a lint finds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// Not reported.
    Allow,
    /// Reported as a warning.
    Warn,
    /// Reported as an error: `papyrus_analyzer lint` fails.
    Deny,
}

impl FromStr for Level {
    type Err = String;

    fn from_str(s: &str) -> Result<Level, String> {
        match s.to_ascii_lowercase().as_str() {
            "allow" => Ok(Level::Allow),
            "warn" => Ok(Level::Warn),
            "deny" => Ok(Level::Deny),
            _ => Err(format!("unknown lint level `{s}`: expected \"allow\", \"warn\" or \"deny\"")),
        }
    }
}

impl fmt::Display for Level {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Level::Allow => "allow",
            Level::Warn => "warn",
            Level::Deny => "deny",
        })
    }
}

/// The groups of lints, as in clippy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    /// Code that is wrong. Denied by default.
    Correctness,
    /// Code that is probably wrong or useless.
    Suspicious,
    /// Code that could be written more simply.
    Complexity,
    /// Code that does not follow conventions.
    Style,
    /// Lints that are often too strict. Allowed by default.
    Pedantic,
}

impl Group {
    /// Every group.
    pub const ALL: [Group; 5] =
        [Group::Correctness, Group::Suspicious, Group::Complexity, Group::Style, Group::Pedantic];

    /// The name used in settings.
    pub fn name(self) -> &'static str {
        match self {
            Group::Correctness => "correctness",
            Group::Suspicious => "suspicious",
            Group::Complexity => "complexity",
            Group::Style => "style",
            Group::Pedantic => "pedantic",
        }
    }

    /// The level of its lints when nothing is configured.
    pub fn default_level(self) -> Level {
        match self {
            Group::Correctness => Level::Deny,
            Group::Pedantic => Level::Allow,
            _ => Level::Warn,
        }
    }
}

/// A lint.
pub struct Lint {
    /// The diagnostic code and name in settings, e.g. `unused-local`.
    pub code: &'static str,
    /// The group.
    pub group: Group,
    /// What it reports, for `papyrus_analyzer lint --list`.
    pub summary: &'static str,
    /// Reports the problems of a file.
    run: fn(&Sema<'_>, &mut Vec<Diagnostic>),
    /// The automatic fix.
    fix: Option<Handler>,
}

/// Every lint, in reporting order.
pub const LINTS: &[Lint] = &[
    lints::index_out_of_bounds::LINT,
    lints::unused_local::LINT,
    lints::unreachable_code::LINT,
    lints::bool_literal_comparison::LINT,
    lints::unchecked_none::CAST,
    lints::unchecked_none::CALL,
];

/// The lint named `code`.
pub fn find(code: &str) -> Option<&'static Lint> {
    LINTS.iter().find(|l| l.code.eq_ignore_ascii_case(code))
}

/// Whether `name` can be configured: a lint, a group, `all` or `warnings`.
pub fn is_known_name(name: &str) -> bool {
    find(name).is_some()
        || Group::ALL.iter().any(|g| g.name().eq_ignore_ascii_case(name))
        || name.eq_ignore_ascii_case("all")
        || name.eq_ignore_ascii_case("warnings")
}

/// The levels of the lints.
///
/// Settings are named by lint, by group or `all`. A lint name wins over its
/// group, which wins over `all`; among settings of the same kind the last one
/// wins, so command line flags given after `papyrus.toml` override it.
/// `warnings = "deny"` (`-D warnings`) turns every warning into an error.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LintConfig {
    settings: Vec<(String, Level)>,
}

impl LintConfig {
    /// Sets the level of a lint, a group, `all` or `warnings`.
    pub fn set(&mut self, name: &str, level: Level) -> Result<(), String> {
        if !is_known_name(name) {
            return Err(format!("unknown lint or lint group `{name}`"));
        }
        self.settings.push((name.to_ascii_lowercase(), level));
        Ok(())
    }

    /// The level of `lint`.
    pub fn level(&self, lint: &Lint) -> Level {
        let last = |name: &str| {
            self.settings.iter().rev().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|&(_, l)| l)
        };
        let level = last(lint.code)
            .or_else(|| last(lint.group.name()))
            .or_else(|| last("all"))
            .unwrap_or_else(|| lint.group.default_level());
        match (level, last("warnings")) {
            (Level::Warn, Some(Level::Deny)) => Level::Deny,
            (Level::Warn, Some(Level::Allow)) => Level::Allow,
            (level, _) => level,
        }
    }
}

/// The lint problems of `file`, with the severity of their level. Files with
/// syntax errors get none: the syntax comes first.
pub fn lint(ws: &Workspace, file: FileId, config: &LintConfig) -> Vec<Diagnostic> {
    let Some(src) = FileSource::load(ws, file) else { return Vec::new() };
    let has_errors = match &ws.file(file).open {
        Some(doc) => !doc.errors.is_empty(),
        None => !papyrus_parser::parse(src.text()).errors().is_empty(),
    };
    lint_source(ws, file, &src, has_errors, config)
}

/// Like [`lint`], with the text already parsed (`has_errors`: it has syntax
/// errors), to check many files without parsing them twice.
pub fn lint_source(
    ws: &Workspace,
    file: FileId,
    src: &FileSource,
    has_errors: bool,
    config: &LintConfig,
) -> Vec<Diagnostic> {
    if has_errors {
        return Vec::new();
    }
    let sema = src.sema(ws, file);
    let ignores = ignore::Ignores::of(sema.root);
    let mut out = Vec::new();
    for lint in LINTS {
        let severity = match config.level(lint) {
            Level::Allow => continue,
            Level::Warn => Severity::Warning,
            Level::Deny => Severity::Error,
        };
        let start = out.len();
        (lint.run)(&sema, &mut out);
        for d in &mut out[start..] {
            d.severity = severity;
        }
    }
    out.retain(|d| !ignores.silence(d));
    out.extend(ignore::check_attributes(ws, file, sema.root));
    out.sort_by_key(|d| d.range.start());
    out
}

/// Quick fixes silencing the lint problems at `range` of `file` with a
/// attribute (see [`ignore`]): on their statement, or in the whole file.
pub fn ignore_fixes(
    ws: &Workspace,
    file: FileId,
    range: TextRange,
    config: &LintConfig,
) -> Vec<Assist> {
    let Some(text) = ws.text(file) else { return Vec::new() };
    let mut out = Vec::new();
    for d in lint(ws, file, config).iter().filter(|d| d.range.intersect(range).is_some()) {
        // Problems of lints can be allowed; the others (attributes) cannot.
        if find(d.code).is_some() {
            out.extend(ignore::ignore_fixes(&text, d));
        }
        out.extend(ignore::attribute_fixes(&text, d));
    }
    // Like the other assists: the fragments of the Creation Kit stay as they are.
    let parse = papyrus_parser::parse(&text);
    let fragments = papyrus_parser::attrs::fragments(parse.syntax_node());
    out.retain(|a: &Assist| {
        !a.edits.iter().any(|e| papyrus_parser::attrs::touches_fragment(&fragments, e.range))
    });
    out
}

/// The fixes of the reported lints applicable at `range` of `file`.
pub fn fixes(ws: &Workspace, file: FileId, range: TextRange, config: &LintConfig) -> Vec<Assist> {
    let handlers: Vec<Handler> =
        LINTS.iter().filter(|l| config.level(l) != Level::Allow).filter_map(|l| l.fix).collect();
    papyrus_ide::assists::run_handlers(ws, file, range, &handlers)
}

/// Applies the fixes of every reported problem of `file`, until none is
/// left (or for a few rounds), and returns the new text and the number of
/// applied fixes. `file` keeps the new text.
pub fn fix_all(ws: &mut Workspace, file: FileId, config: &LintConfig) -> (String, usize) {
    let mut applied = 0;
    for _ in 0..8 {
        let Some(text) = ws.text(file) else { break };
        let mut edits: Vec<papyrus_ide::assists::TextEdit> = Vec::new();
        for d in lint(ws, file, config) {
            let Some(fix) =
                fixes(ws, file, d.range, config).into_iter().find(|a| a.fixes == Some(d.code))
            else {
                continue;
            };
            // Edits of one round must not overlap.
            let overlaps = fix.edits.iter().any(|e| {
                edits.iter().any(|o| {
                    e.range.intersect(o.range).is_some_and(|r| !r.is_empty()) || e.range == o.range
                })
            });
            if !overlaps {
                edits.extend(fix.edits);
                applied += 1;
            }
        }
        if edits.is_empty() {
            break;
        }
        edits.sort_by_key(|e| std::cmp::Reverse(e.range.start()));
        let mut new = text.to_string();
        for e in edits {
            new.replace_range(
                usize::from(e.range.start())..usize::from(e.range.end()),
                &e.new_text,
            );
        }
        let path = ws.file(file).path.clone();
        ws.set_open(path, std::sync::Arc::from(new.as_str()), 0);
    }
    (ws.text(file).map(|t| t.to_string()).unwrap_or_default(), applied)
}

/// The bodies of the functions and events of a file, lowered.
pub(crate) fn bodies<'a>(sema: &'a Sema<'_>) -> impl Iterator<Item = Body> + use<'a> {
    use papyrus_parser::SyntaxKind;
    sema.root
        .descendants()
        .filter(|n| matches!(n.kind(), SyntaxKind::FUNCTION | SyntaxKind::EVENT))
        .filter_map(|n| Body::lower(sema, n))
}

#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) mod test_utils {
    use std::{path::PathBuf, sync::Arc};

    use papyrus_ide::Workspace;

    /// The problems of the lint `code` in `src`, as `line: message`.
    pub(crate) fn check(code: &str, src: &str) -> Vec<String> {
        let mut ws = Workspace::new();
        let file = ws.set_open(PathBuf::from("/ws/T.psc"), Arc::from(src), 0);
        crate::lint(&ws, file, &crate::LintConfig::default())
            .into_iter()
            .filter(|d| d.code == code)
            .map(|d| {
                let line = src[..usize::from(d.range.start())].matches('\n').count() + 1;
                format!("{line}: {}", d.message)
            })
            .collect()
    }
}
