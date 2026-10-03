//! Reserve standard output for the single `--json` document.
//!
//! Commands print progress and run Cargo, the app, and other tools whose
//! output would otherwise interleave with the document. Claiming moves the
//! original stdout aside and points descriptor 1 at stderr for the rest of the
//! process, so every later write, including a child's, stays human-visible
//! without corrupting the machine-readable stream.

use std::io::{self, Write as _};

pub struct JsonStdout {
    out: Box<dyn io::Write>,
}

impl JsonStdout {
    #[cfg(unix)]
    pub fn claim() -> io::Result<Self> {
        use std::os::fd::FromRawFd as _;

        io::stdout().flush()?;
        // SAFETY: descriptor 1 is open for the life of the process; `dup`
        // returns a new descriptor this process exclusively owns or -1.
        let saved = unsafe { libc::dup(libc::STDOUT_FILENO) };
        if saved < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `saved` was just returned by `dup` and nothing else owns it.
        let original = unsafe { std::fs::File::from_raw_fd(saved) };
        // SAFETY: both descriptors are open; dup2 atomically replaces 1.
        if unsafe { libc::dup2(libc::STDERR_FILENO, libc::STDOUT_FILENO) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            out: Box::new(original),
        })
    }

    #[cfg(not(unix))]
    pub fn claim() -> io::Result<Self> {
        Ok(Self {
            out: Box::new(io::stdout()),
        })
    }

    pub fn emit(mut self, document: &serde_json::Value) -> io::Result<()> {
        io::stdout().flush()?;
        serde_json::to_writer(&mut self.out, document)?;
        self.out.write_all(b"\n")?;
        self.out.flush()
    }
}
