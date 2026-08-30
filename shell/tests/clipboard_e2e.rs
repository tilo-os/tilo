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

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read};
use std::os::unix::io::{FromRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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

    /// [`Self::start_with_home`] on a screen of `size`. A wider screen keeps
    /// two window groups on screen at once when one of them is the demo
    /// group, whose tabs are laid out at their normal width.
    fn start_with(home: Option<&Path>, size: (i32, i32)) -> Self {
        // SUN_LEN caps a unix socket path (~108 bytes), so the runtime dir must be
        // short — `/run/user/$UID` also keeps a stray socket out of the repo.
        let runtime_dir = PathBuf::from(format!(
            "/run/user/{}/qos-clip-{}-{}",
            unsafe { libc::getuid() },
            std::process::id(),
            next_id(),
        ));
        std::fs::create_dir_all(&runtime_dir).expect("create private runtime dir");
        let control_socket = runtime_dir.join("ctl");

        let mut command = Command::new(env!("CARGO_BIN_EXE_shell"));
        if let Some(home) = home {
            command.env("HOME", home);
        }
        let mut child = command
            .args(["--mode", "native", "--backend", "headless"])
            .env("XDG_RUNTIME_DIR", &runtime_dir)
            // Point every XDG dir at the throwaway runtime dir so the test neither
            // reads nor writes the developer's real desktop, and each run starts from
            // the same known tree.
            .env("XDG_CONFIG_HOME", runtime_dir.join("config"))
            .env("XDG_DATA_HOME", runtime_dir.join("data"))
            .env("XDG_STATE_HOME", runtime_dir.join("state"))
            .env("XDG_CACHE_HOME", runtime_dir.join("cache"))
            .env("QOS_TEST_CONTROL", &control_socket)
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
            .expect("spawn the compositor under test");

        let log = Arc::new(Mutex::new(Vec::<String>::new()));

        // Both pipes must be drained continuously — a full 64 KiB pipe blocks the
        // compositor mid-write, surfacing as an unrelated timeout much later.
        // `tracing` goes to stdout, the compositor's `eprintln!`s to stderr;
        // missing either can hide the one line that explains a failure.
        let stdout = child.stdout.take().expect("compositor stdout");
        let stdout_log = Arc::clone(&log);
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Ok(mut l) = stdout_log.lock() {
                    l.push(line);
                }
            }
        });

        let (tx, rx) = mpsc::channel::<(String, String)>();
        let stderr = child.stderr.take().expect("compositor stderr");
        let log_writer = Arc::clone(&log);
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
            "compositor announced {:?} but no socket exists there",
            this.wayland_socket
        );
        assert!(
            this.privileged_socket.exists(),
            "compositor announced a privileged socket at {:?} but none exists",
            this.privileged_socket
        );
        this
    }

    fn control(&self, request: serde_json::Value) -> serde_json::Value {
        use std::io::Write;
        let mut stream =
            UnixStream::connect(&self.control_socket).expect("connect to the test-control socket");
        stream
            .set_read_timeout(Some(DEADLINE))
            .expect("set control read timeout");
        let mut line = serde_json::to_vec(&request).expect("encode control request");
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
        let resp = self.control(serde_json::json!({
            "op": "clipboard_copy", "text": text, "primary": primary,
        }));
        assert!(
            resp.get("ok").is_some(),
            "chrome copy was refused: {resp:?}"
        );
    }

    fn chrome_paste(&self, primary: bool) -> Option<String> {
        let resp = self.control(serde_json::json!({
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
        let resp = self.control(serde_json::json!({"op": "state_dump"}));
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
                groups += value
                    .get("window_groups")
                    .and_then(|g| g.as_array())
                    .map_or(0, std::vec::Vec::len);
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
        self.connect_to(&self.wayland_socket)
    }

    fn connect_privileged(&self) -> Connection {
        self.connect_to(&self.privileged_socket)
    }

    fn connect_to(&self, path: &Path) -> Connection {
        let stream = UnixStream::connect(path)
            .unwrap_or_else(|e| panic!("connect to the compositor socket {path:?}: {e}"));
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
                x += STEP;
            }
            y += STEP;
        }
        None
    }

    /// Attach an opaque `wl_shm` buffer and commit, so the compositor has real
    /// content to size and composite the tile from.
    fn attach_buffer(&mut self, width: i32, height: i32) {
        let shm = self
            .state
            .shm
            .clone()
            .expect("compositor advertises wl_shm");
        let stride = width * 4;
        let len = (stride * height) as usize;

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

        use std::os::unix::io::AsFd;
        let pool = shm.create_pool(file.as_fd(), len as i32, &self.qh, ());
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
            wl_keyboard::Event::Key { serial, .. }
            | wl_keyboard::Event::Modifiers { serial, .. } => {
                state.last_serial = serial;
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
            wl_data_device::Event::Leave => {}
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
        let resp = self.control(serde_json::json!({
            "op": "place_next_window", "app_id": app_id,
        }));
        assert!(
            resp.get("ok").is_some(),
            "place_next_window failed: {resp:?}"
        );
    }

    fn pointer_move(&self, x: f64, y: f64) {
        let resp = self.control(serde_json::json!({"op": "pointer_move", "x": x, "y": y}));
        assert!(resp.get("ok").is_some(), "pointer_move failed: {resp:?}");
    }

    fn pointer_button(&self, pressed: bool) {
        let resp = self.control(serde_json::json!({"op": "pointer_button", "pressed": pressed}));
        assert!(resp.get("ok").is_some(), "pointer_button failed: {resp:?}");
    }

    /// The on-screen rect `[x, y, w, h]` of a named chrome target this frame.
    fn named_rect(&self, name: &str) -> Option<[f64; 4]> {
        let resp = self.control(serde_json::json!({"op": "list_targets"}));
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
        let resp = self.control(serde_json::json!({"op": "list_targets"}));
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
        let resp = self.control(serde_json::json!({"op": "snapshot", "path": path}));
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
    let dump = compositor.control(serde_json::json!({"op": "state_dump"}));
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
    let seam = compositor.named_center("group_resize_handle[0]");
    compositor.pointer_move(seam.0, seam.1);
    compositor.pointer_button(true);
    const NARROW_BY: f64 = 1100.0;
    for i in 1..=10 {
        compositor.pointer_move(seam.0 - NARROW_BY * (f64::from(i) / 10.0), seam.1);
        std::thread::sleep(Duration::from_millis(30));
    }
    compositor.pointer_button(false);
    std::thread::sleep(Duration::from_millis(800));
    compositor.snapshot(Path::new("/tmp/qos-dnd-e2e-after-resize.png"));
    assert!(
        files_tab_is_active(&compositor),
        "the demo group must still show Files after the resize.\n{}",
        compositor.debug_state()
    );
    let target_point = target.pointer_target(&compositor);
    // The row the file manager laid out for the fixture file — aimed at
    // its on-screen part, since the tile may extend past the left edge.
    let row = {
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
    };

    // Press on the row and carry it over to the client in small steps, so
    // the file manager's own drag threshold is crossed before the pointer
    // leaves its tile and the compositor sees a drag to promote.
    compositor.pointer_move(row.0, row.1);
    compositor.pointer_button(true);
    const STEPS: usize = 12;
    for i in 1..=STEPS {
        let t = i as f64 / STEPS as f64;
        compositor.pointer_move(
            row.0 + (target_point.0 - row.0) * t,
            row.1 + (target_point.1 - row.1) * t,
        );
        target.pump();
        std::thread::sleep(Duration::from_millis(30));
    }
    compositor.snapshot(Path::new("/tmp/qos-dnd-e2e-mid-drag.png"));
    {
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
            compositor.control(serde_json::json!({"op": "state_dump"}))["state"]["chrome"]
                .get("files_drag")
                .map(|v| format!(
                    "files_drag={v} handed_off={}",
                    compositor.control(serde_json::json!({"op": "state_dump"}))["state"]["chrome"]
                        ["files_drag_handed_off"]
                ))
                .unwrap_or_default(),
            compositor.recent_log(),
        );
    }
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

    compositor.pointer_button(false);
    {
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

/// The same drag, dropped on the real word processor. A live lane: starts
/// `target/debug/word-processor` as an unsandboxed client (needs a GPU to
/// render) and asserts its own report that it inserted the image.
/// Self-skips unless `QOS_LIVE_APP_DND=1` (`docs/testing-live.md`), so
/// hermetic CI stays green.
#[test]
fn a_file_dragged_out_of_the_hosted_file_manager_lands_in_the_word_processor() {
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

    let socket_name = compositor
        .wayland_socket
        .file_name()
        .expect("socket name")
        .to_os_string();
    let mut app = Command::new(&word_processor)
        .env("WAYLAND_DISPLAY", &socket_name)
        // What the launcher exports for a packaged app; `ui` announces it
        // as the Wayland app id, which is how the placement above is
        // matched to this window.
        .env("QOS_PACKAGE_ID", "dev.qos.word-processor")
        .env("XDG_RUNTIME_DIR", &compositor.runtime_dir)
        .env("XDG_CONFIG_HOME", compositor.runtime_dir.join("app-config"))
        .env("XDG_DATA_HOME", compositor.runtime_dir.join("app-data"))
        .env("XDG_CACHE_HOME", compositor.runtime_dir.join("app-cache"))
        .env("HOME", &home)
        .env_remove("DISPLAY")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the word processor");
    let app_log = Arc::new(Mutex::new(Vec::<String>::new()));
    {
        let stderr = app.stderr.take().expect("app stderr");
        let sink = Arc::clone(&app_log);
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if let Ok(mut l) = sink.lock() {
                    l.push(line);
                }
            }
        });
    }

    // Wait for the app's toplevel to be adopted into the tree: the chrome
    // names every foreign tile it lays out.
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
    const STEPS: usize = 16;
    for i in 1..=STEPS {
        let t = i as f64 / STEPS as f64;
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
