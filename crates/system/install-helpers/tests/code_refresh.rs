//! The development VM's installer code refresh: the protocol against the
//! Python host that bundles packets, and the daemon's transaction on fixture
//! roots with every privileged command observed rather than run.
#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::panic_in_result_fn,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "a failed fixture call fails the test; fixtures are small known values"
)]

use std::cell::{Cell, RefCell};
use std::fs;
use std::io::{Cursor, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;

use base64::Engine as _;
use serde_json::{Value, json};
use tilo_install_helpers::code_refresh::{
    self, Files, Guest, LIMIT, SCHEMA, Stream, apply, backend_probe, bootstrap_receive, idle,
    receive, validate, write_all,
};
use tilo_install_helpers::process::Output;
use tilo_install_helpers::py::{Dumps, sha256_hex};
use tilo_install_helpers::{Error, Result};

const HELPER: &str = "usr/libexec/tilo-install";
const CONTRACT: &str = "usr/libexec/tilo-boot-mount-contract";

fn packet(files: &[(&str, &[u8])]) -> Vec<u8> {
    let rows: Vec<Value> = files
        .iter()
        .map(|(path, data)| {
            json!({"path": path, "sha256": sha256_hex(data),
                   "data": base64::engine::general_purpose::STANDARD.encode(data)})
        })
        .collect();
    code_refresh::canonical(&json!({"schema": SCHEMA, "files": rows,
        "source": {"revision": "a".repeat(40), "dirty": true}}))
}

fn framed(raw: &[u8]) -> Vec<u8> {
    let mut data = u32::try_from(raw.len()).unwrap().to_be_bytes().to_vec();
    data.extend_from_slice(raw);
    data
}

/// A port: reads come from a script, writes land where the test can see
/// them after the daemon has closed it.
struct Duplex {
    reader: Cursor<Vec<u8>>,
    written: Rc<RefCell<Vec<u8>>>,
    chunk: usize,
}

impl Duplex {
    fn new(data: Vec<u8>) -> (Self, Rc<RefCell<Vec<u8>>>) {
        let written = Rc::new(RefCell::new(Vec::new()));
        let duplex = Self {
            reader: Cursor::new(data),
            written: Rc::clone(&written),
            chunk: 3,
        };
        (duplex, written)
    }
}

impl Read for Duplex {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let limit = buffer.len().min(self.chunk);
        self.reader.read(&mut buffer[..limit])
    }
}

impl Write for Duplex {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        let count = data.len().min(7);
        self.written.borrow_mut().extend_from_slice(&data[..count]);
        Ok(count)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

type Respond = Box<dyn Fn(&[&str]) -> Result<String>>;

struct Fixture {
    _dir: tempfile::TempDir,
    base: PathBuf,
    root: PathBuf,
    proc_root: PathBuf,
    calls: RefCell<Vec<Vec<String>>>,
    respond: Respond,
    labels: RefCell<Vec<(PathBuf, PathBuf)>>,
    probe: RefCell<Option<Output>>,
    ports: RefCell<Vec<Duplex>>,
    readies: Cell<usize>,
    pauses: Cell<usize>,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("state");
        let root = dir.path().join("root");
        let proc_root = dir.path().join("proc");
        fs::create_dir(&base).unwrap();
        fs::set_permissions(&base, fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir(&proc_root).unwrap();
        Self {
            _dir: dir,
            base,
            root,
            proc_root,
            calls: RefCell::new(Vec::new()),
            respond: Box::new(|_| Ok(String::new())),
            labels: RefCell::new(Vec::new()),
            probe: RefCell::new(None),
            ports: RefCell::new(Vec::new()),
            readies: Cell::new(0),
            pauses: Cell::new(usize::MAX),
        }
    }

    fn helper(&self, relative: &str, data: &[u8]) -> PathBuf {
        let path = self.root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, data).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn marker(&self) -> PathBuf {
        self.base.join("active.json")
    }

    fn mounts(&self) -> Vec<Vec<String>> {
        let calls = self.calls.borrow();
        calls
            .iter()
            .filter(|argv| argv[0] != "systemctl" || argv[1] != "list-units")
            .cloned()
            .collect()
    }
}

impl Guest for Fixture {
    fn base(&self) -> PathBuf {
        self.base.clone()
    }
    fn root(&self) -> PathBuf {
        self.root.clone()
    }
    fn proc_root(&self) -> PathBuf {
        self.proc_root.clone()
    }
    fn command(&self, argv: &[&str]) -> Result<String> {
        self.calls
            .borrow_mut()
            .push(argv.iter().map(|word| (*word).to_owned()).collect());
        (self.respond)(argv)
    }
    fn gate(&self) -> Result<()> {
        Ok(())
    }
    fn lock(&self) -> Result<fs::File> {
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.base.join("lock"))?;
        Ok(file)
    }
    fn copy_label(&self, from: &Path, to: &Path) -> Result<()> {
        self.labels
            .borrow_mut()
            .push((from.to_path_buf(), to.to_path_buf()));
        Ok(())
    }
    fn run_probe(&self, argv: &[&str]) -> Result<Output> {
        assert_eq!(
            argv,
            ["/usr/libexec/tilo-install", "--dev-code-refresh-probe"]
        );
        Ok(self.probe.borrow().clone().unwrap())
    }
    fn fixture_disk(&self) -> bool {
        false
    }
    fn open_port(&self) -> Result<Box<dyn Stream>> {
        let mut ports = self.ports.borrow_mut();
        let port = ports.remove(0);
        Ok(Box::new(port))
    }
    fn ready(&self) -> Result<()> {
        self.readies.set(self.readies.get() + 1);
        Ok(())
    }
    fn bootstrap_handshake(&self) -> bool {
        false
    }
    fn pause(&self) -> Result<()> {
        let left = self.pauses.get();
        if left == 0 {
            return Err(Error::new("end test"));
        }
        self.pauses.set(left - 1);
        Ok(())
    }
}

fn receipt(sha: char) -> Value {
    json!({"schema": SCHEMA, "sha256": sha.to_string().repeat(64), "files": [], "source": {}})
}

fn files(rows: &[(&str, &[u8])]) -> Files {
    rows.iter()
        .map(|(path, data)| ((*path).to_owned(), data.to_vec()))
        .collect()
}

#[test]
fn fragmented_transport_binds_the_source_content() {
    let raw = packet(&[(HELPER, b"#!/bin/bash\necho development\n")]);
    let (mut port, _) = Duplex::new(framed(&raw));
    let (files, receipt) = receive(&mut port).unwrap();
    assert_eq!(
        files,
        vec![(
            HELPER.to_owned(),
            b"#!/bin/bash\necho development\n".to_vec()
        )]
    );
    assert_eq!(
        receipt["source"],
        json!({"revision": "a".repeat(40), "dirty": true})
    );
    assert_eq!(receipt["sha256"], json!(sha256_hex(&raw)));
    let (mut port, written) = Duplex::new(Vec::new());
    write_all(&mut port, &framed(&raw)).unwrap();
    assert_eq!(*written.borrow(), framed(&raw));
}

#[test]
fn bad_hash_path_duplicates_and_packet_size_refused() {
    let raw = packet(&[(HELPER, b"helper\n")]);
    let document: Value = serde_json::from_slice(&raw).unwrap();
    for (variant, message) in [
        ("hash", "code refresh content differs from its bounded hash"),
        ("path", "code refresh path is not uniquely allowlisted"),
        ("duplicate", "code refresh path is not uniquely allowlisted"),
        (
            "dev-target-uki",
            "code refresh path is not uniquely allowlisted",
        ),
        ("revision", "invalid code refresh metadata"),
        ("empty", "invalid code refresh file inventory"),
    ] {
        let mut changed = document.clone();
        let rows = changed["files"].as_array_mut().unwrap();
        match variant {
            "hash" => rows[0]["sha256"] = json!("b".repeat(64)),
            "path" => rows[0]["path"] = json!("../../etc/shadow"),
            "duplicate" => rows.push(rows[0].clone()),
            "dev-target-uki" => {
                rows[0]["path"] = json!("usr/libexec/tilo-installer-dev-target-uki.py");
            }
            "revision" => changed["source"]["revision"] = json!("A".repeat(40)),
            _ => rows.clear(),
        }
        let error = validate(&code_refresh::canonical(&changed)).unwrap_err();
        assert_eq!(error.0, message, "{variant}");
    }
    let (mut port, _) = Duplex::new((u32::try_from(LIMIT).unwrap() + 1).to_be_bytes().to_vec());
    assert_eq!(
        receive(&mut port).unwrap_err().0,
        "code refresh packet exceeds its bound"
    );
    let (mut port, _) = Duplex::new(vec![0, 0, 0, 9, b'{']);
    assert_eq!(
        receive(&mut port).unwrap_err().0,
        "code refresh stream ended early"
    );
    for duplicated in [
        &br#"{"schema":1,"schema":2}"#[..],
        br#"{"files":[],"schema":"x","source":{"dirty":true,"dirty":false}}"#,
    ] {
        assert_eq!(
            validate(duplicated).unwrap_err().0,
            "duplicate code refresh field"
        );
    }
}

#[test]
fn python_host_bundle_validates_to_the_identical_receipt() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let module = repo.join("os/dev/installer-code-bootstrap/tilo-installer-code-protocol.py");
    let checkout = tempfile::tempdir().unwrap();
    for (relative, data) in [
        (
            "os/usr/libexec/tilo-install",
            "#!/bin/bash\necho development\n",
        ),
        ("os/usr/libexec/tilo-boot-mount-contract", "contract é\n"),
        (
            "os/dev/hardware_profile.py",
            "canonical profile implementation\n",
        ),
    ] {
        let path = checkout.path().join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, data).unwrap();
    }
    let script = "import importlib.util, json, sys\n\
        spec = importlib.util.spec_from_file_location('protocol', sys.argv[1])\n\
        protocol = importlib.util.module_from_spec(spec); spec.loader.exec_module(protocol)\n\
        raw = protocol.bundle(sys.argv[2], 'c' * 40, False)\n\
        print(json.dumps({'raw': raw.decode(), 'receipt': protocol.validate(raw)[1],\n\
                          'allowlist': sorted(protocol.ALLOWLIST)}))\n";
    let output = Command::new("python3")
        .args(["-I", "-c", script])
        .arg(&module)
        .arg(checkout.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let python: Value = serde_json::from_slice(&output.stdout).unwrap();
    let mut allowlist = code_refresh::ALLOWLIST.to_vec();
    allowlist.sort_unstable();
    assert_eq!(python["allowlist"], json!(allowlist));
    let (files, receipt) = validate(python["raw"].as_str().unwrap().as_bytes()).unwrap();
    assert_eq!(files.len(), 3);
    assert_eq!(receipt, python["receipt"]);
    assert_eq!(
        Dumps::SORTED.dumps(&receipt),
        Dumps::SORTED.dumps(&python["receipt"])
    );
}

#[test]
fn binding_preserves_destination_mode_and_label_and_records_applied_state() {
    let fixture = Fixture::new();
    let destination = fixture.helper(HELPER, b"original helper");
    let receipt = receipt('a');
    let applied = apply(&fixture, &files(&[(HELPER, b"updated helper")]), &receipt).unwrap();
    assert_eq!(applied, receipt);
    let staged = fixture.base.join("a".repeat(64)).join("tilo-install");
    assert_eq!(fs::read(&staged).unwrap(), b"updated helper");
    assert_eq!(
        fs::metadata(&staged).unwrap().permissions().mode() & 0o777,
        0o755
    );
    assert_eq!(
        *fixture.labels.borrow(),
        vec![(destination.clone(), staged.clone())]
    );
    let destination = destination.to_string_lossy().into_owned();
    assert_eq!(
        fixture.mounts(),
        vec![
            vec![
                "mount".to_owned(),
                "--bind".into(),
                staged.to_string_lossy().into_owned(),
                destination.clone()
            ],
            vec![
                "mount".into(),
                "-o".into(),
                "remount,bind,ro".into(),
                destination
            ],
        ]
    );
    // The receipt the backend and the install proof read, as json.dumps
    // wrote it.
    assert_eq!(
        fs::read_to_string(fixture.marker()).unwrap(),
        format!(
            "{{\"files\": [], \"schema\": \"{SCHEMA}\", \"sha256\": \"{}\", \"source\": {{}}, \"state\": \"applied\"}}\n",
            "a".repeat(64)
        )
    );
    assert_eq!(
        fs::metadata(fixture.marker()).unwrap().permissions().mode() & 0o777,
        0o600
    );
    // The same packet again is already in place: no new generation or mount.
    assert_eq!(
        apply(&fixture, &files(&[(HELPER, b"updated helper")]), &receipt).unwrap(),
        receipt
    );
    assert_eq!(fixture.mounts().len(), 2);
    // A different one after it is a new generation.
    apply(
        &fixture,
        &files(&[(HELPER, b"third helper")]),
        &self::receipt('b'),
    )
    .unwrap();
    assert_eq!(fixture.mounts().len(), 4);
}

#[test]
fn real_backend_flock_refuses_refresh_before_write() {
    let fixture = Fixture::new();
    let _held = advisory_lock::FileLock::try_exclusive(
        fs::File::create(fixture.base.join("lock")).unwrap(),
    )
    .unwrap();
    let error = apply(&fixture, &files(&[]), &receipt('a')).unwrap_err();
    assert_eq!(
        error.0,
        "installer backend is running; code refresh refused"
    );
    assert!(fixture.calls.borrow().is_empty());
    assert!(!fixture.marker().exists());
}

#[test]
fn old_backend_independent_unit_and_process_checks() {
    let mut fixture = Fixture::new();
    fixture.respond =
        Box::new(|_| Ok("tilo-install-123.service loaded activating start task".into()));
    assert_eq!(
        idle(&fixture).unwrap_err().0,
        "installer transient unit is running; code refresh refused"
    );
    fixture.respond = Box::new(|_| Ok("tilo-install-7.service loaded failed failed task\n".into()));
    let process = fixture.proc_root.join("123");
    fs::create_dir(&process).unwrap();
    fs::write(
        process.join("cmdline"),
        b"/usr/bin/tilo-installer\0--fullscreen\0",
    )
    .unwrap();
    fs::create_dir(fixture.proc_root.join("self")).unwrap();
    fs::write(
        fixture.proc_root.join("self/cmdline"),
        b"/usr/libexec/tilo-install\0",
    )
    .unwrap();
    idle(&fixture).unwrap();
    fs::write(
        process.join("cmdline"),
        b"/bin/bash\0/usr/libexec/tilo-install\0--disk\0/dev/test",
    )
    .unwrap();
    assert_eq!(
        idle(&fixture).unwrap_err().0,
        "installer helper process is running; code refresh refused"
    );
}

#[test]
fn failed_second_mount_rolls_back_new_binds_and_keeps_old_receipt() {
    let mut fixture = Fixture::new();
    let first = fixture.helper(HELPER, b"old");
    fixture.helper(CONTRACT, b"old");
    fixture.respond = Box::new(|argv| {
        if argv[..2] == ["mount", "--bind"] && argv[3].ends_with("tilo-boot-mount-contract") {
            return Err(Error::new("simulated mount failure"));
        }
        Ok(String::new())
    });
    let previous = json!({"sha256": "b".repeat(64), "state": "applied"});
    fs::write(fixture.marker(), previous.to_string()).unwrap();
    let error = apply(
        &fixture,
        &files(&[(HELPER, b"first"), (CONTRACT, b"second")]),
        &receipt('a'),
    )
    .unwrap_err();
    assert_eq!(error.0, "simulated mount failure");
    assert_eq!(
        fixture.calls.borrow().last().unwrap(),
        &vec!["umount".to_owned(), first.to_string_lossy().into_owned()]
    );
    let marker: Value = serde_json::from_slice(&fs::read(fixture.marker()).unwrap()).unwrap();
    assert_eq!(marker, previous);
}

#[test]
fn failed_rollback_stops_greeter_and_keeps_incomplete_marker() {
    let mut fixture = Fixture::new();
    fixture.helper(HELPER, b"old");
    fixture.respond = Box::new(|argv| {
        if argv[0] == "umount" || argv[..2] == ["mount", "-o"] {
            return Err(Error::new("simulated transaction failure"));
        }
        Ok(String::new())
    });
    let error = apply(&fixture, &files(&[(HELPER, b"new")]), &receipt('a')).unwrap_err();
    assert_eq!(error.0, "simulated transaction failure");
    assert_eq!(
        fixture.calls.borrow().last().unwrap(),
        &vec![
            "systemctl".to_owned(),
            "stop".into(),
            "greetd.service".into()
        ]
    );
    let marker: Value = serde_json::from_slice(&fs::read(fixture.marker()).unwrap()).unwrap();
    assert_eq!(marker["state"], json!("applying"));
}

#[test]
fn incomplete_or_repeated_generations_are_refused() {
    let fixture = Fixture::new();
    fixture.helper(HELPER, b"old");
    fs::write(fixture.marker(), r#"{"state":"applying"}"#).unwrap();
    assert_eq!(
        apply(&fixture, &files(&[(HELPER, b"new")]), &receipt('a'))
            .unwrap_err()
            .0,
        "previous refresh is incomplete; restart this VM"
    );
    fs::remove_file(fixture.marker()).unwrap();
    fs::create_dir(fixture.base.join("a".repeat(64))).unwrap();
    assert_eq!(
        apply(&fixture, &files(&[(HELPER, b"new")]), &receipt('a'))
            .unwrap_err()
            .0,
        "this installer code generation was already attempted; restart this VM"
    );
    assert!(fixture.mounts().is_empty());
}

#[test]
fn readiness_only_after_successful_first_application() {
    let mut fixture = Fixture::new();
    fixture.helper(HELPER, b"old");
    let listed = Cell::new(0);
    fixture.respond = Box::new(move |argv| {
        if argv[..2] == ["systemctl", "list-units"] {
            listed.set(listed.get() + 1);
            if listed.get() == 1 {
                return Ok("tilo-install-1.service loaded active running task".into());
            }
        }
        Ok(String::new())
    });
    let raw = packet(&[(HELPER, b"new\n")]);
    let (busy, refused) = Duplex::new(framed(&raw));
    let (fresh, answered) = Duplex::new(framed(&raw));
    fixture.ports = RefCell::new(vec![busy, fresh]);
    fixture.pauses.set(1);
    assert_eq!(code_refresh::serve(&fixture).unwrap_err().0, "end test");
    assert_eq!(fixture.readies.get(), 1);
    assert_eq!(
        *refused.borrow(),
        b"{\"error\":\"installer transient unit is running; code refresh refused\",\"ok\":false}\n"
    );
    let response: Value = serde_json::from_slice(&answered.borrow()).unwrap();
    let (_, receipt) = validate(&raw).unwrap();
    assert_eq!(
        response,
        json!({"ok": true, "receipt": receipt, "runtime": {
            "schema": "tilo.installer-code-runtime-probe.v1", "backend": null}})
    );
}

#[test]
fn backend_probe_requires_actual_transient_enforcing_helper_identity() {
    let fixture = Fixture::new();
    let helper = b"# TILO_VM_CODE_REFRESH_PROBE_V1:\nactual helper bytes\n";
    let valid = json!({"schema": "tilo.installer-code-backend-probe.v1", "transient": 1,
        "enforcing": true, "domain": "system_u:system_r:init_t:s0",
        "helper_sha256": sha256_hex(helper)});
    for changed in [
        json!({}),
        json!({"enforcing": false}),
        json!({"transient": 0}),
        json!({"domain": "system_u:system_r:xdm_t:s0"}),
        json!({"helper_sha256": "a".repeat(64)}),
    ] {
        let mut row = valid.clone();
        for (key, value) in changed.as_object().unwrap() {
            row[key] = value.clone();
        }
        let mut stdout = b"boot noise\n".to_vec();
        stdout.extend_from_slice(row.to_string().as_bytes());
        *fixture.probe.borrow_mut() = Some(Output {
            code: Some(0),
            stdout,
            stderr: Vec::new(),
        });
        let result = backend_probe(&fixture, &files(&[(HELPER, helper)]));
        if changed == json!({}) {
            assert_eq!(result.unwrap(), valid);
        } else {
            assert_eq!(
                result.unwrap_err().0,
                "backend probe does not match the executed enforcing transient helper"
            );
        }
    }
    *fixture.probe.borrow_mut() = Some(Output {
        code: Some(3),
        stdout: b"out ".to_vec(),
        stderr: b"err".to_vec(),
    });
    assert_eq!(
        backend_probe(&fixture, &files(&[(HELPER, helper)]))
            .unwrap_err()
            .0,
        "refreshed backend execution probe failed (exit 3): out err"
    );
    // A helper without the probe is not executed.
    assert_eq!(
        backend_probe(&fixture, &files(&[(HELPER, b"plain")])).unwrap(),
        Value::Null
    );
}

#[test]
fn bootstrap_transport_resynchronizes_bounded_early_and_repeated_hellos() {
    let raw = packet(&[(HELPER, b"helper\n")]);
    let mut script = b"LO-1\n".to_vec();
    for _ in 0..20 {
        script.extend_from_slice(b"TILO-CODE-HELLO-1\n");
    }
    script.extend_from_slice(&framed(&raw));
    let (mut port, written) = Duplex::new(script);
    let (files, _) = bootstrap_receive(&mut port).unwrap();
    assert_eq!(files, vec![(HELPER.to_owned(), b"helper\n".to_vec())]);
    let written = String::from_utf8(written.borrow().clone()).unwrap();
    assert_eq!(written.matches("installer-code-ready").count(), 20);
    let (mut port, _) = Duplex::new(b"TILO-CODE-HELLO-1\n".repeat(66));
    assert_eq!(
        bootstrap_receive(&mut port).unwrap_err().0,
        "too many repeated bootstrap hellos"
    );
}

#[test]
fn the_shipped_daemon_refuses_outside_the_development_vm() {
    let output = Command::new(env!("CARGO_BIN_EXE_tilo-vm-install-proof"))
        .arg("code-refresh")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "tilo-vm-install-proof code-refresh: installer code refresh requires the opted-in development scenario VM\n"
    );
    assert!(output.stdout.is_empty());
}

#[test]
fn the_transient_backend_owns_the_lock_and_refuses_an_unapplied_refresh() {
    // The backend's own lock block, run as the launcher re-execs it.
    let backend = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../os/usr/libexec/tilo-install"),
    )
    .unwrap();
    let start = backend
        .find("# Only the transient backend owns this lock.")
        .unwrap();
    let end = backend
        .get(start..)
        .unwrap()
        .find("# TILO_VM_CODE_REFRESH_PROBE_V1:")
        .unwrap()
        + start;
    assert!(
        start
            > backend
                .find("if [ -z \"${TILO_INSTALL_TRANSIENT:-}\" ]; then")
                .unwrap()
    );
    let directory = tempfile::tempdir().unwrap();
    let state = directory.path().join("code-refresh");
    fs::create_dir(&state).unwrap();
    let block = backend
        .get(start..end)
        .unwrap()
        .replace("/run/tilo-installer/code-refresh", state.to_str().unwrap())
        .replace(
            "/usr/libexec/tilo-vm-install-proof",
            env!("CARGO_BIN_EXE_tilo-vm-install-proof"),
        );
    let script = directory.path().join("launcher.sh");
    fs::write(
        &script,
        format!(
            "#!/bin/bash\nset -euo pipefail\nif [ -z \"${{TILO_INSTALL_TRANSIENT:-}}\" ]; then\n \
             TILO_INSTALL_TRANSIENT=1 bash \"$0\"; exit $?\nfi\n{block}echo backend-ran\n"
        ),
    )
    .unwrap();
    let run = || Command::new("bash").arg(&script).output().unwrap();
    let ran = run();
    assert!(
        ran.status.success(),
        "{}",
        String::from_utf8_lossy(&ran.stderr)
    );
    assert_eq!(ran.stdout, b"backend-ran\n");
    fs::write(
        state.join("active.json"),
        r#"{"state":"applied","sha256":"a"}"#,
    )
    .unwrap();
    assert_eq!(run().stdout, b"backend-ran\n");
    for marker in [r#"{"state":"applying"}"#, "{}", "not json"] {
        fs::write(state.join("active.json"), marker).unwrap();
        let refused = run();
        assert!(!refused.status.success(), "{marker}");
        assert!(refused.stdout.is_empty(), "{marker}");
    }
    fs::write(state.join("active.json"), r#"{"state":"applying"}"#).unwrap();
    assert!(
        String::from_utf8_lossy(&run().stderr).contains("installer code refresh is incomplete")
    );
}
