//! A real app launch under the generated Seatbelt profile.
//!
//! A tiny C probe (`fixtures/probe.c`) is compiled and ad-hoc signed per
//! test, installed into a payload directory, and started through
//! [`launcher_macos::launch`] — the production path, profile preflight,
//! `sandbox-exec` and descriptor inheritance included. Each run performs one
//! operation; exit 0 means it succeeded, 1 that the sandbox refused it.

use std::ffi::OsString;
use std::io::Read as _;
use std::os::fd::AsRawFd as _;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::Command;

use launcher::capabilities::CapabilityPlan;
use launcher::netgate::{EgressDecision, EgressPolicy};
use launcher_macos::policy::prepare_container;
use launcher_macos::{
    CodeSignature, LaunchRequest, MacLaunchMode, SeatbeltReason, TrustRequirement, inspect, launch,
};

const OK: i32 = 0;
const REFUSED: i32 = 1;
const PACKAGE: &str = "so.tilo.seatbelt-probe";

struct Fixture {
    _base: tempfile::TempDir,
    root: PathBuf,
    payload: PathBuf,
    probe: PathBuf,
    container: PathBuf,
    secret: PathBuf,
    granted: PathBuf,
}

fn run_ok(cmd: &mut Command) {
    let out = cmd.output().unwrap_or_else(|e| panic!("{cmd:?}: {e}"));
    assert!(
        out.status.success(),
        "{cmd:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn compile_probe(out: &Path) {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/probe.c");
    run_ok(Command::new("cc").arg("-O0").arg("-o").arg(out).arg(source));
    run_ok(
        Command::new("/usr/bin/codesign")
            .args(["--force", "--sign", "-"])
            .arg(out),
    );
}

fn fixture() -> Fixture {
    let base = tempfile::tempdir().unwrap();
    // Seatbelt sees `/private/var/folders/…`, not `/var/folders/…`.
    let root = std::fs::canonicalize(base.path()).unwrap();
    let payload = root.join("payload");
    std::fs::create_dir_all(&payload).unwrap();
    let probe = payload.join("probe");
    compile_probe(&probe);
    std::fs::write(payload.join("data.txt"), b"payload").unwrap();

    let container = prepare_container(&root.join("support"), None, PACKAGE).unwrap();
    let outside = root.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let secret = outside.join("secret.txt");
    std::fs::write(&secret, b"not the app's").unwrap();
    let granted = root.join("granted");
    std::fs::create_dir_all(&granted).unwrap();
    std::fs::write(granted.join("file.txt"), b"granted").unwrap();
    Fixture {
        _base: base,
        root,
        payload,
        probe,
        container,
        secret,
        granted,
    }
}

fn request(f: &Fixture, op: &str, arg: &Path) -> LaunchRequest {
    request_with(f, &f.probe, op, arg.as_os_str().to_owned())
}

fn request_with(f: &Fixture, exe: &Path, op: &str, arg: OsString) -> LaunchRequest {
    LaunchRequest {
        package_id: PACKAGE.to_string(),
        executable: exe.to_path_buf(),
        args: vec![OsString::from(op), arg],
        payload_dir: f.payload.clone(),
        runtime_dylib_dir: None,
        container_dir: f.container.clone(),
        capabilities: CapabilityPlan::empty(),
        egress: EgressDecision::Blocked,
        inherit_fds: Vec::new(),
        env: Vec::new(),
        trust: TrustRequirement::DeveloperId,
    }
}

fn exit_code(req: &LaunchRequest) -> i32 {
    let mut app = launch(req).unwrap_or_else(|e| panic!("launch failed: {e}"));
    assert!(
        matches!(app.mode(), MacLaunchMode::Seatbelt(_)),
        "{:?}",
        app.mode()
    );
    let status = app.wait().unwrap();
    status
        .code()
        .unwrap_or_else(|| panic!("the probe did not exit normally: {status:?}"))
}

#[test]
fn an_ad_hoc_app_is_confined_to_its_payload_and_container() {
    let f = fixture();
    assert!(matches!(
        inspect(&f.probe, &TrustRequirement::DeveloperId),
        CodeSignature::AdHoc { app_sandbox: false }
    ));
    let mut app = launch(&request(&f, "read", &f.payload.join("data.txt"))).unwrap();
    assert_eq!(app.mode(), MacLaunchMode::Seatbelt(SeatbeltReason::AdHoc));
    assert_eq!(app.wait().unwrap().code(), Some(OK), "payload read");

    assert_eq!(
        exit_code(&request(&f, "read", &f.secret)),
        REFUSED,
        "a file outside every grant was readable"
    );
    let home_file = f.root.join("home-like/.ssh/id_ed25519");
    std::fs::create_dir_all(home_file.parent().unwrap()).unwrap();
    std::fs::write(&home_file, b"key").unwrap();
    assert_eq!(exit_code(&request(&f, "read", &home_file)), REFUSED);

    let written = f.container.join(".local/share/state.bin");
    assert_eq!(
        exit_code(&request(&f, "write", &written)),
        OK,
        "container write"
    );
    assert!(written.is_file());

    let escaped = f.root.join("outside/new.txt");
    assert_eq!(
        exit_code(&request(&f, "write", &escaped)),
        REFUSED,
        "a write outside the container succeeded"
    );
    assert!(!escaped.exists());

    let into_payload = f.payload.join("dropped");
    assert_eq!(exit_code(&request(&f, "write", &into_payload)), REFUSED);
    assert!(!into_payload.exists());
}

#[test]
fn the_container_is_the_environment_and_nothing_else_is_inherited() {
    let f = fixture();
    let expect = |kv: String| exit_code(&request_with(&f, &f.probe, "env", OsString::from(kv)));
    assert_eq!(expect(format!("HOME={}", f.container.display())), OK);
    assert_eq!(
        expect(format!("TMPDIR={}", f.container.join("tmp").display())),
        OK
    );
    assert_eq!(
        expect(format!(
            "XDG_CONFIG_HOME={}",
            f.container.join(".config").display()
        )),
        OK
    );

    let tmp_file = f.container.join("tmp/scratch");
    assert_eq!(exit_code(&request(&f, "write", &tmp_file)), OK);
}

#[test]
fn only_granted_paths_widen_the_profile_and_payload_stays_read_only() {
    let f = fixture();
    let granted_file = f.granted.join("file.txt");
    let mut req = request(&f, "read", &granted_file);
    assert_eq!(exit_code(&req), REFUSED, "ungranted path was readable");

    req.capabilities = CapabilityPlan {
        ro_paths: vec![f.granted.clone()],
        ..CapabilityPlan::empty()
    };
    assert_eq!(exit_code(&req), OK, "read-only grant");
    let mut write = request(&f, "write", &f.granted.join("new.txt"));
    write.capabilities = req.capabilities;
    assert_eq!(
        exit_code(&write),
        REFUSED,
        "a read-only grant allowed a write"
    );

    // A read-write grant on a directory containing the payload still cannot
    // make the payload writable.
    let wide = CapabilityPlan {
        rw_paths: vec![f.root.clone()],
        ..CapabilityPlan::empty()
    };
    let mut granted_write = request(&f, "write", &f.root.join("outside/new.txt"));
    granted_write.capabilities = wide.clone();
    assert_eq!(exit_code(&granted_write), OK, "read-write grant");
    let mut payload_write = request(&f, "write", &f.payload.join("dropped"));
    payload_write.capabilities = wide;
    assert_eq!(
        exit_code(&payload_write),
        REFUSED,
        "payload became writable"
    );
    assert!(!f.payload.join("dropped").exists());
}

#[test]
fn code_runs_only_from_the_payload() {
    let f = fixture();
    // The probe exec'd with no operation exits 2: the exec itself worked.
    assert_eq!(exit_code(&request(&f, "exec", &f.probe)), 2);
    assert_eq!(
        exit_code(&request(&f, "exec", Path::new("/bin/sh"))),
        REFUSED
    );

    let copied = f.container.join("probe");
    std::fs::copy(&f.probe, &copied).unwrap();
    assert_eq!(
        exit_code(&request(&f, "exec", &copied)),
        REFUSED,
        "code written to the container ran"
    );
}

#[test]
fn network_follows_the_egress_decision() {
    let f = fixture();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = OsString::from(listener.local_addr().unwrap().port().to_string());

    let offline = request_with(&f, &f.probe, "tcp", port.clone());
    assert_eq!(exit_code(&offline), REFUSED, "offline app reached TCP");

    let mut online = offline;
    online.egress = EgressDecision::Unrestricted;
    assert_eq!(exit_code(&online), OK, "unrestricted egress was blocked");

    // A filesystem socket the user owns (an `ssh-agent`, say) stays closed
    // even with an unrestricted network; only the gate is ever reachable.
    let agent = f.root.join("agent.sock");
    let _agent = UnixListener::bind(&agent).unwrap();
    let mut to_agent = request(&f, "unix", &agent);
    to_agent.egress = EgressDecision::Unrestricted;
    assert_eq!(exit_code(&to_agent), REFUSED, "a user socket was reachable");

    // A host allowlist: the launcher's gate is the only way out, through the
    // proxy environment, and it carries only the granted hosts.
    let filtered = EgressDecision::Filtered(EgressPolicy::from_grants(["localhost"]));
    let mut tcp_gated = request_with(&f, &f.probe, "tcp", port.clone());
    tcp_gated.egress = filtered.clone();
    assert_eq!(
        exit_code(&tcp_gated),
        REFUSED,
        "filtered egress bypassed the gate"
    );
    let through_gate = |target: String| {
        let mut req = request_with(&f, &f.probe, "proxy", OsString::from(target));
        req.egress = filtered.clone();
        exit_code(&req)
    };
    let port = port.to_str().unwrap();
    assert_eq!(
        through_gate(format!("localhost:{port}")),
        OK,
        "a granted host was unreachable through the gate"
    );
    assert_eq!(
        through_gate(format!("127.0.0.1:{port}")),
        REFUSED,
        "an address literal satisfied a hostname grant"
    );
    assert_eq!(
        through_gate("example.com:443".to_string()),
        REFUSED,
        "an ungranted host was reachable"
    );
    let mut to_agent = request(&f, "unix", &agent);
    to_agent.egress = filtered;
    assert_eq!(exit_code(&to_agent), REFUSED, "a user socket was reachable");
}

fn user_cache_dir() -> PathBuf {
    let mut buf = vec![0_u8; 1024];
    // SAFETY: `buf` is writable for its length.
    let len = unsafe {
        libc::confstr(
            libc::_CS_DARWIN_USER_CACHE_DIR,
            buf.as_mut_ptr().cast(),
            buf.len(),
        )
    };
    assert!(len > 0 && len <= buf.len());
    buf.truncate(len.saturating_sub(1));
    std::fs::canonicalize(String::from_utf8(buf).unwrap()).unwrap()
}

/// Removes an app's cache directory, which lives outside the fixture.
struct CacheDir(PathBuf);

impl Drop for CacheDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn each_app_keeps_its_own_shader_cache_and_no_other() {
    let f = fixture();
    let room = format!("cache-test-{}", std::process::id());
    let container = prepare_container(&f.root.join("support"), Some(&room), PACKAGE).unwrap();
    let other = prepare_container(&f.root.join("support"), Some(&room), "so.tilo.other").unwrap();
    let own = CacheDir(user_cache_dir().join(format!("so.tilo+{room}+{PACKAGE}")));
    let _other = CacheDir(user_cache_dir().join(format!("so.tilo+{room}+so.tilo.other")));

    let mut req = request_with(&f, &f.probe, "cache", OsString::from("com.apple.metal"));
    req.container_dir = container;
    assert_eq!(
        exit_code(&req),
        OK,
        "the app's cache directory was not writable"
    );
    assert!(own.0.join("com.apple.metal").is_file());

    let mut intruder = request_with(
        &f,
        &f.probe,
        "cache",
        OsString::from(format!("../so.tilo+{room}+{PACKAGE}/planted")),
    );
    intruder.container_dir = other;
    assert_eq!(
        exit_code(&intruder),
        REFUSED,
        "an app wrote into another app's cache"
    );
    assert!(!own.0.join("planted").exists());
}

#[test]
fn a_path_picked_in_the_runtimes_chooser_opens_and_nothing_else_does() {
    let f = fixture();
    let picked = f.root.join("outside/picked.txt");
    std::fs::write(&picked, b"picked").unwrap();
    let with_token = |path: &Path, token: Option<String>| {
        let mut req = request(&f, "picked", path);
        req.env = token
            .map(|token| (OsString::from("TILO_PROBE_TOKEN"), OsString::from(token)))
            .into_iter()
            .collect();
        exit_code(&req)
    };
    assert_eq!(
        with_token(&picked, None),
        REFUSED,
        "an unpicked path opened"
    );
    let token = tilo_dialogs::confined::issue_extension(&picked).unwrap();
    assert_eq!(
        with_token(&picked, Some(token.clone())),
        OK,
        "the picked path did not open"
    );
    assert_eq!(
        with_token(&f.secret, Some(token)),
        REFUSED,
        "one path's grant opened another"
    );
    let into_payload =
        tilo_dialogs::confined::issue_extension(&f.payload.join("data.txt")).unwrap();
    assert_eq!(
        with_token(&f.payload.join("data.txt"), Some(into_payload)),
        REFUSED,
        "a grant made the payload writable"
    );
}

/// Save through the runtime's chooser, as `tilo_dialogs::confined::serve`
/// answers it: the destination and its one sibling are granted.
fn saved_through_the_chooser(destination: &Path) -> Vec<String> {
    use std::io::{BufRead as _, Write as _};
    use tilo_dialogs::confined::{Answer, encode_request, serve};
    let (mut app, runtime) = std::os::unix::net::UnixStream::pair().unwrap();
    let chosen = destination.to_path_buf();
    let server = std::thread::spawn(move || serve(runtime, |_| Ok(Some(vec![chosen.clone()]))));
    let request = tilo_dialogs::portal::PickRequest::save("Save", "Save", "doc.txt");
    writeln!(app, "{}", encode_request(&request)).unwrap();
    let mut line = String::new();
    std::io::BufReader::new(app.try_clone().unwrap())
        .read_line(&mut line)
        .unwrap();
    drop(app);
    server.join().unwrap().unwrap();
    match Answer::decode(&line).unwrap() {
        Answer::Picked(mut picked) => picked.remove(0).tokens,
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_save_replaces_the_picked_file_through_its_one_sibling() {
    let f = fixture();
    let destination = f.root.join("outside/doc.txt");
    std::fs::write(&destination, b"old").unwrap();
    let replace = |tokens: &[String]| {
        let mut req = request(&f, "replace", &destination);
        req.env = vec![(
            OsString::from("TILO_PROBE_TOKENS"),
            OsString::from(tokens.join("\n")),
        )];
        exit_code(&req)
    };
    let tokens = saved_through_the_chooser(&destination);
    assert_eq!(tokens.len(), 2, "the destination and its sibling");
    assert_eq!(
        replace(&tokens[..1]),
        REFUSED,
        "the sibling was writable without its grant"
    );
    assert_eq!(replace(&tokens), OK, "the save by rename failed");
    assert_eq!(
        std::fs::read(&destination).unwrap(),
        b"TILO_SEATBELT_PROBE_MARKER\0"
    );
    assert!(!f.root.join("outside/unrelated.tmp").exists());
    assert_eq!(replace(&tokens), OK, "a second save failed");
}

/// A confined app captures through the broker it inherits: the descriptor
/// passing and the stream survive the Seatbelt profile, and an undeclared
/// device is refused.
#[test]
fn a_confined_app_captures_only_what_its_broker_grants() {
    use std::sync::Arc;
    use tilo_capture::DeviceKind;
    use tilo_capture::broker::{Policy, Sources, serve};
    let f = fixture();
    let grants_dir = tempfile::tempdir().unwrap();
    let grants = Arc::new(
        tilo_capture::grants::DeviceGrants::open(&grants_dir.path().join("grants")).unwrap(),
    );
    let (app, runtime) = std::os::unix::net::UnixDatagram::pair().unwrap();
    let policy: Arc<dyn Policy> = Arc::new(tilo_capture::grants::GrantPolicy {
        grants,
        room: "personal".into(),
        app: PACKAGE.into(),
        declared: vec![DeviceKind::Camera],
        consent: |_| Ok(true),
    });
    let sources: Arc<dyn Sources> = Arc::new(tilo_capture::broker::synthetic::Devices);
    std::thread::spawn(move || serve(&runtime, &policy, &sources));
    let capture = |what: &str| {
        let mut req = request_with(&f, &f.probe, "capture", OsString::from(what));
        req.inherit_fds = vec![app.as_raw_fd()];
        req.env = vec![(
            OsString::from("TILO_SVC_CAPTURE_FD"),
            OsString::from(app.as_raw_fd().to_string()),
        )];
        exit_code(&req)
    };
    assert_eq!(
        capture("camera 640 480"),
        OK,
        "the granted camera did not stream"
    );
    assert_eq!(
        capture("microphone 48000"),
        REFUSED,
        "an undeclared device was captured"
    );
}

#[test]
fn service_descriptors_are_inherited_and_nothing_else_is() {
    let f = fixture();
    let (mut reader, writer) = std::io::pipe().unwrap();
    let fd = writer.as_raw_fd();
    let mut req = request_with(&f, &f.probe, "fd", OsString::from(fd.to_string()));
    req.inherit_fds = vec![fd];
    assert_eq!(
        exit_code(&req),
        OK,
        "the service descriptor was not inherited"
    );
    drop(writer);
    let mut got = String::new();
    reader.read_to_string(&mut got).unwrap();
    assert_eq!(got, "ok");

    // An inheritable descriptor the request does not name stays behind.
    let (_reader, stray) = std::io::pipe().unwrap();
    let stray_fd = stray.as_raw_fd();
    // SAFETY: clearing FD_CLOEXEC on a descriptor this test owns.
    assert_eq!(unsafe { libc::fcntl(stray_fd, libc::F_SETFD, 0) }, 0);
    let leak = request_with(&f, &f.probe, "fd", OsString::from(stray_fd.to_string()));
    assert_eq!(exit_code(&leak), REFUSED, "an unnamed descriptor leaked");
}

#[test]
fn unsigned_tampered_and_self_entitled_code_is_never_app_sandbox() {
    let f = fixture();
    let trust = TrustRequirement::DeveloperId;

    let unsigned = f.payload.join("unsigned");
    std::fs::copy(&f.probe, &unsigned).unwrap();
    run_ok(
        Command::new("/usr/bin/codesign")
            .arg("--remove-signature")
            .arg(&unsigned),
    );
    assert_eq!(inspect(&unsigned, &trust), CodeSignature::Unsigned);
    // Launching ad-hoc signs it where the CPU requires a signature, and it
    // is confined either way.
    let secret = f.secret.as_os_str().to_owned();
    let mut app = launch(&request_with(&f, &unsigned, "read", secret)).unwrap();
    let expected = if cfg!(target_arch = "aarch64") {
        SeatbeltReason::AdHoc
    } else {
        SeatbeltReason::Unsigned
    };
    assert_eq!(app.mode(), MacLaunchMode::Seatbelt(expected));
    assert_eq!(app.wait().unwrap().code(), Some(REFUSED));

    let entitled = f.payload.join("entitled");
    std::fs::copy(&f.probe, &entitled).unwrap();
    let plist = f.root.join("entitlements.plist");
    std::fs::write(
        &plist,
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\"><dict>\
         <key>com.apple.security.app-sandbox</key><true/>\
         </dict></plist>\n",
    )
    .unwrap();
    run_ok(
        Command::new("/usr/bin/codesign")
            .args(["--force", "--sign", "-", "--entitlements"])
            .arg(&plist)
            .arg(&entitled),
    );
    assert_eq!(
        inspect(&entitled, &trust),
        CodeSignature::AdHoc { app_sandbox: true },
        "an ad-hoc signature's own entitlement claim"
    );
    assert_eq!(
        launcher_macos::classify(&entitled, &trust),
        MacLaunchMode::Seatbelt(SeatbeltReason::AdHoc)
    );

    let tampered = f.payload.join("tampered");
    let mut bytes = std::fs::read(&f.probe).unwrap();
    let marker = b"TILO_SEATBELT_PROBE_MARKER";
    let at = bytes
        .windows(marker.len())
        .position(|w| w == marker)
        .expect("marker in the probe");
    bytes[at] = b'X';
    std::fs::write(&tampered, &bytes).unwrap();
    assert!(
        matches!(inspect(&tampered, &trust), CodeSignature::Invalid { .. }),
        "{:?}",
        inspect(&tampered, &trust)
    );
    assert_eq!(
        launcher_macos::classify(&tampered, &trust),
        MacLaunchMode::Seatbelt(SeatbeltReason::InvalidSignature)
    );
}
