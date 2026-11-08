//! IDE features for Papyrus, independent of the Language Server Protocol.
//!
//! The entry point is the [`Workspace`], which knows every script file and
//! keeps the editor contents of open files. Feature functions take a
//! workspace, a [`FileId`] and byte offsets, and return plain data with byte
//! ranges; the language server converts them to protocol types.
//!
//! Each file is summarized by a [`ScriptIndex`] whose names are interned, so
//! the whole workspace can be indexed up front, while syntax trees only exist
//! for open files or temporarily during a query.

pub mod assists;
mod db;
mod features;
pub mod flags;
pub mod hir;
pub mod index;
mod line_index;
pub mod mod_events;
pub mod none_flow;
pub mod sema;
pub mod states;
pub mod typeck;

use std::path::Path;

pub use papyrus_parser::{TextRange, TextSize};

pub use crate::{
    db::{DiskDoc, FileData, FileId, OpenDoc, Origin, Scope, Workspace},
    features::{
        completion::{
            CompletionItem, CompletionKind, Completions, completions, flag_file_completions,
            member_docs,
        },
        diagnostics::{
            COMPILER_CODES, CheckedBodies, Diagnostic, Related, Severity, diagnostics,
            diagnostics_of, diagnostics_with_bodies, flag_file_diagnostics,
        },
        folding::{Fold, FoldKind, folding_ranges},
        hover::{HoverResult, flag_file_hover, hover},
        inlay_hints::{InlayHint, inlay_hints},
        name_spellings::{NameKind, NameSpelling, name_spellings},
        navigation::{
            Highlight, NavFile, NavTarget, flag_references, goto_declaration, goto_definition,
            highlight, references,
        },
        overrides::{Overridden, Override, OverrideKind, overridden, overrides},
        rename::{RenameEdit, prepare_rename, rename},
        semantic_tokens::{SemanticToken, TokenModifier, TokenType, semantic_tokens},
        signature_help::{SignatureHelp, signature_help},
        symbols::{
            DocumentSymbol, SymbolKind, WorkspaceSymbol, document_symbols, flag_file_symbols,
            workspace_symbols,
        },
    },
    index::ScriptIndex,
    line_index::{LineCol, LineIndex},
};

/// Decodes the bytes of a script file.
///
/// Papyrus sources are often saved in Windows-1252 rather than UTF-8. Valid
/// UTF-8 is used as is (minus a byte order mark); anything else is decoded as
/// Windows-1252, which never fails.
pub fn decode_text(bytes: &[u8]) -> String {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_owned(),
        Err(_) => bytes.iter().map(|&b| windows_1252(b)).collect(),
    }
}

/// Like [`decode_text`], reusing the buffer of UTF-8 text instead of copying it.
pub fn decode_owned(mut bytes: Vec<u8>) -> String {
    if bytes.starts_with(b"\xEF\xBB\xBF") {
        bytes.drain(..3);
    }
    match String::from_utf8(bytes) {
        Ok(s) => s,
        Err(e) => e.as_bytes().iter().map(|&b| windows_1252(b)).collect(),
    }
}

fn windows_1252(b: u8) -> char {
    const HIGH: [char; 32] = [
        '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8D}', 'Ž',
        '\u{8F}', '\u{90}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{9D}',
        'ž', 'Ÿ',
    ];
    match b {
        0x80..=0x9F => HIGH[(b - 0x80) as usize],
        _ => b as char,
    }
}

/// Reads and decodes a script file.
pub fn read_file(path: &Path) -> Option<String> {
    std::fs::read(path).ok().map(decode_owned)
}

/// Like [`read_file`], with the modification time of the file, asked to the
/// open file rather than by path (one lookup of the path instead of two).
pub fn read_file_with_time(path: &Path) -> Option<(String, Option<std::time::SystemTime>)> {
    use std::io::Read as _;
    let mut file = std::fs::File::open(path).ok()?;
    let meta = file.metadata().ok();
    let mut bytes = Vec::with_capacity(meta.as_ref().map_or(0, |m| m.len() as usize));
    file.read_to_end(&mut bytes).ok()?;
    Some((decode_owned(bytes), meta.and_then(|m| m.modified().ok())))
}

/// Parses `text` and summarizes its declarations.
pub fn index_text(text: &str) -> ScriptIndex {
    ScriptIndex::build(papyrus_parser::parse(text).source_file())
}

/// The documentation of papyrus_analyzer itself, as written, with the lines of
/// its doc tests: `(name, Markdown)` of the keywords and the built-in methods.
pub fn documentation() -> Vec<(String, &'static str)> {
    use crate::sema::Builtin;
    let keywords = features::keyword_docs::script_keyword_docs()
        .iter()
        .map(|d| (format!("keyword {}", d.name), d.body));
    let builtins = [
        Builtin::Length,
        Builtin::Find,
        Builtin::RFind,
        Builtin::GoToState,
        Builtin::GetState,
        Builtin::OnBeginState,
        Builtin::OnEndState,
    ]
    .into_iter()
    .map(|b| (format!("built-in {}", b.name()), b.docs_source()));
    keywords.chain(builtins).collect()
}

#[cfg(test)]
mod tests;
