//! The `papyrus.toml` project manifest.
//!
//! A manifest describes where the scripts of a mod live, which other sources
//! they import, the flags file and how to invoke the Papyrus compiler. Its
//! layout follows `Cargo.toml`:
//!
//! ```toml
//! [package]
//! name = "MyMod"
//! version = "0.1.0"
//!

//! [compiler]                                    # optional: without, the built-in compiler
//! path = "${SKYRIM_DIR}/Papyrus Compiler/PapyrusCompiler.exe"
//! args = []                                     # extra arguments
//!
//! [build]
//! sources = ["papyrus"]                         # directories of the scripts to compile
//! output = "papyrus/prebuilt"                   # where `.pex` files go
//! imports = ["${SKYRIM_DIR}/Data/Scripts/Source"]
//! flags = "${SKYRIM_DIR}/Data/Scripts/Source/TESV_Papyrus_Flags.flg"
//! optimize = true
//! release = false
//! final = false
//! quiet = true
//! ```
//!
//! Relative paths are relative to the directory of the manifest. Values may
//! use `${NAME}` variables and a leading `~` for the home directory. The
//! manifest is meant to be committed, so machine specific paths are written
//! as variables, see [`Variables`] for where their values come from.

use std::{
    collections::BTreeMap,
    fmt,
    ops::Range,
    path::{Path, PathBuf},
};

use serde::Deserialize;
use toml::Spanned;

mod editorconfig;
mod vars;
mod workspace;

pub use crate::{
    editorconfig::EditorConfig,
    workspace::{ProjectWorkspace, find_workspace_root, load_workspace, load_workspace_with},
};

pub use crate::vars::{
    SKYRIM_DIR, VarSource, Variables, detect_skyrim_dir, game_dir, game_script_dirs,
};

/// The file name of a manifest.
pub const MANIFEST_NAME: &str = "papyrus.toml";

/// Where a workspace without manifest builds, relative to its folder:
/// `compile` and `decompile` below it.
pub const DEFAULT_BUILD_DIR: &str = "build/papyrus";

/// [`DEFAULT_BUILD_DIR`] of `root`, with the separators of the platform.
pub fn default_build_dir(root: &Path) -> PathBuf {
    root.join(DEFAULT_BUILD_DIR).components().collect()
}

/// A problem in a manifest, with its byte range when known.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManifestError {
    /// The message.
    pub message: String,
    /// The byte range in the manifest.
    pub span: Option<Range<usize>>,
    /// The manifest, when not the loaded one (a member of a workspace).
    pub file: Option<PathBuf>,
}

impl ManifestError {
    fn new(message: impl Into<String>, span: Option<Range<usize>>) -> ManifestError {
        ManifestError { message: message.into(), span, file: None }
    }

    /// Places the error in `file`, unless it already has a file.
    pub(crate) fn in_file(mut self, file: &Path) -> ManifestError {
        self.file.get_or_insert_with(|| file.to_path_buf());
        self
    }
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

/// A value of a manifest, or `key.workspace = true` to take the one of the
/// workspace, like in `Cargo.toml`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Inheritable<T> {
    /// Written in the manifest.
    Value(T),
    /// `key.workspace = true`
    Workspace,
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Inheritable<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::{
            Error, IntoDeserializer, MapAccess, SeqAccess, Visitor,
            value::{BoolDeserializer, SeqAccessDeserializer, StrDeserializer},
        };

        struct Inherit<T>(std::marker::PhantomData<T>);

        impl<'de, T: Deserialize<'de>> Visitor<'de> for Inherit<T> {
            type Value = Inheritable<T>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a value, or `workspace = true`")
            }

            fn visit_str<E: Error>(self, v: &str) -> Result<Self::Value, E> {
                let value: StrDeserializer<'_, E> = v.into_deserializer();
                T::deserialize(value).map(Inheritable::Value)
            }

            fn visit_bool<E: Error>(self, v: bool) -> Result<Self::Value, E> {
                let value: BoolDeserializer<E> = v.into_deserializer();
                T::deserialize(value).map(Inheritable::Value)
            }

            fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<Self::Value, A::Error> {
                T::deserialize(SeqAccessDeserializer::new(seq)).map(Inheritable::Value)
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut inherits = false;
                while let Some(key) = map.next_key::<String>()? {
                    if key != "workspace" {
                        return Err(A::Error::custom(format!(
                            "unexpected `{key}`: expected a value, or `workspace = true`"
                        )));
                    }
                    if !map.next_value::<bool>()? {
                        return Err(A::Error::custom("`workspace` cannot be false"));
                    }
                    inherits = true;
                }
                if inherits {
                    Ok(Inheritable::Workspace)
                } else {
                    Err(A::Error::custom("expected a value, or `workspace = true`"))
                }
            }
        }

        deserializer.deserialize_any(Inherit(std::marker::PhantomData))
    }
}

/// The values of a `[var]` table written in the manifest.
pub(crate) fn plain_vars(raw: &BTreeMap<String, Inheritable<String>>) -> BTreeMap<String, String> {
    raw.iter()
        .filter_map(|(name, value)| match value {
            Inheritable::Value(v) => Some((name.clone(), v.clone())),
            Inheritable::Workspace => None,
        })
        .collect()
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct RawManifest {
    /// The JSON schema of the file, for editors; ignored.
    #[serde(default, rename = "$schema")]
    _schema: Option<String>,
    #[serde(default)]
    package: RawPackage,
    #[serde(default)]
    var: BTreeMap<String, Inheritable<String>>,
    #[serde(default)]
    compiler: RawCompiler,
    #[serde(default)]
    build: RawBuild,
    #[serde(default)]
    format: RawFormat,
    #[serde(default)]
    lints: BTreeMap<String, Spanned<toml::Value>>,
    #[serde(default)]
    workspace: Option<workspace::RawWorkspace>,
    #[serde(default)]
    dependencies: BTreeMap<String, workspace::RawDependency>,
    #[serde(default)]
    test: RawTest,
}

impl RawManifest {
    /// The settings asking for the values of a workspace.
    pub(crate) fn inherited_settings(&self) -> Vec<String> {
        let mut out = Vec::new();
        if lints_inherit(&self.lints) {
            out.push("lints".to_owned());
        }
        if self.format.inherits() {
            out.push("format".to_owned());
        }
        let vars = self.var.iter().filter(|(_, v)| matches!(v, Inheritable::Workspace));
        out.extend(vars.map(|(name, _)| format!("var.{name}")));
        let (compiler, build) = (&self.compiler, &self.build);
        let mut asks = |name: &str, inherits: bool| {
            if inherits {
                out.push(name.to_owned());
            }
        };
        let path = |p: &Option<Spanned<Inheritable<String>>>| {
            p.as_ref().is_some_and(|p| matches!(p.get_ref(), Inheritable::Workspace))
        };
        let flag = |f: Option<Inheritable<bool>>| matches!(f, Some(Inheritable::Workspace));
        asks("compiler.path", path(&compiler.path));
        asks("compiler.args", matches!(compiler.args, Some(Inheritable::Workspace)));
        asks("build.imports", matches!(build.imports, Some(Inheritable::Workspace)));
        asks("build.flags", path(&build.flags));
        asks("build.optimize", flag(build.optimize.clone()));
        asks("build.release", flag(build.release.clone()));
        asks("build.final", flag(build.r#final.clone()));
        asks("build.quiet", flag(build.quiet.clone()));
        out
    }
}

/// `[test]`.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct RawTest {
    #[serde(default)]
    sources: Vec<Spanned<String>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct RawFormat {
    /// `"space"` or `"tab"`, as in `.editorconfig`.
    #[serde(alias = "indent_style")]
    indent_style: Option<Spanned<String>>,
    /// Spaces per level.
    #[serde(alias = "indent_size")]
    indent_size: Option<Spanned<i64>>,
    #[serde(alias = "keyword_case")]
    keyword_case: Option<Spanned<String>>,
    #[serde(default)]
    keywords: BTreeMap<String, Spanned<String>>,
    #[serde(alias = "script_case")]
    script_case: Option<Spanned<String>>,
    #[serde(alias = "function_case")]
    function_case: Option<Spanned<String>>,
    #[serde(alias = "property_case")]
    property_case: Option<Spanned<String>>,
    #[serde(alias = "variable_case")]
    variable_case: Option<Spanned<String>>,
    #[serde(alias = "flag_case")]
    flag_case: Option<Spanned<String>>,
    #[serde(alias = "max_blank_lines")]
    max_blank_lines: Option<usize>,
    #[serde(alias = "max_width")]
    max_width: Option<usize>,
    newline: Option<Spanned<String>>,
    #[serde(alias = "final_newline")]
    final_newline: Option<bool>,
    #[serde(default)]
    exclude: Vec<Spanned<String>>,
    /// `workspace = true`: starts from `[workspace.format]`.
    #[serde(default)]
    workspace: Option<Spanned<bool>>,
}

impl RawFormat {
    fn inherits(&self) -> bool {
        self.workspace.as_ref().is_some_and(|w| *w.get_ref())
    }

    /// Whether the table sets anything but `workspace`.
    fn has_own_values(&self) -> bool {
        self.indent_style.is_some()
            || self.indent_size.is_some()
            || self.keyword_case.is_some()
            || !self.keywords.is_empty()
            || self.script_case.is_some()
            || self.function_case.is_some()
            || self.property_case.is_some()
            || self.variable_case.is_some()
            || self.flag_case.is_some()
            || self.max_blank_lines.is_some()
            || self.max_width.is_some()
            || self.newline.is_some()
            || self.final_newline.is_some()
            || !self.exclude.is_empty()
    }
}

/// Whether a `[lints]` table has `workspace = true`: it is `[workspace.lints]`,
/// like in `Cargo.toml`.
pub(crate) fn lints_inherit(raw: &BTreeMap<String, Spanned<toml::Value>>) -> bool {
    raw.get("workspace").is_some_and(|w| w.get_ref().as_bool() == Some(true))
}

/// `workspace = true` with other values of the `table`, as `cargo` says it.
fn cannot_override(table: &str, span: Option<Range<usize>>) -> ManifestError {
    ManifestError::new(
        format!(
            "cannot override `workspace.{table}` in `{table}`, either remove the overrides or `{table}.workspace = true` and manually specify the {table} settings"
        ),
        span,
    )
}

/// `workspace = true` when there is nothing to inherit, as `cargo` says it.
pub(crate) fn cannot_inherit(table: &str, why: &str) -> ManifestError {
    ManifestError::new(
        format!(
            "error inheriting `{table}` from workspace root manifest's `workspace.{table}`: {why}"
        ),
        None,
    )
}

/// [`cannot_inherit`], without workspace.
pub(crate) const NO_WORKSPACE: &str = "failed to find a workspace root";

/// [`cannot_inherit`], without the table.
pub(crate) fn not_defined(table: &str) -> ManifestError {
    cannot_inherit(table, &format!("`workspace.{table}` was not defined"))
}

/// The `[format]` table: formatter settings shared by the team. Unset values
/// come from `.editorconfig`, then from the editor.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FormatSettings {
    /// `indent-style`: `Some(true)` for `"tab"`, `Some(false)` for `"space"`.
    pub use_tabs: Option<bool>,
    /// `indent-size`: spaces per level.
    pub indent_size: Option<u8>,
    /// `keyword-case`, see [`papyrus_fmt::Case::NAMES`].
    pub keyword_case: Option<papyrus_fmt::Case>,
    /// `[format.keywords]`: spelling of single keywords, e.g. `as = "As"`.
    pub keywords: BTreeMap<String, String>,
    /// `script-case`, `function-case`, `property-case`, `variable-case` and
    /// `flag-case`, in this order: the spelling of names, `canonical` being
    /// the one of their definition. See [`papyrus_fmt::NameCases`].
    pub name_cases: [Option<papyrus_fmt::Case>; 5],
    /// `max-blank-lines`
    pub max_blank_lines: Option<usize>,
    /// `max-width`: longer lines break with `\`; `0`: never.
    pub max_width: Option<usize>,
    /// `newline`: `"auto"`, `"lf"` or `"crlf"`.
    pub newline: Option<papyrus_fmt::Newline>,
    /// `final-newline`
    pub final_newline: Option<bool>,
    /// `exclude`: the scripts left as they are.
    pub exclude: stdx::PathExcludes,
}

impl FormatSettings {
    /// Applies the set values to formatter options.
    pub fn apply(&self, options: &mut papyrus_fmt::FormatOptions) {
        let size = |options: &papyrus_fmt::FormatOptions| match options.indent {
            papyrus_fmt::Indent::Spaces(n) => n,
            papyrus_fmt::Indent::Tabs => 4,
        };
        match (self.use_tabs, self.indent_size) {
            (Some(true), _) => options.indent = papyrus_fmt::Indent::Tabs,
            (Some(false), n) => {
                options.indent = papyrus_fmt::Indent::Spaces(n.unwrap_or(size(options)))
            }
            (None, Some(n)) => {
                // A size alone keeps tabs: it is the width of a tab then.
                if let papyrus_fmt::Indent::Spaces(_) = options.indent {
                    options.indent = papyrus_fmt::Indent::Spaces(n);
                }
            }
            (None, None) => {}
        }
        if let Some(case) = self.keyword_case {
            options.keyword_case = case;
        }
        options.keyword_overrides.extend(self.keywords.iter().map(|(k, v)| (k.clone(), v.clone())));
        let n = &mut options.name_cases;
        let targets =
            [&mut n.scripts, &mut n.functions, &mut n.properties, &mut n.variables, &mut n.flags];
        for (target, case) in targets.into_iter().zip(self.name_cases) {
            if let Some(case) = case {
                *target = case;
            }
        }
        if let Some(n) = self.max_blank_lines {
            options.max_blank_lines = n;
        }
        if let Some(n) = self.max_width {
            options.max_width = n;
        }
        if let Some(newline) = self.newline {
            options.newline = newline;
        }
        if let Some(final_newline) = self.final_newline {
            options.final_newline = final_newline;
        }
    }
}

/// Adds the settings of a `[lints]` table to `config`: they win over the
/// ones already there (of the workspace). `name = "level"` or
/// `name = { level = "level" }`, by lint, group or `all`, as in the `[lints]` of
/// `Cargo.toml`; `exclude` is relative to `base`, the directory of the manifest.
pub(crate) fn lint_settings_into(
    config: &mut papyrus_lint::LintConfig,
    raw: &BTreeMap<String, Spanned<toml::Value>>,
    base: &Path,
    errors: &mut Vec<ManifestError>,
) {
    for (name, value) in raw {
        let span = Some(value.span()).filter(|r| !r.is_empty());
        if name == "workspace" {
            match value.get_ref().as_bool() {
                Some(true) if raw.len() > 1 => errors.push(cannot_override("lints", span)),
                Some(true) => {}
                _ => errors.push(ManifestError::new("`workspace` cannot be false", span)),
            }
            continue;
        }
        if name == "exclude" {
            let patterns = value.get_ref().as_array().and_then(|patterns| {
                patterns.iter().map(toml::Value::as_str).collect::<Option<Vec<&str>>>()
            });
            let Some(patterns) = patterns else {
                errors.push(ManifestError::new("`exclude` must be a list of patterns", span));
                continue;
            };
            for pattern in patterns {
                match check_exclude(pattern) {
                    Ok(()) => config.exclude(base, pattern),
                    Err(message) => errors.push(ManifestError::new(message, span.clone())),
                }
            }
            continue;
        }
        let level = match value.get_ref() {
            toml::Value::String(s) => Some(s.as_str()),
            toml::Value::Table(t) => t.get("level").and_then(toml::Value::as_str),
            _ => None,
        };
        let Some(level) = level else {
            errors.push(ManifestError::new(
                format!("`{name}` must be a level (\"allow\", \"warn\", \"deny\") or {{ level = \"...\" }}"),
                span,
            ));
            continue;
        };
        let result = level.parse().and_then(|level| config.set(name, level));
        if let Err(e) = result {
            errors.push(ManifestError::new(e, span));
        }
    }
}

/// What a pattern of `exclude` cannot be.
fn check_exclude(pattern: &str) -> Result<(), String> {
    if pattern.trim().is_empty() {
        return Err("a pattern of `exclude` cannot be empty".to_owned());
    }
    if pattern.starts_with('!') {
        return Err(format!("`{pattern}`: patterns of `exclude` cannot start with `!`"));
    }
    Ok(())
}

/// Reads only the lint settings of a manifest: its `[lints]`, or
/// `[workspace.lints]` of its workspace with `workspace = true`. Like
/// [`load_format`], the values need no variables.
pub fn load_lints(manifest_path: &Path) -> Result<papyrus_lint::LintConfig, Vec<ManifestError>> {
    let text = std::fs::read_to_string(manifest_path).map_err(|e| {
        vec![ManifestError::new(format!("cannot read {}: {e}", manifest_path.display()), None)]
    })?;
    let raw: RawManifest = toml::from_str(&text)
        .map_err(|e| vec![ManifestError::new(e.message().to_owned(), e.span())])?;
    let mut errors = Vec::new();
    let mut config = papyrus_lint::LintConfig::default();
    if lints_inherit(&raw.lints) {
        match workspace::workspace_of(manifest_path) {
            None => errors.push(cannot_inherit("lints", NO_WORKSPACE)),
            Some((_, ws)) if ws.lints.is_none() => errors.push(not_defined("lints")),
            Some((root, ws)) => {
                let mut inherited = Vec::new();
                let lints = ws.lints.unwrap_or_default();
                lint_settings_into(&mut config, &lints, parent_dir(&root), &mut inherited);
                errors.extend(inherited.into_iter().map(|e| e.in_file(&root)));
            }
        }
    }
    lint_settings_into(&mut config, &raw.lints, parent_dir(manifest_path), &mut errors);
    if errors.is_empty() { Ok(config) } else { Err(errors) }
}

/// The directory of a manifest.
fn parent_dir(manifest_path: &Path) -> &Path {
    manifest_path.parent().unwrap_or(Path::new(""))
}

/// The settings of a `[format]` table; `exclude` is relative to `base`, the
/// directory of the manifest.
fn format_settings(
    raw: &RawFormat,
    base: &Path,
    errors: &mut Vec<ManifestError>,
) -> FormatSettings {
    let span = |r: Range<usize>| Some(r).filter(|r| !r.is_empty());
    let mut out = FormatSettings {
        max_blank_lines: raw.max_blank_lines,
        max_width: raw.max_width,
        final_newline: raw.final_newline,
        ..Default::default()
    };
    if let Some(workspace) = &raw.workspace {
        if !*workspace.get_ref() {
            errors.push(ManifestError::new("`workspace` cannot be false", span(workspace.span())));
        } else if raw.has_own_values() {
            errors.push(cannot_override("format", span(workspace.span())));
        }
    }
    for pattern in &raw.exclude {
        match check_exclude(pattern.get_ref()) {
            Ok(()) => out.exclude.add(base, pattern.get_ref()),
            Err(message) => errors.push(ManifestError::new(message, span(pattern.span()))),
        }
    }
    if let Some(style) = &raw.indent_style {
        out.use_tabs = match style.get_ref().to_ascii_lowercase().as_str() {
            "tab" => Some(true),
            "space" => Some(false),
            _ => {
                errors.push(ManifestError::new(
                    "`indent-style` must be \"space\" or \"tab\"",
                    span(style.span()),
                ));
                None
            }
        };
    }
    if let Some(size) = &raw.indent_size {
        match u8::try_from(*size.get_ref()) {
            Ok(n @ 1..=16) => out.indent_size = Some(n),
            _ => errors.push(ManifestError::new(
                "`indent-size` must be between 1 and 16",
                span(size.span()),
            )),
        }
    }
    if let Some(case) = &raw.keyword_case {
        match case.get_ref().parse() {
            Ok(c) => out.keyword_case = Some(c),
            Err(e) => errors.push(ManifestError::new(e, span(case.span()))),
        }
    }
    let name_cases = [
        &raw.script_case,
        &raw.function_case,
        &raw.property_case,
        &raw.variable_case,
        &raw.flag_case,
    ];
    for (out, case) in out.name_cases.iter_mut().zip(name_cases) {
        if let Some(case) = case {
            match case.get_ref().parse() {
                Ok(c) => *out = Some(c),
                Err(e) => errors.push(ManifestError::new(e, span(case.span()))),
            }
        }
    }
    for (keyword, spelling) in &raw.keywords {
        if papyrus_lexer::keyword(keyword).is_none() {
            errors.push(ManifestError::new(
                format!("`{keyword}` is not a keyword"),
                span(spelling.span()),
            ));
        } else if !spelling.get_ref().eq_ignore_ascii_case(keyword) {
            errors.push(ManifestError::new(
                format!("the spelling of `{keyword}` may only change its case"),
                span(spelling.span()),
            ));
        } else {
            out.keywords.insert(keyword.to_ascii_lowercase(), spelling.get_ref().clone());
        }
    }
    if let Some(newline) = &raw.newline {
        out.newline = match newline.get_ref().to_ascii_lowercase().as_str() {
            "auto" => Some(papyrus_fmt::Newline::Auto),
            "lf" => Some(papyrus_fmt::Newline::Lf),
            "crlf" => Some(papyrus_fmt::Newline::CrLf),
            _ => {
                errors.push(ManifestError::new(
                    "`newline` must be \"auto\", \"lf\" or \"crlf\"",
                    span(newline.span()),
                ));
                None
            }
        };
    }
    out
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct RawPackage {
    name: Option<String>,
    version: Option<String>,
    description: Option<String>,
    authors: Option<Vec<String>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct RawCompiler {
    path: Option<Spanned<Inheritable<String>>>,
    args: Option<Inheritable<Vec<String>>>,
}

/// `[build]`. Unset values are the defaults; `key.workspace = true` takes the
/// one of `[workspace.build]`.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct RawBuild {
    sources: Option<Vec<Spanned<String>>>,
    output: Option<Spanned<String>>,
    imports: Option<Inheritable<Vec<Spanned<String>>>>,
    flags: Option<Spanned<Inheritable<String>>>,
    optimize: Option<Inheritable<bool>>,
    release: Option<Inheritable<bool>>,
    r#final: Option<Inheritable<bool>>,
    quiet: Option<Inheritable<bool>>,
}

fn default_sources() -> Vec<Spanned<String>> {
    vec![Spanned::new(0..0, ".".to_owned())]
}

/// A loaded manifest with every path resolved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Project {
    /// The directory of the manifest.
    pub root: PathBuf,
    /// The manifest file.
    pub manifest_path: PathBuf,
    /// `package.name`.
    pub name: Option<String>,
    /// `package.version`.
    pub version: Option<String>,
    /// `package.description`.
    pub description: Option<String>,
    /// `package.authors`.
    pub authors: Vec<String>,
    /// `compiler.path`. Without it, [`Project::builtin_compiler`] is used.
    pub compiler: Option<PathBuf>,
    /// The built-in compiler, set by the tool using the project: the program
    /// and its leading arguments (e.g. `papyrus_analyzer compile`), followed
    /// by the arguments of `PapyrusCompiler.exe`.
    pub builtin_compiler: Option<(PathBuf, Vec<String>)>,

    /// `compiler.args`: extra compiler arguments.
    pub extra_args: Vec<String>,
    /// `build.sources`: directories of the scripts of the project.
    pub sources: Vec<PathBuf>,
    /// `build.output`: the directory of the compiled `.pex` files.
    pub output: PathBuf,
    /// `build.imports`: other source directories, in priority order.
    pub imports: Vec<PathBuf>,
    /// The sources of `[dependencies]`, transitively: imported before
    /// `imports`.
    pub dependency_sources: Vec<PathBuf>,
    /// `[test] sources`: directories of tests (`; #[test]` functions, which see the built-in `PapyrusAnalyzer` script),
    /// run by `papyrus_analyzer test` and never compiled into `output`.
    pub test_sources: Vec<PathBuf>,
    /// `[dependencies]`: name and manifest directory.
    pub dependencies: Vec<(String, PathBuf)>,
    /// The manifest of the workspace this project is a member of.
    pub workspace_manifest: Option<PathBuf>,
    /// `build.flags`: the flags file.
    pub flags: Option<PathBuf>,
    /// `build.optimize`
    pub optimize: bool,
    /// `build.release`: strip `DebugOnly` calls.
    pub release: bool,
    /// `build.final`: strip `BetaOnly` calls.
    pub r#final: bool,
    /// `build.quiet`: only report errors.
    pub quiet: bool,
    /// `[format]`: formatter settings.
    pub format: FormatSettings,
    /// `[lints]`: the levels of the lints.
    pub lints: papyrus_lint::LintConfig,
    /// The variables used by the manifest, with their value and origin.
    pub variables: BTreeMap<String, (String, VarSource)>,
    /// Non-fatal problems, e.g. directories that do not exist.
    pub warnings: Vec<ManifestError>,
}

/// One compiler invocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompileCommand {
    /// The compiler executable.
    pub program: PathBuf,
    /// Its arguments.
    pub args: Vec<String>,
    /// The working directory.
    pub cwd: PathBuf,
    /// The output directory of this invocation, which must exist.
    pub output: PathBuf,
}

impl fmt::Display for CompileCommand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", quote(&self.program.to_string_lossy()))?;
        for arg in &self.args {
            write!(f, " {}", quote(arg))?;
        }
        Ok(())
    }
}

fn quote(s: &str) -> String {
    if s.contains([' ', '\t', '"']) {
        format!("\"{}\"", s.replace('"', "\\\""))
    } else {
        s.to_owned()
    }
}

/// Searches `dir` and its ancestors for a manifest, then the subdirectories
/// of `dir` up to three levels deep (a workspace folder often contains the
/// mod in a subdirectory).
pub fn find_manifest(dir: &Path) -> Option<PathBuf> {
    dir.ancestors()
        .map(|d| d.join(MANIFEST_NAME))
        .find(|p| p.is_file())
        .or_else(|| find_manifest_below(dir, 3))
}

fn find_manifest_below(dir: &Path, depth: usize) -> Option<PathBuf> {
    if depth == 0 {
        return None;
    }
    let mut subdirs: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            !name.starts_with('.') && name != "node_modules" && name != "target"
        })
        .map(|e| e.path())
        .collect();
    subdirs.sort();
    // Breadth first: the shallowest manifest wins.
    subdirs
        .iter()
        .map(|d| d.join(MANIFEST_NAME))
        .find(|p| p.is_file())
        .or_else(|| subdirs.iter().find_map(|d| find_manifest_below(d, depth - 1)))
}

/// The file name of the machine local, uncommitted companion of a manifest.
pub const LOCAL_MANIFEST_NAME: &str = "papyrus.local.toml";

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLocal {
    /// The JSON schema of the file, for editors; ignored.
    #[serde(default, rename = "$schema")]
    _schema: Option<String>,
    #[serde(default)]
    var: BTreeMap<String, String>,
}

/// Reads and resolves a manifest file, with the `papyrus.local.toml` next to
/// it if any.
pub fn load(manifest_path: &Path, vars: &Variables) -> Result<Project, Vec<ManifestError>> {
    let text = std::fs::read_to_string(manifest_path).map_err(|e| {
        vec![ManifestError::new(format!("cannot read {}: {e}", manifest_path.display()), None)]
    })?;
    let local = read_local(manifest_path);
    parse(manifest_path, &text, local.as_deref(), vars)
}

/// Reads only the formatter settings of a manifest: its `[format]`, or
/// `[workspace.format]` of its workspace with `workspace = true`. Unlike
/// [`load`], this does not need the variables to be defined, so formatting works
/// on any machine.
pub fn load_format(manifest_path: &Path) -> Result<FormatSettings, Vec<ManifestError>> {
    let text = std::fs::read_to_string(manifest_path).map_err(|e| {
        vec![ManifestError::new(format!("cannot read {}: {e}", manifest_path.display()), None)]
    })?;
    let raw: RawManifest = toml::from_str(&text)
        .map_err(|e| vec![ManifestError::new(e.message().to_owned(), e.span())])?;
    let own = parse_format_in(&text, parent_dir(manifest_path))?;
    if !raw.format.inherits() {
        return Ok(own);
    }
    let Some((root, ws)) = workspace::workspace_of(manifest_path) else {
        return Err(vec![cannot_inherit("format", NO_WORKSPACE)]);
    };
    let Some(format) = &ws.format else { return Err(vec![not_defined("format")]) };
    let mut errors = Vec::new();
    let settings = format_settings(format, parent_dir(&root), &mut errors);
    if !errors.is_empty() {
        return Err(errors.into_iter().map(|e| e.in_file(&root)).collect());
    }
    Ok(settings)
}

/// Parses only the `[format]` table of the text of a manifest of the directory
/// `base`.
fn parse_format_in(text: &str, base: &Path) -> Result<FormatSettings, Vec<ManifestError>> {
    let raw: RawManifest = toml::from_str(text)
        .map_err(|e| vec![ManifestError::new(e.message().to_owned(), e.span())])?;
    let mut errors = Vec::new();
    let settings = format_settings(&raw.format, base, &mut errors);
    if errors.is_empty() { Ok(settings) } else { Err(errors) }
}

/// The `[format]` settings of `script`: of the manifest found above it, when its
/// `[build] sources` or `[test] sources` contain the script. Others scripts below
/// the manifest are not of its project.
fn format_settings_of(script: &Path) -> Option<FormatSettings> {
    let script = std::path::absolute(script).unwrap_or_else(|_| script.to_path_buf());
    let manifest = script.parent().and_then(find_manifest_upwards)?;
    let text = std::fs::read_to_string(&manifest).ok()?;
    let raw: RawManifest = toml::from_str(&text).ok()?;
    // A manifest of `[workspace]` only is no project.
    if raw.workspace.is_some() && raw.package.name.is_none() && raw.build.sources.is_none() {
        return None;
    }
    let dir = parent_dir(&manifest);
    let sources = raw.build.sources.clone().unwrap_or_else(default_sources);
    let contains = |source: &Spanned<String>| {
        // Variables need more than formatting does: it may be there.
        source.get_ref().contains("${") || {
            let mut below = stdx::PathExcludes::default();
            below.add(&resolve_path(dir, source.get_ref()), "**");
            below.matches(&script)
        }
    };
    if !sources.iter().chain(&raw.test.sources).any(contains) {
        return None;
    }
    load_format(&manifest).ok()
}

/// Whether `[format] exclude` of the project of `script` leaves it out.
pub fn format_excludes(script: &Path) -> bool {
    let absolute = std::path::absolute(script).unwrap_or_else(|_| script.to_path_buf());
    format_settings_of(script).is_some_and(|settings| settings.exclude.matches(&absolute))
}

/// The formatter options of a script: defaults, then `base` (e.g. the editor
/// settings), then `.editorconfig`, then `[format]` of its project.
pub fn format_options_for(
    script: &Path,
    base: impl FnOnce(&mut papyrus_fmt::FormatOptions),
) -> papyrus_fmt::FormatOptions {
    let mut options = papyrus_fmt::FormatOptions::default();
    base(&mut options);
    EditorConfig::for_file(script).apply(&mut options);
    if let Some(settings) = format_settings_of(script) {
        settings.apply(&mut options);
    }
    options
}

/// Searches `dir` and its ancestors only.
fn find_manifest_upwards(dir: &Path) -> Option<PathBuf> {
    dir.ancestors().map(|d| d.join(MANIFEST_NAME)).find(|p| p.is_file())
}

/// Reads the `papyrus.local.toml` next to a manifest.
pub fn read_local(manifest_path: &Path) -> Option<String> {
    std::fs::read_to_string(manifest_path.with_file_name(LOCAL_MANIFEST_NAME)).ok()
}

/// Parses manifest text and the text of its `papyrus.local.toml`. `vars`
/// provides the editor and process variables; its `local` and `manifest`
/// tables are filled from the files.
pub fn parse(
    manifest_path: &Path,
    text: &str,
    local: Option<&str>,
    vars: &Variables,
) -> Result<Project, Vec<ManifestError>> {
    parse_with(manifest_path, text, local, vars, None)
}

/// The value of the workspace for `key.workspace = true`, `setting` being the
/// key; `None` without workspace, when loaded on its own.
fn inherit<T>(
    setting: &str,
    inherited: Option<&workspace::Inherited>,
    value: impl FnOnce(&workspace::Inherited) -> Option<T>,
    errors: &mut Vec<ManifestError>,
) -> Option<T> {
    let found = value(inherited?);
    if found.is_none() {
        errors.push(not_defined(setting));
    }
    found
}

/// Like [`parse`], for a member of a workspace: `key.workspace = true` takes the
/// value of the workspace.
pub(crate) fn parse_with(
    manifest_path: &Path,
    text: &str,
    local: Option<&str>,
    vars: &Variables,
    inherited: Option<&workspace::Inherited>,
) -> Result<Project, Vec<ManifestError>> {
    let raw: RawManifest = toml::from_str(text)
        .map_err(|e| vec![ManifestError::new(e.message().to_owned(), e.span())])?;
    let outside = vars;
    let mut vars = vars.clone();
    vars.manifest.extend(plain_vars(&raw.var));
    let mut var_errors = Vec::new();
    for (name, value) in &raw.var {
        if matches!(value, Inheritable::Workspace)
            && let Some(i) = inherited
        {
            match i.var_value(name, outside) {
                Ok(value) => {
                    vars.manifest.insert(name.clone(), value);
                }
                Err(error) => var_errors.push(error),
            }
        }
    }
    if let Some(local) = local {
        let raw_local: RawLocal = toml::from_str(local).map_err(|e| {
            vec![ManifestError::new(format!("{LOCAL_MANIFEST_NAME}: {}", e.message()), None)]
        })?;
        vars.local.extend(raw_local.var);
    }

    let root = manifest_path.parent().map(Path::to_path_buf).unwrap_or_default();
    let mut resolver =
        Resolver { root: &root, vars: &vars, used: BTreeMap::new(), errors: var_errors };

    let raw_sources = raw.build.sources.clone().unwrap_or_else(default_sources);
    let compiler = match &raw.compiler.path {
        None => None,
        Some(path) => match path.get_ref() {
            Inheritable::Value(value) => Some(resolver.path_at(value, span_of(path))),
            Inheritable::Workspace => {
                inherit("compiler.path", inherited, |i| i.compiler.clone(), &mut resolver.errors)
            }
        },
    };
    let sources: Vec<PathBuf> = raw_sources.iter().map(|p| resolver.path(p)).collect();
    let output = match &raw.build.output {
        Some(p) => resolver.path(p),
        None => root.join("output"),
    };
    // Like `Cargo.toml`: a value of the member replaces the one of the workspace.
    let own_imports: &[Spanned<String>] = match &raw.build.imports {
        Some(Inheritable::Value(list)) => list,
        _ => &[],
    };
    let mut imports: Vec<PathBuf> = own_imports.iter().map(|p| resolver.path(p)).collect();
    if matches!(raw.build.imports, Some(Inheritable::Workspace)) {
        imports = inherit("build.imports", inherited, |i| i.imports.clone(), &mut resolver.errors)
            .unwrap_or_default();
    }
    let flags = match &raw.build.flags {
        None => None,
        Some(path) => match path.get_ref() {
            Inheritable::Value(value) => Some(resolver.path_at(value, span_of(path))),
            Inheritable::Workspace => {
                inherit("build.flags", inherited, |i| i.flags.clone(), &mut resolver.errors)
            }
        },
    };
    let extra_args: Vec<String> = match &raw.compiler.args {
        None => Vec::new(),
        Some(Inheritable::Value(args)) => args.iter().map(|a| resolver.expand(a, None)).collect(),
        Some(Inheritable::Workspace) => {
            inherit("compiler.args", inherited, |i| i.args.clone(), &mut resolver.errors)
                .unwrap_or_default()
        }
    };
    let mut flag = |setting: &str,
                    own: &Option<Inheritable<bool>>,
                    ws: fn(&workspace::Inherited) -> Option<bool>,
                    default: bool| match own {
        None => default,
        Some(Inheritable::Value(value)) => *value,
        Some(Inheritable::Workspace) => {
            inherit(setting, inherited, ws, &mut resolver.errors).unwrap_or(default)
        }
    };
    let optimize = flag("build.optimize", &raw.build.optimize, |i| i.optimize, true);
    let release = flag("build.release", &raw.build.release, |i| i.release, false);
    let r#final = flag("build.final", &raw.build.r#final, |i| i.r#final, false);
    let quiet = flag("build.quiet", &raw.build.quiet, |i| i.quiet, true);
    // Like `Cargo.toml`: only the tables asking for it, which set nothing else.
    let mut format = format_settings(&raw.format, &root, &mut resolver.errors);
    if raw.format.inherits()
        && let Some(i) = inherited
    {
        match &i.format {
            Some(settings) => format = settings.clone(),
            None => resolver.errors.push(not_defined("format")),
        }
    }
    let mut lints = papyrus_lint::LintConfig::default();
    lint_settings_into(&mut lints, &raw.lints, &root, &mut resolver.errors);
    if lints_inherit(&raw.lints)
        && let Some(i) = inherited
    {
        match &i.lints {
            Some(config) => lints = config.clone(),
            None => resolver.errors.push(not_defined("lints")),
        }
    }
    let test_sources: Vec<PathBuf> = raw.test.sources.iter().map(|p| resolver.path(p)).collect();
    let dependencies: Vec<(String, PathBuf)> = raw
        .dependencies
        .iter()
        .map(|(name, dep)| (name.clone(), resolver.path(&dep.path)))
        .collect();

    if !resolver.errors.is_empty() {
        return Err(resolver.errors);
    }
    let variables = resolver.used;

    let mut warnings = Vec::new();
    for (path, raw) in sources.iter().zip(&raw_sources).chain(imports.iter().zip(own_imports)) {
        if !path.is_dir() {
            warnings.push(ManifestError::new(
                format!("directory not found: {}", path.display()),
                span_of(raw),
            ));
        }
    }
    if let (Some(path), Some(raw)) = (&flags, &raw.build.flags)
        && !path.is_file()
    {
        warnings.push(ManifestError::new(
            format!("flags file not found: {}", path.display()),
            span_of(raw),
        ));
    }
    if let (Some(path), Some(raw)) = (&compiler, &raw.compiler.path)
        && !path.is_file()
    {
        warnings.push(ManifestError::new(
            format!("compiler not found: {}", path.display()),
            span_of(raw),
        ));
    }

    Ok(Project {
        manifest_path: manifest_path.to_path_buf(),
        root,
        name: raw.package.name,
        version: raw.package.version,
        description: raw.package.description,
        authors: raw.package.authors.unwrap_or_default(),
        compiler,
        builtin_compiler: None,
        extra_args,
        sources,
        output,
        imports,
        dependency_sources: Vec::new(),
        test_sources,
        dependencies,
        workspace_manifest: inherited.map(|i| i.manifest.clone()),
        flags,
        optimize,
        release,
        r#final,
        quiet,
        format,
        lints,
        variables,
        warnings,
    })
}

/// The range of a value, when it was read from a file.
fn span_of<T>(value: &Spanned<T>) -> Option<Range<usize>> {
    Some(value.span()).filter(|r| !r.is_empty())
}

struct Resolver<'a> {
    root: &'a Path,
    vars: &'a Variables,
    used: BTreeMap<String, (String, VarSource)>,
    errors: Vec<ManifestError>,
}

impl Resolver<'_> {
    fn path(&mut self, value: &Spanned<String>) -> PathBuf {
        self.path_at(value.get_ref(), span_of(value))
    }

    fn path_at(&mut self, value: &str, span: Option<Range<usize>>) -> PathBuf {
        let expanded = self.expand(value, span);
        resolve_path(self.root, &expanded)
    }

    fn expand(&mut self, value: &str, span: Option<Range<usize>>) -> String {
        match self.vars.expand(value, &mut self.used) {
            Ok(v) => v,
            Err(message) => {
                self.errors.push(ManifestError::new(message, span));
                String::new()
            }
        }
    }
}

/// The variables seen from a manifest: `vars` with the `[var]` tables of the
/// manifest and of its local file filled in. Invalid TOML is tolerated, so
/// that hovers keep working while the file is edited. `manifest_path` finds the
/// values of `NAME.workspace = true`.
pub fn manifest_variables(
    manifest_path: &Path,
    text: &str,
    local: Option<&str>,
    vars: &Variables,
) -> Variables {
    fn var_table(text: &str) -> toml::Table {
        let Ok(mut table) = text.parse::<toml::Table>() else { return toml::Table::new() };
        match table.remove("var") {
            Some(toml::Value::Table(vars)) => vars,
            _ => toml::Table::new(),
        }
    }
    let values = |table: &toml::Table| -> BTreeMap<String, String> {
        let strings = table.iter().filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned())));
        strings.collect()
    };
    let own = var_table(text);
    let mut vars = vars.clone();
    vars.manifest = values(&own);
    vars.local = local.map(|l| values(&var_table(l))).unwrap_or_default();
    let inherits =
        |v: &toml::Value| v.get("workspace").and_then(toml::Value::as_bool) == Some(true);
    if own.values().any(inherits)
        && let Some((_, ws)) = workspace::workspace_of(manifest_path)
    {
        for name in own.iter().filter(|(_, v)| inherits(v)).map(|(k, _)| k) {
            if let Some(value) = ws.var.get(name) {
                vars.manifest.insert(name.clone(), value.clone());
            }
        }
    }
    vars
}

/// The variable referenced at byte `offset` of a manifest: inside a `${...}`
/// reference, or a key of the `[var]` table. Returns the name and its byte
/// range.
pub fn variable_at(text: &str, offset: usize) -> Option<(String, Range<usize>)> {
    let offset = offset.min(text.len());
    let line_start = text[..offset].rfind('\n').map_or(0, |i| i + 1);
    let line_end = text[offset..].find('\n').map_or(text.len(), |i| offset + i);
    let line = &text[line_start..line_end];

    // `${NAME}` around the offset.
    let mut search = 0;
    while let Some(open) = line[search..].find("${") {
        let open = search + open;
        let Some(close) = line[open..].find('}') else { break };
        let close = open + close;
        let (start, end) = (line_start + open, line_start + close + 1);
        if (start..end).contains(&offset) {
            return Some((line[open + 2..close].trim().to_owned(), start..end));
        }
        search = close + 1;
    }

    // `NAME = ...` inside `[var]`.
    let key_end = line.find('=')?;
    let key = line[..key_end].trim();
    let key_start = line_start + line.find(key)?;
    if key.is_empty() || !(key_start..=key_start + key.len()).contains(&offset) {
        return None;
    }
    let section = text[..line_start].lines().rev().map(str::trim).find(|l| l.starts_with('['))?;
    (section == "[var]")
        .then(|| (key.trim_matches('"').to_owned(), key_start..key_start + key.len()))
}

/// Joins a relative path to `root` and normalizes separators.
pub fn resolve_path(root: &Path, value: &str) -> PathBuf {
    let path = PathBuf::from(value);
    let path = if path.is_absolute() { path } else { root.join(path) };
    // Rebuilding from components normalizes separators and drops `.`; `..`
    // pops lexically, so `App/../Lib` and `Lib` name the same directory.
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            std::path::Component::ParentDir if out.file_name().is_some() => {
                out.pop();
            }
            c => out.push(c),
        }
    }
    out
}
impl Project {
    /// A project for a folder without manifest: its scripts compile with the
    /// built-in compiler (set [`Project::builtin_compiler`]), with the given
    /// imports and flags file, into [`DEFAULT_BUILD_DIR`]`/compile`, keeping
    /// the directory structure.
    pub fn without_manifest(
        root: PathBuf,
        imports: Vec<PathBuf>,
        flags: Option<PathBuf>,
    ) -> Project {
        Project {
            manifest_path: root.join(MANIFEST_NAME),
            output: default_build_dir(&root).join("compile"),
            sources: vec![root.clone()],
            root,
            name: None,
            version: None,
            description: None,
            authors: Vec::new(),
            compiler: None,
            builtin_compiler: None,
            extra_args: Vec::new(),
            imports,
            dependency_sources: Vec::new(),
            test_sources: Vec::new(),
            dependencies: Vec::new(),
            workspace_manifest: None,
            flags,
            optimize: false,
            release: false,
            r#final: false,
            quiet: true,
            format: FormatSettings::default(),
            lints: papyrus_lint::LintConfig::default(),
            variables: BTreeMap::new(),
            warnings: Vec::new(),
        }
    }

    /// Every directory that provides scripts for name resolution, in compiler
    /// lookup order: the sources, then the imports.
    pub fn import_dirs(&self) -> Vec<PathBuf> {
        let mut dirs: Vec<PathBuf> = Vec::new();
        for dir in self.sources.iter().chain(&self.dependency_sources).chain(&self.imports) {
            if !dirs.contains(dir) {
                dirs.push(dir.clone());
            }
        }
        dirs
    }

    /// The compiler `-import` list: every directory below the sources that
    /// contains scripts, then the imports. Skyrim scripts live in one flat
    /// namespace, so a script in `sources/a` may use one in `sources/b`, and
    /// the compiler only looks in the listed directories, not below them.
    pub fn compiler_import_dirs(&self) -> Vec<PathBuf> {
        let mut dirs: Vec<PathBuf> = Vec::new();
        for script in self.scripts() {
            if let Some(parent) = script.parent()
                && !dirs.iter().any(|d| d == parent)
            {
                dirs.push(parent.to_path_buf());
            }
        }
        for dir in self.sources.iter().chain(&self.dependency_sources).chain(&self.imports) {
            if !dirs.contains(dir) {
                dirs.push(dir.clone());
            }
        }
        dirs
    }

    /// The command compiling one script.
    ///
    /// The compiler only accepts single files, so the directory structure is
    /// kept by hand: `sources/a/b/X.psc` compiles into `output/a/b/X.pex`.
    pub fn compile_file(&self, script: &Path) -> Result<CompileCommand, String> {
        let dirs = self.compiler_import_dirs();
        self.compile_file_with(script, dirs)
    }

    fn compile_file_with(
        &self,
        script: &Path,
        mut dirs: Vec<PathBuf>,
    ) -> Result<CompileCommand, String> {
        // The directory of the script first, so that it wins over duplicates.
        let parent = script.parent().unwrap_or(&self.root);
        dirs.retain(|d| d != parent);
        dirs.insert(0, parent.to_path_buf());
        // The innermost source directory containing the script.
        let relative = self
            .sources
            .iter()
            .filter_map(|src| parent.strip_prefix(src).ok())
            .min_by_key(|rel| rel.components().count());
        let output = match relative {
            Some(rel) if rel.components().next().is_some() => self.output.join(rel),
            _ => self.output.clone(),
        };
        self.command(script, &dirs, output)
    }

    /// Directories never searched for scripts: the output, the default build
    /// directory, which holds decompiled scripts, and the tests.
    fn skipped_dirs(&self) -> Vec<PathBuf> {
        let mut dirs = vec![self.output.clone(), default_build_dir(&self.root)];
        // Tests are not part of the mod.
        dirs.extend(self.test_sources.iter().cloned());
        dirs
    }

    /// Every script below the source directories, recursively. The output
    /// directory and hidden, `node_modules` and `target` directories are
    /// skipped.
    pub fn scripts(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for src in &self.sources {
            collect_scripts(src, &self.skipped_dirs(), &mut out);
        }
        out.sort();
        out.dedup();
        out
    }

    /// The commands compiling every script below the source directories.
    pub fn compile_all(&self) -> Result<Vec<CompileCommand>, String> {
        let dirs = self.compiler_import_dirs();
        self.scripts().iter().map(|s| self.compile_file_with(s, dirs.clone())).collect()
    }

    fn command(
        &self,
        script: &Path,
        imports: &[PathBuf],
        output: PathBuf,
    ) -> Result<CompileCommand, String> {
        let (program, mut args) = match (&self.compiler, &self.builtin_compiler) {
            (Some(path), _) => {
                // The official compiler needs a flags file.
                if self.flags.is_none() {
                    return Err(format!("set `[build] flags` in {}", self.manifest_path.display()));
                }
                (path.clone(), Vec::new())
            }
            (None, Some((program, prefix))) => (program.clone(), prefix.clone()),
            (None, None) => {
                return Err(format!("set `[compiler] path` in {}", self.manifest_path.display()));
            }
        };
        args.push(script.display().to_string());
        // The built-in compiler knows the Skyrim flags without a file.
        if let Some(flags) = &self.flags {
            args.push(format!("-f={}", flags.display()));
        }
        let imports: Vec<String> = imports.iter().map(|d| d.display().to_string()).collect();
        args.push(format!("-import={}", imports.join(";")));
        args.push(format!("-output={}", output.display()));
        for (on, flag) in [
            (self.optimize, "-optimize"),
            (self.release, "-release"),
            (self.r#final, "-final"),
            (self.quiet, "-quiet"),
        ] {
            if on {
                args.push(flag.to_owned());
            }
        }
        args.extend(self.extra_args.iter().cloned());
        Ok(CompileCommand { program, args, cwd: self.root.clone(), output })
    }
}

fn collect_scripts(dir: &Path, skipped: &[PathBuf], out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let hidden = name.starts_with('.') || name == "node_modules" || name == "target";
            if !hidden && !skipped.contains(&path) {
                collect_scripts(&path, skipped, out);
            }
        } else if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("psc")) {
            out.push(path);
        }
    }
}
/// A manifest to start from. Its `$schema` is the one of the release `version`;
/// the schema documents the settings, in the editor.
pub fn template(version: &str) -> String {
    TEMPLATE.replace("{VERSION}", version)
}

const TEMPLATE: &str = r#""$schema" = "https://raw.githubusercontent.com/SARDONYX-sard/papyrus_analyzer/refs/tags/{VERSION}/schemas/papyrus.schema.json"

[package]
name = "MyMod"
version = "0.1.0"

[var]
SKYRIM_DIR = "${game:SkyrimSE}"

[compiler]
path = "${SKYRIM_DIR}/Papyrus Compiler/PapyrusCompiler.exe"
args = []

[build]
sources = ["."]
output = "output"
imports = ["${SKYRIM_DIR}/Data/Scripts/Source"]
flags = "${SKYRIM_DIR}/Data/Scripts/Source/TESV_Papyrus_Flags.flg"
optimize = true
release = false
final = false
quiet = true

[test]

[dependencies]

[lints]

[format]
"#;

/// A `papyrus.local.toml` to start from.
pub const LOCAL_TEMPLATE: &str = r#"# papyrus.local.toml — this machine only, do not commit (.gitignore).
[var]                           # wins over [var] of papyrus.toml
# SKYRIM_DIR = "${game:SkyrimSE}"
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_env(name: &str) -> Option<String> {
        match name {
            "GAME_ENV" => Some("/env/game".to_owned()),
            "HOME" => Some("/home/me".to_owned()),
            _ => None,
        }
    }

    fn vars() -> Variables {
        let mut v = Variables { process_env: fake_env, ..Variables::default() };
        v.editor.insert("SKYRIM_DIR".to_owned(), "/games/skyrim".to_owned());
        v
    }

    fn parse_str(text: &str) -> Result<Project, Vec<ManifestError>> {
        parse(Path::new("/m/papyrus.toml"), text, None, &vars())
    }

    /// Paths with `/` separators, for platform independent assertions.
    fn s(path: &Path) -> String {
        path.display().to_string().replace('\\', "/")
    }

    fn all(paths: &[PathBuf]) -> Vec<String> {
        paths.iter().map(|p| s(p)).collect()
    }

    #[test]
    fn template_resolves() {
        let text = template("1.2.3");
        assert!(text.contains("/refs/tags/1.2.3/schemas/papyrus.schema.json"), "{text}");
        let p = parse(Path::new("/mods/my/papyrus.toml"), &text, None, &vars()).unwrap();
        assert_eq!(p.name.as_deref(), Some("MyMod"));
        assert_eq!(
            s(p.compiler.as_deref().unwrap()),
            "/games/skyrim/Papyrus Compiler/PapyrusCompiler.exe"
        );
        assert_eq!(all(&p.sources), ["/mods/my"]);
        assert_eq!(s(&p.output), "/mods/my/output");
        assert_eq!(all(&p.imports), ["/games/skyrim/Data/Scripts/Source"]);
        assert!(p.optimize && p.quiet && !p.release);
    }

    #[test]
    fn var_table_and_env() {
        let text = "[var]\nGAME = \"${env:GAME_ENV}\"\n[build]\nimports = [\"${GAME}/a\", \"${SKYRIM_DIR}/b\", \"~/c\"]\n";
        let p = parse_str(text).unwrap();
        assert_eq!(all(&p.imports), ["/env/game/a", "/games/skyrim/b", "/home/me/c"]);
        assert_eq!(p.variables["SKYRIM_DIR"].1, VarSource::Editor);
        assert!(parse_str("[env]\n").unwrap_err()[0].message.contains("unknown field `env`"));
    }

    #[test]
    fn local_file_and_editor_override_the_manifest() {
        let text = "[var]\nMODS = \"/shared\"\n[build]\nimports = [\"${MODS}/a\"]\n";
        let local = "[var]\nMODS = \"/local\"\n";
        let p = parse(Path::new("/m/papyrus.toml"), text, Some(local), &vars()).unwrap();
        assert_eq!(all(&p.imports), ["/local/a"]);
        assert_eq!(p.variables["MODS"].1, VarSource::Local);

        let mut editor = vars();
        editor.editor.insert("MODS".to_owned(), "/editor".to_owned());
        let p = parse(Path::new("/m/papyrus.toml"), text, Some(local), &editor).unwrap();
        assert_eq!(all(&p.imports), ["/editor/a"]);

        let errors =
            parse(Path::new("/m/papyrus.toml"), text, Some("[build]\n"), &vars()).unwrap_err();
        assert!(errors[0].message.starts_with("papyrus.local.toml:"), "{}", errors[0].message);
    }

    #[test]
    fn variable_lookup_for_hovers() {
        let text = "[var]\nGAME = \"${env:GAME_ENV}\"\n[build]\nimports = [\"${GAME}/x\"]\n";
        let at = |needle: &str| text.find(needle).unwrap();
        assert_eq!(variable_at(text, at("GAME =") + 1).unwrap().0, "GAME");
        assert_eq!(variable_at(text, at("${GAME}") + 3).unwrap().0, "GAME");
        assert_eq!(variable_at(text, at("env:") + 1).unwrap().0, "env:GAME_ENV");
        assert!(variable_at(text, at("imports") + 1).is_none(), "not in [var]");
        let vars = manifest_variables(
            Path::new("/nowhere/papyrus.toml"),
            text,
            Some("[var]\nLOCAL = \"1\"\n"),
            &vars(),
        );
        assert_eq!(vars.lookup("GAME").unwrap(), ("/env/game".to_owned(), VarSource::Manifest));
        assert_eq!(vars.lookup("LOCAL").unwrap().1, VarSource::Local);
    }

    #[test]
    fn format_table() {
        let p = parse_str(
            "[format]\nindent_style = \"tab\"\nkeyword-case = \"camelCase\"\nfunction-case = \"preserve\"\nnewline = \"crlf\"\n[format.keywords]\nas = \"As\"\n",
        )
        .unwrap();
        let mut options = papyrus_fmt::FormatOptions::default();
        p.format.apply(&mut options);
        assert_eq!(options.indent, papyrus_fmt::Indent::Tabs);
        assert_eq!(options.keyword_case, papyrus_fmt::Case::CamelCase);
        assert_eq!(options.name_cases.functions, papyrus_fmt::Case::Preserve);
        assert_eq!(options.name_cases.scripts, papyrus_fmt::Case::Canonical);
        assert_eq!(options.newline, papyrus_fmt::Newline::CrLf);
        assert_eq!(options.keyword_overrides["as"], "As");

        let spaces = parse_str("[format]\nindent-style = \"space\"\nindent-size = 2\n").unwrap();
        let mut options = papyrus_fmt::FormatOptions::default();
        spaces.format.apply(&mut options);
        assert_eq!(options.indent, papyrus_fmt::Indent::Spaces(2));

        let errors = parse_str("[format]\nindent-size = 0\nkeyword-case = \"kebab\"\n[format.keywords]\nfoo = \"Foo\"\nif = \"Iff\"\n").unwrap_err();
        let messages: Vec<_> = errors.iter().map(|e| e.message.as_str()).collect();
        assert_eq!(messages.len(), 4, "{messages:?}");
        assert!(errors.iter().all(|e| e.span.is_some()));
    }

    #[test]
    fn errors_have_spans() {
        let text = "[build]\nimports = [\"${MISSING}/x\"]\n";
        let errors = parse_str(text).unwrap_err();
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("`MISSING`"));
        assert_eq!(&text[errors[0].span.clone().unwrap()], "\"${MISSING}/x\"");

        // `$schema` is for editors.
        parse_str("\"$schema\" = \"file://schemas/papyrus.schema.json\"\n").unwrap();

        let errors = parse_str("[build]\nsource = []\n").unwrap_err();
        assert!(errors[0].message.contains("unknown field `source`"), "{}", errors[0].message);
        assert!(errors[0].span.is_some());
    }

    #[test]
    fn compile_commands() {
        let p =
            parse_str(&template("1.2.3").replace(r#"sources = ["."]"#, r#"sources = ["papyrus"]"#))
                .unwrap();
        let cmd = p.compile_file(Path::new("/m/papyrus/Foo.psc")).unwrap();
        let args: Vec<String> = cmd.args.iter().map(|a| a.replace('\\', "/")).collect();
        assert_eq!(
            args,
            [
                "/m/papyrus/Foo.psc",
                "-f=/games/skyrim/Data/Scripts/Source/TESV_Papyrus_Flags.flg",
                "-import=/m/papyrus;/games/skyrim/Data/Scripts/Source",
                "-output=/m/output",
                "-optimize",
                "-quiet",
            ]
        );

        // Nested scripts keep their directory below `output`.
        let nested = p.compile_file(Path::new("/m/papyrus/quests/main/Bar.psc")).unwrap();
        assert_eq!(s(&nested.output), "/m/output/quests/main");
        assert!(
            nested.args[2]
                .replace('\\', "/")
                .starts_with("-import=/m/papyrus/quests/main;/m/papyrus;")
        );

        let mut bare = parse_str("").unwrap();
        assert!(bare.compile_file(Path::new("/m/X.psc")).unwrap_err().contains("[compiler] path"));
        // Without `[compiler] path`, the built-in compiler, which needs no
        // flags file.
        bare.builtin_compiler = Some((PathBuf::from("pa"), vec!["compile".to_owned()]));
        let c = bare.compile_file(Path::new("/m/X.psc")).unwrap();
        assert_eq!(s(&c.program), "pa");
        assert_eq!(c.args[..2], ["compile".to_owned(), "/m/X.psc".to_owned()]);
        assert!(c.args[2].starts_with("-import="));
    }

    #[test]
    fn workspace_members_inherit_and_see_dependencies() {
        let dir = std::env::temp_dir().join(format!("papyrus-ws-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for d in ["mods/Lib", "mods/App", "mods/Old", "game"] {
            std::fs::create_dir_all(dir.join(d)).unwrap();
        }
        let w = |p: &str, text: &str| std::fs::write(dir.join(p), text).unwrap();
        w(
            "papyrus.toml",
            "[var]\nMODS = \"mods\"\n[workspace]\nmembers = [\"${MODS}/*\"]\nexclude = [\"${MODS}/Old\"]\n[workspace.var]\nGAME = \"game\"\n[workspace.build]\nimports = [\"${GAME}\"]\nflags = \"${GAME}/f.flg\"\noptimize = false\n[workspace.lints]\nall = \"deny\"\n[workspace.format]\nindent-size = 2\n",
        );
        w("mods/Lib/papyrus.toml", "[package]\nname = \"Lib\"\n");
        w(
            "mods/App/papyrus.toml",
            "[package]\nname = \"App\"\n[var]\nGAME.workspace = true\n[build]\noutput = \"${GAME}/out\"\nimports.workspace = true\nflags.workspace = true\noptimize.workspace = true\n[dependencies]\nLib = { path = \"../Lib\" }\n[lints]\nworkspace = true\n[format]\nworkspace = true\n",
        );
        w("mods/Old/papyrus.toml", "[package]\nname = \"Old\"\n");

        // Loading from a member finds the workspace.
        let ws = load_workspace(&dir.join("mods/App/papyrus.toml"), &vars()).unwrap();
        let names: Vec<_> = ws.members.iter().map(|m| m.name.clone().unwrap()).collect();
        assert_eq!(names, ["App", "Lib"]);
        let app = &ws.members[0];
        assert_eq!(app.flags.as_deref(), Some(dir.join("game/f.flg").as_path()));
        assert!(!app.optimize);
        assert_eq!(app.output, dir.join("mods/App/game/out"));
        assert_eq!(app.imports, [dir.join("game")]);
        assert_eq!(
            app.import_dirs(),
            [dir.join("mods/App"), dir.join("mods/Lib"), dir.join("game")]
        );
        // Without `key.workspace = true`, like in `Cargo.toml`: nothing of the workspace.
        let lib = &ws.members[1];
        assert_eq!(lib.import_dirs(), [dir.join("mods/Lib")]);
        assert!(lib.optimize);
        assert_eq!(lib.flags, None);
        // With `workspace = true`, the tables of the workspace.
        let level = |p: &Project, code| p.lints.level(papyrus_lint::find(code).unwrap());
        assert_eq!(level(app, "unreachable-code"), papyrus_lint::Level::Deny);
        assert_eq!(app.format.indent_size, Some(2));
        assert_eq!(
            ws.member_for(&dir.join("mods/Lib/A.psc")).unwrap().name.as_deref(),
            Some("Lib")
        );
        // Without `workspace = true`, like in `Cargo.toml`: nothing of these tables.
        assert_eq!(level(lib, "unreachable-code"), papyrus_lint::Level::Warn);
        assert_eq!(lib.format.indent_size, None);
        let on_its_own = load_lints(&dir.join("mods/Lib/papyrus.toml")).unwrap();
        assert_eq!(
            on_its_own.level(papyrus_lint::find("unreachable-code").unwrap()),
            papyrus_lint::Level::Warn
        );
        assert_eq!(load_format(&dir.join("mods/Lib/papyrus.toml")).unwrap().indent_size, None);
        // Without the variables too, as the command line lint and format do.
        let manifest = dir.join("mods/App/papyrus.toml");
        let lints = load_lints(&manifest).unwrap();
        let cli_level = |code| lints.level(papyrus_lint::find(code).unwrap());
        assert_eq!(cli_level("unreachable-code"), papyrus_lint::Level::Deny);
        assert_eq!(load_format(&manifest).unwrap().indent_size, Some(2));
        let excluded = dir.join("mods/Old/papyrus.toml");
        let old = load_lints(&excluded).unwrap();
        assert_eq!(
            old.level(papyrus_lint::find("unreachable-code").unwrap()),
            papyrus_lint::Level::Warn
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn format_settings_are_of_the_scripts_of_the_project() {
        let dir = std::env::temp_dir().join(format!("papyrus-fmt-of-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let manifest = dir.join("papyrus.toml");
        let size = |script: &str| {
            let options = format_options_for(&dir.join(script), |o| {
                o.indent = papyrus_fmt::Indent::Spaces(2);
            });
            options.indent
        };
        let eight = papyrus_fmt::Indent::Spaces(8);
        let editor = papyrus_fmt::Indent::Spaces(2);

        std::fs::write(
            &manifest,
            "[build]\nsources = [\"src\"]\n[test]\nsources = [\"tests\"]\n[format]\nindent-style = \"space\"\nindent-size = 8\nexclude = [\"src/gen\"]\n",
        )
        .unwrap();
        assert_eq!(size("src/A.psc"), eight);
        assert_eq!(size("src/sub/A.psc"), eight);
        assert_eq!(size("tests/T.psc"), eight);
        assert_eq!(size("other/A.psc"), editor);
        assert!(format_excludes(&dir.join("src/gen/A.psc")));
        assert!(!format_excludes(&dir.join("other/gen/A.psc")));

        // Without `sources`, everything below the manifest is.
        std::fs::write(&manifest, "[format]\nindent-style = \"space\"\nindent-size = 8\n").unwrap();
        assert_eq!(size("other/A.psc"), eight);

        // A manifest of `[workspace]` only has no scripts.
        std::fs::write(&manifest, "[workspace]\nmembers = []\n").unwrap();
        assert_eq!(size("other/A.psc"), editor);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn workspace_values_are_used_as_in_cargo() {
        let dir = std::env::temp_dir().join(format!("papyrus-keys-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("App")).unwrap();
        std::fs::create_dir_all(dir.join("Alone")).unwrap();
        let w = |p: &str, text: &str| std::fs::write(dir.join(p), text).unwrap();
        w(
            "papyrus.toml",
            "[var]\nROOT = \"root\"\n[workspace]\nmembers = [\"App\"]\n[workspace.var]\nA = \"${ROOT}/a\"\n[workspace.compiler]\nargs = [\"-x\"]\n[workspace.build]\nimports = [\"${A}\"]\n",
        );
        let manifest = dir.join("App/papyrus.toml");
        let load = |text: &str| {
            std::fs::write(&manifest, text).unwrap();
            load_workspace(&manifest, &vars())
        };
        let errors = |text: &str| {
            let errors = load(text).unwrap_err();
            errors.into_iter().map(|e| e.message).collect::<Vec<_>>().join("\n")
        };

        // A variable of the workspace, expanded there; the others are not seen.
        let ws = load("[var]\nA.workspace = true\n[build]\noutput = \"${A}\"\n").unwrap();
        assert_eq!(ws.members[0].output, dir.join("App/root/a"));
        assert!(errors("[build]\noutput = \"${A}\"\n").contains("undefined variable `A`"));
        assert!(
            errors("[var]\nROOT.workspace = true\n")
                .contains("`workspace.var.ROOT` was not defined")
        );

        // A value of the member replaces the one of the workspace, not added to it.
        let ws = load("[compiler]\nargs.workspace = true\n").unwrap();
        assert_eq!(ws.members[0].extra_args, ["-x"]);
        let ws = load("[compiler]\nargs = [\"-y\"]\n").unwrap();
        assert_eq!(ws.members[0].extra_args, ["-y"]);
        let ws = load("[build]\nimports.workspace = true\n").unwrap();
        assert_eq!(ws.members[0].imports, [dir.join("root/a")]);
        let ws = load("[build]\nimports = [\"mine\"]\n").unwrap();
        assert_eq!(ws.members[0].imports, [dir.join("App/mine")]);

        // What cannot be inherited.
        assert!(
            errors("[build]\nflags.workspace = true\n")
                .contains("`workspace.build.flags` was not defined")
        );
        assert!(
            errors("[build]\nquiet.workspace = true\n")
                .contains("`workspace.build.quiet` was not defined")
        );
        assert!(
            errors("[build]\nimports.workspace = false\n").contains("`workspace` cannot be false")
        );
        // `sources` and `output` belong to each member.
        assert!(load("[build]\noutput.workspace = true\n").is_err());
        assert!(load("[build]\nsources.workspace = true\n").is_err());

        // Without workspace.
        let alone = dir.join("Alone/papyrus.toml");
        std::fs::write(&alone, "[build]\nimports.workspace = true\n").unwrap();
        let found = load_workspace(&alone, &vars()).unwrap_err();
        assert!(found[0].message.contains("failed to find a workspace root"), "{found:?}");

        // The hover of a variable.
        let text = "[var]\nA.workspace = true\n";
        let seen = manifest_variables(&manifest, text, None, &vars());
        assert_eq!(seen.manifest.get("A").map(String::as_str), Some("${ROOT}/a"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn workspace_tables_are_used_as_in_cargo() {
        let dir = std::env::temp_dir().join(format!("papyrus-inherit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("App")).unwrap();
        std::fs::create_dir_all(dir.join("Empty/App")).unwrap();
        let w = |p: &str, text: &str| std::fs::write(dir.join(p), text).unwrap();
        w(
            "papyrus.toml",
            "[workspace]\nmembers = [\"App\"]\n[workspace.lints]\nstyle = \"allow\"\n[workspace.format]\nindent-size = 2\n",
        );
        let manifest = dir.join("App/papyrus.toml");
        let messages = |errors: Vec<ManifestError>| {
            errors.into_iter().map(|e| e.message).collect::<Vec<_>>().join("\n")
        };
        let errors_of = |text: &str| {
            std::fs::write(&manifest, text).unwrap();
            let lints = load_lints(&manifest).err().map(messages);
            let format = load_format(&manifest).err().map(messages);
            let whole = load_workspace(&manifest, &vars()).err().map(messages);
            (lints, format, whole)
        };

        // Overrides.
        let (lints, _, whole) = errors_of("[lints]\nworkspace = true\nall = \"warn\"\n");
        let expected = "cannot override `workspace.lints` in `lints`";
        assert!(lints.unwrap().contains(expected));
        assert!(whole.unwrap().contains(expected));
        let (_, format, whole) = errors_of("[format]\nworkspace = true\nindent-size = 4\n");
        let expected = "cannot override `workspace.format` in `format`";
        assert!(format.unwrap().contains(expected));
        assert!(whole.unwrap().contains(expected));
        let (lints, _, _) = errors_of("[lints]\nworkspace = true\nexclude = [\"a\"]\n");
        assert!(lints.is_some());

        // `false`, even alone.
        let (lints, _, _) = errors_of("[lints]\nworkspace = false\n");
        assert_eq!(lints.as_deref(), Some("`workspace` cannot be false"));
        let (_, format, _) = errors_of("[format]\nworkspace = false\n");
        assert_eq!(format.as_deref(), Some("`workspace` cannot be false"));
        let (lints, _, _) = errors_of("[lints]\nworkspace = \"yes\"\n");
        assert!(lints.is_some());

        // Nothing to inherit: no such table, no workspace.
        w("Empty/papyrus.toml", "[workspace]\nmembers = [\"App\"]\n");
        let in_empty = dir.join("Empty/App/papyrus.toml");
        std::fs::write(&in_empty, "[lints]\nworkspace = true\n[format]\nworkspace = true\n")
            .unwrap();
        assert!(
            messages(load_lints(&in_empty).unwrap_err())
                .contains("`workspace.lints` was not defined")
        );
        assert!(
            messages(load_format(&in_empty).unwrap_err())
                .contains("`workspace.format` was not defined")
        );
        let whole = messages(load_workspace(&in_empty, &vars()).unwrap_err());
        assert!(whole.contains("`workspace.lints` was not defined"), "{whole}");
        assert!(whole.contains("`workspace.format` was not defined"), "{whole}");

        let alone_dir = dir.join("Alone");
        std::fs::create_dir_all(&alone_dir).unwrap();
        let alone = alone_dir.join("papyrus.toml");
        std::fs::write(&alone, "[lints]\nworkspace = true\n[format]\nworkspace = true\n").unwrap();
        let expected = "failed to find a workspace root";
        assert!(messages(load_lints(&alone).unwrap_err()).contains(expected));
        assert!(messages(load_format(&alone).unwrap_err()).contains(expected));
        assert!(messages(load_workspace(&alone, &vars()).unwrap_err()).contains(expected));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn excludes_of_the_table_in_use() {
        let dir = std::env::temp_dir().join(format!("papyrus-exclude-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("App/src")).unwrap();
        let w = |p: &str, text: &str| std::fs::write(dir.join(p), text).unwrap();
        w(
            "papyrus.toml",
            "[workspace]\nmembers = [\"App\"]\n[workspace.lints]\nexclude = [\"App/shared/**\"]\nall = \"warn\"\n[workspace.format]\nexclude = [\"App/shared/**\"]\n",
        );
        w(
            "App/papyrus.toml",
            "[package]\nname = \"App\"\n[lints]\nworkspace = true\n[format]\nworkspace = true\n",
        );

        let project = load_workspace(&dir.join("App/papyrus.toml"), &vars()).unwrap();
        let on_its_own = load_lints(&dir.join("App/papyrus.toml")).unwrap();
        for config in [&on_its_own, &project.members[0].lints] {
            assert!(!config.is_excluded(&dir.join("App/src/T.psc")));
            assert!(config.is_excluded(&dir.join("App/shared/T.psc")));
            // The other settings of the table still count.
            let level = config.level(papyrus_lint::find("unchecked-cast").unwrap());
            assert_eq!(level, papyrus_lint::Level::Warn);
        }

        assert!(!format_excludes(&dir.join("App/src/T.psc")));
        assert!(format_excludes(&dir.join("App/shared/T.psc")));
        let member = &project.members[0];
        assert!(member.format.exclude.matches(&dir.join("App/shared/T.psc")));

        // Its own table, without the ones of the workspace.
        w(
            "App/papyrus.toml",
            "[lints]\nexclude = [\"src/gen\"]\n[format]\nexclude = [\"src/gen\"]\n",
        );
        let alone = load_lints(&dir.join("App/papyrus.toml")).unwrap();
        assert!(alone.is_excluded(&dir.join("App/src/gen/T.psc")));
        assert!(!alone.is_excluded(&dir.join("App/shared/T.psc")));
        assert!(format_excludes(&dir.join("App/src/gen/T.psc")));
        assert!(!format_excludes(&dir.join("App/shared/T.psc")));

        // Mistakes are reported.
        w("App/papyrus.toml", "[lints]\nexclude = \"src\"\n");
        assert!(load_lints(&dir.join("App/papyrus.toml")).is_err());
        w("App/papyrus.toml", "[lints]\nexclude = [\"!src\"]\n");
        assert!(load_lints(&dir.join("App/papyrus.toml")).is_err());
        w("App/papyrus.toml", "[format]\nexclude = [\"!src\"]\n");
        assert!(load_format(&dir.join("App/papyrus.toml")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lint_levels() {
        let p = parse_str(
            "[lints]\nall = \"warn\"\ncomplexity = \"allow\"\nunused-local = { level = \"deny\" }\n",
        )
        .unwrap();
        let level = |code| p.lints.level(papyrus_lint::find(code).unwrap());
        assert_eq!(level("unused-local"), papyrus_lint::Level::Deny);
        assert_eq!(level("bool-literal-comparison"), papyrus_lint::Level::Allow);
        assert_eq!(level("unreachable-code"), papyrus_lint::Level::Warn);

        let errors = parse_str("[lints]\nbogus = \"deny\"\nstyle = \"loud\"\n").unwrap_err();
        let messages: Vec<&str> = errors.iter().map(|e| e.message.as_str()).collect();
        assert_eq!(messages.len(), 2, "{messages:?}");
        assert!(messages[0].contains("bogus") && messages[1].contains("loud"));
    }

    #[test]
    fn without_manifest() {
        let mut p =
            Project::without_manifest(PathBuf::from("/mod"), vec![PathBuf::from("/game")], None);
        p.builtin_compiler = Some((PathBuf::from("pa"), Vec::new()));
        let output = |script: &str| s(&p.compile_file(Path::new(script)).unwrap().output);
        assert_eq!(
            output("/mod/Scripts/Source/A.psc"),
            "/mod/build/papyrus/compile/Scripts/Source"
        );
        assert_eq!(output("/mod/B.psc"), "/mod/build/papyrus/compile");
    }

    #[test]
    fn compile_all_walks_sources() {
        let dir = std::env::temp_dir().join(format!("papyrus-project-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src/sub")).unwrap();
        for f in ["src/A.psc", "src/sub/B.psc", "src/readme.txt"] {
            std::fs::write(dir.join(f), "").unwrap();
        }
        let text =
            "[compiler]\npath = \"c.exe\"\n[build]\nsources = [\"src\"]\nflags = \"f.flg\"\n";
        let p = parse(&dir.join("papyrus.toml"), text, None, &vars()).unwrap();
        let commands = p.compile_all().unwrap();
        let outputs: Vec<String> =
            commands.iter().map(|c| s(c.output.strip_prefix(&dir).unwrap())).collect();
        assert_eq!(outputs, ["output", "output/sub"]);
        // A script may use scripts of sibling directories.
        let imports = commands[0].args[2].replace('\\', "/");
        assert!(imports.contains("/src/sub") && imports.contains("/src;"), "{imports}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
