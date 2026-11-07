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
pub mod none_flow;
pub mod sema;
pub mod states;
pub mod typeck;

use std::path::Path;

pub use papyrus_parser::{TextRange, TextSize};

pub use crate::{
    db::{FileData, FileId, OpenDoc, Origin, Scope, Workspace},
    features::{
        completion::{
            CompletionItem, CompletionKind, Completions, completions, flag_file_completions,
            member_docs,
        },
        diagnostics::{
            COMPILER_CODES, Diagnostic, Related, Severity, diagnostics, diagnostics_of,
            flag_file_diagnostics,
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
    std::fs::read(path).ok().map(|b| decode_text(&b))
}

/// Parses `text` and summarizes its declarations.
pub fn index_text(text: &str) -> ScriptIndex {
    ScriptIndex::build(papyrus_parser::parse(text).source_file())
}

#[cfg(test)]
mod tests;
