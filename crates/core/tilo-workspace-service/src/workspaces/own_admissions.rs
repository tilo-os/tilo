//! What this device and its keys signed into a workspace as their own admission, as its store
//! keeps it: the changes a key's next admission there builds on, so it never signs two changes under one number
//! ([`tilo_peer_state::own_admission::sign_entry`]). A copy of the workspace kept here holds them
//! in its history. Before one is, the file beside where the copy will be keeps what the
//! device's session signed and sent to the workspace's host: a hand-over of the workspace that
//! arrives before the host's answer, or after a restart, finds them there.

use super::{WorkspaceId, Workspaces};

/// The extension of the files that keep what a key of this device signed into a workspace it
/// keeps no copy of yet.
const SIGNED: &str = "signed";

/// One write to the files [`SIGNED`] names at a time: two sign-ins at once each read what the
/// other wrote.
static WRITING: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn writing() -> std::sync::MutexGuard<'static, ()> {
    WRITING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Workspaces {
    fn signed_path(&self, workspace: WorkspaceId, device: &[u8; 32]) -> std::path::PathBuf {
        self.inner
            .dir
            .join(format!("{workspace}.{}.{SIGNED}", hex::encode(device)))
    }

    fn signed_before(&self, workspace: WorkspaceId, device: &[u8; 32]) -> Vec<Vec<u8>> {
        let path = self.signed_path(workspace, device);
        match std::fs::read(&path) {
            Ok(bytes) => postcard::from_bytes(&bytes).unwrap_or_else(|error| {
                tracing::warn!(path = %path.display(), %error, "what this device signed into a workspace does not read");
                Vec::new()
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "what this device signed into a workspace was not read");
                Vec::new()
            }
        }
    }

    /// What `device`, this device or a key of it, signed into `workspace` as its own admission,
    /// with what it builds on: what the copy here holds of it, and what it signed before this
    /// device kept one. Empty where it signed none. A caller that signs on top of it has the
    /// key's turn ([`tilo_peer_state::own_admission::signing_turn`]).
    #[must_use]
    pub fn own_admissions(&self, workspace: WorkspaceId, device: &[u8; 32]) -> Vec<Vec<u8>> {
        let copy = self.copy_now(workspace);
        let mut admissions = copy
            .as_ref()
            .map(|kept| kept.peer.own_admissions(device))
            .unwrap_or_default();
        let _writing = writing();
        let before = self.signed_before(workspace, device);
        let holds = |bytes: &Vec<u8>| {
            copy.as_ref().is_some_and(|kept| {
                automerge::Change::try_from(bytes.as_slice())
                    .is_ok_and(|change| kept.peer.workspace_holds_change(&change.hash()))
            })
        };
        if !before.is_empty() && before.iter().all(holds) {
            // The copy holds all of it: the file has done its part.
            std::fs::remove_file(self.signed_path(workspace, device)).ok();
        }
        for change in before {
            if !admissions.contains(&change) {
                admissions.push(change);
            }
        }
        admissions
    }

    /// Keep `changes`, what `device` signed into `workspace` as its own admission and sends
    /// its host, for its next admission there to build on ([`Self::own_admissions`]).
    ///
    /// # Errors
    /// The file is not written.
    pub fn keep_own_admission(
        &self,
        workspace: WorkspaceId,
        device: &[u8; 32],
        changes: &[Vec<u8>],
    ) -> Result<(), String> {
        if changes.is_empty() {
            return Ok(());
        }
        let _writing = writing();
        let mut kept = self.signed_before(workspace, device);
        for change in changes {
            if !kept.contains(change) {
                kept.push(change.clone());
            }
        }
        let bytes = postcard::to_stdvec(&kept).map_err(|error| error.to_string())?;
        crate::workspace_files::atomic_write(&self.signed_path(workspace, device), &bytes)
            .map_err(|error| error.to_string())
    }

    /// Forget what `device` signed into `workspace` before this device kept a copy of it: the
    /// workspace refused it, so no copy holds it, and the device's next admission there signs
    /// its first change again rather than build on a refusal every copy would refuse after it.
    pub fn forget_own_admission(&self, workspace: WorkspaceId, device: &[u8; 32]) {
        let _writing = writing();
        std::fs::remove_file(self.signed_path(workspace, device)).ok();
    }
}
