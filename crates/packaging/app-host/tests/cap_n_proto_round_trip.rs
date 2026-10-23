//! End-to-end Cap'n Proto round-trip: spin the `AppHostServer` on a
//! Unix socket in a `LocalSet`, dial from a client, and drive
//! install → list → pin → resolve → diskUsage.

#![allow(
    clippy::unwrap_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use crate::common;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use app_host::app_host_capnp::{Pinning as WirePinning, app_host as capnp_app_host};
use app_host::transport;
use capnp_rpc::rpc_twoparty_capnp;
use package_store::{Catalog, PackageStore, SystemStore};

struct Harness {
    socket: PathBuf,
    store_root: PathBuf,
    _tmp: tempfile::TempDir,
}

impl Harness {
    fn new() -> anyhow::Result<Self> {
        let tmp = tempfile::tempdir()?;
        let socket = tmp.path().join("app-host.sock");
        let store_root = tmp.path().join("store");
        let store = Arc::new(PackageStore::at(&store_root));
        let store_dir = store_root.clone();
        // An empty image catalog: the host's own preinstalled packages must
        // not appear in what this store lists.
        let system_dir = tmp.path().join("system");
        std::fs::create_dir_all(&system_dir)?;
        let catalog = Arc::new(Catalog::at(
            &store_root,
            Some(SystemStore {
                data_dir: system_dir.clone(),
                payload_dir: system_dir,
                interfaces: None,
            }),
        ));
        let listener = transport::bind(&socket)?;
        // `serve` loops forever accepting connections; the handle is
        // deliberately not awaited, but must still be bound (not
        // discarded outright) so the task isn't detached-and-forgotten
        // in a way that hides a panic.
        let _handle = tokio::task::spawn_local(transport::serve(listener, store, catalog, 0));
        Ok(Self {
            socket,
            store_root: store_dir,
            _tmp: tmp,
        })
    }

    async fn connect(&self) -> anyhow::Result<capnp_app_host::Client> {
        let mut last = None;
        let deadline = tilo_test_env::hang_deadline();
        while std::time::Instant::now() < deadline {
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
    runtime_portal::CapnpClient::from_connected_io(stream, rpc_twoparty_capnp::Side::Client)
        .bootstrap()
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

#[tokio::test(flavor = "current_thread")]
async fn registry_catalogue_rpc_refuses_caller_supplied_endpoints() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let harness = Harness::new().unwrap();
            let client = harness.connect().await.unwrap();
            let mut request = client.registry_catalogue_request();
            request
                .get()
                .set_registry("https://caller-controlled.example.test/");
            let error = request
                .send()
                .promise
                .await
                .err()
                .expect("caller endpoint must be refused");
            assert!(error.to_string().contains("not configured and enabled"));
        })
        .await;
}

#[test]
fn list_reports_payload_bytes_without_walking() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&rt, async {
        let h = Harness::new().unwrap();
        let store = PackageStore::at(&h.store_root);
        let (bytes, _) = common::build_tapp(2);
        app_host::install::install_tapp(&store, &bytes, app_host::Pinning::Tried, None).unwrap();
        let row = store.registry().unwrap().entries()[0].clone();
        let measured = package_store::tree_size(&row.payload_root);
        assert!(measured > 0);
        assert_eq!(
            row.payload_bytes, measured,
            "install records the tree's size"
        );

        let client = h.connect().await.unwrap();
        let listed_size = |client: capnp_app_host::Client| async move {
            let response = client.list_request().send().promise.await.unwrap();
            let entries = response.get().unwrap().get_entries().unwrap();
            assert_eq!(entries.len(), 1);
            entries.get(0).get_size_bytes()
        };
        assert_eq!(listed_size(client.clone()).await, measured);

        // With the tree gone, a walk would report 0; the recorded size stands.
        make_writable(&row.payload_root);
        std::fs::remove_dir_all(&row.payload_root).unwrap();
        assert_eq!(listed_size(client).await, measured);
    });
}

/// Materialised payload files and their directories are read-only.
fn make_writable(root: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    for entry in std::fs::read_dir(root).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            make_writable(&path);
        }
    }
    std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o755)).unwrap();
}
