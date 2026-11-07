//! Which functions override a function of a parent script, and which are
//! overridden by child scripts.
//!
//! Papyrus has no `override` keyword: a function or event declared with the
//! name of one of a parent script replaces it. The editor shows it:
//!
//! - an inlay hint `overrides Quest` after the declaration, or `event of
//!   Form` for the events the game sends (native events of the engine
//!   scripts);
//! - a code lens `overridden in N scripts` above functions children
//!   replace;
//! - a line in the hover.

use papyrus_intern::Name;
use papyrus_parser::{SyntaxKind, SyntaxNode, TextRange, TextSize};

use super::{def_location, navigation::NavTarget, with_sema};
use crate::{
    FileId, Workspace,
    index::{Flags, Member, MemberKind, ScriptIndex},
    sema::{Def, Sema},
};

/// What a declaration replaces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverrideKind {
    /// A function or event of a parent script.
    Override,
    /// An event the game sends, declared `Native` by an engine script.
    EngineEvent,
}

/// A function or event replacing one of a parent script.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Override {
    /// The name of the declaration in the current file.
    pub name_range: TextRange,
    /// Where the hint goes: the end of the declaration line.
    pub hint_offset: TextSize,
    /// The script declaring the replaced function.
    pub base_script: Name,
    /// The replaced function.
    pub base: NavTarget,
    /// What is replaced.
    pub kind: OverrideKind,
}

/// A function or event that child scripts replace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Overridden {
    /// The name of the declaration in the current file.
    pub name_range: TextRange,
    /// The replacing declarations.
    pub by: Vec<NavTarget>,
}

/// A function or event outside of states (states replace functions of the
/// same script, which is not an override of a parent).
pub(crate) fn is_overridable(m: &Member) -> bool {
    m.is_callable() && m.property.is_none() && m.state.is_none()
}

/// The functions and events of `file` that override a parent script.
pub fn overrides(ws: &Workspace, file: FileId) -> Vec<Override> {
    with_sema(ws, file, |sema| {
        let Some(script) = sema.script_name() else { return Vec::new() };
        let mut out = Vec::new();
        for m in sema.index.members.iter().filter(|m| is_overridable(m)) {
            let Some((base_script, base, kind)) = base_of(sema, script, m.name) else { continue };
            let hint_offset = header_end(sema, m.name_range).unwrap_or(m.name_range.end());
            out.push(Override { name_range: m.name_range, hint_offset, base_script, base, kind });
        }
        out
    })
    .unwrap_or_default()
}

/// The nearest function `name` up the `extends` chain of `script`.
fn base_of(sema: &Sema<'_>, script: Name, name: Name) -> Option<(Name, NavTarget, OverrideKind)> {
    sema.chain(script).skip(1).find_map(|(file, index)| {
        let i = index.members.iter().position(|m| is_overridable(m) && m.name == name)?;
        let m = &index.members[i];
        let kind = if m.kind == MemberKind::Event && m.flags.contains(Flags::NATIVE) {
            OverrideKind::EngineEvent
        } else {
            OverrideKind::Override
        };
        let (target, focus_range, full_range) =
            def_location(sema, Def::Member { file, index: i as u32 })?;
        Some((index.name?, NavTarget { file: target, focus_range, full_range }, kind))
    })
}

/// The end of the first line of the declaration whose name is at `name`.
fn header_end(sema: &Sema<'_>, name: TextRange) -> Option<TextSize> {
    let tok = sema.root.token_at_offset(name.start()).right_biased()?;
    let decl: SyntaxNode<'_> = tok
        .parent()?
        .ancestors()
        .find(|n| matches!(n.kind(), SyntaxKind::FUNCTION | SyntaxKind::EVENT))?;
    let mut end = None;
    let mut cur = decl.first_token();
    while let Some(t) = cur {
        if t.kind() == SyntaxKind::NEWLINE || t.text_range().start() >= decl.text_range().end() {
            break;
        }
        if !t.kind().is_trivia() {
            end = Some(t.text_range().end());
        }
        cur = t.next_token();
    }
    end
}

/// The functions and events of `file` that child scripts override, with the
/// overriding declarations.
pub fn overridden(ws: &Workspace, file: FileId) -> Vec<Overridden> {
    let index = &ws.file(file).index;
    let Some(script) = index.name else { return Vec::new() };
    let children: Vec<(FileId, &ScriptIndex)> = ws
        .files_from(file)
        .filter(|(f, data)| *f != file && extends(ws, &data.index, script))
        .map(|(f, d)| (f, &*d.index))
        .collect();
    if children.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for m in index.members.iter().filter(|m| is_overridable(m)) {
        let by: Vec<NavTarget> = children
            .iter()
            .filter_map(|(f, child)| {
                let c = child.members.iter().find(|c| is_overridable(c) && c.name == m.name)?;
                Some(NavTarget {
                    file: super::navigation::NavFile::Script(*f),
                    focus_range: c.name_range,
                    full_range: c.full_range,
                })
            })
            .collect();
        if !by.is_empty() {
            out.push(Overridden { name_range: m.name_range, by });
        }
    }
    out
}

/// Whether `index` extends `ancestor`, directly or not.
fn extends(ws: &Workspace, index: &ScriptIndex, ancestor: Name) -> bool {
    let mut next = index.extends;
    for _ in 0..64 {
        let Some(name) = next else { return false };
        if name == ancestor {
            return true;
        }
        next = ws.script(name).and_then(|f| ws.file(f).index.extends);
    }
    false
}

/// The line of the hover of a declaration: `Overrides Quest.OnInit`.
pub(crate) fn hover_line(sema: &Sema<'_>, def: Def) -> Option<String> {
    let Def::Member { file, index } = def else { return None };
    let m = sema.member(file, index);
    if !is_overridable(m) {
        return None;
    }
    let script = sema.ws.file(file).index.name?;
    let (base_script, _, kind) = base_of(sema, script, m.name)?;
    Some(match kind {
        OverrideKind::Override => format!("Overrides `{base_script}.{}`", m.name),
        OverrideKind::EngineEvent => format!("Event sent by the game, declared by `{base_script}`"),
    })
}
