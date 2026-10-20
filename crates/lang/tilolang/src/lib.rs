//! tilolang: the compiler for the Tilo app language.
//!
//! Source text goes in; a wasm module, its metadata and the updated ids lock come out. The
//! design is `docs/designs/app-language`: the grammar is implementation.md section 2, the
//! checks are section 3, the module shape and host imports are section 4, the metadata is
//! section 5, and agent blocks are section 6.1.

#![allow(
    clippy::arithmetic_side_effects,
    clippy::too_many_lines,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::module_name_repetitions,
    clippy::similar_names,
    clippy::doc_markdown,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::indexing_slicing,
    reason = "a compiler over source text and wasm indices: offsets are bounded by the source length, \
              the wasm widths are fixed by the ABI, and every path that can fail on user input is a diagnostic"
)]

pub mod abi;
pub mod ast;
mod checker;
mod codegen;
pub mod diag;
mod effects;
mod hash;
pub mod ids;
pub mod ir;
pub mod lexer;
pub mod meta;
mod package;
pub mod parser;
mod prims;
pub mod print;
pub mod resolve;
pub mod types;

pub use diag::{Diagnostic, Span, diagnostics_json};
pub use ids::{Entropy, IdsError, IdsLock, OsEntropy, SeededEntropy};
pub use meta::{PackageMeta, custom_section};
pub use package::{
    BlockKind, Compiled, CompiledBlock, check, compile_block, compile_block_for, compile_package,
    compile_package_with, parse,
};
pub use print::print_module;

/// `a.b.c` as `"a.b.c"` when the expression is a dotted path.
pub(crate) fn check_path(e: &ast::Expr) -> Option<String> {
    match &e.kind {
        ast::ExprKind::Ident(n) => Some(n.clone()),
        ast::ExprKind::Field { base, name } => Some(format!("{}.{}", check_path(base)?, name.name)),
        _ => None,
    }
}
