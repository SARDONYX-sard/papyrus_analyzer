//! Request handlers: protocol in, [`papyrus_ide`] query, protocol out.

use lsp_types::{
    CodeLens, CodeLensParams, Command, CompletionItem, CompletionItemKind, CompletionList,
    CompletionParams, CompletionResponse, CompletionTextEdit, DocumentFormattingParams,
    DocumentHighlight, DocumentHighlightKind, DocumentHighlightParams, DocumentSymbolParams,
    DocumentSymbolResponse, Documentation, FoldingRange, FoldingRangeKind, FoldingRangeParams,
    GotoDefinitionParams, GotoDefinitionResponse, Hover, HoverContents, HoverParams, InlayHint,
    InlayHintKind, InlayHintLabel, InlayHintParams, InlayHintTooltip, InsertTextFormat, Location,
    LocationLink, MarkupContent, MarkupKind, ParameterInformation, ParameterLabel, Position,
    ReferenceParams, SemanticToken, SemanticTokens, SemanticTokensParams,
    SemanticTokensRangeParams, SemanticTokensRangeResult, SemanticTokensResult, SignatureHelp,
    SignatureHelpParams, SignatureInformation, SymbolInformation, TextDocumentPositionParams,
    TextEdit, WorkspaceSymbolParams, WorkspaceSymbolResponse,
};
use papyrus_ide::{FileId, FoldKind, NavFile, TextRange, TextSize};
use rustc_hash::FxHashMap;

use crate::{conv::Lines, ext, server::Server};

/// Maximum number of workspace symbols returned.
const MAX_WORKSPACE_SYMBOLS: usize = 256;

impl Server {
    fn position(&self, pos: &TextDocumentPositionParams) -> Option<(FileId, Lines, TextSize)> {
        let file = self.file(&pos.text_document.uri).ok()?;
        let lines = Lines::of(&self.ws, file)?;
        let offset = lines.offset(pos.position);
        Some((file, lines, offset))
    }

    fn location(
        &self,
        file: FileId,
        range: TextRange,
        cache: &mut FxHashMap<FileId, Lines>,
    ) -> Option<Location> {
        let lines = match cache.entry(file) {
            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::hash_map::Entry::Vacant(e) => e.insert(Lines::of(&self.ws, file)?),
        };
        Some(Location {
            uri: crate::conv::path_to_uri(&self.ws.file(file).path),
            range: lines.range(range),
        })
    }

    fn nav_location(
        &self,
        file: NavFile,
        range: TextRange,
        cache: &mut FxHashMap<FileId, Lines>,
    ) -> Option<Location> {
        match file {
            NavFile::Script(file) => self.location(file, range, cache),
            NavFile::Flags(table) => {
                let flags = self.ws.flag_table(table);
                Some(Location {
                    uri: crate::conv::path_to_uri(flags.path.as_ref()?),
                    range: Lines::flags(flags).range(range),
                })
            }
        }
    }

    /// The offset of a position in a flags file in use, if `uri` is one, with the
    /// index of its table.
    fn flags_position(&self, pos: &TextDocumentPositionParams) -> Option<(u16, Lines, TextSize)> {
        let path = crate::conv::uri_to_path(&pos.text_document.uri)?;
        let table = self.ws.flag_table_at(&path)?;
        let lines = Lines::flags(self.ws.flag_table(table));
        let offset = lines.offset(pos.position);
        Some((table, lines, offset))
    }

    pub(crate) fn hover(&mut self, params: HoverParams) -> Option<Hover> {
        let pos = &params.text_document_position_params;
        if let Some(path) = crate::conv::uri_to_path(&pos.text_document.uri)
            && crate::server::is_manifest(&path)
        {
            let text: std::sync::Arc<str> = match self.other_docs.get(&path) {
                Some(doc) => doc.text.as_str().into(),
                None => std::fs::read_to_string(&path).ok()?.into(),
            };
            let lines =
                Lines { index: std::sync::Arc::new(papyrus_ide::LineIndex::new(&text)), text };
            let (range, value) = self.manifest_hover(&path, lines.offset(pos.position))?;
            return Some(Hover {
                contents: HoverContents::Markup(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value,
                }),
                range: Some(lines.range(range)),
            });
        }
        if let Some((table, lines, offset)) =
            self.flags_position(&params.text_document_position_params)
        {
            let hover = papyrus_ide::flag_file_hover(self.ws.flag_table(table), offset)?;
            return Some(Hover {
                contents: HoverContents::Markup(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value: hover.markdown,
                }),
                range: Some(lines.range(hover.range)),
            });
        }
        let (file, lines, offset) = self.position(&params.text_document_position_params)?;
        // Inside a `; #[allow(...)]` comment: the attribute and its lints.
        if let Some((range, value)) = papyrus_lint::ignore::attribute_hover(&self.ws, file, offset)
        {
            return Some(Hover {
                contents: HoverContents::Markup(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value,
                }),
                range: Some(lines.range(range)),
            });
        }
        let hover = papyrus_ide::hover(&self.ws, file, offset)?;
        Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: hover.markdown,
            }),
            range: Some(lines.range(hover.range)),
        })
    }

    pub(crate) fn goto_definition(
        &mut self,
        params: GotoDefinitionParams,
    ) -> Option<GotoDefinitionResponse> {
        let (file, lines, offset) = self.position(&params.text_document_position_params)?;
        // `; #[mock(Script.Function)]`: the script or the native.
        let targets = match papyrus_lint::ignore::attribute_definition(&self.ws, file, offset) {
            Some(target) => vec![target],
            None => papyrus_ide::goto_definition(&self.ws, file, offset),
        };
        self.nav_response(file, lines, targets)
    }

    /// The overridden function of the parent script (or the empty state).
    pub(crate) fn goto_declaration(
        &mut self,
        params: GotoDefinitionParams,
    ) -> Option<GotoDefinitionResponse> {
        let (file, lines, offset) = self.position(&params.text_document_position_params)?;
        let targets = papyrus_ide::goto_declaration(&self.ws, file, offset);
        self.nav_response(file, lines, targets)
    }

    fn nav_response(
        &mut self,
        file: papyrus_ide::FileId,
        lines: Lines,
        targets: Vec<papyrus_ide::NavTarget>,
    ) -> Option<GotoDefinitionResponse> {
        if targets.is_empty() {
            return None;
        }
        let link_support = self
            .caps
            .text_document
            .as_ref()
            .and_then(|t| t.definition.as_ref())
            .and_then(|d| d.link_support)
            .unwrap_or(false);
        let mut cache = FxHashMap::default();
        cache.insert(file, lines);
        if link_support {
            let mut links = Vec::new();
            for t in targets {
                let full = self.nav_location(t.file, t.full_range, &mut cache)?;
                let focus = self.nav_location(t.file, t.focus_range, &mut cache)?;
                links.push(LocationLink {
                    origin_selection_range: None,
                    target_uri: full.uri,
                    target_range: full.range,
                    target_selection_range: focus.range,
                });
            }
            Some(GotoDefinitionResponse::Link(links))
        } else {
            let locations = targets
                .into_iter()
                .filter_map(|t| self.nav_location(t.file, t.focus_range, &mut cache))
                .collect();
            Some(GotoDefinitionResponse::Array(locations))
        }
    }

    pub(crate) fn references(&mut self, params: ReferenceParams) -> Option<Vec<Location>> {
        let include = params.context.include_declaration;
        let mut cache = FxHashMap::default();
        let refs =
            if let Some((table, _, offset)) = self.flags_position(&params.text_document_position) {
                papyrus_ide::flag_references(&self.ws, table, offset, include)
            } else {
                let (file, lines, offset) = self.position(&params.text_document_position)?;
                cache.insert(file, lines);
                papyrus_ide::references(&self.ws, file, offset, include)
            };
        Some(refs.into_iter().filter_map(|(f, r)| self.nav_location(f, r, &mut cache)).collect())
    }

    pub(crate) fn document_highlight(
        &mut self,
        params: DocumentHighlightParams,
    ) -> Option<Vec<DocumentHighlight>> {
        let (file, lines, offset) = self.position(&params.text_document_position_params)?;
        let highlights = papyrus_ide::highlight(&self.ws, file, offset);
        Some(
            highlights
                .into_iter()
                .map(|h| DocumentHighlight {
                    range: lines.range(h.range),
                    kind: Some(if h.write {
                        DocumentHighlightKind::WRITE
                    } else {
                        DocumentHighlightKind::READ
                    }),
                })
                .collect(),
        )
    }

    pub(crate) fn completion(&mut self, params: CompletionParams) -> Option<CompletionResponse> {
        let (lines, completions) = if let Some((table, lines, offset)) =
            self.flags_position(&params.text_document_position)
        {
            (lines, papyrus_ide::flag_file_completions(self.ws.flag_table(table), offset))
        } else {
            let (file, lines, offset) = self.position(&params.text_document_position)?;
            // Inside a `; #[allow(...)]` comment: the lints.
            if let Some((range, items)) =
                papyrus_lint::ignore::attribute_completions(&self.ws, file, offset)
            {
                let items = items
                    .into_iter()
                    .map(|(label, detail)| CompletionItem {
                        kind: Some(lsp_types::CompletionItemKind::KEYWORD),
                        detail: Some(detail),
                        text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                            range: lines.range(range),
                            new_text: label.clone(),
                        })),
                        label,
                        ..Default::default()
                    })
                    .collect();
                return Some(CompletionResponse::List(CompletionList {
                    is_incomplete: false,
                    items,
                }));
            }
            // These only trigger completion for attributes, not in code.
            let trigger = params.context.as_ref().and_then(|c| c.trigger_character.as_deref());
            if matches!(trigger, Some("[" | "(" | ",")) {
                return None;
            }
            let mut completions = papyrus_ide::completions(&self.ws, file, offset);
            // A quote only opens the states of `GoToState("`: it also ends
            // strings, where the code would be proposed.
            if trigger == Some("\"") {
                completions.items.retain(|i| i.kind == papyrus_ide::CompletionKind::State);
            }
            (lines, completions)
        };
        let snippets = self
            .caps
            .text_document
            .as_ref()
            .and_then(|t| t.completion.as_ref())
            .and_then(|c| c.completion_item.as_ref())
            .and_then(|i| i.snippet_support)
            .unwrap_or(false);

        let items = completions
            .items
            .into_iter()
            // Snippets need snippet support; their label alone means nothing.
            .filter(|item| snippets || item.kind != papyrus_ide::CompletionKind::Snippet)
            .map(|item| {
                let (new_text, format) = match (&item.snippet, snippets) {
                    (Some(snippet), true) => (snippet.clone(), InsertTextFormat::SNIPPET),
                    _ => (plain_insert_text(&item), InsertTextFormat::PLAIN_TEXT),
                };
                CompletionItem {
                    label: item.label.clone(),
                    kind: Some(completion_kind(item.kind)),
                    detail: item.detail,
                    documentation: item.documentation.map(markdown),
                    sort_text: Some(item.sort_text),
                    filter_text: Some(
                        item.label.split('(').next().unwrap_or(&item.label).to_owned(),
                    ),
                    insert_text_format: Some(format),
                    text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                        range: lines.range(item.replace_range),
                        new_text,
                    })),
                    data: item
                        .resolve
                        .map(|(f, i)| serde_json::json!({ "file": f.0, "member": i })),
                    ..Default::default()
                }
            })
            .collect();
        Some(CompletionResponse::List(CompletionList {
            is_incomplete: completions.is_incomplete,
            items,
        }))
    }

    pub(crate) fn resolve_completion(&mut self, mut item: CompletionItem) -> CompletionItem {
        let target = item.data.as_ref().and_then(|d| {
            let file = d.get("file")?.as_u64()? as u32;
            let member = d.get("member")?.as_u64()? as u32;
            Some((FileId(file), member))
        });
        if let Some((file, member)) = target {
            let known = self.ws.files().any(|(id, _)| id == file);
            if known && item.documentation.is_none() {
                item.documentation = papyrus_ide::member_docs(&self.ws, file, member).map(markdown);
            }
        }
        item
    }

    pub(crate) fn signature_help(&mut self, params: SignatureHelpParams) -> Option<SignatureHelp> {
        let (file, _, offset) = self.position(&params.text_document_position_params)?;
        let help = papyrus_ide::signature_help(&self.ws, file, offset)?;
        let label_utf16 = |byte: u32| help.label[..byte as usize].encode_utf16().count() as u32;
        let parameters = help
            .params
            .iter()
            .map(|&(start, end)| ParameterInformation {
                label: ParameterLabel::LabelOffsets([label_utf16(start), label_utf16(end)]),
                documentation: None,
            })
            .collect();
        Some(SignatureHelp {
            signatures: vec![SignatureInformation {
                label: help.label.clone(),
                documentation: help.documentation.map(markdown),
                parameters: Some(parameters),
                active_parameter: help.active_param,
            }],
            active_signature: Some(0),
            active_parameter: help.active_param,
        })
    }

    pub(crate) fn document_symbols(
        &mut self,
        params: DocumentSymbolParams,
    ) -> Option<DocumentSymbolResponse> {
        if let Some(table) = crate::conv::uri_to_path(&params.text_document.uri)
            .and_then(|p| self.ws.flag_table_at(&p))
        {
            let flags = self.ws.flag_table(table);
            let lines = Lines::flags(flags);
            let symbols = papyrus_ide::flag_file_symbols(flags);
            return Some(DocumentSymbolResponse::Nested(
                symbols.into_iter().map(|s| document_symbol(&lines, s)).collect(),
            ));
        }
        let file = self.file(&params.text_document.uri).ok()?;
        let lines = Lines::of(&self.ws, file)?;
        let symbols = papyrus_ide::document_symbols(&self.ws, file);
        Some(DocumentSymbolResponse::Nested(
            symbols.into_iter().map(|s| document_symbol(&lines, s)).collect(),
        ))
    }

    pub(crate) fn workspace_symbols(
        &mut self,
        params: WorkspaceSymbolParams,
    ) -> Option<WorkspaceSymbolResponse> {
        let symbols =
            papyrus_ide::workspace_symbols(&self.ws, &params.query, MAX_WORKSPACE_SYMBOLS);
        let mut cache = FxHashMap::default();
        let infos = symbols
            .into_iter()
            .filter_map(|s| {
                let location = self.location(s.file, s.range, &mut cache)?;
                #[allow(deprecated)]
                Some(SymbolInformation {
                    name: s.name,
                    kind: symbol_kind(s.kind),
                    tags: None,
                    deprecated: None,
                    location,
                    container_name: s.container,
                })
            })
            .collect();
        Some(WorkspaceSymbolResponse::Flat(infos))
    }

    /// `▶ Compile` and `Compile All` above the `ScriptName` line, like the
    /// `▶ Run Test` lenses of rust-analyzer. None without `papyrus.toml`.
    pub(crate) fn code_lens(&mut self, params: CodeLensParams) -> Option<Vec<CodeLens>> {
        let file = self.file(&params.text_document.uri).ok()?;
        let lines = Lines::of(&self.ws, file)?;
        let range = self.ws.file(file).index.name_range?;
        let line = lines.position(range.start()).line;
        let range = lsp_types::Range::new(Position::new(line, 0), Position::new(line, 0));
        let uri = serde_json::to_value(params.text_document.uri.as_str()).ok()?;
        let lens = |title: &str, command: &str, arguments| CodeLens {
            range,
            command: Some(Command {
                title: title.to_owned(),
                command: command.to_owned(),
                arguments,
            }),
            data: None,
        };
        let mut lenses =
            vec![lens("▶\u{fe0e} Compile", "papyrus.compile", Some(vec![uri.clone()]))];
        // `overridden in N scripts` above functions child scripts replace;
        // the client turns the arguments into a references view.
        let mut cache = FxHashMap::default();
        for o in papyrus_ide::overridden(&self.ws, file) {
            let locations: Vec<Location> =
                o.by.iter()
                    .filter_map(|t| self.nav_location(t.file, t.focus_range, &mut cache))
                    .collect();
            let position = lines.position(o.name_range.start());
            let line = lsp_types::Range::new(
                Position::new(position.line, 0),
                Position::new(position.line, 0),
            );
            let title = match locations.len() {
                1 => "overridden in 1 script".to_owned(),
                n => format!("overridden in {n} scripts"),
            };
            let arguments = vec![
                uri.clone(),
                serde_json::to_value(position).ok()?,
                serde_json::to_value(&locations).ok()?,
            ];
            lenses.push(CodeLens {
                range: line,
                command: Some(Command {
                    title,
                    command: "papyrus.showReferences".to_owned(),
                    arguments: Some(arguments),
                }),
                data: None,
            });
        }
        // `▶ Run Test | ⚙ Debug` above `; #[test]` functions: the client runs
        // `papyrus_analyzer run Script.Function`, or debugs it with
        // `papyrus_analyzer debug`.
        if let (Some(text), Some(script)) = (self.ws.text(file), self.ws.file(file).index.name) {
            use papyrus_parser::ast::{self, AstNode};
            let parse = papyrus_parser::parse(&text);
            for attr in papyrus_parser::attrs::attributes(parse.syntax_node()) {
                if attr.inner || !attr.path.eq_ignore_ascii_case("test") {
                    continue;
                }
                let node = parse.syntax_node().covering_node(attr.target);
                let Some(name) = std::iter::once(node)
                    .chain(node.ancestors())
                    .find_map(ast::Function::cast)
                    .and_then(|f| f.name())
                else {
                    continue;
                };
                let line = lines.position(attr.target.start()).line;
                let target = serde_json::Value::String(format!("{}.{}", script, name.text()));
                // Like rust-analyzer: `▶︎ Run Test | ⚙︎ Debug`.
                for (title, command) in [
                    ("▶\u{fe0e} Run Test", "papyrus.runTest"),
                    ("⚙\u{fe0e} Debug", "papyrus.debugTest"),
                ] {
                    lenses.push(CodeLens {
                        range: lsp_types::Range::new(
                            Position::new(line, 0),
                            Position::new(line, 0),
                        ),
                        command: Some(Command {
                            title: title.to_owned(),
                            command: command.to_owned(),
                            arguments: Some(vec![uri.clone(), target.clone()]),
                        }),
                        data: None,
                    });
                }
            }
        }
        Some(lenses)
    }

    pub(crate) fn folding_ranges(
        &mut self,
        params: FoldingRangeParams,
    ) -> Option<Vec<FoldingRange>> {
        let file = self.file(&params.text_document.uri).ok()?;
        let lines = Lines::of(&self.ws, file)?;
        let folds = papyrus_ide::folding_ranges(&self.ws, file);
        Some(
            folds
                .into_iter()
                .filter_map(|f| {
                    let start = lines.position(f.range.start()).line;
                    let end = lines.position(f.range.end()).line;
                    // For regions the closing keyword's line stays visible.
                    let end = if f.kind == FoldKind::Region { end.checked_sub(1)? } else { end };
                    (end > start).then(|| FoldingRange {
                        start_line: start,
                        end_line: end,
                        kind: Some(match f.kind {
                            FoldKind::Region => FoldingRangeKind::Region,
                            FoldKind::Comment => FoldingRangeKind::Comment,
                        }),
                        ..Default::default()
                    })
                })
                .collect(),
        )
    }

    pub(crate) fn semantic_tokens_full(
        &mut self,
        params: SemanticTokensParams,
    ) -> Option<SemanticTokensResult> {
        let file = self.file(&params.text_document.uri).ok()?;
        let data = self.encode_semantic_tokens(file, None)?;
        Some(SemanticTokensResult::Tokens(SemanticTokens { result_id: None, data }))
    }

    pub(crate) fn semantic_tokens_range(
        &mut self,
        params: SemanticTokensRangeParams,
    ) -> Option<SemanticTokensRangeResult> {
        let file = self.file(&params.text_document.uri).ok()?;
        let lines = Lines::of(&self.ws, file)?;
        let range = lines.text_range(params.range);
        let data = self.encode_semantic_tokens(file, Some(range))?;
        Some(SemanticTokensRangeResult::Tokens(SemanticTokens { result_id: None, data }))
    }

    fn encode_semantic_tokens(
        &self,
        file: FileId,
        range: Option<TextRange>,
    ) -> Option<Vec<SemanticToken>> {
        let lines = Lines::of(&self.ws, file)?;
        let tokens = papyrus_ide::semantic_tokens(&self.ws, file, range);
        let mut data = Vec::with_capacity(tokens.len());
        let (mut prev_line, mut prev_col) = (0u32, 0u32);
        for tok in tokens {
            let text = &lines.text[tok.range];
            // Tokens may not span lines: split multi-line comments.
            let mut offset = tok.range.start();
            for part in text.split_inclusive('\n') {
                let content = part.trim_end_matches(['\n', '\r']);
                let len = content.encode_utf16().count() as u32;
                if len > 0 {
                    let pos = lines.position(offset);
                    let delta_line = pos.line - prev_line;
                    let delta_start =
                        if delta_line == 0 { pos.character - prev_col } else { pos.character };
                    data.push(SemanticToken {
                        delta_line,
                        delta_start,
                        length: len,
                        token_type: tok.ty as u32,
                        token_modifiers_bitset: tok.modifiers,
                    });
                    prev_line = pos.line;
                    prev_col = pos.character;
                }
                offset += TextSize::of(part);
            }
        }
        Some(data)
    }

    /// Formats a whole script. Editor options come first, then
    /// `.editorconfig`, then `[format]` of `papyrus.toml`.
    pub(crate) fn formatting(&mut self, params: DocumentFormattingParams) -> Option<Vec<TextEdit>> {
        let file = self.file(&params.text_document.uri).ok()?;
        let data = self.ws.file(file);
        if !self.is_editable(&data.path) {
            log::info!("{}: not formatted, not a script of the workspace", data.path.display());
            return None;
        }
        if papyrus_project::format_excludes(&data.path) {
            log::info!("{}: not formatted, in `exclude` of [format]", data.path.display());
            return None;
        }
        let text = data.open.as_ref()?.text.clone();
        let editor = &params.options;
        let options = papyrus_project::format_options_for(&data.path, |o| {
            o.indent = if editor.insert_spaces {
                papyrus_fmt::Indent::Spaces(editor.tab_size.clamp(1, 16) as u8)
            } else {
                papyrus_fmt::Indent::Tabs
            };
            if let Some(final_newline) = editor.insert_final_newline {
                o.final_newline = final_newline;
            }
        });
        let options = papyrus_fmt::FormatOptions {
            names: crate::fmt::names(&self.ws, file).into(),
            ..options
        };
        match papyrus_fmt::format(&text, &options) {
            Ok(formatted) if formatted == *text => Some(Vec::new()),
            Ok(formatted) => {
                let lines = Lines::of(&self.ws, file)?;
                let range = lines.range(TextRange::up_to(TextSize::of(&*text)));
                Some(vec![TextEdit { range, new_text: formatted }])
            }
            Err(err) => {
                log::info!("{}: {err}", data.path.display());
                None
            }
        }
    }

    pub(crate) fn inlay_hints(&mut self, params: InlayHintParams) -> Option<Vec<InlayHint>> {
        let file = self.file(&params.text_document.uri).ok()?;
        let lines = Lines::of(&self.ws, file)?;
        let range = lines.text_range(params.range);
        let mut out = Vec::new();
        if self.config.inlay_hints.parameter_names {
            out.extend(papyrus_ide::inlay_hints(&self.ws, file, Some(range)).into_iter().map(
                |h| InlayHint {
                    position: lines.position(h.offset),
                    label: InlayHintLabel::String(h.label),
                    kind: Some(InlayHintKind::PARAMETER),
                    text_edits: None,
                    tooltip: Some(InlayHintTooltip::MarkupContent(MarkupContent {
                        kind: MarkupKind::Markdown,
                        value: format!("```papyrus\n{}\n```", h.tooltip),
                    })),
                    padding_left: None,
                    padding_right: Some(true),
                    data: None,
                },
            ));
        }
        if self.config.inlay_hints.overrides {
            let mut cache = FxHashMap::default();
            for o in papyrus_ide::overrides(&self.ws, file) {
                if range.intersect(o.name_range).is_none() {
                    continue;
                }
                // The script name links to the replaced function.
                let location = self.nav_location(o.base.file, o.base.focus_range, &mut cache);
                let (prefix, tooltip) = match o.kind {
                    papyrus_ide::OverrideKind::Override => {
                        ("overrides ", format!("Replaces the function of `{}`", o.base_script))
                    }
                    papyrus_ide::OverrideKind::EngineEvent => (
                        "event of ",
                        format!("An event the game sends, declared by `{}`", o.base_script),
                    ),
                };
                out.push(InlayHint {
                    position: lines.position(o.hint_offset),
                    label: InlayHintLabel::LabelParts(vec![
                        lsp_types::InlayHintLabelPart {
                            value: prefix.to_owned(),
                            ..Default::default()
                        },
                        lsp_types::InlayHintLabelPart {
                            value: o.base_script.to_string(),
                            location,
                            tooltip: Some(lsp_types::InlayHintLabelPartTooltip::String(tooltip)),
                            command: None,
                        },
                    ]),
                    kind: None,
                    text_edits: None,
                    tooltip: None,
                    padding_left: Some(true),
                    padding_right: None,
                    data: None,
                });
            }
        }
        Some(out)
    }
    /// Assists and lint fixes at the selection, as code actions. Quick fixes
    /// carry the diagnostics they fix.
    pub(crate) fn code_actions(
        &mut self,
        params: lsp_types::CodeActionParams,
    ) -> Option<lsp_types::CodeActionResponse> {
        use lsp_types::{CodeActionKind, CodeActionOrCommand, NumberOrString};

        let file = self.file(&params.text_document.uri).ok()?;
        // Code actions edit the file: none for imported scripts.
        if !self.is_editable(&self.ws.file(file).path) {
            return None;
        }
        let lines = Lines::of(&self.ws, file)?;
        let range = lines.text_range(params.range);
        let mut assists = papyrus_ide::assists::assists(&self.ws, file, range);
        let lints = self.lint_config(&self.ws.file(file).path);
        assists.extend(papyrus_lint::fixes(&self.ws, file, range, &lints));
        assists.extend(papyrus_lint::ignore_fixes(&self.ws, file, range, &lints));
        let only = params.context.only.unwrap_or_default();
        let uri = params.text_document.uri;
        let actions = assists
            .into_iter()
            .filter_map(|a| {
                let kind = match a.kind {
                    papyrus_ide::assists::AssistKind::QuickFix => CodeActionKind::QUICKFIX,
                    papyrus_ide::assists::AssistKind::RefactorRewrite => {
                        CodeActionKind::REFACTOR_REWRITE
                    }
                };
                if !only.is_empty() && !only.iter().any(|o| kind.as_str().starts_with(o.as_str())) {
                    return None;
                }
                let diagnostics: Vec<lsp_types::Diagnostic> = params
                    .context
                    .diagnostics
                    .iter()
                    .filter(|d| {
                        a.fixes.is_some_and(|code| {
                            d.code == Some(NumberOrString::String(code.to_owned()))
                        })
                    })
                    .cloned()
                    .collect();
                let edits = a
                    .edits
                    .into_iter()
                    .map(|e| TextEdit { range: lines.range(e.range), new_text: e.new_text })
                    .collect();
                #[allow(clippy::mutable_key_type)]
                let changes = std::iter::once((uri.clone(), edits)).collect();
                // Allowing a problem is an option, not the fix to apply first.
                let preferred = !diagnostics.is_empty() && !a.id.starts_with("allow_lint");
                Some(CodeActionOrCommand::CodeAction(lsp_types::CodeAction {
                    title: a.label,
                    kind: Some(kind),
                    is_preferred: Some(preferred),
                    diagnostics: (!diagnostics.is_empty()).then_some(diagnostics),
                    edit: Some(lsp_types::WorkspaceEdit {
                        changes: Some(changes),
                        ..Default::default()
                    }),
                    ..Default::default()
                }))
            })
            .collect();
        Some(actions)
    }
    /// `textDocument/prepareRename`: the range of the name, or why it cannot
    /// be renamed.
    pub(crate) fn prepare_rename(
        &mut self,
        params: TextDocumentPositionParams,
    ) -> Result<Option<lsp_types::PrepareRenameResponse>, String> {
        let Some((file, lines, offset)) = self.position(&params) else { return Ok(None) };
        let (range, placeholder) = papyrus_ide::prepare_rename(&self.ws, file, offset)?;
        // Every file the rename would change must be ours.
        let edit = papyrus_ide::rename(&self.ws, file, offset, &placeholder)?;
        self.check_editable(&edit)?;
        Ok(Some(lsp_types::PrepareRenameResponse::RangeWithPlaceholder {
            range: lines.range(range),
            placeholder,
        }))
    }

    /// Refuses a rename changing a file that is not ours (an imported script,
    /// the flags file of the game...).
    fn check_editable(&self, edit: &papyrus_ide::RenameEdit) -> Result<(), String> {
        let paths = edit
            .ranges
            .iter()
            .filter_map(|(nav, _)| match nav {
                papyrus_ide::NavFile::Script(f) => Some(self.ws.file(*f).path.clone()),
                papyrus_ide::NavFile::Flags(table) => self.ws.flag_table(*table).path.clone(),
            })
            .chain(edit.file_renames.iter().map(|(f, _)| self.ws.file(*f).path.clone()));
        for path in paths {
            if !self.is_editable(&path) {
                return Err(format!(
                    "cannot rename: it would change {}, which is not a script of the workspace",
                    path.display()
                ));
            }
        }
        Ok(())
    }

    /// `textDocument/rename`.
    pub(crate) fn rename(
        &mut self,
        params: lsp_types::RenameParams,
    ) -> Result<Option<lsp_types::WorkspaceEdit>, String> {
        use lsp_types::{
            DocumentChangeOperation, DocumentChanges, OneOf,
            OptionalVersionedTextDocumentIdentifier, RenameFile, ResourceOp, ResourceOperationKind,
            TextDocumentEdit,
        };

        let Some((file, _, offset)) = self.position(&params.text_document_position) else {
            return Ok(None);
        };
        let edit = papyrus_ide::rename(&self.ws, file, offset, &params.new_name)?;
        self.check_editable(&edit)?;
        let mut cache = FxHashMap::default();
        // Edits grouped by document, in order.
        let mut by_uri: Vec<(lsp_types::Uri, Vec<TextEdit>)> = Vec::new();
        for (nav, range) in edit.ranges {
            let Some(location) = self.nav_location(nav, range, &mut cache) else { continue };
            let edit = TextEdit { range: location.range, new_text: params.new_name.clone() };
            match by_uri.iter_mut().find(|(u, _)| *u == location.uri) {
                Some((_, edits)) => edits.push(edit),
                None => by_uri.push((location.uri, vec![edit])),
            }
        }
        let can_rename_files = self
            .caps
            .workspace
            .as_ref()
            .and_then(|w| w.workspace_edit.as_ref())
            .and_then(|e| e.resource_operations.as_ref())
            .is_some_and(|ops| ops.contains(&ResourceOperationKind::Rename));
        if edit.file_renames.is_empty() || !can_rename_files {
            #[allow(clippy::mutable_key_type)]
            let changes = by_uri.into_iter().collect();
            return Ok(Some(lsp_types::WorkspaceEdit {
                changes: Some(changes),
                ..Default::default()
            }));
        }
        // Text edits first, then the file renames.
        let mut ops: Vec<DocumentChangeOperation> = by_uri
            .into_iter()
            .map(|(uri, edits)| {
                DocumentChangeOperation::Edit(TextDocumentEdit {
                    text_document: OptionalVersionedTextDocumentIdentifier { uri, version: None },
                    edits: edits.into_iter().map(OneOf::Left).collect(),
                })
            })
            .collect();
        for (f, new_path) in edit.file_renames {
            ops.push(DocumentChangeOperation::Op(ResourceOp::Rename(RenameFile {
                old_uri: crate::conv::path_to_uri(&self.ws.file(f).path),
                new_uri: crate::conv::path_to_uri(&new_path),
                options: None,
                annotation_id: None,
            })));
        }
        Ok(Some(lsp_types::WorkspaceEdit {
            document_changes: Some(DocumentChanges::Operations(ops)),
            ..Default::default()
        }))
    }
    pub(crate) fn syntax_tree(&mut self, params: ext::SyntaxTreeParams) -> String {
        let Ok(file) = self.file(&params.text_document.uri) else { return String::new() };
        let Some(text) = self.ws.text(file) else { return String::new() };
        papyrus_parser::parse(&text).debug_dump()
    }
}

/// Without snippet support, insert the bare name (and `()` for calls).
fn plain_insert_text(item: &papyrus_ide::CompletionItem) -> String {
    match &item.snippet {
        Some(snippet) if snippet.contains("$0\n") => item.label.clone(),
        Some(_) => format!("{}()", item.label),
        None => item.label.clone(),
    }
}

fn markdown(value: String) -> Documentation {
    Documentation::MarkupContent(MarkupContent { kind: MarkupKind::Markdown, value })
}

fn completion_kind(kind: papyrus_ide::CompletionKind) -> CompletionItemKind {
    use papyrus_ide::CompletionKind as K;
    match kind {
        K::Keyword => CompletionItemKind::KEYWORD,
        K::Function => CompletionItemKind::FUNCTION,
        K::Event => CompletionItemKind::EVENT,
        K::Property => CompletionItemKind::PROPERTY,
        K::Variable => CompletionItemKind::FIELD,
        K::Local => CompletionItemKind::VARIABLE,
        K::Script => CompletionItemKind::CLASS,
        K::Type => CompletionItemKind::STRUCT,
        K::Flag => CompletionItemKind::KEYWORD,
        K::State => CompletionItemKind::ENUM_MEMBER,
        K::Snippet => CompletionItemKind::SNIPPET,
    }
}

fn symbol_kind(kind: papyrus_ide::SymbolKind) -> lsp_types::SymbolKind {
    use papyrus_ide::SymbolKind as K;
    match kind {
        K::Script => lsp_types::SymbolKind::CLASS,
        K::Function => lsp_types::SymbolKind::FUNCTION,
        K::Event => lsp_types::SymbolKind::EVENT,
        K::Property => lsp_types::SymbolKind::PROPERTY,
        K::Variable => lsp_types::SymbolKind::FIELD,
        K::State => lsp_types::SymbolKind::NAMESPACE,
        K::Flag => lsp_types::SymbolKind::CONSTANT,
    }
}

fn document_symbol(lines: &Lines, s: papyrus_ide::DocumentSymbol) -> lsp_types::DocumentSymbol {
    let range = lines.range(s.full_range);
    let mut selection = lines.range(s.focus_range);
    // The selection range must be contained in the full range.
    if selection.start < range.start || selection.end > range.end {
        selection = range;
    }
    #[allow(deprecated)]
    lsp_types::DocumentSymbol {
        name: if s.name.is_empty() { "?".to_owned() } else { s.name },
        detail: s.detail,
        kind: symbol_kind(s.kind),
        tags: None,
        deprecated: None,
        range,
        selection_range: selection,
        children: Some(s.children.into_iter().map(|c| document_symbol(lines, c)).collect()),
    }
}
