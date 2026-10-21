//! End-to-end Wayland integration test for the native compositor: spawns
//! `tilo-desktop --mode native --backend winit`, scrapes the listening
//! socket out of its stderr (`listening on "wayland-N"` is a stable
//! contract of `compositor::native::run`), drives a raw `wayland-client`
//! toplevel with a unique app-id, and asserts the compositor's
//! `WorkspaceTree` adopts then reaps the matching foreign tab — observed
//! through the `TILO_TEST_STATE_DUMP` introspection hook.
//!
//! `#[ignore]` by default: winit needs a real Wayland/X11 display, which
//! headless CI can't supply.
//!
//! ```text
//! cargo build -p tilo-desktop
//! cargo test -p tilo-desktop --test compositor_test -- --ignored
//! ```

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use wayland_client::protocol::{wl_compositor, wl_registry, wl_surface};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, delegate_noop};
use wayland_protocols::xdg::shell::client::{xdg_toplevel, xdg_wm_base};

/// Walks the `WorkspaceTree` JSON dump untyped, looking for a
/// `TileContent::Foreign` tab whose `app_id` matches. Untyped because a
/// typed mirror would have to enumerate every `TileContent` variant and
/// would break whenever a tile shape grows a field.
fn dump_has_foreign_app_id(dump_path: &std::path::Path, app_id: &str) -> Option<bool> {
    let bytes = std::fs::read(dump_path).ok()?;
    let root: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let workspaces = root.get("workspaces")?.as_array()?;
    for ws in workspaces {
        let Some(strips) = ws.get("strips").and_then(|s| s.as_array()) else {
            continue;
        };
        for strip in strips {
            let Some(groups) = strip.get("window_groups").and_then(|g| g.as_array()) else {
                continue;
            };
            for group in groups {
                let Some(tabs) = group.get("tabs").and_then(|t| t.as_array()) else {
                    continue;
                };
                for tab in tabs {
                    // Tab.tile is serialized as externally-tagged enum:
                    // `{"Foreign":{"app_id":"…","surface_id":N}}`.
                    let foreign = tab.get("tile").and_then(|t| t.get("Foreign"));
                    if let Some(f) = foreign
                        && f.get("app_id").and_then(|a| a.as_str()) == Some(app_id)
                    {
                        return Some(true);
                    }
                }
            }
        }
    }
    Some(false)
}

/// Re-polls every 50ms until the closure yields `Some(true)` or `timeout`.
fn poll_for(timeout: Duration, mut probe: impl FnMut() -> Option<bool>) -> bool {
    let deadline = Instant::now()
        .checked_add(timeout)
        .unwrap_or_else(Instant::now);
    while Instant::now() < deadline {
        if matches!(probe(), Some(true)) {
            return true;
        }
        thread::sleep(Duration::from_millis(50));
    }
    false
}

/// Returns (child handle, socket name like `wayland-3`).
fn spawn_compositor(dump_path: &std::path::Path) -> (std::process::Child, String) {
    tilo_test_env::require_isolated_desktop();
    // Built by `cargo test`'s pre-build step, at the workspace target directory.
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .ancestors()
        .nth(3)
        .expect("shell crate must live under <workspace>/crates/session")
        .to_path_buf();
    let bin = workspace_root.join("target/debug/tilo-desktop");
    assert!(
        bin.exists(),
        "compositor binary not found at {} — run `cargo build -p tilo-desktop` first",
        bin.display()
    );

    let mut child = Command::new(&bin)
        .arg("--mode")
        .arg("native")
        .arg("--backend")
        .arg("winit")
        // Force smithay's `ListeningSocketSource::new_auto` to bind a
        // fresh socket instead of inheriting the host Wayland session.
        .env_remove("WAYLAND_DISPLAY")
        .env("TILO_TEST_STATE_DUMP", dump_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn tilo-desktop");

    // Streamed on a worker thread so it reaches the test logs and is scanned
    // for the "listening on" marker; first matching line wins.
    let stderr = child.stderr.take().expect("child stderr piped");
    let (tx, rx) = mpsc::channel::<String>();
    thread::spawn(move || {
        let reader = BufReader::new(stderr);
        for line in reader.lines().map_while(Result::ok) {
            // Echo so the test runner shows compositor logs on failure.
            eprintln!("[compositor] {line}");
            let _ = tx.send(line);
        }
    });

    let deadline = tilo_test_env::hang_deadline();
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(remaining.min(Duration::from_millis(500))) {
            Ok(line) => {
                if let Some(name) = extract_socket_name(&line) {
                    return (child, name);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    panic!("compositor never printed `tilo compositor listening on …`");
}

fn extract_socket_name(line: &str) -> Option<String> {
    // The contract is `tilo compositor listening on "wayland-N"`.
    let marker = "tilo compositor listening on ";
    let rest = line
        .find(marker)
        .and_then(|i| line.get(i.saturating_add(marker.len())..))?;
    // The OsString's Debug form on Linux is `"wayland-N"` — strip the
    // surrounding double-quotes.
    let trimmed = rest.trim();
    let stripped = trimmed.strip_prefix('"')?.strip_suffix('"')?;
    Some(stripped.to_string())
}

/// `native::run` ends with `process::exit`, so endpoint guards cannot be left
/// to ordinary Rust destruction. Run the production binary twice against
/// one throwaway runtime and require the first run's endpoints to be gone
/// before the second can reclaim the canonical `wayland-1` name.
#[test]
#[ignore = "spawns two real native compositor processes"]
fn clean_exit_removes_runtime_endpoints_and_allows_canonical_rebind() {
    tilo_test_env::require_isolated_desktop();
    let runtime = tempfile::tempdir().expect("temporary runtime");
    let state = runtime.path().join("state.json");
    let identity_root = runtime.path().join("identity-root");
    let spawn = || {
        Command::new(tilo_test_env::package_binary!("shell"))
            .args(["--mode", "native", "--backend", "headless"])
            .env("TILO_RUN_SECONDS", "2")
            .env("XDG_RUNTIME_DIR", runtime.path())
            .env("XDG_CONFIG_HOME", runtime.path().join("config"))
            .env("XDG_DATA_HOME", runtime.path().join("data"))
            .env("XDG_STATE_HOME", runtime.path().join("state"))
            .env("XDG_CACHE_HOME", runtime.path().join("cache"))
            .env("TILO_STATE_PATH", &state)
            .env("TILO_IDENTITY_ROOT", &identity_root)
            .env("TILO_WORKSPACE_ROOTS", "")
            .env("TILO_HEADLESS_SIZE", "640x480")
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("DISPLAY")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn native compositor")
    };
    let wait_for_socket = |child: &mut std::process::Child| {
        let socket = runtime.path().join("wayland-1");
        let deadline = tilo_test_env::hang_deadline();
        while !socket.exists() && child.try_wait().expect("poll compositor").is_none() {
            assert!(
                Instant::now() < deadline,
                "compositor did not bind wayland-1"
            );
            thread::sleep(Duration::from_millis(20));
        }
        assert!(
            socket.exists(),
            "compositor exited before binding wayland-1"
        );
    };
    let assert_clean = || {
        let tilo_session = runtime.path().join("tilo-session");
        assert!(
            !tilo_session.join("wayland-display").exists(),
            "wayland-display marker survived clean compositor exit"
        );
        for root in [runtime.path(), tilo_session.as_path()] {
            if !root.exists() {
                continue;
            }
            for entry in std::fs::read_dir(root).expect("read runtime endpoints") {
                let name = entry
                    .expect("runtime endpoint entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned();
                assert!(
                    !name.starts_with("wayland-")
                        && !name.starts_with("tilo-osk-")
                        && !name.starts_with("tilo-im-")
                        && !name.starts_with("tilo-privileged-")
                        && !std::path::Path::new(&name)
                            .extension()
                            .is_some_and(|extension| extension.eq_ignore_ascii_case("lock")),
                    "stale compositor endpoint remained: {}/{}",
                    root.display(),
                    name
                );
            }
        }
        for service in ["shell-ui.sock", "identity.sock"] {
            assert!(
                !runtime.path().join("tilo/services").join(service).exists(),
                "stale Tilo service endpoint remained: {service}"
            );
        }
    };

    let mut first = spawn();
    wait_for_socket(&mut first);
    assert!(first.wait().expect("wait first compositor").success());
    assert_clean();

    let mut second = spawn();
    wait_for_socket(&mut second);
    assert!(second.wait().expect("wait second compositor").success());
    assert_clean();
}

// Minimal wayland-client app — registry walks, then drives the toplevel.

struct TestClient {
    compositor: Option<wl_compositor::WlCompositor>,
    wm_base: Option<xdg_wm_base::XdgWmBase>,
}

impl TestClient {
    fn new() -> Self {
        Self {
            compositor: None,
            wm_base: None,
        }
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for TestClient {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        (): &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "wl_compositor" => {
                    let c: wl_compositor::WlCompositor =
                        registry.bind(name, version.min(4), qh, ());
                    state.compositor = Some(c);
                }
                "xdg_wm_base" => {
                    let b: xdg_wm_base::XdgWmBase = registry.bind(name, version.min(2), qh, ());
                    state.wm_base = Some(b);
                }
                _ => {}
            }
        }
    }
}

delegate_noop!(TestClient: ignore wl_compositor::WlCompositor);
delegate_noop!(TestClient: ignore wl_surface::WlSurface);

crate::support::wayland::test_client!(TestClient: ping, configure);

delegate_noop!(TestClient: ignore xdg_toplevel::XdgToplevel);

/// Returns whatever it has on timeout so the caller reports the failure.
fn round_trip_until_bound(
    conn: &Connection,
    queue: &mut EventQueue<TestClient>,
    state: &mut TestClient,
    timeout: Duration,
) {
    let deadline = Instant::now()
        .checked_add(timeout)
        .unwrap_or_else(Instant::now);
    while (state.compositor.is_none() || state.wm_base.is_none()) && Instant::now() < deadline {
        let _ = conn.roundtrip();
        let _ = queue.dispatch_pending(state);
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
#[ignore = "requires a host display server (WAYLAND_DISPLAY or DISPLAY) to back winit"]
fn native_compositor_adopts_and_reaps_foreign_tab() {
    let dump_path = std::env::temp_dir().join(format!("tilo-state-{}.json", std::process::id()));
    let _ = std::fs::remove_file(&dump_path);

    let (mut child, socket_name) = spawn_compositor(&dump_path);

    // Closure so a panic in the client path still tears the compositor down.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_client_assertions(&socket_name, &dump_path);
    }));

    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_file(&dump_path);

    if let Err(payload) = result {
        std::panic::resume_unwind(payload);
    }
}

fn run_client_assertions(socket_name: &str, dump_path: &std::path::Path) {
    let app_id = format!("tilo-test-{}", std::process::id());

    // Connect to the freshly-bound socket via `WAYLAND_DISPLAY`, which
    // `wayland-client` reads, set just for the connection scope.
    let conn = {
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("WAYLAND_DISPLAY", socket_name);
        Connection::connect_to_env().expect("connect to compositor socket")
    };
    let mut queue: EventQueue<TestClient> = conn.new_event_queue();
    let qh = queue.handle();
    let display = conn.display();
    let _registry = display.get_registry(&qh, ());

    let mut client = TestClient::new();
    round_trip_until_bound(&conn, &mut queue, &mut client, tilo_test_env::hang_bound());
    let compositor = client
        .compositor
        .clone()
        .expect("compositor global never advertised");
    let wm_base = client
        .wm_base
        .clone()
        .expect("xdg_wm_base global never advertised");

    let surface = compositor.create_surface(&qh, ());
    let xdg_surface = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg_surface.get_toplevel(&qh, ());
    toplevel.set_app_id(app_id.clone());
    surface.commit();
    let _ = conn.flush();
    // Roundtrip so the server processes get_toplevel + set_app_id + commit;
    // this only unsticks the request queue, the state dump is polled below.
    for _ in 0..10 {
        let _ = conn.roundtrip();
        let _ = queue.dispatch_pending(&mut client);
    }

    // Poll the state dump until the foreign tab with our app-id appears.
    let adopted = poll_for(tilo_test_env::hang_bound(), || {
        // Re-pump inside the poll: without it the test blocks on the file
        // system alone and nothing drives the compositor's loop after the
        // initial burst.
        let _ = conn.roundtrip();
        let _ = queue.dispatch_pending(&mut client);
        dump_has_foreign_app_id(dump_path, &app_id)
    });
    if !adopted {
        // Echo the dump on failure so CI logs show the actual shell-state shape.
        match std::fs::read_to_string(dump_path) {
            Ok(s) => eprintln!("dump contents at failure:\n{s}"),
            Err(e) => eprintln!("dump unreadable at failure: {e}"),
        }
    }
    assert!(
        adopted,
        "compositor never adopted a foreign tab with app_id={app_id} (dump at {})",
        dump_path.display()
    );

    // `wayland-client` proxies don't auto-destroy on drop, so explicit
    // `.destroy()` calls are what fire smithay's `toplevel_destroyed`;
    // xdg-shell requires reverse-creation order.
    toplevel.destroy();
    xdg_surface.destroy();
    surface.destroy();
    let _ = conn.flush();
    // Drive the queue so the destroys reach the server before the drop.
    let _ = conn.roundtrip();
    drop(queue);
    drop(conn);

    let reaped = poll_for(tilo_test_env::hang_bound(), || {
        // `Some(true)` means "still adopted"; we want the inverse.
        match dump_has_foreign_app_id(dump_path, &app_id) {
            Some(false) => Some(true),
            Some(true) => Some(false),
            None => None,
        }
    });
    if !reaped {
        match std::fs::read_to_string(dump_path) {
            Ok(s) => eprintln!("dump contents at reap failure:\n{s}"),
            Err(e) => eprintln!("dump unreadable at reap failure: {e}"),
        }
    }
    assert!(
        reaped,
        "compositor never reaped foreign tab app_id={app_id} after client teardown"
    );
}
