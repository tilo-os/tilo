//! Diagnostic output that cannot take the application down with it.
//!
//! `eprintln!` ends in the standard library's `print_to`, whose last act is
//! `panic!("failed printing to {label}: {e}")`. A qOS application is launched
//! with its stderr on a pipe the compositor reads, so when that reading end
//! goes away the next write returns `EPIPE` and that panic fires — under the
//! release profile's `panic = "abort"`, instantly and without unwinding. A
//! diagnostic line then kills the program it was describing, and its window is
//! left on screen with no process behind it.
//!
//! [`diag!`] is `eprintln!` with the write error dropped. Use it wherever an
//! app or the toolkit would reach for `eprintln!`; `tracing` needs no such
//! treatment, its writers already discard I/O errors.

use std::fmt::Arguments;
use std::io::Write;

/// Write one diagnostic line to stderr, discarding any write error.
///
/// Formatted into a `String` first so the line and its terminator reach the
/// pipe in a single `write_all`: a reader decodes this stderr as UTF-8, and a
/// line torn between two halves of a multi-byte character fails that decode.
pub fn write_line(args: Arguments<'_>) {
    let stderr = std::io::stderr();
    let mut out = stderr.lock();
    let mut line = args.to_string();
    line.push('\n');
    let _ = out.write_all(line.as_bytes());
    let _ = out.flush();
}

/// `eprintln!` that cannot kill the process. See the module docs for why that
/// difference is load-bearing.
#[macro_export]
macro_rules! diag {
    ($($arg:tt)*) => {
        $crate::diag::write_line(::std::format_args!($($arg)*))
    };
}
