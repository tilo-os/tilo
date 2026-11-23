//! A files tree a device mounts, as the daemon serving its mount holds it.

use std::sync::{Arc, Mutex};

use fs::namespace::Namespace;

use fs_protocol::files::FileNodeKind;
use fs_protocol::tree::{ROOT_ID, Tree, TreeChunks, chunk_document_id};
use tilo_lock::MutexRecover as _;

/// A tree document this device mounts, as the daemon serving the mount holds it.
pub type Mounted = Arc<Mutex<Namespace>>;

/// Every path `namespace`'s tree holds as the records read it — the projection
/// the Files app renders — sorted, each folder's contents by its folder's path.
pub fn contents(namespace: &Mounted) -> Vec<String> {
    fn walk(tree: &Tree, parent: &str, prefix: &str, found: &mut Vec<String>) {
        for (id, entry) in tree.children(parent) {
            let name = String::from_utf8_lossy(entry.name.as_bytes()).into_owned();
            let path = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            found.push(path.clone());
            if entry.kind == FileNodeKind::Directory {
                walk(tree, id, &path, found);
            }
        }
    }
    // The tree is the records that hold it: its own record, then each chunk.
    let mut chunks = TreeChunks::default();
    {
        let mut namespace = namespace.lock_recover();
        for chunk in namespace.records() {
            let Some(bytes) = namespace.save_document(chunk) else {
                continue;
            };
            chunks
                .read_resource(&chunk_document_id(chunk), &bytes)
                .expect("a record of the tree reads");
        }
    }
    let tree = chunks.tree();
    let mut found = Vec::new();
    walk(&tree, ROOT_ID, "", &mut found);
    found.sort();
    found
}

/// A workspace's tree as `device`'s files daemon mounts it, synced while this value lives.
pub struct Mount {
    tree: fs::tree_sync::TreeSync,
    service: fs::tree_sync::remote::Service,
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
    pub fn service(&self) -> &fs::tree_sync::remote::Service {
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
    let socket = device.directory().join("runtime/tilo/files-tree.sock");
    let (tree, service, store) = tokio::runtime::Handle::current().block_on(async {
        let content = Arc::new(
            fs::ContentStore::open(kept.path().join("content"))
                .await
                .expect("a content store for the mount"),
        );
        let (tree, service) = fs::tree_sync::remote::start(
            socket,
            workspace.to_owned(),
            (
                credentials.device_public_key(),
                credentials.user_public_key(),
            ),
            kept.path().join("namespace"),
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
