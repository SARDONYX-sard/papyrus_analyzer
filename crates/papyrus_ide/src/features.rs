//! IDE features. Shared helpers live here.

pub(crate) mod completion;
pub(crate) mod diagnostics;
pub(crate) mod folding;
pub(crate) mod hover;
pub(crate) mod inlay_hints;
pub(crate) mod keyword_docs;
pub(crate) mod name_spellings;
pub(crate) mod navigation;
pub(crate) mod overrides;
pub(crate) mod rename;
pub(crate) mod semantic_tokens;
pub(crate) mod signature_help;
pub(crate) mod snippets;
pub(crate) mod symbols;

use papyrus_intern::Name;
use papyrus_parser::{
    SyntaxKind, SyntaxNode, SyntaxToken, TextRange, TextSize,
    ast::{self, AstNode},
};

use crate::{
    FileId,
    features::navigation::NavFile,
    index::{MemberKind, TypeRef, render_docs},
    sema::{Def, FileSource, LocalKind, Sema},
};

/// The identifier-like token at `offset`, preferring identifiers over
/// punctuation when the offset is between two tokens.
pub(crate) fn ident_at<'a>(sema: &Sema<'a>, offset: TextSize) -> Option<SyntaxToken<'a>> {
    let is_ident = |t: &SyntaxToken<'_>| {
        matches!(t.kind(), SyntaxKind::IDENT | SyntaxKind::SELF_KW | SyntaxKind::PARENT_KW)
            || (t.kind().is_keyword() && t.parent().is_some_and(|p| p.kind() == SyntaxKind::FLAG))
    };
    let tok = sema
        .root
        .token_at_offset(offset)
        .max_by_key(|t| is_ident(t) || t.kind().is_primitive_type())?;
    is_ident(&tok).then_some(tok)
}

/// What the identifier at `offset` refers to, with the identifier range.
pub(crate) fn def_at(sema: &Sema<'_>, offset: TextSize) -> Option<(TextRange, Def)> {
    // A state named by a string: `GoToState("Busy")`.
    let Some(tok) = ident_at(sema, offset) else {
        return crate::states::state_def_at(sema, offset);
    };
    let range = tok.text_range();
    let parent = tok.parent()?;
    let def = match parent.kind() {
        SyntaxKind::NAME => decl_def(sema, parent)?,
        SyntaxKind::NAME_REF => sema.resolve_name_ref(ast::NameRef::cast(parent)?)?,
        SyntaxKind::TYPE => sema.resolve_type(ast::Type::cast(parent)?)?,
        SyntaxKind::FLAG => flag_def(sema, tok)?,
        _ => return None,
    };
    Some((range, def))
}

/// The definition of a flag token.
pub(crate) fn flag_def(sema: &Sema<'_>, tok: SyntaxToken<'_>) -> Option<Def> {
    if tok.kind() == SyntaxKind::IDENT {
        let (i, _) = sema.ws.flags().get(tok.text())?;
        Some(Def::Flag(i as u32))
    } else {
        Some(Def::LanguageFlag(tok.kind()))
    }
}

/// Documentation of the language flags.
pub(crate) fn language_flag_docs(kind: SyntaxKind) -> (&'static str, &'static str) {
    match kind {
        SyntaxKind::NATIVE_KW => (
            "Native",
            "On a function or event: implemented by the game engine, declared without body.\n\nOn a script: the script is an engine object type.",
        ),
        SyntaxKind::GLOBAL_KW => {
            ("Global", "A function without `Self`, called as `ScriptName.Function()`.")
        }
        SyntaxKind::AUTO_KW => ("Auto", "An auto property, backed by a hidden variable."),
        SyntaxKind::AUTOREADONLY_KW => {
            ("AutoReadOnly", "A read-only auto property whose value is its initializer.")
        }
        _ => ("", ""),
    }
}

/// The definition introduced by a `NAME` node.
pub(crate) fn decl_def(sema: &Sema<'_>, name: SyntaxNode<'_>) -> Option<Def> {
    let decl = name.parent()?;
    let range = name.text_range();
    let text = ast::Name::cast(name)?.text();
    match decl.kind() {
        SyntaxKind::SCRIPT_HEADER => Some(Def::Script { file: sema.file }),
        SyntaxKind::PARAM => Some(Def::Local {
            file: sema.file,
            name: Name::new(text),
            range,
            ty: ast::Param::cast(decl)?.ty().and_then(TypeRef::from_ast),
            kind: LocalKind::Parameter,
        }),
        SyntaxKind::VAR_DECL if decl.parent().is_some_and(|p| p.kind() == SyntaxKind::BLOCK) => {
            Some(Def::Local {
                file: sema.file,
                name: Name::new(text),
                range,
                ty: ast::VarDecl::cast(decl)?.ty().and_then(TypeRef::from_ast),
                kind: LocalKind::Variable,
            })
        }
        _ => {
            let index = sema.index.members.iter().position(|m| m.name_range == range)?;
            Some(Def::Member { file: sema.file, index: index as u32 })
        }
    }
}

/// Presentation of a definition.
pub(crate) struct DefInfo {
    /// Pseudo source code of the declaration.
    pub(crate) signature: String,
    /// Where it is declared, e.g. `Actor` or `local variable`.
    pub(crate) container: Option<String>,
    /// Rendered documentation comments.
    pub(crate) docs: String,
}

pub(crate) fn def_info(sema: &Sema<'_>, def: Def) -> DefInfo {
    match def {
        Def::Local { name, ty, kind, .. } => DefInfo {
            signature: match ty {
                Some(ty) => format!("{ty} {name}"),
                None => name.to_string(),
            },
            container: Some(
                match kind {
                    LocalKind::Variable => "local variable",
                    LocalKind::Parameter => "parameter",
                }
                .to_owned(),
            ),
            docs: String::new(),
        },
        Def::Member { file, index } => {
            let member = sema.member(file, index);
            let script = sema.ws.file(file).index.name_str();
            let container = match (member.property, member.state) {
                (Some(prop), _) => format!("{script}.{prop}"),
                (None, Some(state)) => format!("{script} (State {state})"),
                (None, None) => script.to_owned(),
            };
            DefInfo {
                signature: member.signature(),
                container: Some(container),
                docs: docs_in(sema, file, &member.docs),
            }
        }
        Def::Param { file, member, param } => {
            let m = sema.member(file, member);
            let mut signature = String::new();
            crate::index::render_param(&mut signature, &m.params[param as usize]);
            DefInfo {
                signature,
                container: Some(format!("parameter of {}", m.name)),
                docs: String::new(),
            }
        }
        Def::Script { file } => {
            let index = &sema.ws.file(file).index;
            let mut signature = format!("ScriptName {}", index.name_str());
            if let Some(ext) = index.extends {
                signature.push_str(&format!(" extends {ext}"));
            }
            crate::index::render_flags(&mut signature, index.flags, &index.user_flags);
            DefInfo { signature, container: None, docs: docs_in(sema, file, &index.docs) }
        }
        Def::Builtin(b, elem) => DefInfo {
            signature: b.signature(elem),
            container: Some("built-in".to_owned()),
            docs: b.docs().to_owned(),
        },
        Def::Flag(i) => {
            let flags = sema.ws.flags();
            let def = &flags.defs[i as usize];
            DefInfo {
                signature: def.signature(),
                container: Some(flags.source_name()),
                docs: def.docs.clone(),
            }
        }
        Def::LanguageFlag(kind) => {
            let (name, docs) = language_flag_docs(kind);
            DefInfo {
                signature: name.to_owned(),
                container: Some("language flag".to_owned()),
                docs: docs.to_owned(),
            }
        }
    }
}

/// Renders documentation comments of a file, loading it if needed.
pub(crate) fn docs_in(sema: &Sema<'_>, file: FileId, ranges: &[TextRange]) -> String {
    if ranges.is_empty() {
        return String::new();
    }
    if file == sema.file {
        return render_docs(sema.text, ranges);
    }
    FileSource::load(sema.ws, file).map_or_else(String::new, |src| render_docs(src.text(), ranges))
}

/// The declaration location of a definition: `(file, name range, full range)`.
pub(crate) fn def_location(sema: &Sema<'_>, def: Def) -> Option<(NavFile, TextRange, TextRange)> {
    let script = |file, focus, full| Some((NavFile::Script(file), focus, full));
    match def {
        Def::Local { file, range, .. } => script(file, range, range),
        Def::Member { file, index } => {
            let m = sema.member(file, index);
            script(file, m.name_range, m.full_range)
        }
        Def::Param { file, member, .. } => {
            let m = sema.member(file, member);
            script(file, m.name_range, m.full_range)
        }
        Def::Script { file } => {
            let range = sema.ws.file(file).index.name_range?;
            script(file, range, range)
        }
        Def::Flag(i) => {
            let flags = sema.ws.flags();
            // The built-in defaults have no file to jump to.
            flags.path.as_ref()?;
            let def = flags.defs.get(i as usize)?;
            Some((NavFile::Flags, def.name_range, def.full_range))
        }
        Def::Builtin(..) | Def::LanguageFlag(_) => None,
    }
}
/// A short label for completion details, e.g. `Int Function`.
pub(crate) fn member_kind_label(kind: MemberKind) -> &'static str {
    match kind {
        MemberKind::Function => "Function",
        MemberKind::Event => "Event",
        MemberKind::Property => "Property",
        MemberKind::Variable => "Variable",
        MemberKind::State => "State",
    }
}

/// Runs `f` with semantic access to `file`.
pub(crate) fn with_sema<R>(
    ws: &crate::Workspace,
    file: FileId,
    f: impl FnOnce(&Sema<'_>) -> R,
) -> Option<R> {
    let src = FileSource::load(ws, file)?;
    let sema = src.sema(ws, file);
    Some(f(&sema))
}

/// Returns `true` if `node` is inside a function or event body.
pub(crate) fn in_body(node: SyntaxNode<'_>) -> bool {
    std::iter::once(node).chain(node.ancestors()).any(|n| n.kind() == SyntaxKind::BLOCK)
}
