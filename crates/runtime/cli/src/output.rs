//! Terminal-aware output: OSC 8 hyperlinks on a terminal, plain text when
//! output is redirected. No colour, progress bars or tables.

use std::fmt;
use std::io::{self, IsTerminal, Write};

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
    /// Force hyperlinks on; the default follows [`io::IsTerminal`].
    #[must_use]
    pub fn hyperlinks(mut self, on: bool) -> Self {
        self.hyperlinks = on;
        self
    }

    #[must_use]
    pub fn stderr(mut self) -> Self {
        self.stream = Stream::Stderr;
        self
    }

    pub fn line<S: fmt::Display>(&self, text: S) -> io::Result<()> {
        self.write(|w| writeln!(w, "{text}"))
    }

    /// Text and a clickable link on one line; a pipe or file gets
    /// `<label> [<url>]`, and an empty label shows the URL alone.
    pub fn hyperlink(&self, label: &str, url: &str) -> io::Result<()> {
        self.write(|w| {
            if self.hyperlinks_active(w) {
                // OSC 8: ESC ] 8 ; params ; URI ST label ESC ] 8 ; ; ST
                write!(w, "\x1b]8;;{url}\x1b\\{label}\x1b]8;;\x1b\\")?;
                writeln!(w)
            } else if label.is_empty() {
                writeln!(w, "{url}")
            } else {
                writeln!(w, "{label} [{url}]")
            }
        })
    }

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
        match b.stream {
            Stream::Stdout => {}
            Stream::Stderr => panic!("expected stdout default"),
        }
    }
}
