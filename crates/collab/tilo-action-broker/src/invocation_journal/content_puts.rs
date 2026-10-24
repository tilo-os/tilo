//! Who put which content into which room: one line per finished content
//! put in `content-puts.jsonl`, beside the journal's records and under its
//! single-writer lock. No bytes are kept here, only the attribution; the
//! uploader's identity is never consulted for access.

use serde::{Deserialize, Serialize};

use super::writer::{Injected, Write as Staged};
use super::{Durable, InvocationJournal};

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
    /// Stage `record` for the journal's writer, after every write staged
    /// before it; its ticket resolves once the line is durable.
    ///
    /// # Errors
    ///
    /// Returns why it cannot be staged.
    pub(crate) fn record_content_put(
        &mut self,
        record: &ContentPutRecord,
    ) -> Result<Durable, String> {
        self.check()?;
        let mut line = serde_json::to_vec(record).map_err(|error| error.to_string())?;
        line.push(b'\n');
        self.stage(
            Staged::Beside {
                name: CONTENT_PUTS_FILE,
                line,
            },
            Injected::default(),
        )
    }
}
