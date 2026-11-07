//! Renaming a name everywhere it is used.
//!
//! - Functions and events are renamed with their family: the declaration of
//!   the parent script they override and the overrides of child scripts, so
//!   that overriding keeps working. The events the game sends (native events
//!   of the engine scripts) cannot be renamed.
//! - Parameters are renamed in named arguments of calls too.
//! - A script is renamed with its file, `Old.psc` becoming `New.psc`.
//! - A state is renamed in the scripts sharing it, and in the strings naming
//!   it: `GoToState("Busy")`, `GetState() == "Busy"`.

use papyrus_intern::Name;
use papyrus_parser::{
    SyntaxKind, TextRange, TextSize,
    ast::{self, AstNode},
};

use super::{
    def_at,
    navigation::{NavFile, def_name, find_references},
    overrides, with_sema,
};
use crate::{
    FileId, Origin, Workspace,
    index::{Flags, MemberKind},
    sema::{Def, LocalKind, Sema},
};

/// The edits of a rename.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RenameEdit {
    /// Ranges to replace by the new name, per file.
    pub ranges: Vec<(NavFile, TextRange)>,
    /// Script files to rename: the file and its new path.
    pub file_renames: Vec<(FileId, std::path::PathBuf)>,
}

/// The name at `offset` and its range, when it can be renamed.
pub fn prepare_rename(
    ws: &Workspace,
    file: FileId,
    offset: TextSize,
) -> Result<(TextRange, String), String> {
    with_sema(ws, file, |sema| {
        let (range, def) = def_at(sema, offset).ok_or("nothing to rename here")?;
        check_renamable(sema, def)?;
        Ok((range, sema.text[range].to_owned()))
    })
    .unwrap_or_else(|| Err("unknown file".to_owned()))
}

/// Renames the name at `offset` to `new_name`.
pub fn rename(
    ws: &Workspace,
    file: FileId,
    offset: TextSize,
    new_name: &str,
) -> Result<RenameEdit, String> {
    check_name(new_name)?;
    let defs = with_sema(ws, file, |sema| {
        let (_, def) = def_at(sema, offset).ok_or("nothing to rename here")?;
        check_renamable(sema, def)?;
        let mut defs = family(sema, def);
        defs.extend(param_forms(sema, def));
        let named: Vec<(Def, Name)> =
            defs.into_iter().filter_map(|d| Some((d, def_name(sema, d)?))).collect();
        Ok::<_, String>((def, named))
    })
    .unwrap_or_else(|| Err("unknown file".to_owned()))?;
    let (def, defs) = defs;

    let mut edit = RenameEdit::default();
    for (d, name) in defs {
        // Uses in imported scripts stay as they are: those files are not ours.
        edit.ranges.extend(find_references(ws, d, name, true).into_iter().filter(
            |(f, _)| matches!(f, NavFile::Script(f) if ws.file(*f).origin == Origin::Workspace),
        ));
    }
    edit.ranges.sort_by_key(|&(f, r)| (f, r.start()));
    edit.ranges.dedup();

    if let Def::Script { file: script } = def {
        let path = &ws.file(script).path;
        let old = ws.file(script).index.name.map(|n| n.to_string()).unwrap_or_default();
        if path.file_stem().is_some_and(|s| s.to_string_lossy().eq_ignore_ascii_case(&old)) {
            edit.file_renames.push((script, path.with_file_name(format!("{new_name}.psc"))));
        }
    }
    Ok(edit)
}

/// A valid identifier that is not a keyword.
fn check_name(name: &str) -> Result<(), String> {
    let mut chars = name.chars();
    let valid = chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !valid {
        return Err(format!("`{name}` is not a valid name"));
    }
    if papyrus_lexer::keyword(name).is_some() {
        return Err(format!("`{name}` is a keyword"));
    }
    if name.eq_ignore_ascii_case("Length") {
        return Err("`Length` is reserved".to_owned());
    }
    Ok(())
}

fn check_renamable(sema: &Sema<'_>, def: Def) -> Result<(), String> {
    match def {
        Def::Builtin(..) => return Err("built-in names cannot be renamed".to_owned()),
        Def::LanguageFlag(_) => return Err("keywords cannot be renamed".to_owned()),
        // The flags file belongs to the game or the compiler.
        Def::Flag(..) => return Err("flags of the flags file cannot be renamed".to_owned()),
        _ => {}
    }
    // Only the scripts of the workspace may change: names declared by
    // imported scripts (the game, other mods) are not ours.
    for d in family(sema, def) {
        let Some(file) = declaring_file(d) else { continue };
        let data = sema.ws.file(file);
        if data.origin != Origin::Workspace {
            return Err(format!(
                "`{}` is declared by `{}`, an imported script; only the scripts of the workspace can be renamed",
                def_name(sema, d).map_or_else(String::new, |n| n.to_string()),
                data.path.display()
            ));
        }
        if let Def::Member { file, index } = d {
            let m = sema.member(file, index);
            if m.kind == MemberKind::Event && m.flags.contains(Flags::NATIVE) {
                return Err(format!(
                    "`{}` is an event sent by the game (declared by `{}`)",
                    m.name,
                    sema.ws.file(file).index.name_str()
                ));
            }
        }
    }
    Ok(())
}

/// The file declaring a definition.
fn declaring_file(def: Def) -> Option<FileId> {
    match def {
        Def::Local { file, .. }
        | Def::Member { file, .. }
        | Def::Param { file, .. }
        | Def::Script { file } => Some(file),
        Def::Builtin(..) | Def::Flag(..) | Def::LanguageFlag(_) => None,
    }
}
/// `def` and, for functions and events, the declarations it overrides and
/// those overriding it.
fn family(sema: &Sema<'_>, def: Def) -> Vec<Def> {
    let Def::Member { file, index } = def else { return vec![def] };
    let m = sema.member(file, index);
    if m.kind == MemberKind::State {
        return crate::states::state_family(sema, file, index);
    }
    if !m.is_callable() || m.property.is_some() {
        return vec![def];
    }
    let ws = sema.ws;
    // The topmost declaration up the `extends` chain.
    let script = ws.file(file).index.name;
    let mut top = (file, index);
    if let Some(script) = script {
        for (f, idx) in sema.chain(script).skip(1) {
            if let Some(i) = idx.members.iter().position(|o| same(o, m)) {
                top = (f, i as u32);
            }
        }
    }
    let mut out = vec![Def::Member { file: top.0, index: top.1 }];
    // Every override below it, in all child scripts.
    for o in overrides::overridden(ws, top.0) {
        if o.name_range != ws.file(top.0).index.members[top.1 as usize].name_range {
            continue;
        }
        for t in o.by {
            let NavFile::Script(f) = t.file else { continue };
            if let Some(i) =
                ws.file(f).index.members.iter().position(|c| c.name_range == t.focus_range)
            {
                out.push(Def::Member { file: f, index: i as u32 });
            }
        }
    }
    // Functions of states replace the one of the empty state.
    let mut states = Vec::new();
    for d in &out {
        let Def::Member { file: f, .. } = *d else { continue };
        for (i, s) in ws.file(f).index.members.iter().enumerate() {
            if s.state.is_some() && s.is_callable() && s.property.is_none() && s.name == m.name {
                states.push(Def::Member { file: f, index: i as u32 });
            }
        }
    }
    out.extend(states);
    if !out.contains(&def) {
        out.push(def);
    }
    out.sort_by_key(|d| match d {
        Def::Member { file, index } => (file.0, *index),
        _ => (u32::MAX, 0),
    });
    out.dedup();
    out
}

fn same(a: &crate::index::Member, b: &crate::index::Member) -> bool {
    a.is_callable() && a.property.is_none() && a.state.is_none() && a.name == b.name
}

/// A parameter is referenced as a local in its body and as [`Def::Param`] by
/// named arguments: both are renamed.
fn param_forms(sema: &Sema<'_>, def: Def) -> Vec<Def> {
    match def {
        Def::Local { file, range, kind: LocalKind::Parameter, name, .. } => {
            // The function declaring the parameter.
            let index = sema.ws.file(file).index.members.iter().position(|m| {
                m.full_range.contains_range(range) && m.params.iter().any(|p| p.name == name)
            });
            let Some(member) = index else { return Vec::new() };
            let m = &sema.ws.file(file).index.members[member];
            let Some(param) = m.params.iter().position(|p| p.name == name) else {
                return Vec::new();
            };
            vec![Def::Param { file, member: member as u32, param: param as u32 }]
        }
        Def::Param { file, member, param } => {
            let m = sema.member(file, member);
            let p = &m.params[param as usize];
            // The declaring name, from the syntax of the declaring file.
            let Some(src) = crate::sema::FileSource::load(sema.ws, file) else { return Vec::new() };
            let other = src.sema(sema.ws, file);
            let range = other
                .root
                .descendants()
                .filter(|n| matches!(n.kind(), SyntaxKind::FUNCTION | SyntaxKind::EVENT))
                .find(|n| {
                    n.children()
                        .find_map(ast::Name::cast)
                        .is_some_and(|nm| nm.syntax().text_range() == m.name_range)
                })
                .and_then(|n| n.children().find_map(ast::ParamList::cast))
                .and_then(|l| l.params().nth(param as usize))
                .and_then(|p| p.name())
                .map(|n| n.syntax().text_range());
            let Some(range) = range else { return Vec::new() };
            vec![Def::Local { file, name: p.name, range, ty: p.ty, kind: LocalKind::Parameter }]
        }
        _ => Vec::new(),
    }
}
