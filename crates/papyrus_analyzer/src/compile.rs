//! `papyrus_analyzer compile`: compiles scripts with the built-in compiler.
//!
//! With arguments, they are those of `PapyrusCompiler.exe` (see
//! [`papyrus_compiler::cli`]). Without, the project of the `papyrus.toml` of
//! the current directory is compiled: its sources, imports, flags file,
//! output directory and optimization setting.

#![allow(clippy::print_stdout, clippy::print_stderr)]

use papyrus_compiler::cli::{self, Args};

const USAGE: &str = "\
usage: papyrus_analyzer compile
       papyrus_analyzer compile <object or folder> [<arguments>]

Without arguments: compiles the project of the papyrus.toml of the current
directory (or its ancestors): its sources, imports, flags file, output and
optimization. With arguments: like PapyrusCompiler.exe, as below.

";

/// Runs the subcommand and returns the process exit code.
pub(crate) fn run(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}{}", cli::USAGE);
        return 0;
    }
    if !args.is_empty() {
        return cli::run(args);
    }
    let cwd = std::env::current_dir().unwrap_or_default();
    let Some(manifest) = papyrus_project::find_manifest(&cwd) else {
        eprintln!(
            "no papyrus.toml found; pass the arguments of PapyrusCompiler.exe\n\n{}",
            cli::USAGE
        );
        return 2;
    };
    let vars = papyrus_project::Variables::for_command_line(&cwd);
    let project = match papyrus_project::load(&manifest, &vars) {
        Ok(p) => p,
        Err(errors) => {
            for e in errors {
                eprintln!("{}: {}", manifest.display(), e.message);
            }
            return 1;
        }
    };
    let mut args = match Args::parse(&[]) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };
    args.targets = project.sources.iter().map(|p| p.to_string_lossy().into_owned()).collect();
    args.all = true;
    args.imports = project.import_dirs();
    args.flags = project.flags.clone();
    args.output = Some(project.output.clone());
    args.options.optimize = project.optimize;
    args.quiet = project.quiet;
    // `[build.header]`, over the defaults (`SOURCE_DATE_EPOCH` included).
    let (header, settings) = (&mut args.options.header, &project.header);
    header.user = settings.user.clone().or(header.user.take());
    header.computer = settings.computer.clone().or(header.computer.take());
    header.time = settings.time.or(header.time);
    header.source_time = settings.source_time.or(header.source_time);
    cli::run_args(&args)
}
