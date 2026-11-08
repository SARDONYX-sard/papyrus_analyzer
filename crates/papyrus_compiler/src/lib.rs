//! Compiler of Papyrus scripts (Skyrim) to `.pex` files.
//!
//! The front end is the one of the language server ([`papyrus_ide`]): the
//! script is parsed, checked ([`papyrus_ide::diagnostics`]), lowered to HIR
//! and type checked. Each body then goes through:
//!
//! 1. [`codegen`]: HIR to [`ir::FunctionIr`], instructions with labels and
//!    named operands, following the conventions of the official compiler;
//! 2. [`opt`]: optional optimization passes on the IR;
//! 3. assembly: labels to jump offsets, names to the string table, and the
//!    debug line numbers.

pub mod cli;
mod codegen;
pub mod ir;
mod literal;
pub mod loader;
pub mod log;
pub mod opt;
mod script;
mod strings;

use std::path::Path;

use papyrus_ide::{Diagnostic, FileId, Severity, Workspace, sema::FileSource};
use papyrus_pex::PexFile;

/// Options of [`compile`].
#[derive(Clone, Debug)]
pub struct CompileOptions {
    /// Run the optimization passes of [`opt`].
    pub optimize: bool,
    /// Write line numbers, needed for script errors in the game logs.
    pub debug_info: bool,
}

impl Default for CompileOptions {
    fn default() -> Self {
        CompileOptions { optimize: false, debug_info: true }
    }
}

/// Why a script did not compile.
#[derive(Clone, Debug)]
pub enum CompileError {
    /// The source could not be read.
    Unreadable,
    /// The script has errors.
    Diagnostics(Vec<Diagnostic>),
}

/// Compiles `file` of the workspace. The scripts it uses (parents, imports,
/// types) must be known to the workspace.
pub fn compile(
    ws: &Workspace,
    file: FileId,
    options: &CompileOptions,
) -> Result<PexFile<'static>, CompileError> {
    let path = &ws.file(file).path;
    let start = std::time::Instant::now();
    log::info!("checking {}", path.display());
    // Read, parsed, lowered and type checked once, for the checks and the
    // code.
    let (source, syntax_errors) =
        FileSource::load_with_errors(ws, file).ok_or(CompileError::Unreadable)?;
    let (diagnostics, bodies) =
        papyrus_ide::diagnostics_with_bodies(ws, file, &source, &syntax_errors);
    let errors: Vec<Diagnostic> =
        diagnostics.into_iter().filter(|d| d.severity == Severity::Error).collect();
    if !errors.is_empty() {
        log::info!("{} errors in {}", errors.len(), path.display());
        return Err(CompileError::Diagnostics(errors));
    }
    log::debug!("checked in {:?}", start.elapsed());
    let sema = source.sema(ws, file);
    let info = script::SourceInfo {
        file_name: path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        modified: modified_time(path),
    };
    let pex = script::assemble(sema, bodies, &info, options);
    log::info!(
        "compiled {} in {:?} ({} strings)",
        sema.index.name_str(),
        start.elapsed(),
        pex.string_table.len()
    );
    Ok(pex)
}

fn modified_time(path: &Path) -> i64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs() as i64)
}

#[cfg(test)]
mod tests;
