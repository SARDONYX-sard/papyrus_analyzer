//! Protocol extensions specific to this server.

use lsp_types::{TextDocumentIdentifier, request::Request};
use serde::{Deserialize, Serialize};

/// `papyrus/restart`: soft-restarts the server.
///
/// Drops all indexes, applies the optional settings, re-indexes the workspace
/// and republishes diagnostics. Open documents are kept. Returns the status
/// after the restart was initiated (indexing runs in the background).
pub(crate) enum Restart {}

impl Request for Restart {
    type Params = Option<RestartParams>;
    type Result = StatusResult;
    const METHOD: &'static str = "papyrus/restart";
}

/// Parameters of [`Restart`].
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RestartParams {
    /// New settings, in the same format as `initializationOptions`.
    pub(crate) settings: Option<serde_json::Value>,
}

/// `papyrus/statusChanged`: the projects changed (papyrus.toml created,
/// edited or deleted); the client asks for [`Status`] again.
pub(crate) enum StatusChanged {}

impl lsp_types::notification::Notification for StatusChanged {
    type Params = ();
    const METHOD: &'static str = "papyrus/statusChanged";
}

/// `papyrus/status`: what the server currently knows.
pub(crate) enum Status {}

impl Request for Status {
    type Params = ();
    type Result = StatusResult;
    const METHOD: &'static str = "papyrus/status";
}

/// Result of [`Status`] and [`Restart`].
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StatusResult {
    /// Number of known script files.
    pub(crate) files: usize,
    /// Number of files open in the editor.
    pub(crate) open_files: usize,
    /// Whether the workspace is being indexed.
    pub(crate) indexing: bool,
    /// The check of the scripts not open, while it runs.
    pub(crate) checking: Option<CheckStatus>,
    /// Incremented on every restart.
    pub(crate) generation: u64,
    /// Workspace folders.
    pub(crate) roots: Vec<String>,
    /// Import directories, from `papyrus.toml` and the settings.
    pub(crate) import_directories: Vec<String>,
    /// The `papyrus.toml` of the workspace, if any.
    pub(crate) manifest: Option<String>,
    /// Why `papyrus.toml` is invalid.
    pub(crate) manifest_error: Option<String>,
    /// The flags file, `None` for the built-in Skyrim flags.
    pub(crate) flags_file: Option<String>,
    /// `error`, `warning`, `limited` (no `papyrus.toml`) or `ok`.
    pub(crate) health: String,
    /// Problems behind `health`.
    pub(crate) problems: Vec<String>,
    /// `[build] sources` of `papyrus.toml`.
    pub(crate) sources: Vec<String>,
    /// `[test] sources` of `papyrus.toml`.
    pub(crate) test_sources: Vec<String>,
    /// Where compiled (`compile`) and decompiled (`decompile`) scripts go,
    /// per workspace folder: `[build] output` of `papyrus.toml`, or the
    /// default build directory.
    pub(crate) build_dirs: Vec<BuildDirs>,
    /// Known `${NAME}` variables: `"value (source)"`.
    pub(crate) variables: std::collections::BTreeMap<String, String>,
}

/// `papyrus/compileCommands`: the compiler invocations described by
/// `papyrus.toml`. The client runs them, the server does not.
pub(crate) enum CompileCommands {}

impl Request for CompileCommands {
    type Params = CompileCommandsParams;
    type Result = CompileCommandsResult;
    const METHOD: &'static str = "papyrus/compileCommands";
}

/// Parameters of [`CompileCommands`].
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CompileCommandsParams {
    /// The script to compile; ignored when `all` is set.
    pub(crate) text_document: Option<TextDocumentIdentifier>,
    /// Several scripts to compile; ignored when `all` is set.
    #[serde(default)]
    pub(crate) files: Vec<TextDocumentIdentifier>,
    /// Compile every script of `[build] sources`.
    #[serde(default)]
    pub(crate) all: bool,
}

/// Result of [`CompileCommands`].
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CompileCommandsResult {
    /// The manifest the commands come from.
    pub(crate) manifest: String,
    /// The output directory.
    pub(crate) output: String,
    /// The commands, to run in order.
    pub(crate) commands: Vec<CompileCommand>,
}

/// One compiler invocation.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CompileCommand {
    /// The executable.
    pub(crate) program: String,
    /// Its arguments, unquoted.
    pub(crate) args: Vec<String>,
    /// The working directory.
    pub(crate) cwd: String,
    /// The output directory of this invocation; the client creates it.
    pub(crate) output: String,
    /// The whole command, quoted for display.
    pub(crate) command_line: String,
    /// The scripts it compiles: one for an external compiler, several for the
    /// built-in one, which then reports each failure as
    /// `No output generated for <script>, compilation failed.`
    pub(crate) scripts: Vec<String>,
}

/// `papyrus/manifestTemplate`: a `papyrus.toml` to start from.
pub(crate) enum ManifestTemplate {}

impl Request for ManifestTemplate {
    type Params = ();
    type Result = String;
    const METHOD: &'static str = "papyrus/manifestTemplate";
}

/// `papyrus/syntaxTree`: a debug dump of the syntax tree of a document.
pub(crate) enum SyntaxTree {}

impl Request for SyntaxTree {
    type Params = SyntaxTreeParams;
    type Result = String;
    const METHOD: &'static str = "papyrus/syntaxTree";
}

/// Parameters of [`SyntaxTree`].
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SyntaxTreeParams {
    /// The document.
    pub(crate) text_document: TextDocumentIdentifier,
}

/// `papyrus/decompile`: the source of a `.pex` file, or its assembly.
pub(crate) enum Decompile {}

impl Request for Decompile {
    type Params = DecompileParams;
    type Result = String;
    const METHOD: &'static str = "papyrus/decompile";
}

/// Parameters of [`Decompile`].
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DecompileParams {
    /// The `.pex` file.
    pub(crate) uri: lsp_types::Uri,
    /// The assembly (`.pas`) instead of the source.
    #[serde(default)]
    pub(crate) assembly: bool,
}

/// `papyrus/decompileFiles`: writes the source of `.pex` files, keeping their
/// path relative to their workspace folder.
pub(crate) enum DecompileFiles {}

impl Request for DecompileFiles {
    type Params = DecompileFilesParams;
    type Result = DecompileFilesResult;
    const METHOD: &'static str = "papyrus/decompileFiles";
}

/// Parameters of [`DecompileFiles`].
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DecompileFilesParams {
    /// The `.pex` files.
    pub(crate) files: Vec<lsp_types::Uri>,
}

/// Result of [`DecompileFiles`].
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DecompileFilesResult {
    /// The output directory: `decompile` in the output directory of
    /// `papyrus.toml`, else `build/papyrus/decompile` of the workspace folder.
    pub(crate) output: String,
    /// For each file, in order: the written `.psc`, or the error.
    pub(crate) results: Vec<DecompileFileResult>,
}

/// One file of [`DecompileFilesResult`].
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DecompileFileResult {
    /// The `.pex` file.
    pub(crate) file: lsp_types::Uri,
    /// The written source.
    pub(crate) psc: Option<String>,
    /// Why it failed.
    pub(crate) error: Option<String>,
}

/// The output directories of a workspace folder, see [`StatusResult`].
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BuildDirs {
    /// The workspace folder.
    pub(crate) root: String,
    /// The `.pex` files.
    pub(crate) compile: String,
    /// The decompiled `.psc` files.
    pub(crate) decompile: String,
    /// `[compiler] path`, `None` for the built-in compiler.
    pub(crate) compiler: Option<String>,
}

/// The progress of the check of the closed scripts, see [`StatusResult`].
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CheckStatus {
    /// Scripts checked.
    pub(crate) done: usize,
    /// Scripts to check.
    pub(crate) total: usize,
    /// The last checked script.
    pub(crate) current: String,
    /// The estimated time left, once known.
    pub(crate) remaining_ms: Option<u64>,
}
