//! End-to-end Cap'n Proto round-trip: spin the `AppHostServer` on a
//! Unix socket in a `LocalSet`, dial from a client, and drive
//! install → list → pin → resolve → diskUsage.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use crate::common;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use app_host::app_host_capnp::{Pinning as WirePinning, app_host as capnp_app_host};
use app_host::transport;
use capnp_rpc::{RpcSystem, rpc_twoparty_capnp, twoparty};
use package_store::{Catalog, PackageStore, SystemStore};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

/// Bounds a wait that ends as soon as its condition holds; only a hang reaches it.
const HANG_BOUND: Duration = Duration::from_secs(30);

struct Harness {
    socket: PathBuf,
    _tmp: tempfile::TempDir,
}

impl Harness {
    fn new() -> anyhow::Result<Self> {
        let tmp = tempfile::tempdir()?;
        let socket = tmp.path().join("app-host.sock");
        let store_root = tmp.path().join("store");
        let store = Arc::new(PackageStore::at(&store_root));
        // An empty image catalog: the host's own preinstalled packages must
        // not appear in what this store lists.
        let system_dir = tmp.path().join("system");
        std::fs::create_dir_all(&system_dir)?;
        let catalog = Arc::new(Catalog::at(
            &store_root,
            Some(SystemStore {
                data_dir: system_dir.clone(),
                payload_dir: system_dir,
            }),
        ));
        let listener = transport::bind(&socket)?;
        // `serve` loops forever accepting connections; the handle is
        // deliberately not awaited, but must still be bound (not
        // discarded outright) so the task isn't detached-and-forgotten
        // in a way that hides a panic.
        let _handle = tokio::task::spawn_local(transport::serve(listener, store, catalog, 0));
        Ok(Self { socket, _tmp: tmp })
    }

    async fn connect(&self) -> anyhow::Result<capnp_app_host::Client> {
        let mut last = None;
        let started = std::time::Instant::now();
        while started.elapsed() < HANG_BOUND {
            match tokio::net::UnixStream::connect(&self.socket).await {
                Ok(mut stream) => {
                    use tokio::io::AsyncWriteExt;
                    let handoff = br#"{"protocol":1,"app_id":"so.tilo.store"}"#;
                    stream.write_all(b"QAH1").await?;
                    stream
                        .write_all(&u32::try_from(handoff.len())?.to_be_bytes())
                        .await?;
                    stream.write_all(handoff).await?;
                    return Ok(client_for(stream));
                }
                Err(e) => {
                    last = Some(e);
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            }
        }
        Err(anyhow::anyhow!("connect failed: {last:?}"))
    }
}

fn client_for(stream: tokio::net::UnixStream) -> capnp_app_host::Client {
    let (reader, writer) = stream.into_split();
    let reader = reader.compat();
    let writer = writer.compat_write();
    let network = Box::new(twoparty::VatNetwork::new(
        futures_util::io::BufReader::new(reader),
        futures_util::io::BufWriter::new(writer),
        rpc_twoparty_capnp::Side::Client,
        capnp::message::ReaderOptions::default(),
    ));
    let mut rpc_system = RpcSystem::new(network, None);
    let client: capnp_app_host::Client = rpc_system.bootstrap(rpc_twoparty_capnp::Side::Server);
    tokio::task::spawn_local(rpc_system);
    client
}

#[test]
fn install_list_pin_resolve_over_capnp() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&rt, async {
        let h = Harness::new().unwrap();
        let client = h.connect().await.unwrap();

        // Build a real tapp the server can accept.
        let (bytes, expected_id) = common::build_tapp(2);
        let expected_publisher = app_registry::tapp::load(&bytes)
            .unwrap()
            .manifest
            .publisher
            .identity_bytes()
            .unwrap();

        // install
        let mut req = client.install_request();
        {
            let mut p = req.get();
            p.set_tapp_bytes(&bytes);
            p.set_pinning(WirePinning::Tried);
            p.set_expected_root(expected_id.as_bytes());
            p.set_expected_publisher(&expected_publisher);
            p.set_expected_app_id("so.tilo.editor");
            p.set_expected_version("1.4.2");
            p.set_expected_size_bytes(bytes.len() as u64);
        }
        let resp = req.send().promise.await.unwrap();
        let installed_id: Vec<u8> = resp.get().unwrap().get_id().unwrap().to_vec();
        assert_eq!(&installed_id[..], expected_id.as_bytes());

        // list
        let list_resp = client.list_request().send().promise.await.unwrap();
        let entries = list_resp.get().unwrap().get_entries().unwrap();
        assert_eq!(entries.len(), 1);
        let entry = entries.get(0);
        assert_eq!(entry.get_pinning().unwrap(), WirePinning::Tried);
        assert!(entry.get_user_managed(), "app-host entries are mutable");
        let id_bytes: Vec<u8> = entry.get_id().unwrap().to_vec();
        assert_eq!(&id_bytes[..], expected_id.as_bytes());

        // pin
        let mut pin_req = client.pin_request();
        pin_req.get().set_id(expected_id.as_bytes());
        pin_req.send().promise.await.unwrap();

        // resolve
        let mut resolve_req = client.resolve_request();
        resolve_req.get().set_id(expected_id.as_bytes());
        let resp = resolve_req.send().promise.await.unwrap();
        let entry = resp.get().unwrap().get_entry().unwrap();
        assert_eq!(entry.get_pinning().unwrap(), WirePinning::Pinned);
        let payload_root = entry.get_payload_root().unwrap().to_string().unwrap();
        assert!(!payload_root.is_empty());
        let manifest_text = entry.get_manifest_text().unwrap().to_string().unwrap();
        assert!(manifest_text.contains("so.tilo.editor"));

        // diskUsage
        let du = client.disk_usage_request().send().promise.await.unwrap();
        let result = du.get().unwrap();
        assert!(result.get_total_bytes() > 0);
        let by = result.get_by_pinning().unwrap();
        assert!(
            by.get_pinned_bytes() > 0,
            "pinned bytes should be > 0 after we pinned"
        );
    });
}
