//! Diagnostic output that cannot take the application down with it. `eprintln!` ends in the standard
//! library's `print_to`, whose last act is `panic!("failed printing to {label}: {e}")`.

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
