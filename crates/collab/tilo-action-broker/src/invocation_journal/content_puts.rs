//! Who put which content into which room: one line per finished content
//! put in `content-puts.jsonl`, beside the journal's records and under its
//! single-writer lock. No bytes are kept here, only the attribution; the
//! uploader's identity is never consulted for access.

use std::fs::OpenOptions;
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;

use serde::{Deserialize, Serialize};

use super::{InvocationJournal, private_file};

/// The file the attributions are appended to.
pub(crate) const CONTENT_PUTS_FILE: &str = "content-puts.jsonl";

/// One finished content put.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ContentPutRecord {
    /// Seconds since the Unix epoch, on the broker's policy clock.
    pub(crate) at: u64,
    /// The broker actor that put it (`agent:<id>`, a person's id).
    pub(crate) actor: String,
    pub(crate) room: String,
    pub(crate) hash: String,
    pub(crate) len: u64,
}

impl InvocationJournal {
    /// Append `record`, durable before this returns.
    ///
    /// # Errors
    ///
    /// Returns why it could not be made durable.
    pub(crate) fn record_content_put(&self, record: &ContentPutRecord) -> Result<(), String> {
        self.check()?;
        let mut line = serde_json::to_vec(record).map_err(|error| error.to_string())?;
        line.push(b'\n');
        let path = self.root.join(CONTENT_PUTS_FILE);
        let (mut file, created) = match OpenOptions::new()
            .append(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
        {
            Ok(file) => (file, true),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (
                OpenOptions::new()
                    .append(true)
                    .custom_flags(libc::O_NOFOLLOW)
                    .open(&path)
                    .map_err(|_| "content put attributions unavailable")?,
                false,
            ),
            Err(_) => return Err("content put attributions unavailable".into()),
        };
        private_file(&file)?;
        file.write_all(&line)
            .and_then(|()| file.sync_data())
            .map_err(|_| "content put attribution not durable")?;
        if created {
            // The new file's name is durable only once its directory is.
            std::fs::File::open(&self.root)
                .and_then(|directory| directory.sync_all())
                .map_err(|_| "content put attribution not durable")?;
        }
        Ok(())
    }
}
