//! Diagnostic output that cannot take the application down with it.
//!
//! `println!` and `eprintln!` are not safe in a long-lived process whose
//! output goes somewhere it does not control. Both funnel into the
//! standard library's `print_to`, and its last line is:
//!
//! ```text
//! if let Err(e) = global_s().write_fmt(args) {
//!     panic!("failed printing to {label}: {e}");
//! }
//! ```
//!
//! A qOS application is launched by the compositor with its stdout and
//! stderr on pipes, and the compositor holds the reading ends. The moment
//! one of those ends goes away — the compositor exits, or its relay stops
//! reading — the next write returns `EPIPE`, that `panic!` fires, and under
//! the release/demo profile's `panic = "abort"` the application is gone
//! instantly: no unwind, no message, no chance to save. Its window is left
//! on screen as a corpse the person cannot get out of.
//!
//! That is a diagnostic line killing the program it was supposed to
//! describe. Diagnostics are never worth that, so they go through here
//! instead: same destination, same formatting, but a failed write is
//! dropped rather than promoted to a fatal error.
//!
//! Use [`diag!`] wherever an app or the toolkit would have reached for
//! `eprintln!`. `tracing` needs no such treatment — its writers already
//! discard I/O errors.

use std::fmt::Arguments;
use std::io::Write;

/// Write one diagnostic line to stderr, discarding any write error.
///
/// The line and its terminator are written under a single `stderr` lock so
/// concurrent callers cannot interleave halfway through, which matters
/// beyond tidiness: a relay reading this pipe decodes it as UTF-8, and a
/// line torn between two multi-byte characters would make that decode fail.
pub fn write_line(args: Arguments<'_>) {
    let stderr = std::io::stderr();
    let mut out = stderr.lock();
    // Build the line first so exactly one `write_all` reaches the pipe:
    // two writes could be split by another process sharing this pipe.
    let mut line = args.to_string();
    line.push('\n');
    let _ = out.write_all(line.as_bytes());
    let _ = out.flush();
}

/// `eprintln!` that cannot kill the process.
///
/// Formats and writes to stderr exactly as `eprintln!` does, but a failed
/// write — a closed pipe, a full disk — is discarded instead of panicking.
/// See the module documentation for why that difference is load-bearing.
#[macro_export]
macro_rules! diag {
    ($($arg:tt)*) => {
        $crate::diag::write_line(::std::format_args!($($arg)*))
    };
}
