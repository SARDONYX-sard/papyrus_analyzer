//! Go to definition, find references and document highlights.

use papyrus_intern::Name;
use papyrus_parser::{
    SyntaxKind, SyntaxNode, TextRange, TextSize,
    ast::{self, AstNode},
};

use super::{decl_def, def_at, def_location, with_sema};
use crate::{
    FileId, Workspace,
    index::MemberKind,
    mod_events::{
        ModEventRef, ModEventSite, ModEventUse, mod_event_at, mod_event_ref, names_callable, sites,
    },
    sema::{Def, FileSource, Sema},
};

/// A file that can be navigated to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NavFile {
    /// A script.
    Script(FileId),
    /// The flags file, see [`Workspace::flags`].
    Flags,
}

/// A location to navigate to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NavTarget {
    /// The target file.
    pub file: NavFile,
    /// The range of the declaring name, to select.
    pub focus_range: TextRange,
    /// The range of the whole declaration.
    pub full_range: TextRange,
}

/// The declarations of the name at `offset`.
pub fn goto_definition(ws: &Workspace, file: FileId, offset: TextSize) -> Vec<NavTarget> {
    with_sema(ws, file, |sema| {
        // `GoToState("Busy")`: the states of that name.
        if let Some(state) = crate::states::state_ref_at(sema, offset) {
            return crate::states::candidates(sema, &state)
                .into_iter()
                .filter(|d| d.name.as_str().eq_ignore_ascii_case(state.name))
                .filter_map(|d| def_location(sema, Def::Member { file: d.file, index: d.index }))
                .map(|(file, focus_range, full_range)| NavTarget { file, focus_range, full_range })
                .collect();
        }
        // `SendModEvent("MyEvent")`: who registers it; the other way around too.
        if let Some(event) = event_name_at(sema, offset) {
            let wanted = match event.usage {
                ModEventUse::Send => ModEventUse::Register,
                _ => ModEventUse::Send,
            };
            return same_event(ws, event.name)
                .filter(|s| s.usage == wanted)
                .map(|s| NavTarget {
                    file: NavFile::Script(s.file),
                    focus_range: s.range,
                    full_range: s.range,
                })
                .collect();
        }
        let Some((_, def)) = def_at(sema, offset) else { return Vec::new() };
        // A script may be declared by several files, e.g. two versions of a
        // library in different import directories.
        if let Def::Script { file } = def
            && let Some(name) = ws.file(file).index.name
        {
            return ws
                .scripts_named(name)
                .iter()
                .filter_map(|&f| {
                    let range = ws.file(f).index.name_range?;
                    Some(NavTarget {
                        file: NavFile::Script(f),
                        focus_range: range,
                        full_range: range,
                    })
                })
                .collect();
        }
        def_location(sema, def)
            .map(|(file, focus_range, full_range)| NavTarget { file, focus_range, full_range })
            .into_iter()
            .collect()
    })
    .unwrap_or_default()
}
/// Where the function or event at `offset` is first declared: for a function
/// of a state, the one outside of states; for an override, the function of
/// the parent script it overrides (the nearest one up the `extends` chain).
/// Anything else, or a function declared nowhere else, goes to its
/// definition.
pub fn goto_declaration(ws: &Workspace, file: FileId, offset: TextSize) -> Vec<NavTarget> {
    let parent = with_sema(ws, file, |sema| {
        let (_, Def::Member { file: f, index }) = def_at(sema, offset)? else { return None };
        let member = sema.member(f, index);
        if !member.is_callable() || member.property.is_some() {
            return None;
        }
        let index_of = |f: FileId| &ws.file(f).index;
        let same = |m: &crate::index::Member| {
            m.is_callable() && m.property.is_none() && m.state.is_none() && m.name == member.name
        };
        // A state function: its declaration outside of states.
        if member.state.is_some()
            && let Some(i) = index_of(f).members.iter().position(same)
        {
            return def_location(sema, Def::Member { file: f, index: i as u32 });
        }
        let script = index_of(f).name?;
        sema.chain(script).skip(1).find_map(|(pf, idx)| {
            let i = idx.members.iter().position(same)?;
            def_location(sema, Def::Member { file: pf, index: i as u32 })
        })
    })
    .flatten();
    match parent {
        Some((file, focus_range, full_range)) => vec![NavTarget { file, focus_range, full_range }],
        None => goto_definition(ws, file, offset),
    }
}

/// An occurrence of a name in the current file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Highlight {
    /// The identifier range.
    pub range: TextRange,
    /// The occurrence declares or assigns the name.
    pub write: bool,
}

/// All occurrences of the name at `offset` in the same file.
pub fn highlight(ws: &Workspace, file: FileId, offset: TextSize) -> Vec<Highlight> {
    with_sema(ws, file, |sema| {
        if let Some(event) = event_name_at(sema, offset) {
            return sema
                .root
                .descendant_tokens()
                .filter_map(|t| mod_event_ref(sema, t))
                .filter(|r| {
                    r.usage != ModEventUse::Callback && r.name.eq_ignore_ascii_case(event.name)
                })
                .map(|r| Highlight { range: r.name_range, write: false })
                .collect();
        }
        let Some((_, def)) = def_at(sema, offset) else { return Vec::new() };
        let Some(name) = def_name(sema, def) else { return Vec::new() };
        occurrences(sema, def, name)
    })
    .unwrap_or_default()
}

/// All references to the name at `offset`, in every known file.
pub fn references(
    ws: &Workspace,
    file: FileId,
    offset: TextSize,
    include_declaration: bool,
) -> Vec<(NavFile, TextRange)> {
    let event = with_sema(ws, file, |sema| event_name_at(sema, offset).map(|e| e.name.to_owned()));
    if let Some(Some(event)) = event {
        return same_event(ws, &event).map(|s| (NavFile::Script(s.file), s.range)).collect();
    }
    let Some(Some((def, name))) = with_sema(ws, file, |sema| {
        let (_, def) = def_at(sema, offset)?;
        Some((def, def_name(sema, def)?))
    }) else {
        return Vec::new();
    };
    find_references(ws, def, name, include_declaration)
}

/// The name of a custom event at `offset`, not its callback.
fn event_name_at<'a>(sema: &Sema<'a>, offset: TextSize) -> Option<ModEventRef<'a>> {
    mod_event_at(sema, offset).filter(|r| r.usage != ModEventUse::Callback && !r.name.is_empty())
}

/// The strings naming the custom event `name`, in every known script.
fn same_event(ws: &Workspace, name: &str) -> impl Iterator<Item = ModEventSite> {
    let name = name.to_owned();
    sites(ws).into_iter().filter(move |s| s.name.eq_ignore_ascii_case(&name))
}

/// All uses of the flag defined at `offset` in the flags file.
pub fn flag_references(
    ws: &Workspace,
    offset: TextSize,
    include_declaration: bool,
) -> Vec<(NavFile, TextRange)> {
    let flags = ws.flags();
    let Some(i) = flags.def_at(offset) else { return Vec::new() };
    find_references(ws, Def::Flag(i as u32), flags.defs[i].name, include_declaration)
}

pub(crate) fn find_references(
    ws: &Workspace,
    def: Def,
    name: Name,
    include_declaration: bool,
) -> Vec<(NavFile, TextRange)> {
    let keep = |h: &Highlight, f: FileId| {
        include_declaration || !h.write || !is_declaration(ws, def, f, h.range)
    };

    if let Def::Local { file, .. } = def {
        return with_sema(ws, file, |sema| {
            occurrences(sema, def, name)
                .into_iter()
                .filter(|h| keep(h, file))
                .map(|h| (NavFile::Script(file), h.range))
                .collect()
        })
        .unwrap_or_default();
    }

    let needle = name.as_str().to_ascii_lowercase();
    // Only the files whose index has the name: the others are not read.
    let files: Vec<FileId> =
        ws.files().filter(|(_, f)| f.index.mentions(name)).map(|(id, _)| id).collect();
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).min(16);
    let chunk = files.len().div_ceil(threads).max(1);

    let mut out: Vec<(NavFile, TextRange)> = std::thread::scope(|s| {
        let handles: Vec<_> = files
            .chunks(chunk)
            .map(|chunk| {
                let needle = &needle;
                let keep = &keep;
                s.spawn(move || {
                    let mut found = Vec::new();
                    for &f in chunk {
                        let Some(src) = FileSource::load_if(ws, f, |text| {
                            contains_ignore_ascii_case(text, needle)
                        }) else {
                            continue;
                        };
                        let sema = src.sema(ws, f);
                        found.extend(
                            occurrences(&sema, def, name)
                                .into_iter()
                                .filter(|h| keep(h, f))
                                .map(|h| (NavFile::Script(f), h.range)),
                        );
                    }
                    found
                })
            })
            .collect();
        handles.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
    });
    if let Def::Flag(i) = def
        && include_declaration
        && ws.flags().path.is_some()
    {
        out.push((NavFile::Flags, ws.flags().defs[i as usize].name_range));
    }
    out.sort_by_key(|&(f, r)| (f, r.start()));
    out.dedup();
    out
}
fn is_declaration(ws: &Workspace, def: Def, file: FileId, range: TextRange) -> bool {
    match def {
        Def::Local { range: r, file: f, .. } => f == file && r == range,
        Def::Member { file: f, index } => {
            f == file && ws.file(f).index.members[index as usize].name_range == range
        }
        Def::Script { file: f } => f == file && ws.file(f).index.name_range == Some(range),
        Def::Param { .. } | Def::Builtin(..) | Def::Flag(_) | Def::LanguageFlag(_) => false,
    }
}

pub(crate) fn def_name(sema: &Sema<'_>, def: Def) -> Option<Name> {
    Some(match def {
        Def::Local { name, .. } => name,
        Def::Member { file, index } => sema.member(file, index).name,
        Def::Param { file, member, param } => sema.member(file, member).params[param as usize].name,
        Def::Script { file } => sema.ws.file(file).index.name?,
        Def::Builtin(b, _) => Name::new(b.name()),
        Def::Flag(i) => sema.ws.flags().defs.get(i as usize)?.name,
        Def::LanguageFlag(kind) => Name::new(super::language_flag_docs(kind).0),
    })
}

/// Occurrences of `def` (spelled `name`) in the file of `sema`.
fn occurrences(sema: &Sema<'_>, def: Def, name: Name) -> Vec<Highlight> {
    let mut out = Vec::new();
    // A state is named by strings too: `GoToState("Busy")`.
    let state = match def {
        Def::Member { file, index } if sema.member(file, index).kind == MemberKind::State => {
            Some(file)
        }
        _ => None,
    };
    // So is a callback: `RegisterForModEvent("MyEvent", "OnMyEvent")`.
    let callable =
        matches!(def, Def::Member { file, index } if sema.member(file, index).is_callable());
    for tok in sema.root.descendant_tokens() {
        let named = tok.kind() == SyntaxKind::STRING
            && tok.text().trim_matches('"').eq_ignore_ascii_case(name.as_str());
        if named
            && let Some(file) = state
            && let Some(r) = crate::states::state_ref(sema, tok)
            && crate::states::refers_to(sema, &r, file)
        {
            out.push(Highlight { range: r.name_range, write: false });
            continue;
        }
        if named
            && callable
            && let Some(r) = mod_event_ref(sema, tok)
            && names_callable(sema, &r, def)
        {
            out.push(Highlight { range: r.name_range, write: false });
            continue;
        }
        let Some(parent) = tok.parent() else { continue };
        let flag_keyword = tok.kind().is_keyword() && parent.kind() == SyntaxKind::FLAG;
        if !matches!(tok.kind(), SyntaxKind::IDENT | SyntaxKind::SELF_KW | SyntaxKind::PARENT_KW)
            && !flag_keyword
        {
            continue;
        }
        if !tok.text().eq_ignore_ascii_case(name.as_str()) {
            continue;
        }
        let (resolved, write) = match parent.kind() {
            SyntaxKind::FLAG => (super::flag_def(sema, tok), false),
            SyntaxKind::NAME => (decl_def(sema, parent), true),
            SyntaxKind::NAME_REF => {
                let Some(nr) = ast::NameRef::cast(parent) else { continue };
                (sema.resolve_name_ref(nr), is_assign_target(parent))
            }
            SyntaxKind::TYPE => (ast::Type::cast(parent).and_then(|t| sema.resolve_type(t)), false),
            _ => continue,
        };
        if resolved == Some(def) {
            out.push(Highlight { range: tok.text_range(), write });
        }
    }
    out
}

fn is_assign_target(name_ref: SyntaxNode<'_>) -> bool {
    let Some(parent) = name_ref.parent() else { return false };
    let target = match parent.kind() {
        SyntaxKind::ASSIGN_STMT => Some(name_ref),
        SyntaxKind::FIELD_EXPR
            if ast::FieldExpr::cast(parent).and_then(|f| f.name_ref()).map(|n| n.syntax())
                == Some(name_ref) =>
        {
            parent.parent().filter(|p| p.kind() == SyntaxKind::ASSIGN_STMT).map(|_| parent)
        }
        _ => None,
    };
    target.is_some_and(|t| {
        t.parent()
            .and_then(ast::AssignStmt::cast)
            .and_then(|a| a.target())
            .is_some_and(|e| e.syntax() == t)
    })
}

fn contains_ignore_ascii_case(haystack: &str, needle_lower: &str) -> bool {
    let n = needle_lower.as_bytes();
    if n.is_empty() {
        return true;
    }
    haystack
        .as_bytes()
        .windows(n.len())
        .any(|w| w.iter().zip(n).all(|(a, b)| a.to_ascii_lowercase() == *b))
}

impl FileSource {
    /// Like [`FileSource::load`], but skips parsing when `filter` rejects the text.
    pub(crate) fn load_if(
        ws: &Workspace,
        file: FileId,
        filter: impl FnOnce(&str) -> bool,
    ) -> Option<FileSource> {
        if ws.file(file).open.is_some() {
            return FileSource::load(ws, file).filter(|src| filter(src.text()));
        }
        let text = ws.text(file)?;
        if !filter(&text) {
            return None;
        }
        Some(FileSource::from_text(text))
    }
}
