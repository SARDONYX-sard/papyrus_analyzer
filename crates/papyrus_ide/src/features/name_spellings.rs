//! The spelling of the definition of each name, for the formatter.
//!
//! Papyrus ignores case, so `actor`, `getvalue()` and `event oninit()` work;
//! the formatter writes them like their definition: `Actor` (its
//! `ScriptName`), `GetValue()` (the function), `Event OnInit()` (the event
//! of the parent script it overrides).

use papyrus_parser::{
    SyntaxKind, TextRange,
    ast::{self, AstNode},
};

use super::{decl_def, flag_def, overrides::is_overridable, with_sema};
use crate::{
    FileId, Workspace,
    index::MemberKind,
    sema::{Def, Sema},
};

/// The kind of a name, each with its own case setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NameKind {
    /// A script: types, `extends`, `Import`, receivers like `Debug.Trace`.
    Script,
    /// A function or an event.
    Function,
    /// A property.
    Property,
    /// A local variable, a parameter or a script variable.
    Variable,
    /// A user flag such as `Hidden`.
    Flag,
}

/// The definition of the identifier at `range`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NameSpelling {
    /// The identifier.
    pub range: TextRange,
    /// What it names.
    pub kind: NameKind,
    /// The spelling of the definition.
    pub spelling: String,
}

/// The definitions of the identifiers of `file` that resolve, in source order.
pub fn name_spellings(ws: &Workspace, file: FileId) -> Vec<NameSpelling> {
    with_sema(ws, file, |sema| {
        sema.root
            .descendant_tokens()
            .filter(|t| t.kind() == SyntaxKind::IDENT)
            .filter_map(|tok| {
                let parent = tok.parent()?;
                let (kind, spelling) = match parent.kind() {
                    SyntaxKind::FLAG => def_spelling(sema, flag_def(sema, tok)?)?,
                    SyntaxKind::TYPE => {
                        def_spelling(sema, sema.resolve_type(ast::Type::cast(parent)?)?)?
                    }
                    SyntaxKind::NAME_REF => {
                        def_spelling(sema, sema.resolve_name_ref(ast::NameRef::cast(parent)?)?)?
                    }
                    // A declaration is its own definition, unless it overrides a
                    // function of a parent script.
                    SyntaxKind::NAME => def_spelling(sema, decl_def(sema, parent)?)?,
                    _ => return None,
                };
                Some(NameSpelling { range: tok.text_range(), kind, spelling })
            })
            .collect()
    })
    .unwrap_or_default()
}

/// The name of the function of a parent script that member `index` of
/// `file` overrides.
fn overridden_name(sema: &Sema<'_>, file: FileId, index: u32) -> Option<String> {
    let m = sema.member(file, index);
    if !is_overridable(m) {
        return None;
    }
    // The first declaration, up the chain: every override takes its spelling.
    let script = sema.ws.file(file).index.name?;
    sema.chain(script)
        .skip(1)
        .filter_map(|(_, parent)| {
            let base = parent.members.iter().find(|b| is_overridable(b) && b.name == m.name)?;
            Some(base.name.to_string())
        })
        .last()
}

fn def_spelling(sema: &Sema<'_>, def: Def) -> Option<(NameKind, String)> {
    Some(match def {
        Def::Local { name, .. } => (NameKind::Variable, name.to_string()),
        Def::Param { file, member, param } => {
            (NameKind::Variable, sema.member(file, member).params[param as usize].name.to_string())
        }
        Def::Member { file, index } => {
            let m = sema.member(file, index);
            let kind = match m.kind {
                MemberKind::Function | MemberKind::Event => NameKind::Function,
                MemberKind::Property => NameKind::Property,
                MemberKind::Variable => NameKind::Variable,
                MemberKind::State => return None,
            };
            let name = overridden_name(sema, file, index).unwrap_or_else(|| m.name.to_string());
            (kind, name)
        }
        Def::Script { file } => (NameKind::Script, sema.ws.file(file).index.name?.to_string()),
        Def::Builtin(b, _) => {
            let kind = if b.is_callable() { NameKind::Function } else { NameKind::Property };
            (kind, b.name().to_owned())
        }
        Def::Flag(table, i) => {
            (NameKind::Flag, sema.ws.flag_table(table).defs.get(i as usize)?.name.to_string())
        }
        Def::LanguageFlag(_) => return None,
    })
}
