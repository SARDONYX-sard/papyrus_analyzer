//! The server state and main loop.
//!
//! Everything runs on the main thread except workspace indexing, which happens
//! on a background thread and reports back through a channel. Requests are
//! answered synchronously: queries on a single file take microseconds to a few
//! milliseconds.
//!
//! # Restart
//!
//! An editor extension cannot restart itself once activated, so the server
//! implements the custom `papyrus/restart` request: it drops every cached index,
//! re-reads the settings sent with the request, re-indexes the workspace and
//! republishes diagnostics, while keeping the documents currently open in the
//! editor (the client does not send them again).

use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

use crossbeam_channel::{Receiver, Sender, select};
use lsp_server::{Connection, ErrorCode, Message, Notification, Request, RequestId, Response};
use lsp_types::{
    ClientCapabilities, InitializeParams, NumberOrString, ProgressParams, ProgressParamsValue,
    PublishDiagnosticsParams, Uri, WorkDoneProgress, WorkDoneProgressBegin,
    WorkDoneProgressCreateParams, WorkDoneProgressEnd, WorkDoneProgressReport, notification,
    request,
};
use papyrus_ide::{FileId, Origin, ScriptIndex, Workspace};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;

use crate::{
    config::{CliOptions, Config},
    conv::{self, Lines},
};

/// Messages from the indexing thread.
pub(crate) enum IndexEvent {
    /// `done` of `total` files are indexed.
    Progress { generation: u64, done: usize, total: usize },
    /// Indexing finished. `flag_files` are the `.flg` files found on the way.
    Done {
        generation: u64,
        files: Vec<(PathBuf, ScriptIndex, Origin)>,
        flag_files: Vec<PathBuf>,
        elapsed_ms: u128,
    },
}

/// A document open in the editor that is not a script: the flags file or the
/// project manifest.
pub(crate) struct OtherDoc {
    pub(crate) text: String,
    pub(crate) version: i32,
}

/// The language server.
pub(crate) struct Server {
    sender: Sender<Message>,
    pub(crate) ws: Workspace,
    pub(crate) config: Config,
    cli: CliOptions,
    pub(crate) roots: Vec<PathBuf>,
    pub(crate) caps: ClientCapabilities,
    /// Formatting is registered for scripts only, see [`crate::dynamic_formatting`].
    dynamic_formatting: bool,
    /// The first project of `members`.
    pub(crate) project: Option<papyrus_project::Project>,
    /// The projects of the loaded `papyrus.toml`: its members for a
    /// `[workspace]`, else the project alone.
    pub(crate) members: Vec<papyrus_project::Project>,
    /// The `papyrus.toml` found in the workspace (the root of its
    /// `[workspace]`), valid or not.
    pub(crate) manifest: Option<PathBuf>,
    /// The workspace folders without manifest, when another one has one: all
    /// of them work like `limited` mode.
    pub(crate) limited_roots: Vec<PathBuf>,
    /// A script outside of the sources of a project was opened: the rest of its
    /// workspace folders is known too, for it.
    pub(crate) index_outside: bool,
    /// The manifests whose problems are published.
    pub(crate) manifest_files: Vec<PathBuf>,
    /// Why `manifest` could not be loaded.
    pub(crate) manifest_error: Option<String>,
    /// Open `.flg` and `papyrus.toml` documents, by path.
    pub(crate) other_docs: rustc_hash::FxHashMap<PathBuf, OtherDoc>,
    /// Incremented on every (re)index; results of older runs are dropped.
    generation: u64,
    indexing: bool,
    index_tx: Sender<IndexEvent>,
    index_rx: Receiver<IndexEvent>,
    next_request_id: i32,
    /// The job of the running full check of the closed files; a newer full
    /// check stops it.
    full_check: Arc<std::sync::atomic::AtomicU64>,
    /// The id of the next check job (full or of a few files).
    next_check_job: u64,
    /// The job whose diagnostics each closed file shows: results of older
    /// jobs are dropped.
    checked_job: rustc_hash::FxHashMap<FileId, u64>,
    check_tx: Sender<CheckEvent>,
    check_rx: Receiver<CheckEvent>,
    /// Closed files whose diagnostics are published, to clear them when they
    /// go away.
    published: rustc_hash::FxHashSet<PathBuf>,
    /// The running check of the closed files.
    checking: Option<CheckProgress>,
    /// The threads checking closed files: not all cores, so that the editor
    /// keeps responding during a full check.
    check_pool: Arc<rayon::ThreadPool>,
    /// Set while a message of the editor is handled: the background check
    /// waits, leaving the cores to it.
    check_paused: Arc<std::sync::atomic::AtomicBool>,
    /// Diagnostics of closed files waiting to be published, in order; see
    /// [`Server::flush_publishes`].
    pending: rustc_hash::FxHashMap<PathBuf, Vec<lsp_types::Diagnostic>>,
    pending_order: std::collections::VecDeque<PathBuf>,
    flush_tick: Receiver<Instant>,
}

/// At most this many diagnostics of closed files are published per
/// [`PUBLISH_INTERVAL`]: a burst of thousands of notifications keeps the
/// editor busy, and its answers to hover, completion, ... wait behind them.
const PUBLISH_BATCH: usize = 100;
const PUBLISH_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);

/// What the background check of the closed files sends.
enum CheckEvent {
    /// The diagnostics of one file (`None`: not checked, e.g. it does not
    /// mention the changed scripts).
    File { job: u64, file: FileId, path: PathBuf, diagnostics: Option<Vec<lsp_types::Diagnostic>> },
    /// Every file of the job is checked.
    Done { job: u64 },
}

/// The progress of the full check of the closed files, for `papyrus/status`.
struct CheckProgress {
    job: u64,
    done: usize,
    total: usize,
    /// The last checked file.
    current: String,
    started: Instant,
}

impl CheckProgress {
    /// The estimated time left, from the speed so far.
    fn remaining(&self) -> Option<std::time::Duration> {
        (self.done > 0).then(|| {
            self.started
                .elapsed()
                .mul_f64((self.total - self.done.min(self.total)) as f64 / self.done as f64)
        })
    }
}

type HandlerResult = Result<Value, (ErrorCode, String)>;

impl Server {
    pub(crate) fn new(sender: Sender<Message>, init: &InitializeParams, cli: CliOptions) -> Server {
        let mut roots: Vec<PathBuf> = init
            .workspace_folders
            .iter()
            .flatten()
            .filter_map(|f| conv::uri_to_path(&f.uri))
            .collect();
        #[allow(deprecated)]
        if roots.is_empty() {
            roots.extend(init.root_uri.as_ref().and_then(conv::uri_to_path));
        }
        let (index_tx, index_rx) = crossbeam_channel::unbounded();
        let (check_tx, check_rx) = crossbeam_channel::unbounded();
        Server {
            sender,
            ws: Workspace::new(),
            config: Config::from_json(init.initialization_options.clone()).with_cli(&cli),
            cli,
            roots,
            caps: init.capabilities.clone(),
            dynamic_formatting: crate::dynamic_formatting(init),
            project: None,
            members: Vec::new(),
            manifest: None,
            limited_roots: Vec::new(),
            index_outside: false,
            manifest_files: Vec::new(),
            manifest_error: None,
            other_docs: Default::default(),
            generation: 0,
            indexing: false,
            index_tx,
            index_rx,
            next_request_id: 0,
            full_check: Arc::default(),
            next_check_job: 0,
            checked_job: Default::default(),
            check_tx,
            check_rx,
            published: Default::default(),
            checking: None,
            check_pool: Arc::new(check_pool()),
            check_paused: Arc::default(),
            pending: Default::default(),
            pending_order: Default::default(),
            flush_tick: crossbeam_channel::tick(PUBLISH_INTERVAL),
        }
    }

    /// Runs until the client asks for shutdown.
    pub(crate) fn run(mut self, connection: &Connection) -> anyhow::Result<()> {
        if self.dynamic_formatting {
            // Scripts only: the other documents (papyrus.toml) keep the
            // formatters of their language.
            self.request_client::<request::RegisterCapability>(lsp_types::RegistrationParams {
                registrations: vec![lsp_types::Registration {
                    id: "papyrus-formatting".to_owned(),
                    method: "textDocument/formatting".to_owned(),
                    register_options: Some(serde_json::json!({
                        "documentSelector": [{ "language": "papyrus" }],
                    })),
                }],
            });
        }
        self.load_project();
        self.start_indexing();
        loop {
            // The editor first: hover, completion, ... are answered before
            // any further result of the background check is handled.
            match connection.receiver.try_recv() {
                Ok(msg) => {
                    if self.on_message(connection, msg)? {
                        return Ok(());
                    }
                    continue;
                }
                Err(crossbeam_channel::TryRecvError::Disconnected) => return Ok(()),
                Err(crossbeam_channel::TryRecvError::Empty) => {}
            }
            select! {
                recv(connection.receiver) -> msg => {
                    let Ok(msg) = msg else { return Ok(()) };
                    if self.on_message(connection, msg)? {
                        return Ok(());
                    }
                }
                recv(self.index_rx) -> event => {
                    if let Ok(event) = event {
                        self.on_index_event(event);
                    }
                }
                recv(self.check_rx) -> event => {
                    if let Ok(event) = event {
                        self.on_check_event(event);
                    }
                }
                recv(self.flush_tick) -> _ => self.flush_publishes(),
            }
        }
    }

    /// Handles a message of the editor; `true` once it asked for shutdown.
    fn on_message(&mut self, connection: &Connection, msg: Message) -> anyhow::Result<bool> {
        use std::sync::atomic::Ordering;

        /// Resumes the background check when dropped.
        struct Resume(Arc<std::sync::atomic::AtomicBool>);
        impl Drop for Resume {
            fn drop(&mut self) {
                self.0.store(false, Ordering::SeqCst);
            }
        }
        // The background check waits meanwhile.
        self.check_paused.store(true, Ordering::SeqCst);
        let _resume = Resume(self.check_paused.clone());
        match msg {
            Message::Request(req) => {
                if connection.handle_shutdown(&req)? {
                    return Ok(true);
                }
                self.on_request(req);
            }
            Message::Notification(not) => self.on_notification(not),
            Message::Response(_) => {}
        }
        Ok(false)
    }

    // === Messaging ===

    pub(crate) fn send(&self, msg: impl Into<Message>) {
        if self.sender.send(msg.into()).is_err() {
            log::error!("client connection closed");
        }
    }

    pub(crate) fn notify<N: notification::Notification>(&self, params: N::Params) {
        self.send(Notification::new(N::METHOD.to_owned(), params));
    }

    pub(crate) fn request_client<R: request::Request>(&mut self, params: R::Params) {
        self.next_request_id += 1;
        let id = RequestId::from(format!("papyrus-{}", self.next_request_id));
        self.send(Request::new(id, R::METHOD.to_owned(), params));
    }

    // === Requests ===

    fn on_request(&mut self, req: Request) {
        let Request { id, method, params } = req;
        let start = Instant::now();
        let result = catch_unwind(AssertUnwindSafe(|| self.dispatch(&method, params)))
            .unwrap_or_else(|panic| {
                let msg = panic
                    .downcast_ref::<String>()
                    .map(String::as_str)
                    .or_else(|| panic.downcast_ref::<&str>().copied())
                    .unwrap_or("unknown panic");
                log::error!("request `{method}` panicked: {msg}");
                Err((ErrorCode::InternalError, format!("internal error: {msg}")))
            });
        log::debug!("{method} took {:?}", start.elapsed());
        let response = match result {
            Ok(value) => Response::new_ok(id, value),
            Err((code, message)) => Response::new_err(id, code as i32, message),
        };
        self.send(response);
    }

    fn dispatch(&mut self, method: &str, params: Value) -> HandlerResult {
        use lsp_types::request::*;
        match method {
            HoverRequest::METHOD => handle::<HoverRequest>(self, params, Server::hover),
            request::PrepareRenameRequest::METHOD => {
                let params = serde_json::from_value(params)
                    .map_err(|e| (ErrorCode::InvalidParams, format!("invalid params: {e}")))?;
                let result =
                    self.prepare_rename(params).map_err(|m| (ErrorCode::RequestFailed, m))?;
                serde_json::to_value(result).map_err(|e| (ErrorCode::InternalError, e.to_string()))
            }
            request::Rename::METHOD => {
                let params = serde_json::from_value(params)
                    .map_err(|e| (ErrorCode::InvalidParams, format!("invalid params: {e}")))?;
                let result = self.rename(params).map_err(|m| (ErrorCode::RequestFailed, m))?;
                serde_json::to_value(result).map_err(|e| (ErrorCode::InternalError, e.to_string()))
            }
            request::CodeActionRequest::METHOD => {
                handle::<request::CodeActionRequest>(self, params, Server::code_actions)
            }
            request::GotoDeclaration::METHOD => {
                handle::<request::GotoDeclaration>(self, params, Server::goto_declaration)
            }
            GotoDefinition::METHOD => {
                handle::<GotoDefinition>(self, params, Server::goto_definition)
            }
            References::METHOD => handle::<References>(self, params, Server::references),
            DocumentHighlightRequest::METHOD => {
                handle::<DocumentHighlightRequest>(self, params, Server::document_highlight)
            }
            Completion::METHOD => handle::<Completion>(self, params, Server::completion),
            ResolveCompletionItem::METHOD => {
                handle::<ResolveCompletionItem>(self, params, Server::resolve_completion)
            }
            SignatureHelpRequest::METHOD => {
                handle::<SignatureHelpRequest>(self, params, Server::signature_help)
            }
            DocumentSymbolRequest::METHOD => {
                handle::<DocumentSymbolRequest>(self, params, Server::document_symbols)
            }
            WorkspaceSymbolRequest::METHOD => {
                handle::<WorkspaceSymbolRequest>(self, params, Server::workspace_symbols)
            }
            CodeLensRequest::METHOD => handle::<CodeLensRequest>(self, params, Server::code_lens),
            Formatting::METHOD => handle::<Formatting>(self, params, Server::formatting),
            InlayHintRequest::METHOD => {
                handle::<InlayHintRequest>(self, params, Server::inlay_hints)
            }
            FoldingRangeRequest::METHOD => {
                handle::<FoldingRangeRequest>(self, params, Server::folding_ranges)
            }
            SemanticTokensFullRequest::METHOD => {
                handle::<SemanticTokensFullRequest>(self, params, Server::semantic_tokens_full)
            }
            SemanticTokensRangeRequest::METHOD => {
                handle::<SemanticTokensRangeRequest>(self, params, Server::semantic_tokens_range)
            }
            crate::ext::Restart::METHOD => {
                handle::<crate::ext::Restart>(self, params, Server::restart)
            }
            crate::ext::SyntaxTree::METHOD => {
                handle::<crate::ext::SyntaxTree>(self, params, Server::syntax_tree)
            }
            crate::ext::Status::METHOD => {
                handle::<crate::ext::Status>(self, params, Server::status)
            }
            crate::ext::ManifestTemplate::METHOD => {
                handle::<crate::ext::ManifestTemplate>(self, params, |_, ()| {
                    papyrus_project::template(env!("CARGO_PKG_VERSION"))
                })
            }
            crate::ext::Decompile::METHOD => {
                let params = serde_json::from_value(params)
                    .map_err(|e| (ErrorCode::InvalidParams, format!("invalid params: {e}")))?;
                let text = self.decompile(params).map_err(|m| (ErrorCode::RequestFailed, m))?;
                Ok(Value::String(text))
            }
            crate::ext::DecompileFiles::METHOD => {
                handle::<crate::ext::DecompileFiles>(self, params, Server::decompile_files)
            }
            crate::ext::CompileCommands::METHOD => {
                let params = serde_json::from_value(params)
                    .map_err(|e| (ErrorCode::InvalidParams, format!("invalid params: {e}")))?;
                let result = self
                    .compile_commands(params)
                    .map_err(|message| (ErrorCode::RequestFailed, message))?;
                serde_json::to_value(result).map_err(|e| (ErrorCode::InternalError, e.to_string()))
            }
            _ => Err((ErrorCode::MethodNotFound, format!("unknown request `{method}`"))),
        }
    }

    // === Notifications ===

    fn on_notification(&mut self, not: Notification) {
        use lsp_types::notification::*;
        let method = not.method.clone();
        let result = catch_unwind(AssertUnwindSafe(|| match not.method.as_str() {
            DidOpenTextDocument::METHOD => {
                notify::<DidOpenTextDocument>(not, |p| self.did_open(p));
            }
            DidChangeTextDocument::METHOD => {
                notify::<DidChangeTextDocument>(not, |p| self.did_change(p));
            }
            DidCloseTextDocument::METHOD => {
                notify::<DidCloseTextDocument>(not, |p| self.did_close(p));
            }
            DidSaveTextDocument::METHOD => {
                notify::<DidSaveTextDocument>(not, |p| self.did_save(p));
            }
            DidChangeWatchedFiles::METHOD => {
                notify::<DidChangeWatchedFiles>(not, |p| self.did_change_watched_files(p));
            }
            DidChangeConfiguration::METHOD => {
                notify::<DidChangeConfiguration>(not, |p| {
                    let config = Config::from_json(Some(p.settings)).with_cli(&self.cli);
                    self.set_config(config);
                });
            }
            _ => {}
        }));
        if result.is_err() {
            log::error!("notification `{method}` panicked");
        }
    }

    fn did_open(&mut self, params: lsp_types::DidOpenTextDocumentParams) {
        let doc = params.text_document;
        let Some(path) = conv::uri_to_path(&doc.uri) else { return };
        if !is_script(&path) {
            self.other_docs.insert(path.clone(), OtherDoc { text: doc.text, version: doc.version });
            self.other_doc_changed(&path);
            return;
        }
        let origin = self.origin_of(&path);
        // Of a folder with a manifest: the other scripts of no project are read now.
        let starts_outside = origin == Origin::Outside
            && !self.index_outside
            && self.roots.iter().any(|r| path.starts_with(r) && !self.limited_roots.contains(r));
        let file = self.ws.set_open(path, Arc::from(doc.text), doc.version);
        if self.ws.file(file).origin != Origin::Builtin {
            self.ws.set_origin(file, origin);
        }
        if starts_outside {
            self.index_outside = true;
            self.start_indexing();
        }
        self.publish_open_diagnostics();
    }

    fn did_change(&mut self, params: lsp_types::DidChangeTextDocumentParams) {
        let Some(path) = conv::uri_to_path(&params.text_document.uri) else { return };
        let version = params.text_document.version;
        if let Some(doc) = self.other_docs.get_mut(&path) {
            apply_changes(&mut doc.text, params.content_changes);
            doc.version = version;
            self.other_doc_changed(&path);
            return;
        }
        let Some(file) = self.ws.file_id(&path) else { return };
        let Some(doc) = &self.ws.file(file).open else { return };

        let mut text = doc.text.to_string();
        apply_changes(&mut text, params.content_changes);
        self.ws.set_open(path, Arc::from(text), version);
        self.publish_open_diagnostics();
    }

    fn did_save(&mut self, params: lsp_types::DidSaveTextDocumentParams) {
        let Some(path) = conv::uri_to_path(&params.text_document.uri) else { return };
        if is_manifest(&path) {
            self.reload_project();
        } else if is_script(&path) {
            // The scripts using the saved one may have new problems.
            let name = self.ws.file_id(&path).and_then(|f| self.ws.file(f).index.name);
            self.check_mentioning(name.into_iter().map(|n| n.to_string()).collect());
        }
    }

    fn did_close(&mut self, params: lsp_types::DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        let path = conv::uri_to_path(&uri);
        if let Some(path) = &path
            && self.other_docs.remove(path).is_some()
        {
            if self.ws.is_flags_file(path) {
                self.ws.reload_flags(path);
            }
            if is_manifest(path) {
                // Keep showing problems of the file on disk.
                self.reload_project();
                return;
            }
        } else if let Some(path) = &path
            && let Some(file) = self.ws.file_id(path)
        {
            self.ws.close(file);
            // A script of no directory of the project was known for being open;
            // the built-in ones stay.
            if !self.index_dirs().iter().any(|(d, _)| path.starts_with(d))
                && self.ws.file_id(path) == Some(file)
                && self.ws.file(file).origin != Origin::Builtin
            {
                self.ws.remove(file);
            }
            // A source of the project keeps its diagnostics, of the disk text
            // (closing forgets files deleted from the disk).
            if self.ws.file_id(path) == Some(file)
                && self.is_checked(file, &self.build_output_dirs())
                && let Some(diagnostics) = self.file_diagnostics(file)
            {
                self.published.insert(path.clone());
                self.notify::<notification::PublishDiagnostics>(PublishDiagnosticsParams {
                    uri,
                    diagnostics,
                    version: None,
                });
                return;
            }
        }
        self.notify::<notification::PublishDiagnostics>(PublishDiagnosticsParams {
            uri,
            diagnostics: Vec::new(),
            version: None,
        });
        self.publish_open_diagnostics();
    }

    fn did_change_watched_files(&mut self, params: lsp_types::DidChangeWatchedFilesParams) {
        // The scripts that changed, by name: the scripts using them are
        // checked again.
        let mut changed: Vec<String> = Vec::new();
        let build_dirs = self.build_output_dirs();
        let indexed_dirs = self.index_dirs();
        for change in params.changes {
            let Some(path) = conv::uri_to_path(&change.uri) else { continue };
            // Compiled and decompiled scripts are not sources.
            if build_dirs.iter().any(|d| path.starts_with(d)) {
                continue;
            }
            if is_manifest(&path) {
                if !self.other_docs.contains_key(&path) {
                    self.reload_project();
                }
                continue;
            }
            if is_flags(&path) {
                if self.ws.is_flags_file(&path) && !self.other_docs.contains_key(&path) {
                    self.ws.reload_flags(&path);
                    self.publish_flags_diagnostics();
                }
                continue;
            }
            if !is_script(&path) {
                continue;
            }
            // Scripts of no directory of the project stay unknown.
            let existing = self.ws.file_id(&path);
            if existing.is_none() && !indexed_dirs.iter().any(|(d, _)| path.starts_with(d)) {
                continue;
            }
            if existing.is_some_and(|f| self.ws.file(f).open.is_some()) {
                // The editor contents are authoritative.
                continue;
            }
            changed
                .extend(existing.and_then(|f| self.ws.file(f).index.name).map(|n| n.to_string()));
            match papyrus_ide::read_file(&path) {
                Some(text) => {
                    let origin = self.origin_of(&path);
                    let index = papyrus_ide::index_text(&text);
                    changed.extend(index.name.map(|n| n.to_string()));
                    self.ws.set_disk_index(path, index, origin);
                }
                None => {
                    if let Some(file) = existing {
                        self.ws.remove(file);
                    }
                }
            }
        }
        self.publish_open_diagnostics();
        if !changed.is_empty() {
            self.check_mentioning(changed);
        }
    }

    fn set_config(&mut self, config: Config) {
        if config == self.config {
            return;
        }
        let reload = config.import_directories != self.config.import_directories
            || config.flags_file != self.config.flags_file
            || config.var != self.config.var;
        let refresh_hints = config.inlay_hints != self.config.inlay_hints;
        self.config = config;
        if refresh_hints
            && self
                .caps
                .workspace
                .as_ref()
                .and_then(|w| w.inlay_hint.as_ref())
                .and_then(|i| i.refresh_support)
                .unwrap_or(false)
        {
            self.request_client::<request::InlayHintRefreshRequest>(());
        }
        if reload {
            // Variables may change the paths of papyrus.toml too.
            self.load_project();
            self.reset_workspace();
            self.start_indexing();
        }
        self.refresh_diagnostics();
    }

    fn origin_of(&self, path: &Path) -> Origin {
        let in_root = self.roots.iter().any(|r| path.starts_with(r));
        let in_tests =
            || self.members.iter().flat_map(|m| &m.test_sources).any(|d| path.starts_with(d));
        match self.import_dirs().iter().position(|d| path.starts_with(d)) {
            Some(i) if !in_root => Origin::Import(i as u16),
            None if !self.members.is_empty() && !in_tests() => Origin::Outside,
            _ => Origin::Workspace,
        }
    }

    // === Restart ===

    /// Drops all indexes, keeping the open documents.
    pub(crate) fn reset_workspace(&mut self) {
        let open: Vec<(PathBuf, Arc<str>, i32)> = self
            .ws
            .open_files()
            .filter_map(|(_, f)| {
                f.open.as_ref().map(|d| (f.path.clone(), d.text.clone(), d.version))
            })
            .collect();
        self.ws = Workspace::new();
        let scopes = self.scopes();
        self.ws.set_scopes(&scopes);
        self.install_builtins();
        for (path, text, version) in open {
            let origin = self.origin_of(&path);
            let file = self.ws.set_open(path, text, version);
            if self.ws.file(file).origin != Origin::Builtin {
                self.ws.set_origin(file, origin);
            }
        }
        let open_flags: Vec<(PathBuf, String)> = self
            .other_docs
            .iter()
            .filter(|(p, _)| is_flags(p))
            .map(|(p, d)| (p.clone(), d.text.clone()))
            .collect();
        for (path, text) in open_flags {
            self.ws.set_flags_open(path, Arc::from(text));
        }
    }

    /// Handles `papyrus/restart`.
    fn restart(&mut self, params: Option<crate::ext::RestartParams>) -> crate::ext::StatusResult {
        log::info!("restarting the server");
        if let Some(settings) = params.and_then(|p| p.settings) {
            self.config = Config::from_json(Some(settings)).with_cli(&self.cli);
        }
        self.load_project();
        self.reset_workspace();
        self.start_indexing();
        self.publish_open_diagnostics();
        self.refresh_code_lenses();
        self.status(())
    }

    fn status(&mut self, _: ()) -> crate::ext::StatusResult {
        let (health, problems) = self.health();
        crate::ext::StatusResult {
            health: health.to_owned(),
            problems,
            files: self.ws.len(),
            open_files: self.ws.open_files().count(),
            indexing: self.indexing,
            checking: self.check_status(),
            generation: self.generation,
            roots: self.roots.iter().map(|p| p.display().to_string()).collect(),
            import_directories: self
                .import_dirs()
                .iter()
                .map(|p| p.display().to_string())
                .collect(),
            manifest: self.manifest.as_ref().map(|p| p.display().to_string()),
            manifest_error: self.manifest_error.clone(),
            flags_file: self
                .project
                .as_ref()
                .and_then(|p| p.flags.as_ref())
                .or(self.ws.flags().path.as_ref())
                .map(|p| p.display().to_string()),
            variables: self.variables_report(),
            build_dirs: self.build_dirs(),
            sources: self
                .members
                .iter()
                .flat_map(|p| &p.sources)
                .map(|p| p.display().to_string())
                .collect(),
            test_sources: self
                .members
                .iter()
                .flat_map(|p| &p.test_sources)
                .map(|p| p.display().to_string())
                .collect(),
        }
    }

    // === Indexing ===

    pub(crate) fn start_indexing(&mut self) {
        self.generation += 1;
        self.indexing = true;
        let generation = self.generation;
        let dirs = self.index_dirs();
        // Next to the manifests too, outside of the sources.
        let flag_dirs: Vec<PathBuf> = self.members.iter().map(|m| m.root.clone()).collect();
        for (dir, _) in &dirs {
            if !dir.is_dir() {
                log::warn!("not a directory: {}", dir.display());
            }
        }

        if self.progress_supported() {
            let token = progress_token(generation);
            self.request_client::<request::WorkDoneProgressCreate>(WorkDoneProgressCreateParams {
                token: token.clone(),
            });
            self.notify::<notification::Progress>(ProgressParams {
                token,
                value: ProgressParamsValue::WorkDone(WorkDoneProgress::Begin(
                    WorkDoneProgressBegin {
                        title: "Indexing Papyrus scripts".to_owned(),
                        cancellable: Some(false),
                        message: None,
                        percentage: Some(0),
                    },
                )),
            });
        }

        // Build outputs hold copies of scripts (decompiled ones): not sources.
        let skipped = self.build_output_dirs();
        let tx = self.index_tx.clone();
        let spawned = std::thread::Builder::new()
            .name("papyrus-indexer".to_owned())
            .spawn(move || index_workspace(generation, &dirs, &flag_dirs, &skipped, &tx));
        if let Err(err) = spawned {
            log::error!("cannot spawn the indexer: {err}");
            self.indexing = false;
        }
    }

    /// The directories whose scripts are known. With a manifest, what it names:
    /// sources, tests, dependencies and imports, as for the compiler. Without any
    /// manifest (limited mode): the workspace folders. Next to a project, the
    /// scripts of no project ([`Origin::Outside`]): of the folders without manifest,
    /// and of the others once one of these scripts is opened.
    pub(crate) fn index_dirs(&self) -> Vec<(PathBuf, Origin)> {
        let mut dirs: Vec<(PathBuf, Origin)> = Vec::new();
        let own: Vec<PathBuf> = if self.members.is_empty() {
            self.roots.clone()
        } else {
            let sources = self.members.iter().flat_map(|m| m.sources.iter().chain(&m.test_sources));
            sources.cloned().collect()
        };
        for dir in own {
            if !dirs.iter().any(|(d, _)| dir.starts_with(d)) {
                dirs.retain(|(d, _)| !d.starts_with(&dir));
                dirs.push((dir, Origin::Workspace));
            }
        }
        for dir in self.import_dirs() {
            // Directories inside another one are already scanned.
            if !dirs.iter().any(|(d, _)| dir.starts_with(d)) {
                let origin = self.origin_of(&dir);
                dirs.push((dir, origin));
            }
        }
        if !self.members.is_empty() {
            let outside = |root: &&PathBuf| self.index_outside || self.limited_roots.contains(root);
            // After the others: a script is of its project first.
            dirs.extend(self.roots.iter().filter(outside).map(|r| (r.clone(), Origin::Outside)));
        }
        dirs
    }

    fn on_index_event(&mut self, event: IndexEvent) {
        match event {
            IndexEvent::Progress { generation, done, total } => {
                if generation != self.generation || !self.progress_supported() {
                    return;
                }
                self.notify::<notification::Progress>(ProgressParams {
                    token: progress_token(generation),
                    value: ProgressParamsValue::WorkDone(WorkDoneProgress::Report(
                        WorkDoneProgressReport {
                            cancellable: None,
                            message: Some(format!("{done}/{total}")),
                            percentage: Some((done * 100 / total.max(1)) as u32),
                        },
                    )),
                });
            }
            IndexEvent::Done { generation, files, flag_files, elapsed_ms } => {
                if generation != self.generation {
                    return;
                }
                let count = files.len();
                for (path, index, origin) in files {
                    self.ws.set_disk_index(path, index, origin);
                }
                self.select_flags_file(flag_files);
                self.indexing = false;
                log::info!("indexed {count} scripts in {elapsed_ms} ms");
                if self.progress_supported() {
                    self.notify::<notification::Progress>(ProgressParams {
                        token: progress_token(generation),
                        value: ProgressParamsValue::WorkDone(WorkDoneProgress::End(
                            WorkDoneProgressEnd { message: Some(format!("{count} scripts")) },
                        )),
                    });
                }
                self.publish_open_diagnostics();
                self.check_closed_files();
            }
        }
    }

    fn progress_supported(&self) -> bool {
        self.caps.window.as_ref().and_then(|w| w.work_done_progress).unwrap_or(false)
    }

    // === Diagnostics ===

    /// Publishes the diagnostics of every file: open ones now, closed ones in
    /// the background.
    pub(crate) fn refresh_diagnostics(&mut self) {
        self.publish_open_diagnostics();
        self.check_closed_files();
    }

    pub(crate) fn publish_open_diagnostics(&self) {
        let open: Vec<FileId> = self.ws.open_files().map(|(id, _)| id).collect();
        for file in open {
            self.publish_diagnostics(file);
        }
    }

    fn publish_diagnostics(&self, file: FileId) {
        let data = self.ws.file(file);
        let Some(doc) = &data.open else { return };
        let Some(diagnostics) = self.file_diagnostics(file) else { return };
        self.notify::<notification::PublishDiagnostics>(PublishDiagnosticsParams {
            uri: conv::path_to_uri(&data.path),
            diagnostics,
            version: Some(doc.version),
        });
    }

    fn file_diagnostics(&self, file: FileId) -> Option<Vec<lsp_types::Diagnostic>> {
        let lints = self.lint_config(&self.ws.file(file).path);
        file_diagnostics(&self.ws, file, self.config.semantic_diagnostics, &lints)
    }

    /// The directories of compiled and decompiled scripts: never sources.
    fn build_output_dirs(&self) -> Vec<PathBuf> {
        self.build_dirs()
            .into_iter()
            .flat_map(|b| [PathBuf::from(b.compile), PathBuf::from(b.decompile)])
            .chain(self.roots.iter().map(|r| papyrus_project::default_build_dir(r)))
            .collect()
    }

    /// Whether `file` is checked when closed: a script of the sources of the
    /// projects of `papyrus.toml`, outside of the build directories. Without
    /// projects (limited mode), only open files are checked.
    fn is_checked(&self, file: FileId, build_dirs: &[PathBuf]) -> bool {
        let data = self.ws.file(file);
        if data.origin != Origin::Workspace || build_dirs.iter().any(|d| data.path.starts_with(d)) {
            return false;
        }
        self.members
            .iter()
            .flat_map(|p| p.sources.iter().chain(&p.test_sources))
            .any(|s| data.path.starts_with(s))
    }

    /// The closed files to check.
    fn closed_checked_files(&self) -> Vec<FileId> {
        let build_dirs = self.build_output_dirs();
        self.ws
            .files()
            .map(|(id, _)| id)
            .filter(|&id| self.ws.file(id).open.is_none() && self.is_checked(id, &build_dirs))
            .collect()
    }

    /// Checks every closed source script in the background, publishing as
    /// results come; a newer full check stops the running one.
    pub(crate) fn check_closed_files(&mut self) {
        use std::sync::atomic::Ordering;

        let files = self.closed_checked_files();
        // Problems of files that are not checked anymore go away.
        let checked: rustc_hash::FxHashSet<PathBuf> =
            files.iter().map(|&f| self.ws.file(f).path.clone()).collect();
        for path in std::mem::take(&mut self.published) {
            if checked.contains(&path) {
                self.published.insert(path);
            } else if self.ws.file_id(&path).is_none_or(|f| self.ws.file(f).open.is_none()) {
                self.pending.remove(&path);
                self.notify::<notification::PublishDiagnostics>(PublishDiagnosticsParams {
                    uri: conv::path_to_uri(&path),
                    diagnostics: Vec::new(),
                    version: None,
                });
            }
        }
        let job = self.spawn_check(files, None);
        self.full_check.store(job, Ordering::SeqCst);
    }

    /// Checks again the closed source scripts whose text mentions one of
    /// `names` (scripts that changed): only they can see a difference.
    fn check_mentioning(&mut self, names: Vec<String>) {
        let files = self.closed_checked_files();
        self.spawn_check(files, Some(names));
    }

    /// Checks `files` in the background (only those mentioning `names` when
    /// set), and returns the job id.
    fn spawn_check(&mut self, files: Vec<FileId>, names: Option<Vec<String>>) -> u64 {
        use std::sync::atomic::Ordering;

        self.next_check_job += 1;
        let job = self.next_check_job;
        let full = names.is_none();
        if full {
            self.checking = Some(CheckProgress {
                job,
                done: 0,
                total: files.len(),
                current: String::new(),
                started: Instant::now(),
            });
        }
        let needles: Vec<String> =
            names.unwrap_or_default().iter().map(|n| n.to_ascii_lowercase()).collect();
        let ws = self.ws.clone();
        let semantic = self.config.semantic_diagnostics;
        let lints = self.lint_configs();
        let running = self.full_check.clone();
        let tx = self.check_tx.clone();
        let pool = self.check_pool.clone();
        let paused = self.check_paused.clone();
        let spawned =
            std::thread::Builder::new().name("papyrus-checker".to_owned()).spawn(move || {
                use rayon::prelude::*;
                let start = Instant::now();
                // A newer full check replaces this one.
                let cancelled = || full && running.load(Ordering::SeqCst) > job;
                pool.install(|| {
                    files.par_iter().for_each(|&file| {
                        // Requests of the editor come first.
                        while paused.load(Ordering::SeqCst) && !cancelled() {
                            std::thread::sleep(std::time::Duration::from_millis(1));
                        }
                        if cancelled() {
                            return;
                        }
                        let path = ws.file(file).path.clone();
                        let mentions = needles.is_empty()
                            || ws.text(file).is_some_and(|text| {
                                let lower = text.to_ascii_lowercase();
                                needles.iter().any(|n| lower.contains(n.as_str()))
                            });
                        let diagnostics = if mentions {
                            file_diagnostics(&ws, file, semantic, lints.for_path(&path))
                        } else {
                            None
                        };
                        let _ = tx.send(CheckEvent::File { job, file, path, diagnostics });
                    })
                });
                if !cancelled() {
                    log::info!("checked {} scripts in {:?}", files.len(), start.elapsed());
                    let _ = tx.send(CheckEvent::Done { job });
                }
            });
        if let Err(err) = spawned {
            log::error!("cannot spawn the checker: {err}");
        }
        job
    }

    fn on_check_event(&mut self, event: CheckEvent) {
        let (job, file, path, diagnostics) = match event {
            CheckEvent::Done { job } => {
                if self.checking.as_ref().is_some_and(|p| p.job == job) {
                    self.checking = None;
                }
                return;
            }
            CheckEvent::File { job, file, path, diagnostics } => (job, file, path, diagnostics),
        };
        if let Some(p) = self.checking.as_mut().filter(|p| p.job == job) {
            p.done += 1;
            p.current =
                path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        }
        let Some(diagnostics) = diagnostics else { return };
        // Results of a job older than the last one shown for the file are
        // stale; an open file shows the diagnostics of the editor text.
        if self.checked_job.get(&file).is_some_and(|&j| j > job)
            || self.ws.file_id(&path) != Some(file)
            || self.ws.file(file).open.is_some()
        {
            return;
        }
        self.checked_job.insert(file, job);
        // Most scripts have no problem: telling the editor so, file by file,
        // would flood it. Only files showing problems need an update.
        if diagnostics.is_empty() {
            if !self.published.remove(&path) {
                return;
            }
        } else {
            self.published.insert(path.clone());
        }
        if self.pending.insert(path.clone(), diagnostics).is_none() {
            self.pending_order.push_back(path);
        }
    }

    /// Publishes the next [`PUBLISH_BATCH`] pending diagnostics of closed
    /// files.
    fn flush_publishes(&mut self) {
        for _ in 0..PUBLISH_BATCH {
            let Some(path) = self.pending_order.pop_front() else { return };
            let Some(diagnostics) = self.pending.remove(&path) else { continue };
            // Opened since: the editor text has its own diagnostics.
            if self.ws.file_id(&path).is_some_and(|f| self.ws.file(f).open.is_some()) {
                continue;
            }
            self.notify::<notification::PublishDiagnostics>(PublishDiagnosticsParams {
                uri: conv::path_to_uri(&path),
                diagnostics,
                version: None,
            });
        }
    }

    /// The running full check, for `papyrus/status`.
    pub(crate) fn check_status(&self) -> Option<crate::ext::CheckStatus> {
        self.checking.as_ref().map(|p| crate::ext::CheckStatus {
            done: p.done,
            total: p.total,
            current: p.current.clone(),
            remaining_ms: p.remaining().map(|d| d.as_millis() as u64),
        })
    }
    /// Resolves a document URI to a known file.
    pub(crate) fn file(&self, uri: &Uri) -> Result<FileId, (ErrorCode, String)> {
        conv::uri_to_path(uri).and_then(|p| self.ws.file_id(&p)).ok_or_else(|| {
            (ErrorCode::InvalidParams, format!("unknown document: {}", uri.as_str()))
        })
    }
}

/// The threads of the background check: half of the cores, the others are
/// left to the editor and to the requests.
fn check_pool() -> rayon::ThreadPool {
    let cores = std::thread::available_parallelism().map_or(2, |n| n.get());
    rayon::ThreadPoolBuilder::new()
        .num_threads((cores / 2).max(1))
        .thread_name(|i| format!("papyrus-check-{i}"))
        .build()
        .expect("cannot create the check threads")
}

fn progress_token(generation: u64) -> NumberOrString {
    NumberOrString::String(format!("papyrus/indexing/{generation}"))
}

fn handle<R>(
    server: &mut Server,
    params: Value,
    f: impl FnOnce(&mut Server, R::Params) -> R::Result,
) -> HandlerResult
where
    R: request::Request,
    R::Params: DeserializeOwned,
    R::Result: Serialize,
{
    let params: R::Params = serde_json::from_value(params)
        .map_err(|e| (ErrorCode::InvalidParams, format!("invalid params: {e}")))?;
    serde_json::to_value(f(server, params)).map_err(|e| (ErrorCode::InternalError, e.to_string()))
}

fn notify<N>(not: Notification, f: impl FnOnce(N::Params))
where
    N: notification::Notification,
    N::Params: DeserializeOwned,
{
    match serde_json::from_value(not.params) {
        Ok(params) => f(params),
        Err(err) => log::warn!("invalid `{}` params: {err}", N::METHOD),
    }
}

pub(crate) fn is_script(path: &Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("psc"))
}

pub(crate) fn is_flags(path: &Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("flg"))
}

/// `papyrus.toml` or its machine local `papyrus.local.toml`.
pub(crate) fn is_manifest(path: &Path) -> bool {
    path.file_name().is_some_and(|n| {
        n.eq_ignore_ascii_case(papyrus_project::MANIFEST_NAME)
            || n.eq_ignore_ascii_case(papyrus_project::LOCAL_MANIFEST_NAME)
    })
}

/// Applies editor changes to `text`.
fn apply_changes(text: &mut String, changes: Vec<lsp_types::TextDocumentContentChangeEvent>) {
    for change in changes {
        match change.range {
            Some(range) => {
                // Positions refer to the text after the previous change.
                let lines = Lines {
                    index: Arc::new(papyrus_ide::LineIndex::new(text)),
                    text: Arc::from(text.as_str()),
                };
                let range = lines.text_range(range);
                text.replace_range(
                    usize::from(range.start())..usize::from(range.end()),
                    &change.text,
                );
            }
            None => *text = change.text,
        }
    }
}

/// Finds and indexes every script below `dirs`, in parallel; flags files are
/// looked for below `flag_dirs` too.
fn index_workspace(
    generation: u64,
    dirs: &[(PathBuf, Origin)],
    flag_dirs: &[PathBuf],
    skipped: &[PathBuf],
    tx: &Sender<IndexEvent>,
) {
    let start = Instant::now();
    let mut paths = Vec::new();
    let mut flag_files = Vec::new();
    for (dir, origin) in dirs {
        // The flags files of what is of no project are not for the projects.
        let mut none = Vec::new();
        let flags = if *origin == Origin::Outside { &mut none } else { &mut flag_files };
        collect_scripts(dir, *origin, skipped, &mut paths, flags);
    }
    for dir in flag_dirs.iter().filter(|f| !dirs.iter().any(|(d, _)| f.starts_with(d))) {
        collect_scripts(dir, Origin::Workspace, skipped, &mut Vec::new(), &mut flag_files);
    }
    // A folder of no project holds the projects too: the first origin wins.
    let mut seen = rustc_hash::FxHashSet::default();
    paths.retain(|(path, _): &(PathBuf, Origin)| seen.insert(path.clone()));
    let total = paths.len();
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).min(16);
    let chunk = total.div_ceil(threads).max(1);
    let done = std::sync::atomic::AtomicUsize::new(0);

    let files: Vec<(PathBuf, ScriptIndex, Origin)> = std::thread::scope(|s| {
        let handles: Vec<_> = paths
            .chunks(chunk)
            .map(|chunk| {
                let done = &done;
                s.spawn(move || {
                    let mut out = Vec::with_capacity(chunk.len());
                    for (path, origin) in chunk {
                        if let Some(text) = papyrus_ide::read_file(path) {
                            out.push((path.clone(), papyrus_ide::index_text(&text), *origin));
                        }
                        let n = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                        if n.is_multiple_of(500) {
                            let _ = tx.send(IndexEvent::Progress { generation, done: n, total });
                        }
                    }
                    out
                })
            })
            .collect();
        handles.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
    });

    let elapsed_ms = start.elapsed().as_millis();
    let _ = tx.send(IndexEvent::Done { generation, files, flag_files, elapsed_ms });
}

fn collect_scripts(
    dir: &Path,
    origin: Origin,
    skipped: &[PathBuf],
    out: &mut Vec<(PathBuf, Origin)>,
    flag_files: &mut Vec<PathBuf>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    // Sorted: among scripts declared twice, the first indexed wins, which
    // must not depend on the order of the file system.
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|e| e.path());
    for entry in entries {
        let path = entry.path();
        let Ok(ty) = entry.file_type() else { continue };
        if ty.is_dir() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.')
                || name == "node_modules"
                || name == "target"
                || skipped.contains(&path)
            {
                continue;
            }
            collect_scripts(&path, origin, skipped, out, flag_files);
        } else if is_script(&path) {
            out.push((path, origin));
        } else if is_flags(&path) {
            flag_files.push(path);
        }
    }
}

/// The LSP diagnostics of a file: compiler checks and lints. The text is
/// parsed once for both.
fn file_diagnostics(
    ws: &Workspace,
    file: FileId,
    semantic: bool,
    lints: &papyrus_lint::LintConfig,
) -> Option<Vec<lsp_types::Diagnostic>> {
    let lines = Lines::of(ws, file)?;
    let (src, errors) = match &ws.file(file).open {
        Some(doc) => (papyrus_ide::sema::FileSource::load(ws, file)?, doc.errors.to_vec()),
        None => papyrus_ide::sema::FileSource::parse(lines.text.clone()),
    };
    let found = papyrus_ide::diagnostics_of(ws, file, &src, &errors)
        .into_iter()
        .filter(|d| semantic || !d.needs_workspace())
        .chain(papyrus_lint::lint_source(ws, file, &src, !errors.is_empty(), lints));
    Some(
        found
            .map(|d| lsp_types::Diagnostic {
                range: lines.range(d.range),
                severity: Some(match d.severity {
                    papyrus_ide::Severity::Error => lsp_types::DiagnosticSeverity::ERROR,
                    papyrus_ide::Severity::Warning => lsp_types::DiagnosticSeverity::WARNING,
                }),
                code: Some(NumberOrString::String(d.code.to_owned())),
                source: Some("papyrus".to_owned()),
                message: d.message,
                related_information: (!d.related.is_empty()).then(|| {
                    d.related
                        .iter()
                        .filter_map(|r| {
                            let range = if r.file == file {
                                lines.range(r.range)
                            } else {
                                Lines::of(ws, r.file)?.range(r.range)
                            };
                            Some(lsp_types::DiagnosticRelatedInformation {
                                location: lsp_types::Location {
                                    uri: conv::path_to_uri(&ws.file(r.file).path),
                                    range,
                                },
                                message: r.message.clone(),
                            })
                        })
                        .collect()
                }),
                ..Default::default()
            })
            .collect(),
    )
}
