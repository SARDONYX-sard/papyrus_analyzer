//! The command line of the compiler, compatible with `PapyrusCompiler.exe`:
//!
//! ```text
//! papyrus_compiler <object or folder> [<arguments>]
//!   -import|i=<dirs>   import directories, separated by semicolons
//!   -output|o=<dir>    output directory (default: the current directory)
//!   -flags|f=<file>    flags file, also searched in the import directories
//!   -optimize|op       optimize
//!   -all|a             compile every script of the folder
//!   -quiet|q           only report failures
//!   -debug|d           print the generated code
//!   -noasm             do not write an assembly file (default)
//!   -keepasm           also write the assembly (`.pas`) next to the `.pex`
//!   -asmonly           only write the assembly
//!   -release, -final   accepted for Fallout 4 compatibility; no effect
//!   -nodebuginfo       leave out line numbers (not in the official compiler)
//!   -diagnostics=<f>   `human` (snippets, the default on a terminal) or
//!                      `official` (`file(line,col): message`, the default otherwise,
//!                      as `PapyrusCompiler.exe` prints them for problem matchers)
//!   -log[=<file>]      log what the compiler does, to stderr or a file
//!   -loglevel=<filter> log filter like `RUST_LOG` (default: `info`)
//! ```
//!
//! Options start with `-` or `/`; values follow `=`, `:` or come as the next
//! argument. The object is a script name (`Foo` or `Foo.psc`) looked up in
//! the current directory and the import directories, or a path.

#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::{
    io::IsTerminal,
    path::{Path, PathBuf},
    sync::Arc,
};

use annotate_snippets::{AnnotationKind, Level, Renderer, Snippet};
use papyrus_ide::{Diagnostic, FileId, Severity, Workspace};
use rayon::prelude::*;

use crate::{
    CompileError, CompileOptions,
    log::{self, LogTarget},
};

/// The usage text.
pub const USAGE: &str = "\
usage: papyrus_compiler <object or folder> [<arguments>]

  object     the script to compile: a name (`Foo`, `Foo.psc`) or a path
  folder     the folder to compile, with -all

arguments:
  -import|i=<dirs>   import directories, separated by semicolons
  -output|o=<dir>    output directory (default: the current directory)
  -flags|f=<file>    flags file, also searched in the import directories
  -optimize|op       optimize
  -all|a             compile every script of the folder
  -quiet|q           only report failures
  -debug|d           print the generated code
  -noasm             do not write an assembly file (default)
  -keepasm           also write the assembly (.pas) next to the .pex
  -asmonly           only write the assembly
  -nodebuginfo       leave out line numbers
  -diagnostics=<f>   human (default on a terminal) or official (file(line,col): message)
  -log[=<file>]      log what the compiler does, to stderr or a file
  -loglevel=<filter> log filter: error, warn, info (default), debug, trace
  -?                 print this help

examples:
  Compile one script found in the import directories:
    papyrus_compiler MyQuestScript -i=\"Scripts\\Source;C:\\Skyrim\\Data\\Scripts\\Source\" -o=Scripts -f=TESV_Papyrus_Flags.flg

  Compile every script of a folder, optimized, only reporting failures:
    papyrus_compiler Scripts\\Source -all -op -q -i=\"Scripts\\Source;C:\\Skyrim\\Data\\Scripts\\Source\" -o=Scripts -f=TESV_Papyrus_Flags.flg

  Look at the generated code without writing a .pex:
    papyrus_compiler Scripts\\Source\\MyQuestScript.psc -asmonly -o=out -i=C:\\Skyrim\\Data\\Scripts\\Source

  Log what happens to a file, functions included:
    papyrus_compiler MyQuestScript -i=... -log=compile.log -loglevel=debug

  In PowerShell, quote arguments containing dots: \"-f=TESV_Papyrus_Flags.flg\".";

/// How errors are printed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiagnosticFormat {
    /// With the source lines, for people.
    Human,
    /// `file(line,col): message`, like `PapyrusCompiler.exe`.
    Official,
}

/// What to write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AsmMode {
    /// Only the `.pex`.
    None,
    /// The `.pex` and the `.pas`.
    Keep,
    /// Only the `.pas`.
    Only,
}

/// Parsed arguments.
#[derive(Clone, Debug)]
pub struct Args {
    /// Objects, or folders with `all`.
    pub targets: Vec<String>,
    /// Import directories, in priority order.
    pub imports: Vec<PathBuf>,
    /// Output directory.
    pub output: Option<PathBuf>,
    /// Flags file.
    pub flags: Option<PathBuf>,
    /// Compile whole folders.
    pub all: bool,
    /// Only report failures.
    pub quiet: bool,
    /// Print the generated code.
    pub debug: bool,
    /// Assembly output.
    pub asm: AsmMode,
    /// Compiler options.
    pub options: CompileOptions,
    /// `-?`
    pub help: bool,
    /// `-diagnostics`: `None` picks by whether stderr is a terminal.
    pub diagnostics: Option<DiagnosticFormat>,
    /// `-log`: where progress logs go.
    pub log: Option<LogTarget>,
    /// `-loglevel`
    pub log_level: String,
}

impl Args {
    /// Parses `PapyrusCompiler.exe` style arguments.
    pub fn parse(args: &[String]) -> Result<Args, String> {
        let mut out = Args {
            targets: Vec::new(),
            imports: Vec::new(),
            output: None,
            flags: None,
            all: false,
            quiet: false,
            debug: false,
            asm: AsmMode::None,
            options: CompileOptions::default(),
            help: false,
            diagnostics: None,
            log: None,
            log_level: "info".to_owned(),
        };
        let mut it = args.iter();
        while let Some(arg) = it.next() {
            let Some(opt) = arg.strip_prefix("--").or_else(|| arg.strip_prefix(['-', '/'])) else {
                out.targets.push(arg.clone());
                continue;
            };
            // `/` also starts absolute paths on Unix.
            if arg.starts_with('/') && Path::new(arg).exists() {
                out.targets.push(arg.clone());
                continue;
            }
            let (name, inline) = match opt.find(['=', ':']) {
                // Keep `C:\` in values: split at the first separator only.
                Some(i) => (&opt[..i], Some(opt[i + 1..].to_owned())),
                None => (opt, None),
            };
            let mut value = || {
                inline
                    .clone()
                    .or_else(|| it.next().cloned())
                    .ok_or(format!("`{arg}` needs a value"))
            };
            match name.to_ascii_lowercase().as_str() {
                "import" | "i" => out.imports.extend(
                    value()?.split(';').map(str::trim).filter(|s| !s.is_empty()).map(PathBuf::from),
                ),
                "output" | "o" => out.output = Some(PathBuf::from(value()?)),
                "flags" | "f" => out.flags = Some(PathBuf::from(value()?)),
                "optimize" | "op" => out.options.optimize = true,
                "all" | "a" => out.all = true,
                "quiet" | "q" => out.quiet = true,
                "debug" | "d" => out.debug = true,
                "noasm" => out.asm = AsmMode::None,
                "keepasm" => out.asm = AsmMode::Keep,
                "asmonly" => out.asm = AsmMode::Only,
                "release" | "final" => {}
                "nodebuginfo" => out.options.debug_info = false,
                "diagnostics" => {
                    out.diagnostics = Some(match value()?.to_ascii_lowercase().as_str() {
                        "human" => DiagnosticFormat::Human,
                        "official" => DiagnosticFormat::Official,
                        other => return Err(format!("unknown diagnostics format `{other}`")),
                    });
                }
                // The file is optional: only take it after `=`.
                "log" => {
                    out.log = Some(
                        inline.clone().map_or(LogTarget::Stderr, |f| LogTarget::File(f.into())),
                    );
                }
                "loglevel" => out.log_level = value()?,
                "?" | "h" | "help" => out.help = true,
                _ => return Err(format!("unknown argument `{arg}`")),
            }
        }
        Ok(out)
    }
}

/// Runs the compiler with `PapyrusCompiler.exe` style arguments and returns
/// the process exit code.
pub fn run(args: &[String]) -> i32 {
    match Args::parse(args) {
        Ok(a) if a.help => {
            println!("{USAGE}");
            0
        }
        Ok(a) if a.targets.is_empty() => {
            eprintln!("{USAGE}");
            2
        }
        Ok(a) => run_args(&a),
        Err(e) => {
            eprintln!("{e}\n\n{USAGE}");
            2
        }
    }
}

/// Runs the compiler.
pub fn run_args(args: &Args) -> i32 {
    if let Some(target) = &args.log
        && let Err(e) = log::init(target, &args.log_level)
    {
        eprintln!("{e}");
        return 2;
    }
    let cwd = std::env::current_dir().unwrap_or_default();
    let mut scripts = Vec::new();
    for target in &args.targets {
        match resolve_target(target, args, &cwd) {
            Ok(found) => scripts.extend(found),
            Err(e) => {
                eprintln!("{e}");
                return 1;
            }
        }
    }

    let mut ws = Workspace::new();
    if let Some(flags) = &args.flags {
        let path = find_in(flags, &args.imports, &cwd);
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                log::info!("flags file {}", path.display());
                ws.set_flags_file(path, Arc::from(text));
            }
            Err(e) => {
                eprintln!("{}: {e}", path.display());
                return 1;
            }
        }
    }
    log::info!("{} scripts to compile", scripts.len());
    crate::loader::load(&mut ws, &scripts, &args.imports);

    let terminal = std::io::stderr().is_terminal();
    let format = args.diagnostics.unwrap_or(if terminal {
        DiagnosticFormat::Human
    } else {
        DiagnosticFormat::Official
    });
    let renderer = if terminal { Renderer::styled() } else { Renderer::plain() };
    let output = args.output.clone().unwrap_or_else(|| cwd.clone());
    // Once, not per script.
    if let Err(e) = std::fs::create_dir_all(&output) {
        eprintln!("{}: {e}", output.display());
        return 1;
    }
    let cx = BatchCx { ws: &ws, args, output: &output, format, renderer: &renderer };
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
    if !args.quiet || failed > 0 {
        println!(
            "\nBatch compile of {} files finished. {} succeeded, {failed} failed.",
            scripts.len(),
            scripts.len() - failed
        );
    }
    i32::from(failed > 0)
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

/// The scripts named by a command line target.
fn resolve_target(target: &str, args: &Args, cwd: &Path) -> Result<Vec<PathBuf>, String> {
    if args.all {
        let dir = find_in(Path::new(target), &args.imports, cwd);
        if !dir.is_dir() {
            return Err(format!("folder not found: {target}"));
        }
        let mut out = Vec::new();
        // Like the official compiler, `-all` does not recurse.
        collect(&dir, &mut out, false);
        return Ok(out);
    }
    let mut file = PathBuf::from(target);
    if file.extension().is_none() {
        file.set_extension("psc");
    }
    let found = find_in(&file, &args.imports, cwd);
    if found.is_file() {
        Ok(vec![std::path::absolute(&found).unwrap_or(found)])
    } else {
        Err(format!("script not found: {target}"))
    }
}

/// `path` as is when it exists, else the first match in the current
/// directory and the import directories.
fn find_in(path: &Path, imports: &[PathBuf], cwd: &Path) -> PathBuf {
    if path.is_absolute() || path.exists() {
        return path.to_path_buf();
    }
    std::iter::once(cwd)
        .chain(imports.iter().map(PathBuf::as_path))
        .map(|dir| dir.join(path))
        .find(|p| p.exists())
        .unwrap_or_else(|| path.to_path_buf())
}

/// A diagnostic with its source lines, and the related places (another
/// definition, the declaration of a script...) as context annotations.
fn render(renderer: &Renderer, ws: &Workspace, path: &Path, text: &str, d: &Diagnostic) -> String {
    let level = match d.severity {
        Severity::Error => Level::ERROR,
        Severity::Warning => Level::WARNING,
    };
    let clamp = |text: &str, r: papyrus_ide::TextRange| {
        let start = usize::from(r.start()).min(text.len());
        start..usize::from(r.end()).clamp(start, text.len())
    };
    let path_text = path.display().to_string();
    let file = ws.file_id(path);
    // Related places of other files, with their text.
    let others: Vec<(String, std::sync::Arc<str>, &papyrus_ide::Related)> = d
        .related
        .iter()
        .filter(|r| Some(r.file) != file)
        .filter_map(|r| Some((ws.file(r.file).path.display().to_string(), ws.text(r.file)?, r)))
        .collect();
    let mut snippet = Snippet::source(text)
        .line_start(1)
        .path(&path_text)
        .annotation(AnnotationKind::Primary.span(clamp(text, d.range)).label(d.code));
    for r in d.related.iter().filter(|r| Some(r.file) == file) {
        snippet = snippet
            .annotation(AnnotationKind::Context.span(clamp(text, r.range)).label(&r.message));
    }
    let mut group = level.primary_title(&d.message).element(snippet);
    for (other_path, other_text, r) in &others {
        group = group.element(
            Snippet::source(&**other_text).line_start(1).path(other_path).annotation(
                AnnotationKind::Context.span(clamp(other_text, r.range)).label(&r.message),
            ),
        );
    }
    renderer.render(&[group])
}
/// `file(line,col): message`, one-based, as `PapyrusCompiler.exe` prints.
fn render_official(path: &Path, text: &str, d: &Diagnostic) -> String {
    let start = usize::from(d.range.start()).min(text.len());
    let before = &text[..text.floor_char_boundary(start)];
    let line = before.matches('\n').count() + 1;
    let col = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
    format!("{}({line},{col}): {}", path.display(), d.message)
}

/// The `.psc` files of `path`.
fn collect(path: &Path, out: &mut Vec<PathBuf>, recursive: bool) {
    if path.is_file() {
        if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("psc")) {
            // Absolute, so that a file reached from a target and from an
            // import directory is the same file of the workspace.
            out.push(std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf()));
        }
        return;
    }
    let Ok(entries) = std::fs::read_dir(path) else { return };
    // The type comes with the entry: no `stat` per file, which matters with
    // thousands of scripts.
    let mut entries: Vec<(PathBuf, bool)> =
        entries.flatten().map(|e| (e.path(), e.file_type().is_ok_and(|t| t.is_dir()))).collect();
    entries.sort();
    for (entry, is_dir) in entries {
        if is_dir {
            if recursive {
                collect(&entry, out, recursive);
            }
        } else if entry.extension().is_some_and(|e| e.eq_ignore_ascii_case("psc")) {
            out.push(std::path::absolute(&entry).unwrap_or(entry));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Args {
        Args::parse(&args.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>()).unwrap()
    }

    #[test]
    fn official_arguments() {
        let a = parse(&[
            "Foo.psc",
            "-f=TESV_Papyrus_Flags.flg",
            r"-i=C:\Src;D:\Other",
            "-o",
            r"C:\Out",
            "-op",
            "/q",
            "-keepasm",
        ]);
        assert_eq!(a.targets, ["Foo.psc"]);
        assert_eq!(a.flags, Some(PathBuf::from("TESV_Papyrus_Flags.flg")));
        assert_eq!(a.imports, [PathBuf::from(r"C:\Src"), PathBuf::from(r"D:\Other")]);
        assert_eq!(a.output, Some(PathBuf::from(r"C:\Out")));
        assert!(a.options.optimize && a.quiet);
        assert_eq!(a.asm, AsmMode::Keep);
        assert!(Args::parse(&["-bogus".to_owned()]).is_err());
    }
}
