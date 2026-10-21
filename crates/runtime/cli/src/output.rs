//! Terminal-aware output helpers.
//!
//! The builder emits OSC 8 hyperlinks on terminals and plain text when output
//! is redirected. It does not provide colour, progress bars, or tables;
//! callers needing those can use their preferred output library.

use std::fmt;
use std::io::{self, IsTerminal, Write};

/// Chainable builder returned by [`crate::output()`].
pub struct OutputBuilder {
    hyperlinks: bool,
    stream: Stream,
}

#[derive(Clone, Copy)]
enum Stream {
    Stdout,
    Stderr,
}

impl OutputBuilder {
    /// Force hyperlinks on; default is auto-detect via [`io::IsTerminal`].
    #[must_use]
    pub fn hyperlinks(mut self, on: bool) -> Self {
        self.hyperlinks = on;
        self
    }

    /// Route to stderr instead of stdout.
    #[must_use]
    pub fn stderr(mut self) -> Self {
        self.stream = Stream::Stderr;
        self
    }

    /// Write a bare line to the stream.
    ///
    /// # Errors
    ///
    /// Returns the underlying I/O error if the write to the stream fails.
    pub fn line<S: fmt::Display>(&self, text: S) -> io::Result<()> {
        self.write(|w| writeln!(w, "{text}"))
    }

    /// Write text and a clickable hyperlink (OSC 8) on the same line; pipes
    /// and files fall back to `<label> [<url>]`. An empty label renders the
    /// URL itself as the anchor.
    ///
    /// # Errors
    ///
    /// Returns the underlying I/O error if the write to the stream fails.
    pub fn hyperlink(&self, label: &str, url: &str) -> io::Result<()> {
        self.write(|w| {
            if self.hyperlinks_active(w) {
                // OSC 8 sequence: ESC ] 8 ; params ; URI ST label ESC ] 8 ; ; ST
                write!(w, "\x1b]8;;{url}\x1b\\{label}\x1b]8;;\x1b\\")?;
                writeln!(w)
            } else if label.is_empty() {
                writeln!(w, "{url}")
            } else {
                writeln!(w, "{label} [{url}]")
            }
        })
    }

    /// Emit a `file://` hyperlink for the given path — a convenience wrapper
    /// over [`OutputBuilder::hyperlink`].
    ///
    /// # Errors
    ///
    /// Returns the underlying I/O error if the write to the stream fails.
    pub fn hyperlink_path<P: AsRef<std::path::Path>>(
        &self,
        label: &str,
        path: P,
    ) -> io::Result<()> {
        let p = path.as_ref();
        let url = format!("file://{}", p.display());
        self.hyperlink(label, &url)
    }

    fn write<F>(&self, f: F) -> io::Result<()>
    where
        F: FnOnce(&mut dyn Write) -> io::Result<()>,
    {
        match self.stream {
            Stream::Stdout => {
                let stdout = io::stdout();
                let mut lock = stdout.lock();
                f(&mut lock)
            }
            Stream::Stderr => {
                let stderr = io::stderr();
                let mut lock = stderr.lock();
                f(&mut lock)
            }
        }
    }

    /// Whether to actually emit hyperlink escapes, given the stream's TTY status.
    fn hyperlinks_active(&self, w: &dyn Write) -> bool {
        let _ = w; // borrow — we can't call is_terminal on a &dyn Write
        if !self.hyperlinks {
            return false;
        }
        match self.stream {
            Stream::Stdout => io::stdout().is_terminal(),
            Stream::Stderr => io::stderr().is_terminal(),
        }
    }
}

/// Start a terminal-aware output builder; hyperlinks default to on-if-terminal.
pub fn output() -> OutputBuilder {
    OutputBuilder {
        hyperlinks: true,
        stream: Stream::Stdout,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_defaults_to_stdout_with_hyperlinks() {
        let b = output();
        assert!(b.hyperlinks);
        // Stream default is stdout.
        match b.stream {
            Stream::Stdout => {}
            Stream::Stderr => panic!("expected stdout default"),
        }
    }
}
