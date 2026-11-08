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

mod batch;
mod diagnostics;
mod targets;

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use papyrus_ide::Workspace;

pub use crate::log::LogTarget;
use crate::{CompileOptions, log};
use targets::{find_in, resolve_target};

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

    let output = args.output.clone().unwrap_or_else(|| cwd.clone());
    // Once, not per script.
    if let Err(e) = std::fs::create_dir_all(&output) {
        eprintln!("{}: {e}", output.display());
        return 1;
    }
    let failed = batch::compile_all(&ws, &scripts, args, &output);
    if !args.quiet || failed > 0 {
        println!(
            "\nBatch compile of {} files finished. {} succeeded, {failed} failed.",
            scripts.len(),
            scripts.len() - failed
        );
    }
    i32::from(failed > 0)
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
