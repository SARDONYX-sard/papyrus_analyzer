//! Document outline and workspace-wide symbol search.

use papyrus_parser::TextRange;

use crate::{
    FileId, Workspace,
    flags::FlagTable,
    index::{Member, MemberKind, ScriptIndex},
};

/// The kind of a symbol.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SymbolKind {
    /// A script.
    Script,
    /// A function.
    Function,
    /// An event.
    Event,
    /// A property.
    Property,
    /// A script variable.
    Variable,
    /// A state.
    State,
    /// A flag of the flags file.
    Flag,
}

impl From<MemberKind> for SymbolKind {
    fn from(kind: MemberKind) -> SymbolKind {
        match kind {
            MemberKind::Function => SymbolKind::Function,
            MemberKind::Event => SymbolKind::Event,
            MemberKind::Property => SymbolKind::Property,
            MemberKind::Variable => SymbolKind::Variable,
            MemberKind::State => SymbolKind::State,
        }
    }
}

/// A node of the document outline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentSymbol {
    /// The name.
    pub name: String,
    /// The signature.
    pub detail: Option<String>,
    /// The kind.
    pub kind: SymbolKind,
    /// The whole declaration.
    pub full_range: TextRange,
    /// The declaring name.
    pub focus_range: TextRange,
    /// Nested symbols.
    pub children: Vec<DocumentSymbol>,
}

/// The outline of a file: the script, containing its declarations.
pub fn document_symbols(ws: &Workspace, file: FileId) -> Vec<DocumentSymbol> {
    let data = ws.file(file);
    let index: &ScriptIndex = &data.index;
    let symbol = |m: &Member| DocumentSymbol {
        name: m.name.to_string(),
        detail: Some(m.signature()),
        kind: m.kind.into(),
        full_range: m.full_range,
        focus_range: m.name_range,
        children: Vec::new(),
    };

    let mut top: Vec<DocumentSymbol> = Vec::new();
    for m in index.members.iter() {
        let sym = symbol(m);
        if let Some(prop) = m.property {
            // Accessor: nest under its property, which may itself be in a state.
            if let Some(parent) = find_last(&mut top, SymbolKind::Property, prop.as_str()) {
                parent.children.push(sym);
                continue;
            }
        }
        if let Some(state) = m.state {
            let parent = top.iter_mut().rev().find(|t| {
                t.kind == SymbolKind::State && t.name.eq_ignore_ascii_case(state.as_str())
            });
            if let Some(parent) = parent {
                parent.children.push(sym);
                continue;
            }
        }
        top.push(sym);
    }

    let Some(name) = index.name else { return top };
    let focus = index.name_range.unwrap_or_default();
    let end = top.iter().map(|s| s.full_range.end()).max().unwrap_or(focus.end()).max(focus.end());
    let mut detail = format!("ScriptName {name}");
    if let Some(ext) = index.extends {
        detail.push_str(&format!(" extends {ext}"));
    }
    crate::index::render_flags(&mut detail, index.flags, &index.user_flags);
    vec![DocumentSymbol {
        name: name.to_string(),
        detail: Some(detail),
        kind: SymbolKind::Script,
        full_range: TextRange::new(0.into(), end),
        focus_range: focus,
        children: top,
    }]
}

/// The outline of the flags file.
pub fn flag_file_symbols(flags: &FlagTable) -> Vec<DocumentSymbol> {
    flags
        .defs
        .iter()
        .map(|d| DocumentSymbol {
            name: d.name.to_string(),
            detail: Some(d.signature()),
            kind: SymbolKind::Flag,
            full_range: d.full_range,
            focus_range: d.name_range,
            children: Vec::new(),
        })
        .collect()
}

/// The last symbol of `kind` named `name`, searching nested symbols too.
fn find_last<'s>(
    symbols: &'s mut [DocumentSymbol],
    kind: SymbolKind,
    name: &str,
) -> Option<&'s mut DocumentSymbol> {
    for sym in symbols.iter_mut().rev() {
        if sym.kind == kind && sym.name.eq_ignore_ascii_case(name) {
            return Some(sym);
        }
        if sym.kind == SymbolKind::State
            && let Some(found) = find_last(&mut sym.children, kind, name)
        {
            return Some(found);
        }
    }
    None
}

/// A symbol found by [`workspace_symbols`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceSymbol {
    /// The name.
    pub name: String,
    /// The kind.
    pub kind: SymbolKind,
    /// The declaring script.
    pub container: Option<String>,
    /// The file.
    pub file: FileId,
    /// The declaring name.
    pub range: TextRange,
}

/// Searches scripts and their declarations whose name fuzzily matches `query`.
pub fn workspace_symbols(ws: &Workspace, query: &str, limit: usize) -> Vec<WorkspaceSymbol> {
    let query = query.to_ascii_lowercase();
    let mut scored: Vec<(u32, WorkspaceSymbol)> = Vec::new();
    for (file, data) in ws.files() {
        let index = &data.index;
        if let (Some(name), Some(range)) = (index.name, index.name_range)
            && let Some(score) = fuzzy_score(name.as_str(), &query)
        {
            scored.push((
                score,
                WorkspaceSymbol {
                    name: name.to_string(),
                    kind: SymbolKind::Script,
                    container: None,
                    file,
                    range,
                },
            ));
        }
        for m in index.members.iter() {
            if m.property.is_some() {
                continue;
            }
            if let Some(score) = fuzzy_score(m.name.as_str(), &query) {
                scored.push((
                    score + 1,
                    WorkspaceSymbol {
                        name: m.name.to_string(),
                        kind: m.kind.into(),
                        container: index.name.map(|n| n.to_string()),
                        file,
                        range: m.name_range,
                    },
                ));
            }
        }
    }
    scored.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.name.len().cmp(&b.1.name.len())));
    scored.into_iter().take(limit).map(|(_, s)| s).collect()
}

/// Scores `name` against a lowercase `query`: lower is better, `None` if the
/// query is not a subsequence of the name.
fn fuzzy_score(name: &str, query: &str) -> Option<u32> {
    if query.is_empty() {
        return Some(10);
    }
    let lower = name.to_ascii_lowercase();
    if lower == query {
        return Some(0);
    }
    if lower.starts_with(query) {
        return Some(2);
    }
    if lower.contains(query) {
        return Some(4);
    }
    let mut chars = lower.chars();
    query.chars().all(|q| chars.any(|c| c == q)).then_some(8)
}
