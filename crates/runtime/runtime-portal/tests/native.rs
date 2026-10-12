#![cfg(unix)]
use std::{
    os::fd::AsRawFd,
    os::unix::{fs::PermissionsExt, net::UnixStream, process::CommandExt},
    process::Command,
};

#[expect(
    clippy::unwrap_used,
    reason = "test helper: a test binary that cannot name itself cannot run these tests"
)]
fn child() -> Command {
    child_at(std::env::current_exe().unwrap())
}

fn child_at(executable: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut command = Command::new(executable);
    command
        .args(["--exact", "bootstrap_child", "--nocapture"])
        .env("TILO_BOOTSTRAP_TEST_CHILD", "1")
        .env_remove("TILO_SVC_COLLAB_FD")
        .env_remove("TILO_SVC_IDENTITY_FD")
        .env_remove("TILO_RUNTIME_STANDALONE")
        .env_remove("TILO_RUNTIME_CONNECTION_OWNER_PID")
        .env_remove("TILO_RUNTIME_BINARY")
        .env_remove("XDG_CURRENT_DESKTOP");
    command
}

#[test]
fn source_profile_host_lookup_keeps_installed_and_desktop_boundaries() {
    let root = std::env::temp_dir().join(format!("tilo-host-profiles-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir_all(root.join("target/thin-app")).unwrap();
    std::fs::create_dir_all(root.join("target/debug")).unwrap();
    std::fs::create_dir_all(root.join("crates/runtime/tilo-runtime")).unwrap();
    let app = root.join("target/thin-app/ebook");
    std::fs::copy(std::env::current_exe().unwrap(), &app).unwrap();
    let host = root.join("target/debug/tilo-runtime");
    std::fs::write(
        &host,
        "#!/bin/sh\nprintf 'SOURCE HOST\\n'; printf '%s\\n' \"$@\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&host, std::fs::Permissions::from_mode(0o700)).unwrap();
    let output = child_at(&app).env("PATH", &root).output().unwrap();
    assert!(
        !output.status.success(),
        "unmarked directories are not a checkout"
    );
    std::fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
    std::fs::write(
        root.join("crates/runtime/tilo-runtime/Cargo.toml"),
        "[package]\n",
    )
    .unwrap();
    let output = child_at(&app).env("PATH", &root).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains("SOURCE HOST\n--run-app\ndev.tilo.bootstrap-test")
    );
    let output = child_at(&app)
        .env("PATH", &root)
        .env("XDG_CURRENT_DESKTOP", "tilo")
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "the desktop never selects the standalone host"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("-p shell --bin tilo-runtime-mcp"));
    std::fs::remove_file(&host).unwrap();
    let output = child_at(&app).env("PATH", &root).output().unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("cargo build --locked -p tilo-runtime")
    );
    std::fs::write(&host, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&host, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::create_dir(root.join("bin")).unwrap();
    let installed = root.join("bin/ebook");
    std::fs::hard_link(&app, &installed).unwrap();
    let output = child_at(&installed).env("PATH", &root).output().unwrap();
    assert!(
        !output.status.success(),
        "an installed app never searches Cargo profiles"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("beside the app or on PATH"));
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn bootstrap_child() {
    if std::env::var_os("TILO_BOOTSTRAP_TEST_CHILD").is_none() {
        return;
    }
    if std::env::var_os("TILO_BOOTSTRAP_LOOKUP_ONLY").is_some() {
        assert!(runtime_portal::service_fd("collab").is_none());
        assert!(runtime_portal::service_fd("identity").is_none());
        assert!(!runtime_portal::is_tilo_native());
        return;
    }
    match runtime_portal::native::ensure_attached("dev.tilo.bootstrap-test") {
        Ok(()) => println!("RETURNED"),
        Err(error) => {
            eprintln!("BOOTSTRAP ERROR: {error}");
            #[expect(
                clippy::exit,
                reason = "the re-executed child reports failure to the parent test through its exit status"
            )]
            std::process::exit(21);
        }
    }
}
/// Let `fds` survive into the child's exec (they are close-on-exec by default).
fn inherit(command: &mut Command, fds: Vec<std::os::fd::RawFd>) {
    unsafe {
        command.pre_exec(move || {
            for &fd in &fds {
                libc::fcntl(fd, libc::F_SETFD, 0);
            }
            Ok(())
        });
    }
}

#[test]
fn invalid_inherited_connections_fail_closed() {
    // A malformed inherited connection never starts another host.
    let output = child()
        .env("TILO_SVC_COLLAB_FD", "bad")
        .env("TILO_RUNTIME_BINARY", "/must-not-run")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(21), "case malformed");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("inherited"),
        "case malformed"
    );

    // Aliased inherited descriptors are rejected before SDK ownership.
    {
        let (socket, _peer) = UnixStream::pair().unwrap();
        let fd = socket.as_raw_fd();
        let mut command = child();
        command
            .env("TILO_SVC_COLLAB_FD", fd.to_string())
            .env("TILO_SVC_IDENTITY_FD", format!("0{fd}"));
        inherit(&mut command, vec![fd]);
        let output = command.output().unwrap();
        assert_eq!(output.status.code(), Some(21), "case aliased");
    }

    // An inherited datagram is rejected as the wrong SDK transport.
    {
        let (socket, _peer) = std::os::unix::net::UnixDatagram::pair().unwrap();
        let (stream, _stream_peer) = UnixStream::pair().unwrap();
        let fds = [socket.as_raw_fd(), stream.as_raw_fd()];
        let mut command = child();
        command
            .env("TILO_SVC_COLLAB_FD", fds[0].to_string())
            .env("TILO_SVC_IDENTITY_FD", fds[1].to_string());
        inherit(&mut command, fds.to_vec());
        assert_eq!(
            command.output().unwrap().status.code(),
            Some(21),
            "case datagram"
        );
    }

    // Current and unknown transfer provenance fail closed.
    let mut current = Command::new("/bin/sh");
    current
        .args([
            "-c",
            "export TILO_RUNTIME_CONNECTION_OWNER_PID=$$; exec \"$@\"",
            "provenance-test",
        ])
        .arg(std::env::current_exe().unwrap())
        .args(["--exact", "bootstrap_child", "--nocapture"])
        .env("TILO_BOOTSTRAP_TEST_CHILD", "1")
        .env("TILO_SVC_COLLAB_FD", "bad")
        .env_remove("TILO_SVC_IDENTITY_FD")
        .env("TILO_RUNTIME_STANDALONE", "1");
    assert_eq!(
        current.output().unwrap().status.code(),
        Some(21),
        "case current provenance"
    );
    for owner in ["unknown", "0"] {
        let output = child()
            .env("TILO_RUNTIME_CONNECTION_OWNER_PID", owner)
            .env("TILO_RUNTIME_STANDALONE", "1")
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(21), "case owner {owner}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("inherited"),
            "case owner {owner}"
        );
    }
}
#[test]
fn bootstrap_attaches_standalone_inherited_or_through_the_host() {
    // Explicit standalone bypasses host lookup.
    let output = child()
        .env("TILO_RUNTIME_STANDALONE", "1")
        .env("TILO_RUNTIME_BINARY", "/must-not-run")
        .output()
        .unwrap();
    assert!(output.status.success(), "case explicit standalone");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("RETURNED"),
        "case explicit standalone"
    );

    // Complete inherited connections take precedence over profile and binary.
    {
        let (a, _a_peer) = UnixStream::pair().unwrap();
        let (b, _b_peer) = UnixStream::pair().unwrap();
        let fds = [a.as_raw_fd(), b.as_raw_fd()];
        let mut command = child();
        command
            .env("TILO_SVC_COLLAB_FD", fds[0].to_string())
            .env("TILO_SVC_IDENTITY_FD", fds[1].to_string())
            .env("TILO_RUNTIME_PROFILE", "relative-invalid")
            .env("TILO_RUNTIME_BINARY", "/must-not-run");
        inherit(&mut command, fds.to_vec());
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "case complete inherited: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    // A launcher launch hands over only the services it has: an app that
    // declares no collaboration service gets just its identity broker, and
    // that is a complete inherited attachment, not a reason to start a host.
    for service in ["TILO_SVC_IDENTITY_FD", "TILO_SVC_COLLAB_FD"] {
        let (socket, _peer) = UnixStream::pair().unwrap();
        let fd = socket.as_raw_fd();
        let mut command = child();
        command
            .env(service, fd.to_string())
            .env("TILO_RUNTIME_BINARY", "/must-not-run");
        inherit(&mut command, vec![fd]);
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "case only {service}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    // Bootstrap exec preserves the original arguments and working directory.
    {
        let root = std::env::temp_dir().join(format!("tilo-bootstrap-test-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let host = root.join("host");
        std::fs::write(&host, "#!/bin/sh\nprintf '%s\\n' \"$PWD\" \"$@\"\n").unwrap();
        std::fs::set_permissions(&host, std::fs::Permissions::from_mode(0o700)).unwrap();
        let output = child()
            .current_dir(&root)
            .env("TILO_RUNTIME_BINARY", &host)
            .output()
            .unwrap();
        assert!(output.status.success(), "case host exec");
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(
            stdout.contains("--run-app\ndev.tilo.bootstrap-test\n--\n"),
            "{stdout}"
        );
        assert!(
            stdout.contains("--exact\nbootstrap_child\n--nocapture"),
            "{stdout}"
        );
        assert!(stdout.contains(root.to_str().unwrap()));
        assert!(!stdout.contains("RETURNED"));
        std::fs::remove_dir_all(root).unwrap();
    }

    // Ancestor-owned descriptors are hidden even when bootstrap is skipped.
    {
        let (a, _a_peer) = UnixStream::pair().unwrap();
        let (b, _b_peer) = UnixStream::pair().unwrap();
        let fds = [a.as_raw_fd(), b.as_raw_fd()];
        let mut command = child();
        command
            .env("TILO_BOOTSTRAP_LOOKUP_ONLY", "1")
            .env("TILO_WORKSPACE_ID", "ancestor-workspace")
            .env(
                "TILO_RUNTIME_CONNECTION_OWNER_PID",
                std::process::id().to_string(),
            )
            .env("TILO_SVC_COLLAB_FD", fds[0].to_string())
            .env("TILO_SVC_IDENTITY_FD", fds[1].to_string());
        inherit(&mut command, fds.to_vec());
        assert!(command.status().unwrap().success(), "case ancestor hidden");
    }

    // A standalone child ignores ancestor metadata without mutating it.
    let output = child()
        .env(
            "TILO_RUNTIME_CONNECTION_OWNER_PID",
            std::process::id().to_string(),
        )
        .env("TILO_SVC_COLLAB_FD", "closed-ancestor-fd")
        .env("TILO_RUNTIME_STANDALONE", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "case standalone ignores ancestor: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn adopting_service_descriptor_restores_close_on_exec() {
    use std::os::fd::IntoRawFd;
    let (socket, _peer) = UnixStream::pair().unwrap();
    let raw = socket.into_raw_fd();
    unsafe {
        assert_eq!(libc::fcntl(raw, libc::F_SETFD, 0), 0);
    }
    let client = runtime_portal::UnixCapnpClient::from_connected_fd(
        raw,
        capnp_rpc::rpc_twoparty_capnp::Side::Client,
    )
    .unwrap();
    assert_ne!(
        unsafe { libc::fcntl(raw, libc::F_GETFD) } & libc::FD_CLOEXEC,
        0,
        "SDK-owned service leaked into subsequent execs"
    );
    drop(client);
}

#[test]
fn the_host_is_the_standalone_runtime_outside_the_tilo_desktop_and_the_sessions_inside_it() {
    let root = std::env::temp_dir().join(format!("tilo-host-choice-test-{}", std::process::id()));
    let both = root.join("both");
    let standalone_only = root.join("standalone-only");
    for directory in [&both, &standalone_only] {
        std::fs::create_dir_all(directory).unwrap();
    }
    let host = |directory: &std::path::Path, name: &str| {
        let path = directory.join(name);
        std::fs::write(&path, format!("#!/bin/sh\necho HOST {name}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        path
    };
    host(&both, "tilo-runtime");
    host(&both, "tilo-runtime-mcp");
    host(&standalone_only, "tilo-runtime");
    let explicit = host(&root, "explicit-host");
    let run = |path: &std::path::Path, desktop: Option<&str>, binary: Option<&std::path::Path>| {
        let mut command = child();
        command.env("PATH", path);
        if let Some(desktop) = desktop {
            command.env("XDG_CURRENT_DESKTOP", desktop);
        }
        if let Some(binary) = binary {
            command.env("TILO_RUNTIME_BINARY", binary);
        }
        command.output().unwrap()
    };
    // The re-executed test prints libtest's header before the host runs.
    let chose = |output: &std::process::Output| {
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .find_map(|line| line.strip_prefix("HOST ").map(str::to_owned))
    };

    // Stock macOS or a foreign Linux desktop: the standalone runtime first.
    let foreign = run(&both, None, None);
    assert_eq!(
        chose(&foreign).as_deref(),
        Some("tilo-runtime"),
        "case foreign"
    );
    let other_desktop = run(&both, Some("GNOME"), None);
    assert_eq!(
        chose(&other_desktop).as_deref(),
        Some("tilo-runtime"),
        "case GNOME"
    );
    // Inside the Tilo desktop: the session's host.
    let tilo = run(&both, Some("tilo"), None);
    assert_eq!(
        chose(&tilo).as_deref(),
        Some("tilo-runtime-mcp"),
        "case tilo"
    );
    // ...and never a second authority beside it.
    let missing = run(&standalone_only, Some("tilo"), None);
    assert_eq!(
        missing.status.code(),
        Some(21),
        "case tilo without its host"
    );
    assert!(
        String::from_utf8_lossy(&missing.stderr).contains("tilo-runtime-mcp"),
        "case tilo without its host"
    );
    // The explicit binary overrides either choice.
    for desktop in [None, Some("tilo")] {
        let chosen = run(&both, desktop, Some(&explicit));
        assert_eq!(
            chose(&chosen).as_deref(),
            Some("explicit-host"),
            "case explicit {desktop:?}"
        );
    }
    std::fs::remove_dir_all(root).unwrap();
}
