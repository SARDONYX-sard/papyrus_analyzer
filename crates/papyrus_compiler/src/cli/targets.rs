//! The scripts a command line names: a script by name or path, or the
//! scripts of a folder (`-all`).

use std::path::{Path, PathBuf};

use super::Args;
/// The scripts named by a command line target.
pub(super) fn resolve_target(
    target: &str,
    args: &Args,
    cwd: &Path,
) -> Result<Vec<PathBuf>, String> {
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
pub(super) fn find_in(path: &Path, imports: &[PathBuf], cwd: &Path) -> PathBuf {
    if path.is_absolute() || path.exists() {
        return path.to_path_buf();
    }
    std::iter::once(cwd)
        .chain(imports.iter().map(PathBuf::as_path))
        .map(|dir| dir.join(path))
        .find(|p| p.exists())
        .unwrap_or_else(|| path.to_path_buf())
}

/// The `.psc` files of `path`.
pub(super) fn collect(path: &Path, out: &mut Vec<PathBuf>, recursive: bool) {
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
