//! The versions written through a workspace's folder on this device, pushed to the device
//! serving the workspace. A version is this device's alone until that device keeps it.
//!
//! An editor saves a file often, and each save is a version. A file has one push going at a
//! time: the versions saved while it goes only replace the one that goes next, so a version a
//! later one replaced before it left is never pushed, and a file saved a hundred times while its
//! first push goes is pushed twice.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use iroh_blobs::Hash;
use tilo_fs::tree_sync::FILES_APP;
use tilo_lock::MutexRecover as _;
use tilo_server_protocol::WorkspaceId;
use tokio::sync::Notify;

use crate::workspaces::Workspaces;

/// How often what goes is looked at even when nothing said it moved: a host that dropped its
/// link answers nothing until it is back.
const LOOK_EVERY: Duration = Duration::from_secs(5);

/// The versions written into one workspace's files here, pushed to its host as they are written.
/// Dropping it stops the pushing; what was pushed stays in the workspace's push ledger.
pub struct WrittenPushes {
    shared: Arc<Shared>,
}

#[derive(Default)]
struct Shared {
    files: Mutex<Files>,
    wrote: Notify,
}

/// Each file's newest version not pushed yet, and what its push still waits for.
#[derive(Default)]
struct Files {
    /// The newest version of each file that was not pushed yet.
    newest: BTreeMap<u64, Hash>,
    /// The content each file's push waits for the host to keep; `None` while it is being read
    /// to push.
    going: BTreeMap<u64, Option<Vec<String>>>,
}

impl Files {
    fn wrote(&mut self, file: u64, version: Hash) {
        self.newest.insert(file, version);
    }

    /// The versions due now, each file's newest whose last push the host has kept: they are
    /// being pushed from here on.
    fn due(&mut self, going: impl Fn(&[String]) -> bool) -> Vec<(u64, Hash)> {
        self.going
            .retain(|_, waits| waits.as_deref().is_none_or(&going));
        let due: Vec<(u64, Hash)> = self
            .newest
            .iter()
            .filter(|(file, _)| !self.going.contains_key(file))
            .map(|(file, version)| (*file, *version))
            .collect();
        for (file, _) in &due {
            self.newest.remove(file);
            self.going.insert(*file, None);
        }
        due
    }

    /// `file`'s push went out, and waits for the host to keep `content`.
    fn pushed(&mut self, file: u64, content: Vec<String>) {
        self.going.insert(file, Some(content));
    }
}

impl WrittenPushes {
    /// Push what is written into `workspace`'s files, read from `store`, through `workspaces`.
    /// The pushing runs on the process's app runtime, which lives as long as the process: a
    /// runtime a caller drives only while it sets the folder up would leave it waiting.
    #[must_use]
    pub fn start(
        workspaces: Workspaces,
        workspace: WorkspaceId,
        store: Arc<tilo_fs::ContentStore>,
    ) -> Self {
        let shared = Arc::new(Shared::default());
        drop(tilo_rt::app().handle().spawn(push(
            Arc::downgrade(&shared),
            workspaces,
            workspace,
            store,
        )));
        Self { shared }
    }

    /// `file` (its inode) now holds `version`.
    pub fn wrote(&self, file: u64, version: Hash) {
        self.shared.files.lock_recover().wrote(file, version);
        self.shared.wrote.notify_one();
    }
}

/// Push each file's newest version once its last push is kept, for as long as `shared` lives.
async fn push(
    shared: Weak<Shared>,
    workspaces: Workspaces,
    workspace: WorkspaceId,
    store: Arc<tilo_fs::ContentStore>,
) {
    let name = workspace.to_string();
    loop {
        let Some(held) = shared.upgrade() else {
            return;
        };
        let moved = workspaces.content_moved();
        let due = held
            .files
            .lock_recover()
            .due(|content| workspaces.any_content_going(&name, content));
        if due.is_empty() {
            tokio::select! {
                () = held.wrote.notified() => {}
                () = moved => {}
                () = tokio::time::sleep(LOOK_EVERY) => {}
            }
            continue;
        }
        drop(moved);
        let (copies, store, at) = (workspaces.clone(), Arc::clone(&store), name.clone());
        // A version's manifest is read from the disk: on a thread of its own.
        let pushed = tokio::task::spawn_blocking(move || {
            due.into_iter()
                .map(|(file, version)| (file, push_version(&copies, &at, &store, version)))
                .collect::<Vec<_>>()
        })
        .await
        .unwrap_or_else(|error| std::panic::resume_unwind(error.into_panic()));
        let mut files = held.files.lock_recover();
        for (file, content) in pushed {
            files.pushed(file, content);
        }
    }
}

/// Push `version`, a version this device wrote into the files of `workspace`, to the device
/// serving the workspace: its manifest and every chunk it lists, each going until that device
/// keeps it ([`Workspaces::push_content`]). The content pushed, by hash.
fn push_version(
    workspaces: &Workspaces,
    workspace: &str,
    store: &tilo_fs::ContentStore,
    version: Hash,
) -> Vec<String> {
    let manifest_hash = version.to_hex();
    workspaces.push_content(workspace, FILES_APP, &manifest_hash);
    let mut pushed = vec![manifest_hash];
    let Some(manifest) = store.local_now(version) else {
        return pushed;
    };
    let manifest = match tilo_fs::content::Manifest::decode(&manifest) {
        Ok(manifest) => manifest,
        Err(error) => {
            tracing::error!(%workspace, %version, %error, "a version this device wrote names no manifest, so its bytes stay here");
            return pushed;
        }
    };
    for chunk in manifest.chunks() {
        let hash = Hash::from(chunk.hash).to_hex();
        workspaces.push_content(workspace, FILES_APP, &hash);
        pushed.push(hash);
    }
    pushed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(n: u8) -> Hash {
        Hash::new([n])
    }

    /// An editor saving a file a hundred times while its first push goes pushes it twice: the
    /// first save, and the last once the first is kept. A version replaced before it left never
    /// goes, and another file's saves are not held up by it.
    #[test]
    fn saves_while_a_push_goes_push_only_the_newest_once_it_is_kept() {
        let mut files = Files::default();
        let kept = std::cell::RefCell::new(std::collections::BTreeSet::<String>::new());
        let going = |content: &[String]| content.iter().any(|hash| !kept.borrow().contains(hash));

        files.wrote(1, version(0));
        assert_eq!(files.due(going), vec![(1, version(0))]);
        files.pushed(1, vec![version(0).to_hex()]);

        for n in 1..=100 {
            files.wrote(1, version(n));
        }
        files.wrote(2, version(200));
        assert_eq!(
            files.due(going),
            vec![(2, version(200))],
            "the first file's push still goes; the other file goes at once"
        );
        assert!(files.due(going).is_empty(), "nothing is pushed twice");

        kept.borrow_mut().insert(version(0).to_hex());
        assert_eq!(
            files.due(going),
            vec![(1, version(100))],
            "once the first push is kept, only the newest save goes"
        );
    }

    /// A file saved while its version is still being read to push waits for that push, rather
    /// than going beside it.
    #[test]
    fn a_save_while_its_push_is_read_waits_for_it() {
        let mut files = Files::default();
        let going = |_: &[String]| false;
        files.wrote(1, version(1));
        assert_eq!(files.due(going), vec![(1, version(1))]);
        files.wrote(1, version(2));
        assert!(files.due(going).is_empty(), "the first is still being read");
        files.pushed(1, Vec::new());
        assert_eq!(files.due(going), vec![(1, version(2))]);
    }
}
