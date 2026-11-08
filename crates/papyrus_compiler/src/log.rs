//! Progress logs: what the compiler is doing, with the `log` feature.
//!
//! The levels are:
//!
//! - `info`: files (indexing, compiling, writing) and timings;
//! - `debug`: functions and the effect of optimization passes;
//! - `trace`: the generated code of every function.
//!
//! Without the feature, the macros compile to nothing (their arguments are
//! still type checked).

use std::path::PathBuf;

macro_rules! info {
    ($($t:tt)*) => {{
        #[cfg(feature = "log")]
        tracing::info!($($t)*);
        #[cfg(not(feature = "log"))]
        if false { let _ = format!($($t)*); }
    }};
}

macro_rules! debug {
    ($($t:tt)*) => {{
        #[cfg(feature = "log")]
        tracing::debug!($($t)*);
        #[cfg(not(feature = "log"))]
        if false { let _ = format!($($t)*); }
    }};
}

macro_rules! trace {
    ($($t:tt)*) => {{
        #[cfg(feature = "log")]
        tracing::trace!($($t)*);
        #[cfg(not(feature = "log"))]
        if false { let _ = format!($($t)*); }
    }};
}

pub(crate) use {debug, info, trace};

/// Where the logs go.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LogTarget {
    /// Standard error.
    Stderr,
    /// A file, created or truncated.
    File(PathBuf),
}

/// Installs the logger. `filter` uses the syntax of `RUST_LOG`, e.g. `debug`
/// or `papyrus_compiler=trace`.
#[cfg(feature = "log")]
pub(crate) fn init(target: &LogTarget, filter: &str) -> Result<(), String> {
    use tracing_subscriber::EnvFilter;

    let filter = EnvFilter::try_new(filter).map_err(|e| format!("invalid log filter: {e}"))?;
    let builder = tracing_subscriber::fmt().with_env_filter(filter).with_target(false);
    let result = match target {
        LogTarget::Stderr => {
            use std::io::IsTerminal;
            builder
                .with_ansi(std::io::stderr().is_terminal())
                .with_writer(std::io::stderr)
                .try_init()
        }
        LogTarget::File(path) => {
            let file =
                std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
            builder.with_ansi(false).with_writer(std::sync::Mutex::new(file)).try_init()
        }
    };
    result.map_err(|e| e.to_string())
}

/// Installs the logger: not available without the `log` feature.
#[cfg(not(feature = "log"))]
pub(crate) fn init(_target: &LogTarget, _filter: &str) -> Result<(), String> {
    Err("this build has no logs (feature `log`)".to_owned())
}
