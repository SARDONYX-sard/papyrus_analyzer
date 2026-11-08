//! Compiler of Papyrus scripts (Skyrim) to `.pex` files.
//!
//! The public API is small: [`compile`] a script of a workspace that
//! [`loader`] filled, or run the command line of `PapyrusCompiler.exe`
//! ([`cli`]). Inside, a script goes through:
//!
//! 1. the front end of the language server ([`papyrus_ide`]): parsed,
//!    checked ([`papyrus_ide::diagnostics`]), lowered to HIR and type
//!    checked;
//! 2. `codegen`: each function body to instructions with labels and named
//!    operands (`codegen::ir`), following the conventions of the official
//!    compiler, then the optional optimizations (`codegen::opt`);
//! 3. `assemble`: the declarations and the bodies to a `.pex` file: labels to
//!    jump offsets, names to the string table, and the debug line numbers.
//!
//! ```text
//! src/
//!   lib.rs        compile(): the steps above, for one script
//!   loader.rs     the scripts a compilation needs, in a workspace
//!   codegen/      a function body to instructions
//!   assemble/     a script to a .pex file
//!   cli/          the command line
//!   log.rs        progress logs (`-log`)
//! ```

mod assemble;
pub mod cli;
mod codegen;
pub mod loader;
mod log;

use papyrus_ide::{Diagnostic, FileId, Severity, Workspace, sema::FileSource};
use papyrus_pex::PexFile;

/// Options of [`compile`].
#[derive(Clone, Debug)]
pub struct CompileOptions {
    /// Run the optimization passes (like `-optimize` of the official
    /// compiler).
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
    let info = assemble::SourceInfo {
        file_name: path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        modified: ws
            .modified(file)
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs() as i64),
    };
    let pex = assemble::assemble(sema, bodies, &info, options);
    log::info!(
        "compiled {} in {:?} ({} strings)",
        sema.index.name_str(),
        start.elapsed(),
        pex.string_table.len()
    );
    Ok(pex)
}

#[cfg(test)]
mod tests;
