//! Workspaces of several projects, like Cargo workspaces.
//!
//! ```toml
//! # papyrus.toml at the root
//! [workspace]
//! members = ["Requiem", "mods/*"]   # directories with a papyrus.toml; `*` = every subdirectory
//! exclude = ["mods/old"]
//!
//! [workspace.var]                   # variables of every member
//! [workspace.compiler]              # defaults of `[compiler]`
//! [workspace.build]                 # defaults of `[build]`: imports (added after the member's), flags, ...
//! [workspace.lints]                 # applied before the `[lints]` of the member
//! [workspace.format]                # overridden by the `[format]` of the member
//! ```
//!
//! A member lists the projects it uses in `[dependencies]`, by path:
//!
//! ```toml
//! [dependencies]
//! MCMHelper = { path = "../MCMHelper" }
//! ```
//!
//! Their sources (and theirs, transitively) are imported after the member's
//! own sources, before its imports: like for the compiler, a member sees its
//! sources, its dependencies and its imports, nothing else.
//!
//! `members`, `exclude` and every path of the workspace may use the
//! variables of the root: `[var]`, `[workspace.var]` and its
//! `papyrus.local.toml`.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use serde::Deserialize;
use toml::Spanned;

use crate::{
    FormatSettings, MANIFEST_NAME, ManifestError, Project, RawCompiler, RawFormat, RawLocal,
    Variables, format_settings, lint_settings_into, resolve_path,
};

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub(crate) struct RawWorkspace {
    #[serde(default)]
    members: Vec<Spanned<String>>,
    #[serde(default)]
    exclude: Vec<Spanned<String>>,
    #[serde(default)]
    var: BTreeMap<String, String>,
    #[serde(default)]
    compiler: RawCompiler,
    #[serde(default)]
    build: RawWorkspaceBuild,
    #[serde(default)]
    pub(crate) lints: BTreeMap<String, Spanned<toml::Value>>,
    #[serde(default)]
    pub(crate) format: RawFormat,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct RawWorkspaceBuild {
    #[serde(default)]
    imports: Vec<Spanned<String>>,
    flags: Option<Spanned<String>>,
    optimize: Option<bool>,
    release: Option<bool>,
    r#final: Option<bool>,
    quiet: Option<bool>,
}

/// `name = { path = "..." }` of `[dependencies]`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawDependency {
    pub(crate) path: Spanned<String>,
}

/// What members inherit from their workspace, resolved.
#[derive(Clone, Debug, Default)]
pub(crate) struct Inherited {
    /// The workspace manifest.
    pub(crate) manifest: PathBuf,
    /// `[workspace.var]` and `[var]` of the root, unexpanded.
    pub(crate) var: BTreeMap<String, String>,
    /// `[var]` of the `papyrus.local.toml` of the root.
    pub(crate) local: BTreeMap<String, String>,
    pub(crate) compiler: Option<PathBuf>,
    pub(crate) args: Vec<String>,
    pub(crate) imports: Vec<PathBuf>,
    pub(crate) flags: Option<PathBuf>,
    pub(crate) optimize: Option<bool>,
    pub(crate) release: Option<bool>,
    pub(crate) r#final: Option<bool>,
    pub(crate) quiet: Option<bool>,
    pub(crate) lints: papyrus_lint::LintConfig,
    pub(crate) format: FormatSettings,
}

/// The projects of a workspace manifest, or the one project of a manifest
/// without `[workspace]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectWorkspace {
    /// The root manifest.
    pub manifest_path: PathBuf,
    /// The projects, the root first when it is a project too.
    pub members: Vec<Project>,
}

impl ProjectWorkspace {
    /// The member whose sources contain `path`, the innermost one.
    pub fn member_for(&self, path: &Path) -> Option<&Project> {
        self.members
            .iter()
            .filter_map(|p| {
                let depth = p
                    .sources
                    .iter()
                    .filter(|s| path.starts_with(s))
                    .map(|s| s.components().count())
                    .max()?;
                Some((depth, p))
            })
            .max_by_key(|(depth, _)| *depth)
            .map(|(_, p)| p)
    }
}

/// Loads the workspace of `manifest_path`: the workspace it is the root of,
/// or the one of the ancestors listing it as a member, else the project
/// alone.
pub fn load_workspace(
    manifest_path: &Path,
    vars: &Variables,
) -> Result<ProjectWorkspace, Vec<ManifestError>> {
    load_workspace_with(manifest_path, vars, &|p| std::fs::read_to_string(p).ok())
}

/// Like [`load_workspace`], reading the manifests of the workspace and their
/// `papyrus.local.toml` with `read_file` (e.g. the text of an editor);
/// dependencies outside of the workspace are read from disk.
pub fn load_workspace_with(
    manifest_path: &Path,
    vars: &Variables,
    read_file: &dyn Fn(&Path) -> Option<String>,
) -> Result<ProjectWorkspace, Vec<ManifestError>> {
    let read = |path: &Path| {
        read_file(path).ok_or_else(|| {
            vec![ManifestError::new(format!("cannot read {}", path.display()), None).in_file(path)]
        })
    };
    let read_local = |path: &Path| read_file(&path.with_file_name(crate::LOCAL_MANIFEST_NAME));
    let root =
        find_workspace_root(manifest_path, vars).unwrap_or_else(|| manifest_path.to_path_buf());
    let text = read(&root)?;
    let raw: crate::RawManifest = toml::from_str(&text)
        .map_err(|e| vec![ManifestError::new(e.message().to_owned(), e.span()).in_file(&root)])?;
    let Some(ws) = raw.workspace else {
        let mut project = crate::parse(&root, &text, read_local(&root).as_deref(), vars)?;
        resolve_dependencies(std::slice::from_mut(&mut project), vars)?;
        return Ok(ProjectWorkspace { manifest_path: root, members: vec![project] });
    };

    let dir = root.parent().map(Path::to_path_buf).unwrap_or_default();
    let inherited = inherited(&root, &dir, &ws, &raw.var, read_local(&root), vars)?;
    let mut members = Vec::new();
    // The root is a project too when it says so.
    let table: toml::Table = text.parse().unwrap_or_default();
    if table.contains_key("package") || table.contains_key("build") {
        members.push(crate::parse_with(
            &root,
            &text,
            read_local(&root).as_deref(),
            vars,
            Some(&inherited),
        )?);
    }
    let ws_vars = inherited.vars(vars);
    let mut errors = Vec::new();
    let mut expand = |value: &Spanned<String>| {
        let span = Some(value.span()).filter(|r| !r.is_empty());
        ws_vars
            .expand(value.get_ref(), &mut BTreeMap::new())
            .map_err(|message| errors.push(ManifestError::new(message, span).in_file(&root)))
            .ok()
    };
    let excluded: Vec<PathBuf> =
        ws.exclude.iter().filter_map(&mut expand).map(|e| resolve_path(&dir, &e)).collect();
    let patterns: Vec<_> = ws.members.iter().filter_map(|p| Some((p, expand(p)?))).collect();
    for (pattern, expanded) in patterns {
        let span = Some(pattern.span()).filter(|r| !r.is_empty());
        let dirs = expand_member(&dir, &expanded);
        if dirs.is_empty() {
            errors.push(
                ManifestError::new(format!("no member matches `{expanded}`"), span).in_file(&root),
            );
        }
        for member in dirs.into_iter().filter(|d| !excluded.contains(d)) {
            let path = member.join(MANIFEST_NAME);
            match read(&path).and_then(|text| {
                crate::parse_with(
                    &path,
                    &text,
                    read_local(&path).as_deref(),
                    vars,
                    Some(&inherited),
                )
                .map_err(|errs| errs.into_iter().map(|e| e.in_file(&path)).collect())
            }) {
                Ok(project) => members.push(project),
                Err(errs) => errors.extend(errs),
            }
        }
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    resolve_dependencies(&mut members, vars)?;
    Ok(ProjectWorkspace { manifest_path: root, members })
}

/// The workspace settings, expanded with the variables of the root.
fn inherited(
    root: &Path,
    dir: &Path,
    ws: &RawWorkspace,
    root_var: &BTreeMap<String, String>,
    root_local: Option<String>,
    vars: &Variables,
) -> Result<Inherited, Vec<ManifestError>> {
    let mut var = root_var.clone();
    var.extend(ws.var.clone());
    let local = match root_local {
        Some(text) => {
            toml::from_str::<RawLocal>(&text)
                .map_err(|e| {
                    vec![
                        ManifestError::new(e.message().to_owned(), None)
                            .in_file(&root.with_file_name(crate::LOCAL_MANIFEST_NAME)),
                    ]
                })?
                .var
        }
        None => BTreeMap::new(),
    };
    let mut vars = vars.clone();
    vars.manifest = var.clone();
    vars.local = local.clone();
    let mut errors = Vec::new();
    let args = ws
        .compiler
        .args
        .iter()
        .map(|a| {
            vars.expand(a, &mut BTreeMap::new()).unwrap_or_else(|message| {
                errors.push(ManifestError::new(message, None));
                String::new()
            })
        })
        .collect();
    let mut path = |value: &Spanned<String>| {
        let span = Some(value.span()).filter(|r| !r.is_empty());
        match vars.expand(value.get_ref(), &mut BTreeMap::new()) {
            Ok(v) => Some(resolve_path(dir, &v)),
            Err(message) => {
                errors.push(ManifestError::new(message, span).in_file(root));
                None
            }
        }
    };
    let compiler = ws.compiler.path.as_ref().and_then(&mut path);
    let imports: Vec<PathBuf> = ws.build.imports.iter().filter_map(&mut path).collect();
    let flags = ws.build.flags.as_ref().and_then(&mut path);
    let mut lints = papyrus_lint::LintConfig::default();
    lint_settings_into(&mut lints, &ws.lints, &mut errors);
    let format = format_settings(&ws.format, &mut errors);
    if !errors.is_empty() {
        return Err(errors.into_iter().map(|e| e.in_file(root)).collect());
    }
    Ok(Inherited {
        manifest: root.to_path_buf(),
        var,
        local,
        compiler,
        args,
        imports,
        flags,
        optimize: ws.build.optimize,
        release: ws.build.release,
        r#final: ws.build.r#final,
        quiet: ws.build.quiet,
        lints,
        format,
    })
}

impl Inherited {
    /// The variables of the workspace settings: `vars` with `[var]` and
    /// `[workspace.var]` of the root and its `papyrus.local.toml`.
    fn vars(&self, vars: &Variables) -> Variables {
        let mut vars = vars.clone();
        vars.manifest = self.var.clone();
        vars.local = self.local.clone();
        vars
    }
}

/// The member directories of a pattern, with variables expanded: a path, or
/// a path ending with `*` for every subdirectory with a manifest.
fn expand_member(dir: &Path, pattern: &str) -> Vec<PathBuf> {
    let pattern = pattern.trim_end_matches(['/', '\\']);
    match pattern.strip_suffix('*') {
        Some(parent) => {
            let parent = resolve_path(dir, parent.trim_end_matches(['/', '\\']));
            let Ok(entries) = std::fs::read_dir(&parent) else { return Vec::new() };
            let mut dirs: Vec<PathBuf> = entries
                .flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                .map(|e| e.path())
                .filter(|p| p.join(MANIFEST_NAME).is_file())
                .collect();
            dirs.sort();
            dirs
        }
        None => {
            let member = resolve_path(dir, pattern);
            if member.join(MANIFEST_NAME).is_file() { vec![member] } else { Vec::new() }
        }
    }
}

/// The manifest of the workspace listing `manifest_path` as a member: the
/// nearest ancestor manifest with `[workspace]` whose members include it.
/// `vars` are the editor and process variables, for the member patterns.
pub fn find_workspace_root(manifest_path: &Path, vars: &Variables) -> Option<PathBuf> {
    let member = manifest_path.parent()?;
    let text = std::fs::read_to_string(manifest_path).ok()?;
    if text.parse::<toml::Table>().is_ok_and(|t| t.contains_key("workspace")) {
        return Some(manifest_path.to_path_buf());
    }
    for dir in member.ancestors().skip(1) {
        let candidate = dir.join(MANIFEST_NAME);
        let Ok(text) = std::fs::read_to_string(&candidate) else { continue };
        let Ok(raw) = toml::from_str::<crate::RawManifest>(&text) else { continue };
        let Some(ws) = raw.workspace else { continue };
        let mut var = raw.var;
        var.extend(ws.var);
        let local = crate::read_local(&candidate)
            .and_then(|t| toml::from_str::<RawLocal>(&t).ok())
            .map(|l| l.var)
            .unwrap_or_default();
        let vars = Inherited { var, local, ..Default::default() }.vars(vars);
        let members = ws
            .members
            .iter()
            .filter_map(|m| vars.expand(m.get_ref(), &mut BTreeMap::new()).ok())
            .flat_map(|m| expand_member(dir, &m));
        let excluded = ws
            .exclude
            .iter()
            .filter_map(|e| vars.expand(e.get_ref(), &mut BTreeMap::new()).ok())
            .any(|e| resolve_path(dir, &e) == member);
        if !excluded && members.into_iter().any(|m| m == member) {
            return Some(candidate);
        }
    }
    None
}

/// The manifest and the `[workspace]` table of the workspace of `manifest_path`,
/// for what needs no variables: lint levels and formatter settings.
pub(crate) fn workspace_of(manifest_path: &Path) -> Option<(PathBuf, RawWorkspace)> {
    let dir = manifest_path.parent()?;
    let root = find_workspace_root(manifest_path, &Variables::for_command_line(dir))?;
    let text = std::fs::read_to_string(&root).ok()?;
    let raw: crate::RawManifest = toml::from_str(&text).ok()?;
    Some((root, raw.workspace?))
}

/// Fills `dependency_sources` from `[dependencies]`: the sources of the
/// dependencies, transitively. A dependency is a member of the workspace, or
/// any directory with a manifest.
fn resolve_dependencies(
    members: &mut [Project],
    vars: &Variables,
) -> Result<(), Vec<ManifestError>> {
    // Sources and dependencies of every project seen, by manifest directory.
    let mut known: BTreeMap<PathBuf, (Vec<PathBuf>, Vec<PathBuf>)> = members
        .iter()
        .map(|p| {
            (
                p.root.clone(),
                (p.sources.clone(), p.dependencies.iter().map(|d| d.1.clone()).collect()),
            )
        })
        .collect();
    let mut errors = Vec::new();
    for member in members.iter_mut() {
        let mut sources = Vec::new();
        let mut stack: Vec<PathBuf> = member.dependencies.iter().map(|d| d.1.clone()).collect();
        let mut seen = vec![member.root.clone()];
        while let Some(dep) = stack.pop() {
            if seen.contains(&dep) {
                continue;
            }
            seen.push(dep.clone());
            if !known.contains_key(&dep) {
                // Not a member: its manifest alone.
                match crate::load(&dep.join(MANIFEST_NAME), vars) {
                    Ok(p) => {
                        let deps = p.dependencies.iter().map(|d| d.1.clone()).collect();
                        known.insert(dep.clone(), (p.sources, deps));
                    }
                    Err(_) => {
                        errors.push(
                            ManifestError::new(
                                format!(
                                    "dependency not found: {}",
                                    dep.join(MANIFEST_NAME).display()
                                ),
                                None,
                            )
                            .in_file(&member.manifest_path),
                        );
                        continue;
                    }
                }
            }
            let (dep_sources, dep_deps) = &known[&dep];
            sources.extend(
                dep_sources.iter().filter(|s| !sources.contains(*s)).cloned().collect::<Vec<_>>(),
            );
            stack.extend(dep_deps.iter().cloned());
        }
        member.dependency_sources = sources;
    }
    if errors.is_empty() { Ok(()) } else { Err(errors) }
}
