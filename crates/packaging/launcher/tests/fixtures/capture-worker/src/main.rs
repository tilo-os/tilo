use std::{
    fs,
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    os::unix::{fs::PermissionsExt, net::UnixListener},
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};
static STOP: AtomicBool = AtomicBool::new(false);
#[repr(C)]
struct Passwd {
    name: *mut std::ffi::c_char,
    password: *mut std::ffi::c_char,
    uid: u32,
    gid: u32,
    gecos: *mut std::ffi::c_char,
    home: *mut std::ffi::c_char,
    shell: *mut std::ffi::c_char,
}
#[repr(C)]
struct CapabilityHeader {
    version: u32,
    pid: i32,
}
#[repr(C)]
#[derive(Default)]
struct CapabilityData {
    effective: u32,
    permitted: u32,
    inheritable: u32,
}
unsafe extern "C" {
    fn unshare(flags: i32) -> i32;
    fn capget(header: *mut CapabilityHeader, data: *mut CapabilityData) -> i32;
    fn prctl(option: i32, ...) -> i32;
    fn getpwuid(uid: u32) -> *mut Passwd;
    fn fcntl(fd: i32, command: i32, ...) -> i32;
    fn getuid() -> u32;
    fn geteuid() -> u32;
    fn getgid() -> u32;
    fn getegid() -> u32;
    fn getgroups(size: i32, groups: *mut u32) -> i32;
    fn setfsuid(uid: u32) -> i32;
    fn setfsgid(gid: u32) -> i32;
    fn signal(number: i32, handler: extern "C" fn(i32)) -> usize;
}
extern "C" fn stop(_: i32) {
    STOP.store(true, Ordering::Relaxed);
}
fn main() {
    unsafe {
        signal(15, stop);
    }
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|arg| {
        arg == "--original-read" || arg == "--original-openpgp" || arg == "--original-mounted-read"
    }) {
        original_read(&args);
        return;
    }
    let uid: u32 = args[0].parse().unwrap();
    assert_eq!(unsafe { getuid() }, uid);
    assert_eq!(unsafe { geteuid() }, uid);
    assert_eq!(u32::try_from(unsafe { setfsuid(u32::MAX) }).unwrap(), uid);
    assert_eq!(
        fs::read("/run/tilo/migration/source/document").unwrap(),
        b"selected bytes"
    );
    assert!(
        fs::OpenOptions::new()
            .write(true)
            .open("/run/tilo/migration/source/document")
            .is_err()
    );
    assert!(fs::read(&args[1]).is_err());
    assert!(fs::read(&args[2]).is_err());
    assert!(fs::read("/run/tilo/migration/source/../source-secret").is_err());
    assert!(fs::read("/run/tilo/migration/output/../output-secret").is_err());
    for fd in [44, 45, 46, 47, 48, args[4].parse().unwrap()] {
        assert!(fs::read_link(format!("/proc/self/fd/{fd}")).is_err());
    }
    let address: SocketAddr = args[3].parse().unwrap();
    assert!(TcpStream::connect_timeout(&address, Duration::from_millis(100)).is_err());
    fs::write(
        "/run/tilo/migration/output/exported",
        b"actual exported bytes",
    )
    .unwrap();
    // Output is writable data, without execution authority.
    fs::copy(
        "/proc/self/exe",
        "/run/tilo/migration/output/forbidden-exec",
    )
    .unwrap();
    fs::set_permissions(
        "/run/tilo/migration/output/forbidden-exec",
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    assert!(
        std::process::Command::new("/run/tilo/migration/output/forbidden-exec")
            .arg("should-never-run")
            .spawn()
            .is_err()
    );
    let socket = "/run/tilo/app/provider.sock";
    let listener = UnixListener::bind(socket).unwrap();
    fs::set_permissions(socket, fs::Permissions::from_mode(0o600)).unwrap();
    listener.set_nonblocking(true).unwrap();
    while !STOP.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((mut stream, _)) => {
                let mut byte = [0];
                stream.read_exact(&mut byte).unwrap();
                stream.write_all(b"verified\n").unwrap();
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(5))
            }
            Err(error) => panic!("fixture socket: {error}"),
        }
    }
}

// Finite original-source read fixture; this is actual worker execution after
// canonical namespace construction and shim credential drop, not a ready flag.
fn original_read(args: &[String]) {
    use std::os::{
        fd::FromRawFd,
        unix::fs::{FileExt, MetadataExt},
    };
    // Observe the actual root-sealed digest inherited through canonical exec.
    let claim = unsafe { fs::File::from_raw_fd(50) };
    let metadata = claim.metadata().unwrap();
    assert_eq!(metadata.uid(), 0);
    assert_eq!(metadata.nlink(), 0);
    assert_eq!(metadata.len(), 32);
    assert_eq!(unsafe { fcntl(50, 1034) } & 15, 15);
    let mut digest = [0; 32];
    claim.read_exact_at(&mut digest, 0).unwrap();
    assert_eq!(digest, [7; 32]);
    drop(claim);
    assert_eq!(unsafe { getuid() }, 1000);
    assert_eq!(unsafe { geteuid() }, 1000);
    assert_eq!(unsafe { getgid() }, 1001);
    assert_eq!(unsafe { getegid() }, 1001);
    // Filesystem credentials govern DAC separately from effective UID/GID.
    assert_eq!(unsafe { setfsuid(u32::MAX) } as u32, 1000);
    assert_eq!(unsafe { setfsgid(u32::MAX) } as u32, 1001);
    // Actual libc lookup through the generated readonly account after source
    // UID drop, not a helper string assertion before entering the namespace.
    let account = unsafe { getpwuid(getuid()) };
    assert!(
        !account.is_null(),
        "source UID must have its sandbox account"
    );
    let account = unsafe { &*account };
    assert_eq!((account.uid, account.gid), (1000, 1001));
    assert_eq!(
        unsafe { std::ffi::CStr::from_ptr(account.name) }.to_bytes(),
        b"app"
    );
    assert_eq!(
        unsafe { std::ffi::CStr::from_ptr(account.home) }.to_bytes(),
        b"/run/tilo/app"
    );
    for path in [
        "/etc",
        "/etc/alternatives",
        "/run/tilo-runtime",
        "/var",
        "/var/tilo-apps",
        "/usr/share",
        "/usr/share/X11",
    ] {
        let metadata = fs::metadata(path).unwrap();
        assert_eq!(metadata.uid(), 0);
        assert_eq!(metadata.permissions().mode() & 0o7777, 0o755);
    }
    assert_eq!(
        fs::metadata("/etc/passwd").unwrap().permissions().mode() & 0o7777,
        0o444
    );
    let mut groups = [0; 1];
    assert_eq!(unsafe { getgroups(1, groups.as_mut_ptr()) }, 1);
    assert_eq!(groups, [1002]);
    assert_eq!(
        fs::read("/run/tilo/migration/source/group-only").unwrap(),
        b"source group bytes"
    );
    assert!(fs::write("/run/tilo/migration/source/group-only", b"changed").is_err());
    assert!(
        matches!(fs::metadata(&args[1]), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
    );
    assert!(
        matches!(fs::metadata("/run/tilo/migration/source/../source-secret"), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
    );
    assert!(fs::write("/run/tilo/migration/source/owner-writable", b"changed").is_err());
    assert!(fs::write("/run/tilo/migration/source/forbidden-new-file", b"changed").is_err());
    assert_eq!(
        fs::read("/run/tilo/migration/source/owner-writable").unwrap(),
        b"unchanged owner bytes"
    );
    // Query the actual kernel state without requiring access to proc files,
    // which the worker's mandatory Landlock policy deliberately restricts.
    for fd in [40, 41, 44, 45, 46, 47, 48, 49] {
        assert_eq!(
            unsafe { fcntl(fd, 1) },
            -1,
            "source descriptor {fd} survived"
        );
        assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(9));
    }
    let mut header = CapabilityHeader {
        version: 0x2008_0522,
        pid: 0,
    };
    let mut capabilities = [CapabilityData::default(), CapabilityData::default()];
    assert_eq!(unsafe { capget(&mut header, capabilities.as_mut_ptr()) }, 0);
    for data in capabilities {
        assert_eq!(
            (data.inheritable, data.permitted, data.effective),
            (0, 0, 0)
        );
    }
    for capability in 0..=40u64 {
        assert_eq!(unsafe { prctl(47, 1u64, capability, 0u64, 0u64) }, 0);
    }
    assert_eq!(unsafe { prctl(39, 0u64, 0u64, 0u64, 0u64) }, 1);
    // unshare(0) succeeds without privilege absent the full baseline. This
    // observes actual reinstatement, independently of the cleared capabilities.
    assert_eq!(unsafe { unshare(0) }, -1);
    assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(1));
    let archive = std::process::Command::new("/usr/bin/tar")
        .args([
            "--create",
            "--format=pax",
            "--file=-",
            "--directory=/run/tilo/migration/source",
            "group-only",
        ])
        .output()
        .unwrap();
    assert!(
        archive.status.success(),
        "canonical signed GNUtar could not read source groups"
    );
    assert!(
        archive
            .stdout
            .windows(b"source group bytes".len())
            .any(|window| window == b"source group bytes")
    );
    if args[0] == "--original-mounted-read" {
        use std::os::unix::{ffi::OsStrExt, fs::MetadataExt};
        let root = std::path::Path::new("/run/tilo/migration/source");
        assert_eq!(
            fs::read(root.join("mounted-directory/leaf")).unwrap(),
            b"group mounted bytes"
        );
        assert_eq!(
            fs::read(root.join("mounted-file")).unwrap(),
            b"group mounted bytes"
        );
        assert!(
            fs::symlink_metadata(root.join("mounted-directory"))
                .unwrap()
                .is_dir()
        );
        assert!(
            fs::symlink_metadata(root.join("mounted-file"))
                .unwrap()
                .is_file()
        );
        let raw = root.join(std::ffi::OsStr::from_bytes(b"raw-\xff"));
        let raw_metadata = fs::symlink_metadata(&raw).unwrap();
        assert!(raw_metadata.is_file());
        assert_eq!(raw_metadata.uid(), unsafe { getuid() });
        assert_eq!(raw_metadata.permissions().mode() & 0o600, 0o600);
        assert_eq!(fs::read(&raw).unwrap(), b"owner mounted bytes");
        assert!(fs::write(&raw, b"changed").is_err());
        assert_eq!(
            fs::read(root.join("mounted-directory/owner-writable")).unwrap(),
            b"owner mounted bytes"
        );
        let writable = root.join("mounted-directory/owner-writable");
        let metadata = fs::symlink_metadata(&writable).unwrap();
        assert_eq!(metadata.uid(), unsafe { getuid() });
        assert_eq!(metadata.permissions().mode() & 0o600, 0o600);
        assert!(fs::write(&writable, b"changed").is_err());
        assert_eq!(fs::read(&writable).unwrap(), b"owner mounted bytes");
        assert_eq!(fs::read(&raw).unwrap(), b"owner mounted bytes");
        for fd in 1800..2056 {
            assert_eq!(unsafe { fcntl(fd, 1) }, -1, "root input FD survived");
            assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(9));
        }
        let archive = std::process::Command::new("/usr/bin/tar")
            .args([
                "--create",
                "--format=pax",
                "--file=-",
                "--directory=/run/tilo/migration/source",
                "mounted-directory",
                "mounted-file",
            ])
            .arg(std::ffi::OsStr::from_bytes(b"raw-\xff"))
            .output()
            .unwrap();
        assert!(archive.status.success());
        assert!(
            archive
                .stdout
                .windows(b"group mounted bytes".len())
                .any(|w| w == b"group mounted bytes")
        );
        assert!(
            archive
                .stdout
                .windows(b"owner mounted bytes".len())
                .any(|w| w == b"owner mounted bytes")
        );
    }
    if args[0] == "--original-openpgp" {
        original_openpgp();
    }
    fs::write("/run/tilo/app/worker-proof", b"fresh private worker state").unwrap();
    let socket = "/run/tilo/app/provider.sock";
    let listener = UnixListener::bind(socket).unwrap();
    fs::set_permissions(socket, fs::Permissions::from_mode(0o600)).unwrap();
    let (mut stream, _) = listener.accept().unwrap();
    let mut request = [0];
    stream.read_exact(&mut request).unwrap();
    assert_eq!(request, [b'?']);
    stream.write_all(b"verified\n").unwrap();
}

// Actual signed helper transactions after original credentials/FD50/confinement.
fn original_openpgp() {
    use std::os::unix::fs::MetadataExt as _;
    use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
    use std::process::Command;
    // Retain the observed Unix denial; no /tmp permission is expanded.
    let temporary = fs::metadata("/tmp").unwrap();
    assert_eq!(temporary.uid(), 0);
    assert_eq!(temporary.gid(), 0);
    assert_eq!(temporary.mode() & 0o7777, 0o755);
    assert_eq!(
        fs::create_dir("/tmp/ostree-capsule-create-probe")
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::PermissionDenied,
    );
    // Equivalent to configure_ostree's admitted private TMPDIR, under the
    // actual original UID and fully enforced signed worker sandbox.
    let scratch = "/run/tilo/app/ostree-temp-probe";
    fs::DirBuilder::new().mode(0o700).create(scratch).unwrap();
    let owner = unsafe { geteuid() };
    let metadata = fs::metadata(scratch).unwrap();
    assert_eq!(metadata.uid(), owner);
    assert_eq!(metadata.mode() & 0o7777, 0o700);
    let proof_path = format!("{scratch}/write-proof");
    let mut proof = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&proof_path)
        .unwrap();
    proof.write_all(b"private temporary state").unwrap();
    let metadata = proof.metadata().unwrap();
    assert_eq!(metadata.uid(), owner);
    assert_eq!(metadata.mode() & 0o7777, 0o600);
    drop(proof);
    assert_eq!(fs::read(&proof_path).unwrap(), b"private temporary state");
    fs::remove_file(proof_path).unwrap();
    fs::remove_dir(scratch).unwrap();
    let boot_id = fs::read_to_string("/proc/sys/kernel/random/boot_id").unwrap();
    assert_eq!(boot_id.trim().len(), 36);
    assert!(
        boot_id
            .trim()
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
    );
    assert_eq!(
        fs::read("/proc/sys/kernel/hostname").unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied,
        "signed OSTree admission must not grant neighboring kernel metadata",
    );
    assert!(
        fs::OpenOptions::new()
            .write(true)
            .open("/proc/sys/kernel/random/boot_id")
            .is_err()
    );
    let source = "/run/tilo/migration/source";
    let commit = fs::read_to_string(format!("{source}/commit")).unwrap();
    let commit = commit.trim();
    assert_eq!(commit.len(), 64);
    assert!(commit.bytes().all(|byte| byte.is_ascii_hexdigit()));
    let signer = fs::read_to_string(format!("{source}/approved-key-id")).unwrap();
    let wrong = fs::read_to_string(format!("{source}/wrong-key-id")).unwrap();
    for key_id in [&signer, &wrong] {
        assert_eq!(key_id.len(), 16);
        assert!(key_id.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }
    assert!(
        signer != wrong,
        "wrong-key source must identify a distinct real exported key"
    );
    assert!(
        fs::read(format!("{source}/approved.asc")).unwrap()
            != fs::read(format!("{source}/wrong.asc")).unwrap(),
        "real exported public keys must be distinct",
    );
    for (name, key, accepted) in [
        ("valid", "approved.asc", true),
        ("wrong", "wrong.asc", false),
    ] {
        let repo = format!("/run/tilo/app/{name}-repo");
        fs::create_dir(&repo).unwrap();
        let run = |args: &[&str]| {
            Command::new("/usr/bin/ostree")
                .env_clear()
                .env("LC_ALL", "C")
                .env("LD_LIBRARY_PATH", "/run/tilo/migration/tools/lib")
                .env("HOME", "/run/tilo/app")
                .env("TMPDIR", "/run/tilo/app")
                .arg(format!("--repo={repo}"))
                .args(args)
                .output()
                .unwrap()
        };
        let initialized = run(&["init", "--mode=archive"]);
        // Only fixed synthetic public-key/commit commands use this diagnostic;
        // never emit stdout, key contents, or a caller-supplied command line.
        let diagnostic = |output: &std::process::Output| {
            let stderr = String::from_utf8_lossy(&output.stderr[..output.stderr.len().min(2048)])
                .replace(&repo, "[synthetic-repository]")
                .replace(source, "[synthetic-source]")
                .replace("/run/tilo", "[sandbox]")
                .chars()
                .filter(|ch| !ch.is_control() || matches!(ch, '\n' | '\t'))
                .collect::<String>();
            format!(
                "status={} stderr_bytes={} stderr_truncated={} stderr={}",
                output.status,
                output.stderr.len(),
                output.stderr.len() > 2048,
                stderr,
            )
        };
        assert!(
            initialized.status.success(),
            "synthetic OSTree init refused: {}",
            diagnostic(&initialized),
        );
        let import = format!("--gpg-import={source}/{key}");
        let origin = format!("file://{source}/repo");
        let imported = run(&[
            "remote",
            "add",
            &import,
            "--set=gpg-verify=true",
            "fixture",
            &origin,
        ]);
        assert!(
            imported.status.success(),
            "synthetic public-key remote import refused: {}",
            diagnostic(&imported),
        );
        // Same canonical flags as portable/export.rs::verify_carrier.
        let result = run(&[
            "pull-local",
            "--untrusted",
            "--gpg-verify",
            "--remote=fixture",
            &format!("{source}/repo"),
            commit,
        ]);
        assert_eq!(
            result.status.success(),
            accepted,
            "signed OpenPGP acceptance differs: {}",
            diagnostic(&result),
        );
        if accepted {
            let shown = run(&["cat", commit, "/document"]);
            assert!(
                shown.status.success(),
                "synthetic signed commit read refused: {}",
                diagnostic(&shown),
            );
            assert_eq!(shown.stdout, b"signed fixture document");
        } else {
            // OSTree's canonical missing-key diagnostic is emitted by
            // ostree_gpg_verify_result_describe; it contains no literal "gpg".
            // Require its full observed refusal for this exact signed commit
            // and the approved real key, not any generic command failure.
            assert_eq!(result.status.code(), Some(1));
            assert!(result.stderr.len() <= 4096);
            let reason = String::from_utf8(result.stderr.clone())
                .unwrap()
                .to_ascii_lowercase();
            let lines: Vec<_> = reason.lines().collect();
            assert_eq!(lines.len(), 2);
            assert!(
                lines[0].starts_with(&format!("error: commit {commit}: signature made "))
                    && lines[0].ends_with(&format!(
                        " using eddsa key id {}",
                        signer.to_ascii_lowercase()
                    ))
                    && lines[1] == "can't check signature: public key not found",
                "wrong-key refusal must identify exact signed commit and missing approved key: {}",
                diagnostic(&result),
            );
        }
    }
    assert!(fs::write(format!("{source}/repo/forbidden"), b"mutation").is_err());
}
