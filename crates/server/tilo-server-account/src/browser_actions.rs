//! The Browser page actions an account ran, as the server's log keeps
//! them.
//!
//! Every page action the server runs on a hosted Browser is one line of a
//! file it appends to ([`ACTIONS_LOG_ENV`]), written before the action: a server
//! that stops leaves no action uncounted, and a line still being written is
//! skipped rather than read as another. An account's count is the lines of
//! its billing period ([`Log::count`]); nothing here knows prices.

use std::io::{BufRead as _, BufReader, Write as _};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// The log of Browser page actions the server keeps, a file. Set on a host
/// that runs hosted Browser; the server writes it and reads it, this crate
/// reads it for the account's subscription.
pub const ACTIONS_LOG_ENV: &str = "TILO_SERVER_BROWSER_ACTIONS";

/// One page action an account ran.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PageAction {
    pub account: String,
    /// The workspace whose Browser it ran in.
    pub workspace: String,
    /// When it ran, Unix milliseconds.
    pub at_ms: u64,
}

/// The log file of page actions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Log {
    path: PathBuf,
}

impl Log {
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// Append `action` and wait until it is on disk: a count is money.
    ///
    /// # Errors
    ///
    /// The file cannot be written.
    pub fn record(&self, action: &PageAction) -> std::io::Result<()> {
        if let Some(dir) = self.path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        let mut line = serde_json::to_vec(action).map_err(std::io::Error::other)?;
        line.push(b'\n');
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        file.write_all(&line)?;
        file.sync_data()
    }

    /// The page actions `account` ran in `[from_ms, to_ms)`: the lines of
    /// its billing period. A line that does not read (a torn write at a
    /// crash) is skipped.
    #[must_use]
    pub fn count(&self, account: &str, from_ms: u64, to_ms: u64) -> u64 {
        let Ok(file) = std::fs::File::open(&self.path) else {
            return 0;
        };
        let count = BufReader::new(file)
            .lines()
            .map_while(Result::ok)
            .filter_map(|line| serde_json::from_str::<PageAction>(&line).ok())
            .filter(|action| action.account == account && (from_ms..to_ms).contains(&action.at_ms))
            .count();
        u64::try_from(count).unwrap_or(u64::MAX)
    }
}
