//! A files tree a device mounts, as the daemon serving its mount holds it.

use std::sync::{Arc, Mutex};

use fs::namespace::Namespace;
use fs_protocol::files::FileNodeKind;
use fs_protocol::tree::{ROOT_ID, Tree, TreeChunks, chunk_document_id};
use tilo_lock::MutexRecover as _;

/// A tree document this device mounts, as the daemon serving the mount holds it.
pub type Mounted = Arc<Mutex<Namespace>>;

/// A tree document nothing has mounted yet, as a device mounts a workspace it
/// holds no tree of.
pub fn namespace(author: &str) -> Mounted {
    Arc::new(Mutex::new(Namespace::new(author).expect("a tree document")))
}

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

/// Publish `namespace` on `cluster` as `device`'s mount does: the daemon of
/// that device, on the collaboration socket its session serves.
pub fn mount_on(
    device: &crate::common::people::Device,
    namespace: &Mounted,
    cluster: &str,
) -> tokio::task::JoinHandle<()> {
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
    fs::tree_document::spawn(
        Arc::clone(namespace),
        cluster.to_owned(),
        credentials,
        device
            .directory()
            .join("runtime")
            .join("tilo/services/collab.sock"),
        device
            .directory()
            .join("runtime")
            .join("tilo/services/identity.sock"),
    )
}
