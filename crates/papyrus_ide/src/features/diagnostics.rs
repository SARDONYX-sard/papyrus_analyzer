//! Syntax and semantic diagnostics.
//!
//! The checks follow the official compiler: what it rejects is an error.
//!
//! Semantic checks that depend on other scripts (unknown types, members and
//! names) only run when the workspace knows the base game scripts (detected by
//! the presence of `Form`), and member checks only when the whole `extends`
//! chain of the receiver is known. Without the game sources every name would
//! look unresolved.

use papyrus_intern::Name;
use papyrus_parser::{
    SyntaxKind, TextRange,
    ast::{self, AstNode},
};
use rustc_hash::FxHashMap;

use super::in_body;
use crate::{
    FileId, Workspace,
    flags::{FlagTable, FlagTargets},
    index::MemberKind,
    sema::{Builtin, FileSource, Sema, Ty},
};

/// The codes of the diagnostics of the compiler checks, errors the official
/// compiler rejects too (lints have their own codes, in `papyrus_lint`).
pub const COMPILER_CODES: &[&str] = &[
    "syntax",
    "unknown-type",
    "unknown-member",
    "unknown-param",
    "unresolved-name",
    "script-name-mismatch",
    "extends-self",
    "builtin-redefined",
    "duplicate-state",
    "duplicate-definition",
    "state-function-not-declared",
    "reserved-name",
    "name-is-type",
    "name-conflict",
    "unknown-flag",
    "misplaced-flag",
    "type-mismatch",
    "invalid-operator",
    "invalid-cast",
    "invalid-member",
    "invalid-assignment",
    "not-an-array",
    "not-global",
    "read-only",
    "wrong-arg-count",
    "missing-return-value",
    "unexpected-return-value",
];

/// The severity of a [`Diagnostic`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    /// The script does not compile.
    Error,
    /// The script probably has a bug.
    Warning,
}

/// A problem in a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    /// The location.
    pub range: TextRange,
    /// The severity.
    pub severity: Severity,
    /// The message.
    pub message: String,
    /// A stable identifier of the check.
    pub code: &'static str,
    /// Other places the problem is about, e.g. the first definition of a
    /// duplicate name, like the notes of rustc.
    pub related: Vec<Related>,
}

/// A place a [`Diagnostic`] refers to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Related {
    /// The file.
    pub file: FileId,
    /// The range.
    pub range: TextRange,
    /// What is there, e.g. `first defined here`.
    pub message: String,
}

impl Diagnostic {
    /// Adds a related place in `file`.
    pub fn with_related(
        mut self,
        file: FileId,
        range: TextRange,
        message: impl Into<String>,
    ) -> Diagnostic {
        self.related.push(Related { file, range, message: message.into() });
        self
    }

    /// Returns `true` for checks that depend on other scripts being known:
    /// unknown types, members, parameters and names.
    pub fn needs_workspace(&self) -> bool {
        matches!(self.code, "unknown-type" | "unknown-member" | "unknown-param" | "unresolved-name")
            || TYPE_CODES.contains(&self.code)
    }

    /// An error.
    pub fn error(range: TextRange, code: &'static str, message: impl Into<String>) -> Diagnostic {
        Diagnostic {
            range,
            severity: Severity::Error,
            message: message.into(),
            code,
            related: Vec::new(),
        }
    }

    /// A warning.
    pub fn warning(range: TextRange, code: &'static str, message: impl Into<String>) -> Diagnostic {
        Diagnostic {
            range,
            severity: Severity::Warning,
            message: message.into(),
            code,
            related: Vec::new(),
        }
    }
}

/// All diagnostics of a file.
pub fn diagnostics(ws: &Workspace, file: FileId) -> Vec<Diagnostic> {
    let data = ws.file(file);
    let (src, errors) = match &data.open {
        Some(doc) => (FileSource::load(ws, file), doc.errors.to_vec()),
        None => match ws.text(file) {
            Some(text) => {
                let (src, errors) = FileSource::parse(text);
                (Some(src), errors)
            }
            None => (None, Vec::new()),
        },
    };
    match src {
        Some(src) => diagnostics_of(ws, file, &src, &errors),
        None => Vec::new(),
    }
}

/// Like [`diagnostics`], with the text already parsed: `src` and its
/// `syntax_errors`, to check many files without parsing them twice.
pub fn diagnostics_of(
    ws: &Workspace,
    file: FileId,
    src: &FileSource,
    syntax_errors: &[papyrus_parser::SyntaxError],
) -> Vec<Diagnostic> {
    let mut out: Vec<Diagnostic> =
        syntax_errors.iter().map(|e| Diagnostic::error(e.range(), "syntax", e.message())).collect();
    let has_syntax_errors = !syntax_errors.is_empty();
    let sema = src.sema(ws, file);
    let sema = &sema;
    check_script_name(sema, &mut out);
    check_duplicates(sema, &mut out);
    check_declarations(sema, &mut out);
    check_flags(sema, &mut out);
    if !has_syntax_errors {
        check_initializers(sema, &mut out);
    }
    // Name resolution on broken code is noisy; fix the syntax first.
    if !has_syntax_errors && ws.script(Name::new("Form")).is_some() {
        check_names(sema, &mut out);
        check_types(sema, &mut out);
    }
    out.sort_by_key(|d| (d.range.start(), d.severity));
    out
}
/// Diagnostics of the flags file.
pub fn flag_file_diagnostics(flags: &FlagTable) -> Vec<Diagnostic> {
    flags
        .errors
        .iter()
        .map(|e| Diagnostic::error(e.range, "flags-syntax", e.message.clone()))
        .collect()
}

fn check_script_name(sema: &Sema<'_>, out: &mut Vec<Diagnostic>) {
    let index = sema.index;
    let (Some(name), Some(range)) = (index.name, index.name_range) else { return };
    let path = &sema.ws.file(sema.file).path;
    let Some(stem) = path.file_stem().map(|s| s.to_string_lossy()) else { return };
    if !stem.eq_ignore_ascii_case(name.as_str()) {
        out.push(Diagnostic::error(
            range,
            "script-name-mismatch",
            format!("script name `{name}` does not match the file name `{stem}.psc`"),
        ));
    }
    if let Some(ext) = index.extends
        && ext == name
    {
        out.push(Diagnostic::error(
            index.extends_range.unwrap_or(range),
            "extends-self",
            "a script cannot extend itself",
        ));
    }
}

fn check_duplicates(sema: &Sema<'_>, out: &mut Vec<Diagnostic>) {
    // Functions and events are scoped by state; variables, properties and
    // states each have their own namespace (a variable and a property may
    // share a name).
    let mut seen: FxHashMap<(Option<Name>, Option<Name>, u8, Name), usize> = FxHashMap::default();
    for (i, m) in sema.index.members.iter().enumerate() {
        let callable = m.is_callable();
        let category = match m.kind {
            MemberKind::State => 2,
            MemberKind::Property => 3,
            _ if callable => 1,
            _ => 0,
        };
        // The compiler generates both in every script.
        if callable
            && m.property.is_none()
            && ["GetState", "GotoState"].iter().any(|b| m.name.as_str().eq_ignore_ascii_case(b))
        {
            let message = match m.state {
                None => format!("`{}` is generated by the compiler and cannot be defined", m.name),
                Some(_) => format!("`{}` cannot be overridden in a state", m.name),
            };
            out.push(Diagnostic::error(m.name_range, "builtin-redefined", message));
        }
        let key = (if callable { m.state } else { None }, m.property, category, m.name);
        if let Some(&first) = seen.get(&key) {
            let first = &sema.index.members[first];
            if m.kind == MemberKind::State && first.kind == MemberKind::State {
                // The compiler merges the blocks (vanilla scripts do this);
                // duplicate functions are reported per state above.
                out.push(
                    Diagnostic::warning(
                        m.name_range,
                        "duplicate-state",
                        format!("state `{}` is declared again; the blocks are merged", m.name),
                    )
                    .with_related(
                        sema.file,
                        first.name_range,
                        "first declared here",
                    ),
                );
            } else {
                out.push(
                    Diagnostic::error(
                        m.name_range,
                        "duplicate-definition",
                        format!("`{}` is already defined in this scope", m.name),
                    )
                    .with_related(
                        sema.file,
                        first.name_range,
                        "first defined here",
                    ),
                );
            }
        } else {
            seen.insert(key, i);
        }
    }

    // A state function must match a function of the empty state.
    for m in sema.index.members.iter() {
        let Some(state) = m.state else { continue };
        if !m.is_callable() || m.property.is_some() {
            continue;
        }
        let script = sema.script_name();
        let declared_elsewhere = sema
            .index
            .members
            .iter()
            .any(|o| o.state.is_none() && o.is_callable() && o.name == m.name)
            || script.is_some_and(|s| {
                sema.chain(s)
                    .skip(1)
                    .any(|(_, idx)| idx.find(m.name, |o| o.is_callable()).is_some())
            })
            || m.name.as_str().eq_ignore_ascii_case(Builtin::OnBeginState.name())
            || m.name.as_str().eq_ignore_ascii_case(Builtin::OnEndState.name());
        let parent_unknown =
            sema.index.extends.is_some_and(|p| sema.ws.script_near(p, sema.file).is_none());
        if !declared_elsewhere && !parent_unknown && m.kind == MemberKind::Function {
            out.push(Diagnostic::error(
                m.name_range,
                "state-function-not-declared",
                format!(
                    "function `{}` in state `{state}` must also be declared outside of any state",
                    m.name
                ),
            ));
        }
    }
}

/// Naming rules of the compiler for declarations:
///
/// - `Length` cannot name anything (it is the array length);
/// - variables, locals and properties cannot have the name of a script;
/// - parameters and locals cannot have the name of a script variable, and
///   locals not the name of a parameter, of a property of the script, or of
///   a local of an enclosing block (locals of sibling blocks may share it).
fn check_declarations(sema: &Sema<'_>, out: &mut Vec<Diagnostic>) {
    // The declaration of a member of the script, outside of states.
    let own = |kind: MemberKind, name: Name| {
        sema.index
            .members
            .iter()
            .find(|m| m.kind == kind && m.state.is_none() && m.name == name)
            .map(|m| m.name_range)
    };
    for node in sema.root.descendants() {
        match node.kind() {
            SyntaxKind::NAME => {
                let Some(name) = ast::Name::cast(node) else { continue };
                if name.text().eq_ignore_ascii_case("Length")
                    && node.parent().is_some_and(|p| p.kind() != SyntaxKind::SCRIPT_HEADER)
                {
                    out.push(Diagnostic::error(
                        node.text_range(),
                        "reserved-name",
                        "`Length` is reserved for the length of arrays and cannot be a name",
                    ));
                }
            }
            SyntaxKind::VAR_DECL | SyntaxKind::PROPERTY => {
                let Some(name) = node.children().find_map(ast::Name::cast) else { continue };
                let n = Name::new(name.text());
                let script = if sema.script_name() == Some(n) {
                    Some(sema.file)
                } else {
                    sema.ws.script_near(n, sema.file)
                };
                if let Some(script) = script {
                    let mut d = Diagnostic::error(
                        name.syntax().text_range(),
                        "name-is-type",
                        format!(
                            "`{n}` is the name of a script; variables and properties cannot use it"
                        ),
                    );
                    if let Some(range) = sema.ws.file(script).index.name_range {
                        d = d.with_related(
                            script,
                            range,
                            format!("the script `{n}` is declared here"),
                        );
                    }
                    out.push(d);
                }
            }
            SyntaxKind::FUNCTION | SyntaxKind::EVENT => {
                let mut params = Vec::new();
                let list = node.children().find_map(ast::ParamList::cast);
                for p in list.iter().flat_map(|l| l.params()) {
                    let Some(name) = p.name() else { continue };
                    let n = Name::new(name.text());
                    if let Some(var) = own(MemberKind::Variable, n) {
                        out.push(
                            Diagnostic::error(
                                name.syntax().text_range(),
                                "name-conflict",
                                format!("`{n}` is already defined as a variable of the script"),
                            )
                            .with_related(
                                sema.file,
                                var,
                                "the variable is declared here",
                            ),
                        );
                    }
                    params.push((n, name.syntax().text_range()));
                }
                let Some(block) = node.children().find_map(ast::Block::cast) else { continue };
                let mut scopes = vec![Vec::new()];
                check_locals(sema, &own, &params, block, &mut scopes, out);
            }
            _ => {}
        }
    }
}

/// A name and the range of its declaration.
type Declared = (Name, TextRange);

fn check_locals(
    sema: &Sema<'_>,
    own: &dyn Fn(MemberKind, Name) -> Option<TextRange>,
    params: &[Declared],
    block: ast::Block<'_>,
    scopes: &mut Vec<Vec<Declared>>,
    out: &mut Vec<Diagnostic>,
) {
    let find = |list: &[Declared], n: Name| list.iter().find(|(m, _)| *m == n).map(|&(_, r)| r);
    for stmt in block.stmts() {
        let blocks: Vec<ast::Block<'_>> = match stmt {
            ast::Stmt::VarDecl(var) => {
                let Some(name) = var.name() else { continue };
                let n = Name::new(name.text());
                let conflict = if let Some(r) = find(params, n) {
                    Some(("as a parameter", r, "the parameter is declared here"))
                } else if let Some(r) = scopes.last().and_then(|s| find(s, n)) {
                    Some(("in this block", r, "first declared here"))
                } else if let Some(r) = scopes.iter().find_map(|s| find(s, n)) {
                    Some((
                        "in an enclosing block, and locals cannot shadow it",
                        r,
                        "the outer local is declared here",
                    ))
                } else if let Some(r) = own(MemberKind::Variable, n) {
                    Some(("as a variable of the script", r, "the variable is declared here"))
                } else {
                    own(MemberKind::Property, n).map(|r| {
                        ("as a property of the script", r, "the property is declared here")
                    })
                };
                if let Some((why, range, note)) = conflict {
                    out.push(
                        Diagnostic::error(
                            name.syntax().text_range(),
                            "name-conflict",
                            format!("`{n}` is already defined {why}"),
                        )
                        .with_related(sema.file, range, note),
                    );
                }
                if let Some(s) = scopes.last_mut() {
                    s.push((n, name.syntax().text_range()));
                }
                continue;
            }
            ast::Stmt::If(i) => i
                .then_block()
                .into_iter()
                .chain(i.else_ifs().filter_map(|c| c.block()))
                .chain(i.else_clause().and_then(|c| c.block()))
                .collect(),
            ast::Stmt::While(w) => w.body().into_iter().collect(),
            _ => continue,
        };
        for inner in blocks {
            scopes.push(Vec::new());
            check_locals(sema, own, params, inner, scopes, out);
            scopes.pop();
        }
    }
}
fn check_flags(sema: &Sema<'_>, out: &mut Vec<Diagnostic>) {
    let table = sema.ws.flags_of(sema.file);
    for node in sema.root.descendants() {
        let Some(flag) = ast::Flag::cast(node) else { continue };
        let (Some(tok), Some(decl)) = (flag.token(), node.parent()) else { continue };
        let auto_property = ast::Property::cast(decl).is_some_and(|p| p.is_auto());
        let Some(target) = FlagTargets::of_declaration(decl.kind(), auto_property) else {
            continue;
        };
        let range = node.text_range();
        let allowed = match tok.kind() {
            SyntaxKind::IDENT => match table.get(tok.text()) {
                Some((_, def)) => def.targets,
                None => {
                    out.push(Diagnostic::error(
                        range,
                        "unknown-flag",
                        format!(
                            "unknown flag `{}`, not defined in {}",
                            tok.text(),
                            table.source_name()
                        ),
                    ));
                    continue;
                }
            },
            SyntaxKind::NATIVE_KW => FlagTargets::SCRIPT.union(FlagTargets::FUNCTION),
            SyntaxKind::GLOBAL_KW => FlagTargets::FUNCTION,
            SyntaxKind::AUTO_KW | SyntaxKind::AUTOREADONLY_KW => FlagTargets::PROPERTY,
            _ => continue,
        };
        if !allowed.intersects(target) {
            out.push(Diagnostic::error(
                range,
                "misplaced-flag",
                format!(
                    "flag `{}` is not allowed on {} (allowed on: {})",
                    tok.text(),
                    target_label(decl.kind()),
                    allowed.names().join(", ")
                ),
            ));
        }
    }
}

fn target_label(kind: SyntaxKind) -> &'static str {
    match kind {
        SyntaxKind::SCRIPT_HEADER => "scripts",
        SyntaxKind::PROPERTY => "properties",
        SyntaxKind::VAR_DECL => "variables",
        SyntaxKind::EVENT => "events",
        _ => "functions",
    }
}
/// Codes of the type checker, see [`crate::typeck`].
const TYPE_CODES: &[&str] = &[
    "type-mismatch",
    "invalid-operator",
    "invalid-cast",
    "invalid-member",
    "invalid-assignment",
    "not-an-array",
    "not-global",
    "read-only",
    "wrong-arg-count",
    "missing-return-value",
    "unexpected-return-value",
];

/// Type checks bodies.
fn check_types(sema: &Sema<'_>, out: &mut Vec<Diagnostic>) {
    for node in sema.root.descendants() {
        if !matches!(node.kind(), SyntaxKind::FUNCTION | SyntaxKind::EVENT) {
            continue;
        }
        let Some(body) = crate::hir::Body::lower(sema, node) else { continue };
        let result = crate::typeck::check_body(sema, &body);
        out.extend(
            result.errors.into_iter().map(|e| Diagnostic::error(e.range, e.code, e.message)),
        );
    }
}

/// Checks the initial values of script variables, properties and parameter
/// defaults, which do not depend on other scripts.
fn check_initializers(sema: &Sema<'_>, out: &mut Vec<Diagnostic>) {
    for node in sema.root.descendants() {
        // Locals take any expression; they are checked with their body.
        if node.parent().is_some_and(|p| p.kind() == SyntaxKind::BLOCK) {
            continue;
        }
        let (ty, value, convert) = if let Some(v) = ast::VarDecl::cast(node) {
            (v.ty(), v.initializer(), false)
        } else if let Some(p) = ast::Property::cast(node) {
            (p.ty(), p.initializer(), p.is_read_only())
        } else if let Some(p) = ast::Param::cast(node) {
            (p.ty(), p.default_value(), false)
        } else {
            continue;
        };
        let Some(value) = value else { continue };
        let declared = ty.and_then(crate::index::TypeRef::from_ast);
        if let Some(e) = crate::typeck::check_initializer(declared, value, convert) {
            out.push(Diagnostic::error(e.range, e.code, e.message));
        }
    }
}

fn check_names(sema: &Sema<'_>, out: &mut Vec<Diagnostic>) {
    for node in sema.root.descendants() {
        match node.kind() {
            SyntaxKind::TYPE => {
                let Some(ty) = ast::Type::cast(node) else { continue };
                if ty.is_primitive() || ty.base().is_none() {
                    continue;
                }
                if sema.resolve_type(ty).is_none() {
                    out.push(Diagnostic::error(
                        ty.base().map_or(node.text_range(), |b| b.text_range()),
                        "unknown-type",
                        format!(
                            "unknown type `{}`{}",
                            ty.base_text(),
                            builtin_hint(sema, ty.base_text())
                        ),
                    ));
                }
            }
            SyntaxKind::IMPORT => {
                let Some(nr) = node.children().find_map(ast::NameRef::cast) else { continue };
                if sema.ws.script_near(Name::new(nr.text()), sema.file).is_none() {
                    out.push(Diagnostic::error(
                        nr.syntax().text_range(),
                        "unknown-type",
                        format!("unknown script `{}`{}", nr.text(), builtin_hint(sema, nr.text())),
                    ));
                }
            }
            SyntaxKind::EXTENDS_CLAUSE => {
                let Some(nr) = node.children().find_map(ast::NameRef::cast) else { continue };
                if sema.resolve_name_ref(nr).is_none() {
                    out.push(Diagnostic::error(
                        nr.syntax().text_range(),
                        "unknown-type",
                        format!("unknown script `{}`", nr.text()),
                    ));
                }
            }
            SyntaxKind::NAME_REF if in_body(node) => check_name_ref(sema, node, out),
            _ => {}
        }
    }
}

fn check_name_ref(
    sema: &Sema<'_>,
    node: papyrus_parser::SyntaxNode<'_>,
    out: &mut Vec<Diagnostic>,
) {
    let Some(nr) = ast::NameRef::cast(node) else { return };
    if nr.is_self() || nr.is_parent() || sema.resolve_name_ref(nr).is_some() {
        return;
    }
    let Some(parent) = node.parent() else { return };
    let range = nr.syntax().text_range();

    if let Some(field) = ast::FieldExpr::cast(parent).filter(|f| f.name_ref() == Some(nr)) {
        // `receiver.member`: only report when the receiver type is fully known.
        let Some(recv) = field.receiver().and_then(|r| sema.infer(r)) else { return };
        let script = match recv {
            Ty::Value(t) if t.array => {
                out.push(Diagnostic::error(
                    range,
                    "unknown-member",
                    format!("arrays have no member `{}`", nr.text()),
                ));
                return;
            }
            Ty::Value(t) => match t.object() {
                Some(s) => s,
                None => {
                    out.push(Diagnostic::error(
                        range,
                        "unknown-member",
                        format!("type `{t}` has no member `{}`", nr.text()),
                    ));
                    return;
                }
            },
            Ty::Static(s) => s,
            Ty::None => return,
        };
        if sema.chain_is_complete(script) {
            out.push(Diagnostic::error(
                range,
                "unknown-member",
                format!("`{script}` has no member `{}`", nr.text()),
            ));
        }
        return;
    }

    if ast::Arg::cast(parent).is_some_and(|a| a.name() == Some(nr)) {
        let call = parent.parent().and_then(|p| p.parent()).and_then(ast::CallExpr::cast);
        if call.and_then(|c| c.callee_name()).and_then(|n| sema.resolve_name_ref(n)).is_some() {
            out.push(Diagnostic::error(
                range,
                "unknown-param",
                format!("no parameter named `{}`", nr.text()),
            ));
        }
        return;
    }

    // A plain name: only report when every script it could come from is known.
    let complete = sema.script_name().is_none_or(|s| sema.chain_is_complete(s))
        && sema.index.imports.iter().all(|&i| sema.ws.script_near(i, sema.file).is_some());
    if complete {
        out.push(Diagnostic::error(
            range,
            "unresolved-name",
            format!("cannot find `{}` in this scope{}", nr.text(), builtin_hint(sema, nr.text())),
        ));
    }
}

/// Why the built-in script `name` (`PapyrusAnalyzer`) is not seen, if it is
/// one.
fn builtin_hint(sema: &Sema<'_>, name: &str) -> &'static str {
    let builtin = sema.ws.script(Name::new(name)).is_some_and(|f| {
        sema.ws.file(f).origin == crate::Origin::Builtin && !sema.ws.is_visible(f, sema.file)
    });
    if builtin { ": it is only available in the [test] sources of papyrus.toml" } else { "" }
}
