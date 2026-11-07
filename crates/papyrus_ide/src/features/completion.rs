//! Code completion.
//!
//! The context is decided from the tokens left of the cursor:
//!
//! - after `receiver.`: the members of the receiver type;
//! - after `extends`, `Import`, `As` or `New`: script names;
//! - after `Event` or `Function` at script level: overridable declarations of
//!   the parent scripts, inserted with their full signature;
//! - inside a body: locals, members, imported global functions, scripts and
//!   statement keywords;
//! - at script level: declaration keywords, types and flags.

use papyrus_intern::Name;
use papyrus_parser::{
    SyntaxKind, SyntaxToken, TextRange, TextSize,
    ast::{self, AstNode},
};

use super::{in_body, member_kind_label, with_sema};
use crate::{
    FileId, Workspace,
    flags::{FlagTable, FlagTargets},
    index::{Member, MemberKind, render_param},
    sema::{FileSource, LocalKind, MemberItem, Sema, Ty},
};

/// The kind of a completion item, mapped to an icon by editors.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CompletionKind {
    /// A keyword.
    Keyword,
    /// A function.
    Function,
    /// An event.
    Event,
    /// A property.
    Property,
    /// A script variable.
    Variable,
    /// A local variable or parameter.
    Local,
    /// A script (type).
    Script,
    /// A primitive type.
    Type,
    /// A declaration flag.
    Flag,
    /// A state.
    State,
    /// A code snippet.
    Snippet,
}

/// A completion proposal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionItem {
    /// The displayed text, also inserted when `snippet` is `None`.
    pub label: String,
    /// The kind.
    pub kind: CompletionKind,
    /// A one line description, e.g. the signature.
    pub detail: Option<String>,
    /// Markdown documentation.
    pub documentation: Option<String>,
    /// Text in LSP snippet syntax (`$1`, `$0`), to insert instead of `label`.
    pub snippet: Option<String>,
    /// The range replaced by the completion.
    pub replace_range: TextRange,
    /// Sort key; lower sorts first.
    pub sort_text: String,
    /// A member whose documentation can be loaded lazily with [`member_docs`].
    pub resolve: Option<(FileId, u32)>,
}

/// The result of [`completions`].
#[derive(Clone, Debug, Default)]
pub struct Completions {
    /// The proposals.
    pub items: Vec<CompletionItem>,
    /// `true` if typing more characters may yield new proposals.
    pub is_incomplete: bool,
}

/// Maximum number of script names proposed at once.
const MAX_SCRIPTS: usize = 300;

const STATEMENT_KEYWORDS: &[&str] = &[
    "If", "ElseIf", "Else", "EndIf", "While", "EndWhile", "Return", "New", "As", "True", "False",
    "None", "Self", "Parent",
];
const DECL_KEYWORDS: &[&str] = &[
    "Function",
    "EndFunction",
    "Event",
    "EndEvent",
    "Property",
    "EndProperty",
    "State",
    "EndState",
    "Auto",
    "Import",
    "ScriptName",
    "Extends",
];
const PRIMITIVES: &[&str] = &["Bool", "Int", "Float", "String"];

/// Completion proposals at `offset`.
pub fn completions(ws: &Workspace, file: FileId, offset: TextSize) -> Completions {
    with_sema(ws, file, |sema| Ctx::new(sema, offset).complete()).unwrap_or_default()
}

/// Loads the documentation of a member, for lazily resolved items.
pub fn member_docs(ws: &Workspace, file: FileId, index: u32) -> Option<String> {
    let member = ws.file(file).index.members.get(index as usize)?;
    let src = FileSource::load(ws, file)?;
    let docs = crate::index::render_docs(src.text(), &member.docs);
    (!docs.is_empty()).then_some(docs)
}

struct Ctx<'s, 'a> {
    sema: &'s Sema<'a>,
    offset: TextSize,
    replace_range: TextRange,
    prefix: String,
    /// The significant token before the word being completed.
    prev: Option<SyntaxToken<'a>>,
    out: Completions,
}

impl<'s, 'a> Ctx<'s, 'a> {
    fn new(sema: &'s Sema<'a>, offset: TextSize) -> Ctx<'s, 'a> {
        let left = sema.root.token_at_offset(offset).left_biased();
        let word = left.filter(|t| {
            (t.kind() == SyntaxKind::IDENT || t.kind().is_keyword())
                && t.text_range().start() < offset
        });
        let (replace_range, prefix, before) = match word {
            Some(w) => {
                let range = TextRange::new(w.text_range().start(), offset);
                (w.text_range(), sema.text[range].to_owned(), w.prev_token())
            }
            None => (TextRange::empty(offset), String::new(), left),
        };
        let mut prev = before;
        while let Some(t) = prev {
            if t.kind() == SyntaxKind::WHITESPACE || t.kind().is_comment() {
                prev = t.prev_token();
            } else {
                break;
            }
        }
        // `left` may be the newline right after the word when offset == end.
        Ctx { sema, offset, replace_range, prefix, prev, out: Completions::default() }
    }

    fn complete(mut self) -> Completions {
        let prev_kind = self.prev.map(|t| t.kind());
        if let Some(state) = crate::states::state_ref_at(self.sema, self.offset) {
            self.add_states(&state);
            return self.out;
        }
        if let Some(event) = crate::mod_events::mod_event_at(self.sema, self.offset) {
            self.add_mod_event(&event);
            return self.out;
        }
        if self.in_comment_or_string() {
            return self.out;
        }
        if let Some(decl) = self.flag_context() {
            self.add_flags(decl);
            return self.out;
        }
        match prev_kind {
            Some(SyntaxKind::DOT) => self.complete_members(),
            Some(SyntaxKind::EXTENDS_KW | SyntaxKind::IMPORT_KW) => self.add_scripts(false),
            Some(SyntaxKind::AS_KW | SyntaxKind::NEW_KW) => {
                self.add_primitives();
                self.add_scripts(false);
            }
            Some(SyntaxKind::EVENT_KW) if !self.in_body() => self.add_overrides(MemberKind::Event),
            Some(SyntaxKind::FUNCTION_KW) if !self.in_body() => {
                self.add_overrides(MemberKind::Function);
            }
            _ if self.in_body() => self.complete_body(),
            _ => self.complete_item(),
        }
        self.out
    }

    fn in_comment_or_string(&self) -> bool {
        let Some(tok) = self.sema.root.token_at_offset(self.offset).left_biased() else {
            return false;
        };
        let inside = tok.text_range().start() < self.offset && self.offset < tok.text_range().end();
        // A string without its closing quote yet goes on at its end.
        let open_string = tok.kind() == SyntaxKind::STRING
            && (tok.text().len() == 1 || !tok.text().ends_with('"'));
        (tok.kind().is_comment() || tok.kind() == SyntaxKind::STRING) && inside
            || (tok.kind() == SyntaxKind::LINE_COMMENT || open_string)
                && self.offset == tok.text_range().end()
    }

    /// The states `GoToState("...")` can name: of the script and its parents,
    /// then of the scripts extending it.
    fn add_states(&mut self, state: &crate::states::StateRef<'a>) {
        let mut seen: Vec<Name> = Vec::new();
        for decl in crate::states::candidates(self.sema, state) {
            if seen.contains(&decl.name) {
                continue;
            }
            seen.push(decl.name);
            let script = self.sema.ws.file(decl.file).index.name;
            let item = self.push(
                decl.name.as_str(),
                CompletionKind::State,
                if decl.in_child { 2 } else { 1 },
            );
            item.replace_range = state.name_range;
            item.detail = script.map(|s| format!("State {} ({s})", decl.name));
        }
    }

    /// What a string of the custom events can name: for a callback, the events
    /// written for the object, not those of the game; else the events the other
    /// scripts name.
    fn add_mod_event(&mut self, event: &crate::mod_events::ModEventRef<'a>) {
        use crate::mod_events::{ModEventUse, callables, is_event, sites};
        let ws = self.sema.ws;
        if event.usage == ModEventUse::Callback {
            let mut seen: Vec<Name> = Vec::new();
            for decl in callables(self.sema, event) {
                let mine = decl.distance == 0 || ws.file(decl.file).origin.is_own();
                if !mine || !is_event(self.sema, &decl) || seen.contains(&decl.name) {
                    continue;
                }
                seen.push(decl.name);
                let signature = self.sema.member(decl.file, decl.index).signature();
                let item = self.push(decl.name.as_str(), CompletionKind::Event, decl.distance + 1);
                item.replace_range = event.name_range;
                item.detail = Some(signature);
            }
            return;
        }
        let mut seen: Vec<String> = Vec::new();
        for site in sites(ws, self.sema.file) {
            let here = site.file == self.sema.file && site.range == event.name_range;
            let key = site.name.to_ascii_lowercase();
            if here || seen.contains(&key) {
                continue;
            }
            seen.push(key);
            let item = self.push(site.name.clone(), CompletionKind::Event, 1);
            item.replace_range = event.name_range;
            item.detail = Some(format!("mod event ({})", ws.file(site.file).index.name_str()));
        }
    }

    fn anchor(&self) -> Option<papyrus_parser::SyntaxNode<'a>> {
        let tok = self.sema.root.token_at_offset(self.offset).left_biased()?;
        tok.parent()
    }

    fn in_body(&self) -> bool {
        self.anchor().is_some_and(in_body)
    }

    /// The declaration whose trailing flags are being typed: the cursor is on
    /// the header line, after the name, the parameters or the initializer.
    fn flag_context(&self) -> Option<papyrus_parser::SyntaxNode<'a>> {
        let prev = self.prev?;
        let parent = prev.parent()?;
        let decl = std::iter::once(parent).chain(parent.ancestors()).find(|n| {
            matches!(
                n.kind(),
                SyntaxKind::SCRIPT_HEADER
                    | SyntaxKind::FUNCTION
                    | SyntaxKind::EVENT
                    | SyntaxKind::PROPERTY
                    | SyntaxKind::VAR_DECL
                    | SyntaxKind::BLOCK
            )
        })?;
        if decl.kind() == SyntaxKind::BLOCK
            || decl.parent().is_some_and(|p| p.kind() == SyntaxKind::BLOCK)
        {
            return None;
        }
        let callable = matches!(decl.kind(), SyntaxKind::FUNCTION | SyntaxKind::EVENT);
        let ok = match parent.kind() {
            SyntaxKind::FLAG => true,
            SyntaxKind::PARAM_LIST => prev.kind() == SyntaxKind::R_PAREN,
            SyntaxKind::NAME | SyntaxKind::NAME_REF => {
                !callable
                    && parent
                        .parent()
                        .is_some_and(|p| p == decl || p.kind() == SyntaxKind::EXTENDS_CLAUSE)
            }
            // The end of an initializer such as `= 5` or `= None`.
            _ => {
                !callable
                    && decl.child_node(SyntaxKind::INITIALIZER).is_some_and(|init| {
                        init.last_token() == Some(prev) && prev.kind() != SyntaxKind::EQ
                    })
            }
        };
        ok.then_some(decl)
    }

    fn push(
        &mut self,
        label: impl Into<String>,
        kind: CompletionKind,
        sort: u8,
    ) -> &mut CompletionItem {
        let label = label.into();
        let sort_text = format!("{sort}{}", label.to_ascii_lowercase());
        self.out.items.push(CompletionItem {
            label,
            kind,
            detail: None,
            documentation: None,
            snippet: None,
            replace_range: self.replace_range,
            sort_text,
            resolve: None,
        });
        self.out.items.last_mut().unwrap()
    }

    fn push_member(&mut self, file: FileId, index: u32, m: &Member, sort: u8) {
        let kind = match m.kind {
            MemberKind::Function => CompletionKind::Function,
            MemberKind::Event => CompletionKind::Event,
            MemberKind::Property => CompletionKind::Property,
            MemberKind::Variable => CompletionKind::Variable,
            MemberKind::State => CompletionKind::State,
        };
        let snippet = m.is_callable().then(|| call_snippet(m.name.as_str(), !m.params.is_empty()));
        let item = self.push(m.name.as_str(), kind, sort);
        item.detail = Some(m.signature());
        item.snippet = snippet;
        item.resolve = (!m.docs.is_empty()).then_some((file, index));
    }

    fn complete_members(&mut self) {
        let Some(dot) = self.prev else { return };
        let Some(field) = dot.parent().and_then(ast::FieldExpr::cast) else { return };
        let Some(recv) = field.receiver().and_then(|r| self.sema.infer(r)) else { return };
        for item in self.sema.members_of(recv) {
            match item {
                MemberItem::Member(file, index, m) => self.push_member(file, index, m, 0),
                MemberItem::Builtin(b, elem) => {
                    let kind = if b.is_callable() {
                        CompletionKind::Function
                    } else {
                        CompletionKind::Property
                    };
                    let snippet =
                        b.is_callable().then(|| call_snippet(b.name(), !b.params(elem).is_empty()));
                    let item = self.push(b.name(), kind, 1);
                    item.detail = Some(b.signature(elem));
                    item.documentation = Some(b.docs().to_owned());
                    item.snippet = snippet;
                }
            }
        }
    }

    fn complete_body(&mut self) {
        let sema = self.sema;
        if let Some(anchor) = self.anchor() {
            let mut seen: Vec<Name> = Vec::new();
            for (name, _, ty, kind) in sema.locals_at(anchor, self.offset).into_iter().rev() {
                if seen.contains(&name) {
                    continue;
                }
                seen.push(name);
                let item = self.push(name.as_str(), CompletionKind::Local, 0);
                item.detail = Some(match (ty, kind) {
                    (Some(ty), LocalKind::Parameter) => format!("{ty} {name} (parameter)"),
                    (Some(ty), LocalKind::Variable) => format!("{ty} {name}"),
                    (None, _) => name.to_string(),
                });
            }
        }

        if let Some(script) = sema.script_name() {
            let ty =
                Ty::Value(crate::index::TypeRef::scalar(crate::index::BaseType::Object(script)));
            for item in sema.members_of(ty) {
                match item {
                    MemberItem::Member(file, index, m) => self.push_member(file, index, m, 1),
                    MemberItem::Builtin(b, _) => {
                        let item = self.push(b.name(), CompletionKind::Function, 3);
                        item.detail = Some(b.signature(None));
                        item.snippet = Some(call_snippet(b.name(), !b.params(None).is_empty()));
                    }
                }
            }
            // Global functions of the own script are not part of `members_of`.
            for (file, index) in sema.chain(script).take(1) {
                for (i, m) in index.empty_state_members() {
                    if m.is_callable() && m.is_global() {
                        self.push_member(file, i as u32, m, 1);
                    }
                }
            }
        }

        for &import in sema.index.imports.iter() {
            let Some(file) = sema.ws.script_near(import, sema.file) else { continue };
            let index = &sema.ws.file(file).index;
            for (i, m) in index.empty_state_members() {
                if m.is_callable() && m.is_global() {
                    self.push_member(file, i as u32, m, 2);
                }
            }
        }

        for kw in STATEMENT_KEYWORDS {
            self.push(*kw, CompletionKind::Keyword, 4);
        }
        self.add_snippets(super::snippets::SnippetContext::Body);
        self.add_primitives();
        self.add_scripts(true);
    }

    fn complete_item(&mut self) {
        for kw in DECL_KEYWORDS {
            self.push(*kw, CompletionKind::Keyword, 1);
        }
        self.add_snippets(super::snippets::SnippetContext::Item);
        self.add_primitives();
        self.add_scripts(true);
    }

    /// The snippets of `context`, where a statement or declaration starts:
    /// first on its line.
    fn add_snippets(&mut self, context: super::snippets::SnippetContext) {
        let text = self.sema.text;
        let start = usize::from(self.replace_range.start());
        let line_start = text[..start].rfind('\n').map_or(0, |i| i + 1);
        if !text[line_start..start].trim().is_empty() {
            return;
        }
        for s in super::snippets::snippets(context) {
            let item = self.push(s.prefix, CompletionKind::Snippet, 5);
            item.detail = Some(s.description.to_owned());
            item.documentation =
                Some(format!("```papyrus\n{}\n```", super::snippets::preview(&s.body)));
            item.snippet = Some(s.body);
        }
    }

    fn add_primitives(&mut self) {
        for ty in PRIMITIVES {
            self.push(*ty, CompletionKind::Type, 2);
        }
    }

    /// Language flags and user flags allowed on `decl`, minus those present.
    fn add_flags(&mut self, decl: papyrus_parser::SyntaxNode<'a>) {
        let auto_property = ast::Property::cast(decl).is_some_and(|p| p.is_auto());
        let Some(target) = FlagTargets::of_declaration(decl.kind(), auto_property) else { return };
        let present: Vec<&str> = decl
            .children()
            .filter_map(ast::Flag::cast)
            .filter(|f| f.syntax().text_range().end() < self.replace_range.start())
            .map(|f| f.text())
            .collect();
        let is_present = |name: &str| present.iter().any(|p| p.eq_ignore_ascii_case(name));

        let language: &[SyntaxKind] = match decl.kind() {
            SyntaxKind::SCRIPT_HEADER | SyntaxKind::EVENT => &[SyntaxKind::NATIVE_KW],
            SyntaxKind::FUNCTION => &[SyntaxKind::GLOBAL_KW, SyntaxKind::NATIVE_KW],
            SyntaxKind::PROPERTY if present.is_empty() || !auto_property => {
                &[SyntaxKind::AUTO_KW, SyntaxKind::AUTOREADONLY_KW]
            }
            _ => &[],
        };
        for &kind in language {
            let (name, docs) = super::language_flag_docs(kind);
            if is_present(name) {
                continue;
            }
            let item = self.push(name, CompletionKind::Flag, 0);
            item.detail = Some("language flag".to_owned());
            item.documentation = Some(docs.to_owned());
        }

        let table = self.sema.ws.flags_of(self.sema.file);
        let source = table.source_name();
        for def in table.defs.iter().filter(|d| d.targets.intersects(target)) {
            if is_present(def.name.as_str()) {
                continue;
            }
            let item = self.push(def.name.as_str(), CompletionKind::Flag, 1);
            item.detail = Some(format!("{} ({source})", def.signature()));
            item.documentation = (!def.docs.is_empty()).then(|| def.docs.clone());
        }
    }

    fn add_scripts(&mut self, low_priority: bool) {
        let first = self.prefix.chars().next().map(|c| c.to_ascii_lowercase());
        let mut matching: Vec<_> = self
            .sema
            .ws
            .all_scripts_from(self.sema.file)
            // Like names: only the scripts of the project, for its scripts.
            .filter(|&(name, _)| self.sema.ws.script_near(name, self.sema.file).is_some())
            .filter(|(name, _)| {
                first.is_none_or(|c| {
                    name.as_str().chars().next().map(|f| f.to_ascii_lowercase()) == Some(c)
                })
            })
            .collect();
        if matching.len() > MAX_SCRIPTS {
            // Prefer names that start with the whole prefix.
            let prefix = self.prefix.to_ascii_lowercase();
            matching.sort_by_key(|(name, _)| {
                (!name.as_str().to_ascii_lowercase().starts_with(&prefix), name.as_str().len())
            });
            matching.truncate(MAX_SCRIPTS);
            self.out.is_incomplete = true;
        }
        let sort = if low_priority { 5 } else { 0 };
        for (name, file) in matching {
            let detail = self.sema.ws.file(file).index.extends.map(|e| format!("extends {e}"));
            let item = self.push(name.as_str(), CompletionKind::Script, sort);
            item.detail = detail;
        }
    }

    /// Events and functions of the parent scripts, inserted as a full
    /// declaration with body.
    fn add_overrides(&mut self, kind: MemberKind) {
        let sema = self.sema;
        let Some(parent) = sema.index.extends else { return };
        let mut seen: Vec<Name> =
            sema.index.members.iter().filter(|m| m.kind == kind).map(|m| m.name).collect();
        let end = if kind == MemberKind::Event { "EndEvent" } else { "EndFunction" };
        for (file, index) in sema.chain(parent) {
            for (i, m) in index.empty_state_members() {
                if m.kind != kind || seen.contains(&m.name) || m.is_global() {
                    continue;
                }
                seen.push(m.name);
                let mut params = String::new();
                for (j, p) in m.params.iter().enumerate() {
                    if j > 0 {
                        params.push_str(", ");
                    }
                    render_param(&mut params, p);
                }
                // `Native` declarations of the parent are overridden by a
                // normal declaration with a body.
                let snippet = format!("{}({})\n\t$0\n{end}", m.name, escape_snippet(&params));
                let label = format!("{}({params})", m.name);
                let kind = if kind == MemberKind::Event {
                    CompletionKind::Event
                } else {
                    CompletionKind::Function
                };
                let item = self.push(label, kind, 0);
                item.detail =
                    Some(format!("{} of {}", member_kind_label(m.kind), index.name_str()));
                item.snippet = Some(snippet);
                item.resolve = (!m.docs.is_empty()).then_some((file, i as u32));
            }
        }
    }
}

fn call_snippet(name: &str, has_params: bool) -> String {
    if has_params { format!("{name}($1)$0") } else { format!("{name}()$0") }
}

fn escape_snippet(s: &str) -> String {
    s.replace('\\', "\\\\").replace('$', "\\$").replace('}', "\\}")
}

/// Completion in the flags file: `Flag` definitions at the top level and the
/// targets (`Script`, `Property`, `Variable`, `Function`) inside `{ }`.
pub fn flag_file_completions(flags: &FlagTable, offset: TextSize) -> Completions {
    let text: &str = &flags.text;
    let offset = usize::from(offset).min(text.len());
    let before = &text[..offset];

    // No completion inside comments.
    let line_start = before.rfind('\n').map_or(0, |i| i + 1);
    let in_block_comment =
        before.rfind("/*").is_some_and(|open| before[open..].find("*/").is_none());
    if before[line_start..].contains("//") || in_block_comment {
        return Completions::default();
    }

    let word_start = before
        .char_indices()
        .rev()
        .take_while(|(_, c)| c.is_ascii_alphanumeric() || *c == '_')
        .last()
        .map_or(offset, |(i, _)| i);
    let replace_range =
        TextRange::new(TextSize::new(word_start as u32), TextSize::new(offset as u32));
    let item =
        |label: &str, kind, detail: &str, snippet: Option<String>, sort: &str| CompletionItem {
            label: label.to_owned(),
            kind,
            detail: Some(detail.to_owned()),
            documentation: None,
            snippet,
            replace_range,
            sort_text: format!("{sort}{label}"),
            resolve: None,
        };

    let open = before.rfind('{');
    let close = before.rfind('}');
    let in_braces = match (open, close) {
        (Some(o), Some(c)) => o > c,
        (Some(_), None) => true,
        _ => false,
    };
    let mut out = Completions::default();
    if in_braces {
        // The whole `{ ... }` block, also after the cursor.
        let start = open.unwrap_or(0);
        let end = text[offset..].find('}').map_or(text.len(), |i| offset + i);
        let listed = format!("{}{}", &text[start..word_start], &text[offset..end]);
        for (target, detail) in [
            ("Script", "the script header"),
            ("Property", "properties"),
            ("Variable", "script variables (and auto properties)"),
            ("Function", "functions and events"),
        ] {
            let present = listed
                .split(|c: char| !c.is_ascii_alphanumeric())
                .any(|w| w.eq_ignore_ascii_case(target) && !w.is_empty());
            if !present {
                out.items.push(item(
                    target,
                    CompletionKind::Keyword,
                    &format!("flag allowed on {detail}"),
                    None,
                    "0",
                ));
            }
        }
        return out;
    }

    // The first free bit index for a new flag.
    let next_index = (0..32u32).find(|i| flags.defs.iter().all(|d| d.index != *i)).unwrap_or(0);
    out.items.push(item(
        "Flag",
        CompletionKind::Snippet,
        "Flag <name> <index> { <targets> }",
        Some(format!("Flag ${{1:Name}} ${{2:{next_index}}}\n{{\n\t${{3:Script}}\n}}$0")),
        "0",
    ));
    out.items.push(item("Flag", CompletionKind::Keyword, "keyword", None, "1"));
    out
}
