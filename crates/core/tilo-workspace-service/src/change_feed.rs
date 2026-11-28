//! What changed in the records of the workspaces a device holds: the feed a watch of a whole
//! workspace reads ([`tilo_actions::remote::ChangeFeed`]), taken from the device's documents.
//! The daemon answers its callers with it, reading the documents its workspace service holds
//! ([`DocumentsFeed::page_of_service`]), and a desktop's session with the same feed over the
//! documents it reads ([`DocumentsFeed::page_of`]).
//!
//! A caller follows the workspace it works in. Each workspace's log is kept for as long as the
//! feed is, whoever asks, so every connection reads the same changes in the same order. A record
//! is asked for since the heads the log last read it at, so one that stands where it stood is
//! not read again. The workspace's members are a part of its governance, not a document it
//! lists: each change of them its governance names is told as a change of their resource
//! ([`MEMBERS_RESOURCE`]).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use collab_types::history::feed::{ChangeLog, DocumentKey};
use collab_types::history::words::HistoryWords;
use tilo_actions::remote::{ActionCallError, Caller, ChangePage};
use tilo_lock::MutexRecover as _;
use tilo_peer_state::workspace_documents::{DeviceDocuments, DocumentRead, MemberChange};

use crate::device_documents::ServiceDocuments;

/// One record as a round of the feed read it: where it stands, and the record whole when it
/// moved; `None` for a record that cannot be read now.
type Read = (DocumentKey, Option<([u8; 32], Option<Vec<u8>>)>);

/// Each workspace's log of its records' changes, each change said in `words`.
pub struct DocumentsFeed {
    words: Arc<dyn HistoryWords + Send + Sync>,
    logs: Mutex<HashMap<String, ChangeLog>>,
}

impl DocumentsFeed {
    #[must_use]
    pub fn new(words: Arc<dyn HistoryWords + Send + Sync>) -> Self {
        Self {
            words,
            logs: Mutex::new(HashMap::new()),
        }
    }

    /// The changes after `after` in the workspace `caller` works in, its records read from
    /// `reads`.
    ///
    /// # Errors
    /// The caller works in no workspace, or its records cannot be listed.
    pub fn page_of(
        &self,
        reads: &dyn DeviceDocuments,
        caller: &Caller,
        after: Option<&str>,
    ) -> Result<ChangePage, ActionCallError> {
        let workspace = working_in(caller)?;
        let records = listed(reads.listing(workspace))?;
        let members = members(reads.member_changes(workspace))?;
        let read = records
            .into_iter()
            .map(|record| {
                let since = self.heads_of(workspace, &record);
                let read = reads.read(workspace, &record.0, &record.1, since.as_deref());
                let read = match read {
                    Ok(DocumentRead::Changes { .. }) => {
                        reads.read(workspace, &record.0, &record.1, None)
                    }
                    read => read,
                };
                (record, stood(read))
            })
            .collect();
        Ok(self.take_in(workspace, (read, members), caller, after))
    }

    /// [`Self::page_of`] of the documents the device's own workspace service holds, each read
    /// awaited on the service's runtime.
    ///
    /// # Errors
    /// As [`Self::page_of`].
    pub async fn page_of_service(
        &self,
        reads: &ServiceDocuments,
        caller: &Caller,
        after: Option<&str>,
    ) -> Result<ChangePage, ActionCallError> {
        let workspace = working_in(caller)?;
        let records = listed(reads.listing(workspace))?;
        let members = members(reads.member_changes(workspace))?;
        let mut read = Vec::with_capacity(records.len());
        for record in records {
            let since = self.heads_of(workspace, &record).unwrap_or_default();
            let key = (record.0.as_str(), record.1.as_str());
            let mut now = reads.read_awaited(workspace, key, &since).await;
            if matches!(now, Ok(DocumentRead::Changes { .. })) {
                now = reads.read_awaited(workspace, key, &[]).await;
            }
            read.push((record, stood(now)));
        }
        Ok(self.take_in(workspace, (read, members), caller, after))
    }

    fn heads_of(&self, workspace: &str, record: &DocumentKey) -> Option<Vec<[u8; 32]>> {
        self.logs
            .lock_recover()
            .get(workspace)
            .and_then(|log| log.heads_of(record))
    }

    /// Take `read` and the changes of the `members` into the workspace's log, and answer the
    /// changes after `after` that `caller` reads.
    fn take_in(
        &self,
        workspace: &str,
        (read, members): (Vec<Read>, Vec<MemberChange>),
        caller: &Caller,
        after: Option<&str>,
    ) -> ChangePage {
        let records: Vec<DocumentKey> = read.iter().map(|(record, _)| record.clone()).collect();
        let stood: HashMap<DocumentKey, ([u8; 32], Option<Vec<u8>>)> = read
            .into_iter()
            .filter_map(|(record, now)| Some((record, now?)))
            .collect();
        let mut logs = self.logs.lock_recover();
        let log = logs.entry(workspace.to_owned()).or_default();
        log.take_in_named(
            &(
                tilo_action_policy::agent_answers::WORKSPACE_APP.to_owned(),
                MEMBERS_RESOURCE.to_owned(),
            ),
            &members
                .into_iter()
                .map(|change| (change.change, change.message, change.time, change.subjects))
                .collect::<Vec<_>>(),
        );
        let revisions: HashMap<DocumentKey, [u8; 32]> = stood
            .iter()
            .map(|(record, (revision, _))| (record.clone(), *revision))
            .collect();
        let stood = std::cell::RefCell::new(stood);
        log.take_in(
            &records,
            |record| revisions.get(record).copied(),
            |record| {
                stood
                    .borrow_mut()
                    .get_mut(record)
                    .and_then(|(_, bytes)| bytes.take())
            },
            self.words.as_ref(),
        );
        let page = log.page(after, caller.acting().member_id());
        drop(logs);
        page
    }
}

/// The workspace `caller` works in, which its watch follows.
fn working_in(caller: &Caller) -> Result<&str, ActionCallError> {
    caller
        .workspace()
        .ok_or_else(|| ActionCallError::Unavailable {
            reason: "a watch of a workspace follows the workspace its caller works in, and this \
                 caller works in none"
                .into(),
        })
}

/// The id of the resource a workspace's members are, as its package and every view name it: the
/// Workspace package's `members` root, which every member reads.
pub const MEMBERS_RESOURCE: &str = "workspace/members";

fn members(
    changes: Result<Vec<MemberChange>, String>,
) -> Result<Vec<MemberChange>, ActionCallError> {
    changes.map_err(|reason| ActionCallError::Unavailable {
        reason: format!("the workspace's members are not read: {reason}"),
    })
}

fn listed(
    listing: Result<Vec<tilo_actions::remote::ListedResource>, String>,
) -> Result<Vec<DocumentKey>, ActionCallError> {
    listing
        .map(|listed| {
            listed
                .into_iter()
                .map(|listed| (listed.address.app, listed.address.record))
                .collect()
        })
        .map_err(|reason| ActionCallError::Unavailable {
            reason: format!("the workspace's records are not read: {reason}"),
        })
}

/// Where a record stands after `read`: its revision, with the record whole when it was read
/// whole.
fn stood(read: Result<DocumentRead, String>) -> Option<([u8; 32], Option<Vec<u8>>)> {
    match read.ok()? {
        DocumentRead::Whole {
            revision, bytes, ..
        } => Some((revision, Some(bytes))),
        DocumentRead::Unchanged(revision) => Some((revision, None)),
        DocumentRead::Changes { .. } | DocumentRead::Absent | DocumentRead::Pending => None,
    }
}
