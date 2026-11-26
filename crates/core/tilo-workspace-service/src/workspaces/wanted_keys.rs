//! A workspace's keys are looked up under its lock and waited for with the lock let go.
//!
//! A write is drafted and kept under its workspace's lock, and the device's key service can take
//! up to [`OPEN_WAIT`](super::sealed_documents::OPEN_WAIT) to open a group: the first write of a
//! document sealed to its own readers founds that group, and a device that just came to a
//! workspace waits to be let into it. Waited for under the lock, that would hold up every other
//! user of the workspace for as long (a hand-over catching the workspace up gives up first). So
//! under the lock a write only looks the keys up; when they are not here it lets the lock go,
//! waits for them, and takes the lock again to make the write anew against the workspace as it
//! stands then. What reads a record under the lock on its own (a compaction, a checkpoint's
//! check) asks for the keys in the background instead, and reads it the next time it runs.

use std::sync::Arc;

use super::sealed_documents::DocumentKeys;
use super::{DeviceKeys, HostedWorkspace};
use crate::documents::sealed::DocumentKeyAsk;

/// The keys of a group a write needs and this device does not hold yet: `readers`' group `group`
/// ([`GroupKeys::group_keys`](super::GroupKeys::group_keys)).
pub(in crate::workspaces) struct WantedKeys {
    device: DeviceKeys,
    group: String,
    readers: String,
}

impl WantedKeys {
    pub(in crate::workspaces) fn new(device: &DeviceKeys, group: String, readers: String) -> Self {
        Self {
            device: Arc::clone(device),
            group,
            readers,
        }
    }

    /// Wait for the keys, opening the group when it has none yet: called with no workspace's
    /// lock taken.
    async fn arrive(&self) -> Result<(), String> {
        match self
            .device
            .group_keys(&self.group, &self.readers, true)
            .await
        {
            Some(Ok(_)) => Ok(()),
            Some(Err(error)) => Err(error),
            None => Err("this device holds no keys of the document's readers yet".into()),
        }
    }

    /// Ask for the keys on a task of their own, which holds no workspace's lock: what reads a
    /// record under the lock without them reads it once they are here, the next time it runs.
    pub(in crate::workspaces) fn ask_later(self) {
        tokio::spawn(async move {
            if let Err(error) = self.arrive().await {
                tracing::debug!(group = %self.group, %error, "a group's keys are not here yet");
            }
        });
    }
}

/// The keys `ask` names, looked up and never waited for: what a holder of a workspace's lock
/// reads a document's keys with. [`Unkept::Keys`] when this device holds none of the group yet.
pub(in crate::workspaces) async fn looked_up(
    device: &DeviceKeys,
    ask: DocumentKeyAsk,
) -> Result<DocumentKeys, Unkept> {
    match device.group_keys(&ask.group, ask.readers(), false).await {
        Some(Ok(keys)) => Ok(ask.with_keys(device, keys).await),
        Some(Err(_)) | None => Err(Unkept::Keys(WantedKeys::new(
            device,
            ask.group.clone(),
            ask.readers().to_owned(),
        ))),
    }
}

/// Why a write under its workspace's lock did not go through.
pub(in crate::workspaces) enum Unkept {
    /// It was refused, saying why.
    Refused(String),
    /// It needs keys this device does not hold yet: they are waited for with the lock let go,
    /// and the write is made again.
    Keys(WantedKeys),
}

impl From<String> for Unkept {
    fn from(why: String) -> Self {
        Self::Refused(why)
    }
}

impl From<&str> for Unkept {
    fn from(why: &str) -> Self {
        Self::Refused(why.to_owned())
    }
}

impl std::fmt::Debug for Unkept {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(why) => f.write_str(why),
            Self::Keys(wanted) => write!(f, "waiting for the keys of {}", wanted.group),
        }
    }
}

/// The waits of one write for its keys: the group it last waited for, so keys that arrived and
/// are not there again when the write looks under the lock end the write rather than loop.
#[derive(Default)]
pub(in crate::workspaces) struct KeyWaits(Option<(String, String)>);

impl KeyWaits {
    /// Wait for the keys `unkept` names, with no workspace's lock taken: `Ok` to make the write
    /// again, else why it is refused.
    pub(in crate::workspaces) async fn wait(&mut self, unkept: Unkept) -> Result<(), String> {
        let wanted = match unkept {
            Unkept::Refused(why) => return Err(why),
            Unkept::Keys(wanted) => wanted,
        };
        let group = (wanted.group.clone(), wanted.readers.clone());
        if self.0.as_ref() == Some(&group) {
            return Err(format!(
                "the keys of {} arrived and were gone again when the write looked for them",
                wanted.group
            ));
        }
        wanted.arrive().await?;
        self.0 = Some(group);
        Ok(())
    }
}

/// Make `write` under `state`'s lock until it goes through or is refused: when it needs keys this
/// device does not hold yet, the lock is let go (`write` owns the guard and drops it as it
/// answers), the keys are waited for, and `write` runs again under a new hold of the lock.
pub(in crate::workspaces) async fn write_with_keys<T, F, Fut>(
    state: &Arc<tokio::sync::Mutex<HostedWorkspace>>,
    mut write: F,
) -> Result<T, String>
where
    F: FnMut(tokio::sync::OwnedMutexGuard<HostedWorkspace>) -> Fut,
    Fut: Future<Output = Result<T, Unkept>>,
{
    let mut waits = KeyWaits::default();
    loop {
        let hosted = Arc::clone(state).lock_owned().await;
        match write(hosted).await {
            Ok(written) => return Ok(written),
            Err(unkept) => waits.wait(unkept).await?,
        }
    }
}
