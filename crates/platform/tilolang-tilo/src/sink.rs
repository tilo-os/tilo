//! Where settled outside effects go while there is no real capability behind them.
//!
//! An outside effect (an app's `outside("post", ..)`) waits in the room record until the device
//! that owns it settles it: delivers it to a sink and records that it was delivered. Today the sink
//! of the host is a log: one JSON line per effect, in a file under the state directory of
//! `tilolang-dev`, so what an approved letter would have sent can be read.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde_json::Value as Json;
use tilolang_runtime::OutsideSink;

/// The directory the development sink writes under: `$XDG_STATE_HOME/tilolang-dev`, else
/// `~/.local/state/tilolang-dev`.
#[must_use]
pub fn dev_state_dir() -> PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local").join("state"))
        })
        .unwrap_or_else(std::env::temp_dir);
    base.join("tilolang-dev")
}

/// A sink that appends each delivered effect to a file as one JSON line.
#[derive(Clone, Debug)]
pub struct LogSink {
    path: PathBuf,
}

impl LogSink {
    /// A sink writing to `path`; the directory is made when the first effect arrives.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The development sink: `outside.log` in [`dev_state_dir`].
    #[must_use]
    pub fn dev() -> Self {
        Self::new(dev_state_dir().join("outside.log"))
    }

    /// The file the effects are written to.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl OutsideSink for LogSink {
    fn deliver(&mut self, capability: &str, payload: &Json) -> Result<(), String> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
        }
        let line = serde_json::json!({"capability": capability, "payload": payload});
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|error| error.to_string())?;
        writeln!(file, "{line}").map_err(|error| error.to_string())
    }
}
