//! A function body to instructions.
//!
//! - [`body`]: the HIR of a body (from `papyrus_ide`) to an [`ir::FunctionIr`],
//!   following the conventions of the official compiler;
//! - [`ir`]: the instructions, with symbolic labels and named operands;
//! - [`opt`]: the optional optimization passes on them;
//! - [`constant`]: the values of literals and their conversions at compile
//!   time, shared with the assembly of initial values.

mod body;
pub(crate) mod constant;
pub(crate) mod ir;
pub(crate) mod opt;

pub(crate) use body::{ScriptCx, function};
