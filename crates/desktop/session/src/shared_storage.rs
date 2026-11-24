//! What each of the person's devices says it holds of its workspaces: this device keeps what its
//! storage reading splits per workspace in the record of the person's devices, and every device's
//! is read back from that record, with when it was last with a host.
//!
//! The filesystem daemon counts the bytes; the record carries them so a device the person looks
//! at sees what their other devices hold without asking each one. Nothing here names a file: the
//! split is counts per workspace.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use tilo_system_resources::storage::{STORAGE as STORAGE_RESOURCE, Storage, WorkspaceHold};

use crate::owner_session::OwnerDevices;

/// The fact under which a device keeps what it holds in its entry of the record of the person's
/// devices: [`WorkspaceHold`]s, postcard in hex.
pub(crate) const STORAGE: &str = "storage";

/// The most workspaces one device says it holds: a workspace is one share of one person's, and a
/// device mounts a handful, so a value claiming more is not one this reads.
const MAX_WORKSPACES: usize = 64;

/// The longest name one of a device's workspaces takes.
const MAX_NAME: usize = 256;

/// How stale this device's entry may grow before its reading is read again. The split moves as
/// files are fetched and evicted, which the record never sees; it costs a scan to know.
const READ_AGAIN: Duration = Duration::from_secs(30);

/// `holds` as the record keeps them: postcard in hex.
fn encoded(holds: &[WorkspaceHold]) -> Option<String> {
    postcard::to_allocvec(holds)
        .inspect_err(|error| tracing::warn!(%error, "what this device holds does not encode"))
        .ok()
        .map(hex::encode)
}

/// The holds a device keeps in `value`, where they hold: no more workspaces than a device mounts,
/// none named twice, and every name a name.
pub(crate) fn decoded(value: &str) -> Option<Vec<WorkspaceHold>> {
    let holds: Vec<WorkspaceHold> = crate::platform::fabric::decoded(value)?;
    let names: BTreeSet<&str> = holds.iter().map(|held| held.workspace.as_str()).collect();
    (holds.len() <= MAX_WORKSPACES
        && names.len() == holds.len()
        && holds
            .iter()
            .all(|held| !held.workspace.is_empty() && held.workspace.len() <= MAX_NAME))
    .then_some(holds)
}

/// Keeps this device's entry in step with its storage reading, so the person's other devices read
/// what it holds.
#[derive(Default)]
pub(crate) struct Publisher {
    /// What the last pass kept, so a reading that says the same writes nothing.
    kept: Option<Vec<WorkspaceHold>>,
    /// When the reading was last read: the split moves with no record revision.
    read_at: Option<Instant>,
}

impl Publisher {
    /// Keep what this device's storage reading splits per workspace, once it read it and found it
    /// says something else.
    pub(crate) fn follow(&mut self, devices: &OwnerDevices, services: &tilo_service::Services) {
        let now = Instant::now();
        if self
            .read_at
            .is_some_and(|read| now.duration_since(read) < READ_AGAIN)
        {
            return;
        }
        let Some(reading) = services.read(STORAGE_RESOURCE) else {
            return;
        };
        let Ok(storage) = serde_json::from_value::<Storage>(reading) else {
            tracing::warn!("the storage reading does not read back as its type");
            return;
        };
        // Before the first scan answers, the reading holds nothing yet: saying so would tell the
        // person's other devices this device holds nothing of their workspaces.
        if !storage.scanned {
            return;
        }
        self.read_at = Some(now);
        let holds = storage.residency;
        if self.kept.as_ref() == Some(&holds) {
            return;
        }
        if let Some(value) = encoded(&holds) {
            devices.keep_own(STORAGE, &value);
        }
        self.kept = Some(holds);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn held(workspace: &str, only_here: u64) -> WorkspaceHold {
        WorkspaceHold {
            workspace: workspace.to_owned(),
            only_here_bytes: only_here,
            ..WorkspaceHold::default()
        }
    }

    #[test]
    fn what_a_device_holds_round_trips_through_its_entry() {
        let holds = vec![held("studio", 900), held("archive", 0)];
        let value = encoded(&holds).expect("a split encodes");
        assert_eq!(decoded(&value), Some(holds));
    }

    #[test]
    fn a_value_that_is_no_split_is_ignored() {
        let twice = vec![held("studio", 1), held("studio", 2)];
        assert!(
            decoded(&encoded(&twice).unwrap()).is_none(),
            "a workspace named twice is no split"
        );
        assert!(decoded(&encoded(&[held("", 1)]).unwrap()).is_none());
        assert!(decoded("not hex").is_none());
        let many: Vec<WorkspaceHold> = (0..=MAX_WORKSPACES)
            .map(|at| held(&format!("w{at}"), 1))
            .collect();
        assert!(
            decoded(&encoded(&many).unwrap()).is_none(),
            "a device mounts a handful of workspaces"
        );
    }
}
