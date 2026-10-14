//! The service against real libsecret: `secret-tool` and a
//! `gi.repository.Secret` client talk to the built `tilo-secret-service`
//! over a private session bus.
//!
//! The bus is a `dbus-daemon` of this test's own, with its own runtime,
//! home and data directories and a development device identity; nothing
//! reaches the person's session bus or keyring. The tools must exist: a
//! check that cannot observe its subject fails (CI installs them with
//! `tools/ci/ensure-session-bus-tools.sh`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    clippy::arithmetic_side_effects,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const SERVICE: &str = env!("CARGO_BIN_EXE_tilo-secret-service");
const NAME: &str = "org.freedesktop.secrets";

/// A private session bus and the directories every client of it uses.
struct Session {
    root: tempfile::TempDir,
    bus: Child,
    address: String,
    service: Option<Child>,
}

impl Session {
    fn start() -> Self {
        let root = tempfile::tempdir().unwrap();
        for directory in ["run", "home", "data", "identity"] {
            let path = root.path().join(directory);
            std::fs::create_dir(&path).unwrap();
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
            }
        }
        let mut bus = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .arg(format!(
                "--address=unix:path={}",
                root.path().join("run/bus").display()
            ))
            .stdout(Stdio::piped())
            .spawn()
            .expect(
                "this test needs dbus-daemon (tools/ci/ensure-session-bus-tools.sh installs it)",
            );
        let mut address = String::new();
        std::io::BufReader::new(bus.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        assert!(!address.trim().is_empty(), "dbus-daemon printed no address");
        let mut session = Self {
            root,
            bus,
            address: address.trim().to_owned(),
            service: None,
        };
        session.start_service();
        session
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    /// A command with exactly this session's environment and nothing of the
    /// person's.
    fn command(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut command = Command::new(program);
        command
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("DBUS_SESSION_BUS_ADDRESS", &self.address)
            .env("XDG_RUNTIME_DIR", self.path("run"))
            .env("XDG_DATA_HOME", self.path("data"))
            .env("HOME", self.path("home"))
            .env("LC_ALL", "C.UTF-8");
        // Where a runner keeps libsecret and its bindings when they are not
        // under /usr.
        for name in [
            "LD_LIBRARY_PATH",
            "GI_TYPELIB_PATH",
            "PYTHONPATH",
            "XDG_DATA_DIRS",
        ] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        command
    }

    fn start_service(&mut self) {
        let service = self
            .command(SERVICE)
            .args(["serve", "--identity-root"])
            .arg(self.path("identity"))
            .stdout(Stdio::null())
            .spawn()
            .expect("the service binary starts");
        self.service = Some(service);
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let owned = self
                .command("busctl")
                .args(["--user", "--no-pager", "status", NAME])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|status| status.success());
            if owned {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "the service never claimed {NAME}"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn stop_service(&mut self) {
        if let Some(mut service) = self.service.take() {
            let _ = service.kill();
            let _ = service.wait();
        }
    }

    /// `secret-tool args`, with `input` on standard input.
    fn secret_tool(&self, args: &[&str], input: &str) -> (bool, String) {
        let mut child = self
            .command("secret-tool")
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("this test needs secret-tool (libsecret)");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        (output.status.success(), text)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop_service();
        let _ = self.bus.kill();
        let _ = self.bus.wait();
    }
}

fn client_script() -> &'static Path {
    Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/libsecret_client.py"
    ))
}

#[test]
fn secret_tool_stores_looks_up_searches_and_clears_across_a_restart() {
    let mut session = Session::start();

    let (stored, said) =
        session.secret_tool(&["store", "--label=Mail", "account", "carol"], "hunter2");
    assert!(stored, "store failed: {said}");
    let (found, secret) = session.secret_tool(&["lookup", "account", "carol"], "");
    assert!(found, "lookup failed: {secret}");
    assert_eq!(secret, "hunter2");
    let (searched, listing) = session.secret_tool(&["search", "--all", "account", "carol"], "");
    assert!(searched, "search failed: {listing}");
    assert!(listing.contains("label = Mail"), "{listing}");
    assert!(listing.contains("secret = hunter2"), "{listing}");
    assert!(listing.contains("attribute.account = carol"), "{listing}");

    // The vault holds none of it in the clear.
    let vault = std::fs::read(session.path("data/tilo/secrets/vault.tseal")).unwrap();
    assert!(!vault.windows(7).any(|window| window == b"hunter2"));
    assert!(!vault.windows(4).any(|window| window == b"Mail"));

    // A new service process over the same files: nothing was lost.
    session.stop_service();
    session.start_service();
    let (found, secret) = session.secret_tool(&["lookup", "account", "carol"], "");
    assert!(found && secret == "hunter2", "after restart: {secret}");

    let (cleared, said) = session.secret_tool(&["clear", "account", "carol"], "");
    assert!(cleared, "clear failed: {said}");
    let (found, said) = session.secret_tool(&["lookup", "account", "carol"], "");
    assert!(!found, "a cleared item was still found: {said}");
}

#[test]
fn a_gi_secret_client_works_over_both_session_algorithms_and_browses_items() {
    let session = Session::start();
    // An item another application (secret-tool) made: the python client must
    // not see it through lookup nor through the collection's Items.
    let (stored, said) = session.secret_tool(
        &["store", "--label=Foreign", "account", "from-secret-tool"],
        "not-yours",
    );
    assert!(stored, "{said}");

    let output = session
        .command("python3")
        .arg(client_script())
        .output()
        .expect("python3 runs");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.status.success() && text.trim_end().ends_with("done"),
        "the libsecret client failed (it needs python3-gobject and libsecret):\n{text}"
    );
    for check in [
        "dh session negotiated",
        "store",
        "lookup",
        "store replaces",
        "Items lists the caller's one item",
        "item Label",
        "item Attributes",
        "item Created and Modified",
        "item secret over the dh session",
        "another application's item is invisible",
        "plain session returns the secret",
        "Items includes the plain-session item",
    ] {
        assert!(
            text.contains(&format!("ok {check}")),
            "missing `{check}`:\n{text}"
        );
    }

    // And the foreign item is still there for its owner.
    let (found, secret) = session.secret_tool(&["lookup", "account", "from-secret-tool"], "");
    assert!(found && secret == "not-yours", "{secret}");
}
