//! The set of known script files.
//!
//! Every file has a [`ScriptIndex`]. Files opened in the editor additionally keep
//! their text and syntax tree, which are replaced on every edit. Closed files are
//! read back from disk on demand, e.g. to show the documentation of a function
//! declared in another script.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use papyrus_intern::{Name, Symbol};
use papyrus_parser::{SyntaxError, SyntaxTree};
use rustc_hash::FxHashMap;

use crate::{LineIndex, ScriptIndex, flags::FlagTable};

/// Identifies a file of a [`Workspace`]. Ids are never reused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FileId(pub u32);

/// Where a file comes from, which decides which file wins when several files
/// declare the same script.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Origin {
    /// A file of the opened workspace folders.
    Workspace,
    /// A file of the `n`-th configured import directory.
    Import(u16),
    /// A script built into papyrus_analyzer (`PapyrusAnalyzer`, for tests):
    /// seen only by the files of the test directories
    /// ([`Workspace::set_test_dirs`]), and after every other script of the
    /// same name.
    Builtin,
}

/// The contents of a file opened in the editor.
#[derive(Clone, Debug)]
pub struct OpenDoc {
    /// The current text.
    pub text: Arc<str>,
    /// The editor version of the text.
    pub version: i32,
    /// The syntax tree of `text`.
    pub tree: Arc<SyntaxTree>,
    /// The syntax errors of `text`.
    pub errors: Arc<[SyntaxError]>,
    /// Line index of `text`.
    pub line_index: Arc<LineIndex>,
}

impl OpenDoc {
    /// Parses `text`.
    pub fn new(text: Arc<str>, version: i32) -> (OpenDoc, ScriptIndex) {
        let parse = papyrus_parser::parse(&text);
        let index = ScriptIndex::build(parse.source_file());
        let (tree, errors) = parse.into_parts();
        let line_index = Arc::new(LineIndex::new(&text));
        let doc =
            OpenDoc { text, version, tree: Arc::new(tree), errors: errors.into(), line_index };
        (doc, index)
    }
}

/// A known file.
#[derive(Clone, Debug)]
pub struct FileData {
    /// Absolute path.
    pub path: PathBuf,
    /// Declarations of the file.
    pub index: Arc<ScriptIndex>,
    /// Editor contents, when open.
    pub open: Option<OpenDoc>,
    /// Where the file comes from.
    pub origin: Origin,
    /// The [`Scope`] the file belongs to, if any.
    scope: Option<u8>,
    /// The scopes seeing the file, as bits.
    visible_in: u64,
}

/// A project of a multi-project workspace, for name resolution: like for the
/// compiler, its scripts see only the scripts below its `visible`
/// directories. A file belongs to the scope with the innermost source
/// directory containing it; files of no scope see every script.
#[derive(Clone, Debug, Default)]
pub struct Scope {
    /// The source directories of the project.
    pub sources: Vec<PathBuf>,
    /// The directories its scripts see: its sources, the sources of its
    /// dependencies and its imports.
    pub visible: Vec<PathBuf>,
}

/// [`Scope`] directories as [`path_key`]s ending with `/`.
#[derive(Clone, Debug, Default)]
struct ScopeKeys {
    sources: Vec<String>,
    visible: Vec<String>,
}

/// At most this many scopes restrict name resolution; files of the others
/// see every script.
const MAX_SCOPES: usize = 64;

/// All known files. Cheap to clone: indexes and trees are shared.
#[derive(Clone, Debug, Default)]
pub struct Workspace {
    files: Vec<Option<FileData>>,
    by_path: FxHashMap<String, FileId>,
    by_script: FxHashMap<Symbol, Vec<FileId>>,
    flags: FlagTable,
    scopes: Vec<ScopeKeys>,
    /// The test directories, as [`path_key`]s ending with `/`.
    test_dirs: Vec<String>,
}

/// Normalizes a path into a lookup key. Windows paths are case-insensitive.
fn path_key(path: &Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    if cfg!(windows) { s.to_lowercase() } else { s }
}

/// The [`path_key`] of a directory, ending with `/`.
fn dir_key(dir: &Path) -> String {
    let mut k = path_key(dir);
    if !k.ends_with('/') {
        k.push('/');
    }
    k
}

impl Workspace {
    /// Creates an empty workspace.
    pub fn new() -> Workspace {
        Workspace::default()
    }

    /// Looks a file up by path.
    pub fn file_id(&self, path: &Path) -> Option<FileId> {
        self.by_path.get(&path_key(path)).copied()
    }

    /// The data of a file.
    ///
    /// # Panics
    ///
    /// Panics if the file was removed.
    pub fn file(&self, id: FileId) -> &FileData {
        self.files[id.0 as usize].as_ref().expect("file was removed")
    }

    /// Iterates over all files.
    pub fn files(&self) -> impl Iterator<Item = (FileId, &FileData)> {
        self.files.iter().enumerate().filter_map(|(i, f)| f.as_ref().map(|f| (FileId(i as u32), f)))
    }

    /// Iterates over the files opened in the editor.
    pub fn open_files(&self) -> impl Iterator<Item = (FileId, &FileData)> {
        self.files().filter(|(_, f)| f.open.is_some())
    }

    /// Number of known files.
    pub fn len(&self) -> usize {
        self.by_path.len()
    }

    /// Adds or replaces the index of a file read from disk. Editor contents of
    /// an open file take precedence and are kept.
    pub fn set_disk_index(&mut self, path: PathBuf, index: ScriptIndex, origin: Origin) -> FileId {
        match self.file_id(&path) {
            Some(id) => {
                let file = self.files[id.0 as usize].as_mut().expect("file was removed");
                file.origin = file.origin.min(origin);
                if file.open.is_none() {
                    let old = std::mem::replace(&mut file.index, Arc::new(index));
                    self.reindex_script(id, old.name);
                }
                id
            }
            None => self.insert(FileData {
                path,
                index: Arc::new(index),
                open: None,
                origin,
                scope: None,
                visible_in: 0,
            }),
        }
    }

    /// Sets the editor contents of a file, adding the file if needed.
    pub fn set_open(&mut self, path: PathBuf, text: Arc<str>, version: i32) -> FileId {
        let (doc, index) = OpenDoc::new(text, version);
        match self.file_id(&path) {
            Some(id) => {
                let file = self.files[id.0 as usize].as_mut().expect("file was removed");
                file.open = Some(doc);
                let old = std::mem::replace(&mut file.index, Arc::new(index));
                self.reindex_script(id, old.name);
                id
            }
            None => self.insert(FileData {
                path,
                index: Arc::new(index),
                open: Some(doc),
                origin: Origin::Workspace,
                scope: None,
                visible_in: 0,
            }),
        }
    }

    /// Drops the editor contents of a file and re-reads it from disk. The file is
    /// forgotten when it does not exist on disk anymore.
    pub fn close(&mut self, id: FileId) {
        let path = self.file(id).path.clone();
        match crate::read_file(&path) {
            Some(text) => {
                let index = crate::index_text(&text);
                let file = self.files[id.0 as usize].as_mut().expect("file was removed");
                file.open = None;
                let old = std::mem::replace(&mut file.index, Arc::new(index));
                self.reindex_script(id, old.name);
            }
            None => self.remove(id),
        }
    }

    /// Forgets a file.
    pub fn remove(&mut self, id: FileId) {
        let Some(file) = self.files[id.0 as usize].take() else { return };
        self.by_path.remove(&path_key(&file.path));
        if let Some(name) = file.index.name
            && let Some(ids) = self.by_script.get_mut(&name.key())
        {
            ids.retain(|&i| i != id);
        }
    }

    /// Sets the projects of a multi-project workspace; empty for one project,
    /// whose scripts see every script.
    pub fn set_scopes(&mut self, scopes: &[Scope]) {
        self.scopes = scopes
            .iter()
            .map(|s| ScopeKeys {
                sources: s.sources.iter().map(|d| dir_key(d)).collect(),
                visible: s.visible.iter().map(|d| dir_key(d)).collect(),
            })
            .collect();
        let scopings: Vec<_> =
            self.files.iter().map(|f| f.as_ref().map(|f| self.scoping(&f.path))).collect();
        for (file, scoping) in self.files.iter_mut().zip(scopings) {
            if let (Some(file), Some(scoping)) = (file, scoping) {
                (file.scope, file.visible_in) = scoping;
            }
        }
    }

    /// Sets the test directories, whose files see the [`Origin::Builtin`]
    /// scripts.
    pub fn set_test_dirs(&mut self, dirs: &[PathBuf]) {
        self.test_dirs = dirs.iter().map(|d| dir_key(d)).collect();
    }

    /// Whether `file` is below a test directory.
    pub fn is_test_file(&self, file: FileId) -> bool {
        let key = path_key(&self.file(file).path);
        self.test_dirs.iter().any(|d| key.starts_with(d.as_str()))
    }

    /// Whether the script of `target` may be used by `from`: built-in
    /// scripts only by test files.
    pub fn is_visible(&self, target: FileId, from: FileId) -> bool {
        self.file(target).origin != Origin::Builtin || self.is_test_file(from)
    }

    /// The scope of a file and the scopes seeing it.
    fn scoping(&self, path: &Path) -> (Option<u8>, u64) {
        if self.scopes.is_empty() {
            return (None, 0);
        }
        let key = path_key(path);
        let scopes = self.scopes.iter().take(MAX_SCOPES).enumerate();
        let scope = scopes
            .clone()
            .filter_map(|(i, s)| {
                let depth = s
                    .sources
                    .iter()
                    .filter(|d| key.starts_with(d.as_str()))
                    .map(String::len)
                    .max()?;
                Some((depth, i as u8))
            })
            .max_by_key(|(depth, _)| *depth)
            .map(|(_, i)| i);
        let visible_in = scopes
            .filter(|(_, s)| s.visible.iter().any(|d| key.starts_with(d.as_str())))
            .fold(0, |bits, (i, _)| bits | 1 << i);
        (scope, visible_in)
    }

    fn insert(&mut self, mut data: FileData) -> FileId {
        (data.scope, data.visible_in) = self.scoping(&data.path);
        let id = FileId(self.files.len() as u32);
        self.by_path.insert(path_key(&data.path), id);
        if let Some(name) = data.index.name {
            self.by_script.entry(name.key()).or_default().push(id);
        }
        self.files.push(Some(data));
        id
    }

    fn reindex_script(&mut self, id: FileId, old: Option<Name>) {
        let new = self.file(id).index.name;
        if old.map(|n| n.key()) == new.map(|n| n.key()) {
            return;
        }
        if let Some(old) = old
            && let Some(ids) = self.by_script.get_mut(&old.key())
        {
            ids.retain(|&i| i != id);
        }
        if let Some(new) = new {
            self.by_script.entry(new.key()).or_default().push(id);
        }
    }

    /// The file declaring script `name`.
    ///
    /// When several files declare the same script, open files win, then files
    /// whose name matches the script name, then the origin order.
    pub fn script(&self, name: Name) -> Option<FileId> {
        self.pick_script(name, None)
    }

    /// Like [`Workspace::script`], as seen from file `near`: a file always sees
    /// itself under its own script name, and among duplicates the file sharing
    /// the longest directory prefix with `near` wins after open files and origin.
    pub fn script_near(&self, name: Name, near: FileId) -> Option<FileId> {
        if self.file(near).index.name == Some(name) {
            return Some(near);
        }
        self.pick_script(name, Some(near))
    }

    fn pick_script(&self, name: Name, near: Option<FileId>) -> Option<FileId> {
        let ids = self.by_script.get(&name.key())?;
        // A file of a scope sees only the scripts visible from its scope.
        let scope = near.and_then(|n| self.file(n).scope);
        let visible = |id: FileId| {
            // Built-in scripts are in no directory: `is_visible` tells.
            let in_scope = self.file(id).origin == Origin::Builtin
                || scope.is_none_or(|s| self.file(id).visible_in & 1 << s != 0);
            in_scope && near.is_none_or(|n| self.is_visible(id, n))
        };
        if let [only] = ids.as_slice() {
            return Some(*only).filter(|&id| visible(id));
        }
        let near_dir = near.and_then(|n| self.file(n).path.parent().map(Path::to_path_buf));
        ids.iter().copied().filter(|&id| visible(id)).min_by_key(|&id| {
            let f = self.file(id);
            let stem_matches = f
                .path
                .file_stem()
                .is_some_and(|s| s.to_string_lossy().eq_ignore_ascii_case(name.as_str()));
            let shared = near_dir.as_deref().map_or(0, |dir| {
                dir.components().zip(f.path.components()).take_while(|(a, b)| a == b).count()
            });
            (f.open.is_none(), !stem_matches, f.origin, std::cmp::Reverse(shared), id)
        })
    }

    /// All files declaring script `name`.
    pub fn scripts_named(&self, name: Name) -> &[FileId] {
        self.by_script.get(&name.key()).map_or(&[], |v| v)
    }

    /// Iterates over every distinct script name, with its best file.
    pub fn all_scripts(&self) -> impl Iterator<Item = (Name, FileId)> + '_ {
        self.by_script.keys().filter_map(|&key| {
            let name = Name::new(key.as_str());
            let id = self.script(name)?;
            Some((self.file(id).index.name.unwrap_or(name), id))
        })
    }

    /// The user flags (`Hidden`, `Conditional`, ...) of the flags file.
    pub fn flags(&self) -> &FlagTable {
        &self.flags
    }

    /// Sets the flags file read from disk. Ignored while the current flags file
    /// is open in the editor, unless `path` is another file.
    pub fn set_flags_file(&mut self, path: PathBuf, text: Arc<str>) {
        if self.flags.open
            && self.flags.path.as_deref().is_some_and(|p| path_key(p) == path_key(&path))
        {
            return;
        }
        self.flags = FlagTable::parse(Some(path), text);
    }

    /// Sets the editor contents of the flags file.
    pub fn set_flags_open(&mut self, path: PathBuf, text: Arc<str>) {
        self.flags = FlagTable::parse(Some(path), text);
        self.flags.open = true;
    }

    /// Marks the flags file as closed, re-reading it from disk.
    pub fn close_flags(&mut self) {
        let Some(path) = self.flags.path.clone() else { return };
        self.flags = match crate::read_file(&path) {
            Some(text) => FlagTable::parse(Some(path), Arc::from(text)),
            None => FlagTable::default(),
        };
    }

    /// Returns `true` if `path` is the current flags file.
    pub fn is_flags_file(&self, path: &Path) -> bool {
        self.flags.path.as_deref().is_some_and(|p| path_key(p) == path_key(path))
    }

    /// The text of a file: the editor contents, or the file read from disk.
    pub fn text(&self, id: FileId) -> Option<Arc<str>> {
        let file = self.file(id);
        match &file.open {
            Some(doc) => Some(doc.text.clone()),
            None => crate::read_file(&file.path).map(Arc::from),
        }
    }
}
