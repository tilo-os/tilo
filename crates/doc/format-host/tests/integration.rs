//! Integration tests for the format-router host:
//! * `single_provider_round_trip` — registry with the Markdown provider,
//!   exercise detect + importAs + exportAs on a real document **through
//!   Cap'n Proto** end to end, byte-equality on round-trip.
//! * `markdown_provider_in_process` — the same provider driven directly:
//!   host entropy reaches the guest, and malformed canonical bytes surface as
//!   a structured error.
//! * `registry_hot_reload` — start empty, drop a provider in, assert the next
//!   call sees it.
//! * `unknown_mime_returns_clear_error` — no provider for the mime →
//!   `importAs` rejects with a clear `unsupported` message.
//! * `hostile_guests_are_stopped_by_the_per_call_limits` — hand-written
//!   hostile guests (a runaway loop, a memory hog, an oversized initial
//!   memory) are stopped by the per-call limits and the host stays usable.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use capnp_rpc::rpc_twoparty_capnp;
use format_host::format_router_capnp::format_router;
use format_host::router::{Router, load_into};
use format_host::runner::{self, Limits, LoadedProvider};
use format_host::{transport, watch::spawn_watcher};

/// Bounds a wait that ends as soon as its condition holds; only a hang reaches it.
const HANG_BOUND: Duration = Duration::from_secs(30);

const BUILD_HINT: &str =
    "builds a WASM provider; run crates/doc/format-host/tests/run-provider-integration.sh";

/// Path to the Markdown provider built by `tests/run-provider-integration.sh`.
fn markdown_wasm() -> PathBuf {
    let here = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let wasm = here
        .parent()
        .expect("format-host crate has a parent directory")
        .join("tilo-doc-codec-markdown-wasm")
        .join("target")
        .join("wasm32-unknown-unknown")
        .join("release")
        .join("tilo_doc_codec_markdown_wasm.wasm");
    assert!(wasm.is_file(), "Markdown provider is missing; {BUILD_HINT}");
    wasm
}

/// A real Markdown document: this repository's format-provider guide.
const REAL_DOC: &[u8] = include_bytes!("../../../../docs/app-model/formats.md");

/// A module that satisfies the ABI and runs `import_body` as its import entry
/// point.
fn hostile_guest(memory_pages: &str, import_body: &str) -> Vec<u8> {
    let wat = format!(
        r#"(module
            (memory (export "memory") {memory_pages})
            (func (export "tilo_fp_abi_version") (result i32) i32.const 1)
            (func (export "tilo_fp_alloc") (param i32) (result i32) i32.const 1024)
            (func (export "tilo_fp_detect") (param i32 i32) (result i64) i64.const 0)
            (func (export "tilo_fp_import") (param i32 i32) (result i64) {import_body})
            (func (export "tilo_fp_export") (param i32 i32) (result i64) i64.const 0))"#
    );
    wat::parse_str(wat).expect("hostile guest WAT must parse")
}

/// Spin up the host in-process: router with a freshly-written registry,
/// capnp transport on a tempdir Unix socket.
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

        let router = Router::new(runner::engine());
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
        let deadline = Instant::now().checked_add(HANG_BOUND).unwrap();
        loop {
            match tokio::net::UnixStream::connect(&self.socket).await {
                Ok(stream) => return Ok(client_for(stream)),
                Err(e) if Instant::now() >= deadline => {
                    return Err(anyhow::anyhow!("connect failed: {e:?}"));
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(25)).await,
            }
        }
    }
}

fn client_for(stream: tokio::net::UnixStream) -> format_router::Client {
    runtime_portal::CapnpClient::from_connected_io(stream, rpc_twoparty_capnp::Side::Client)
        .bootstrap()
}

fn markdown_registry(wasm: &std::path::Path) -> String {
    format!(
        r#"
[[provider]]
mime     = "text/markdown"
package  = "so.tilo.markdown"
wasm     = "{}"
version  = "1.0.0"
priority = "builtin"
"#,
        wasm.display()
    )
}

#[allow(
    clippy::future_not_send,
    reason = "capnp-rpc requests are Rc-based and run on the test LocalSet"
)]
async fn import_as(client: &format_router::Client, bytes: &[u8]) -> Vec<u8> {
    let mut imp = client.import_as_request();
    {
        let mut p = imp.get();
        p.set_bytes(bytes);
        p.set_provider_id("so.tilo.markdown");
    }
    let response = imp.send().promise.await.unwrap();
    response.get().unwrap().get_canonical().unwrap().to_vec()
}

#[allow(
    clippy::future_not_send,
    reason = "capnp-rpc requests are Rc-based and run on the test LocalSet"
)]
async fn export_as(client: &format_router::Client, canonical: &[u8]) -> Vec<u8> {
    let mut exp = client.export_as_request();
    {
        let mut p = exp.get();
        p.set_canonical(canonical);
        p.set_provider_id("so.tilo.markdown");
    }
    let response = exp.send().promise.await.unwrap();
    response.get().unwrap().get_bytes().unwrap().to_vec()
}

#[test]
#[ignore = "builds a WASM provider; run crates/doc/format-host/tests/run-provider-integration.sh"]
fn single_provider_round_trip() {
    let wasm = markdown_wasm();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&rt, async move {
        let h = HostHarness::new(&markdown_registry(&wasm)).unwrap();
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
            names.contains(&"so.tilo.markdown".to_string()),
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
            "so.tilo.markdown"
        );

        // import → export round trip. Exported markdown is idempotent (tilo-doc
        // tests prove this); after one round trip the same source re-imports
        // to identical bytes.
        let canonical_a = import_as(&client, REAL_DOC).await;
        assert!(
            !canonical_a.is_empty(),
            "canonical bytes flow through capnp"
        );
        let out_a = export_as(&client, &canonical_a).await;
        let text = String::from_utf8(out_a.clone()).unwrap();
        assert!(text.starts_with("# Format providers"), "got {text:?}");
        assert!(text.contains("tilo-format-host"), "body text survives");

        let canonical_b = import_as(&client, &out_a).await;
        let out_b = export_as(&client, &canonical_b).await;
        assert_eq!(
            out_a, out_b,
            "round-trip is idempotent through the WASM boundary"
        );
    });
}

#[test]
#[ignore = "builds a WASM provider; run crates/doc/format-host/tests/run-provider-integration.sh"]
fn markdown_provider_in_process() {
    let provider =
        LoadedProvider::from_path(&runner::engine(), &markdown_wasm(), Limits::default()).unwrap();
    let detected = provider.detect(b"# Heading\n\n- item\n").unwrap();
    assert_eq!(detected.mime.as_deref(), Some("text/markdown"));
    // Each import creates a fresh Automerge actor from host entropy, so two
    // imports of the same text never share an actor id.
    let a = provider.import_doc(REAL_DOC).unwrap();
    let b = provider.import_doc(REAL_DOC).unwrap();
    assert_ne!(a, b, "actor ids must come from host entropy");
    assert_eq!(
        provider.export_doc(&a).unwrap(),
        provider.export_doc(&b).unwrap()
    );
    let err = provider
        .export_doc(b"not an automerge document")
        .unwrap_err();
    assert_eq!(err.kind, "parse", "{err:?}");
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
#[ignore = "builds a WASM provider; run crates/doc/format-host/tests/run-provider-integration.sh"]
fn registry_hot_reload() {
    let wasm = markdown_wasm();
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
        std::fs::write(&h.registry_path, markdown_registry(&wasm)).unwrap();
        // The watcher debounces 100ms; give it a couple ticks.
        let deadline = Instant::now().checked_add(HANG_BOUND).unwrap();
        while Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let mut list = client.list_request();
            let _ = list.get();
            let response = list.send().promise.await.unwrap();
            let providers = response.get().unwrap().get_providers().unwrap();
            if !providers.is_empty() {
                let name = providers.get(0).unwrap().to_string().unwrap();
                assert_eq!(name, "so.tilo.markdown");
                return;
            }
        }
        panic!("provider never appeared after hot-reload");
    });
}

#[test]
fn hostile_guests_are_stopped_by_the_per_call_limits() {
    // Phase `runaway_guest_exhausts_fuel`.
    {
        let wasm = hostile_guest("1", "(loop $spin (br $spin)) unreachable");
        let limits = Limits {
            fuel: 50_000_000,
            ..Limits::default()
        };
        let provider = LoadedProvider::from_bytes(&runner::engine(), &wasm, limits).unwrap();
        let started = Instant::now();
        let err = provider.import_doc(b"head").unwrap_err();
        assert_eq!(err.kind, "resource_limit", "{err:?}");
        assert!(err.message.starts_with("fuel budget exhausted"), "{err:?}");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "took {:?}",
            started.elapsed()
        );
        // Every call gets a fresh budget: the provider stays usable.
        let err = provider.import_doc(b"again").unwrap_err();
        assert_eq!(err.kind, "resource_limit", "{err:?}");
    }
    // Phase `memory_hog_hits_cap`.
    {
        // Grow one page at a time until the host refuses.
        let wasm = hostile_guest(
            "1",
            "(loop $grow (drop (memory.grow (i32.const 1))) (br $grow)) unreachable",
        );
        let limits = Limits {
            max_memory_bytes: 16 * 1024 * 1024,
            ..Limits::default()
        };
        let provider = LoadedProvider::from_bytes(&runner::engine(), &wasm, limits).unwrap();
        let err = provider.import_doc(b"head").unwrap_err();
        assert_eq!(err.kind, "resource_limit", "{err:?}");
        assert!(err.message.starts_with("memory cap hit"), "{err:?}");
    }
    // Phase `oversized_initial_memory_is_refused`.
    {
        // 8192 pages = 512 MiB, twice the default ceiling.
        let wasm = hostile_guest("8192", "i64.const 0");
        let err = LoadedProvider::from_bytes(&runner::engine(), &wasm, Limits::default())
            .err()
            .expect("a module whose initial memory exceeds the cap must not load");
        assert!(err.to_string().contains("instantiating"), "{err:#}");
    }
}
