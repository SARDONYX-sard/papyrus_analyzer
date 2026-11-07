//! `papyrus_analyzer fmt`: formats scripts from the command line, with the
//! same options as the editor: `.editorconfig`, then `[format]` of
//! `papyrus.toml`.
//!
//! Names are written like their definition, as in the editor: the scripts
//! are resolved against the imports of the `papyrus.toml` of the current
//! directory (or its ancestors) and its flags file; without manifest, against
//! the game scripts of `SKYRIM_DIR`. Only the scripts the formatted ones use
//! are read.

#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use papyrus_ide::{FileId, Workspace};

const USAGE: &str = "\
usage: papyrus_analyzer fmt [OPTIONS] [PATH]...

Formats scripts (directories recursively; default: the current directory)
with .editorconfig, then [format] of papyrus.toml. Names are written like
their definition, resolved against the imports of papyrus.toml.

options:
  --check      only list the unformatted files; exit code 1 if any
  -h, --help   print this help";

/// Runs the subcommand and returns the process exit code: 0 on success, 1
/// when `--check` found unformatted files or a file could not be formatted.
pub(crate) fn run(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return 0;
    }
    let check = args.iter().any(|a| a == "--check");
    let mut paths: Vec<PathBuf> =
        args.iter().filter(|a| !a.starts_with("--")).map(PathBuf::from).collect();
    if paths.is_empty() {
        paths.push(PathBuf::from("."));
    }
    let mut scripts = Vec::new();
    for path in &paths {
        collect(path, &mut scripts);
    }
    scripts.retain(|s| !papyrus_project::format_excludes(s));

    let sources: Vec<Result<Source, String>> = scripts.iter().map(|s| Source::read(s)).collect();
    let mut ws = workspace(&scripts);
    let files: Vec<Option<FileId>> = scripts
        .iter()
        .zip(&sources)
        .map(|(path, src)| {
            // The text formatted, so that the offsets of the names match.
            let src = src.as_ref().ok()?;
            Some(ws.set_open(path.clone(), Arc::from(src.text.as_str()), 0))
        })
        .collect();

    // Files are independent: formatted in parallel, reported in order.
    use rayon::prelude::*;
    let results: Vec<Result<bool, String>> = scripts
        .par_iter()
        .zip(&sources)
        .zip(&files)
        .map(|((path, src), file)| {
            let src = src.as_ref().map_err(Clone::clone)?;
            let names = file.map(|f| names(&ws, f)).unwrap_or_default();
            format_file(path, src, check, names)
        })
        .collect();
    let mut failed = false;
    let mut changed = 0;
    for (script, result) in scripts.iter().zip(results) {
        match result {
            Ok(true) => {
                changed += 1;
                if check {
                    println!("{}", script.display());
                    failed = true;
                }
            }
            Ok(false) => {}
            Err(err) => {
                eprintln!("{}: {err}", script.display());
                failed = true;
            }
        }
    }
    if !check {
        println!("formatted {changed} of {} scripts", scripts.len());
    }
    i32::from(failed)
}

/// A script to format.
struct Source {
    /// The file starts with a byte order mark, kept when writing.
    bom: bool,
    text: String,
}

impl Source {
    fn read(path: &Path) -> Result<Source, String> {
        let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        let (bom, body) = match bytes.strip_prefix(b"\xEF\xBB\xBF") {
            Some(rest) => (true, rest),
            None => (false, &bytes[..]),
        };
        // Writing back other encodings (often Windows-1252) as UTF-8 would
        // change the file beyond formatting.
        let text = std::str::from_utf8(body).map_err(|_| "skipped: not UTF-8".to_owned())?;
        Ok(Source { bom, text: text.to_owned() })
    }
}

/// Formats one file; returns whether it changed (or would change).
fn format_file(
    path: &Path,
    src: &Source,
    check: bool,
    names: papyrus_fmt::Names,
) -> Result<bool, String> {
    let options = papyrus_fmt::FormatOptions {
        names: Arc::new(names),
        ..papyrus_project::format_options_for(path, |_| {})
    };
    let formatted = papyrus_fmt::format(&src.text, &options).map_err(|e| e.to_string())?;
    if formatted == src.text {
        return Ok(false);
    }
    if !check {
        let mut out = Vec::with_capacity(formatted.len() + 3);
        if src.bom {
            out.extend_from_slice(b"\xEF\xBB\xBF");
        }
        out.extend_from_slice(formatted.as_bytes());
        std::fs::write(path, out).map_err(|e| e.to_string())?;
    }
    Ok(true)
}

/// The definitions of the names of `file`, for the formatter.
pub(crate) fn names(ws: &Workspace, file: FileId) -> papyrus_fmt::Names {
    use papyrus_fmt::NameKind as Fmt;
    use papyrus_ide::NameKind as Ide;
    let mut names = papyrus_fmt::Names::default();
    for n in papyrus_ide::name_spellings(ws, file) {
        let kind = match n.kind {
            Ide::Script => Fmt::Script,
            Ide::Function => Fmt::Function,
            Ide::Property => Fmt::Property,
            Ide::Variable => Fmt::Variable,
            Ide::Flag => Fmt::Flag,
        };
        names.insert(n.range.start().into(), kind, n.spelling);
    }
    names
}

/// A workspace with `scripts`, the scripts they use from the imports and the
/// flags file, for name resolution (`fmt`, `lint`): the imports of the
/// `papyrus.toml` of the current directory, else the game scripts.
pub(crate) fn workspace(scripts: &[PathBuf]) -> Workspace {
    workspace_with_imports(scripts).0
}

/// Like [`workspace`], with the import directories, to load more scripts later.
pub(crate) fn workspace_with_imports(scripts: &[PathBuf]) -> (Workspace, Vec<PathBuf>) {
    let cwd = std::env::current_dir().unwrap_or_default();
    let vars = papyrus_project::Variables::for_command_line(&cwd);
    let manifest =
        cwd.ancestors().map(|d| d.join(papyrus_project::MANIFEST_NAME)).find(|p| p.is_file());
    let (imports, members) = match &manifest {
        Some(m) => match papyrus_project::load_workspace(m, &vars) {
            Ok(ws) => (
                ws.members.iter().flat_map(papyrus_project::Project::import_dirs).collect(),
                ws.members,
            ),
            Err(errors) => {
                // Formatting still works, names of other scripts are kept.
                eprintln!("{}: {}", m.display(), errors[0].message);
                (Vec::new(), Vec::new())
            }
        },
        None => {
            let skyrim = vars
                .lookup(papyrus_project::SKYRIM_DIR)
                .map(|(dir, _)| PathBuf::from(dir))
                .or_else(papyrus_project::detect_skyrim_dir);
            (skyrim.map(|d| papyrus_project::game_script_dirs(&d)).unwrap_or_default(), Vec::new())
        }
    };
    let mut ws = Workspace::new();
    papyrus_compiler::loader::load(&mut ws, scripts, &imports);
    // Each script with what its project sees, and its flags.
    ws.set_scopes(&crate::project::project_scopes(&members, &[]));
    (ws, imports)
}

fn collect(path: &Path, out: &mut Vec<PathBuf>) {
    if path.is_file() {
        if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("psc")) {
            out.push(path.to_path_buf());
        }
        return;
    }
    let Ok(entries) = std::fs::read_dir(path) else { return };
    let mut entries: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    entries.sort();
    for entry in entries {
        let name = entry.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if entry.is_dir() && (name.starts_with('.') || name == "node_modules" || name == "target") {
            continue;
        }
        collect(&entry, out);
    }
}
