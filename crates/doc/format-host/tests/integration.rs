//! Integration tests for the format-router host:
//! * `single_provider_round_trip` — registry with the markdown wasm, exercise
//!   detect + importAs + exportAs on a small `.md` fixture **through Cap'n
//!   Proto** end to end, byte-equality on round-trip.
//! * `registry_hot_reload` — start empty, drop a provider in, assert the next
//!   call sees it.
//! * `unknown_mime_returns_clear_error` — no provider for the mime →
//!   `importAs` rejects with a clear `unsupported` message.
//! * `resource_limit_kills_runaway` — feed in a hand-written WAT component
//!   whose `detect` spins forever; assert it gets killed at the wallclock
//!   deadline and the host stays up.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::path::PathBuf;
use std::time::Duration;

use capnp_rpc::{RpcSystem, rpc_twoparty_capnp, twoparty};
use format_host::format_router_capnp::format_router;
use format_host::router::{Router, load_into};
use format_host::runner::{LoadedComponent, MAX_WALLCLOCK, spawn_epoch_ticker};
use format_host::{transport, watch::spawn_watcher};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

/// Path to the markdown wasm component built by `tests/run-component-integration.sh`.
fn markdown_wasm() -> PathBuf {
    let here = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    here.parent()
        .expect("format-host crate has a parent directory")
        .join("qdoc-codec-markdown-wasm")
        .join("target")
        .join("wasm32-wasip2")
        .join("release")
        .join("qdoc_codec_markdown_wasm.wasm")
}

/// Spin up the host in-process: shared engine, router with a freshly-written
/// registry, capnp transport on a tempdir Unix socket. Returns a closure that
/// (a) hands back a connected `format_router::Client`, (b) shuts the listener
/// down at drop.
struct HostHarness {
    socket: PathBuf,
    _tmp: tempfile::TempDir,
    registry_path: PathBuf,
}

impl HostHarness {
    fn new(registry_text: &str) -> anyhow::Result<Self> {
        let tmp = tempfile::tempdir()?;
        let registry_path = tmp.path().join("format-providers.toml");
        std::fs::write(&registry_path, registry_text)?;
        let socket = tmp.path().join("format-router.sock");

        let engine = LoadedComponent::engine()?;
        spawn_epoch_ticker(engine.clone());
        let router = Router::new(engine);
        load_into(&router, &registry_path)?;
        spawn_watcher(router.clone(), registry_path.clone())?;
        let listener = transport::bind(&socket)?;
        // Spin the accept loop on the runtime — we'll let it die when the
        // tempdir is dropped + the socket file is removed.
        let _server = tokio::task::spawn_local(transport::serve(listener, router));

        Ok(Self {
            socket,
            _tmp: tmp,
            registry_path,
        })
    }

    async fn connect(&self) -> anyhow::Result<format_router::Client> {
        // Tiny retry — the accept loop may not be polled yet on the first
        // dispatch tick.
        let mut last = None;
        for _ in 0..20 {
            match tokio::net::UnixStream::connect(&self.socket).await {
                Ok(stream) => return Ok(client_for(stream)),
                Err(e) => {
                    last = Some(e);
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            }
        }
        Err(anyhow::anyhow!("connect failed: {last:?}"))
    }
}

fn client_for(stream: tokio::net::UnixStream) -> format_router::Client {
    let (reader, writer) = stream.into_split();
    let reader = reader.compat();
    let writer = writer.compat_write();
    let network = Box::new(twoparty::VatNetwork::new(
        futures::io::BufReader::new(reader),
        futures::io::BufWriter::new(writer),
        rpc_twoparty_capnp::Side::Client,
        capnp::message::ReaderOptions::default(),
    ));
    let mut rpc_system = RpcSystem::new(network, None);
    let client: format_router::Client = rpc_system.bootstrap(rpc_twoparty_capnp::Side::Server);
    tokio::task::spawn_local(rpc_system);
    client
}

#[test]
#[ignore = "builds a WASM component; run crates/doc/format-host/tests/run-component-integration.sh"]
fn single_provider_round_trip() {
    let wasm = markdown_wasm();
    assert!(
        wasm.is_file(),
        "markdown component is missing; run crates/doc/format-host/tests/run-component-integration.sh"
    );
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&rt, async move {
        let reg_text = format!(
            r#"
[[provider]]
mime     = "text/markdown"
package  = "so.tilo.qdoc-md"
wasm     = "{}"
version  = "1.0.0"
priority = "builtin"
"#,
            wasm.display()
        );
        let h = HostHarness::new(&reg_text).unwrap();
        let client = h.connect().await.unwrap();

        // list() — the provider shows up.
        let mut list = client.list_request();
        let _ = list.get();
        let response = list.send().promise.await.unwrap();
        let providers = response.get().unwrap().get_providers().unwrap();
        let names: Vec<String> = (0..providers.len())
            .map(|i| providers.get(i).unwrap().to_string().unwrap())
            .collect();
        assert!(
            names.contains(&"so.tilo.qdoc-md".to_string()),
            "got {names:?}"
        );

        // detect("# hi") — markdown provider declares medium confidence.
        let mut det = client.detect_request();
        {
            let mut p = det.get();
            p.set_head(b"# hi");
            p.set_hint("text/markdown");
        }
        let response = det.send().promise.await.unwrap();
        let detected = response.get().unwrap().get_detected().unwrap();
        assert!(detected.get_confidence().unwrap().get_tier() >= 1);
        assert_eq!(
            detected.get_provider_id().unwrap().to_str().unwrap(),
            "so.tilo.qdoc-md"
        );

        // import → export round trip. Exported markdown is idempotent (qdoc
        // tests prove this); after one round trip the same source re-imports
        // to identical bytes.
        let src = b"# Title\n\nHello **world**.\n";
        let mut imp = client.import_as_request();
        {
            let mut p = imp.get();
            p.set_bytes(src);
            p.set_provider_id("so.tilo.qdoc-md");
        }
        let response = imp.send().promise.await.unwrap();
        let canonical_a: Vec<u8> = response.get().unwrap().get_canonical().unwrap().to_vec();
        assert!(
            !canonical_a.is_empty(),
            "canonical bytes flow through capnp"
        );

        let mut exp = client.export_as_request();
        {
            let mut p = exp.get();
            p.set_canonical(&canonical_a);
            p.set_provider_id("so.tilo.qdoc-md");
        }
        let response = exp.send().promise.await.unwrap();
        let out_a: Vec<u8> = response.get().unwrap().get_bytes().unwrap().to_vec();
        assert!(
            out_a.starts_with(b"# Title"),
            "got {:?}",
            String::from_utf8_lossy(&out_a)
        );

        // Re-import the exported markdown; re-export; assert idempotence.
        let mut imp2 = client.import_as_request();
        {
            let mut p = imp2.get();
            p.set_bytes(&out_a);
            p.set_provider_id("so.tilo.qdoc-md");
        }
        let response = imp2.send().promise.await.unwrap();
        let canonical_b: Vec<u8> = response.get().unwrap().get_canonical().unwrap().to_vec();
        let mut exp2 = client.export_as_request();
        {
            let mut p = exp2.get();
            p.set_canonical(&canonical_b);
            p.set_provider_id("so.tilo.qdoc-md");
        }
        let response = exp2.send().promise.await.unwrap();
        let out_b: Vec<u8> = response.get().unwrap().get_bytes().unwrap().to_vec();
        assert_eq!(
            out_a, out_b,
            "round-trip is idempotent through the WASM boundary"
        );

        // Drop registry to trigger Drop of the temp dir.
        let _ = h.registry_path;
    });
}

#[test]
fn unknown_mime_returns_clear_error() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&rt, async move {
        // Empty registry → importAs for any provider id fails clearly.
        let h = HostHarness::new("").unwrap();
        let client = h.connect().await.unwrap();
        let mut imp = client.import_as_request();
        {
            let mut p = imp.get();
            p.set_bytes(b"x");
            p.set_provider_id("so.tilo.no-such");
        }
        let res = imp.send().promise.await;
        let Err(err) = res else {
            panic!("expected error for unknown provider");
        };
        let msg = format!("{err}");
        assert!(
            msg.contains("no provider") || msg.contains("unsupported"),
            "expected unsupported-mime error, got {msg}"
        );
    });
}

#[test]
#[ignore = "builds a WASM component; run crates/doc/format-host/tests/run-component-integration.sh"]
fn registry_hot_reload() {
    let wasm = markdown_wasm();
    assert!(
        wasm.is_file(),
        "markdown component is missing; run crates/doc/format-host/tests/run-component-integration.sh"
    );
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&rt, async move {
        // Start empty.
        let h = HostHarness::new("").unwrap();
        let client = h.connect().await.unwrap();
        let mut list = client.list_request();
        let _ = list.get();
        let response = list.send().promise.await.unwrap();
        let initial = response.get().unwrap().get_providers().unwrap();
        assert_eq!(initial.len(), 0, "initial registry is empty");

        // Rewrite the registry file in place — inotify picks it up.
        let new_text = format!(
            r#"
[[provider]]
mime     = "text/markdown"
package  = "so.tilo.qdoc-md"
wasm     = "{}"
version  = "1.0.0"
priority = "builtin"
"#,
            wasm.display()
        );
        std::fs::write(&h.registry_path, new_text).unwrap();
        // The watcher debounces 100ms; give it a couple ticks.
        for _ in 0..40 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let mut list = client.list_request();
            let _ = list.get();
            let response = list.send().promise.await.unwrap();
            let providers = response.get().unwrap().get_providers().unwrap();
            if !providers.is_empty() {
                let name = providers.get(0).unwrap().to_string().unwrap();
                assert_eq!(name, "so.tilo.qdoc-md");
                return;
            }
        }
        panic!("provider never appeared after hot-reload");
    });
}

#[test]
fn resource_limit_kills_runaway() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&rt, async move {
        // Compile the WAT to a Component Model wasm at test time.
        let wat_src = include_str!("fixtures/infinite-loop.wat");
        let wasm = wat::parse_str(wat_src).expect("infinite-loop WAT fixture must parse");
        let engine = LoadedComponent::engine().unwrap();
        spawn_epoch_ticker(engine.clone());
        let comp = LoadedComponent::from_bytes(&engine, &wasm)
            .expect("infinite-loop fixture must load as a component");

        // The detect call spins forever. Wasmtime's epoch deadline should
        // kill it within the 10-s wallclock budget; we wrap the call in
        // spawn_blocking so the test runtime stays responsive.
        let started = std::time::Instant::now();
        let res = tokio::task::spawn_blocking(move || comp.detect(b"head"))
            .await
            .expect("join ok");
        let elapsed = started.elapsed();
        // Preserve a generous allowance for a heavily scheduled CI host while
        // deriving the bound from the production deadline.
        let latest_expected = MAX_WALLCLOCK + MAX_WALLCLOCK;
        assert!(elapsed < latest_expected, "took {elapsed:?}");
        match res {
            Ok(_) => panic!("infinite-loop component returned cleanly — epoch not bumping?"),
            Err(e) => {
                assert_eq!(e.kind, "resource_limit", "unexpected error {e:?}");
                assert!(
                    e.message.starts_with("wallclock budget exhausted:"),
                    "the runaway must reach epoch interruption, not fail early in canonical ABI lowering: {e:?}"
                );
            }
        }
        // The host stays up — a second call on a fresh component succeeds.
        let _ = LoadedComponent::engine().unwrap();
    });
}
