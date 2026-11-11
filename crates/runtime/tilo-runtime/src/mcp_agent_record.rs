//! `tilo_read` of `record:<app>:<record>`: a record of the workspace as any reader reads it, through the one record door
//! ([`tilo_actions::remote::RecordReads`]): its content as a structured value whatever app keeps
//! it, decided as the agent may read it now, and woken by the next write to it.

use super::{ActionHost, McpServer, Reply, json};
use tilo_actions::remote::{RecordAddress, RecordAsk, RecordCopy, read_records};

/// How long a record read waits for a write past the revision it was given.
const RECORD_WAIT_MS: u64 = 25_000;
/// Longest answer, in characters, a record read gives: a longer chain is read a part at a time.
const RECORD_CHARS: usize = 200_000;

impl<H: ActionHost + Send + Sync + 'static> McpServer<H> {
    /// `tilo_read` of `record:<app>:<record>`: the record (its chain of parts, when `parts`), once
    /// it differs from the revision `after` or the wait passed.
    pub(super) async fn agent_record(
        &self,
        (app, record): (&str, &str),
        parts: bool,
        after: Option<String>,
    ) -> Result<Reply, String> {
        let reads = self
            .host()
            .record_reads()
            .ok_or(tilo_actions::remote::RECORD_READS_UNAVAILABLE)?;
        let asks = [RecordAsk {
            address: RecordAddress {
                app: app.to_owned(),
                record: record.to_owned(),
            },
            joined: parts,
        }];
        let wait = if after.is_some() { RECORD_WAIT_MS } else { 0 };
        let deadline = tilo_clock::Instant::now()
            .checked_add(std::time::Duration::from_millis(wait))
            .unwrap_or_else(tilo_clock::Instant::now);
        let caller = self.execution_caller();
        // The owner captures each record on this thread's behalf, and the wait holds it.
        let readings = tokio::task::spawn_blocking(move || {
            read_records(reads.as_ref(), &caller, &asks, after.as_deref(), deadline)
        })
        .await
        .map_err(|error| format!("the record read ended: {error}"))?
        .map_err(|error| error.to_string())?;
        let mut records = Vec::new();
        for (record, copy) in readings.records {
            records.push(match copy {
                None => json!({"record": record, "arriving": true}),
                Some(RecordCopy::Absent) => json!({"record": record, "absent": true}),
                Some(copy) => json!({"record": record, "value": copy.readable()?}),
            });
        }
        let answer = json!({"revision": readings.revision, "records": records});
        let text = answer.to_string();
        if text.len() > RECORD_CHARS {
            return Err(format!(
                "the record reads as {} characters, more than {RECORD_CHARS}: read its parts one \
                 at a time, each by its own record id",
                text.len()
            ));
        }
        Ok(Reply {
            text,
            failed: false,
            structured: Some(answer),
        })
    }
}
