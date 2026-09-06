//! End-to-end clipboard, primary selection and drag-and-drop, against a real
//! qOS compositor with real Wayland clients — no mocks, no in-process
//! `State`, no `arboard`. Each client connects to a `UnixStream` inside the
//! compositor's private runtime dir, never `Connection::connect_to_env()` or
//! `WAYLAND_DISPLAY`.
//!
//! Why: under a nested GNOME session `arboard` reaches X11 through Xwayland
//! and *appears* to work, so a clipboard test could pass while the qOS
//! compositor had no selection protocol at all.
//! [`TestClient::assert_is_qos_compositor`] checks the seat name the peer
//! reports, so a reroute to another compositor fails loudly instead of the
//! suite passing against a session it never touched.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read};
use std::os::unix::io::{FromRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use rand_core::OsRng;
use threshold_identity::ThresholdIdentity;
use ui::widget::identity_flow::IdentityRecord;
use wayland_client::protocol::{
    wl_buffer::WlBuffer,
    wl_compositor::WlCompositor,
    wl_data_device::{self, WlDataDevice},
    wl_data_device_manager::{DndAction, WlDataDeviceManager},
    wl_data_offer::{self, WlDataOffer},
    wl_data_source::{self, WlDataSource},
    wl_keyboard::{self, WlKeyboard},
    wl_pointer::{self, WlPointer},
    wl_registry::{self, WlRegistry},
    wl_seat::{self, WlSeat},
    wl_shm::{Format, WlShm},
    wl_shm_pool::WlShmPool,
    wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, delegate_noop};
use wayland_protocols::wp::primary_selection::zv1::client::{
    zwp_primary_selection_device_manager_v1::ZwpPrimarySelectionDeviceManagerV1,
    zwp_primary_selection_device_v1::{self, ZwpPrimarySelectionDeviceV1},
    zwp_primary_selection_offer_v1::{self, ZwpPrimarySelectionOfferV1},
    zwp_primary_selection_source_v1::{self, ZwpPrimarySelectionSourceV1},
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::{self, XdgToplevel},
    xdg_wm_base::{self, XdgWmBase},
};

/// The mime type qOS offers text selections as. Must match
/// `shell::compositor::selection::TEXT_MIME` — asserted below so a change on
/// one side cannot drift silently past the other.
const TEXT_MIME: &str = "text/plain;charset=utf-8";

/// The seat name the qOS compositor registers. Used as the proof that a client
/// is talking to *this* compositor and not a session one.
const QOS_SEAT_NAME: &str = "quick-desktop";

/// How long any single wait-for-condition may take. Generous, because a cold
/// headless compositor spins up a wgpu offscreen renderer on its first frame.
const DEADLINE: Duration = Duration::from_secs(25);

// Compositor harness

struct Compositor {
    child: Child,
    runtime_dir: PathBuf,
    /// The headless screen, `(width, height)` — what `pointer_target` scans.
    size: (i32, i32),
    /// The public socket — what a sandboxed app gets.
    wayland_socket: PathBuf,
    /// The privileged socket — what a clipboard manager gets.
    privileged_socket: PathBuf,
    control_socket: PathBuf,
    /// Everything the compositor wrote to stderr, so a failure can quote the
    /// compositor's own account of what it did rather than only the client's.
    log: Arc<Mutex<Vec<String>>>,
}

impl Compositor {
    /// Boot a headless compositor and wait until every socket is published;
    /// panics with the compositor's log on failure rather than turning a broken
    /// compositor into a skipped test.
    fn start() -> Self {
        Self::start_with_home(None)
    }

    /// [`Self::start`], with the compositor's `HOME` pointed at `home` so the
    /// hosted file manager opens on a directory the test prepared.
    fn start_with_home(home: Option<&Path>) -> Self {
        Self::start_with(home, (1280, 800))
    }

    /// Start with a deliberately persisted input configuration. This is kept
    /// inside the fixture's XDG tree so an input-layout test cannot ever read
    /// or rewrite the person running it’s desktop preferences.
    fn start_with_input_settings(input: serde_json::Value) -> Self {
        Self::start_with_fixture_settings((1280, 800), serde_json::json!({ "input": input }))
    }

    /// [`Self::start_with_home`] on a screen of `size`. A wider screen keeps
    /// two window groups on screen at once when one of them is the demo
    /// group, whose tabs are laid out at their normal width.
    fn start_with(home: Option<&Path>, size: (i32, i32)) -> Self {
        Self::start_with_config(home, size, None, false)
    }

    /// Host-desktop mode is intentionally a separate fixture mode: it is the
    /// only supported configuration in which qOS starts the rootless Xwayland
    /// bridge. It still uses the same private XDG tree and Wayland socket as
    /// every other test, never the caller's session.
    fn start_host_desktop() -> Self {
        Self::start_with_config(None, (1280, 800), None, true)
    }

    fn start_with_fixture_settings(size: (i32, i32), settings: serde_json::Value) -> Self {
        Self::start_with_config(None, size, Some(settings), false)
    }

    fn start_with_config(
        home: Option<&Path>,
        size: (i32, i32),
        settings: Option<serde_json::Value>,
        host_desktop: bool,
    ) -> Self {
        // SUN_LEN caps a unix socket path (~108 bytes), so the runtime dir must be
        // short — `/run/user/$UID` also keeps a stray socket out of the repo.
        let runtime_dir = PathBuf::from(format!(
            "/run/user/{}/qos-clip-{}-{}",
            unsafe { libc::getuid() },
            std::process::id(),
            next_id(),
        ));
        std::fs::create_dir_all(&runtime_dir).expect("create private runtime dir");
        if let Some(settings) = settings {
            let settings_path = runtime_dir.join("config/qos/settings.json");
            let parent = settings_path.parent().expect("settings parent");
            std::fs::create_dir_all(parent).expect("create fixture config root");
            std::fs::write(
                &settings_path,
                serde_json::to_vec(&settings).expect("encode fixture settings"),
            )
            .expect("write fixture settings");
        }
        let control_socket = runtime_dir.join("ctl");
        // Production first boot is intentionally empty. This harness predates
        // that policy and exercises the hosted Files tab, so seed the demo
        // tree explicitly instead of depending on the production default.
        let state_path = runtime_dir.join("state.json");
        shell::shell::persist::save_tree_to(
            &shell::shell::state::WorkspaceTree::bootstrap_demo(),
            &state_path,
        )
        .expect("seed the clipboard e2e desktop");
        let identity_root = Self::seed_identity(&runtime_dir);

        let mut child = Self::spawn_compositor(
            home,
            size,
            &runtime_dir,
            &control_socket,
            &state_path,
            &identity_root,
            host_desktop,
        );
        let log = Arc::new(Mutex::new(Vec::<String>::new()));
        let rx = Self::drain_log_pipes(&mut child, &log);
        let found = Self::wait_for_sockets(&rx, &mut child, &log);

        let this = Self {
            child,
            size,
            wayland_socket: runtime_dir.join(&found["public"]),
            privileged_socket: runtime_dir.join(&found["privileged"]),
            control_socket: PathBuf::from(&found["control"]),
            runtime_dir,
            log: Arc::clone(&log),
        };
        assert!(
            this.wayland_socket.exists(),
            "compositor announced {} but no socket exists there",
            this.wayland_socket.display()
        );
        assert!(
            this.privileged_socket.exists(),
            "compositor announced a privileged socket at {} but none exists",
            this.privileged_socket.display()
        );
        this
    }

    /// Give this desktop fixture an already-completed identity ceremony.
    /// Clipboard tests exercise normal desktop clients; leaving the fixture
    /// identity empty correctly activates OOBE, which must withhold focus.
    fn seed_identity(runtime_dir: &Path) -> PathBuf {
        let identity_dir = runtime_dir.join("config/qos/identity");
        let identity = ThresholdIdentity::bootstrap_single_device(&mut OsRng)
            .expect("create clipboard fixture identity");
        let record = IdentityRecord::from_threshold_identity(
            &identity,
            "Clipboard test",
            [128, 128, 128],
            SystemTime::UNIX_EPOCH,
        )
        .expect("encode clipboard fixture identity");
        let bytes = record
            .to_json_bytes()
            .expect("serialize clipboard fixture identity");
        let provider = runtime_identity::BrokerFileProvider::new(&identity_dir);
        runtime_identity::import_identity_with_provider(
            &identity_dir,
            "primary",
            &bytes,
            &provider,
        )
        .expect("persist clipboard fixture identity");
        runtime_dir.join("identity-root")
    }

    /// Spawn the `shell` binary under test against a throwaway `HOME`/XDG env,
    /// pointed at the fixture's runtime dir and sockets. Split out of
    /// [`Self::start_with`] to keep it short.
    fn spawn_compositor(
        home: Option<&Path>,
        size: (i32, i32),
        runtime_dir: &Path,
        control_socket: &Path,
        state_path: &Path,
        identity_root: &Path,
        host_desktop: bool,
    ) -> Child {
        let mut command = Command::new(env!("CARGO_BIN_EXE_shell"));
        if let Some(home) = home {
            command.env("HOME", home).env("QOS_USER_HOME", home);
        }
        command.args(["--mode", "native", "--backend", "headless"]);
        if host_desktop {
            command.arg("--host-desktop-session");
            // The nested headless lane intentionally has no DRM render node.
            // Keep Xwayland on its software path; the production session does
            // not set these and continues to use hardware acceleration.
            command
                .env("XWAYLAND_NO_GLAMOR", "1")
                .env("LIBGL_ALWAYS_SOFTWARE", "1");
            // Cargo runs this repository in its declared build container.
            // Use the immutable host's installed compatibility helper there,
            // while leaving normal production PATH resolution untouched.
            let host_bin = Path::new("/run/host/usr/bin/xwayland-satellite");
            if host_bin.is_file() {
                let mut paths = vec![PathBuf::from("/run/host/usr/bin")];
                if let Some(current) = std::env::var_os("PATH") {
                    paths.extend(std::env::split_paths(&current));
                }
                command.env(
                    "PATH",
                    std::env::join_paths(paths).expect("compose host helper PATH"),
                );
            }
        }
        command
            .env("XDG_RUNTIME_DIR", runtime_dir)
            // Point every XDG dir at the throwaway runtime dir so the test neither
            // reads nor writes the developer's real desktop, and each run starts from
            // the same known tree.
            .env("XDG_CONFIG_HOME", runtime_dir.join("config"))
            .env("XDG_DATA_HOME", runtime_dir.join("data"))
            .env("XDG_STATE_HOME", runtime_dir.join("state"))
            .env("XDG_CACHE_HOME", runtime_dir.join("cache"))
            // A CI checkout commonly lives at `/workspace`. It is not a qOS
            // collaborative mount, and must not win over the fixture home as
            // the hosted Files app's initial directory.
            .env("QOS_WORKSPACE_ROOTS", "")
            .env("QOS_STATE_PATH", state_path)
            .env("QOS_IDENTITY_ROOT", identity_root)
            .env("QOS_TEST_CONTROL", control_socket)
            .env("NIRIOS_HEADLESS_SIZE", format!("{}x{}", size.0, size.1))
            // smithay logs selection/drag decisions at debug; "denying drag from
            // client without implicit grab" is what distinguishes a rejected drag from
            // one delivered to the wrong surface.
            .env(
                "RUST_LOG",
                "warn,smithay::wayland::selection=debug,shell=debug,file_explorer=debug",
            )
            // Removing both display variables is what keeps this suite from ever
            // reaching the developer's session — the compositor binds a fresh socket
            // with nothing to fall back onto.
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("DISPLAY")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn the compositor under test")
    }

    /// Drain the compositor's stdout/stderr pipes on background threads —
    /// unread output blocks the compositor mid-write, surfacing as an
    /// unrelated timeout much later. Returns the channel that
    /// [`Self::wait_for_sockets`] reads the announced socket paths from.
    /// Split out of [`Self::start_with`] to keep it short.
    fn drain_log_pipes(
        child: &mut Child,
        log: &Arc<Mutex<Vec<String>>>,
    ) -> mpsc::Receiver<(String, String)> {
        // `tracing` goes to stdout, the compositor's `eprintln!`s to stderr;
        // missing either can hide the one line that explains a failure.
        let stdout = child.stdout.take().expect("compositor stdout");
        let stdout_log = Arc::clone(log);
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Ok(mut l) = stdout_log.lock() {
                    l.push(line);
                }
            }
        });

        let (tx, rx) = mpsc::channel::<(String, String)>();
        let stderr = child.stderr.take().expect("compositor stderr");
        let log_writer = Arc::clone(log);
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                for (marker, key) in [
                    ("nirios compositor listening on ", "public"),
                    ("nirios compositor privileged socket on ", "privileged"),
                    ("nirios compositor test_control listening on ", "control"),
                ] {
                    if let Some(rest) = line.split_once(marker).map(|(_, r)| r) {
                        // The compositor prints an `OsString` in `Debug` form.
                        let value = rest
                            .trim()
                            .trim_start_matches('"')
                            .trim_end_matches('"')
                            .to_string();
                        let _ = tx.send((key.to_string(), value));
                    }
                }
                if let Ok(mut l) = log_writer.lock() {
                    l.push(line);
                }
            }
        });
        rx
    }

    /// Block until all three sockets (public, privileged, control) have been
    /// announced, or panic with the compositor's log on timeout. Split out of
    /// [`Self::start_with`] to keep it short.
    fn wait_for_sockets(
        rx: &mpsc::Receiver<(String, String)>,
        child: &mut Child,
        log: &Arc<Mutex<Vec<String>>>,
    ) -> HashMap<String, String> {
        let mut found: HashMap<String, String> = HashMap::new();
        let started = Instant::now();
        while found.len() < 3 {
            let remaining = DEADLINE.saturating_sub(started.elapsed());
            if let Ok((key, value)) = rx.recv_timeout(remaining) {
                found.insert(key, value);
            } else {
                let _ = child.kill();
                panic!(
                    "compositor did not publish all three sockets within {DEADLINE:?}; \
                     got {found:?}. Compositor log:\n{}",
                    log.lock().map(|l| l.join("\n")).unwrap_or_default()
                );
            }
        }
        found
    }

    fn control(&self, request: &serde_json::Value) -> serde_json::Value {
        use std::io::Write;
        let mut stream =
            UnixStream::connect(&self.control_socket).expect("connect to the test-control socket");
        stream
            .set_read_timeout(Some(DEADLINE))
            .expect("set control read timeout");
        let mut line = serde_json::to_vec(request).expect("encode control request");
        line.push(b'\n');
        stream.write_all(&line).expect("write control request");
        let mut reader = BufReader::new(stream);
        let mut response = String::new();
        reader
            .read_line(&mut response)
            .expect("read control response");
        serde_json::from_str(&response)
            .unwrap_or_else(|e| panic!("control response was not JSON ({e}): {response:?}"))
    }

    fn chrome_copy(&self, text: &str, primary: bool) {
        let resp = self.control(&serde_json::json!({
            "op": "clipboard_copy", "text": text, "primary": primary,
        }));
        assert!(
            resp.get("ok").is_some(),
            "chrome copy was refused: {resp:?}"
        );
    }

    fn chrome_paste(&self, primary: bool) -> Option<String> {
        let resp = self.control(&serde_json::json!({
            "op": "clipboard_paste", "primary": primary,
        }));
        let clipboard = resp
            .get("clipboard")
            .unwrap_or_else(|| panic!("expected a clipboard response, got {resp:?}"));
        clipboard
            .get("text")
            .and_then(|t| t.as_str())
            .map(str::to_string)
    }

    fn recent_log(&self) -> String {
        self.log
            .lock()
            .map(|l| {
                let start = l.len().saturating_sub(40);
                l[start..].join("\n")
            })
            .unwrap_or_default()
    }

    /// Compact view of the compositor's live shell state, for failure
    /// messages. A focus or adoption problem is unreadable from "timed out";
    /// it is obvious from the tab list.
    fn debug_state(&self) -> String {
        let resp = self.control(&serde_json::json!({"op": "state_dump"}));
        let Some(tree) = resp.get("state").and_then(|s| s.get("workspace_tree")) else {
            return format!("<no workspace_tree in {resp}>");
        };
        let mut tabs = Vec::new();
        let mut groups = 0usize;
        walk_json(tree, &mut |value| {
            if let Some(foreign) = value.get("Foreign") {
                tabs.push(foreign.to_string());
            }
            if value.get("window_groups").is_some() {
                groups = groups.saturating_add(
                    value
                        .get("window_groups")
                        .and_then(|g| g.as_array())
                        .map_or(0, std::vec::Vec::len),
                );
            }
        });
        let mut all_tabs = Vec::new();
        walk_json(tree, &mut |value| {
            if let (Some(title), Some(_)) = (value.get("title"), value.get("tile")) {
                all_tabs.push(title.to_string());
            }
        });
        format!(
            "window groups: {groups}\nall tabs: [{}]\nforeign tiles: [{}]\n\
             active_strip: {}\nactive_group: {}",
            all_tabs.join(", "),
            tabs.join(", "),
            tree.get("active_strip")
                .map_or_else(|| "?".into(), std::string::ToString::to_string),
            tree.get("active_group")
                .map_or_else(|| "?".into(), std::string::ToString::to_string),
        )
    }

    /// Open a client connection on the public socket — the one a sandboxed app
    /// gets, and the only one the launcher bind-mounts into a sandbox.
    fn connect(&self) -> Connection {
        Self::connect_to(&self.wayland_socket)
    }

    fn connect_privileged(&self) -> Connection {
        Self::connect_to(&self.privileged_socket)
    }

    fn connect_to(path: &Path) -> Connection {
        let stream = UnixStream::connect(path)
            .unwrap_or_else(|e| panic!("connect to the compositor socket {}: {e}", path.display()));
        Connection::from_socket(stream).expect("wayland connection from socket")
    }
}

impl Drop for Compositor {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.runtime_dir);
    }
}

/// Visit every object in a JSON tree. Used to find a foreign tile wherever the
/// workspace tree happens to nest it, so the test does not encode the tree's
/// exact shape and break on an unrelated refactor.
fn walk_json(value: &serde_json::Value, f: &mut impl FnMut(&serde_json::Value)) {
    match value {
        serde_json::Value::Object(map) => {
            f(value);
            for v in map.values() {
                walk_json(v, f);
            }
        }
        serde_json::Value::Array(items) => {
            for v in items {
                walk_json(v, f);
            }
        }
        _ => {}
    }
}

/// Monotonic suffix so two compositors in one test process never collide on a
/// runtime-dir name.
fn next_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    N.fetch_add(1, Ordering::Relaxed)
}

// Wayland test client

#[derive(Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool tracks an independent, orthogonal piece of Wayland protocol state \
              (documented individually below), not a set of mutually exclusive variants an \
              enum would express better"
)]
struct ClientState {
    globals: Vec<(String, u32, u32)>,
    compositor: Option<WlCompositor>,
    wm_base: Option<XdgWmBase>,
    seat: Option<WlSeat>,
    seat_name: Option<String>,
    data_manager: Option<WlDataDeviceManager>,
    primary_manager: Option<ZwpPrimarySelectionDeviceManagerV1>,
    shm: Option<WlShm>,

    /// Set once `xdg_surface.configure` has been acked — the surface is mapped.
    configured: bool,
    /// Set while this client holds keyboard focus, which gates `set_selection`.
    keyboard_focused: bool,
    /// The exact XKB text keymap the compositor sent on keyboard focus. A
    /// protocol client must make decisions from this map, not from the
    /// compositor's preferred locale or from the test process environment.
    keyboard_keymap: Option<String>,
    /// Raw evdev transitions observed on the real Wayland socket. These are
    /// deliberately captured before toolkit translation, allowing the test to
    /// prove that a held modifier/release pair reaches a foreign client.
    keyboard_events: Vec<(u32, bool)>,
    /// The most recent protocol repeat contract, `(rate_hz, delay_ms)`.
    keyboard_repeat_info: Option<(i32, i32)>,
    /// Latest serial from any input event, for requests that need one.
    last_serial: u32,
    /// Set once the pointer entered this surface — the implicit grab `start_drag` needs.
    pointer_entered: bool,
    pointer_button_serial: Option<u32>,

    /// Mime types on the most recent clipboard offer, and the offer itself.
    clipboard_offer: Option<WlDataOffer>,
    clipboard_offer_mimes: Vec<String>,
    /// Set when the compositor delivered `wl_data_device.selection` — a clipboard, not a drag.
    clipboard_selection_seen: bool,

    primary_offer: Option<ZwpPrimarySelectionOfferV1>,
    primary_offer_mimes: Vec<String>,
    primary_selection_seen: bool,

    /// Drag-and-drop: the offer that entered this client's surface.
    dnd_offer: Option<WlDataOffer>,
    dnd_offer_mimes: Vec<String>,
    dnd_entered: bool,
    dnd_dropped: bool,

    /// Mime types this client was asked to serve. Text isn't stored here — it
    /// rides as each source's user data, so a client owning both selections
    /// serves each from its own string.
    send_requests: Vec<String>,
    /// Set when the compositor told our source it was superseded.
    source_cancelled: bool,
}

struct TestClient {
    conn: Connection,
    queue: EventQueue<ClientState>,
    qh: QueueHandle<ClientState>,
    state: ClientState,
    surface: Option<WlSurface>,
    toplevel: Option<XdgToplevel>,
    data_device: Option<WlDataDevice>,
    primary_device: Option<ZwpPrimarySelectionDeviceV1>,
    app_id: String,
}

impl TestClient {
    fn new(conn: Connection, app_id: &str) -> Self {
        let mut queue: EventQueue<ClientState> = conn.new_event_queue();
        let qh = queue.handle();
        let mut state = ClientState::default();
        let _registry = conn.display().get_registry(&qh, ());
        // First roundtrip collects the global advertisements; the second lets
        // the binds made while handling them complete.
        queue.roundtrip(&mut state).expect("registry roundtrip");
        queue.roundtrip(&mut state).expect("bind roundtrip");
        Self {
            conn,
            queue,
            qh,
            state,
            surface: None,
            toplevel: None,
            data_device: None,
            primary_device: None,
            app_id: app_id.to_string(),
        }
    }

    fn assert_is_qos_compositor(&mut self) {
        self.roundtrip();
        assert_eq!(
            self.state.seat_name.as_deref(),
            Some(QOS_SEAT_NAME),
            "connected to something that is not the qOS compositor (seat name mismatch). \
             Globals seen: {:?}",
            self.global_names(),
        );
    }

    fn global_names(&self) -> Vec<String> {
        self.state
            .globals
            .iter()
            .map(|(n, _, _)| n.clone())
            .collect()
    }

    fn has_global(&self, name: &str) -> bool {
        self.state.globals.iter().any(|(n, _, _)| n == name)
    }

    fn roundtrip(&mut self) {
        self.queue
            .roundtrip(&mut self.state)
            .expect("client roundtrip");
    }

    fn clear_keyboard_trace(&mut self) {
        self.state.keyboard_events.clear();
    }

    fn wait_for_keyboard_events(&mut self, at_least: usize) {
        self.wait_until(
            |s| s.keyboard_events.len() >= at_least,
            "the focused client did not receive the expected keyboard transitions",
        );
    }

    /// Map an xdg-toplevel and return once the compositor has given this
    /// client keyboard focus — the precondition for setting a selection.
    fn map_window(&mut self, compositor: &Compositor) {
        let wl_compositor = self
            .state
            .compositor
            .clone()
            .expect("compositor advertises wl_compositor");
        let wm_base = self
            .state
            .wm_base
            .clone()
            .expect("compositor advertises xdg_wm_base");
        let surface = wl_compositor.create_surface(&self.qh, ());
        let xdg_surface = wm_base.get_xdg_surface(&surface, &self.qh, ());
        let toplevel = xdg_surface.get_toplevel(&self.qh, ());
        toplevel.set_title(format!("clipboard-e2e {}", self.app_id));
        toplevel.set_app_id(self.app_id.clone());
        surface.commit();
        self.surface = Some(surface.clone());
        self.toplevel = Some(toplevel);
        self.step(&format!(
            "toplevel committed (wl_surface protocol id {}), waiting for keyboard focus",
            surface.id().protocol_id()
        ));

        let seat = self
            .state
            .seat
            .clone()
            .expect("compositor advertises wl_seat");
        if let Some(mgr) = self.state.data_manager.clone() {
            self.data_device = Some(mgr.get_data_device(&seat, &self.qh, ()));
        }
        if let Some(mgr) = self.state.primary_manager.clone() {
            self.primary_device = Some(mgr.get_device(&seat, &self.qh, ()));
        }

        // A toplevel with no buffer has no size (tile laid out at zero area, not
        // addressable) — attach a real wl_shm buffer, which is also just what an
        // app does.
        self.wait_until(
            |s| s.configured,
            "the compositor never sent an xdg_surface.configure",
        );
        self.attach_buffer(320, 240);

        let started = Instant::now();
        while started.elapsed() < DEADLINE {
            if self.state.keyboard_focused {
                self.step("focused");
                return;
            }
            self.pump();
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!(
            "the compositor never gave `{}` keyboard focus after mapping a toplevel \
             (waited {DEADLINE:?}). Setting a selection requires focus, so nothing \
             downstream can work without it.\n{}",
            self.app_id,
            compositor.debug_state(),
        );
    }

    /// Find a screen position at which this client receives pointer input.
    /// Asked of the client, not computed from the compositor's layout: its own
    /// `wl_pointer.enter` is the only authority, so a strip-layout change moves
    /// the drag with it instead of aiming at chrome.
    fn pointer_target(&mut self, compositor: &Compositor) -> (f64, f64) {
        if let Some(p) = self.try_pointer_target(compositor) {
            return p;
        }
        let (w, h) = compositor.size;
        panic!(
            "`{}` never received a pointer enter anywhere on a {w}x{h} screen, so its \
             window is not on screen and no drag could be aimed at it.\n{}",
            self.app_id,
            compositor.debug_state(),
        );
    }

    /// [`Self::pointer_target`] without the panic.
    fn try_pointer_target(&mut self, compositor: &Compositor) -> Option<(f64, f64)> {
        const STEP: i32 = 40;
        let (w, h) = compositor.size;
        self.state.pointer_entered = false;
        let mut y = STEP / 2;
        while y < h {
            let mut x = STEP / 2;
            while x < w {
                compositor.pointer_move(f64::from(x), f64::from(y));
                self.pump();
                if self.state.pointer_entered {
                    self.step(&format!("receives pointer input at ({x}, {y})"));
                    return Some((f64::from(x), f64::from(y)));
                }
                x = x.saturating_add(STEP);
            }
            y = y.saturating_add(STEP);
        }
        None
    }

    /// Attach an opaque `wl_shm` buffer and commit, so the compositor has real
    /// content to size and composite the tile from.
    fn attach_buffer(&mut self, width: i32, height: i32) {
        use std::os::unix::io::AsFd;
        let shm = self
            .state
            .shm
            .clone()
            .expect("compositor advertises wl_shm");
        let stride = width.saturating_mul(4);
        let len = usize::try_from(stride.saturating_mul(height)).unwrap_or(0);

        // SAFETY: a null-terminated name and a valid flag word are the whole
        // contract of `memfd_create`.
        let raw = unsafe { libc::memfd_create(c"qos-clipboard-e2e".as_ptr(), 0) };
        assert!(
            raw >= 0,
            "memfd_create: {}",
            std::io::Error::last_os_error()
        );
        // SAFETY: `memfd_create` just handed us this descriptor and nothing
        // else owns it.
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        let file = std::fs::File::from(fd);
        file.set_len(len as u64).expect("size the shm pool");
        // Opaque mid-grey, so a rendered frame is visibly the client's own.
        let pixels = vec![0x80u8; len];
        {
            use std::os::unix::fs::FileExt;
            file.write_at(&pixels, 0).expect("fill the shm pool");
        }

        let pool = shm.create_pool(
            file.as_fd(),
            i32::try_from(len).unwrap_or(i32::MAX),
            &self.qh,
            (),
        );
        let buffer = pool.create_buffer(0, width, height, stride, Format::Argb8888, &self.qh, ());
        let surface = self.surface.clone().expect("surface exists once mapped");
        surface.attach(Some(&buffer), 0, 0);
        surface.damage(0, 0, width, height);
        surface.commit();
        self.roundtrip();
        self.step(&format!("attached a {width}x{height} buffer"));
    }

    /// Note a milestone, so a run that stalls says where under `--nocapture`.
    fn step(&self, what: &str) {
        eprintln!("[clipboard-e2e] {}: {what}", self.app_id);
    }

    /// Pump this client until `cond` holds, failing with `what` on timeout.
    /// Built on `roundtrip` + sleep rather than `blocking_dispatch`: a blocking
    /// dispatch parks until the compositor sends something, so a compositor
    /// that sends *nothing* — the exact failure this suite catches — would hang
    /// instead of failing.
    fn wait_until(&mut self, cond: impl Fn(&ClientState) -> bool, what: &str) {
        let started = Instant::now();
        while started.elapsed() < DEADLINE {
            if cond(&self.state) {
                return;
            }
            self.pump();
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("{what} (waited {DEADLINE:?}, app_id {})", self.app_id);
    }

    /// Pump without requiring progress, for waits on something outside this client.
    fn pump(&mut self) {
        self.queue
            .roundtrip(&mut self.state)
            .expect("client roundtrip while pumping");
    }

    /// Take ownership of the clipboard. The source stays alive on `self.state`
    /// so the client can answer the compositor's forwarded `send` request.
    fn set_clipboard(&mut self, text: &str) {
        let mgr = self
            .state
            .data_manager
            .clone()
            .expect("compositor advertises wl_data_device_manager");
        let device = self
            .data_device
            .clone()
            .expect("data device bound in map_window");
        let source = mgr.create_data_source(&self.qh, text.to_string());
        source.offer(TEXT_MIME.to_string());
        device.set_selection(Some(&source), self.state.last_serial);
        self.roundtrip();
    }

    fn set_primary(&mut self, text: &str) {
        let mgr = self
            .state
            .primary_manager
            .clone()
            .expect("compositor advertises zwp_primary_selection_device_manager_v1");
        let device = self
            .primary_device
            .clone()
            .expect("primary device bound in map_window");
        let source = mgr.create_source(&self.qh, text.to_string());
        source.offer(TEXT_MIME.to_string());
        device.set_selection(Some(&source), self.state.last_serial);
        self.roundtrip();
    }

    /// Read the current clipboard. `peer` is pumped while the transfer runs —
    /// when another client in this process owns the selection, it must be
    /// dispatched to answer the `send` request or nothing gets written.
    fn read_clipboard(&mut self, peer: Option<&mut TestClient>) -> String {
        self.wait_until(
            |s| s.clipboard_selection_seen && s.clipboard_offer.is_some(),
            "no clipboard offer ever arrived",
        );
        assert!(
            self.state
                .clipboard_offer_mimes
                .iter()
                .any(|m| m == TEXT_MIME),
            "clipboard offer did not advertise {TEXT_MIME}; got {:?}",
            self.state.clipboard_offer_mimes,
        );
        self.step("clipboard offer received, requesting the bytes");
        let offer = self.state.clipboard_offer.clone().expect("clipboard offer");
        let (read_fd, write_fd) = make_pipe();
        offer.receive(TEXT_MIME.to_string(), write_fd.as_fd_ref());
        drop(write_fd);
        self.conn.flush().expect("flush the receive request");
        drain(read_fd, self, peer)
    }

    fn read_primary(&mut self, peer: Option<&mut TestClient>) -> String {
        self.wait_until(
            |s| s.primary_selection_seen && s.primary_offer.is_some(),
            "no primary-selection offer ever arrived",
        );
        assert!(
            self.state
                .primary_offer_mimes
                .iter()
                .any(|m| m == TEXT_MIME),
            "primary offer did not advertise {TEXT_MIME}; got {:?}",
            self.state.primary_offer_mimes,
        );
        let offer = self.state.primary_offer.clone().expect("primary offer");
        let (read_fd, write_fd) = make_pipe();
        offer.receive(TEXT_MIME.to_string(), write_fd.as_fd_ref());
        drop(write_fd);
        self.conn.flush().expect("flush the receive request");
        drain(read_fd, self, peer)
    }

    fn read_dnd(&mut self, peer: &mut TestClient) -> String {
        let bytes = self.read_dnd_bytes(TEXT_MIME, Some(peer));
        String::from_utf8(bytes).expect("selection bytes were not UTF-8")
    }

    /// Read the drag-and-drop offer as `mime`; `peer` is pumped when the source
    /// is another test client (a compositor-sourced drag needs none).
    fn read_dnd_bytes(&mut self, mime: &str, peer: Option<&mut TestClient>) -> Vec<u8> {
        let offer = self.state.dnd_offer.clone().expect("a drag offer");
        let (read_fd, write_fd) = make_pipe();
        offer.receive(mime.to_string(), write_fd.as_fd_ref());
        drop(write_fd);
        self.conn.flush().expect("flush the receive request");
        drain_bytes(read_fd, self, peer)
    }
}

/// An `OwnedFd` plus a borrow helper, so `receive` can take `&fd`.
struct Fd(OwnedFd);

impl Fd {
    fn as_fd_ref(&self) -> std::os::unix::io::BorrowedFd<'_> {
        use std::os::unix::io::AsFd;
        self.0.as_fd()
    }
}

fn make_pipe() -> (Fd, Fd) {
    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: `fds` is a live two-element array, which is what pipe2 fills.
    let rc = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) };
    assert!(rc == 0, "pipe2 failed: {}", std::io::Error::last_os_error());
    // SAFETY: both descriptors were just created by pipe2 and are unowned.
    unsafe {
        (
            Fd(OwnedFd::from_raw_fd(fds[0])),
            Fd(OwnedFd::from_raw_fd(fds[1])),
        )
    }
}

/// Read `fd` to EOF while pumping both clients, and return it as UTF-8.
/// The pumping is the trick: a same-process transfer only completes if the
/// *owner* is dispatched to answer the `send` request.
fn drain(fd: Fd, reader: &mut TestClient, peer: Option<&mut TestClient>) -> String {
    String::from_utf8(drain_bytes(fd, reader, peer)).expect("selection bytes were not UTF-8")
}

fn drain_bytes(fd: Fd, reader: &mut TestClient, peer: Option<&mut TestClient>) -> Vec<u8> {
    use std::os::unix::io::AsRawFd;
    // SAFETY: `fd` owns the descriptor and outlives this call; `F_SETFL` is
    // total on any descriptor.
    unsafe {
        let flags = libc::fcntl(fd.0.as_raw_fd(), libc::F_GETFL);
        libc::fcntl(fd.0.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK);
    }
    let mut file = std::fs::File::from(fd.0);
    let mut out = Vec::new();
    let mut chunk = [0u8; 4096];
    let started = Instant::now();
    let mut peer = peer;
    while started.elapsed() < DEADLINE {
        match file.read(&mut chunk) {
            Ok(0) => return out,
            Ok(n) => out.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                reader.pump();
                if let Some(p) = peer.as_deref_mut() {
                    p.pump();
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(e) => panic!("reading the selection pipe failed: {e}"),
        }
    }
    panic!(
        "the selection transfer never reached EOF within {DEADLINE:?}; \
         got {:?} so far",
        String::from_utf8_lossy(&out)
    );
}

// Dispatch impls

impl Dispatch<WlRegistry, ()> for ClientState {
    fn event(
        state: &mut Self,
        registry: &WlRegistry,
        event: wl_registry::Event,
        (): &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        else {
            return;
        };
        state.globals.push((interface.clone(), name, version));
        match interface.as_str() {
            "wl_compositor" => {
                state.compositor = Some(registry.bind(name, version.min(4), qh, ()));
            }
            "xdg_wm_base" => {
                state.wm_base = Some(registry.bind(name, version.min(3), qh, ()));
            }
            "wl_seat" => {
                state.seat = Some(registry.bind(name, version.min(7), qh, ()));
            }
            "wl_data_device_manager" => {
                state.data_manager = Some(registry.bind(name, version.min(3), qh, ()));
            }
            "zwp_primary_selection_device_manager_v1" => {
                state.primary_manager = Some(registry.bind(name, 1, qh, ()));
            }
            "wl_shm" => {
                state.shm = Some(registry.bind(name, 1, qh, ()));
            }
            _ => {}
        }
    }
}

impl Dispatch<WlSeat, ()> for ClientState {
    fn event(
        state: &mut Self,
        seat: &WlSeat,
        event: wl_seat::Event,
        (): &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_seat::Event::Name { name } => state.seat_name = Some(name),
            wl_seat::Event::Capabilities { capabilities } => {
                let caps = match capabilities {
                    wayland_client::WEnum::Value(c) => c,
                    wayland_client::WEnum::Unknown(_) => return,
                };
                if caps.contains(wl_seat::Capability::Keyboard) {
                    seat.get_keyboard(qh, ());
                }
                if caps.contains(wl_seat::Capability::Pointer) {
                    seat.get_pointer(qh, ());
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<WlKeyboard, ()> for ClientState {
    fn event(
        state: &mut Self,
        _: &WlKeyboard,
        event: wl_keyboard::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_keyboard::Event::Enter { serial, .. } => {
                state.keyboard_focused = true;
                state.last_serial = serial;
            }
            wl_keyboard::Event::Leave { serial, .. } => {
                state.keyboard_focused = false;
                state.last_serial = serial;
            }
            wl_keyboard::Event::Keymap { fd, size, .. } => {
                let mut bytes = Vec::new();
                let file = std::fs::File::from(fd);
                // The protocol promises `size` bytes including a trailing
                // NUL. Bound the read to it: a malformed compositor must not
                // make this diagnostic client consume an unbounded fd.
                let take = u64::from(size);
                file.take(take)
                    .read_to_end(&mut bytes)
                    .expect("read wl_keyboard keymap");
                if bytes.last() == Some(&0) {
                    let _ = bytes.pop();
                }
                state.keyboard_keymap = Some(
                    String::from_utf8(bytes).expect("wl_keyboard keymap must be UTF-8 XKB text"),
                );
            }
            wl_keyboard::Event::Key {
                serial,
                key,
                state: key_state,
                ..
            } => {
                state.last_serial = serial;
                let pressed = matches!(
                    key_state,
                    wayland_client::WEnum::Value(wl_keyboard::KeyState::Pressed)
                );
                state.keyboard_events.push((key, pressed));
            }
            wl_keyboard::Event::Modifiers { serial, .. } => state.last_serial = serial,
            wl_keyboard::Event::RepeatInfo { rate, delay } => {
                state.keyboard_repeat_info = Some((rate, delay));
            }
            _ => {}
        }
    }
}

impl Dispatch<WlPointer, ()> for ClientState {
    fn event(
        state: &mut Self,
        _: &WlPointer,
        event: wl_pointer::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_pointer::Event::Enter { serial, .. } => {
                state.pointer_entered = true;
                state.last_serial = serial;
            }
            wl_pointer::Event::Leave { serial, .. } => {
                state.pointer_entered = false;
                state.last_serial = serial;
            }
            wl_pointer::Event::Button {
                serial, state: s, ..
            } => {
                state.last_serial = serial;
                if matches!(
                    s,
                    wayland_client::WEnum::Value(wl_pointer::ButtonState::Pressed)
                ) {
                    state.pointer_button_serial = Some(serial);
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<XdgWmBase, ()> for ClientState {
    fn event(
        _: &mut Self,
        wm_base: &XdgWmBase,
        event: xdg_wm_base::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm_base.pong(serial);
        }
    }
}

impl Dispatch<XdgSurface, ()> for ClientState {
    fn event(
        state: &mut Self,
        xdg_surface: &XdgSurface,
        event: xdg_surface::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            xdg_surface.ack_configure(serial);
            state.configured = true;
        }
    }
}

impl Dispatch<XdgToplevel, ()> for ClientState {
    fn event(
        _: &mut Self,
        _: &XdgToplevel,
        _: xdg_toplevel::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlDataDevice, ()> for ClientState {
    fn event(
        state: &mut Self,
        _: &WlDataDevice,
        event: wl_data_device::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            // A fresh offer object. Which selection it belongs to is only
            // known once `selection` or `enter` names it, so park it.
            wl_data_device::Event::DataOffer { id } => {
                state.clipboard_offer_mimes.clear();
                state.clipboard_offer = Some(id);
            }
            wl_data_device::Event::Selection { id } => {
                state.clipboard_selection_seen = true;
                if let Some(offer) = id {
                    state.clipboard_offer = Some(offer);
                } else {
                    state.clipboard_offer = None;
                    state.clipboard_offer_mimes.clear();
                }
            }
            wl_data_device::Event::Enter { id, serial, .. } => {
                state.dnd_entered = true;
                state.last_serial = serial;
                if let Some(offer) = id {
                    state.dnd_offer_mimes = std::mem::take(&mut state.clipboard_offer_mimes);
                    // Accept a type the source actually offered (text if present, else the
                    // first) and a copy action, or the source is told the drop was refused.
                    let accept = state
                        .dnd_offer_mimes
                        .iter()
                        .find(|m| m.as_str() == TEXT_MIME)
                        .or_else(|| state.dnd_offer_mimes.first())
                        .cloned();
                    offer.accept(serial, accept);
                    offer.set_actions(DndAction::Copy, DndAction::Copy);
                    state.dnd_offer = Some(offer);
                }
            }
            wl_data_device::Event::Drop => {
                state.dnd_dropped = true;
            }
            _ => {}
        }
    }

    // `wl_data_device.data_offer` is a server-created object, so the client
    // has to say what user data a new `wl_data_offer` gets.
    wayland_client::event_created_child!(ClientState, WlDataDevice, [
        wl_data_device::EVT_DATA_OFFER_OPCODE => (WlDataOffer, ()),
    ]);
}

impl Dispatch<WlDataOffer, ()> for ClientState {
    fn event(
        state: &mut Self,
        offer: &WlDataOffer,
        event: wl_data_offer::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_data_offer::Event::Offer { mime_type } = event {
            // Route the mime onto whichever offer this is. A drag offer and a
            // clipboard offer can be live at the same time.
            if state.dnd_offer.as_ref().is_some_and(|o| o == offer) {
                state.dnd_offer_mimes.push(mime_type);
            } else {
                state.clipboard_offer_mimes.push(mime_type);
            }
        }
    }
}

impl Dispatch<WlDataSource, String> for ClientState {
    fn event(
        state: &mut Self,
        _: &WlDataSource,
        event: wl_data_source::Event,
        served_text: &String,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_data_source::Event::Send { mime_type, fd } => {
                state.send_requests.push(mime_type);
                write_and_close(fd, served_text);
            }
            wl_data_source::Event::Cancelled => state.source_cancelled = true,
            _ => {}
        }
    }
}

impl Dispatch<ZwpPrimarySelectionDeviceV1, ()> for ClientState {
    fn event(
        state: &mut Self,
        _: &ZwpPrimarySelectionDeviceV1,
        event: zwp_primary_selection_device_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_primary_selection_device_v1::Event::DataOffer { offer } => {
                state.primary_offer_mimes.clear();
                state.primary_offer = Some(offer);
            }
            zwp_primary_selection_device_v1::Event::Selection { id } => {
                state.primary_selection_seen = true;
                if let Some(offer) = id {
                    state.primary_offer = Some(offer);
                } else {
                    state.primary_offer = None;
                    state.primary_offer_mimes.clear();
                }
            }
            _ => {}
        }
    }

    wayland_client::event_created_child!(ClientState, ZwpPrimarySelectionDeviceV1, [
        zwp_primary_selection_device_v1::EVT_DATA_OFFER_OPCODE
            => (ZwpPrimarySelectionOfferV1, ()),
    ]);
}

impl Dispatch<ZwpPrimarySelectionOfferV1, ()> for ClientState {
    fn event(
        state: &mut Self,
        _: &ZwpPrimarySelectionOfferV1,
        event: zwp_primary_selection_offer_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwp_primary_selection_offer_v1::Event::Offer { mime_type } = event {
            state.primary_offer_mimes.push(mime_type);
        }
    }
}

impl Dispatch<ZwpPrimarySelectionSourceV1, String> for ClientState {
    fn event(
        state: &mut Self,
        _: &ZwpPrimarySelectionSourceV1,
        event: zwp_primary_selection_source_v1::Event,
        served_text: &String,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_primary_selection_source_v1::Event::Send { mime_type, fd } => {
                state.send_requests.push(mime_type);
                write_and_close(fd, served_text);
            }
            zwp_primary_selection_source_v1::Event::Cancelled => state.source_cancelled = true,
            _ => {}
        }
    }
}

delegate_noop!(ClientState: ignore WlCompositor);
delegate_noop!(ClientState: ignore WlSurface);
delegate_noop!(ClientState: ignore WlDataDeviceManager);
delegate_noop!(ClientState: ignore ZwpPrimarySelectionDeviceManagerV1);
delegate_noop!(ClientState: ignore WlShm);
delegate_noop!(ClientState: ignore WlShmPool);
delegate_noop!(ClientState: ignore WlBuffer);

/// Answer a `send` request: write the served text and close — closing is
/// what signals EOF to the reader.
fn write_and_close(fd: OwnedFd, text: &str) {
    use std::io::Write;
    let mut file = std::fs::File::from(fd);
    let _ = file.write_all(text.as_bytes());
}

// Tests

/// Translate a trace exactly as a real Wayland toolkit does: with the XKB
/// keymap delivered by the compositor, rather than with the locale of the
/// test runner. The protocol uses evdev codes; xkbcommon uses evdev + 8.
fn translated_keysyms(keymap_text: &str, trace: &[(u32, bool)]) -> Vec<u32> {
    use smithay::input::keyboard::xkb;

    let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
    let keymap = xkb::Keymap::new_from_string(
        &context,
        keymap_text.to_owned(),
        xkb::KEYMAP_FORMAT_TEXT_V1,
        xkb::KEYMAP_COMPILE_NO_FLAGS,
    )
    .expect("the compositor sent a parseable XKB keymap");
    let mut state = xkb::State::new(&keymap);
    let mut symbols = Vec::new();
    for &(evdev, pressed) in trace {
        let keycode = xkb::Keycode::new(evdev.saturating_add(8));
        if pressed {
            symbols.push(u32::from(state.key_get_one_sym(keycode)));
        }
        let direction = if pressed {
            xkb::KeyDirection::Down
        } else {
            xkb::KeyDirection::Up
        };
        let _ = state.update_key(keycode, direction);
    }
    symbols
}

/// A GUI application can spawn helpers. Put it in its own process group so a
/// failed live-client probe cannot leave an Electron renderer attached to the
/// developer's machine after the isolated compositor is gone.
struct KillProcessGroup(Child);

impl Drop for KillProcessGroup {
    fn drop(&mut self) {
        let pid = i32::try_from(self.0.id()).unwrap_or(i32::MAX);
        // `-pid` targets the group established immediately before exec. It
        // contains only the throwaway client command below, never this test
        // runner or the caller's graphical session.
        let _ = unsafe { libc::kill(-pid, libc::SIGTERM) };
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if self.0.try_wait().ok().flatten().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        // A wedged Electron renderer must not wedge the test runner. The PID
        // is also the private process-group id established in `pre_exec`.
        let _ = unsafe { libc::kill(-pid, libc::SIGKILL) };
        let _ = self.0.wait();
    }
}

/// Start the installed ChatGPT Electron client strictly through qOS's
/// rootless Xwayland bridge. The test is opt-in because it opens a real
/// heavyweight graphical process, but it is a genuine compatibility lane:
/// private HOME/XDG state, qOS-created Xauthority, no inherited display or
/// D-Bus session, and a child process group that is always torn down.
fn spawn_isolated_chatgpt_x11(
    compositor: &Compositor,
) -> Option<(KillProcessGroup, Arc<Mutex<Vec<String>>>)> {
    let configured = std::env::var_os("QOS_TEST_ELECTRON_BINARY").map(PathBuf::from);
    let launch = configured
        .filter(|path| path.is_file())
        .map(|path| (path, None))
        .or_else(|| {
            let local = PathBuf::from("/usr/bin/chatgpt");
            local.is_file().then_some((local, None))
        })
        .or_else(|| {
            let bridge = PathBuf::from("/usr/bin/distrobox-host-exec");
            let host_app = PathBuf::from("/run/host/usr/bin/chatgpt");
            (bridge.is_file() && host_app.is_file())
                .then_some((bridge, Some(PathBuf::from("/usr/bin/chatgpt"))))
        });
    let Some((program, host_program)) = launch else {
        eprintln!(
            "SKIP Xwayland/Electron lane: no installed ChatGPT binary (set QOS_TEST_ELECTRON_BINARY to override)"
        );
        return None;
    };
    let bridge = compositor.runtime_dir.join("qos-session/xwayland");
    let deadline = Instant::now() + Duration::from_secs(10);
    let authority = loop {
        let authority = std::fs::read_dir(&bridge).ok().and_then(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .find(|path| {
                    path.file_name()
                        .is_some_and(|name| name.to_string_lossy().starts_with("Xauthority-"))
                })
        });
        if let Some(authority) = authority {
            break authority;
        }
        assert!(
            Instant::now() < deadline,
            "qOS did not publish its private Xwayland authority; compositor log:\n{}",
            compositor.recent_log()
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    let number = authority
        .file_name()?
        .to_string_lossy()
        .strip_prefix("Xauthority-")?
        .to_string();
    let profile = compositor.runtime_dir.join("chatgpt-profile");
    std::fs::create_dir_all(&profile).expect("create isolated Electron profile");

    let mut command = Command::new(&program);
    if let Some(host_program) = host_program {
        command.arg(host_program);
    }
    command
        .arg("--ozone-platform=x11")
        .env("DISPLAY", format!(":{number}"))
        .env("XAUTHORITY", authority)
        .env("HOME", &profile)
        .env("XDG_CONFIG_HOME", profile.join("config"))
        .env("XDG_DATA_HOME", profile.join("data"))
        .env("XDG_CACHE_HOME", profile.join("cache"))
        .env("XDG_STATE_HOME", profile.join("state"))
        .env("XDG_RUNTIME_DIR", &compositor.runtime_dir)
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("DBUS_SESSION_BUS_ADDRESS")
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    // SAFETY: `setpgid` is one async-signal-safe syscall before exec, with no
    // captured Rust state. The new process group makes the Drop cleanup exact.
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command
        .spawn()
        .expect("spawn isolated ChatGPT Electron client");
    let log = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&log);
    let stderr = child.stderr.take().expect("Electron stderr");
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if let Ok(mut lines) = sink.lock() {
                lines.push(line);
            }
        }
    });
    Some((KillProcessGroup(child), log))
}

#[test]
#[ignore = "starts an installed Electron client in a private Xwayland session; opt in for compatibility review"]
fn installed_electron_client_maps_through_the_private_xwayland_bridge() {
    let compositor = Compositor::start_host_desktop();
    // Keep one ordinary native Wayland client alive throughout the Electron
    // launch. A bridge that only works in an otherwise empty compositor is
    // not compatible with a desktop where qOS and host applications coexist.
    let mut native = TestClient::new(compositor.connect(), "org.qos.test.native-alongside-x11");
    native.assert_is_qos_compositor();
    native.map_window(&compositor);
    let before = compositor
        .control(&serde_json::json!({ "op": "foreign_frames" }))
        .get("foreign_frames")
        .and_then(serde_json::Value::as_array)
        .map_or(0, Vec::len);
    let Some((_child, log)) = spawn_isolated_chatgpt_x11(&compositor) else {
        return;
    };
    let started = Instant::now();
    loop {
        let frames = compositor.control(&serde_json::json!({ "op": "foreign_frames" }));
        let mapped = frames
            .get("foreign_frames")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|entries| entries.len() > before);
        if mapped {
            return;
        }
        assert!(
            started.elapsed() < Duration::from_secs(45),
            "the installed Electron client never mapped through qOS Xwayland.\nElectron log:\n{}\nCompositor log:\n{}",
            log.lock().map(|lines| lines.join("\n")).unwrap_or_default(),
            compositor.recent_log(),
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// This is deliberately a real client-to-compositor exercise, not a unit
/// test of `KeyboardSettings`: qOS must serialize the selected German keymap
/// onto the Wayland seat and forward complete press/release/modifier streams
/// to a focused foreign client. Electron and Xwayland both consume this same
/// seat contract.
#[test]
fn real_wayland_client_gets_de_nodeadkeys_symbols_modifiers_and_repeat_contract() {
    let compositor = Compositor::start_with_input_settings(serde_json::json!({
        "keyboard_layout": "de:nodeadkeys",
        "key_repeat": 0.5,
    }));
    let mut client = TestClient::new(compositor.connect(), "org.qos.test.de-keyboard");
    client.assert_is_qos_compositor();
    client.map_window(&compositor);
    client.wait_until(
        |s| s.keyboard_keymap.is_some() && s.keyboard_repeat_info.is_some(),
        "focused foreign client never received the keymap and repeat contract",
    );

    let keymap = client
        .state
        .keyboard_keymap
        .clone()
        .expect("wait above established a keymap");
    assert!(
        keymap.contains("nodeadkeys"),
        "the client received a keymap other than the persisted de:nodeadkeys layout"
    );
    let (rate, delay) = client
        .state
        .keyboard_repeat_info
        .expect("wait above established repeat info");
    assert!(
        (10..=50).contains(&rate) && (200..=600).contains(&delay),
        "repeat information must be usable by GTK/Qt/Electron clients, got rate={rate} delay={delay}"
    );

    client.clear_keyboard_trace();
    // Physical positions: code 21 is the US-Y / German-Z position; code 12
    // becomes `?` under Shift in de(nodeadkeys); code 43 becomes apostrophe.
    // Holding modifiers as separate transitions proves the protocol does not
    // collapse Shift into a synthetic text event (which breaks selection in
    // Electron and all standard toolkits).
    for (key, down) in [
        ("Shift", true),
        ("y", true),
        ("y", false),
        ("-", true),
        ("-", false),
        ("\\", true),
        ("\\", false),
        ("Shift", false),
        ("Control", true),
        ("ArrowLeft", true),
        ("ArrowLeft", false),
        ("Control", false),
        // A held editing key has one press and one release. The client owns
        // repeated delivery according to `repeat_info`; qOS must never leave
        // the key logically down or manufacture a second press on release.
        ("Backspace", true),
        ("Backspace", false),
    ] {
        let operation = if down { "key_down" } else { "key_up" };
        let response = compositor.control(&serde_json::json!({ "op": operation, "key": key }));
        assert!(
            response.get("ok").is_some(),
            "test control refused {operation} {key}: {response:?}"
        );
    }
    client.wait_for_keyboard_events(14);
    let trace = client.state.keyboard_events.clone();
    assert_eq!(
        trace,
        vec![
            (42, true),
            (21, true),
            (21, false),
            (12, true),
            (12, false),
            (43, true),
            (43, false),
            (42, false),
            (29, true),
            (105, true),
            (105, false),
            (29, false),
            (14, true),
            (14, false),
        ],
        "the foreign client must receive every physical transition exactly once"
    );
    let symbols = translated_keysyms(&keymap, &trace);
    use smithay::input::keyboard::xkb::keysyms;
    assert_eq!(
        symbols,
        vec![
            keysyms::KEY_Shift_L,
            keysyms::KEY_Z,
            keysyms::KEY_question,
            keysyms::KEY_apostrophe,
            keysyms::KEY_Control_L,
            keysyms::KEY_Left,
            keysyms::KEY_BackSpace,
        ],
        "the delivered keymap must make German punctuation, uppercase text and Ctrl+Arrow available to a real client"
    );
}

/// A real toolkit must find every global it treats as mandatory — GTK
/// refuses the display outright (stopping every GTK program) if
/// `wl_data_device_manager` is missing. qOS's own toolkit never asks for
/// it, so this asserts the full set a foreign toolkit needs.
#[test]
fn compositor_advertises_the_globals_a_toolkit_requires() {
    let compositor = Compositor::start();
    let mut client = TestClient::new(compositor.connect(), "org.qos.test.toolkit");
    client.assert_is_qos_compositor();

    // A registry that came back empty would satisfy any "is X absent" check,
    // so establish first that we actually read one.
    assert!(
        client.global_names().len() > 5,
        "the registry looks empty ({:?}) — this test would pass vacuously",
        client.global_names(),
    );

    for required in [
        "wl_compositor",
        "wl_shm",
        "wl_seat",
        "wl_data_device_manager",
        "xdg_wm_base",
    ] {
        assert!(
            client.has_global(required),
            "{required} is not advertised. A toolkit that treats it as mandatory \
             refuses the display and the application never opens a window. \
             Globals seen: {:?}",
            client.global_names(),
        );
    }
}

#[test]
fn clipboard_crosses_from_one_client_to_another() {
    let compositor = Compositor::start();
    let payload = "cross-process clipboard payload — ünïcödé ✂";

    let mut source = TestClient::new(compositor.connect(), "org.qos.test.source");
    source.assert_is_qos_compositor();
    assert!(
        source.has_global("wl_data_device_manager"),
        "the compositor must advertise wl_data_device_manager; globals: {:?}",
        source.global_names()
    );
    source.map_window(&compositor);
    source.set_clipboard(payload);

    // Mapping the second window moves focus, which is what makes the compositor
    // offer it the selection — the real sequence a user performs.
    let mut sink = TestClient::new(compositor.connect(), "org.qos.test.sink");
    sink.assert_is_qos_compositor();
    sink.map_window(&compositor);

    let received = sink.read_clipboard(Some(&mut source));
    assert_eq!(
        received, payload,
        "the second client did not receive what the first copied"
    );
    assert!(
        source.state.send_requests.iter().any(|m| m == TEXT_MIME),
        "the source client was never asked for its data, so the bytes came \
         from somewhere other than the selection: {:?}",
        source.state.send_requests
    );
}

/// Copy in the compositor's own chrome, paste in a sandboxed app. Chrome,
/// terminal and file-explorer are linked into the binary, not Wayland
/// clients, so this goes chrome -> `ui::clipboard` -> the compositor's
/// mirror -> a compositor-owned selection -> the focused client.
#[test]
fn clipboard_crosses_from_in_process_chrome_to_a_client() {
    let compositor = Compositor::start();
    let payload = "copied inside the compositor process";

    let mut client = TestClient::new(compositor.connect(), "org.qos.test.chrome-sink");
    client.assert_is_qos_compositor();
    client.map_window(&compositor);

    compositor.chrome_copy(payload, false);

    let received = client.read_clipboard(None);
    assert_eq!(
        received, payload,
        "a client did not receive what in-process chrome copied"
    );
}

/// Copy in a sandboxed app, paste in the compositor's own chrome — proves
/// the synchronous paste mirror fills from the live protocol, not just
/// from chrome's own writes.
#[test]
fn clipboard_crosses_from_a_client_to_in_process_chrome() {
    let compositor = Compositor::start();
    let payload = "copied inside a sandboxed client";

    let mut client = TestClient::new(compositor.connect(), "org.qos.test.chrome-source");
    client.assert_is_qos_compositor();
    client.map_window(&compositor);
    client.set_clipboard(payload);

    // The compositor asks the client for the bytes asynchronously, so the
    // client has to keep being dispatched while the mirror fills.
    let started = Instant::now();
    let mut seen = None;
    while started.elapsed() < DEADLINE {
        client.pump();
        seen = compositor.chrome_paste(false);
        if seen.as_deref() == Some(payload) {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        seen.as_deref(),
        Some(payload),
        "in-process chrome never saw the client's clipboard"
    );
    assert!(
        client.state.send_requests.iter().any(|m| m == TEXT_MIME),
        "the compositor never asked the client for its data: {:?}",
        client.state.send_requests
    );
}

/// Middle-click paste: the primary selection is a separate buffer from the
/// clipboard, and both must survive the trip between processes.
#[test]
fn primary_selection_crosses_between_processes_and_does_not_alias_the_clipboard() {
    let compositor = Compositor::start();
    let clipboard_payload = "this went on the clipboard";
    let primary_payload = "this was drag-selected";

    let mut source = TestClient::new(compositor.connect(), "org.qos.test.primary-source");
    source.assert_is_qos_compositor();
    assert!(
        source.has_global("zwp_primary_selection_device_manager_v1"),
        "the compositor must advertise the primary-selection manager; globals: {:?}",
        source.global_names()
    );
    source.map_window(&compositor);
    source.set_clipboard(clipboard_payload);
    source.set_primary(primary_payload);

    let mut sink = TestClient::new(compositor.connect(), "org.qos.test.primary-sink");
    sink.assert_is_qos_compositor();
    sink.map_window(&compositor);

    let got_primary = sink.read_primary(Some(&mut source));
    assert_eq!(
        got_primary, primary_payload,
        "middle-click paste did not receive the drag-selected text"
    );

    let got_clipboard = sink.read_clipboard(Some(&mut source));
    assert_eq!(
        got_clipboard, clipboard_payload,
        "the clipboard was overwritten by the primary selection"
    );
}

/// The primary selection also has to reach in-process chrome, since the
/// terminal that consumes it on middle-click runs inside the compositor.
#[test]
fn primary_selection_crosses_from_a_client_to_in_process_chrome() {
    let compositor = Compositor::start();
    let payload = "primary selection from a client";

    let mut client = TestClient::new(compositor.connect(), "org.qos.test.primary-chrome");
    client.assert_is_qos_compositor();
    client.map_window(&compositor);
    client.set_primary(payload);

    let started = Instant::now();
    let mut seen = None;
    while started.elapsed() < DEADLINE {
        client.pump();
        seen = compositor.chrome_paste(true);
        if seen.as_deref() == Some(payload) {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        seen.as_deref(),
        Some(payload),
        "in-process chrome never saw the client's primary selection"
    );
}

/// In-process chrome's primary selection must reach clients too, and must not
/// disturb the clipboard.
#[test]
fn primary_selection_crosses_from_in_process_chrome_to_a_client() {
    let compositor = Compositor::start();

    let mut client = TestClient::new(compositor.connect(), "org.qos.test.primary-out");
    client.assert_is_qos_compositor();
    client.map_window(&compositor);

    compositor.chrome_copy("chrome clipboard", false);
    compositor.chrome_copy("chrome primary", true);

    assert_eq!(client.read_primary(None), "chrome primary");
    assert_eq!(client.read_clipboard(None), "chrome clipboard");
}

/// `wlr-data-control` is the clipboard-manager protocol: binding it reads
/// every selection on the seat, no focus or user interaction required.
/// Must be invisible on the public (sandbox-bind-mounted) socket and
/// present on the privileged one — asserted in both directions, since only
/// checking absence would also pass if the global never existed.
#[test]
fn data_control_is_privileged_socket_only() {
    let compositor = Compositor::start();

    let mut sandboxed = TestClient::new(compositor.connect(), "org.qos.test.sandboxed");
    sandboxed.assert_is_qos_compositor();
    for manager in [
        "zwlr_data_control_manager_v1",
        "ext_data_control_manager_v1",
    ] {
        assert!(
            !sandboxed.has_global(manager),
            "{manager} is visible to a client on the public socket, which is the \
             socket every sandboxed app gets. Any app could then read every \
             password, TOTP code and recovery phrase the user copies. Globals \
             seen: {:?}",
            sandboxed.global_names(),
        );
    }
    // The ordinary, focus-gated selection protocols must still be there, or
    // the assertion above would be satisfied by a compositor with no clipboard
    // at all.
    assert!(sandboxed.has_global("wl_data_device_manager"));
    assert!(sandboxed.has_global("zwp_primary_selection_device_manager_v1"));

    let mut privileged = TestClient::new(
        compositor.connect_privileged(),
        "org.qos.test.clipboard-manager",
    );
    privileged.assert_is_qos_compositor();
    for manager in [
        "zwlr_data_control_manager_v1",
        "ext_data_control_manager_v1",
    ] {
        assert!(
            privileged.has_global(manager),
            "{manager} is missing on the privileged socket, so no clipboard \
             manager can ever run. Globals seen: {:?}",
            privileged.global_names(),
        );
    }
}

/// Drag a piece of text out of one client and drop it on another. The
/// pointer is driven through the compositor's real input path (test-control
/// socket synthesises the same events the backend would), so `start_drag`
/// rides a genuine implicit grab, not an invented serial.
#[test]
fn drag_and_drop_carries_data_between_two_clients() {
    let compositor = Compositor::start();
    let payload = "dragged between two clients";

    // Both windows have to be on screen at once for a drag to cross between
    // them, and in a scrollable-strip desktop that means separate window
    // groups rather than two tabs of one.
    compositor.place_next_window("org.qos.test.dnd-source");
    let mut source = TestClient::new(compositor.connect(), "org.qos.test.dnd-source");
    source.assert_is_qos_compositor();
    source.map_window(&compositor);

    compositor.place_next_window("org.qos.test.dnd-target");
    let mut target = TestClient::new(compositor.connect(), "org.qos.test.dnd-target");
    target.assert_is_qos_compositor();
    target.map_window(&compositor);

    // Click the source once before measuring anything — clicking raises its
    // window group, which scrolls the strip, so a position measured before
    // the click is stale afterward.
    let settle_point = source.pointer_target(&compositor);
    compositor.pointer_move(settle_point.0, settle_point.1);
    compositor.pointer_button(true);
    compositor.pointer_button(false);

    let target_point = target.pointer_target(&compositor);
    let source_point = source.pointer_target(&compositor);

    // Press inside the source tile for a real implicit grab; the serial must
    // come from *this* press since smithay only honours `start_drag` against
    // the serial of the currently held grab.
    source.state.pointer_button_serial = None;
    compositor.pointer_move(source_point.0, source_point.1);
    compositor.pointer_button(true);
    source.wait_until(
        |s| s.pointer_button_serial.is_some(),
        "the source client never received a pointer press, so it has no implicit \
         grab to start a drag from",
    );

    let mgr = source
        .state
        .data_manager
        .clone()
        .expect("wl_data_device_manager");
    let device = source.data_device.clone().expect("data device");
    let drag_source = mgr.create_data_source(&source.qh, payload.to_string());
    drag_source.offer(TEXT_MIME.to_string());
    drag_source.set_actions(DndAction::Copy);
    let grab_serial = source.state.pointer_button_serial.expect("grab serial");
    device.start_drag(
        Some(&drag_source),
        source.surface.as_ref().expect("source surface"),
        None,
        grab_serial,
    );
    source.roundtrip();

    // Move over the target tile and release.
    compositor.pointer_move(target_point.0, target_point.1);
    {
        let started = Instant::now();
        while started.elapsed() < DEADLINE {
            if target.state.dnd_entered && target.state.dnd_offer.is_some() {
                break;
            }
            target.pump();
            source.pump();
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            target.state.dnd_entered && target.state.dnd_offer.is_some(),
            "the drag never entered the target client's surface.\nCompositor log:\n{}",
            compositor.recent_log(),
        );
    }
    compositor.pointer_button(false);
    target.wait_until(|s| s.dnd_dropped, "the target client never saw the drop");

    let received = target.read_dnd(&mut source);
    assert_eq!(
        received, payload,
        "the dropped data did not match what the source offered"
    );
}

impl Compositor {
    /// Open the next window of `app_id` in its own group, so two client windows
    /// can be on screen at once.
    fn place_next_window(&self, app_id: &str) {
        let resp = self.control(&serde_json::json!({
            "op": "place_next_window", "app_id": app_id,
        }));
        assert!(
            resp.get("ok").is_some(),
            "place_next_window failed: {resp:?}"
        );
    }

    fn pointer_move(&self, x: f64, y: f64) {
        let resp = self.control(&serde_json::json!({"op": "pointer_move", "x": x, "y": y}));
        assert!(resp.get("ok").is_some(), "pointer_move failed: {resp:?}");
    }

    fn pointer_button(&self, pressed: bool) {
        let resp = self.control(&serde_json::json!({"op": "pointer_button", "pressed": pressed}));
        assert!(resp.get("ok").is_some(), "pointer_button failed: {resp:?}");
    }

    /// The on-screen rect `[x, y, w, h]` of a named chrome target this frame.
    fn named_rect(&self, name: &str) -> Option<[f64; 4]> {
        let resp = self.control(&serde_json::json!({"op": "list_targets"}));
        let targets = resp.get("targets")?.as_array()?;
        targets.iter().find_map(|t| {
            (t.get("name")?.as_str()? == name).then(|| {
                let r = t.get("rect")?.as_array()?;
                Some([
                    r[0].as_f64()?,
                    r[1].as_f64()?,
                    r[2].as_f64()?,
                    r[3].as_f64()?,
                ])
            })?
        })
    }

    fn named_center(&self, name: &str) -> (f64, f64) {
        let started = Instant::now();
        while started.elapsed() < DEADLINE {
            if let Some([x, y, w, h]) = self.named_rect(name)
                && w > 0.0
                && h > 0.0
            {
                return (x + w / 2.0, y + h / 2.0);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!(
            "chrome never laid out a target named {name:?} (waited {DEADLINE:?}).\n{}",
            self.debug_state()
        );
    }

    fn target_names(&self) -> Vec<String> {
        let resp = self.control(&serde_json::json!({"op": "list_targets"}));
        resp.get("targets")
            .and_then(|t| t.as_array())
            .map(|ts| {
                ts.iter()
                    .filter_map(|t| t.get("name")?.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn snapshot(&self, path: &Path) {
        let resp = self.control(&serde_json::json!({"op": "snapshot", "path": path}));
        assert!(resp.get("ok").is_some(), "snapshot failed: {resp:?}");
    }

    fn click_at(&self, x: f64, y: f64) {
        self.pointer_move(x, y);
        self.pointer_button(true);
        self.pointer_button(false);
    }
}

/// A 1x1 transparent PNG — the smallest real image a drop can carry.
const TINY_PNG: [u8; 67] = [
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
    0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE,
    0x42, 0x60, 0x82,
];

fn files_tab_is_active(compositor: &Compositor) -> bool {
    let dump = compositor.control(&serde_json::json!({"op": "state_dump"}));
    let mut active = false;
    walk_json(&dump, &mut |v| {
        let (Some(active_tab), Some(tabs)) = (
            v.get("active_tab"),
            v.get("tabs").and_then(|t| t.as_array()),
        ) else {
            return;
        };
        if tabs.iter().any(|t| {
            t.get("id") == Some(active_tab)
                && t.get("title").and_then(|x| x.as_str()) == Some("Files")
        }) {
            active = true;
        }
    });
    active
}

/// Narrow the demo group with the real seam-drag gesture so a client stays
/// visible while the Files tab is focused (the demo group is laid out at
/// full output width). Split out of the drag-and-drop tests below to keep
/// each one short.
fn narrow_demo_group_for_files(compositor: &Compositor) {
    const NARROW_BY: f64 = 1100.0;
    let seam = compositor.named_center("group_resize_handle[0]");
    compositor.pointer_move(seam.0, seam.1);
    compositor.pointer_button(true);
    for i in 1..=10 {
        compositor.pointer_move(seam.0 - NARROW_BY * (f64::from(i) / 10.0), seam.1);
        std::thread::sleep(Duration::from_millis(30));
    }
    compositor.pointer_button(false);
    std::thread::sleep(Duration::from_millis(800));
    compositor.snapshot(Path::new("/tmp/qos-dnd-e2e-after-resize.png"));
    assert!(
        files_tab_is_active(compositor),
        "the demo group must still show Files after the resize.\n{}",
        compositor.debug_state()
    );
}

/// The row the file manager laid out for `photo.png`, aimed at its
/// on-screen part since the tile may extend past the left edge. Split out
/// of the drag-and-drop tests below to keep each one short.
fn locate_file_row(compositor: &Compositor) -> (f64, f64) {
    compositor.snapshot(Path::new("/tmp/qos-dnd-e2e-before-drag.png"));
    let [x, y, w, h] = compositor
        .named_rect("file_row:photo.png")
        .unwrap_or_else(|| {
            panic!(
                "the file manager laid out no row for photo.png; targets: {:?}\n{}",
                compositor.target_names(),
                compositor.debug_state()
            )
        });
    let x0 = x.max(0.0);
    let x1 = (x + w).min(f64::from(compositor.size.0));
    assert!(
        x1 > x0 + 20.0,
        "the photo.png row is not on screen: [{x}, {y}, {w}, {h}]"
    );
    (f64::midpoint(x0, x1), y + h / 2.0)
}

/// Press on the file row and carry it over to the client in small steps —
/// crossing the file manager's own drag threshold before the pointer
/// leaves its tile, so the compositor sees a drag to promote — then wait
/// for the client to see `enter` with an offer. Split out of the
/// drag-and-drop tests below to keep each one short.
fn drag_file_row_onto_client(
    compositor: &Compositor,
    target: &mut TestClient,
    row: (f64, f64),
    target_point: (f64, f64),
) {
    const STEPS: u32 = 12;
    compositor.pointer_move(row.0, row.1);
    compositor.pointer_button(true);
    for i in 1..=STEPS {
        let t = f64::from(i) / f64::from(STEPS);
        compositor.pointer_move(
            row.0 + (target_point.0 - row.0) * t,
            row.1 + (target_point.1 - row.1) * t,
        );
        target.pump();
        std::thread::sleep(Duration::from_millis(30));
    }
    compositor.snapshot(Path::new("/tmp/qos-dnd-e2e-mid-drag.png"));
    let started = Instant::now();
    while started.elapsed() < DEADLINE {
        if target.state.dnd_entered && target.state.dnd_offer.is_some() {
            break;
        }
        target.pump();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        target.state.dnd_entered && target.state.dnd_offer.is_some(),
        "the chrome drag never entered the client's surface (drag from {row:?} to {target_point:?}; \
         file_view={:?} row_rect={:?}).\nState:\n{}\nCompositor log:\n{}",
        compositor.named_rect("file_view"),
        compositor.named_rect("file_row:photo.png"),
        compositor.control(&serde_json::json!({"op": "state_dump"}))["state"]["chrome"]
            .get("files_drag")
            .map(|v| format!(
                "files_drag={v} handed_off={}",
                compositor.control(&serde_json::json!({"op": "state_dump"}))["state"]["chrome"]
                    ["files_drag_handed_off"]
            ))
            .unwrap_or_default(),
        compositor.recent_log(),
    );
}

/// Release the pointer and wait for the client to see the drop. Split out
/// of the drag-and-drop tests below to keep each one short.
fn release_and_wait_for_drop(compositor: &Compositor, target: &mut TestClient) {
    compositor.pointer_button(false);
    let started = Instant::now();
    while started.elapsed() < DEADLINE && !target.state.dnd_dropped {
        target.pump();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        target.state.dnd_dropped,
        "the client never saw the drop.\nCompositor log:\n{}",
        compositor.recent_log()
    );
}

/// Drag a file out of the hosted file manager (a widget inside the
/// compositor, not a client) and drop it on a client's window. Runs
/// through the real input path; the compositor promotes the in-process
/// drag to a server-side `wl_data_device` drag the moment the pointer
/// reaches the client, which then gets enter, an offer carrying both the
/// path list and the file's own bytes, and the drop.
#[test]
fn a_file_dragged_out_of_the_hosted_file_manager_drops_onto_a_client() {
    let home = PathBuf::from(format!(
        "/run/user/{}/qos-dnd-home-{}-{}",
        unsafe { libc::getuid() },
        std::process::id(),
        next_id(),
    ));
    std::fs::create_dir_all(&home).expect("create the fixture home");
    let photo = home.join("photo.png");
    std::fs::write(&photo, TINY_PNG).expect("write the fixture image");

    let compositor = Compositor::start_with(Some(&home), (2560, 900));

    compositor.place_next_window("org.qos.test.dnd-target");
    let mut target = TestClient::new(compositor.connect(), "org.qos.test.dnd-target");
    target.assert_is_qos_compositor();
    target.map_window(&compositor);

    // Let the strip finish making room for the new group before reading
    // where the pills are; a pill read mid-animation is clicked where it
    // used to be.
    std::thread::sleep(Duration::from_secs(1));
    let files_pill = compositor.named_center("tab:Files");
    compositor.click_at(files_pill.0, files_pill.1);
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        files_tab_is_active(&compositor),
        "clicking the Files pill at {files_pill:?} did not select the Files tab.\n{}",
        compositor.debug_state()
    );

    // The demo group is laid out at full output width, so with Files focused
    // the client is off-screen; narrow the demo group with the real gesture
    // (drag the seam handle) so the client stays visible while Files is
    // focused.
    narrow_demo_group_for_files(&compositor);
    let target_point = target.pointer_target(&compositor);
    // The row the file manager laid out for the fixture file — aimed at
    // its on-screen part, since the tile may extend past the left edge.
    let row = locate_file_row(&compositor);

    drag_file_row_onto_client(&compositor, &mut target, row, target_point);
    // The client's accept-mime/action requests from `enter` must reach the
    // compositor before release, or the drop is refused as unaccepted —
    // matching the protocol and what a real client's event loop avoids by
    // flushing as it goes.
    target.roundtrip();
    assert!(
        target
            .state
            .dnd_offer_mimes
            .iter()
            .any(|m| m == "text/uri-list"),
        "the offer must carry the path list; offered {:?}",
        target.state.dnd_offer_mimes
    );
    assert!(
        target
            .state
            .dnd_offer_mimes
            .iter()
            .any(|m| m == "image/png"),
        "a single image is also offered as its own bytes; offered {:?}",
        target.state.dnd_offer_mimes
    );

    release_and_wait_for_drop(&compositor, &mut target);

    let uris = target.read_dnd_bytes("text/uri-list", None);
    assert_eq!(
        String::from_utf8_lossy(&uris),
        format!("file://{}\r\n", photo.display()),
        "the URI list names the dragged file"
    );
    let bytes = target.read_dnd_bytes("image/png", None);
    assert_eq!(
        bytes, TINY_PNG,
        "the image bytes arrive intact through the pipe"
    );
    target.state.dnd_offer.as_ref().expect("offer").finish();
    target.roundtrip();

    assert!(
        photo.exists(),
        "a drop onto a client is a copy; the file manager must not have moved the file"
    );
    let _ = std::fs::remove_dir_all(&home);
}

/// Spawn the real `word-processor` binary against the compositor under test,
/// draining its stderr onto a background thread. Split out of
/// [`a_file_dragged_out_of_the_hosted_file_manager_lands_in_the_word_processor`]
/// to keep it short.
fn spawn_word_processor(
    compositor: &Compositor,
    word_processor: &Path,
    home: &Path,
) -> (Child, Arc<Mutex<Vec<String>>>) {
    let socket_name = compositor
        .wayland_socket
        .file_name()
        .expect("socket name")
        .to_os_string();
    let mut app = Command::new(word_processor)
        .env("WAYLAND_DISPLAY", &socket_name)
        // What the launcher exports for a packaged app; `ui` announces it
        // as the Wayland app id, which is how the placement above is
        // matched to this window.
        .env("QOS_PACKAGE_ID", "dev.qos.word-processor")
        .env("XDG_RUNTIME_DIR", &compositor.runtime_dir)
        .env("XDG_CONFIG_HOME", compositor.runtime_dir.join("app-config"))
        .env("XDG_DATA_HOME", compositor.runtime_dir.join("app-data"))
        .env("XDG_CACHE_HOME", compositor.runtime_dir.join("app-cache"))
        .env("HOME", home)
        .env_remove("DISPLAY")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the word processor");
    let app_log = Arc::new(Mutex::new(Vec::<String>::new()));
    let stderr = app.stderr.take().expect("app stderr");
    let sink = Arc::clone(&app_log);
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if let Ok(mut l) = sink.lock() {
                l.push(line);
            }
        }
    });
    (app, app_log)
}

/// Wait for the word processor's toplevel to be adopted into the tree — the
/// chrome names every foreign tile it lays out. Split out of
/// [`a_file_dragged_out_of_the_hosted_file_manager_lands_in_the_word_processor`]
/// to keep it short.
fn wait_for_word_processor_mapped(compositor: &Compositor, app_log: &Arc<Mutex<Vec<String>>>) {
    let started = Instant::now();
    loop {
        let adopted = compositor
            .target_names()
            .iter()
            .any(|n| n.starts_with("foreign-surface-"));
        if adopted {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(90),
            "the word processor never mapped a window.\nApp log:\n{}\nCompositor log:\n{}",
            app_log.lock().map(|l| l.join("\n")).unwrap_or_default(),
            compositor.recent_log()
        );
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// The same drag, dropped on the real word processor. A live lane: starts
/// `target/debug/word-processor` as an unsandboxed client (needs a GPU to
/// render) and asserts its own report that it inserted the image.
/// Self-skips unless `QOS_LIVE_APP_DND=1` (`docs/testing-live.md`), so
/// hermetic CI stays green.
#[test]
fn a_file_dragged_out_of_the_hosted_file_manager_lands_in_the_word_processor() {
    const STEPS: u32 = 16;
    if std::env::var_os("QOS_LIVE_APP_DND").is_none() {
        eprintln!("skipping: set QOS_LIVE_APP_DND=1 to run the word-processor drop lane");
        return;
    }
    let word_processor =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../target/debug/word-processor");
    assert!(
        word_processor.exists(),
        "build it first: cargo build -p word-processor ({})",
        word_processor.display()
    );

    let home = PathBuf::from(format!(
        "/run/user/{}/qos-dnd-wp-{}-{}",
        unsafe { libc::getuid() },
        std::process::id(),
        next_id(),
    ));
    std::fs::create_dir_all(&home).expect("create the fixture home");
    let photo = home.join("photo.png");
    std::fs::write(&photo, TINY_PNG).expect("write the fixture image");

    let compositor = Compositor::start_with_home(Some(&home));
    compositor.place_next_window("dev.qos.word-processor");

    let (mut app, app_log) = spawn_word_processor(&compositor, &word_processor, &home);
    wait_for_word_processor_mapped(&compositor, &app_log);
    // Let it paint its first frame, so the tile has a real size.
    std::thread::sleep(Duration::from_secs(3));

    // Same recipe as the client test: select Files, narrow the demo group
    // by its seam handle so the app stays on screen while Files is focused.
    let files_pill = compositor.named_center("tab:Files");
    compositor.click_at(files_pill.0, files_pill.1);
    std::thread::sleep(Duration::from_millis(500));
    let seam = compositor.named_center("group_resize_handle[0]");
    compositor.pointer_move(seam.0, seam.1);
    compositor.pointer_button(true);
    for i in 1..=10 {
        compositor.pointer_move(seam.0 - 1100.0 * (f64::from(i) / 10.0), seam.1);
        std::thread::sleep(Duration::from_millis(30));
    }
    compositor.pointer_button(false);
    std::thread::sleep(Duration::from_millis(800));
    assert!(
        files_tab_is_active(&compositor),
        "the demo group must still show Files after the resize.\n{}",
        compositor.debug_state()
    );
    let row = compositor.named_center("file_row:photo.png");

    let tile_name = compositor
        .target_names()
        .into_iter()
        .find(|n| n.starts_with("foreign-surface-"))
        .expect("the word processor's tile is laid out");
    let [tx, ty, tw, th] = compositor.named_rect(&tile_name).expect("tile rect");
    // Drop into the page body, below the app's toolbar.
    let (drop_x, drop_y) = (tx + tw / 2.0, ty + th * 0.6);

    compositor.pointer_move(row.0, row.1);
    compositor.pointer_button(true);
    for i in 1..=STEPS {
        let t = f64::from(i) / f64::from(STEPS);
        compositor.pointer_move(row.0 + (drop_x - row.0) * t, row.1 + (drop_y - row.1) * t);
        std::thread::sleep(Duration::from_millis(40));
    }
    std::thread::sleep(Duration::from_millis(300));
    compositor.pointer_button(false);

    let started = Instant::now();
    let inserted = loop {
        let hit = app_log.lock().is_ok_and(|l| {
            l.iter()
                .any(|line| line.contains("word-processor: inserted image from"))
        });
        if hit {
            break true;
        }
        if started.elapsed() > DEADLINE {
            break false;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let _ = app.kill();
    let _ = app.wait();
    assert!(
        inserted,
        "the word processor never reported inserting the dropped image.\nApp log:\n{}\nCompositor log:\n{}",
        app_log.lock().map(|l| l.join("\n")).unwrap_or_default(),
        compositor.recent_log()
    );
    let _ = std::fs::remove_dir_all(&home);
}
