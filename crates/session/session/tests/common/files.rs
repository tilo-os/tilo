//! A files tree a device mounts, as the daemon serving its mount holds it.

use std::sync::{Arc, Mutex};

use fs::namespace::{Namespace, ROOT_ID};

use fs_protocol::files::FileNodeKind;
use tilo_lock::MutexRecover as _;

/// A tree this device mounts, as the daemon serving the mount holds it.
pub type Mounted = Arc<Mutex<Namespace>>;

/// Every path `namespace`'s tree holds, out of the trash, sorted.
pub fn contents(namespace: &Mounted) -> Vec<String> {
    fn walk(namespace: &Namespace, parent: &str, prefix: &str, found: &mut Vec<String>) {
        for id in namespace.child_node_ids(parent) {
            let Some(entry) = namespace.entry_for_node(&id) else {
                continue;
            };
            let name = String::from_utf8_lossy(entry.name.as_bytes()).into_owned();
            let path = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            found.push(path.clone());
            if entry.kind == FileNodeKind::Directory {
                walk(namespace, &id, &path, found);
            }
        }
    }
    let namespace = namespace.lock_recover();
    let mut found = Vec::new();
    walk(&namespace, ROOT_ID, "", &mut found);
    found.sort();
    found
}

/// A workspace's tree as `device`'s files daemon mounts it, synced while this value lives.
pub struct Mount {
    tree: fs::files_sync::StoreSync,
    service: fs::files_sync::remote::Service,
    store: Arc<fs::ContentStore>,
    _kept: tempfile::TempDir,
}

impl Mount {
    /// The tree the mount holds, as the daemon's mount reads and changes it.
    pub fn namespace(&self) -> Mounted {
        Arc::clone(&self.tree.namespace)
    }

    /// The rest of what the session's connection carries: file bytes, keys and access.
    #[allow(
        dead_code,
        reason = "not every test of the tree reads its files' bytes"
    )]
    pub fn service(&self) -> &fs::files_sync::remote::Service {
        &self.service
    }

    /// The mount's store of its files' bytes.
    #[allow(
        dead_code,
        reason = "not every test of the tree reads its files' bytes"
    )]
    pub fn store(&self) -> &Arc<fs::ContentStore> {
        &self.store
    }
}

/// Mount `workspace`'s tree as `device`'s files daemon does: synced through the device's own
/// workspace service, on the socket its session serves the trees on.
pub fn mount_on(device: &crate::common::people::Device, workspace: &str) -> Mount {
    let now_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("the clock reads after the epoch")
        .as_nanos();
    let credentials = fs::identity::load_certified_mount_identity(
        &device.identity(),
        u64::try_from(now_ns).expect("the clock reads a time a certificate holds"),
    )
    .expect("this device's certificate is valid")
    .into_parts()
    .1;
    let kept = tempfile::tempdir().expect("a directory for the mount's tree");
    let socket = device
        .directory()
        .join("runtime")
        .join("tilo/services")
        .join(format!("{}.sock", fs::files_sync::SOCKET));
    let mount = fs::namespace::MountAuthor::person(
        hex::encode(credentials.device_public_key()),
        tilo_actions::MemberId::person(tilo_actions::PersonId::from_root(
            credentials.user_public_key(),
        )),
    )
    .in_workspace(workspace.to_owned());
    let (tree, service, store) = tokio::runtime::Handle::current().block_on(async {
        let content = Arc::new(
            fs::ContentStore::open(kept.path().join("content"))
                .await
                .expect("a content store for the mount"),
        );
        let (tree, service) = fs::files_sync::remote::start(
            socket,
            workspace.to_owned(),
            mount,
            Arc::clone(&content),
        )
        .await
        .expect("the mount's tree opens");
        (tree, service, content)
    });
    Mount {
        tree,
        service,
        store,
        _kept: kept,
    }
}
