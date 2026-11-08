//! Language server for the Papyrus scripting language.
//!
//! Speaks the Language Server Protocol over stdio. Logs go to stderr and are
//! controlled with the `RUST_LOG` environment variable (default: `info`).

mod builtin;
mod compile;
mod config;
mod conv;
mod debug;
mod decompile;
mod disasm;
mod doctest;
mod ext;
mod fmt;
mod handlers;
mod lint;
mod project;
mod run;
mod server;

use lsp_types::{
    CompletionOptions, FoldingRangeProviderCapability, HoverProviderCapability, InitializeParams,
    InitializeResult, OneOf, SaveOptions, SemanticTokensFullOptions, SemanticTokensLegend,
    SemanticTokensOptions, SemanticTokensServerCapabilities, ServerCapabilities, ServerInfo,
    SignatureHelpOptions, TextDocumentSyncCapability, TextDocumentSyncKind,
    TextDocumentSyncOptions, TextDocumentSyncSaveOptions, WorkDoneProgressOptions,
};
use papyrus_ide::{TokenModifier, TokenType};

// The global allocator: see the features of Cargo.toml.
#[cfg(all(feature = "jemalloc", not(target_env = "msvc")))]
#[global_allocator]
static ALLOCATOR: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

#[cfg(all(feature = "mimalloc", not(all(feature = "jemalloc", not(target_env = "msvc")))))]
#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[allow(clippy::print_stdout)]
fn main() -> anyhow::Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    // `help COMMAND` is `COMMAND --help`.
    if args.first().is_some_and(|a| a == "help") {
        args.remove(0);
        args.truncate(1);
        args.push("--help".to_owned());
    }
    // Subcommands first: their own `--help`.
    let command: Option<fn(&[String]) -> i32> = match args.first().map(String::as_str) {
        Some("compile") => Some(compile::run),
        Some("debug") => Some(debug::run),
        Some("decompile") => Some(decompile::run),
        Some("disasm") => Some(disasm::run),
        Some("fmt") => Some(fmt::run),
        Some("lint") => Some(lint::run),
        Some("test") => Some(run::test),
        Some("run") => Some(run::run),
        _ => None,
    };
    if let Some(command) = command {
        std::process::exit(command(&args[1..]));
    }
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("papyrus_analyzer {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{}", config::USAGE);
        return Ok(());
    }
    let cli = config::CliOptions::parse(args).map_err(anyhow::Error::msg)?;

    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .target(env_logger::Target::Stderr)
        .init();
    log::info!("papyrus_analyzer {} starting", env!("CARGO_PKG_VERSION"));

    let (connection, io_threads) = lsp_server::Connection::stdio();
    let (id, params) = connection.initialize_start()?;
    let init: InitializeParams = serde_json::from_value(params)?;
    let result = InitializeResult {
        capabilities: capabilities(&init),
        server_info: Some(ServerInfo {
            name: "papyrus_analyzer".to_owned(),
            version: Some(env!("CARGO_PKG_VERSION").to_owned()),
        }),
    };
    connection.initialize_finish(id, serde_json::to_value(result)?)?;

    server::Server::new(connection.sender.clone(), &init, cli).run(&connection)?;
    drop(connection);
    io_threads.join()?;
    log::info!("papyrus_analyzer stopped");
    Ok(())
}

/// Whether the client registers formatting dynamically: then the server
/// does it for scripts only, not for papyrus.toml and the flags file (it
/// would show up among the formatters of TOML).
pub(crate) fn dynamic_formatting(init: &InitializeParams) -> bool {
    (init.capabilities.text_document.as_ref())
        .and_then(|t| t.formatting.as_ref())
        .and_then(|f| f.dynamic_registration)
        .unwrap_or(false)
}

fn capabilities(init: &InitializeParams) -> ServerCapabilities {
    ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Options(TextDocumentSyncOptions {
            open_close: Some(true),
            change: Some(TextDocumentSyncKind::INCREMENTAL),
            will_save: None,
            will_save_wait_until: None,
            save: Some(TextDocumentSyncSaveOptions::SaveOptions(SaveOptions {
                include_text: Some(false),
            })),
        })),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        definition_provider: Some(OneOf::Left(true)),
        declaration_provider: Some(lsp_types::DeclarationCapability::Simple(true)),
        rename_provider: Some(OneOf::Right(lsp_types::RenameOptions {
            prepare_provider: Some(true),
            work_done_progress_options: WorkDoneProgressOptions::default(),
        })),
        code_action_provider: Some(lsp_types::CodeActionProviderCapability::Options(
            lsp_types::CodeActionOptions {
                code_action_kinds: Some(vec![
                    lsp_types::CodeActionKind::QUICKFIX,
                    lsp_types::CodeActionKind::REFACTOR_REWRITE,
                ]),
                ..Default::default()
            },
        )),
        references_provider: Some(OneOf::Left(true)),
        document_highlight_provider: Some(OneOf::Left(true)),
        document_symbol_provider: Some(OneOf::Left(true)),
        workspace_symbol_provider: Some(OneOf::Left(true)),
        folding_range_provider: Some(FoldingRangeProviderCapability::Simple(true)),
        code_lens_provider: Some(lsp_types::CodeLensOptions { resolve_provider: Some(false) }),
        // Registered for scripts once initialized, see `dynamic_formatting`.
        document_formatting_provider: (!dynamic_formatting(init)).then_some(OneOf::Left(true)),
        inlay_hint_provider: Some(OneOf::Left(true)),
        completion_provider: Some(CompletionOptions {
            resolve_provider: Some(true),
            // `[`, `(` and `,` for the lints of `; #[allow(...)]` comments,
            // where the editor does not suggest as you type; `"` for the states
            // of `GoToState("...")`, in strings likewise.
            trigger_characters: Some([".", "[", "(", ",", "\""].map(str::to_owned).to_vec()),
            ..Default::default()
        }),
        signature_help_provider: Some(SignatureHelpOptions {
            trigger_characters: Some(vec!["(".to_owned(), ",".to_owned()]),
            retrigger_characters: None,
            work_done_progress_options: WorkDoneProgressOptions::default(),
        }),
        semantic_tokens_provider: Some(SemanticTokensServerCapabilities::SemanticTokensOptions(
            SemanticTokensOptions {
                legend: SemanticTokensLegend {
                    token_types: TokenType::ALL.iter().map(|(_, n)| (*n).into()).collect(),
                    token_modifiers: TokenModifier::ALL.iter().map(|(_, n)| (*n).into()).collect(),
                },
                full: Some(SemanticTokensFullOptions::Bool(true)),
                range: Some(true),
                work_done_progress_options: WorkDoneProgressOptions::default(),
            },
        )),
        ..Default::default()
    }
}
