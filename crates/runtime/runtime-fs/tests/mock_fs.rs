//! Integration test: end-to-end round-trip over a socket pair against an
//! in-memory server speaking the filesystem protocol's requests and replies.
//!
//! Covers the framed connection apps drive, the request/reply shapes of the
//! workspace and file families, and the idiomatic `FsClient` wrappers (`open`,
//! `list`, `stat`, `FileHandle::write_at`/`read_all`).
//!
//! The backend is an in-memory map; directories are inferred from
//! path prefixes rather than represented explicitly, so the test only
//! exercises write→read on files and a single list.

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    clippy::string_slice,
    clippy::unwrap_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use runtime_fs::connection::{Capabilities, Connection, Object, ServeObject, serve_connected};
use runtime_fs::wire::{FsReply, FsRequest};
use runtime_fs::{DirEntry, EntryKind, FsClient, Metadata, OpenMode};

type Store = Arc<Mutex<BTreeMap<String, Vec<u8>>>>;

/// What one served object answers with.
type Dispatch = Pin<Box<dyn Future<Output = Result<FsReply, String>> + Send + 'static>>;

fn ready(reply: Result<FsReply, String>) -> Dispatch {
    Box::pin(std::future::ready(reply))
}

fn normalise(path: &str) -> String {
    path.trim_start_matches('/').to_owned()
}

fn metadata(size: u64) -> Metadata {
    Metadata {
        kind: EntryKind::File,
        size,
        modified_unix_seconds: Some(0),
        needs_hydration: false,
    }
}

struct FakeFile {
    store: Store,
    path: String,
}

impl ServeObject for FakeFile {
    fn dispatch(self: Arc<Self>, _: &Capabilities, request: FsRequest) -> Dispatch {
        match request {
            FsRequest::Read { offset, max_len } => {
                let store = self.store.lock().expect("store");
                let Some(bytes) = store.get(&self.path) else {
                    return ready(Err("file missing".into()));
                };
                let offset = usize::try_from(offset).expect("offset fits usize");
                let start = offset.min(bytes.len());
                let end = start.saturating_add(max_len as usize).min(bytes.len());
                ready(Ok(FsReply::Bytes(Some(bytes[start..end].to_vec()))))
            }
            FsRequest::Write { offset, bytes } => {
                let offset = usize::try_from(offset).expect("offset fits usize");
                let mut store = self.store.lock().expect("store");
                let entry = store.entry(self.path.clone()).or_default();
                if entry.len() < offset {
                    entry.resize(offset, 0);
                }
                // In-place splice: overwrite [offset..offset+len), extending
                // as needed.
                for (i, byte) in bytes.iter().enumerate() {
                    let dst = offset.saturating_add(i);
                    if dst < entry.len() {
                        entry[dst] = *byte;
                    } else {
                        entry.push(*byte);
                    }
                }
                ready(Ok(FsReply::Written(Some(
                    u32::try_from(bytes.len()).expect("write size fits u32"),
                ))))
            }
            FsRequest::Flush => ready(Ok(FsReply::Ack(runtime_fs::wire::NativeEntryStatus::Ok))),
            FsRequest::HandleMetadata => {
                let size = self
                    .store
                    .lock()
                    .expect("store")
                    .get(&self.path)
                    .map_or(0, |bytes| bytes.len() as u64);
                ready(Ok(FsReply::Metadata(Some(metadata(size)))))
            }
            _ => ready(Err("unexpected file request".into())),
        }
    }
}

struct FakeFs {
    store: Store,
}

impl ServeObject for FakeFs {
    fn dispatch(self: Arc<Self>, capabilities: &Capabilities, request: FsRequest) -> Dispatch {
        match request {
            FsRequest::Open { path, mode } => {
                let path = normalise(&path);
                // Truncate on write; leave alone on append/read.
                if mode == OpenMode::Write {
                    self.store
                        .lock()
                        .expect("store")
                        .insert(path.clone(), Vec::new());
                } else if mode == OpenMode::Append {
                    self.store
                        .lock()
                        .expect("store")
                        .entry(path.clone())
                        .or_default();
                }
                let file = FakeFile {
                    store: Arc::clone(&self.store),
                    path,
                };
                ready(Ok(FsReply::Handle(Some(
                    capabilities.register(Arc::new(file)),
                ))))
            }
            FsRequest::List { path } => {
                let path = normalise(&path);
                let prefix = if path.is_empty() {
                    String::new()
                } else {
                    format!("{path}/")
                };
                let store = self.store.lock().expect("store");
                let mut entries: Vec<DirEntry> = store
                    .iter()
                    .filter_map(|(key, bytes)| {
                        let rest = key.strip_prefix(&prefix)?;
                        // Only direct children (no further '/').
                        if rest.contains('/') {
                            return None;
                        }
                        Some(DirEntry {
                            name: rest.to_owned(),
                            metadata: metadata(bytes.len() as u64),
                        })
                    })
                    .collect();
                entries.sort_by(|a, b| a.name.cmp(&b.name));
                ready(Ok(FsReply::Entries(Some(entries))))
            }
            FsRequest::Stat { path } => {
                let store = self.store.lock().expect("store");
                let Some(bytes) = store.get(&normalise(&path)) else {
                    return ready(Err("no such path".into()));
                };
                ready(Ok(FsReply::Metadata(Some(metadata(bytes.len() as u64)))))
            }
            // Directories are implicit in this test.
            FsRequest::Mkdir { .. } => {
                ready(Ok(FsReply::Ack(runtime_fs::wire::NativeEntryStatus::Ok)))
            }
            FsRequest::Remove { path } => {
                self.store.lock().expect("store").remove(&normalise(&path));
                ready(Ok(FsReply::Ack(runtime_fs::wire::NativeEntryStatus::Ok)))
            }
            _ => ready(Err("unexpected workspace request".into())),
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn round_trip_write_read_list_stat() {
    tokio::task::LocalSet::new()
        .run_until(round_trip_write_read_list_stat_inner())
        .await;
}

async fn round_trip_write_read_list_stat_inner() {
    let (server_side, client_side) = tokio::net::UnixStream::pair().unwrap();
    let store: Store = Arc::new(Mutex::new(BTreeMap::new()));
    let backend = Arc::new(FakeFs {
        store: Arc::clone(&store),
    });
    tokio::task::spawn_local(async move {
        let _ = serve_connected(server_side, backend).await;
    });

    let connection =
        Connection::connect("mock-fs-client", client_side.into_std().unwrap()).expect("connect");
    let fs = FsClient::from_object(Object::root(connection));

    // 1. Open two files under docs/ and write to each.
    let notes = fs
        .open("docs/notes.md", runtime_fs::OpenMode::Write)
        .await
        .expect("open notes");
    notes
        .write_at(0, b"hello world")
        .await
        .expect("write notes");

    let plan = fs
        .open("docs/plan.md", runtime_fs::OpenMode::Write)
        .await
        .expect("open plan");
    plan.write_at(0, b"shipping").await.expect("write plan");

    // 2. Read notes back and confirm.
    let notes_ro = fs
        .open("docs/notes.md", runtime_fs::OpenMode::Read)
        .await
        .expect("reopen notes");
    let contents = notes_ro.read_all().await.expect("read notes");
    assert_eq!(contents, b"hello world");

    // 3. Stat matches.
    let md = fs.stat("docs/notes.md").await.expect("stat notes");
    assert_eq!(md.size, 11);
    assert_eq!(md.kind, runtime_fs::EntryKind::File);

    // 4. List docs/ returns both entries by name.
    let listing = fs.list("docs").await.expect("list docs");
    assert_eq!(listing.len(), 2);
    assert_eq!(listing[0].name, "notes.md");
    assert_eq!(listing[0].metadata.size, 11);
    assert_eq!(listing[1].name, "plan.md");
    assert_eq!(listing[1].metadata.size, 8);

    // 5. FileHandle.metadata short-circuits path resolution.
    let md_h = notes_ro.metadata().await.expect("handle metadata");
    assert_eq!(md_h.size, 11);
}
