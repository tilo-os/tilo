//! End-to-end clipboard, primary selection, and drag-and-drop against the
//! Tilo compositor with Wayland clients. Each client connects to a
//! `UnixStream` inside the
//! compositor's private runtime dir, never `Connection::connect_to_env()` or
//! `WAYLAND_DISPLAY`.
//!
//! Why: under a nested GNOME session `arboard` reaches X11 through Xwayland
//! and *appears* to work, so a clipboard test could pass while the Tilo
//! compositor had no selection protocol at all.
//! [`TestClient::assert_is_tilo_compositor`] checks the seat name the peer
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
use std::io::{BufRead, BufReader, Cursor, Read};
use std::os::unix::io::{FromRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use getrandom::{SysRng, rand_core::UnwrapErr};
use smithay::input::keyboard::xkb::keysyms;
use threshold_identity::ThresholdIdentity;
use ui::clipboard::{ClipboardBackend, ClipboardTarget};
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
    wl_touch::{self, WlTouch},
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
    xdg_wm_base::XdgWmBase,
};

/// The mime type Tilo offers text selections as. Must match
/// `shell::compositor::selection::TEXT_MIME` — asserted below so a change on
/// one side cannot drift silently past the other.
const TEXT_MIME: &str = "text/plain;charset=utf-8";

/// The seat name the Tilo compositor registers. Used as the proof that a client
/// is talking to *this* compositor and not a session one.
const TILO_SEAT_NAME: &str = "tilo-desktop";

/// Match the native session's launcher-less record scope explicitly, so
/// inherited package/workspace variables cannot redirect fixture documents.
const FIXTURE_RECORD_SCOPE: &str = "00000000-0000-0000-0000-000000000000";

/// How long any single wait-for-condition may take, startup included: the
/// workspace-wide hang bound. Every wait here returns as soon as its condition
/// holds, and a compositor that exits or closes its stderr fails the wait at
/// once with its log, so this only decides when a stuck wait is called hung.
fn deadline() -> Duration {
    tilo_test_env::hang_bound()
}

/// Spacing of the grid [`TestClient::pointer_target`] probes for a client.
const POINTER_GRID_STEP: i32 = 40;

/// How often a layout is sampled while waiting for it to stop moving: about
/// one frame, so two equal samples straddle a repaint.
const LAYOUT_SAMPLE: Duration = Duration::from_millis(17);

/// Consecutive equal samples that count as a settled layout.
const SETTLED_SAMPLES: usize = 4;

/// Block until one of `clients` receives an event, or `extra` becomes
/// readable, for at most `timeout`, then dispatch whatever arrived.
///
/// Each client's pending requests are flushed first, so a wait never parks
/// on an answer to a request that was never sent. Each park is capped
/// because a connection another thread also reads (the app clipboard
/// worker's) can take this queue's events off the socket.
fn dispatch_any(
    clients: &mut [&mut TestClient],
    extra: Option<std::os::unix::io::BorrowedFd<'_>>,
    timeout: Duration,
) {
    use std::os::unix::io::AsRawFd;
    const PARK: Duration = Duration::from_millis(50);
    let mut guards = Vec::with_capacity(clients.len());
    let mut queued = false;
    for client in clients.iter_mut() {
        client
            .queue
            .dispatch_pending(&mut client.state)
            .expect("dispatch queued client events");
        client.queue.flush().expect("flush client requests");
        let guard = client.queue.prepare_read();
        queued |= guard.is_none();
        guards.push(guard);
    }
    if !queued {
        let mut fds: Vec<libc::pollfd> = guards
            .iter()
            .flatten()
            .map(|guard| guard.connection_fd().as_raw_fd())
            .chain(extra.map(|fd| fd.as_raw_fd()))
            .map(|fd| libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            })
            .collect();
        let park = i32::try_from(timeout.min(PARK).as_millis()).unwrap_or(i32::MAX);
        let nfds = libc::nfds_t::try_from(fds.len()).expect("a handful of descriptors");
        // SAFETY: `fds` is a live array of `nfds` initialised pollfds whose
        // descriptors outlive the call.
        let rc = unsafe { libc::poll(fds.as_mut_ptr(), nfds, park) };
        let err = std::io::Error::last_os_error();
        assert!(
            rc >= 0 || err.kind() == std::io::ErrorKind::Interrupted,
            "poll the client connections: {err}"
        );
        let mut readable = fds.iter().map(|fd| fd.revents != 0);
        for guard in guards.iter_mut().filter_map(Option::take) {
            if readable.next() == Some(true) {
                match guard.read() {
                    Ok(_) => {}
                    Err(wayland_client::backend::WaylandError::Io(e))
                        if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(e) => panic!("read client events: {e}"),
                }
            }
        }
    }
    drop(guards);
    for client in clients.iter_mut() {
        client
            .queue
            .dispatch_pending(&mut client.state)
            .expect("dispatch client events");
    }
}

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
    /// The durable tree used by this fixture.  Keeping it explicit lets a
    /// lifecycle test restart the actual compositor process against the same
    /// isolated desktop, rather than merely deserialising a second model.
    state_path: PathBuf,
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
    fn start_with_input_settings(input: &serde_json::Value) -> Self {
        Self::start_with_fixture_settings((1280, 800), serde_json::json!({ "input": input }))
    }

    /// [`Self::start_with_home`] on a screen of `size`. A wider screen keeps
    /// two window groups on screen at once when one of them is the demo
    /// group, whose tabs are laid out at their normal width.
    fn start_with(home: Option<&Path>, size: (i32, i32)) -> Self {
        Self::start_with_config(home, size, None, false)
    }

    /// Host-desktop mode is intentionally a separate fixture mode: it is the
    /// only supported configuration in which Tilo starts the rootless Xwayland
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
            "/run/user/{}/tilo-clip-{}-{}",
            unsafe { libc::getuid() },
            std::process::id(),
            next_id(),
        ));
        std::fs::create_dir_all(&runtime_dir).expect("create private runtime dir");
        if let Some(settings) = settings {
            let settings_path = runtime_dir.join("config/tilo/settings.json");
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
        let mut fixture_tree = shell::shell::state::WorkspaceTree::bootstrap_demo();
        // An untouched bootstrap demo is deliberately recognised as legacy
        // sample data and migrated to a clean first-boot tree on load. This
        // suite requires the hosted Files tab, so mark the persisted
        // tree as test-owned instead of accidentally exercising that cleanup.
        fixture_tree.desktop.workspaces[0].name = "Clipboard test desktop".into();
        shell::shell::persist::save_tree_to(&fixture_tree, &state_path)
            .expect("seed the clipboard e2e desktop");
        Self::seed_tab_records(&runtime_dir, &fixture_tree);
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
        let found = Self::wait_until_serving(&rx, &mut child, &log);

        let this = Self {
            child,
            size,
            wayland_socket: runtime_dir.join(&found["public"]),
            privileged_socket: runtime_dir.join(&found["privileged"]),
            control_socket: PathBuf::from(&found["control"]),
            state_path,
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

    /// This fixture authored the saved tabs, so it also creates their
    /// documents before native startup performs existing-only admission.
    /// Restart deliberately reuses these files without creating them again.
    fn seed_tab_records(runtime_dir: &Path, tree: &shell::shell::state::WorkspaceTree) {
        let mut owners = ui::record::RecordOwners::new(runtime_dir.join("data"));
        let records = owners
            .open(shell::shell::SHELL_RECORDS_PACKAGE, FIXTURE_RECORD_SCOPE)
            .expect("open the fixture's explicit Shell record namespace");
        for tab in tree
            .desktop
            .workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
        {
            shell::shell::tab_documents::create_scratch_tab_record(&records, tab.id)
                .expect("create the authored fixture tab's document");
            records
                .try_open_record(&shell::shell::tab_documents::scratch_tab_record(tab.id))
                .expect("the fixture tab must admit its existing document");
        }
        records
            .try_flush()
            .expect("persist every fixture tab document before starting the compositor");
        // Release the writers and canonical ownership gates before the child
        // opens these same records in its own session store.
        drop(records);
        drop(owners);
    }

    /// Kill and relaunch the native headless compositor on its existing
    /// isolated XDG tree.  This is deliberately a process restart: reading
    /// `state.json` in the test process would not prove native startup and its
    /// first-frame reconciliation keep a closed foreign surface absent.
    fn restart_headless(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();

        let identity_root = self.runtime_dir.join("identity-root");
        let mut child = Self::spawn_compositor(
            None,
            self.size,
            &self.runtime_dir,
            &self.control_socket,
            &self.state_path,
            &identity_root,
            false,
        );
        let rx = Self::drain_log_pipes(&mut child, &self.log);
        let found = Self::wait_until_serving(&rx, &mut child, &self.log);
        self.wayland_socket = self.runtime_dir.join(&found["public"]);
        self.privileged_socket = self.runtime_dir.join(&found["privileged"]);
        self.control_socket = PathBuf::from(&found["control"]);
        self.child = child;
    }

    /// Give this desktop fixture an already-completed identity ceremony.
    /// Clipboard tests exercise normal desktop clients; leaving the fixture
    /// identity empty correctly activates OOBE, which must withhold focus.
    fn seed_identity(runtime_dir: &Path) -> PathBuf {
        let identity_dir = runtime_dir.join("config/tilo/identity");
        let identity = ThresholdIdentity::bootstrap_single_device(&mut UnwrapErr(SysRng))
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
        tilo_test_env::require_isolated_desktop();
        let mut command = Command::new(env!("CARGO_BIN_EXE_shell"));
        if let Some(home) = home {
            command.env("HOME", home).env("TILO_USER_HOME", home);
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
            // Keep satellite and the Xwayland binary it execs in one runtime.
            // Mixing `/run/host/usr/bin` into a build container's PATH can
            // select a newer host Xwayland with a libc ABI the container does
            // not provide. That fails before satellite's private Wayland
            // server (and its xwayland-shell-v1 global) can even be exercised.
            // The opt-in workstation lane therefore uses the installed pair
            // from the environment in which this test binary is running.
        }
        command
            .env("XDG_RUNTIME_DIR", runtime_dir)
            // Point every XDG dir at the throwaway runtime dir so the test neither
            // reads nor writes the developer's desktop, and each run starts from
            // the same known tree.
            .env("XDG_CONFIG_HOME", runtime_dir.join("config"))
            .env("XDG_DATA_HOME", runtime_dir.join("data"))
            .env("XDG_STATE_HOME", runtime_dir.join("state"))
            .env("XDG_CACHE_HOME", runtime_dir.join("cache"))
            // Match the namespace provisioned once by seed_tab_records.
            .env("TILO_PACKAGE_ID", shell::shell::SHELL_RECORDS_PACKAGE)
            .env("TILO_WORKSPACE_ID", FIXTURE_RECORD_SCOPE)
            // A CI checkout commonly lives at `/workspace`. It is not a Tilo
            // collaborative mount, and must not win over the fixture home as
            // the hosted Files app's initial directory.
            .env("TILO_WORKSPACE_ROOTS", "")
            .env("TILO_STATE_PATH", state_path)
            .env("TILO_IDENTITY_ROOT", identity_root)
            .env("TILO_TEST_CONTROL", control_socket)
            .env("TILO_HEADLESS_SIZE", format!("{}x{}", size.0, size.1))
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
    /// [`Self::wait_until_serving`] reads the announced socket paths from.
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
                    ("tilo compositor listening on ", "public"),
                    ("tilo compositor privileged socket on ", "privileged"),
                    ("tilo compositor test_control listening on ", "control"),
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

    /// Block until the compositor is actually serving, not merely bound.
    ///
    /// The compositor announces each socket as it binds it, during startup
    /// and before its event loop first turns. Nothing on those sockets is
    /// answered until then, so taking the announcements as readiness charges
    /// the whole cold start to the first request a test makes. Readiness is
    /// therefore a handshake: all three sockets announced, then a
    /// `list_targets` request answered with at least one laid-out target,
    /// which only a completed frame produces. One [`deadline`],
    /// measured from spawn, bounds all of it; a compositor that dies fails
    /// the wait immediately.
    fn wait_until_serving(
        rx: &mpsc::Receiver<(String, String)>,
        child: &mut Child,
        log: &Arc<Mutex<Vec<String>>>,
    ) -> HashMap<String, String> {
        let started = Instant::now();
        let fail = |child: &mut Child, why: &str| -> ! {
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "compositor did not start: {why} (after {:?}). Compositor log:\n{}",
                started.elapsed(),
                log.lock().map(|l| l.join("\n")).unwrap_or_default()
            );
        };

        let mut found: HashMap<String, String> = HashMap::new();
        while found.len() < 3 {
            let remaining = deadline().saturating_sub(started.elapsed());
            match rx.recv_timeout(remaining) {
                Ok((key, value)) => {
                    found.insert(key, value);
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => fail(
                    child,
                    &format!("it closed stderr having announced only {found:?}"),
                ),
                Err(mpsc::RecvTimeoutError::Timeout) => fail(
                    child,
                    &format!("it announced only {found:?} within the hang bound"),
                ),
            }
        }

        let control = PathBuf::from(&found["control"]);
        loop {
            match Self::laid_out_targets(&control, child, started) {
                Ok(0) => std::thread::sleep(Duration::from_millis(20)),
                Ok(_) => break,
                Err(why) => fail(child, &why),
            }
        }
        eprintln!(
            "[clipboard-e2e] compositor serving after {:?}",
            started.elapsed()
        );
        found
    }

    /// One readiness probe: ask for the laid-out targets and wait for the
    /// answer while the compositor is alive and startup has time left.
    /// Returns how many targets the latest frame laid out.
    fn laid_out_targets(
        control: &Path,
        child: &mut Child,
        started: Instant,
    ) -> Result<usize, String> {
        use std::io::Write;
        const POLL: Duration = Duration::from_millis(100);
        let mut stream = UnixStream::connect(control)
            .map_err(|e| format!("connect to test control {}: {e}", control.display()))?;
        stream
            .set_read_timeout(Some(POLL))
            .map_err(|e| format!("set control read timeout: {e}"))?;
        stream
            .write_all(b"{\"op\":\"list_targets\"}\n")
            .map_err(|e| format!("write the readiness request: {e}"))?;
        // Short reads let the loop notice an exit. `read_until` keeps the
        // bytes that arrived before a timeout, so a response split across
        // reads is still reassembled.
        let mut reader = BufReader::new(stream);
        let mut response = Vec::new();
        loop {
            match reader.read_until(b'\n', &mut response) {
                Ok(_) if response.last() == Some(&b'\n') => break,
                Ok(_) => return Err("test control closed without answering".into()),
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock
                            | std::io::ErrorKind::TimedOut
                            | std::io::ErrorKind::Interrupted
                    ) => {}
                Err(e) => return Err(format!("read the readiness response: {e}")),
            }
            if let Ok(Some(status)) = child.try_wait() {
                return Err(format!("it exited with {status}"));
            }
            if started.elapsed() >= deadline() {
                return Err(
                    "its event loop did not answer test control within the hang bound".into(),
                );
            }
        }
        let value: serde_json::Value = serde_json::from_slice(&response).map_err(|e| {
            format!(
                "readiness response was not JSON ({e}): {:?}",
                String::from_utf8_lossy(&response)
            )
        })?;
        value
            .get("targets")
            .and_then(serde_json::Value::as_array)
            .map(Vec::len)
            .ok_or_else(|| format!("readiness response had no targets: {value}"))
    }

    fn control(&self, request: &serde_json::Value) -> serde_json::Value {
        use std::io::Write;
        let mut stream =
            UnixStream::connect(&self.control_socket).expect("connect to the test-control socket");
        stream
            .set_read_timeout(Some(deadline()))
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

    /// The native compositor's current durable tree, exposed by the
    /// test-control endpoint rather than reached through this test's model.
    fn workspace_tree(&self) -> serde_json::Value {
        self.control(&serde_json::json!({"op": "state_dump"}))
            .get("state")
            .and_then(|state| state.get("workspace_tree"))
            .cloned()
            .unwrap_or_else(|| panic!("state dump omitted workspace_tree"))
    }

    fn key(&self, key: &str) {
        let response = self.control(&serde_json::json!({"op": "key", "key": key}));
        assert!(
            response.get("ok").is_some(),
            "key {key:?} failed: {response:?}"
        );
    }

    #[allow(
        clippy::fn_params_excessive_bools,
        reason = "the test-control protocol exposes the compositor's four independent modifier bits"
    )]
    fn modifiers(&self, ctrl: bool, shift: bool, alt: bool, logo: bool) {
        let response = self.control(&serde_json::json!({
            "op": "modifiers", "ctrl": ctrl, "shift": shift, "alt": alt, "logo": logo,
        }));
        assert!(
            response.get("ok").is_some(),
            "modifier update failed: {response:?}"
        );
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

    fn chrome_copy_image(&self) {
        let resp = self.control(&serde_json::json!({"op": "clipboard_copy_image"}));
        assert!(
            resp.get("ok").is_some(),
            "chrome image copy was refused: {resp:?}"
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
        let mut active_tabs = Vec::new();
        walk_json(tree, &mut |value| {
            if let (Some(group), Some(tab)) = (value.get("id"), value.get("active_tab")) {
                active_tabs.push(format!("{group}:{tab}"));
            }
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
             active_strip: {}\nactive_group: {}\nactive tabs by group: [{}]\nstate: {}\nrecent compositor log:\n{}",
            all_tabs.join(", "),
            tabs.join(", "),
            tree.get("active_strip")
                .map_or_else(|| "?".into(), std::string::ToString::to_string),
            tree.get("active_group")
                .map_or_else(|| "?".into(), std::string::ToString::to_string),
            active_tabs.join(", "),
            resp.get("state")
                .map_or_else(|| "?".into(), std::string::ToString::to_string),
            self.recent_log(),
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
        if std::thread::panicking() {
            // Observe the owned child before cleanup kills it. A client's
            // BrokenPipe alone cannot distinguish a compositor exit from a
            // closed Wayland connection. Keep the original panic and never
            // make diagnostics a second panic during unwinding.
            use std::io::Write as _;
            let status = self.child.try_wait();
            let log: String = self.recent_log().chars().take(16_384).collect();
            let _ = writeln!(
                std::io::stderr().lock(),
                "owned compositor status before failure cleanup: {status:?}\n\
                 compositor log tail (at most 40 lines / 16384 characters):\n{log}"
            );
        }
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

fn window_group_count(tree: &serde_json::Value) -> usize {
    let mut count = 0usize;
    walk_json(tree, &mut |value| {
        count = count.saturating_add(
            value
                .get("window_groups")
                .and_then(serde_json::Value::as_array)
                .map_or(0, Vec::len),
        );
    });
    count
}

/// Foreign clients are keyed by the compositor-global surface id, never by
/// app-id. This deliberately returns every surface for an app so a two-window
/// application cannot pass lifecycle coverage by folding its windows into one
/// tab. The low protocol-id bits may legitimately match across connections.
fn foreign_surface_ids(tree: &serde_json::Value, app_id: &str) -> Vec<u64> {
    let mut ids = Vec::new();
    walk_json(tree, &mut |value| {
        let Some(foreign) = value.get("Foreign") else {
            return;
        };
        if foreign.get("app_id").and_then(serde_json::Value::as_str) == Some(app_id)
            && let Some(id) = foreign
                .get("surface_id")
                .and_then(serde_json::Value::as_u64)
        {
            ids.push(id);
        }
    });
    ids.sort_unstable();
    ids
}

fn wait_for_foreign_surface_count(
    compositor: &Compositor,
    app_id: &str,
    count: usize,
    label: &str,
) -> Vec<u64> {
    let started = Instant::now();
    loop {
        let ids = foreign_surface_ids(&compositor.workspace_tree(), app_id);
        if ids.len() == count {
            return ids;
        }
        assert!(
            started.elapsed() < deadline(),
            "{label}; wanted {count} surfaces, got {ids:?}.\n{}",
            compositor.debug_state()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn group_order(tree: &serde_json::Value) -> Vec<u64> {
    tree.get("workspaces")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .flat_map(|workspace| {
            workspace
                .get("strips")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
        })
        .flat_map(|strip| {
            strip
                .get("window_groups")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
        })
        .filter_map(|group| group.get("id").and_then(serde_json::Value::as_u64))
        .collect()
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
    /// Keep the verified protocol endpoint alive for the whole client.
    touch: Option<WlTouch>,
    data_manager: Option<WlDataDeviceManager>,
    primary_manager: Option<ZwpPrimarySelectionDeviceManagerV1>,
    shm: Option<WlShm>,

    /// Set once `xdg_surface.configure` has been acked — the surface is mapped.
    configured: bool,
    /// The newest non-zero size an `xdg_toplevel.configure` asked for.
    toplevel_size: Option<(i32, i32)>,
    /// Set while this client holds keyboard focus, which gates `set_selection`.
    keyboard_focused: bool,
    /// The compositor requested that this exact xdg toplevel exit.  `close`
    /// carries no serial/ack: the application proves it acted by destroying
    /// its own objects below.
    toplevel_close_requested: bool,
    /// The exact XKB text keymap the compositor sent on keyboard focus. A
    /// protocol client must make decisions from this map, not from the
    /// compositor's preferred locale or from the test process environment.
    keyboard_keymap: Option<String>,
    /// Raw evdev transitions observed on the Wayland socket. These are
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
    /// Set after the seat advertised touch and this client issued
    /// `wl_seat.get_touch`; injection tests wait for this protocol precondition.
    touch_bound: bool,
    /// Never ask for a `wl_touch`, like a toolkit without touch support.
    refuse_touch: bool,
    /// Exact `wl_touch` events observed on the client connection.
    touch_events: Vec<TouchTrace>,
    /// Pointer traffic observed after the test clears setup noise.
    pointer_events: Vec<&'static str>,

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

#[derive(Debug, Clone, PartialEq, Eq)]
enum TouchTrace {
    Down(i32),
    Motion(i32),
    Up(i32),
    Cancel,
    Frame,
}

struct TestClient {
    conn: Connection,
    queue: EventQueue<ClientState>,
    qh: QueueHandle<ClientState>,
    state: ClientState,
    surface: Option<WlSurface>,
    xdg_surface: Option<XdgSurface>,
    toplevel: Option<XdgToplevel>,
    data_device: Option<WlDataDevice>,
    primary_device: Option<ZwpPrimarySelectionDeviceV1>,
    app_id: String,
}

/// MIME-specific data from one actual Wayland source. Held descriptors model
/// a copying application that never finishes its transfer.
type HeldClipboardPipes = Arc<Mutex<Vec<(String, OwnedFd)>>>;

struct RichSource {
    html: String,
    plain: String,
    held: Option<HeldClipboardPipes>,
}

/// Keep the source responsive while the sink maps/focuses its window. The
/// production reader's deadline must not depend on the test's dispatch order.
struct ServingClipboard {
    stop: Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl ServingClipboard {
    fn start(mut client: TestClient, payload: RichSource) -> Self {
        let held = payload.held.clone();
        let manager = client.state.data_manager.as_ref().expect("data manager");
        let source = manager.create_data_source(&client.qh, payload);
        source.offer("text/html".into());
        source.offer(TEXT_MIME.into());
        client
            .data_device
            .as_ref()
            .expect("source data device")
            .set_selection(Some(&source), client.state.last_serial);
        client.roundtrip();
        if let Some(held) = held {
            // The compositor mirrors each advertised MIME before the sink
            // exists. Complete those requests first so later held pipes are
            // unambiguously the app worker's own paired transfer.
            client.wait_until(
                |state| {
                    state.send_requests.iter().any(|mime| mime == "text/html")
                        && state.send_requests.iter().any(|mime| mime == TEXT_MIME)
                },
                "compositor mirror did not request both advertised formats",
            );
            held.lock().expect("mirror pipes").clear();
        }
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let thread = std::thread::spawn(move || {
            let started = Instant::now();
            while !worker_stop.load(std::sync::atomic::Ordering::Acquire)
                && started.elapsed() < deadline()
            {
                dispatch_any(&mut [&mut client], None, Duration::from_millis(5));
            }
        });
        Self {
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for ServingClipboard {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let result = thread.join();
            if !std::thread::panicking() {
                result.expect("clipboard source dispatch thread");
            }
        }
    }
}

fn wait_for_clipboard(client: &mut TestClient, condition: impl Fn() -> bool, failure: &str) {
    wait_for_clipboard_before(client, condition, failure, tilo_test_env::hang_deadline());
}

fn wait_for_clipboard_before(
    client: &mut TestClient,
    condition: impl Fn() -> bool,
    failure: &str,
    end: Instant,
) {
    while !condition() && Instant::now() < end {
        dispatch_any(&mut [&mut *client], None, Duration::from_millis(5));
    }
    assert!(
        Instant::now() < end,
        "{failure}: fixture exceeded its deadline"
    );
    assert!(condition(), "{failure}");
}

fn cancellation_clipboard(app: &TestClient) -> (ui::clipboard::WaylandClipboard, Instant) {
    // The fixture's absolute failure deadline precedes transfer expiry. A
    // successful clear therefore proves cancellation rather than a timer firing.
    let end = tilo_test_env::hang_deadline();
    let read_budget = deadline().checked_mul(2).expect("cancellation read budget");
    let clipboard = ui::clipboard::WaylandClipboard::new_for_wayland_test_with_read_budget(
        app.conn.clone(),
        read_budget,
    );
    assert!(
        clipboard.wait_ready_for_wayland_test(end.saturating_duration_since(Instant::now())),
        "clipboard worker did not bind its devices before fixture focus"
    );
    (clipboard, end)
}

impl TestClient {
    fn new(conn: Connection, app_id: &str) -> Self {
        Self::with_state(conn, app_id, ClientState::default())
    }

    /// A client whose toolkit has no touch support: it never asks the seat
    /// for a `wl_touch`.
    fn without_touch(conn: Connection, app_id: &str) -> Self {
        Self::with_state(
            conn,
            app_id,
            ClientState {
                refuse_touch: true,
                ..ClientState::default()
            },
        )
    }

    fn with_state(conn: Connection, app_id: &str, mut state: ClientState) -> Self {
        let mut queue: EventQueue<ClientState> = conn.new_event_queue();
        let qh = queue.handle();
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
            xdg_surface: None,
            toplevel: None,
            data_device: None,
            primary_device: None,
            app_id: app_id.to_string(),
        }
    }

    fn assert_is_tilo_compositor(&mut self) {
        self.roundtrip();
        assert_eq!(
            self.state.seat_name.as_deref(),
            Some(TILO_SEAT_NAME),
            "connected to something that is not the Tilo compositor (seat name mismatch). \
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

    fn clear_touch_and_pointer_trace(&mut self) {
        self.state.touch_events.clear();
        self.state.pointer_events.clear();
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
        self.xdg_surface = Some(xdg_surface);
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
        while started.elapsed() < deadline() {
            if self.state.keyboard_focused {
                self.step("focused");
                return;
            }
            dispatch_any(
                &mut [&mut *self],
                None,
                deadline().saturating_sub(started.elapsed()),
            );
        }
        panic!(
            "the compositor never gave `{}` keyboard focus after mapping a toplevel \
             (waited out the hang bound). Setting a selection requires focus, so nothing \
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

    /// Answer the compositor's configures with buffers of the asked size,
    /// as an application does, until the compositor shows this window: a
    /// window still waiting for a frame that fits its tab is not on screen,
    /// so no pointer reaches it.
    fn present(&mut self, compositor: &Compositor, surface_id: u64) {
        let mut answered = None;
        let started = Instant::now();
        loop {
            self.pump();
            if let Some(size) = self.state.toplevel_size
                && answered != Some(size)
            {
                self.attach_buffer(size.0, size.1);
                answered = Some(size);
            }
            if foreign_frame_presented(compositor, surface_id) {
                self.step("presented");
                return;
            }
            assert!(
                started.elapsed() < deadline(),
                "`{}` (surface {surface_id}) was never shown after answering its configures \
                 (waited out the hang bound).\n{}",
                self.app_id,
                compositor.debug_state(),
            );
        }
    }

    /// [`Self::pointer_target`] without the panic.
    ///
    /// The grid is visited in the same row-major order either way, and only
    /// this client's `wl_pointer.enter` accepts a point. Grid points near a
    /// laid-out client surface are tried first, which finds the same first
    /// point as a whole-screen scan without moving the pointer across every
    /// empty cell; the whole screen is still scanned when that finds nothing.
    fn try_pointer_target(&mut self, compositor: &Compositor) -> Option<(f64, f64)> {
        // A point found while the strip still scrolls is stale once it stops.
        compositor.wait_for_layout_to_settle(
            |name| name.starts_with("foreign-surface-"),
            "client surfaces before aiming the pointer",
        );
        let surfaces = compositor.foreign_surface_rects();
        let near_a_surface = |x: f64, y: f64| {
            let margin = f64::from(POINTER_GRID_STEP);
            surfaces.iter().any(|[sx, sy, sw, sh]| {
                x >= sx - margin
                    && x <= sx + sw + margin
                    && y >= sy - margin
                    && y <= sy + sh + margin
            })
        };
        if !surfaces.is_empty()
            && let Some(point) = self.scan_pointer_grid(compositor, near_a_surface)
        {
            return Some(point);
        }
        self.scan_pointer_grid(compositor, |_, _| true)
    }

    /// Move the pointer over the screen's grid points that `visit` admits, in
    /// row-major order, and return the first at which this client reports a
    /// `wl_pointer.enter`.
    fn scan_pointer_grid(
        &mut self,
        compositor: &Compositor,
        visit: impl Fn(f64, f64) -> bool,
    ) -> Option<(f64, f64)> {
        let (w, h) = compositor.size;
        self.state.pointer_entered = false;
        let mut y = POINTER_GRID_STEP / 2;
        while y < h {
            let mut x = POINTER_GRID_STEP / 2;
            while x < w {
                if visit(f64::from(x), f64::from(y)) {
                    compositor.pointer_move(f64::from(x), f64::from(y));
                    self.pump();
                    if self.state.pointer_entered {
                        self.step(&format!("receives pointer input at ({x}, {y})"));
                        return Some((f64::from(x), f64::from(y)));
                    }
                }
                x = x.saturating_add(POINTER_GRID_STEP);
            }
            y = y.saturating_add(POINTER_GRID_STEP);
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
        let raw = unsafe { libc::memfd_create(c"tilo-clipboard-e2e".as_ptr(), 0) };
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

    /// Dispatch this client's events until `cond` holds, failing with `what`
    /// on timeout. Each wait wakes on the next event from the compositor but
    /// never parks past [`deadline()`]: a plain `blocking_dispatch` would park
    /// forever on a compositor that sends *nothing* — the exact failure this
    /// suite catches — instead of failing.
    fn wait_until(&mut self, cond: impl Fn(&ClientState) -> bool, what: &str) {
        let started = Instant::now();
        while started.elapsed() < deadline() {
            if cond(&self.state) {
                return;
            }
            dispatch_any(
                &mut [&mut *self],
                None,
                deadline().saturating_sub(started.elapsed()),
            );
        }
        if cond(&self.state) {
            return;
        }
        panic!("{what} (waited out the hang bound, app_id {})", self.app_id);
    }

    /// Pump without requiring progress, for waits on something outside this client.
    fn pump(&mut self) {
        self.queue
            .roundtrip(&mut self.state)
            .expect("client roundtrip while pumping");
    }

    /// Cooperate with an `xdg_toplevel.close` by destroying in reverse
    /// creation order.  A compositor close request is intentionally not a
    /// process kill, so this is the application's observable exit action.
    fn destroy_toplevel(&mut self) {
        if let Some(toplevel) = self.toplevel.take() {
            toplevel.destroy();
        }
        if let Some(xdg_surface) = self.xdg_surface.take() {
            xdg_surface.destroy();
        }
        if let Some(surface) = self.surface.take() {
            surface.destroy();
        }
        self.conn.flush().expect("flush client teardown");
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
        let bytes = self.read_clipboard_bytes(TEXT_MIME, peer);
        String::from_utf8(bytes).expect("selection bytes were not UTF-8")
    }

    /// A selection set just before this call may still be on its way, with
    /// an earlier selection's offer already delivered, so wait for an offer
    /// that advertises `mime` rather than taking the first one.
    fn read_clipboard_bytes(&mut self, mime: &str, peer: Option<&mut TestClient>) -> Vec<u8> {
        // Successive copies each publish a new selection, and an earlier one
        // (the text a caller copied before an image, say) can reach this
        // client first. Wait for the offer that carries the wanted format
        // rather than reading whichever selection happened to arrive.
        self.wait_until(
            |s| {
                s.clipboard_selection_seen
                    && s.clipboard_offer.is_some()
                    && s.clipboard_offer_mimes
                        .iter()
                        .any(|offered| offered == mime)
            },
            &format!("no clipboard offer advertising {mime} ever arrived"),
        );
        self.step("clipboard offer received, requesting the bytes");
        let offer = self.state.clipboard_offer.clone().expect("clipboard offer");
        let (read_fd, write_fd) = make_pipe();
        offer.receive(mime.to_string(), write_fd.as_fd_ref());
        drop(write_fd);
        self.conn.flush().expect("flush the receive request");
        drain_bytes(read_fd, self, peer)
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
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn clipboard_crosses_between_clients_and_in_process_chrome_as_text_and_png() {
    // Phase `clipboard_crosses_from_one_client_to_another`.
    {
        let compositor = Compositor::start();
        let payload = "cross-process clipboard payload — ünïcödé ✂";

        let mut source = TestClient::new(compositor.connect(), "org.tilo.test.source");
        source.assert_is_tilo_compositor();
        assert!(
            source.has_global("wl_data_device_manager"),
            "the compositor must advertise wl_data_device_manager; globals: {:?}",
            source.global_names()
        );
        source.map_window(&compositor);
        source.set_clipboard(payload);

        // Mapping the second window moves focus, which is what makes the compositor
        // offer it the selection — the real sequence a user performs.
        let mut sink = TestClient::new(compositor.connect(), "org.tilo.test.sink");
        sink.assert_is_tilo_compositor();
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

    // Phase `clipboard_crosses_from_in_process_chrome_to_a_client`.
    // Copy in the compositor's own chrome, paste in a sandboxed app. Chrome,
    // terminal and file-explorer are linked into the binary, not Wayland
    // clients, so this goes chrome -> `ui::clipboard` -> the compositor's
    // mirror -> a compositor-owned selection -> the focused client.
    {
        let compositor = Compositor::start();
        let payload = "copied inside the compositor process";

        let mut client = TestClient::new(compositor.connect(), "org.tilo.test.chrome-sink");
        client.assert_is_tilo_compositor();
        client.map_window(&compositor);

        compositor.chrome_copy(payload, false);

        let received = client.read_clipboard(None);
        assert_eq!(
            received, payload,
            "a client did not receive what in-process chrome copied"
        );
    }

    // Phase `clipboard_crosses_from_a_client_to_in_process_chrome`.
    // Copy in a sandboxed app, paste in the compositor's own chrome — proves
    // the synchronous paste mirror fills from the live protocol, not just
    // from chrome's own writes.
    {
        let compositor = Compositor::start();
        let payload = "copied inside a sandboxed client";

        let mut client = TestClient::new(compositor.connect(), "org.tilo.test.chrome-source");
        client.assert_is_tilo_compositor();
        client.map_window(&compositor);
        client.set_clipboard(payload);

        // The compositor asks the client for the bytes asynchronously, so the
        // client has to keep being dispatched while the mirror fills.
        let started = Instant::now();
        let mut seen = None;
        while started.elapsed() < deadline() {
            seen = compositor.chrome_paste(false);
            if seen.as_deref() == Some(payload) {
                break;
            }
            // Serve the compositor's `send` request as soon as it arrives.
            dispatch_any(&mut [&mut client], None, Duration::from_millis(5));
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

    // Phase `image_clipboard_from_in_process_chrome_offers_png_and_keeps_text`.
    {
        let compositor = Compositor::start();
        let mut client = TestClient::new(compositor.connect(), "org.tilo.test.image-sink");
        client.assert_is_tilo_compositor();
        client.map_window(&compositor);
        compositor.chrome_copy("caption survives image copy", false);
        compositor.chrome_copy_image();
        // The text copy's own selection may arrive first; the image copy
        // replaces it with an offer advertising both formats.
        client.wait_until(
            |s| {
                s.clipboard_offer_mimes
                    .iter()
                    .any(|mime| mime == "image/png")
            },
            "the image copy's selection never arrived",
        );

        let png = client.read_clipboard_bytes("image/png", None);
        let decoded = ui::png::decode_rgba8(&png).expect("complete PNG");
        assert_eq!((decoded.width, decoded.height), (2, 1));
        assert_eq!(
            decoded.rgba,
            [0xff, 0x00, 0x80, 0xff, 0x10, 0x20, 0x30, 0x40]
        );

        // One compositor-owned selection advertises both formats, so image copy
        // must not make the previously copied text disappear.
        assert_eq!(client.read_clipboard(None), "caption survives image copy");
    }

    // Phase `app_wayland_clipboard_publishes_png_and_text_to_another_client`.
    {
        let compositor = Compositor::start();
        let mut app = TestClient::new(compositor.connect(), "org.tilo.test.app-clipboard");
        app.assert_is_tilo_compositor();
        let clipboard = ui::clipboard::WaylandClipboard::new_for_wayland_test(app.conn.clone());
        app.map_window(&compositor);

        // This is the app-side backend, not HostClipboard: its publication worker
        // owns a second wl_data_device on the app connection and must observe the
        // focused seat before accepting the queued copy.
        assert!(clipboard.set_text(ClipboardTarget::Clipboard, "app caption"));
        assert!(clipboard.set_image(2, 1, &[0xff, 0x00, 0x80, 0xff, 0x10, 0x20, 0x30, 0x40]));

        let mut sink = TestClient::new(compositor.connect(), "org.tilo.test.app-sink");
        sink.assert_is_tilo_compositor();
        sink.map_window(&compositor);
        let png = sink.read_clipboard_bytes("image/png", None);
        let decoded = ui::png::decode_rgba8(&png).expect("complete PNG");
        assert_eq!((decoded.width, decoded.height), (2, 1));
        assert_eq!(
            decoded.rgba,
            [0xff, 0x00, 0x80, 0xff, 0x10, 0x20, 0x30, 0x40]
        );
        assert_eq!(sink.read_clipboard(None), "app caption");
    }
}

#[test]
fn native_borrowed_display_pastes_the_compositors_current_selection() {
    let compositor = Compositor::start();
    let mut client = TestClient::new(compositor.connect(), "org.tilo.test.native-paste");
    client.assert_is_tilo_compositor();
    // This is the native runner's constructor, including its smithay-clipboard
    // readers, rather than the publication-only protocol fixture constructor.
    // SAFETY: client owns the display and outlives this backend.
    let clipboard =
        unsafe { ui::clipboard::WaylandClipboard::new(client.conn.backend().display_ptr().cast()) };
    assert!(clipboard.wait_ready_for_wayland_test(deadline()));
    client.map_window(&compositor);
    compositor.key("a");
    client.roundtrip();
    let payload = "https://example.com/native-paste";
    compositor.chrome_copy(payload, false);
    client.roundtrip();
    assert_eq!(
        clipboard.text(ClipboardTarget::Clipboard).as_deref(),
        Some(payload),
        "the actual native reader lost a selection published by the compositor"
    );
    assert_eq!(
        clipboard
            .request_text(ClipboardTarget::Clipboard)
            .recv_timeout(deadline())
            .expect("explicit native paste reply")
            .as_deref(),
        Some(payload),
        "explicit paste must read the focused compositor selection"
    );
}

#[test]
fn wayland_rich_clipboard_pairs_one_offer_and_clears_on_focus_loss() {
    let compositor = Compositor::start();
    let mut source = TestClient::new(compositor.connect(), "org.tilo.test.rich-source");
    source.assert_is_tilo_compositor();
    source.map_window(&compositor);
    let _serving = ServingClipboard::start(
        source,
        RichSource {
            html: "<p><strong>文🙂</strong></p>".into(),
            plain: "same offer: 文🙂".into(),
            held: None,
        },
    );
    let mut app = TestClient::new(compositor.connect(), "org.tilo.test.rich-sink");
    app.assert_is_tilo_compositor();
    let (clipboard, end) = cancellation_clipboard(&app);
    app.map_window(&compositor);
    // Exercise the real worker alongside the app's own connection and queue.
    wait_for_clipboard_before(
        &mut app,
        || clipboard.rich_text().is_some(),
        "rich offer missing",
        end,
    );
    let offer = clipboard.rich_text().expect("completed rich offer");
    assert_eq!(offer.html, "<p><strong>文🙂</strong></p>");
    assert_eq!(offer.plain.as_deref(), Some("same offer: 文🙂"));

    let mut other = TestClient::new(compositor.connect(), "org.tilo.test.rich-focus-loss");
    other.map_window(&compositor);
    app.wait_until(|state| !state.keyboard_focused, "sink did not lose focus");
    wait_for_clipboard_before(
        &mut app,
        || clipboard.rich_text().is_none(),
        "rich cache survived Leave",
        end,
    );
}

#[test]
fn wayland_rich_clipboard_replacement_keeps_ordinary_plain_paste() {
    let compositor = Compositor::start();
    let mut source = TestClient::new(compositor.connect(), "org.tilo.test.rich-replace-source");
    source.map_window(&compositor);
    let _serving = ServingClipboard::start(
        source,
        RichSource {
            html: "<b>old</b>".into(),
            plain: "old fallback".into(),
            held: None,
        },
    );
    let mut app = TestClient::new(compositor.connect(), "org.tilo.test.rich-replace-sink");
    app.assert_is_tilo_compositor();
    let (clipboard, end) = cancellation_clipboard(&app);
    app.map_window(&compositor);
    wait_for_clipboard_before(
        &mut app,
        || clipboard.rich_text().is_some(),
        "initial rich offer missing",
        end,
    );

    let previous = app.state.clipboard_offer.clone();
    compositor.chrome_copy("replacement plain 文🙂", false);
    app.wait_until(
        |state| state.clipboard_offer != previous,
        "replacement offer missing",
    );
    assert_eq!(app.read_clipboard(None), "replacement plain 文🙂");
    wait_for_clipboard_before(
        &mut app,
        || clipboard.rich_text().is_none(),
        "superseded rich offer survived",
        end,
    );
}

#[test]
fn wayland_rich_clipboard_null_selection_clears_completed_offer() {
    let compositor = Compositor::start();
    let mut source = TestClient::new(compositor.connect(), "org.tilo.test.rich-null-source");
    source.map_window(&compositor);
    let _serving = ServingClipboard::start(
        source,
        RichSource {
            html: "<b>before NULL</b>".into(),
            plain: "before NULL".into(),
            held: None,
        },
    );
    let mut app = TestClient::new(compositor.connect(), "org.tilo.test.rich-null-sink");
    app.assert_is_tilo_compositor();
    let (clipboard, end) = cancellation_clipboard(&app);
    app.map_window(&compositor);
    wait_for_clipboard_before(
        &mut app,
        || clipboard.rich_text().is_some(),
        "initial rich offer missing",
        end,
    );
    app.data_device
        .as_ref()
        .expect("data device")
        .set_selection(None, app.state.last_serial);
    app.roundtrip();
    app.wait_until(
        |state| state.clipboard_offer.is_none(),
        "NULL selection missing",
    );
    wait_for_clipboard_before(
        &mut app,
        || clipboard.rich_text().is_none(),
        "NULL selection retained rich data",
        end,
    );
}

#[test]
fn wayland_rich_clipboard_deadline_closes_stalled_pipe_and_rejects_late_bytes() {
    use std::io::Write as _;
    use std::os::fd::AsRawFd as _;
    let compositor = Compositor::start();
    let held = Arc::new(Mutex::new(Vec::new()));
    let mut source = TestClient::new(compositor.connect(), "org.tilo.test.rich-stall-source");
    source.map_window(&compositor);
    let _serving = ServingClipboard::start(
        source,
        RichSource {
            html: "<b>late</b>".into(),
            plain: "late".into(),
            held: Some(Arc::clone(&held)),
        },
    );
    let mut app = TestClient::new(compositor.connect(), "org.tilo.test.rich-stall-sink");
    app.assert_is_tilo_compositor();
    let clipboard = ui::clipboard::WaylandClipboard::new_for_wayland_test(app.conn.clone());
    assert!(
        clipboard.wait_ready_for_wayland_test(deadline()),
        "clipboard worker did not bind its devices before fixture focus"
    );
    app.map_window(&compositor);
    wait_for_clipboard(
        &mut app,
        || {
            let pipes = held.lock().expect("held pipes");
            let paired = pipes.iter().any(|(mime, _)| mime == "text/html")
                && pipes.iter().any(|(mime, _)| mime == TEXT_MIME);
            drop(pipes);
            paired
        },
        "worker did not request both formats",
    );
    // No focus/selection change occurs here: closure must come from the
    // production deadline, not from a later replacement or test teardown.
    wait_for_clipboard(
        &mut app,
        || {
            let pipes = held.lock().expect("held pipes");
            let closed = pipes.iter().all(|(_, fd)| {
                let mut poll = libc::pollfd {
                    fd: fd.as_raw_fd(),
                    events: libc::POLLOUT,
                    revents: 0,
                };
                // SAFETY: the mutex keeps the descriptor alive for this pollfd.
                let ready = unsafe { libc::poll(&raw mut poll, 1, 0) };
                assert!(ready >= 0, "poll held producer pipe");
                ready > 0 && poll.revents & libc::POLLERR != 0
            });
            drop(pipes);
            closed
        },
        "deadline did not close both stalled readers",
    );
    let expired_pipes = std::mem::take(&mut *held.lock().expect("held pipes"));
    for (mime, fd) in expired_pipes {
        let error = std::fs::File::from(fd)
            .write_all(b"late")
            .expect_err("expired pipe must be closed");
        assert_eq!(
            error.kind(),
            std::io::ErrorKind::BrokenPipe,
            "late {mime} data"
        );
    }
    assert!(clipboard.rich_text().is_none());
    let previous = app.state.clipboard_offer.clone();
    compositor.chrome_copy("after stalled source", false);
    app.wait_until(
        |state| state.clipboard_offer != previous,
        "post-timeout offer missing",
    );
    assert_eq!(app.read_clipboard(None), "after stalled source");
    assert!(clipboard.rich_text().is_none());
}

/// A `wl_touch` client sees the partial two-contact stream, then exactly
/// the protocol cancellation boundary when a third direct contact transfers
/// ownership to the shell. This reaches the native Wayland seat; the headless
/// input adapter reuses DRM's ownership arena.
#[test]
fn third_direct_touch_contact_cancels_the_real_wayland_touch_client_without_leaks() {
    let compositor = Compositor::start();
    let mut client = TestClient::new(compositor.connect(), "org.tilo.test.touch-owner");
    client.assert_is_tilo_compositor();
    client.map_window(&compositor);
    client.wait_until(
        |state| state.touch_bound,
        "the client did not finish binding wl_touch before injection",
    );
    // `get_touch` is issued while dispatching the seat-capability event.
    // Complete one more roundtrip so that request is on the server before
    // the test-control path emits its first `wl_touch.down`.
    client.roundtrip();
    let (x, y) = client.pointer_target(&compositor);
    client.clear_touch_and_pointer_trace();

    // `pointer_target` deliberately scans a coarse grid and may return the
    // last pixel inside a tile. Keep every initial contact at that proven
    // in-surface point rather than offsetting it outside the client rect.
    for slot in [0, 1, 2] {
        compositor.touch("e2e-panel", slot, "down", x, y);
    }
    client.wait_until(
        |state| state.touch_events.len() >= 3,
        "the third contact did not cancel the real wl_touch client stream",
    );
    assert_eq!(
        client.state.touch_events,
        vec![TouchTrace::Down(0), TouchTrace::Down(1), TouchTrace::Cancel,],
        "the client receives its first two downs, then the complete cancellation boundary"
    );

    // These are physical events arriving after ownership already moved to the
    // shell. No down/motion/up or pointer emulation may escape to the client.
    for slot in [0, 1, 2] {
        compositor.touch("e2e-panel", slot, "motion", x, y);
    }
    for slot in [0, 1, 2] {
        compositor.touch("e2e-panel", slot, "up", x, y);
    }
    // Test control injects each edge before it answers, so every event it
    // produced is queued ahead of this sync's reply; a second roundtrip also
    // covers anything the compositor emits while handling the first.
    client.roundtrip();
    client.roundtrip();
    assert_eq!(
        client.state.touch_events,
        vec![TouchTrace::Down(0), TouchTrace::Down(1), TouchTrace::Cancel,],
        "no post-cancel touch event may leak to the client"
    );
    assert!(
        client.state.pointer_events.is_empty(),
        "a shell-owned multi-touch gesture must not manufacture client pointer motion, click, or drag: {:?}",
        client.state.pointer_events
    );
}

/// A one-finger tap on a client tile is a `wl_touch` down/up pair for that
/// client and nothing else. The compositor's pointer emulation exists for
/// shell chrome; a client that already received the touch sequence must not
/// also see a `wl_pointer` click, or every tap would activate it twice.
#[test]
fn a_touch_tap_on_a_client_activates_it_once() {
    let compositor = Compositor::start();
    let mut client = TestClient::new(compositor.connect(), "org.tilo.test.touch-tap");
    client.assert_is_tilo_compositor();
    client.map_window(&compositor);
    client.wait_until(
        |state| state.touch_bound,
        "the client did not finish binding wl_touch before injection",
    );
    client.roundtrip();
    let (x, y) = client.pointer_target(&compositor);
    client.clear_touch_and_pointer_trace();

    compositor.touch("e2e-panel", 0, "down", x, y);
    compositor.touch("e2e-panel", 0, "frame", x, y);
    compositor.touch("e2e-panel", 0, "up", x, y);
    compositor.touch("e2e-panel", 0, "frame", x, y);
    client.wait_until(
        |state| state.touch_events.contains(&TouchTrace::Up(0)),
        "the tapped client never received its wl_touch up",
    );
    // Test control injects each edge before it answers, so every event it
    // produced is queued ahead of this sync's reply; a second roundtrip also
    // covers anything the compositor emits while handling the first.
    client.roundtrip();
    client.roundtrip();

    assert_eq!(
        client.state.touch_events,
        vec![
            TouchTrace::Down(0),
            TouchTrace::Frame,
            TouchTrace::Up(0),
            TouchTrace::Frame,
        ],
        "the tapped client receives exactly one touch sequence"
    );
    let buttons = client
        .state
        .pointer_events
        .iter()
        .filter(|event| **event == "button")
        .count();
    assert_eq!(
        buttons, 0,
        "a tap already delivered as wl_touch must not also click the client: {:?}",
        client.state.pointer_events
    );
}

/// A client that never asked for `wl_touch` still works under a finger: a
/// tap on it arrives as exactly one pointer click, and no touch traffic.
#[test]
fn a_touch_tap_on_a_client_without_wl_touch_is_one_click() {
    let compositor = Compositor::start();
    let mut client = TestClient::without_touch(compositor.connect(), "org.tilo.test.touch-less");
    client.assert_is_tilo_compositor();
    client.map_window(&compositor);
    client.roundtrip();
    assert!(!client.state.touch_bound);
    let (x, y) = client.pointer_target(&compositor);
    client.clear_touch_and_pointer_trace();

    compositor.touch("e2e-panel", 0, "down", x, y);
    compositor.touch("e2e-panel", 0, "frame", x, y);
    compositor.touch("e2e-panel", 0, "up", x, y);
    compositor.touch("e2e-panel", 0, "frame", x, y);
    client.wait_until(
        |state| {
            state
                .pointer_events
                .iter()
                .filter(|e| **e == "button")
                .count()
                >= 2
        },
        "the touch-less client never received the tap as a click",
    );
    client.roundtrip();
    client.roundtrip();

    let buttons = client
        .state
        .pointer_events
        .iter()
        .filter(|event| **event == "button")
        .count();
    assert_eq!(
        buttons, 2,
        "one press and one release: {:?}",
        client.state.pointer_events
    );
    assert!(client.state.touch_events.is_empty());
}

/// The desktop help overlay speaks the input language in use. A headless
/// session starts with no input devices at all; the first touch contact
/// attaches its panel and switches help to gestures, and typing switches it
/// back to keyboard shortcuts while the touchscreen stays available.
#[test]
fn desktop_help_follows_the_input_device_in_use() {
    let compositor = Compositor::start();
    let input = || {
        let response = compositor.control(&serde_json::json!({"op": "state_dump"}));
        response
            .get("state")
            .and_then(|state| state.get("chrome"))
            .and_then(|chrome| chrome.get("input"))
            .cloned()
            .unwrap_or_else(|| panic!("state_dump carries no chrome input: {response}"))
    };

    let initial = input();
    assert_eq!(
        initial["available"],
        serde_json::json!([]),
        "no input device may be assumed before one exists: {initial}"
    );
    assert_eq!(initial["help_modality"], "pointer", "{initial}");

    let (w, h) = compositor.size;
    let (x, y) = (f64::from(w) / 2.0, f64::from(h) / 2.0);
    compositor.touch("e2e-panel", 0, "down", x, y);
    compositor.touch("e2e-panel", 0, "frame", x, y);
    compositor.touch("e2e-panel", 0, "up", x, y);
    compositor.touch("e2e-panel", 0, "frame", x, y);
    let touched = input();
    assert_eq!(touched["modality"], "touch", "{touched}");
    assert_eq!(touched["help_modality"], "touch", "{touched}");
    assert_eq!(
        touched["available"],
        serde_json::json!(["touch"]),
        "{touched}"
    );

    compositor.key("F1");
    let typed = input();
    assert_eq!(typed["modality"], "keyboard", "{typed}");
    assert_eq!(typed["help_modality"], "keyboard", "{typed}");
    assert_eq!(
        typed["available"],
        serde_json::json!(["keyboard", "touch"]),
        "{typed}"
    );
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
    while started.elapsed() < deadline() {
        match file.read(&mut chunk) {
            Ok(0) => return out,
            Ok(n) => out.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // Wake on bytes in the pipe or on either client's next event
                // (the owner's `send` request is what produces the bytes).
                use std::os::unix::io::AsFd;
                let remaining = deadline().saturating_sub(started.elapsed());
                match peer.as_deref_mut() {
                    Some(p) => dispatch_any(&mut [&mut *reader, p], Some(file.as_fd()), remaining),
                    None => dispatch_any(&mut [&mut *reader], Some(file.as_fd()), remaining),
                }
            }
            Err(e) => panic!("reading the selection pipe failed: {e}"),
        }
    }
    panic!(
        "the selection transfer never reached EOF within the hang bound; \
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
                if caps.contains(wl_seat::Capability::Touch) && !state.refuse_touch {
                    state.touch = Some(seat.get_touch(qh, ()));
                    state.touch_bound = true;
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
                state.pointer_events.push("enter");
            }
            wl_pointer::Event::Leave { serial, .. } => {
                state.pointer_entered = false;
                state.last_serial = serial;
                state.pointer_events.push("leave");
            }
            wl_pointer::Event::Motion { .. } => {
                state.pointer_events.push("motion");
            }
            wl_pointer::Event::Button {
                serial, state: s, ..
            } => {
                state.last_serial = serial;
                state.pointer_events.push("button");
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

impl Dispatch<WlTouch, ()> for ClientState {
    fn event(
        state: &mut Self,
        _: &WlTouch,
        event: wl_touch::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_touch::Event::Down { id, .. } => state.touch_events.push(TouchTrace::Down(id)),
            wl_touch::Event::Motion { id, .. } => state.touch_events.push(TouchTrace::Motion(id)),
            wl_touch::Event::Up { id, .. } => state.touch_events.push(TouchTrace::Up(id)),
            wl_touch::Event::Cancel => state.touch_events.push(TouchTrace::Cancel),
            wl_touch::Event::Frame => state.touch_events.push(TouchTrace::Frame),
            _ => {}
        }
    }
}

crate::support::wayland::test_client!(ClientState: ping);

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
        state: &mut Self,
        _: &XdgToplevel,
        event: xdg_toplevel::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            xdg_toplevel::Event::Close => state.toplevel_close_requested = true,
            xdg_toplevel::Event::Configure { width, height, .. } if width > 0 && height > 0 => {
                state.toplevel_size = Some((width, height));
            }
            _ => {}
        }
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

impl Dispatch<WlDataSource, RichSource> for ClientState {
    fn event(
        state: &mut Self,
        _: &WlDataSource,
        event: wl_data_source::Event,
        payload: &RichSource,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_data_source::Event::Send { mime_type, fd } = event {
            state.send_requests.push(mime_type.clone());
            if let Some(held) = &payload.held {
                held.lock()
                    .expect("held producer pipes")
                    .push((mime_type, fd));
            } else if mime_type == "text/html" {
                write_and_close(fd, &payload.html);
            } else if mime_type == TEXT_MIME {
                write_and_close(fd, &payload.plain);
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

/// Assert the observable layout carried by a compiled XKB wire map.
///
/// libxkbcommon 1.8 expands `de(nodeadkeys)` and omits the variant label from
/// its serialized text, whereas newer versions retain the include. These
/// physical positions distinguish the intended layout from both US and normal
/// German dead-key behavior without depending on compiler metadata.
fn assert_de_nodeadkeys_keymap(keymap: &str) {
    let symbols = translated_keysyms(
        keymap,
        &[
            (21, true),
            (21, false),
            (42, true),
            (12, true),
            (12, false),
            (43, true),
            (43, false),
            (42, false),
        ],
    );
    assert_eq!(
        symbols,
        vec![
            keysyms::KEY_z,
            keysyms::KEY_Shift_L,
            keysyms::KEY_question,
            keysyms::KEY_apostrophe
        ],
        "the client received a keymap other than the persisted de:nodeadkeys layout"
    );
}

/// A GUI application can spawn helpers. Put it in its own process group so a
/// failed live-client probe cannot leave an Electron renderer attached to the
/// developer's machine after the isolated compositor is gone.
struct KillProcessGroup {
    child: Child,
    groups: std::collections::BTreeSet<i32>,
}

impl KillProcessGroup {
    fn new(child: Child, direct_group: bool) -> Self {
        let mut groups = std::collections::BTreeSet::new();
        if direct_group {
            groups.insert(i32::try_from(child.id()).unwrap_or(i32::MAX));
        }
        Self { child, groups }
    }

    fn remember_group(&mut self, group: i32) {
        if group > 1 {
            self.groups.insert(group);
        }
    }
}

impl Drop for KillProcessGroup {
    #[allow(
        clippy::arithmetic_side_effects,
        reason = "test cleanup uses negated positive process-group ids and a short bounded deadline"
    )]
    fn drop(&mut self) {
        let pid = i32::try_from(self.child.id()).unwrap_or(i32::MAX);
        // The direct probe owns `pid`'s group.  The production launcher path
        // records only process groups found below its own child, never the
        // test runner's inherited group.
        for group in &self.groups {
            let _ = unsafe { libc::kill(-*group, libc::SIGTERM) };
        }
        let _ = unsafe { libc::kill(pid, libc::SIGTERM) };
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if self.child.try_wait().ok().flatten().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        // A wedged Electron renderer must not wedge the test runner.
        for group in &self.groups {
            let _ = unsafe { libc::kill(-*group, libc::SIGKILL) };
        }
        let _ = self.child.wait();
    }
}

/// Inspect only the throwaway process group created by this test.  This tells
/// an Electron wrapper dropping a launch token apart from a satellite bridge
/// dropping one, without reading the user's running desktop processes or
/// printing the opaque capability value.
fn private_process_group_has_env(child: &mut KillProcessGroup, key: &[u8]) -> bool {
    let Ok(child_pid) = i32::try_from(child.child.id()) else {
        return false;
    };
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return false;
    };
    entries.filter_map(Result::ok).any(|entry| {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<i32>() else {
            return false;
        };
        // Do not inspect arbitrary same-UID programs: only the bounded
        // descendant chain of the launcher process this test created.
        let mut ancestor = pid;
        let mut belongs_to_launch = false;
        for _ in 0..=16 {
            if ancestor == child_pid {
                belongs_to_launch = true;
                break;
            }
            let Ok(stat) = std::fs::read_to_string(format!("/proc/{ancestor}/stat")) else {
                break;
            };
            let Some((_, fields)) = stat.rsplit_once(") ") else {
                break;
            };
            let Some(parent) = fields
                .split_ascii_whitespace()
                .nth(1)
                .and_then(|parent| parent.parse::<i32>().ok())
            else {
                break;
            };
            if parent <= 1 || parent == ancestor {
                break;
            }
            ancestor = parent;
        }
        if !belongs_to_launch {
            return false;
        }
        let has_key = std::fs::read(entry.path().join("environ"))
            .ok()
            .is_some_and(|env| {
                env.split(|byte| *byte == 0)
                    .any(|item| item.starts_with(key))
            });
        if has_key {
            // SAFETY: querying a process group is read-only; a process
            // exiting between enumeration and this call simply returns -1.
            let group = unsafe { libc::getpgid(pid) };
            child.remember_group(group);
        }
        has_key
    })
}

/// Read the mapped X11 window's advertised PID/startup properties through the
/// private satellite display.  Returns only booleans and PIDs, never an
/// activation token, so diagnostics cannot disclose the one-shot capability.
fn private_x11_activation_facts(compositor: &Compositor) -> Vec<(u32, bool, bool)> {
    let Some((display, authority)) = private_xwayland_connection(compositor) else {
        return Vec::new();
    };
    let run_xprop = |args: &[String]| {
        Command::new("/usr/bin/xprop")
            .args(args)
            .env_clear()
            .env("DISPLAY", &display)
            .env("XAUTHORITY", &authority)
            .env("PATH", "/usr/bin:/bin")
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
    };
    let root_args = vec!["-root".to_string(), "_NET_CLIENT_LIST".to_string()];
    let Some(root) = run_xprop(&root_args) else {
        return Vec::new();
    };
    root.split_whitespace()
        .filter_map(|word| word.strip_prefix("0x"))
        .filter_map(|hex| u64::from_str_radix(hex.trim_end_matches(','), 16).ok())
        .filter_map(|id| u32::try_from(id).ok())
        .filter_map(|id| {
            let args = vec![
                "-id".to_string(),
                format!("0x{id:x}"),
                "_NET_WM_PID".to_string(),
                "_NET_STARTUP_ID".to_string(),
            ];
            let props = run_xprop(&args)?;
            let pid = props
                .lines()
                .find(|line| line.starts_with("_NET_WM_PID"))?
                .split_whitespace()
                .last()?
                .parse::<u32>()
                .ok()?;
            let has_startup_id = props
                .lines()
                .any(|line| line.starts_with("_NET_STARTUP_ID"));
            let has_token = std::fs::read(format!("/proc/{pid}/environ"))
                .ok()
                .is_some_and(|env| {
                    env.split(|byte| *byte == 0)
                        .any(|item| item.starts_with(b"XDG_ACTIVATION_TOKEN="))
                });
            Some((pid, has_token, has_startup_id))
        })
        .collect()
}

/// Start the installed `ChatGPT` Electron client strictly through Tilo's
/// rootless Xwayland bridge. The test is opt-in because it opens a real
/// heavyweight graphical process, but it is a genuine compatibility lane:
/// private HOME/XDG state, Tilo-created Xauthority, no inherited display or
/// D-Bus session, and a child process group that is always torn down.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "the timeout is a fixed, short test-fixture deadline"
)]
fn private_xwayland_connection(compositor: &Compositor) -> Option<(String, PathBuf)> {
    let bridge = compositor.runtime_dir.join("tilo-session/xwayland");
    let deadline = tilo_test_env::hang_deadline();
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
            "Tilo did not publish its private Xwayland authority; compositor log:\n{}",
            compositor.recent_log()
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    let number = authority
        .file_name()?
        .to_string_lossy()
        .strip_prefix("Xauthority-")?
        .to_string();
    Some((format!(":{number}"), authority))
}

#[allow(
    clippy::too_many_lines,
    reason = "the opt-in end-to-end launcher fixture keeps private XDG, display, and process-group setup together"
)]
fn spawn_isolated_chatgpt_x11(
    compositor: &Compositor,
    activation_token: Option<&str>,
) -> Option<(KillProcessGroup, Arc<Mutex<Vec<String>>>)> {
    let launcher = std::env::var_os("TILO_TEST_LAUNCHER_BINARY")
        .map(PathBuf::from)
        .filter(|path| path.is_file());
    let Some(launcher) = launcher else {
        eprintln!(
            "SKIP Xwayland/Electron lane: set TILO_TEST_LAUNCHER_BINARY to the built tilo-launcher; this test intentionally exercises the production host-launch process-group path"
        );
        return None;
    };
    let (display, authority) = private_xwayland_connection(compositor)?;
    let profile = compositor.runtime_dir.join("chatgpt-profile");
    std::fs::create_dir_all(&profile).expect("create isolated Electron profile");
    // The test runs in a disposable distrobox where the installed host entry
    // is visible under `/run/host`, while production tilo-launcher runs on the
    // host where its `Exec=/usr/bin/chatgpt` is correct.  Preserve every
    // installed entry field and rewrite only that mount boundary in a private
    // first-precedence XDG data root, so this remains a real `run-host`
    // launch rather than a direct Electron invocation.
    let installed_entry = [
        PathBuf::from("/usr/share/applications/chatgpt.desktop"),
        PathBuf::from("/run/host/usr/share/applications/chatgpt.desktop"),
    ]
    .into_iter()
    .find(|path| path.is_file())?;
    let entry = std::fs::read_to_string(&installed_entry).ok()?;
    let exec = entry
        .lines()
        .find(|line| line == &"Exec=chatgpt %U")
        .map(|_| "Exec=/run/host/usr/bin/chatgpt %U")?;
    let entry = entry.replacen("Exec=chatgpt %U", exec, 1);
    let data_home = profile.join("share");
    let applications = data_home.join("applications");
    std::fs::create_dir_all(&applications).ok()?;
    std::fs::write(applications.join("chatgpt.desktop"), entry).ok()?;

    let desktop_id = "chatgpt";
    let xdg_data_dirs = "/nonexistent/tilo-private-host-launch-fixture";
    let mut preflight = Command::new(&launcher);
    preflight
        .env_clear()
        .args([
            "run-host",
            "--desktop-id",
            desktop_id,
            "--x11-display",
            &display,
            "--x11-authority",
        ])
        .arg(&authority)
        .arg("--dry-run")
        .env("PATH", "/usr/bin:/bin")
        .env("LANG", "C.UTF-8")
        .env("HOME", &profile)
        .env("XDG_CONFIG_HOME", profile.join("config"))
        .env("XDG_DATA_HOME", &data_home)
        .env("XDG_DATA_DIRS", xdg_data_dirs)
        .env("XDG_CACHE_HOME", profile.join("cache"))
        .env("XDG_STATE_HOME", profile.join("state"))
        .env("XDG_RUNTIME_DIR", &compositor.runtime_dir);
    let preflight = preflight.output().expect("run host-launch preflight");
    assert!(
        preflight.status.success(),
        "private host-entry fixture must resolve through tilo-launcher before Electron starts: {}",
        String::from_utf8_lossy(&preflight.stderr)
    );

    let mut command = Command::new(launcher);
    command
        // Match `HostLaunch::to_command`: the test must not accidentally
        // inherit the user's desktop/display or a prior launch capability.
        .env_clear()
        .args([
            "run-host",
            "--desktop-id",
            desktop_id,
            "--x11-display",
            &display,
            "--x11-authority",
        ])
        .arg(&authority)
        .env("DISPLAY", display)
        .env("XAUTHORITY", authority)
        .env("PATH", "/usr/bin:/bin")
        .env("LANG", "C.UTF-8")
        .env("HOME", &profile)
        .env("XDG_CONFIG_HOME", profile.join("config"))
        .env("XDG_DATA_HOME", &data_home)
        .env("XDG_DATA_DIRS", xdg_data_dirs)
        .env("XDG_CACHE_HOME", profile.join("cache"))
        .env("XDG_STATE_HOME", profile.join("state"))
        .env("XDG_RUNTIME_DIR", &compositor.runtime_dir)
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("DBUS_SESSION_BUS_ADDRESS")
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    if let Some(token) = activation_token {
        // Tilo's compositor passes this intermediary only to tilo-launcher.
        // The launcher clears it, creates the production per-launch process
        // group, then gives the final host child only the standard variables.
        command.env("TILO_HOST_ACTIVATION_TOKEN", token);
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
    Some((KillProcessGroup::new(child, false), log))
}

fn spawn_isolated_x11_probe(
    compositor: &Compositor,
    activation_token: &str,
) -> Option<KillProcessGroup> {
    let program = Path::new("/usr/bin/zenity");
    if !program.is_file() {
        eprintln!("SKIP Xwayland token probe: zenity is not installed");
        return None;
    }
    let (display, authority) = private_xwayland_connection(compositor)?;
    let mut command = Command::new(program);
    command
        .env_clear()
        .args([
            "--info",
            "--text=private xwayland activation probe",
            "--timeout=8",
        ])
        .env("DISPLAY", display)
        .env("XAUTHORITY", authority)
        .env("GDK_BACKEND", "x11")
        .env("XDG_ACTIVATION_TOKEN", activation_token)
        .env("DESKTOP_STARTUP_ID", activation_token)
        .env("HOME", compositor.runtime_dir.join("x11-probe-home"))
        .env("XDG_RUNTIME_DIR", &compositor.runtime_dir)
        .env("PATH", "/usr/bin:/bin")
        .env("LANG", "C.UTF-8")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: the probe's new process group is established before exec and
    // contains no user-session process, so `Drop` can reclaim it exactly.
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command
        .spawn()
        .ok()
        .map(|child| KillProcessGroup::new(child, true))
}

#[test]
#[ignore = "starts installed Electron with a compositor-minted token in a private Xwayland session; opt in for compatibility review"]
#[allow(
    clippy::too_many_lines,
    reason = "The opt-in compatibility test keeps native and Electron clients alive through one ordered mapping, focus, and placement lifecycle."
)]
fn installed_electron_client_maps_focuses_and_places_through_private_xwayland() {
    let compositor = Compositor::start_host_desktop();
    // Keep one ordinary native Wayland client alive throughout the Electron
    // launch. A bridge that only works in an otherwise empty compositor is
    // not compatible with a desktop where Tilo and host applications coexist.
    let mut native = TestClient::new(compositor.connect(), "org.tilo.test.native-alongside-x11");
    native.assert_is_tilo_compositor();
    native.map_window(&compositor);
    let before = compositor
        .control(&serde_json::json!({ "op": "foreign_frames" }))
        .get("foreign_frames")
        .and_then(|payload| payload.get("frames"))
        .and_then(serde_json::Value::as_array)
        .map_or(0, Vec::len);
    let before_state = compositor.control(&serde_json::json!({ "op": "state_dump" }));
    let before_groups = before_state
        .get("state")
        .and_then(|state| state.get("workspace_tree"))
        .map_or(0, window_group_count);
    let display_name = "ChatGPT host activation test";
    let token = compositor.mint_host_launch_activation("chatgpt", display_name);
    let Some((mut child, log)) = spawn_isolated_chatgpt_x11(&compositor, Some(&token)) else {
        return;
    };
    let started = Instant::now();
    let mut token_reached_electron_process_group = false;
    loop {
        token_reached_electron_process_group |=
            private_process_group_has_env(&mut child, b"XDG_ACTIVATION_TOKEN=");
        let frames = compositor.control(&serde_json::json!({ "op": "foreign_frames" }));
        let mapped = frames
            .get("foreign_frames")
            .and_then(|payload| payload.get("frames"))
            .and_then(serde_json::Value::as_array)
            .is_some_and(|entries| entries.len() > before);
        if mapped {
            break;
        }
        assert!(
            started.elapsed() < deadline(),
            "the installed Electron client never mapped through Tilo Xwayland.\nLast foreign-frame state:\n{}\nElectron log:\n{}\nCompositor log:\n{}",
            serde_json::to_string_pretty(&frames).unwrap_or_else(|_| frames.to_string()),
            log.lock().map(|lines| lines.join("\n")).unwrap_or_default(),
            compositor.recent_log(),
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let x11_facts = private_x11_activation_facts(&compositor);
    assert!(
        token_reached_electron_process_group,
        "the ChatGPT launcher/Electron process group discarded XDG_ACTIVATION_TOKEN before satellite could read it"
    );
    // Rootless satellite identifies the client with XRes.  It does not have
    // to publish the non-standard `_NET_WM_PID` EWMH property, so use it only
    // as supplementary evidence when available rather than making the
    // standards-path regression depend on it.
    if !x11_facts.is_empty() {
        assert!(
            x11_facts.iter().any(|(_, has_token, _)| *has_token),
            "the mapped Electron _NET_WM_PID has no XDG_ACTIVATION_TOKEN; X11 facts (pid, has_token, has_startup_id): {x11_facts:?}"
        );
    }
    native.wait_until(
        |state| !state.keyboard_focused,
        "the X11/Electron toplevel mapped but did not consume the host activation token to take focus",
    );
    // X11 clients are not required to bind the Wayland xdg-activation
    // protocol themselves. Chromium keeps the standard token in its process
    // environment, while xwayland-satellite maps the window under the
    // desktop entry's StartupWMClass. The shell's identity-keyed pending
    // placement is therefore the compatibility contract; token consumption
    // is an optional focus refinement, not a prerequisite for opening an X11
    // application. A separate generic-X11 test below covers satellite's
    // activation relay with a client which publishes startup notification.
    let state = compositor.control(&serde_json::json!({ "op": "state_dump" }));
    let tree = state
        .get("state")
        .and_then(|state| state.get("workspace_tree"))
        .cloned()
        .unwrap_or_else(|| panic!("state dump did not contain a workspace tree: {state:?}"));
    let mut titles = Vec::new();
    let mut chatgpt_surfaces = 0usize;
    walk_json(&tree, &mut |value| {
        if let (Some(title), Some(tile)) = (value.get("title"), value.get("tile")) {
            titles.push(title.as_str().unwrap_or_default().to_string());
            if tile
                .get("Foreign")
                .and_then(|foreign| foreign.get("app_id"))
                .and_then(serde_json::Value::as_str)
                .is_some_and(|app_id| app_id.eq_ignore_ascii_case("chatgpt"))
            {
                chatgpt_surfaces = chatgpt_surfaces.saturating_add(1);
            }
        }
    });
    assert!(
        titles.iter().any(|title| title == "ChatGPT")
            || titles.iter().any(|title| title == display_name),
        "satellite mapped Electron but Tilo did not retain its desktop-entry identity. tabs: {titles:?}\nElectron log:\n{}\nCompositor log:\n{}",
        log.lock().map(|lines| lines.join("\n")).unwrap_or_default(),
        compositor.recent_log(),
    );
    assert_eq!(
        chatgpt_surfaces, 1,
        "one installed ChatGPT launch must create exactly one live foreign tab; tabs: {titles:?}"
    );
    assert_eq!(
        window_group_count(&tree),
        before_groups + 1,
        "the installed Electron surface must claim the launch's requested NewGroup placement; tree: {}",
        serde_json::to_string_pretty(&tree).unwrap_or_else(|_| tree.to_string())
    );
    // A mapped heavyweight client must not leave the compositor event loop
    // wedged: this final independent round trip is deliberately after focus,
    // identity reconciliation and tree inspection.
    assert!(
        compositor
            .control(&serde_json::json!({ "op": "foreign_frames" }))
            .get("foreign_frames")
            .is_some(),
        "the compositor stopped servicing control traffic after Electron mapped"
    );
}

#[test]
fn generic_x11_client_relays_compositor_token_through_private_satellite() {
    let compositor = Compositor::start_host_desktop();
    let mut native = TestClient::new(compositor.connect(), "org.tilo.test.native-x11-token");
    native.map_window(&compositor);
    let before = compositor
        .control(&serde_json::json!({ "op": "foreign_frames" }))
        .get("foreign_frames")
        .and_then(|payload| payload.get("frames"))
        .and_then(serde_json::Value::as_array)
        .map_or(0, Vec::len);
    let display_name = "Generic X11 activation probe";
    let token = compositor.mint_host_launch_activation("org.example.x11-probe", display_name);
    let Some(_probe) = spawn_isolated_x11_probe(&compositor, &token) else {
        return;
    };
    let started = Instant::now();
    loop {
        let mapped = compositor
            .control(&serde_json::json!({ "op": "foreign_frames" }))
            .get("foreign_frames")
            .and_then(|payload| payload.get("frames"))
            .and_then(serde_json::Value::as_array)
            .is_some_and(|frames| frames.len() > before);
        if mapped {
            break;
        }
        assert!(
            started.elapsed() < deadline(),
            "the private generic X11 probe did not map through satellite. compositor log:\n{}",
            compositor.recent_log()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    native.wait_until(
        |state| !state.keyboard_focused,
        "the private generic X11 probe mapped but did not consume the compositor activation token",
    );
    let state = compositor.control(&serde_json::json!({ "op": "state_dump" }));
    let tree = state
        .get("state")
        .and_then(|state| state.get("workspace_tree"))
        .unwrap_or_else(|| panic!("state dump did not contain workspace tree: {state:?}"));
    let mut titles = Vec::new();
    walk_json(tree, &mut |value| {
        if let (Some(title), Some(_tile)) = (value.get("title"), value.get("tile")) {
            titles.push(title.as_str().unwrap_or_default().to_string());
        }
    });
    assert!(
        titles.iter().any(|title| title == display_name),
        "generic X11 token was not relayed through satellite/Tilo relocation. tabs: {titles:?}\ncompositor log:\n{}",
        compositor.recent_log(),
    );
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn real_clients_get_immediate_focus_and_the_full_keyboard_contract() {
    // Phase `keyboard_focus_does_not_wait_for_mismatched_frame_presentation`.
    // This is a client-to-compositor exercise, not a unit test of
    // `KeyboardSettings`: Tilo must serialize the selected German keymap
    // onto the Wayland seat and forward complete press/release/modifier streams
    // to a focused foreign client. Electron and Xwayland both consume this same
    // seat contract.
    {
        let compositor = Compositor::start();
        let app_id = "org.tilo.test.focus-before-resize";
        let mut client = TestClient::new(compositor.connect(), app_id);
        client.assert_is_tilo_compositor();

        // `map_window` attaches its deliberate 320x240 first frame and only
        // returns once it has received wl_keyboard.enter. The tab is much larger,
        // so visual presentation remains behind the anti-spoof resize gate; input
        // must not wait for that gate's ten-second backstop.
        client.map_window(&compositor);

        let frames = compositor.control(&serde_json::json!({ "op": "foreign_frames" }));
        let frame = frames
            .get("foreign_frames")
            .and_then(|payload| payload.get("frames"))
            .and_then(serde_json::Value::as_array)
            .and_then(|frames| {
                frames.iter().find(|frame| {
                    frame.get("app_id").and_then(serde_json::Value::as_str) == Some(app_id)
                })
            })
            .unwrap_or_else(|| {
                panic!(
                    "mapped focused client was absent from foreign frame state: {frames:?}\\ncompositor log:\\n{}",
                    compositor.recent_log()
                )
            });
        assert_eq!(
            frame.get("buffer").and_then(|buffer| buffer.get("w")),
            Some(&serde_json::json!(320))
        );
        assert_eq!(
            frame.get("buffer").and_then(|buffer| buffer.get("h")),
            Some(&serde_json::json!(240))
        );
        assert_eq!(
            frame.get("presented").and_then(serde_json::Value::as_bool),
            Some(false),
            "the test must retain the visual resize gate while proving focus arrived first; frame: {frame:?}"
        );
    }

    // Phase `real_wayland_client_gets_de_nodeadkeys_symbols_modifiers_and_repeat_contract`.
    {
        let compositor = Compositor::start_with_input_settings(&serde_json::json!({
            "keyboard_layout": "de:nodeadkeys",
            "key_repeat": 0.5,
        }));
        let mut client = TestClient::new(compositor.connect(), "org.tilo.test.de-keyboard");
        client.assert_is_tilo_compositor();
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
        assert_de_nodeadkeys_keymap(&keymap);
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
            // repeated delivery according to `repeat_info`; Tilo must never leave
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
}

/// Waits until `state_dump` reports the chrome overlay `name` open.
fn wait_for_overlay(compositor: &Compositor, name: &str, failure: &str) {
    let started = Instant::now();
    loop {
        let state = compositor.control(&serde_json::json!({ "op": "state_dump" }));
        if state["state"]["chrome"]["overlays"][name].as_bool() == Some(true) {
            return;
        }
        assert!(
            started.elapsed() < deadline(),
            "{failure}. {}",
            compositor.debug_state()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn compositor_modes_take_and_return_client_input_cleanly() {
    // Phase `print_screen_capture_owns_both_edges_and_restores_a_clean_client_focus`.
    // Print Screen is compositor-owned, including its release. This drives the
    // developer socket through the input ledger rather than only asking the
    // overlay host to open Capture Studio: a client must lose focus while capture
    // is active, never see a stray Print release, and regain a clean keyboard
    // stream after Capture Studio closes.
    {
        let compositor = Compositor::start();
        let mut client = TestClient::new(compositor.connect(), "org.tilo.test.print-screen");
        client.assert_is_tilo_compositor();
        client.map_window(&compositor);
        client.clear_keyboard_trace();

        for (operation, key) in [("key_down", "PrintScreen"), ("key_up", "PrintScreen")] {
            let response = compositor.control(&serde_json::json!({ "op": operation, "key": key }));
            assert!(
                response.get("ok").is_some(),
                "test control refused {operation} for Print Screen: {response:?}"
            );
        }
        client.wait_until(
            |state| !state.keyboard_focused,
            "Capture Studio did not take keyboard focus from the mapped client",
        );
        client.roundtrip();
        assert!(
            client.state.keyboard_events.is_empty(),
            "the focused client received a Print Screen edge despite capture owning the shortcut: {:?}",
            client.state.keyboard_events
        );

        // A second Print while capture is active is consumed, rather than opening
        // a second capture surface or leaking into the now-unfocused client.
        for (operation, key) in [("key_down", "PrintScreen"), ("key_up", "PrintScreen")] {
            let response = compositor.control(&serde_json::json!({ "op": operation, "key": key }));
            assert!(
                response.get("ok").is_some(),
                "test control refused the second {operation}: {response:?}"
            );
        }
        client.roundtrip();
        assert!(
            !client.state.keyboard_focused && client.state.keyboard_events.is_empty(),
            "the second Print must leave the existing capture focus and client ledger untouched"
        );

        let response = compositor.control(&serde_json::json!({ "op": "key", "key": "Escape" }));
        assert!(
            response.get("ok").is_some(),
            "test control refused Escape to close Capture Studio: {response:?}"
        );
        client.wait_until(
            |state| state.keyboard_focused,
            "the exact prior client did not regain keyboard focus after Capture Studio closed",
        );
        client.clear_keyboard_trace();
        for (operation, key) in [("key_down", "Tab"), ("key_up", "Tab")] {
            let response = compositor.control(&serde_json::json!({ "op": operation, "key": key }));
            assert!(
                response.get("ok").is_some(),
                "test control refused {operation} for post-capture Tab: {response:?}"
            );
        }
        client.wait_for_keyboard_events(2);
        assert_eq!(
            client.state.keyboard_events,
            vec![(15, true), (15, false)],
            "the first post-capture key must have one matched press/release and no delayed Print release"
        );
    }

    // Phase `power_menu_lock_enters_tilo_session_and_cuts_off_client_input`.
    // Lock from the visible power menu must enter Tilo's own session lifecycle,
    // rather than merely asking the surrounding login manager to lock.  Drive
    // the production shortcut and menu keyboard path, then prove both the
    // compositor state and a real Wayland client's focus/input boundary.
    {
        let compositor = Compositor::start();
        let mut client = TestClient::new(compositor.connect(), "org.tilo.test.power-menu-lock");
        client.assert_is_tilo_compositor();
        client.map_window(&compositor);
        client.clear_keyboard_trace();

        // Open Quick Settings through its configured default (Super+Q), then
        // Reverse-Tab from no hidden selection to the final root action, Power,
        // and open it. This remains stable as high-frequency controls are added
        // before the utility row;
        // the next Enter activates the default Lock row in the modal.
        // Each overlay opens on a later compositor turn than the key that asks
        // for it, so wait for it before sending the keys meant for it: sent
        // early, they reach whatever had focus before.
        compositor.modifiers(false, false, false, true);
        compositor.key("q");
        compositor.modifiers(false, false, false, false);
        wait_for_overlay(
            &compositor,
            "quick_settings",
            "Super+Q did not open Quick Settings",
        );
        client.wait_until(
            |state| !state.keyboard_focused,
            "Quick Settings left the real Wayland client focused",
        );
        compositor.modifiers(false, true, false, false);
        compositor.key("Tab");
        compositor.modifiers(false, false, false, false);
        compositor.key("Enter");
        wait_for_overlay(
            &compositor,
            "power_menu",
            "Enter on Power did not open the power menu",
        );
        compositor.key("Enter");

        let started = Instant::now();
        loop {
            let state = compositor.control(&serde_json::json!({ "op": "state_dump" }));
            if state
                .get("state")
                .and_then(|state| state.get("chrome"))
                .and_then(|chrome| chrome.get("session_locked"))
                .and_then(serde_json::Value::as_bool)
                == Some(true)
            {
                break;
            }
            assert!(
                started.elapsed() < deadline(),
                "power-menu Lock did not enter the Tilo locked state. {}",
                compositor.debug_state()
            );
            std::thread::sleep(Duration::from_millis(20));
        }

        client.wait_until(
            |state| !state.keyboard_focused,
            "power-menu Lock left the real Wayland client focused",
        );
        // A post-lock key is routed only to the lock controller.  No menu or
        // shortcut input from the whole sequence may leak to the hidden client.
        compositor.key("x");
        client.roundtrip();
        assert!(
            client.state.keyboard_events.is_empty(),
            "power-menu Lock leaked input to the hidden client: {:?}",
            client.state.keyboard_events
        );
    }
}

/// The desktop's core lifecycle must be true at the protocol boundary, not
/// only after directly mutating a `WorkspaceTree`: two actual xdg clients
/// sharing an app-id remain separate; real shortcut input navigates and
/// reorders their groups; close reaches the selected client's `xdg_toplevel`;
/// then that client's own destroy, unrelated chrome revisions, and a fresh
/// compositor process all keep the selected surface absent.
#[allow(
    clippy::too_many_lines,
    reason = "the regression test intentionally keeps its full real-client lifecycle in one scenario"
)]
#[test]
fn real_same_app_windows_close_exact_surface_without_resurrection_after_restart() {
    let mut compositor = Compositor::start();
    let app_id = "org.tilo.test.same-app-lifecycle";

    // NewGroup is the production pending-placement path.  Use it twice for
    // the same app-id: app-id identity is intentionally insufficient to
    // identify one of an application's windows.
    compositor.place_next_window(app_id);
    let mut first = TestClient::new(compositor.connect(), app_id);
    first.assert_is_tilo_compositor();
    first.map_window(&compositor);
    let first_protocol_id = u64::from(
        first
            .surface
            .as_ref()
            .expect("first client surface")
            .id()
            .protocol_id(),
    );
    let first_ids = wait_for_foreign_surface_count(
        &compositor,
        app_id,
        1,
        "the first mapped client did not contribute exactly one foreign surface",
    );
    let first_surface = first_ids
        .first()
        .copied()
        .expect("the first mapped client's compositor-global surface id");

    compositor.place_next_window(app_id);
    let mut selected = TestClient::new(compositor.connect(), app_id);
    selected.assert_is_tilo_compositor();
    selected.map_window(&compositor);
    let selected_protocol_id = u64::from(
        selected
            .surface
            .as_ref()
            .expect("selected client surface")
            .id()
            .protocol_id(),
    );
    assert_eq!(
        first_protocol_id, selected_protocol_id,
        "independent Wayland clients may reuse the same protocol-local object id"
    );

    let initial_ids = wait_for_foreign_surface_count(
        &compositor,
        app_id,
        2,
        "two mapped windows from one app-id did not occupy distinct foreign tabs",
    );
    let selected_surface = *initial_ids
        .iter()
        .find(|surface| **surface != first_surface)
        .expect("the second client receives its own compositor-global surface id");
    assert_ne!(
        first_surface, selected_surface,
        "the compositor must namespace equal protocol ids by client connection"
    );
    first.present(&compositor, first_surface);
    selected.present(&compositor, selected_surface);
    let initial = compositor.workspace_tree();

    // These go through the native test-control executor, its real
    // `WindowEvent` dispatch, and the per-frame compositor reconciliation.
    // They are deliberately not calls to the host's navigation/reorder model.
    let active_before_super = initial.get("active_group").cloned();
    compositor.modifiers(false, false, false, true);
    compositor.key("ArrowLeft");
    compositor.modifiers(false, false, false, false);
    let after_super = compositor.workspace_tree();
    assert_ne!(
        after_super.get("active_group").cloned(),
        active_before_super,
        "Super+Left must navigate away from the focused real client group"
    );

    let active_before_alt = after_super.get("active_group").cloned();
    compositor.modifiers(false, false, true, false);
    compositor.key("Tab");
    compositor.modifiers(false, false, false, false);
    let after_alt = compositor.workspace_tree();
    assert_ne!(
        after_alt.get("active_group").cloned(),
        active_before_alt,
        "Alt+Tab must traverse a live window group rather than only acknowledge input"
    );

    // Let the client itself tell us where it is on screen, then click it to
    // make the later close target unambiguous after both navigation paths.
    let point = selected.pointer_target(&compositor);
    compositor.click_at(point.0, point.1);
    selected.wait_until(
        |state| state.keyboard_focused,
        "pointer focus did not return to the selected same-app client",
    );

    let order_before = group_order(&compositor.workspace_tree());
    compositor.modifiers(false, true, false, true);
    compositor.key("ArrowLeft");
    compositor.modifiers(false, false, false, false);
    assert_ne!(
        group_order(&compositor.workspace_tree()),
        order_before,
        "Super+Shift+Left must reorder the focused real client's group"
    );

    // Re-read the real client's pointer enter after layout changed, then use
    // the actual close shortcut.  The close event has no protocol ack; the
    // selected client proves receipt and cooperates by destroying its objects.
    let point = selected.pointer_target(&compositor);
    compositor.click_at(point.0, point.1);
    selected.wait_until(
        |state| state.keyboard_focused,
        "selected client lost focus before its close shortcut",
    );
    compositor.modifiers(false, false, false, true);
    compositor.key("w");
    compositor.modifiers(false, false, false, false);
    selected.wait_until(
        |state| state.toplevel_close_requested,
        "Super+W did not reach the selected xdg_toplevel.close",
    );
    first.roundtrip();
    assert!(
        !first.state.toplevel_close_requested,
        "closing one window must not address its same-app sibling"
    );
    selected.destroy_toplevel();

    let wait_for_tree = |want: &[u64], label: &str| {
        let started = Instant::now();
        loop {
            let got = foreign_surface_ids(&compositor.workspace_tree(), app_id);
            if got == want {
                return;
            }
            assert!(
                started.elapsed() < deadline(),
                "{label}; wanted {want:?}, got {got:?}.\n{}",
                compositor.debug_state()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    wait_for_tree(
        &[first_surface],
        "destroying the selected client did not reap exactly its foreign tab",
    );

    // A chrome-only revision exercises the native frame loop's foreign-tab
    // mirror while the first same-app client remains live.  It used to be the
    // edge that reinserted a still-closing foreign surface.
    compositor.modifiers(false, false, false, true);
    compositor.key("Tab");
    compositor.modifiers(false, false, false, false);
    compositor.key("Escape");
    wait_for_tree(
        &[first_surface],
        "an unrelated revision/resync resurrected the selected closed surface",
    );

    // The actual TILO_STATE_PATH must contain no process-local Wayland ids.
    // This read uses the production persistence decoder, not a raw string
    // search. Live exact-close durability was established above while the
    // innocent sibling remained mapped; restart safety is the separate rule
    // that neither live surface id may be serialized as restorable layout.
    let persisted = shell::shell::persist::load_tree_from(&compositor.state_path)
        .expect("the close must atomically persist a reloadable WorkspaceTree");
    let persisted = serde_json::to_value(persisted).expect("serialize persisted tree for probe");
    assert_eq!(
        foreign_surface_ids(&persisted, app_id),
        Vec::<u64>::new(),
        "TILO_STATE_PATH must strip every process-local foreign surface binding"
    );

    compositor.restart_headless();
    let wait_selected_absent = |label: &str| {
        let started = Instant::now();
        loop {
            let got = foreign_surface_ids(&compositor.workspace_tree(), app_id);
            if !got.contains(&selected_surface) {
                return;
            }
            assert!(
                started.elapsed() < deadline(),
                "{label}; selected {selected_surface:?} reappeared in {got:?}.\n{}",
                compositor.debug_state()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    wait_selected_absent("the restarted native compositor resurrected a durably closed surface");

    // Reconnect a fresh process from the same application after native
    // startup.  Whether startup prunes the now-disconnected old sibling or
    // keeps its persisted placeholder is an independent policy; neither may
    // manufacture the selected surface id, and a new same-app window must be
    // adopted as its own new protocol surface.
    let mut survivor = TestClient::new(compositor.connect(), app_id);
    survivor.assert_is_tilo_compositor();
    survivor.map_window(&compositor);
    let survivor_protocol_id = u64::from(
        survivor
            .surface
            .as_ref()
            .expect("reconnected same-app surface")
            .id()
            .protocol_id(),
    );
    let after_reconnect = wait_for_foreign_surface_count(
        &compositor,
        app_id,
        1,
        "the fresh same-app client was not adopted after restart",
    );
    let survivor_surface = after_reconnect
        .first()
        .copied()
        .expect("the reconnected client's compositor-global surface id");
    assert_eq!(
        survivor_surface & u64::from(u32::MAX),
        survivor_protocol_id,
        "the global identity must retain the client's protocol-local low bits"
    );
    assert!(
        after_reconnect.contains(&survivor_surface) && !after_reconnect.contains(&selected_surface),
        "a same-app reconnect must add its new surface without resurrecting the selected closed one; got {after_reconnect:?}"
    );
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn toolkit_globals_are_advertised_and_privileged_protocols_stay_socket_only() {
    // Phase `compositor_advertises_the_globals_a_toolkit_requires`.
    // A real toolkit must find every global it treats as mandatory — GTK
    // refuses the display outright (stopping every GTK program) if
    // `wl_data_device_manager` is missing. Tilo's own toolkit never asks for
    // it, so this asserts the full set a foreign toolkit needs.
    {
        let compositor = Compositor::start();
        let mut client = TestClient::new(compositor.connect(), "org.tilo.test.toolkit");
        client.assert_is_tilo_compositor();

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

    // Phase `data_control_is_privileged_socket_only`.
    // `wlr-data-control` is the clipboard-manager protocol: binding it reads
    // every selection on the seat, no focus or user interaction required.
    // Must be invisible on the public (sandbox-bind-mounted) socket and
    // present on the privileged one — asserted in both directions, since only
    // checking absence would also pass if the global never existed.
    {
        let compositor = Compositor::start();

        let mut sandboxed = TestClient::new(compositor.connect(), "org.tilo.test.sandboxed");
        sandboxed.assert_is_tilo_compositor();
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
            "org.tilo.test.clipboard-manager",
        );
        privileged.assert_is_tilo_compositor();
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

    // Phase `session_lock_is_privileged_socket_only`.
    // Session-lock is an authentication boundary, just like clipboard-manager
    // access: ordinary applications must not be able to acquire a compositor
    // lock and later release it without passing Tilo authentication.
    {
        let compositor = Compositor::start();

        let mut public = TestClient::new(compositor.connect(), "org.tilo.test.untrusted-locker");
        public.assert_is_tilo_compositor();
        assert!(
            !public.has_global("ext_session_lock_manager_v1"),
            "session-lock is visible on the public socket; any app could acquire and release the desktop lock. Globals: {:?}",
            public.global_names(),
        );

        let mut trusted = TestClient::new(
            compositor.connect_privileged(),
            "org.tilo.test.session-locker",
        );
        trusted.assert_is_tilo_compositor();
        assert!(
            trusted.has_global("ext_session_lock_manager_v1"),
            "session-lock is missing from the privileged session socket. Globals: {:?}",
            trusted.global_names(),
        );
    }
}

#[test]
fn primary_selection_crosses_every_boundary_without_aliasing_the_clipboard() {
    // Phase `primary_selection_crosses_between_processes_and_does_not_alias_the_clipboard`.
    // Middle-click paste: the primary selection is a separate buffer from the
    // clipboard, and both must survive the trip between processes.
    {
        let compositor = Compositor::start();
        let clipboard_payload = "this went on the clipboard";
        let primary_payload = "this was drag-selected";

        let mut source = TestClient::new(compositor.connect(), "org.tilo.test.primary-source");
        source.assert_is_tilo_compositor();
        assert!(
            source.has_global("zwp_primary_selection_device_manager_v1"),
            "the compositor must advertise the primary-selection manager; globals: {:?}",
            source.global_names()
        );
        source.map_window(&compositor);
        source.set_clipboard(clipboard_payload);
        source.set_primary(primary_payload);

        let mut sink = TestClient::new(compositor.connect(), "org.tilo.test.primary-sink");
        sink.assert_is_tilo_compositor();
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

    // Phase `primary_selection_crosses_from_a_client_to_in_process_chrome`.
    // The primary selection also has to reach in-process chrome, since the
    // terminal that consumes it on middle-click runs inside the compositor.
    {
        let compositor = Compositor::start();
        let payload = "primary selection from a client";

        let mut client = TestClient::new(compositor.connect(), "org.tilo.test.primary-chrome");
        client.assert_is_tilo_compositor();
        client.map_window(&compositor);
        client.set_primary(payload);

        let started = Instant::now();
        let mut seen = None;
        while started.elapsed() < deadline() {
            seen = compositor.chrome_paste(true);
            if seen.as_deref() == Some(payload) {
                break;
            }
            // Serve the compositor's `send` request as soon as it arrives.
            dispatch_any(&mut [&mut client], None, Duration::from_millis(5));
        }
        assert_eq!(
            seen.as_deref(),
            Some(payload),
            "in-process chrome never saw the client's primary selection"
        );
    }

    // Phase `primary_selection_crosses_from_in_process_chrome_to_a_client`.
    // In-process chrome's primary selection must reach clients too, and must not
    // disturb the clipboard.
    {
        let compositor = Compositor::start();

        let mut client = TestClient::new(compositor.connect(), "org.tilo.test.primary-out");
        client.assert_is_tilo_compositor();
        client.map_window(&compositor);

        compositor.chrome_copy("chrome clipboard", false);
        compositor.chrome_copy("chrome primary", true);

        assert_eq!(client.read_primary(None), "chrome primary");
        assert_eq!(client.read_clipboard(None), "chrome clipboard");
    }
}

/// Drag text from one client onto the outer edge of another tiled window.
/// The shell owns that visible placement zone, materialises the payload, and
/// opens it in a new sibling group rather than leaking the offer to the app
/// below. The pointer is driven through the compositor's real input path, so
/// `start_drag` rides a genuine implicit grab rather than an invented serial.
#[test]
fn client_text_drag_on_a_tiled_edge_opens_a_new_group() {
    let compositor = Compositor::start();
    let payload = "dragged between two clients";

    // Both windows have to be on screen at once for a drag to cross between
    // them, and in a scrollable-strip desktop that means separate window
    // groups rather than two tabs of one.
    compositor.place_next_window("org.tilo.test.dnd-source");
    let mut source = TestClient::new(compositor.connect(), "org.tilo.test.dnd-source");
    source.assert_is_tilo_compositor();
    source.map_window(&compositor);

    compositor.place_next_window("org.tilo.test.dnd-target");
    let mut target = TestClient::new(compositor.connect(), "org.tilo.test.dnd-target");
    target.assert_is_tilo_compositor();
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
    let before = tree_shape(&compositor.workspace_tree());

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

    // `pointer_target` finds the first client pixel, which lies in the large
    // left outer band. Moving there must therefore select the inter-window
    // target before the tiled client gets a Wayland offer.
    compositor.pointer_move(target_point.0, target_point.1);
    for _ in 0..25 {
        target.pump();
        source.pump();
        assert!(
            !target.state.dnd_entered && target.state.dnd_offer.is_none(),
            "a shell-owned tiled edge must not offer the payload to the client below"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    compositor.pointer_button(false);
    let started = Instant::now();
    let after = loop {
        source.pump();
        target.pump();
        let tree = compositor.workspace_tree();
        let shape = tree_shape(&tree);
        if shape.0 == before.0 + 1
            && shape.1 == before.1 + 1
            && shape.2.iter().any(|title| {
                !before.2.contains(title)
                    && Path::new(title)
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("txt"))
            })
        {
            break (shape, tree);
        }
        assert!(
            started.elapsed() < deadline(),
            "the shell never opened the dragged text at the tiled edge.\n{}",
            compositor.debug_state()
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(
        after.0.0,
        before.0 + 1,
        "the outer edge must create a sibling group for dragged text"
    );
    let opened_title = after
        .0
        .2
        .iter()
        .find(|title| {
            !before.2.contains(title)
                && Path::new(title)
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("txt"))
        })
        .expect("the materialised text tab has a .txt title");
    let (_, opened_group_titles) = group_for_tab_title(&after.1, opened_title)
        .expect("the materialised text belongs to a durable window group");
    assert_eq!(
        opened_group_titles,
        [opened_title.as_str()],
        "the outer edge creates a singleton sibling group"
    );
}

impl Compositor {
    /// Ask the private test-control transport to mint the exact external
    /// activation token a user-selected host launch receives.  The token is
    /// passed only to the throwaway client below, never to the user's session.
    fn mint_host_launch_activation(&self, desktop_id: &str, display_name: &str) -> String {
        let resp = self.control(&serde_json::json!({
            "op": "mint_host_launch_activation",
            "desktop_id": desktop_id,
            "display_name": display_name,
        }));
        resp.get("answer")
            .and_then(|answer| answer.get("activation_token"))
            .and_then(serde_json::Value::as_str)
            .map_or_else(
                || panic!("mint_host_launch_activation failed: {resp:?}"),
                str::to_string,
            )
    }

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

    fn set_group_width(&self, permille: u16) {
        let resp = self.control(&serde_json::json!({
            "op": "set_group_width", "permille": permille,
        }));
        assert!(resp.get("ok").is_some(), "set_group_width failed: {resp:?}");
    }

    fn seed_files_drag(&self, path: &Path) {
        let resp = self.control(&serde_json::json!({
            "op": "seed_files_drag",
            "paths": [path],
        }));
        assert!(resp.get("ok").is_some(), "seed_files_drag failed: {resp:?}");
    }

    fn pointer_move(&self, x: f64, y: f64) {
        let resp = self.control(&serde_json::json!({"op": "pointer_move", "x": x, "y": y}));
        assert!(resp.get("ok").is_some(), "pointer_move failed: {resp:?}");
    }

    fn pointer_button(&self, pressed: bool) {
        let resp = self.control(&serde_json::json!({"op": "pointer_button", "pressed": pressed}));
        assert!(resp.get("ok").is_some(), "pointer_button failed: {resp:?}");
    }

    /// Feed one direct-touch hardware edge through the explicit test-control
    /// socket. The native adapter uses DRM's real ownership arena before it
    /// forwards anything to the Wayland seat.
    fn touch(&self, device: &str, slot: u32, phase: &str, x: f64, y: f64) {
        let resp = self.control(&serde_json::json!({
            "op": "touch",
            "device": device,
            "slot": slot,
            "phase": phase,
            "x": x,
            "y": y,
        }));
        assert!(resp.get("ok").is_some(), "touch {phase:?} failed: {resp:?}");
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
        while started.elapsed() < deadline() {
            if let Some([x, y, w, h]) = self.named_rect(name)
                && w > 0.0
                && h > 0.0
            {
                return (x + w / 2.0, y + h / 2.0);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!(
            "chrome never laid out a target named {name:?} (waited out the hang bound).\n{}",
            self.debug_state()
        );
    }

    /// Every laid-out target whose name `pick` admits, with its rect.
    fn target_rects(&self, pick: impl Fn(&str) -> bool) -> Vec<(String, [f64; 4])> {
        let resp = self.control(&serde_json::json!({"op": "list_targets"}));
        let mut rects: Vec<(String, [f64; 4])> = resp
            .get("targets")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|t| {
                let name = t.get("name")?.as_str()?;
                if !pick(name) {
                    return None;
                }
                let r = t.get("rect")?.as_array()?;
                Some((
                    name.to_string(),
                    [
                        r.first()?.as_f64()?,
                        r.get(1)?.as_f64()?,
                        r.get(2)?.as_f64()?,
                        r.get(3)?.as_f64()?,
                    ],
                ))
            })
            .collect();
        rects.sort_by(|a, b| a.0.cmp(&b.0));
        rects
    }

    /// The on-screen rects of every laid-out client surface.
    fn foreign_surface_rects(&self) -> Vec<[f64; 4]> {
        self.target_rects(|name| name.starts_with("foreign-surface-"))
            .into_iter()
            .map(|(_, rect)| rect)
            .filter(|[_, _, w, h]| *w > 0.0 && *h > 0.0)
            .collect()
    }

    /// Wait until the targets `pick` admits stop moving: the same names at
    /// the same rects for [`SETTLED_SAMPLES`] samples about a frame apart.
    /// A position read mid-animation is clicked where the target used to be.
    fn wait_for_layout_to_settle(&self, pick: impl Fn(&str) -> bool, what: &str) {
        let started = Instant::now();
        let mut last = self.target_rects(&pick);
        let mut equal = 1usize;
        while equal < SETTLED_SAMPLES {
            assert!(
                started.elapsed() < deadline(),
                "{what}: the layout never stopped moving; last {last:?}.\n{}",
                self.debug_state()
            );
            std::thread::sleep(LAYOUT_SAMPLE);
            let next = self.target_rects(&pick);
            equal = if next == last {
                equal.saturating_add(1)
            } else {
                1
            };
            last = next;
        }
    }

    /// Wait until `cond` holds on the compositor's control state, failing
    /// with `what` and the compositor's state after [`deadline()`].
    fn wait_for(&self, what: &str, mut cond: impl FnMut(&Self) -> bool) {
        let started = Instant::now();
        while !cond(self) {
            assert!(
                started.elapsed() < deadline(),
                "{what} (waited out the hang bound).\n{}",
                self.debug_state()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
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

/// An opaque, deliberately multicolour PNG. Besides protocol byte equality it
/// makes the compositor's real-pixel drag preview inspectable in the mid-drag
/// screenshot; the old transparent 1x1 fixture could only prove a blank card.
fn visible_png() -> Vec<u8> {
    let pixels = image::RgbaImage::from_fn(24, 16, |x, y| {
        let left = x < 12;
        let top = y < 8;
        match (left, top) {
            (true, true) => image::Rgba([239, 68, 68, 255]),
            (false, true) => image::Rgba([59, 130, 246, 255]),
            (true, false) => image::Rgba([34, 197, 94, 255]),
            (false, false) => image::Rgba([250, 204, 21, 255]),
        }
    });
    let mut encoded = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(pixels)
        .write_to(&mut encoded, image::ImageFormat::Png)
        .expect("encode visible PNG fixture");
    encoded.into_inner()
}

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

/// Narrow the demo group so a client stays visible while the Files tab is
/// focused (the demo group is laid out at full output width). Split out of
/// the drag-and-drop tests below to keep each one short.
fn narrow_demo_group_for_files(compositor: &Compositor) {
    // DnD is the contract under test. Give both groups a deterministic
    // quarter-strip width through the same live + persisted width setter the
    // resize gesture commits, avoiding resize hit-testing as an unrelated
    // precondition for reaching the source row.
    compositor.set_group_width(250);
    compositor.wait_for_layout_to_settle(
        |name| {
            name.starts_with("tab:")
                || name.starts_with("file_row:")
                || name.starts_with("foreign-surface-")
        },
        "the resized groups",
    );
    compositor.snapshot(Path::new("/tmp/tilo-dnd-e2e-after-resize.png"));
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
    compositor.snapshot(Path::new("/tmp/tilo-dnd-e2e-before-drag.png"));
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
    // Start in the middle of the visible row.  The deterministic group width
    // above leaves ample source area, so the drag can cross its threshold
    // before approaching either the sidebar or the neighbouring client.
    (x0 + (x1 - x0) / 2.0, y + h / 2.0)
}

/// Press on the file row and carry it into a tiled window's shell-owned drop
/// zone in small steps, crossing Files' own drag threshold before leaving its
/// tile. The destination client deliberately receives no Wayland offer: the
/// visible left/right body zones now mean place-and-open content as a tab.
fn drag_file_row_to_shell_target(
    compositor: &Compositor,
    target: &mut TestClient,
    path: &Path,
    row: (f64, f64),
    target_point: (f64, f64),
) {
    const STEPS: u32 = 12;
    compositor.pointer_move(row.0, row.1);
    compositor.pointer_button(true);
    // Files' widget tests own press-to-drag arming. Seed the exact production
    // slot here so this integration case isolates the compositor's hand-off
    // into a Wayland data-device drag while retaining a real chrome grab.
    compositor.seed_files_drag(path);
    // Cross the file row's drag threshold while still inside the row. The
    // client can be close enough vertically that interpolating straight to
    // it otherwise leaves the row on the very first motion event.
    // Test-control coordinates are physical while the hosted widget applies
    // the output scale factor. Move far enough to clear the 6 logical-pixel
    // threshold even on a 2x output, then interpolate from that latched point.
    // The fixture gives the row enough room to clear the strict 6 logical-pixel
    // threshold even when the physical test-control coordinates are scaled.
    // Once latched, the compositor can hand the live drag rightward.
    let latched = (row.0 - 32.0, row.1);
    compositor.pointer_move(latched.0, latched.1);
    target.pump();
    std::thread::sleep(Duration::from_millis(30));
    for i in 1..=STEPS {
        let t = f64::from(i) / f64::from(STEPS);
        compositor.pointer_move(
            latched.0 + (target_point.0 - latched.0) * t,
            latched.1 + (target_point.1 - latched.1) * t,
        );
        std::thread::sleep(Duration::from_millis(30));
    }
    compositor.snapshot(Path::new("/tmp/tilo-dnd-e2e-mid-drag.png"));
}

fn tree_shape(tree: &serde_json::Value) -> (usize, usize, Vec<String>) {
    let mut groups: usize = 0;
    let mut tabs: usize = 0;
    let mut titles = Vec::new();
    walk_json(tree, &mut |value| {
        if let Some(window_groups) = value.get("window_groups").and_then(|v| v.as_array()) {
            groups = groups.saturating_add(window_groups.len());
        }
        if let (Some(title), Some(_)) = (value.get("title"), value.get("tile")) {
            tabs = tabs.saturating_add(1);
            if let Some(title) = title.as_str() {
                titles.push(title.to_string());
            }
        }
    });
    (groups, tabs, titles)
}

/// Find the group containing a foreign app and return its durable id plus
/// the foreign app ids in tab order. Placeholder tabs are deliberately
/// omitted: this helper asserts the relative placement of real client
/// windows without coupling the interaction test to the demo group's
/// shell-owned starter tabs.
fn foreign_group_for_app(
    value: &serde_json::Value,
    wanted_app_id: &str,
) -> Option<(u64, Vec<String>)> {
    if let Some(tabs) = value.get("tabs").and_then(serde_json::Value::as_array) {
        let app_ids: Vec<String> = tabs
            .iter()
            .filter_map(|tab| {
                tab.get("tile")?
                    .get("Foreign")?
                    .get("app_id")?
                    .as_str()
                    .map(str::to_owned)
            })
            .collect();
        if app_ids.iter().any(|app_id| app_id == wanted_app_id) {
            let group_id = value.get("id")?.as_u64()?;
            return Some((group_id, app_ids));
        }
    }
    match value {
        serde_json::Value::Array(values) => values
            .iter()
            .find_map(|value| foreign_group_for_app(value, wanted_app_id)),
        serde_json::Value::Object(values) => values
            .values()
            .find_map(|value| foreign_group_for_app(value, wanted_app_id)),
        _ => None,
    }
}

/// Find the durable group containing a tab title and return every title in
/// that group. Hosted apps do not have a foreign process/app id, so their
/// persisted tab identity is the interaction-level oracle.
fn group_for_tab_title(
    value: &serde_json::Value,
    wanted_title: &str,
) -> Option<(u64, Vec<String>)> {
    if let Some(tabs) = value.get("tabs").and_then(serde_json::Value::as_array) {
        let titles: Vec<String> = tabs
            .iter()
            .filter_map(|tab| tab.get("title")?.as_str().map(str::to_owned))
            .collect();
        if titles.iter().any(|title| title == wanted_title) {
            return Some((value.get("id")?.as_u64()?, titles));
        }
    }
    match value {
        serde_json::Value::Array(values) => values
            .iter()
            .find_map(|value| group_for_tab_title(value, wanted_title)),
        serde_json::Value::Object(values) => values
            .values()
            .find_map(|value| group_for_tab_title(value, wanted_title)),
        _ => None,
    }
}

/// Drag a real image out of hosted Files and release on another tiled
/// window's body. This proves the new shell-owned behavior end to end: the
/// carried pixels remain visible, the underlying client is not offered the
/// payload, and release opens a new tab in that existing window group.
#[test]
fn a_file_dragged_out_of_hosted_files_opens_in_the_target_window_group() {
    let home = PathBuf::from(format!(
        "/run/user/{}/tilo-dnd-home-{}-{}",
        unsafe { libc::getuid() },
        std::process::id(),
        next_id(),
    ));
    std::fs::create_dir_all(&home).expect("create the fixture home");
    let photo = home.join("photo.png");
    std::fs::write(&photo, visible_png()).expect("write the fixture image");

    let compositor = Compositor::start_with(Some(&home), (2560, 900));

    compositor.place_next_window("org.tilo.test.dnd-target");
    let mut target = TestClient::new(compositor.connect(), "org.tilo.test.dnd-target");
    target.assert_is_tilo_compositor();
    target.map_window(&compositor);

    // Let the strip finish making room for the new group before reading
    // where the pills are; a pill read mid-animation is clicked where it
    // used to be.
    compositor.wait_for_layout_to_settle(
        |name| name.starts_with("tab:") || name.starts_with("foreign-surface-"),
        "the strip making room for the new group",
    );
    let files_pill = compositor.named_center("tab:Files");
    compositor.click_at(files_pill.0, files_pill.1);
    compositor.wait_for(
        &format!("clicking the Files pill at {files_pill:?} did not select the Files tab"),
        files_tab_is_active,
    );

    // The demo group is laid out at full output width, so with Files focused
    // the client is off-screen; narrow the demo group with the real gesture
    // (drag the seam handle) so the client stays visible while Files is
    // focused.
    narrow_demo_group_for_files(&compositor);
    // The body zone belongs to shell placement now, so deliberately resolve
    // it from the compositor's live foreign-surface geometry. Asking the
    // client to discover it via wl_pointer.enter would be circular: correct
    // shell ownership means that enter is intentionally withheld.
    let target_name = compositor
        .target_names()
        .into_iter()
        .find(|name| name.starts_with("foreign-surface-"))
        .expect("the mapped target has compositor geometry");
    let [target_x, target_y, target_w, target_h] = compositor
        .named_rect(&target_name)
        .expect("the mapped target has a live rectangle");
    // 62.5% is firmly in the right-side body target while remaining clear of
    // the deliberately generous outer 25% inter-group edge band.
    let target_point = (target_x + target_w * 0.625, target_y + target_h / 2.0);
    // The row the file manager laid out for the fixture file — aimed at
    // its on-screen part, since the tile may extend past the left edge.
    let row = locate_file_row(&compositor);
    let before_tree = compositor.workspace_tree();
    let before = tree_shape(&before_tree);
    let (target_group, before_foreign_order) =
        foreign_group_for_app(&before_tree, "org.tilo.test.dnd-target")
            .expect("the destination client belongs to a durable window group");
    assert_eq!(before_foreign_order, ["org.tilo.test.dnd-target"]);

    drag_file_row_to_shell_target(&compositor, &mut target, &photo, row, target_point);
    // The visible body zone belongs to the shell. Flush the foreign client
    // before release so this assertion would observe an accidental Wayland
    // handoff rather than merely checking an undrained queue.
    target.roundtrip();
    assert!(
        !target.state.dnd_entered && target.state.dnd_offer.is_none(),
        "a shell-owned place-and-open target must not offer the payload to the client"
    );
    compositor.pointer_button(false);
    let started = Instant::now();
    let after = loop {
        let tree = compositor.workspace_tree();
        let shape = tree_shape(&tree);
        if shape.1 == before.1 + 1 && group_for_tab_title(&tree, "photo.png").is_some() {
            break (shape, tree);
        }
        assert!(
            started.elapsed() < deadline(),
            "the shell never opened photo.png at the body target.\n{}",
            compositor.debug_state()
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(
        after.0.0, before.0,
        "an interior body drop joins the existing group rather than creating a new one"
    );
    let (opened_group, after_titles) = group_for_tab_title(&after.1, "photo.png")
        .expect("the opened image belongs to a durable window group");
    assert_eq!(
        opened_group, target_group,
        "the image must join the group under the pointer"
    );
    assert_eq!(
        after_titles,
        ["Dnd Target", "photo.png"],
        "a right-interior drop appends the opened image after the target tab"
    );

    assert!(
        photo.exists(),
        "opening by drag is a copy; Files must not move the source"
    );
    let _ = std::fs::remove_dir_all(&home);
}

/// Spawn the `word-processor` binary against the compositor under test,
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
        .env("TILO_PACKAGE_ID", "so.tilo.word-processor")
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

/// Whether the compositor shows client surface `surface_id`'s window.
fn foreign_frame_presented(compositor: &Compositor, surface_id: u64) -> bool {
    compositor
        .control(&serde_json::json!({ "op": "foreign_frames" }))
        .get("foreign_frames")
        .and_then(|payload| payload.get("frames"))
        .and_then(serde_json::Value::as_array)
        .is_some_and(|frames| {
            frames.iter().any(|frame| {
                frame.get("surface_id").and_then(serde_json::Value::as_u64) == Some(surface_id)
                    && frame.get("presented").and_then(serde_json::Value::as_bool) == Some(true)
            })
        })
}

/// Whether any client surface has presented a frame yet.
fn any_foreign_frame_presented(compositor: &Compositor) -> bool {
    compositor
        .control(&serde_json::json!({ "op": "foreign_frames" }))
        .get("foreign_frames")
        .and_then(|payload| payload.get("frames"))
        .and_then(serde_json::Value::as_array)
        .is_some_and(|frames| {
            frames.iter().any(|frame| {
                frame
                    .get("frame_seq")
                    .and_then(serde_json::Value::as_u64)
                    .is_some_and(|seq| seq > 0)
            })
        })
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
            started.elapsed() < deadline(),
            "the word processor never mapped a window.\nApp log:\n{}\nCompositor log:\n{}",
            app_log.lock().map(|l| l.join("\n")).unwrap_or_default(),
            compositor.recent_log()
        );
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// The same drag, dropped on the word processor. A live lane: starts
/// `target/debug/word-processor` as an unsandboxed client (needs a GPU to
/// render) and asserts its own report that it inserted the image.
/// Self-skips unless `TILO_LIVE_APP_DND=1` (`docs/testing-live.md`), so
/// hermetic CI stays green.
#[test]
fn a_file_dragged_out_of_the_hosted_file_manager_lands_in_the_word_processor() {
    const STEPS: u32 = 16;
    if std::env::var_os("TILO_LIVE_APP_DND").is_none() {
        eprintln!("skipping: set TILO_LIVE_APP_DND=1 to run the word-processor drop lane");
        return;
    }
    let word_processor =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../target/debug/word-processor");
    assert!(
        word_processor.exists(),
        "build it first: cargo build -p word-processor ({})",
        word_processor.display()
    );

    let home = PathBuf::from(format!(
        "/run/user/{}/tilo-dnd-wp-{}-{}",
        unsafe { libc::getuid() },
        std::process::id(),
        next_id(),
    ));
    std::fs::create_dir_all(&home).expect("create the fixture home");
    let photo = home.join("photo.png");
    std::fs::write(&photo, visible_png()).expect("write the fixture image");

    let compositor = Compositor::start_with_home(Some(&home));
    compositor.place_next_window("so.tilo.word-processor");

    let (mut app, app_log) = spawn_word_processor(&compositor, &word_processor, &home);
    wait_for_word_processor_mapped(&compositor, &app_log);
    // Let it present its first frame, so the tile has a real size.
    compositor.wait_for(
        "the word processor never presented a frame",
        any_foreign_frame_presented,
    );

    // Same recipe as the client test: select Files, narrow the demo group
    // by its seam handle so the app stays on screen while Files is focused.
    compositor.wait_for_layout_to_settle(
        |name| name.starts_with("tab:") || name.starts_with("foreign-surface-"),
        "the strip making room for the word processor",
    );
    let files_pill = compositor.named_center("tab:Files");
    compositor.click_at(files_pill.0, files_pill.1);
    compositor.wait_for(
        "clicking the Files pill did not select the Files tab",
        files_tab_is_active,
    );
    let seam = compositor.named_center("group_resize_handle[0]");
    compositor.pointer_move(seam.0, seam.1);
    compositor.pointer_button(true);
    for i in 1..=10 {
        compositor.pointer_move(seam.0 - 1100.0 * (f64::from(i) / 10.0), seam.1);
        std::thread::sleep(Duration::from_millis(30));
    }
    compositor.pointer_button(false);
    compositor.wait_for_layout_to_settle(
        |name| name.starts_with("file_row:") || name.starts_with("foreign-surface-"),
        "the resized groups",
    );
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
        if started.elapsed() > deadline() {
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
