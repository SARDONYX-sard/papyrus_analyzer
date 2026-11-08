//! Compiling the scripts of a command line on all cores, writing their
//! outputs, and reporting in the order of the scripts.

use std::{
    io::IsTerminal,
    path::{Path, PathBuf},
};

use annotate_snippets::Renderer;
use papyrus_ide::{FileId, Workspace};
use rayon::prelude::*;

use super::{
    Args, AsmMode, DiagnosticFormat,
    diagnostics::{render, render_official},
};
use crate::{CompileError, log};

/// Compiles `scripts` of `ws` into `output` (which exists), prints what
/// happened, and returns how many failed.
pub(super) fn compile_all(
    ws: &Workspace,
    scripts: &[PathBuf],
    args: &Args,
    output: &Path,
) -> usize {
    let terminal = std::io::stderr().is_terminal();
    let format = args.diagnostics.unwrap_or(if terminal {
        DiagnosticFormat::Human
    } else {
        DiagnosticFormat::Official
    });
    let renderer = if terminal { Renderer::styled() } else { Renderer::plain() };
    let cx = BatchCx { ws, args, output, format, renderer: &renderer };
    let jobs: Vec<(&PathBuf, FileId)> =
        scripts.iter().filter_map(|path| Some((path, ws.file_id(path)?))).collect();
    // Compiled and written on all cores; `collect` keeps the order of the
    // reports.
    let reports: Vec<Report> =
        jobs.par_iter().map(|&(path, file)| cx.compile(path, file)).collect();
    let mut failed = 0;
    for report in reports {
        print!("{}", report.stdout);
        eprint!("{}", report.stderr);
        failed += usize::from(report.failed);
    }
    failed
}
/// What every compilation of a batch shares.
struct BatchCx<'a> {
    ws: &'a Workspace,
    args: &'a Args,
    output: &'a Path,
    format: DiagnosticFormat,
    renderer: &'a Renderer,
}

/// What the compilation of one script prints, and whether it failed.
#[derive(Default)]
struct Report {
    stdout: String,
    stderr: String,
    failed: bool,
}

impl BatchCx<'_> {
    /// Compiles one script and writes its outputs. Prints nothing: scripts
    /// compile in parallel, their reports are printed in order.
    fn compile(&self, path: &Path, file: FileId) -> Report {
        use std::fmt::Write as _;
        let (ws, args) = (self.ws, self.args);
        let mut r = Report::default();
        if !args.quiet {
            let _ = writeln!(r.stdout, "Compiling \"{}\"...", path.display());
        }
        match crate::compile(ws, file, &args.options) {
            Ok(pex) => {
                let name = ws.file(file).index.name_str();
                match write_outputs(&pex, self.output, name, args, &mut r.stdout) {
                    Ok(()) if !args.quiet => r.stdout.push_str("Compilation succeeded.\n"),
                    Ok(()) => {}
                    Err(e) => {
                        let _ = writeln!(r.stderr, "{e}");
                        r.failed = true;
                    }
                }
            }
            Err(CompileError::Unreadable) => {
                let _ = writeln!(r.stderr, "{}: cannot read the file", path.display());
                r.failed = true;
            }
            Err(CompileError::Diagnostics(diags)) => {
                let text = ws.text(file).unwrap_or_default();
                for d in &diags {
                    let _ = match self.format {
                        DiagnosticFormat::Human => {
                            writeln!(r.stderr, "{}", render(self.renderer, ws, path, &text, d))
                        }
                        DiagnosticFormat::Official => {
                            writeln!(r.stderr, "{}", render_official(path, &text, d))
                        }
                    };
                }
                let _ = writeln!(
                    r.stderr,
                    "No output generated for {}, compilation failed.",
                    path.display()
                );
                r.failed = true;
            }
        }
        r
    }
}

fn write_outputs(
    pex: &papyrus_pex::PexFile<'_>,
    dir: &Path,
    name: &str,
    args: &Args,
    stdout: &mut String,
) -> Result<(), String> {
    let asm = || papyrus_pex::disasm::disassemble(pex, Default::default());
    if args.debug {
        stdout.push_str(&asm());
        stdout.push('\n');
    }
    if args.asm != AsmMode::None {
        let path = dir.join(format!("{name}.pas"));
        std::fs::write(&path, asm()).map_err(|e| format!("{}: {e}", path.display()))?;
        log::info!("wrote {}", path.display());
    }
    if args.asm != AsmMode::Only {
        let path = dir.join(format!("{name}.pex"));
        let bytes = pex.to_bytes().map_err(|e| format!("{}: {e}", path.display()))?;
        std::fs::write(&path, &bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        log::info!("wrote {} ({} bytes)", path.display(), bytes.len());
    }
    Ok(())
}
