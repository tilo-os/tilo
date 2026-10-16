//! Integration test: end-to-end RPC round-trip over a Unix socket
//! against an in-memory `WorkspaceFs` server.
//!
//! Covers the `UnixCapnpClient` plumbing used by apps,
//! the generated `WorkspaceFs` schema, and the idiomatic `FsClient`
//! wrappers (`open`, `list`, `stat`, `FileHandle::write_at`/`read_all`).
//!
//! The backend is an in-memory map; directories are inferred from
//! path prefixes rather than represented explicitly, so the test only
//! exercises write→read on files and a single list.

// capnp's generated `Server` traits declare each verb as `-> impl Future`,
// and these fakes return capnp's own `Promise`, which is the concrete type
// behind it. The lint's concern is a caller coming to depend on the narrower
// return; nothing outside this file constructs these servers, so the
// refinement stays where it is written.
#![allow(refining_impl_trait_internal)]
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::os::fd::IntoRawFd;
use std::rc::Rc;

use capnp::capability::Promise;
use capnp_rpc::rpc_twoparty_capnp;
use tokio::net::UnixStream;
use tokio::task::LocalSet;

use runtime_fs::raw::{EntryKind as WireKind, OpenMode, file_handle, workspace_fs};
use runtime_fs::{FsClient, WorkspaceFsClient};
use runtime_portal::UnixCapnpClient;

// shared in-memory backend

type Store = Rc<RefCell<BTreeMap<String, Vec<u8>>>>;

fn normalise(path: &str) -> String {
    path.trim_start_matches('/').to_owned()
}

// server impls

struct FakeFile {
    store: Store,
    path: String,
}

impl file_handle::Server for FakeFile {
    fn read(
        self: Rc<Self>,
        params: file_handle::ReadParams,
        mut results: file_handle::ReadResults,
    ) -> Promise<(), capnp::Error> {
        let (offset, max_len) = {
            let r = match params.get() {
                Ok(r) => r,
                Err(e) => return Promise::err(e),
            };
            (
                usize::try_from(r.get_offset()).expect("offset fits usize"),
                r.get_max_len() as usize,
            )
        };
        let store = self.store.borrow();
        let Some(bytes) = store.get(&self.path) else {
            return Promise::err(capnp::Error::failed("file missing".into()));
        };
        if offset >= bytes.len() {
            results.get().set_bytes(&[]);
            return Promise::ok(());
        }
        let end = offset.saturating_add(max_len).min(bytes.len());
        results.get().set_bytes(&bytes[offset..end]);
        Promise::ok(())
    }

    fn write(
        self: Rc<Self>,
        params: file_handle::WriteParams,
        mut results: file_handle::WriteResults,
    ) -> Promise<(), capnp::Error> {
        let r = match params.get() {
            Ok(r) => r,
            Err(e) => return Promise::err(e),
        };
        let offset = usize::try_from(r.get_offset()).expect("offset fits usize");
        let bytes = match r.get_bytes() {
            Ok(b) => b.to_vec(),
            Err(e) => return Promise::err(e),
        };
        let mut store = self.store.borrow_mut();
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
        results
            .get()
            .set_written(u32::try_from(bytes.len()).expect("write size fits u32"));
        Promise::ok(())
    }

    fn flush(
        self: Rc<Self>,
        _params: file_handle::FlushParams,
        _results: file_handle::FlushResults,
    ) -> Promise<(), capnp::Error> {
        Promise::ok(())
    }

    fn metadata(
        self: Rc<Self>,
        _params: file_handle::MetadataParams,
        mut results: file_handle::MetadataResults,
    ) -> Promise<(), capnp::Error> {
        let store = self.store.borrow();
        let size = store.get(&self.path).map_or(0, |b| b.len() as u64);
        let mut md = results.get().init_metadata();
        md.set_kind(WireKind::File);
        md.set_size(size);
        md.set_modified_unix_seconds(0);
        md.set_needs_hydration(false);
        Promise::ok(())
    }
}

struct FakeFs {
    store: Store,
}

impl workspace_fs::Server for FakeFs {
    fn open(
        self: Rc<Self>,
        params: workspace_fs::OpenParams,
        mut results: workspace_fs::OpenResults,
    ) -> Promise<(), capnp::Error> {
        let r = match params.get() {
            Ok(r) => r,
            Err(e) => return Promise::err(e),
        };
        let path = match r.get_path().and_then(|t| {
            t.to_str()
                .map(std::borrow::ToOwned::to_owned)
                .map_err(|e| capnp::Error::failed(format!("bad utf8: {e}")))
        }) {
            Ok(p) => normalise(&p),
            Err(e) => return Promise::err(e),
        };
        let Ok(mode) = r.get_mode() else {
            return Promise::err(capnp::Error::failed("bad mode".into()));
        };
        // Truncate on write; leave alone on append/read.
        if matches!(mode, OpenMode::Write) {
            self.store.borrow_mut().insert(path.clone(), Vec::new());
        } else if matches!(mode, OpenMode::Append) {
            self.store.borrow_mut().entry(path.clone()).or_default();
        }
        let file = FakeFile {
            store: self.store.clone(),
            path,
        };
        let client: file_handle::Client = capnp_rpc::new_client(file);
        results.get().set_handle(client);
        Promise::ok(())
    }

    fn list(
        self: Rc<Self>,
        params: workspace_fs::ListParams,
        mut results: workspace_fs::ListResults,
    ) -> Promise<(), capnp::Error> {
        let r = match params.get() {
            Ok(r) => r,
            Err(e) => return Promise::err(e),
        };
        let path = match r.get_path().and_then(|t| {
            t.to_str()
                .map(std::borrow::ToOwned::to_owned)
                .map_err(|e| capnp::Error::failed(format!("bad utf8: {e}")))
        }) {
            Ok(p) => normalise(&p),
            Err(e) => return Promise::err(e),
        };
        let prefix = if path.is_empty() {
            String::new()
        } else {
            format!("{path}/")
        };
        let store = self.store.borrow();
        let matching: Vec<(String, u64)> = store
            .iter()
            .filter_map(|(k, v)| {
                if !k.starts_with(&prefix) {
                    return None;
                }
                let rest = &k[prefix.len()..];
                // Only direct children (no further '/').
                if rest.contains('/') {
                    return None;
                }
                Some((rest.to_owned(), v.len() as u64))
            })
            .collect();
        let mut entries = results
            .get()
            .init_entries(u32::try_from(matching.len()).expect("entry count fits u32"));
        for (i, (name, size)) in matching.iter().enumerate() {
            let mut e = entries
                .reborrow()
                .get(u32::try_from(i).expect("index fits u32"));
            e.set_name(name);
            let mut md = e.init_metadata();
            md.set_kind(WireKind::File);
            md.set_size(*size);
            md.set_modified_unix_seconds(0);
            md.set_needs_hydration(false);
        }
        Promise::ok(())
    }

    fn stat(
        self: Rc<Self>,
        params: workspace_fs::StatParams,
        mut results: workspace_fs::StatResults,
    ) -> Promise<(), capnp::Error> {
        let path = match params
            .get()
            .and_then(runtime_fs::fs_capnp::workspace_fs::stat_params::Reader::get_path)
            .and_then(|t| {
                t.to_str()
                    .map(std::borrow::ToOwned::to_owned)
                    .map_err(|e| capnp::Error::failed(format!("bad utf8: {e}")))
            }) {
            Ok(p) => normalise(&p),
            Err(e) => return Promise::err(e),
        };
        let store = self.store.borrow();
        let Some(bytes) = store.get(&path) else {
            return Promise::err(capnp::Error::failed("no such path".into()));
        };
        let mut md = results.get().init_metadata();
        md.set_kind(WireKind::File);
        md.set_size(bytes.len() as u64);
        md.set_modified_unix_seconds(0);
        md.set_needs_hydration(false);
        Promise::ok(())
    }

    fn mkdir(
        self: Rc<Self>,
        _params: workspace_fs::MkdirParams,
        _results: workspace_fs::MkdirResults,
    ) -> Promise<(), capnp::Error> {
        // Directories are implicit in this test.
        Promise::ok(())
    }

    fn remove(
        self: Rc<Self>,
        params: workspace_fs::RemoveParams,
        _results: workspace_fs::RemoveResults,
    ) -> Promise<(), capnp::Error> {
        let path = match params
            .get()
            .and_then(runtime_fs::fs_capnp::workspace_fs::remove_params::Reader::get_path)
            .and_then(|t| {
                t.to_str()
                    .map(std::borrow::ToOwned::to_owned)
                    .map_err(|e| capnp::Error::failed(format!("bad utf8: {e}")))
            }) {
            Ok(p) => normalise(&p),
            Err(e) => return Promise::err(e),
        };
        self.store.borrow_mut().remove(&path);
        Promise::ok(())
    }
}

// the test itself

#[tokio::test(flavor = "current_thread")]
async fn round_trip_write_read_list_stat() {
    let local = LocalSet::new();
    local
        .run_until(async {
            let (server_side, client_side) = UnixStream::pair().unwrap();

            let store: Store = Rc::new(RefCell::new(BTreeMap::new()));
            let backend = FakeFs {
                store: store.clone(),
            };
            let backend_client: workspace_fs::Client = capnp_rpc::new_client(backend);

            let server_rpc = runtime_portal::serve_connected_io(server_side, backend_client.client);
            tokio::task::spawn_local(async move {
                let _ = server_rpc.await;
            });

            let client_fd = client_side.into_std().unwrap().into_raw_fd();
            let unix =
                UnixCapnpClient::from_connected_fd(client_fd, rpc_twoparty_capnp::Side::Client)
                    .expect("connect");
            let client: WorkspaceFsClient = unix.bootstrap();
            let fs = FsClient::from_raw(client);

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
            let mut listing = fs.list("docs").await.expect("list docs");
            listing.sort_by(|a, b| a.name.cmp(&b.name));
            assert_eq!(listing.len(), 2);
            assert_eq!(listing[0].name, "notes.md");
            assert_eq!(listing[0].metadata.size, 11);
            assert_eq!(listing[1].name, "plan.md");
            assert_eq!(listing[1].metadata.size, 8);

            // 5. FileHandle.metadata short-circuits path resolution.
            let md_h = notes_ro.metadata().await.expect("handle metadata");
            assert_eq!(md_h.size, 11);
        })
        .await;
}
