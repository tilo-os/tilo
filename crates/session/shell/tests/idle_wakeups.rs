use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use getrandom::{SysRng, rand_core::UnwrapErr};
use threshold_identity::ThresholdIdentity;
use tilo_identity_ceremony::IdentityRecord;

/// How long the desktop is left alone after both apps are drawn, so startup
/// work (font caches, record flushes, the first save) is behind it.
pub(crate) const SETTLE: Duration = Duration::from_secs(5);

/// Match the native session's launcher-less record scope explicitly, so
/// inherited package or workspace variables cannot redirect the documents
/// this fixture seeds.
const FIXTURE_RECORD_SCOPE: &str = "00000000-0000-0000-0000-000000000000";

/// How many desktops this process started, so each has its own runtime dir.
static STARTED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// A line the compositor printed, with when the test read it.
pub(crate) type Log = Arc<Mutex<Vec<(Instant, String)>>>;

pub(crate) struct Desktop {
    child: Child,
    control: PathBuf,
    runtime_dir: PathBuf,
    log: Log,
    door: bool,
}

/// The shell Cargo built beside an integration test, where the run put it.
fn built_shell() -> Option<std::ffi::OsString> {
    option_env!("CARGO_BIN_EXE_shell")
        .map(|built| tilo_test_env::package_binary_at("shell", built).into_os_string())
}

impl Desktop {
    /// The headless compositor on a fresh private runtime, signed in, with a
    /// Notes tab saved in its session; returns once its control socket
    /// answers with a laid-out frame.
    pub(crate) fn start() -> Self {
        Self::start_with(notes_desktop())
    }

    /// The same with `tree` as the saved session (`idle_memory` starts an
    /// empty desktop).
    pub(crate) fn start_with(tree: shell::shell::state::WorkspaceTree) -> Self {
        Self::start_configured(tree, false)
    }

    /// [`Self::start_with`], serving the agent door at
    /// `tilo-session/agent.sock` in its runtime directory (where a client
    /// inside the login looks) when `door`.
    pub(crate) fn start_with_door(tree: shell::shell::state::WorkspaceTree, door: bool) -> Self {
        Self::start_configured(tree, door)
    }

    fn start_configured(tree: shell::shell::state::WorkspaceTree, door: bool) -> Self {
        tilo_test_env::require_isolated_desktop();
        // SUN_LEN caps a unix socket path, so the runtime dir stays short.
        // One per desktop: this module's tests run beside each other.
        let runtime_dir = PathBuf::from(format!(
            "/run/user/{}/tilo-idle-{}-{}",
            rustix::process::getuid().as_raw(),
            std::process::id(),
            STARTED.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        ));
        let _ = std::fs::remove_dir_all(&runtime_dir);
        std::fs::create_dir_all(&runtime_dir).expect("create the private runtime dir");
        // A login always has its home: startup resolves it as the trusted
        // root of the settings copy and refuses to start without it.
        std::fs::create_dir_all(runtime_dir.join("home")).expect("create the session's home");
        let state_path = runtime_dir.join("state.json");
        shell::shell::persist::save_tree_to(&tree, &state_path).expect("seed the saved session");
        seed_tab_records(&runtime_dir, &tree);
        // Saving made this tree the state file's writer: it holds the file's
        // `.writer-lock` until it is dropped. The compositor is the only writer
        // of the session it restores, so release it before that starts, or the
        // compositor runs without its durable owner ("file lease is busy").
        drop(tree);
        seed_identity(&runtime_dir);
        let (child, log) = Self::spawn_compositor(&runtime_dir, door);
        let desktop = Self {
            child,
            control: runtime_dir.join("ctl"),
            runtime_dir,
            log,
            door,
        };
        desktop.wait_started();
        desktop
    }

    /// End the compositor as the session ends it (SIGTERM) and start it
    /// again on the same login: its saved session, records and identity.
    pub(crate) fn restart(&mut self) {
        let pid = rustix::process::Pid::from_child(&self.child);
        rustix::process::kill_process(pid, rustix::process::Signal::TERM)
            .expect("ask the compositor to end");
        self.child.wait().expect("the compositor ends");
        let (child, log) = Self::spawn_compositor(&self.runtime_dir, self.door);
        self.child = child;
        self.log = log;
        self.wait_started();
    }

    fn spawn_compositor(runtime_dir: &Path, door: bool) -> (Child, Log) {
        let state_path = runtime_dir.join("state.json");
        let identity_root = runtime_dir.join("identity-root");
        let control = runtime_dir.join("ctl");

        // Cargo supplies this at compile time for integration tests, not
        // examples, so it is read with `option_env!`: the idle-wakeups example
        // includes this file and must build without it. Scheduled and A/B runs
        // explicitly select the shell they built.
        let shell = std::env::var_os("TILO_IDLE_SHELL_BIN")
            .or_else(built_shell)
            .expect("set TILO_IDLE_SHELL_BIN to the explicitly built shell binary");
        let mut command = Command::new(shell);
        // A person's session keeps GStreamer's plugin registry in its cache.
        // Share one across this run's test processes (tools/test-env.sh), so
        // only the first pays for the scan.
        if let Some(cache) = std::env::var_os("TILO_TEST_CACHE_DIR") {
            command.env(
                "GST_REGISTRY_1_0",
                Path::new(&cache).join("gst-registry.bin"),
            );
        }
        command
            .args(["--mode", "native", "--backend", "headless"])
            .env("XDG_RUNTIME_DIR", runtime_dir)
            .env("XDG_CONFIG_HOME", runtime_dir.join("config"))
            .env("XDG_DATA_HOME", runtime_dir.join("data"))
            .env("XDG_STATE_HOME", runtime_dir.join("state"))
            .env("XDG_CACHE_HOME", runtime_dir.join("cache"))
            .env("HOME", runtime_dir.join("home"))
            .env("TILO_PACKAGE_ID", shell::shell::SHELL_RECORDS_PACKAGE)
            .env("TILO_WORKSPACE_ID", FIXTURE_RECORD_SCOPE)
            .env("TILO_WORKSPACE_ROOTS", "")
            .env("TILO_STATE_PATH", &state_path)
            .env("TILO_IDENTITY_ROOT", &identity_root)
            .env("TILO_TEST_CONTROL", &control)
            .env("TILO_HEADLESS_SIZE", "1920x1080")
            // Nobody touches it, so it must not lock or blank in the
            // window either: those are state changes, not idling.
            .env("TILO_IDLE_LOCK_SECS", "0")
            .env("TILO_IDLE_BLANK_SECS", "0")
            // A `paint-tick:` line per loop iteration (see `LoopStats`).
            .env("TILO_PAINT_STATS", "ticks")
            // Every round of channel-key questions is logged, so the gate counts
            // the rounds the shovel starts, not how often contention woke it.
            .env("RUST_LOG", "warn,tilo::collab::key_rounds=debug")
            // As the session starts the compositor (os/usr/libexec/tilo-session):
            // glibc reads its tunables when a process starts.
            .env(
                "GLIBC_TUNABLES",
                launcher::hardening::NO_HUGE_PAGES_TUNABLES,
            );
        if door {
            command.env(
                "TILO_AGENT_DOOR",
                runtime_dir.join("tilo-session/agent.sock"),
            );
        }
        let mut child = command
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("DISPLAY")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn the compositor under test");
        let log: Log = Arc::default();
        for pipe in [
            child
                .stdout
                .take()
                .map(|out| Box::new(out) as Box<dyn std::io::Read + Send>),
            child
                .stderr
                .take()
                .map(|err| Box::new(err) as Box<dyn std::io::Read + Send>),
        ]
        .into_iter()
        .flatten()
        {
            // Unread output would block the compositor mid-write.
            let log = Arc::clone(&log);
            std::thread::spawn(move || {
                for line in BufReader::new(pipe).lines().map_while(Result::ok) {
                    if let Ok(mut log) = log.lock() {
                        log.push((Instant::now(), line));
                    }
                }
            });
        }
        (child, log)
    }

    fn wait_started(&self) {
        self.wait_for("a laid-out first frame", |targets| !targets.is_empty());
        // A compositor that cannot take its own state file's writer lease runs
        // without its durable owner and its collaboration service: nothing
        // published to the broker, and every app that waits for it stays dark.
        let busy = matching(&self.log, &["file lease is busy"], 3);
        assert!(
            busy.is_empty(),
            "the compositor found its state file's writer lease taken:\n{busy}"
        );
    }

    /// The control socket.
    pub(crate) fn control_path(&self) -> &Path {
        &self.control
    }

    /// The private runtime directory the compositor runs in.
    pub(crate) fn runtime_dir(&self) -> &Path {
        &self.runtime_dir
    }

    pub(crate) fn pid(&self) -> u32 {
        self.child.id()
    }

    #[allow(
        dead_code,
        reason = "used by the explicitly built scheduled idle benchmark"
    )]
    pub(crate) fn log_since(&self, since: Instant) -> Vec<(Instant, String)> {
        self.log
            .lock()
            .map(|log| log.iter().filter(|(at, _)| *at >= since).cloned().collect())
            .unwrap_or_default()
    }

    /// Whether the compositor has printed a line containing `needle`.
    pub(crate) fn logged(&self, needle: &str) -> bool {
        self.log
            .lock()
            .is_ok_and(|log| log.iter().any(|(_, line)| line.contains(needle)))
    }

    /// The last lines the compositor printed, leaving out its per-iteration
    /// `paint-tick` lines, which the report summarises.
    /// The compositor's log, shared with the thread that fills it.
    pub(crate) fn log_handle(&self) -> Log {
        Arc::clone(&self.log)
    }

    pub(crate) fn log_tail(&self) -> String {
        self.log
            .lock()
            .map(|log| {
                let mut tail: Vec<&str> = log
                    .iter()
                    .rev()
                    .map(|(_, line)| line.as_str())
                    .filter(|line| !line.contains("paint-tick: "))
                    .take(60)
                    .collect();
                tail.reverse();
                tail.join("\n")
            })
            .unwrap_or_default()
    }

    /// One request on the control socket; `None` while it does not answer.
    pub(crate) fn try_control(&self, request: &serde_json::Value) -> Option<serde_json::Value> {
        let mut stream = UnixStream::connect(&self.control).ok()?;
        stream
            .set_read_timeout(Some(tilo_test_env::hang_bound()))
            .ok()?;
        let mut line = serde_json::to_vec(request).ok()?;
        line.push(b'\n');
        stream.write_all(&line).ok()?;
        let mut response = String::new();
        BufReader::new(stream).read_line(&mut response).ok()?;
        serde_json::from_str(&response).ok()
    }

    pub(crate) fn control(&self, request: &serde_json::Value) -> serde_json::Value {
        self.try_control(request).unwrap_or_else(|| {
            panic!(
                "the compositor did not answer {request}. Log:\n{}",
                self.log_tail()
            )
        })
    }

    fn target_names(&self) -> Vec<String> {
        self.try_control(&serde_json::json!({"op": "list_targets"}))
            .and_then(|reply| reply.get("targets").and_then(|t| t.as_array()).cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(|target| target.get("name")?.as_str().map(str::to_owned))
            .collect()
    }

    /// Wait until the laid-out targets satisfy `done`, failing with the log
    /// if the compositor exits or the hang bound runs out.
    pub(crate) fn wait_for(&self, what: &str, done: impl Fn(&[String]) -> bool) {
        let deadline = tilo_test_env::hang_deadline();
        loop {
            let targets = self.target_names();
            if done(&targets) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "waited for {what}; targets {targets:?}. Log:\n{}",
                self.log_tail()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// The last `count` lines of `log` holding any of `needles`.
pub(crate) fn matching(log: &Log, needles: &[&str], count: usize) -> String {
    log.lock()
        .map(|log| {
            let mut found: Vec<&str> = log
                .iter()
                .rev()
                .map(|(_, line)| line.as_str())
                .filter(|line| needles.iter().any(|needle| line.contains(needle)))
                .take(count)
                .collect();
            found.reverse();
            found.join("\n")
        })
        .unwrap_or_default()
}

impl Drop for Desktop {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!("compositor log tail:\n{}", self.log_tail());
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.runtime_dir);
    }
}

/// A signed-in person's desktop with one Notes notebook open, as a saved
/// session restores it.
pub(crate) fn notes_desktop() -> shell::shell::state::WorkspaceTree {
    let mut tree = shell::shell::state::WorkspaceTree::bootstrap_demo();
    // Keep the demo's first workspace with only its Notes tab; an untouched
    // demo tree would be migrated away as legacy sample data on load.
    tree.desktop.workspaces.truncate(1);
    let home = &mut tree.desktop.workspaces[0];
    home.name = "Idle desktop".into();
    home.collaborators.clear();
    let group = &mut home.strips[0].window_groups[0];
    group.tabs.retain(|tab| tab.title == "Notes");
    group.active_tab = group.tabs[0].id;
    tree
}

/// Create the saved tabs' documents before native startup admits only
/// existing ones, then release them so the compositor can open them.
fn seed_tab_records(runtime_dir: &Path, tree: &shell::shell::state::WorkspaceTree) {
    let mut owners = ui::record::RecordOwners::new(runtime_dir.join("data"));
    let records = owners
        .open(shell::shell::SHELL_RECORDS_PACKAGE, FIXTURE_RECORD_SCOPE)
        .expect("open the fixture's Shell record namespace");
    for tab in tree
        .desktop
        .workspaces
        .iter()
        .flat_map(|workspace| &workspace.strips)
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
    {
        shell::shell::tab_documents::create_scratch_tab_record(&records, tab.id)
            .expect("create the saved tab's document");
    }
    records
        .try_flush()
        .expect("persist the saved tabs' documents");
    drop(records);
    drop(owners);
}

/// A completed identity ceremony: without one the desktop is in first-run
/// onboarding, which is not the idle desktop a person leaves.
fn seed_identity(runtime_dir: &Path) -> PathBuf {
    let identity_dir = runtime_dir.join("config/tilo/identity");
    let identity = ThresholdIdentity::bootstrap_single_device(&mut UnwrapErr(SysRng))
        .expect("create the fixture identity");
    let record = IdentityRecord::from_threshold_identity(
        &identity,
        "Idle test",
        [128, 128, 128],
        SystemTime::UNIX_EPOCH,
    )
    .expect("encode the fixture identity");
    let bytes = record
        .to_json_bytes()
        .expect("serialize the fixture identity");
    let provider = runtime_identity::BrokerFileProvider::new(&identity_dir);
    runtime_identity::import_identity_with_provider(&identity_dir, "primary", &bytes, &provider)
        .expect("persist the fixture identity");
    runtime_dir.join("identity-root")
}

/// Whether a process named `name` runs in the compositor's tree.
pub(crate) fn running(desktop: &Desktop, name: &str) -> bool {
    tilo_test_env::idle::process_tree(desktop.pid())
        .expect("list the compositor's process tree")
        .into_iter()
        .any(|pid| {
            std::fs::read_to_string(format!("/proc/{pid}/comm"))
                .is_ok_and(|comm| comm.trim() == name)
        })
}

/// Wait until no process named `name` runs in the compositor's tree.
pub(crate) fn wait_until_gone(desktop: &Desktop, name: &str) {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        if !running(desktop, name) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{name} was still running after the hang bound. Log:\n{}",
            desktop.log_tail()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The Notes notebook the session restores publishes its actions where the
/// palette, search and agents find them. The session mounts its restored
/// hosted tabs while it builds the desktop, before it binds the
/// collaboration broker and identity service they connect to; the tab found
/// no socket, stayed offline and published nothing until it was reopened
/// (fixed by the session-services handshake).
#[test]
fn the_restored_notes_notebook_publishes_its_actions() {
    let desktop = Desktop::start();
    desktop.wait_for("the restored Notes notebook", |targets| {
        targets.iter().any(|name| name == "notes.first_note")
    });
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let reply = desktop.control(&serde_json::json!({
            "op": "actions_enumerate", "query": "New note", "detail": "compact", "wait_ms": 5000,
        }));
        let published = reply["answer"]["actions"].as_array().is_some_and(|rows| {
            rows.iter()
                .any(|row| row["application"] == "so.tilo.notes" && row["operation"] == "new-note")
        });
        if published {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the restored Notes tab never published new-note: {reply}\nLog:\n{}",
            desktop.log_tail()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}
