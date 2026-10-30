//! tilolang: the compiler for the Tilo app language.
//!
//! Source text goes in; a wasm module, its metadata and the updated ids lock come out. The
//! compiler knows nothing of Tilo: the runtime (`tilolang-runtime`) runs what it emits, and the
//! platform crate (`tilolang-tilo`) connects that to Tilo. The design is
//! `docs/designs/app-language`; the grammar is `grammar.md` next to this crate's README.
//!
//! The steps, in order:
//!
//! * [`lexer`] and [`parser`]: text to the syntax tree ([`ast`]); [`print`](mod@print) writes it
//!   back in its one canonical form.
//! * [`ids`]: the ids lock, which gives every declaration a stable id.
//! * `resolve` and `checker`: names, types, merge rules and what each kind of declaration may
//!   do, lowered to the IR ([`ir`]) in the same walk.
//! * `effects`: each action's effect class and what every declaration can reach.
//! * `codegen`: the IR to a wasm module that imports the host functions of [`abi`].
//! * [`meta`]: the metadata the module carries, which the runtime, the platform and agents read.

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
    clippy::cast_precision_loss,
    clippy::many_single_char_names,
    clippy::format_push_string,
    clippy::single_match_else,
    reason = "a compiler over source text and wasm indices: offsets are bounded by the source length, \
              the wasm widths are fixed by the ABI, and every path that can fail on user input is a diagnostic"
)]

pub mod abi;
pub mod ast;
pub mod calendar;
mod checker;
pub mod cli;
mod codegen;
pub mod describe;
pub mod diag;
pub mod duration;
mod effects;
mod hash;
pub mod host;
pub mod identifier;
pub mod ids;
pub mod ir;
pub mod lexer;
pub mod meta;
pub mod natural;
pub mod origin;
mod package;
pub mod parser;
pub mod pred;
mod prims;
pub mod print;
pub mod resolve;
pub mod search;
pub mod source;
pub mod stdlib;
pub mod toolkit;
pub mod types;
pub mod written;

pub use checker::{RESOURCE_TYPE_NAME, resource_key};
pub use diag::{Diagnostic, Span, diagnostics_json, did_you_mean};
pub use host::{
    HOST_APP, INVOCATION_TYPE, RESULT_ERR, RESULT_ERROR, RESULT_OK, RESULT_TYPE, RESULT_VALUE,
    host_meta,
};
pub use ids::{Entropy, IdsError, IdsLock, OsEntropy, SeededEntropy};
pub use meta::{PackageMeta, custom_section};
pub use package::{
    Binding, BlockKind, Compiled, CompiledBlock, CompiledProgram, FileSyntax, PROGRAM_APP_PREFIX,
    check, check_package, compile_block, compile_block_for, compile_package,
    compile_package_source, compile_package_source_with, compile_package_with, compile_program,
    compile_program_reading, parse, parse_package, parse_package_files,
};
pub use print::{print_file, print_module};
pub use source::{PackageSource, SourceFile};
pub use written::{CompiledView, WRITTEN_VIEW_APP_PREFIX, compile_view_definition};

/// `a.b.c` as `"a.b.c"` when the expression is a dotted path.
pub(crate) fn check_path(e: &ast::Expr) -> Option<String> {
    match &e.kind {
        ast::ExprKind::Ident(n) => Some(n.clone()),
        ast::ExprKind::Field { base, name } => Some(format!("{}.{}", check_path(base)?, name.name)),
        _ => None,
    }
}
