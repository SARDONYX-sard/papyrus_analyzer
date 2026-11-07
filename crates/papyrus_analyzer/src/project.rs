//! `papyrus.toml` and the flags file.
//!
//! Each workspace folder is on its own. One with a manifest (searching its
//! ancestors too) has its projects: the import directories, the flags file and
//! the compiler invocation come from there. A manifest with `[workspace]`, or a
//! member of one, loads every member: each script then sees, compiles and is
//! linted like in its own project. One without is limited: its scripts see the
//! folder and the game scripts, with the default settings. Scripts of neither
//! are known only while open, with the default settings. Problems are published
//! as diagnostics on each manifest, live while it is edited; the projects are
//! reloaded, and the workspace re-indexed when needed, once a manifest is saved.
//!
//! The flags file of a script is, in order of preference: `[build] flags` of its
//! project, the `papyrus.flagsFile` setting, a `TESV_Papyrus_Flags.flg` found
//! while indexing, any other `.flg` found while indexing, or the built-in Skyrim
//! flags.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use lsp_types::{NumberOrString, PublishDiagnosticsParams, notification};
use papyrus_ide::{LineIndex, TextRange, TextSize};
use papyrus_project::{ManifestError, Project, Variables};

use crate::{
    conv::{self, Lines},
    ext::{
        BuildDirs, CompileCommand, CompileCommandsParams, CompileCommandsResult,
        DecompileFileResult, DecompileFilesParams, DecompileFilesResult, DecompileParams,
    },
    server::{Server, is_flags, is_manifest},
};

impl Server {
    /// The variables of this machine, with the editor values on top.
    pub(crate) fn vars(&self) -> Variables {
        Variables::new(self.config.var.clone()).with_workspace_folders(self.roots.clone())
    }

    /// Expands the variables of a path setting, relative to the first
    /// workspace folder.
    fn expand_setting(&self, value: &Path) -> Option<PathBuf> {
        let text = value.to_string_lossy();
        match self.vars().expand(&text, &mut BTreeMap::new()) {
            Ok(expanded) => {
                let root = self.roots.first().cloned().unwrap_or_default();
                Some(papyrus_project::resolve_path(&root, &expanded))
            }
            Err(err) => {
                log::warn!("setting `{text}`: {err}");
                None
            }
        }
    }

    /// Whether a workspace folder works without manifest, or every one: its
    /// scripts need the game scripts.
    fn has_limited_folder(&self) -> bool {
        self.members.is_empty() || !self.limited_roots.is_empty()
    }

    /// The directories of the configured settings, and of the game scripts of
    /// `SKYRIM_DIR` when a folder is limited, so that it resolves vanilla scripts
    /// with no configuration at all.
    fn limited_imports(&self) -> Vec<PathBuf> {
        let configured =
            self.config.import_directories.iter().filter_map(|d| self.expand_setting(d));
        let game = match self.has_limited_folder() {
            true => self
                .skyrim_dir()
                .map(|(dir, _)| papyrus_project::game_script_dirs(&dir))
                .unwrap_or_default(),
            false => Vec::new(),
        };
        configured.chain(game).collect()
    }

    /// Import directories: those of the projects, then the configured ones,
    /// then the game scripts for the limited folders.
    pub(crate) fn import_dirs(&self) -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        for dir in self.members.iter().flat_map(Project::import_dirs).chain(self.limited_imports())
        {
            if !dirs.contains(&dir) {
                dirs.push(dir);
            }
        }
        dirs
    }

    /// The scripts each project sees, like the compiler: its sources, its
    /// dependencies, its imports and the configured import directories. Empty
    /// without project (limited mode), where scripts see every script.
    pub(crate) fn scopes(&self) -> Vec<papyrus_ide::Scope> {
        if self.members.is_empty() {
            return Vec::new();
        }
        let configured: Vec<PathBuf> =
            self.config.import_directories.iter().filter_map(|d| self.expand_setting(d)).collect();
        let mut scopes = project_scopes(&self.members, &configured);
        let limited_imports = self.limited_imports();
        scopes.extend(self.limited_roots.iter().map(|root| papyrus_ide::Scope {
            sources: vec![root.clone()],
            visible: std::iter::once(root.clone()).chain(limited_imports.iter().cloned()).collect(),
            flags: None,
        }));
        scopes
    }

    /// Whether the server may change `path` (format, rename, code actions):
    /// a file of the workspace folders, outside of the import directories.
    /// Like rust-analyzer with libraries, imported scripts (the game sources
    /// above all) are never changed, even when they are open.
    pub(crate) fn is_editable(&self, path: &Path) -> bool {
        // Windows paths ignore case, and drives come as `d:` or `D:`.
        let norm = |p: &Path| {
            let s = p.to_string_lossy().replace('\\', "/").to_ascii_lowercase();
            if s.ends_with('/') { s } else { format!("{s}/") }
        };
        let file = norm(path);
        let below = |dir: &Path| file.starts_with(&norm(dir));
        let configured =
            self.config.import_directories.iter().filter_map(|d| self.expand_setting(d));
        let game = match self.has_limited_folder() {
            true => self.skyrim_dir().map(|(d, _)| papyrus_project::game_script_dirs(&d)),
            false => None,
        };
        let mut libraries = self
            .members
            .iter()
            .flat_map(|m| m.imports.iter().cloned())
            .chain(configured)
            .chain(game.into_iter().flatten());
        self.roots.iter().any(|r| below(r)) && !libraries.any(|d| below(&d))
    }

    /// The project whose sources contain `path`, the innermost one.
    pub(crate) fn member_for(&self, path: &Path) -> Option<&Project> {
        innermost(&self.members, path)
    }

    /// The flags file setting, with variables expanded.
    pub(crate) fn flags_setting(&self) -> Option<PathBuf> {
        self.config.flags_file.as_deref().and_then(|f| self.expand_setting(f))
    }

    /// Known variables and their origin, for `papyrus/status`.
    pub(crate) fn variables_report(&self) -> BTreeMap<String, String> {
        let mut out: BTreeMap<String, String> = self
            .project
            .iter()
            .flat_map(|p| &p.variables)
            .map(|(k, (v, src))| (k.clone(), format!("{v} ({src})")))
            .collect();
        if self.project.is_none()
            && let Some((dir, src)) = self.skyrim_dir()
        {
            out.entry(papyrus_project::SKYRIM_DIR.to_owned())
                .or_insert(format!("{} ({src})", dir.display()));
        }
        out
    }

    /// `SKYRIM_DIR` for a workspace without manifest: the variable if set,
    /// else the detected installation.
    fn skyrim_dir(&self) -> Option<(PathBuf, String)> {
        if let Some((dir, src)) = self.vars().lookup(papyrus_project::SKYRIM_DIR) {
            return Some((PathBuf::from(dir), src.to_string()));
        }
        papyrus_project::detect_skyrim_dir().map(|d| (d, "detected".to_owned()))
    }

    /// The overall state shown by the client, with the problems: `error`,
    /// `warning`, `limited` (no `papyrus.toml`: scripts compile with the
    /// built-in compiler and the editor settings only) or `ok`.
    pub(crate) fn health(&self) -> (&'static str, Vec<String>) {
        let mut errors = Vec::new();
        let mut warnings = Vec::new();
        if let Some(error) = &self.manifest_error {
            errors.push(format!("papyrus.toml: {error}"));
        }
        for project in &self.members {
            let manifest = match self.members.len() {
                1 => papyrus_project::MANIFEST_NAME.to_owned(),
                _ => project.manifest_path.display().to_string(),
            };
            warnings.extend(project.warnings.iter().map(|w| format!("{manifest}: {}", w.message)));
        }
        for dir in &self.config.import_directories {
            let text = dir.to_string_lossy();
            if let Err(err) = self.vars().expand(&text, &mut BTreeMap::new()) {
                errors.push(format!("papyrus.importDirectories `{text}`: {err}"));
            }
        }
        for (_, flags) in self.ws.flag_tables().filter(|(_, t)| !t.errors.is_empty()) {
            warnings.push(format!("{}: {} syntax errors", flags.source_name(), flags.errors.len()));
        }
        let health = if !errors.is_empty() {
            "error"
        } else if !warnings.is_empty() {
            "warning"
        } else if self.manifest.is_none() {
            "limited"
        } else {
            "ok"
        };
        errors.extend(warnings);
        (health, errors)
    }

    /// Hover on a `${NAME}` reference or a `[var]` key of `papyrus.toml` or
    /// `papyrus.local.toml`: the resolved value and where it comes from.
    pub(crate) fn manifest_hover(
        &self,
        path: &Path,
        offset: TextSize,
    ) -> Option<(TextRange, String)> {
        let text = self.doc_or_disk(path)?;
        let (name, range) = papyrus_project::variable_at(&text, usize::from(offset))?;
        let manifest = path.with_file_name(papyrus_project::MANIFEST_NAME);
        let local_path = path.with_file_name(papyrus_project::LOCAL_MANIFEST_NAME);
        let (manifest_text, local_text) = if *path == manifest {
            (text.clone(), self.doc_or_disk(&local_path))
        } else {
            (self.doc_or_disk(&manifest).unwrap_or_default(), Some(text.clone()))
        };
        let vars = papyrus_project::manifest_variables(
            &manifest,
            &manifest_text,
            local_text.as_deref(),
            &self.vars(),
        );
        let markdown = match vars.lookup(&name) {
            Some((value, source)) => format!("`${{{name}}}` = `{value}`\n\n*from {source}*"),
            None => match vars.expand(&format!("${{{name}}}"), &mut BTreeMap::new()) {
                Err(err) => format!("`${{{name}}}`: {err}"),
                Ok(value) => format!("`${{{name}}}` = `{value}`"),
            },
        };
        let range =
            TextRange::new(TextSize::new(range.start as u32), TextSize::new(range.end as u32));
        Some((range, markdown))
    }

    fn doc_or_disk(&self, path: &Path) -> Option<String> {
        match self.other_docs.get(path) {
            Some(doc) => Some(doc.text.clone()),
            None => std::fs::read_to_string(path).ok(),
        }
    }

    /// The manifests of the workspace folders: of their workspace when in one;
    /// and the workspace folders without any.
    fn manifests_of_folders(&self) -> (Vec<PathBuf>, Vec<PathBuf>) {
        let vars = self.vars();
        let (mut manifests, mut limited) = (Vec::new(), Vec::new());
        for root in &self.roots {
            match papyrus_project::find_manifest(root) {
                Some(found) => {
                    // A member of a workspace loads the whole workspace.
                    let manifest =
                        papyrus_project::find_workspace_root(&found, &vars).unwrap_or(found);
                    if !manifests.contains(&manifest) {
                        manifests.push(manifest);
                    }
                }
                None => limited.push(root.clone()),
            }
        }
        (manifests, limited)
    }

    /// Loads `manifests`, from the editor when open.
    fn load_workspaces(&self, manifests: &[PathBuf]) -> Vec<LoadedWorkspace> {
        let vars = self.vars();
        let read = |p: &Path| self.doc_or_disk(p);
        let load = |m: &PathBuf| (m.clone(), papyrus_project::load_workspace_with(m, &vars, &read));
        manifests.iter().map(load).collect()
    }

    /// Loads the manifests and the members of their `[workspace]`, from the
    /// editor when open, and publishes their problems.
    pub(crate) fn load_project(&mut self) {
        let (manifests, limited) = self.manifests_of_folders();
        self.limited_roots = limited;
        let first = manifests.first().cloned();
        if first != self.manifest {
            self.manifest = first;
            self.refresh_code_lenses();
        }
        self.manifest_error = None;
        let loaded = self.load_workspaces(&manifests);
        self.publish_manifest_problems(&loaded);
        let mut members: Vec<Project> = Vec::new();
        for (path, result) in loaded {
            match result {
                Ok(ws) => {
                    log::info!("loaded {} ({} projects)", path.display(), ws.members.len());
                    members.extend(ws.members);
                }
                Err(errors) => {
                    let first = &errors[0];
                    log::warn!("invalid {}: {first}", path.display());
                    if self.manifest_error.is_none() {
                        self.manifest_error = Some(match &first.file {
                            Some(file) if *file != path => {
                                format!("{}: {}", file.display(), first.message)
                            }
                            _ => first.message.clone(),
                        });
                    }
                    // Keep the previous projects rather than losing all imports.
                    let previous = self.members.iter().filter(|m| {
                        m.manifest_path == path || m.workspace_manifest.as_ref() == Some(&path)
                    });
                    members.extend(previous.cloned());
                }
            }
        }
        // A project of several workspace folders counts once.
        let mut seen: Vec<PathBuf> = Vec::new();
        members.retain(|m| {
            let new = !seen.contains(&m.manifest_path);
            seen.push(m.manifest_path.clone());
            new
        });
        self.project = members.first().cloned();
        self.members = members;
        let scopes = self.scopes();
        self.ws.set_scopes(&scopes);
        self.install_builtins();
        // The client shows the mode (limited or not) and the sources.
        self.notify::<crate::ext::StatusChanged>(());
    }

    /// Adds the built-in `PapyrusAnalyzer` script, seen by the test sources
    /// of the projects.
    pub(crate) fn install_builtins(&mut self) {
        let test_dirs: Vec<PathBuf> =
            self.members.iter().flat_map(|m| m.test_sources.iter().cloned()).collect();
        crate::builtin::install(&mut self.ws, &test_dirs);
    }

    /// Publishes the problems of each manifest of `loaded`, clearing those of the
    /// manifests published before.
    fn publish_manifest_problems(&mut self, loaded: &[LoadedWorkspace]) {
        // The problems of a file, and whether its workspace does not load.
        let mut problems: BTreeMap<PathBuf, (Vec<ManifestError>, bool)> =
            std::mem::take(&mut self.manifest_files)
                .into_iter()
                .map(|p| (p, (Vec::new(), false)))
                .collect();
        for (root, result) in loaded {
            match result {
                Ok(ws) => {
                    for m in &ws.members {
                        let entry = problems.entry(m.manifest_path.clone()).or_default();
                        entry.0.extend(m.warnings.clone());
                    }
                    problems.entry(root.clone()).or_default();
                }
                Err(errors) => {
                    for e in errors {
                        let file = e.file.clone().unwrap_or_else(|| root.clone());
                        let entry = problems.entry(file).or_default();
                        entry.0.push(e.clone());
                        entry.1 = true;
                    }
                    problems.entry(root.clone()).or_default().1 = true;
                }
            }
        }
        for (file, (list, fatal)) in &problems {
            let text = self.doc_or_disk(file).unwrap_or_default();
            self.publish_manifest_diagnostics(file, &text, list, *fatal);
        }
        self.manifest_files = problems.into_keys().collect();
    }

    /// Asks the client to query code lenses again, e.g. after the manifest
    /// appeared or disappeared.
    pub(crate) fn refresh_code_lenses(&mut self) {
        let supported = self
            .caps
            .workspace
            .as_ref()
            .and_then(|w| w.code_lens.as_ref())
            .and_then(|c| c.refresh_support)
            .unwrap_or(false);
        if supported {
            self.request_client::<lsp_types::request::CodeLensRefresh>(());
        }
    }

    /// Reloads the manifests and re-indexes when the directories they name or
    /// the flags changed.
    pub(crate) fn reload_project(&mut self) {
        let before = (self.index_dirs(), self.project.as_ref().and_then(|p| p.flags.clone()));
        self.load_project();
        let after = (self.index_dirs(), self.project.as_ref().and_then(|p| p.flags.clone()));
        if before != after {
            log::info!("project changed, re-indexing");
            self.reset_workspace();
            self.start_indexing();
        }
        self.publish_flags_diagnostics();
        self.refresh_diagnostics();
    }

    fn publish_manifest_diagnostics(
        &self,
        path: &Path,
        text: &str,
        problems: &[ManifestError],
        fatal: bool,
    ) {
        let index = LineIndex::new(text);
        let lines = Lines { text: Arc::from(text), index: Arc::new(index) };
        let diagnostics = problems
            .iter()
            .map(|p| {
                let range = p.span.clone().map_or(TextRange::empty(TextSize::new(0)), |r| {
                    TextRange::new(TextSize::new(r.start as u32), TextSize::new(r.end as u32))
                });
                lsp_types::Diagnostic {
                    range: lines.range(range),
                    severity: Some(if fatal {
                        lsp_types::DiagnosticSeverity::ERROR
                    } else {
                        lsp_types::DiagnosticSeverity::WARNING
                    }),
                    code: Some(NumberOrString::String("manifest".to_owned())),
                    source: Some("papyrus".to_owned()),
                    message: p.message.clone(),
                    ..Default::default()
                }
            })
            .collect();
        self.notify::<notification::PublishDiagnostics>(PublishDiagnosticsParams {
            uri: conv::path_to_uri(path),
            diagnostics,
            version: self.other_docs.get(path).map(|d| d.version),
        });
    }

    /// A `.flg` or `papyrus.toml` document changed in the editor.
    pub(crate) fn other_doc_changed(&mut self, path: &Path) {
        let Some(doc) = self.other_docs.get(path) else { return };
        if is_manifest(path) {
            // Validate live, with the settings of the workspace; the projects
            // themselves are reloaded on save.
            let manifest = path.with_file_name(papyrus_project::MANIFEST_NAME);
            let root =
                papyrus_project::find_workspace_root(&manifest, &self.vars()).unwrap_or(manifest);
            // The other manifests keep their problems.
            let (mut manifests, _) = self.manifests_of_folders();
            if !manifests.contains(&root) {
                manifests.push(root);
            }
            let loaded = self.load_workspaces(&manifests);
            self.publish_manifest_problems(&loaded);
        } else if is_flags(path) {
            // Only the active flags file (or any, without one) drives the scripts.
            if self.ws.is_flags_file(path) || self.ws.flags().path.is_none() {
                let text = Arc::from(doc.text.as_str());
                self.ws.set_flags_open(path.to_path_buf(), text);
                self.publish_flags_diagnostics();
                self.refresh_diagnostics();
            }
        }
    }

    /// Chooses and loads the flags file of the scripts of no project with one.
    /// `found` are `.flg` files seen while indexing; empty to reload the current
    /// one.
    pub(crate) fn select_flags_file(&mut self, found: Vec<PathBuf>) {
        let preferred = found
            .iter()
            .find(|p| {
                p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("TESV_Papyrus_Flags.flg"))
            })
            .or(found.first())
            .cloned();
        let chosen = self.flags_setting().or(preferred).or_else(|| self.ws.flags().path.clone());
        let Some(path) = chosen else {
            self.publish_flags_diagnostics();
            return;
        };
        if let Some(doc) = self.other_docs.get(&path) {
            let text = Arc::from(doc.text.as_str());
            self.ws.set_flags_open(path, text);
        } else if let Some(text) = papyrus_ide::read_file(&path) {
            self.ws.set_flags_file(path, Arc::from(text));
        } else {
            log::warn!("cannot read flags file {}", path.display());
            return;
        }
        log::info!(
            "flags: {} ({} flags)",
            self.ws.flags().source_name(),
            self.ws.flags().defs.len()
        );
        self.publish_flags_diagnostics();
    }

    pub(crate) fn publish_flags_diagnostics(&self) {
        for (_, flags) in self.ws.flag_tables() {
            let Some(path) = &flags.path else { continue };
            let lines = Lines::flags(flags);
            let diagnostics = papyrus_ide::flag_file_diagnostics(flags)
                .into_iter()
                .map(|d| lsp_types::Diagnostic {
                    range: lines.range(d.range),
                    severity: Some(lsp_types::DiagnosticSeverity::ERROR),
                    code: Some(NumberOrString::String(d.code.to_owned())),
                    source: Some("papyrus".to_owned()),
                    message: d.message,
                    ..Default::default()
                })
                .collect();
            self.notify::<notification::PublishDiagnostics>(PublishDiagnosticsParams {
                uri: conv::path_to_uri(path),
                diagnostics,
                version: None,
            });
        }
    }

    /// Handles `papyrus/compileCommands`.
    pub(crate) fn compile_commands(
        &mut self,
        params: CompileCommandsParams,
    ) -> Result<CompileCommandsResult, String> {
        // The manifest may have been edited since the last load.
        self.load_project();
        if let Some(error) = &self.manifest_error {
            return Err(format!("{} is invalid: {error}", papyrus_project::MANIFEST_NAME));
        }
        // Without manifest (limited mode), the scripts still compile with the
        // built-in compiler, the configured imports and the known flags file.
        let mut projects = self.members.clone();
        let flags = self.flags_setting().or_else(|| self.ws.flags().path.clone());
        if projects.is_empty() {
            let root = self.roots.first().cloned().ok_or("no workspace folder")?;
            projects.push(Project::without_manifest(root, self.import_dirs(), flags));
        } else {
            for root in &self.limited_roots {
                let (root, imports) = (root.clone(), self.limited_imports());
                projects.push(Project::without_manifest(root, imports, flags.clone()));
            }
        }
        // `papyrus_analyzer compile` takes the arguments of PapyrusCompiler.exe.
        let exe = std::env::current_exe().map_err(|e| format!("cannot find the server: {e}"))?;
        for project in &mut projects {
            project.builtin_compiler = Some((exe.clone(), vec!["compile".to_owned()]));
        }
        let mut docs = params.files;
        docs.extend(params.text_document);
        let commands = if params.all || docs.is_empty() {
            let mut commands = Vec::new();
            for project in &projects {
                commands.extend(project.compile_all()?);
            }
            commands
        } else {
            docs.iter()
                .map(|doc| {
                    let path = conv::uri_to_path(&doc.uri).ok_or("not a file")?;
                    // A script of a member compiles with the settings of the member.
                    innermost(&projects, &path).unwrap_or(&projects[0]).compile_file(&path)
                })
                .collect::<Result<Vec<_>, String>>()?
        };
        let first = &projects[0];
        Ok(CompileCommandsResult {
            manifest: self.manifest.as_ref().unwrap_or(&first.manifest_path).display().to_string(),
            output: first.output.display().to_string(),
            commands: commands
                .into_iter()
                .map(|c| CompileCommand {
                    command_line: c.to_string(),
                    program: c.program.display().to_string(),
                    args: c.args,
                    cwd: c.cwd.display().to_string(),
                    output: c.output.display().to_string(),
                })
                .collect(),
        })
    }

    /// Handles `papyrus/decompile`. For a `.psc` file, the assembly of the
    /// script compiled in memory, as the editor has it, with the settings of
    /// its project.
    pub(crate) fn decompile(&mut self, params: DecompileParams) -> Result<String, String> {
        let path = conv::uri_to_path(&params.uri).ok_or("not a file")?;
        if !path.extension().is_some_and(|e| e.eq_ignore_ascii_case("psc")) {
            return crate::decompile::decompile_file(&path, params.assembly).map(|(_, text)| text);
        }
        let file = self.ws.file_id(&path).ok_or("not a script of the workspace")?;
        let options = papyrus_compiler::CompileOptions {
            optimize: self.member_for(&path).is_some_and(|p| p.optimize),
            debug_info: true,
        };
        match papyrus_compiler::compile(&self.ws, file, &options) {
            Ok(pex) => Ok(papyrus_pex::disasm::disassemble(&pex, Default::default())),
            Err(papyrus_compiler::CompileError::Unreadable) => Err("cannot read the script".into()),
            Err(papyrus_compiler::CompileError::Diagnostics(errors)) => Err(format!(
                "the script has {} errors, the first: {}",
                errors.len(),
                errors[0].message
            )),
        }
    }

    /// The lint levels of the scripts of the projects of the manifest.
    pub(crate) fn lint_configs(&self) -> LintConfigs {
        let mut by_dir: Vec<(PathBuf, papyrus_lint::LintConfig)> = self
            .members
            .iter()
            .flat_map(|m| {
                let dirs = m.sources.iter().chain(&m.test_sources);
                dirs.map(|s| (s.clone(), m.lints.clone()))
            })
            .collect();
        by_dir.sort_by_key(|(dir, _)| std::cmp::Reverse(dir.components().count()));
        LintConfigs { by_dir, default: self.default_lint_config() }
    }

    /// The lint levels of `path`: `[lints]` of its project.
    pub(crate) fn lint_config(&self, path: &Path) -> papyrus_lint::LintConfig {
        match self.member_for(path) {
            Some(p) => p.lints.clone(),
            None => self.default_lint_config(),
        }
    }

    /// The lint levels of scripts of no project: the defaults; `[lints]` of
    /// `papyrus.toml`, read on its own, when no manifest loads (e.g. undefined
    /// variables).
    fn default_lint_config(&self) -> papyrus_lint::LintConfig {
        match (&self.project, &self.manifest) {
            (None, Some(manifest)) => papyrus_project::load_lints(manifest).unwrap_or_default(),
            _ => papyrus_lint::LintConfig::default(),
        }
    }

    /// The output directories and compiler of each project (each workspace
    /// folder without manifest), for `papyrus/status`.
    pub(crate) fn build_dirs(&self) -> Vec<BuildDirs> {
        let projects = self.members.iter().map(|p| BuildDirs {
            root: p.root.display().to_string(),
            compile: p.output.display().to_string(),
            decompile: p.output.join("decompile").display().to_string(),
            compiler: p.compiler.as_ref().map(|c| c.display().to_string()),
        });
        let limited = if self.members.is_empty() { &self.roots } else { &self.limited_roots };
        projects
            .chain(limited.iter().map(|root| {
                let build = papyrus_project::default_build_dir(root);
                BuildDirs {
                    root: root.display().to_string(),
                    compile: build.join("compile").display().to_string(),
                    decompile: build.join("decompile").display().to_string(),
                    compiler: None,
                }
            }))
            .collect()
    }

    /// Where the decompiled script of `path` goes: the directory structure
    /// below the returned base directory is kept in the returned output.
    fn decompile_target(&self, path: &Path) -> (PathBuf, PathBuf) {
        // The innermost project or workspace folder containing the file.
        let project = self
            .members
            .iter()
            .filter(|p| path.starts_with(&p.root))
            .max_by_key(|p| p.root.components().count());
        if let Some(p) = project {
            return (p.root.clone(), p.output.join("decompile"));
        }
        let root = self
            .roots
            .iter()
            .filter(|r| path.starts_with(r))
            .max_by_key(|r| r.components().count())
            .cloned()
            .or_else(|| path.parent().map(Path::to_path_buf))
            .unwrap_or_default();
        let output = match &self.project {
            Some(p) if !self.limited_roots.contains(&root) => p.output.join("decompile"),
            _ => papyrus_project::default_build_dir(&root).join("decompile"),
        };
        (root, output)
    }

    /// Handles `papyrus/decompileFiles`.
    pub(crate) fn decompile_files(&mut self, params: DecompileFilesParams) -> DecompileFilesResult {
        let mut output = None;
        let results = params
            .files
            .into_iter()
            .map(|file| {
                let result = conv::uri_to_path(&file)
                    .ok_or_else(|| "not a file".to_owned())
                    .and_then(|path| {
                        let (root, out) = self.decompile_target(&path);
                        let relative =
                            path.strip_prefix(&root).unwrap_or(&path).with_extension("psc");
                        let target = out.join(relative);
                        output.get_or_insert(out);
                        let (_, text) = crate::decompile::decompile_file(&path, false)?;
                        crate::decompile::write(&target, &text)?;
                        Ok(target.display().to_string())
                    });
                match result {
                    Ok(psc) => DecompileFileResult { file, psc: Some(psc), error: None },
                    Err(e) => DecompileFileResult { file, psc: None, error: Some(e) },
                }
            })
            .collect();
        let output = output.unwrap_or_else(|| {
            self.decompile_target(&self.roots.first().cloned().unwrap_or_default()).1
        });
        DecompileFilesResult { output: output.display().to_string(), results }
    }
}

/// The lint levels of the scripts of each project, by source directory.
#[derive(Clone, Default)]
pub(crate) struct LintConfigs {
    /// Innermost directories first.
    by_dir: Vec<(PathBuf, papyrus_lint::LintConfig)>,
    default: papyrus_lint::LintConfig,
}

impl LintConfigs {
    /// The lint levels of `path`.
    pub(crate) fn for_path(&self, path: &Path) -> &papyrus_lint::LintConfig {
        self.by_dir.iter().find(|(dir, _)| path.starts_with(dir)).map_or(&self.default, |(_, c)| c)
    }
}

/// The scripts each of `projects` sees, like the compiler, and its flags file:
/// one scope for its sources, one for its tests, which see these as well.
pub(crate) fn project_scopes(
    projects: &[Project],
    configured: &[PathBuf],
) -> Vec<papyrus_ide::Scope> {
    let mut scopes = Vec::new();
    for project in projects {
        let visible: Vec<PathBuf> =
            project.import_dirs().into_iter().chain(configured.iter().cloned()).collect();
        scopes.push(papyrus_ide::Scope {
            sources: project.sources.clone(),
            visible: visible.clone(),
            flags: project.flags.clone(),
        });
        if !project.test_sources.is_empty() {
            scopes.push(papyrus_ide::Scope {
                sources: project.test_sources.clone(),
                visible: project.test_sources.iter().cloned().chain(visible).collect(),
                flags: project.flags.clone(),
            });
        }
    }
    scopes
}

/// A manifest, and its projects or why they do not load.
type LoadedWorkspace = (PathBuf, Result<papyrus_project::ProjectWorkspace, Vec<ManifestError>>);

/// The project whose sources or tests contain `path`, the innermost one.
fn innermost<'a>(projects: &'a [Project], path: &Path) -> Option<&'a Project> {
    projects
        .iter()
        .filter_map(|p| {
            let depth = p
                .sources
                .iter()
                .chain(&p.test_sources)
                .filter(|s| path.starts_with(s))
                .map(|s| s.components().count())
                .max()?;
            Some((depth, p))
        })
        .max_by_key(|(depth, _)| *depth)
        .map(|(_, p)| p)
}
