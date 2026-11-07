//! Loading the scripts a compilation needs.
//!
//! The import directories of a game hold thousands of scripts, and parsing
//! them all takes longer than compiling a mod. The loader only lists their
//! file names, then parses the scripts the targets need, transitively:
//!
//! - for the compiled scripts, every name they mention (types, `extends`,
//!   `Import`, and identifiers, which may be scripts used as receivers);
//! - for the scripts they use, the names of their declarations (`extends`,
//!   `Import`, the types of properties, variables, parameters and results),
//!   which is all name resolution and type checking look at.
//!
//! The other scripts are registered with their name only: the compiler checks
//! that variables are not named like any known script.

use std::path::{Path, PathBuf};

use papyrus_ide::{
    Origin, ScriptIndex, Workspace,
    index::{BaseType, TypeRef},
};
use papyrus_intern::Name;
use papyrus_parser::{
    SyntaxKind,
    ast::{self, AstNode},
};
use rustc_hash::{FxHashMap, FxHashSet};

use crate::log;

/// Adds `targets` and the scripts of `imports` they need to `ws`.
pub fn load(ws: &mut Workspace, targets: &[PathBuf], imports: &[PathBuf]) {
    let texts: Vec<(PathBuf, String)> = targets
        .iter()
        .filter_map(|path| Some((path.clone(), papyrus_ide::read_file(path)?)))
        .collect();
    load_texts(ws, &texts, imports);
}

/// Like [`load`], with the text of the targets, which need not be on disk: the
/// examples of doc tests.
pub fn load_texts(ws: &mut Workspace, targets: &[(PathBuf, String)], imports: &[PathBuf]) {
    let start = std::time::Instant::now();
    // Script name (lowercase) to file, the first import directory winning.
    let mut files: FxHashMap<String, (PathBuf, Origin)> = FxHashMap::default();
    for (i, dir) in imports.iter().enumerate() {
        let origin = Origin::Import(i.min(u16::MAX as usize) as u16);
        let dir = std::path::absolute(dir).unwrap_or_else(|_| dir.clone());
        for path in scripts_of(&dir) {
            let Some(stem) = path.file_stem() else { continue };
            files.entry(stem.to_string_lossy().to_ascii_lowercase()).or_insert((path, origin));
        }
    }
    log::info!("{} scripts in the import directories", files.len());

    let mut loaded: FxHashSet<String> = FxHashSet::default();
    let mut queue: Vec<String> = Vec::new();
    for (path, text) in targets {
        let index = papyrus_ide::index_text(text);
        if let Some(name) = index.name {
            loaded.insert(name.as_str().to_ascii_lowercase());
        }
        mentioned_names(text, &mut queue);
        declared_names(&index, &mut queue);
        ws.set_disk_index(path.clone(), index, Origin::Workspace);
    }

    let mut parsed = 0;
    while let Some(name) = queue.pop() {
        let key = name.to_ascii_lowercase();
        if !loaded.insert(key.clone()) {
            continue;
        }
        let Some((path, origin)) = files.get(&key) else { continue };
        let Some(text) = papyrus_ide::read_file(path) else { continue };
        let index = papyrus_ide::index_text(&text);
        declared_names(&index, &mut queue);
        ws.set_disk_index(path.clone(), index, *origin);
        parsed += 1;
    }

    // The rest by name only; not the ones a previous load parsed.
    let mut stubs = 0;
    for (key, (path, origin)) in &files {
        if loaded.contains(key) || ws.file_id(path).is_some() {
            continue;
        }
        let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let index = ScriptIndex { name: Some(Name::new(&stem)), ..ScriptIndex::default() };
        ws.set_disk_index(path.clone(), index, *origin);
        stubs += 1;
    }
    log::info!("parsed {parsed} scripts, {stubs} by name only, in {:?}", start.elapsed());
}

/// Every name the source mentions that could be a script.
fn mentioned_names(text: &str, out: &mut Vec<String>) {
    let parse = papyrus_parser::parse(text);
    let root = parse.source_file();
    for node in root.syntax().descendants() {
        let name = match node.kind() {
            SyntaxKind::NAME_REF => ast::NameRef::cast(node).map(|n| n.text()),
            SyntaxKind::TYPE => {
                ast::Type::cast(node).filter(|t| !t.is_primitive()).map(|t| t.base_text())
            }
            _ => None,
        };
        if let Some(name) = name.filter(|n| !n.is_empty()) {
            out.push(name.to_owned());
        }
    }
}

/// The scripts the declarations of a script refer to.
fn declared_names(index: &ScriptIndex, out: &mut Vec<String>) {
    let mut push = |ty: Option<TypeRef>| {
        if let Some(TypeRef { base: BaseType::Object(name), .. }) = ty {
            out.push(name.as_str().to_owned());
        }
    };
    for m in index.members.iter() {
        push(m.ty);
        for p in m.params.iter() {
            push(p.ty);
        }
    }
    out.extend(index.extends.iter().chain(index.imports.iter()).map(|n| n.as_str().to_owned()));
}

/// The `.psc` files below the import directory `dir`, listed once per process:
/// the game scripts do not change while a command runs, and a run of tests loads
/// scripts many times.
fn scripts_of(dir: &Path) -> Vec<PathBuf> {
    use std::sync::{Mutex, OnceLock};
    static LISTED: OnceLock<Mutex<FxHashMap<PathBuf, Vec<PathBuf>>>> = OnceLock::new();
    let listed = LISTED.get_or_init(Default::default);
    if let Some(found) = listed.lock().ok().and_then(|l| l.get(dir).cloned()) {
        return found;
    }
    let mut found = Vec::new();
    list_scripts(dir, &mut found);
    if let Ok(mut l) = listed.lock() {
        l.insert(dir.to_path_buf(), found.clone());
    }
    found
}

/// The `.psc` files below `dir`.
fn list_scripts(dir: &Path, out: &mut Vec<PathBuf>) {
    // The type comes with the entry: no `stat` per file, which matters with
    // thousands of scripts.
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<(PathBuf, bool)> =
        entries.flatten().map(|e| (e.path(), e.file_type().is_ok_and(|t| t.is_dir()))).collect();
    entries.sort();
    for (path, is_dir) in entries {
        if is_dir {
            list_scripts(&path, out);
        } else if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("psc")) {
            out.push(path);
        }
    }
}
