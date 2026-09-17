#![cfg(unix)]
use std::{
    os::fd::AsRawFd,
    os::unix::{fs::PermissionsExt, net::UnixStream, process::CommandExt},
    process::Command,
};

fn child() -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "bootstrap_child", "--nocapture"])
        .env("QOS_BOOTSTRAP_TEST_CHILD", "1")
        .env_remove("QOS_SVC_COLLAB_FD")
        .env_remove("QOS_SVC_IDENTITY_FD")
        .env_remove("QOS_RUNTIME_STANDALONE");
    command
}
#[test]
fn bootstrap_child() {
    if std::env::var_os("QOS_BOOTSTRAP_TEST_CHILD").is_none() {
        return;
    }
    match runtime_portal::native::ensure_attached("dev.qos.bootstrap-test") {
        Ok(()) => println!("RETURNED"),
        Err(error) => {
            eprintln!("BOOTSTRAP ERROR: {error}");
            std::process::exit(21);
        }
    }
}
#[test]
fn malformed_inherited_connection_never_starts_another_host() {
    let output = child()
        .env("QOS_SVC_COLLAB_FD", "bad")
        .env("QOS_RUNTIME_BINARY", "/must-not-run")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(21));
    assert!(String::from_utf8_lossy(&output.stderr).contains("inherited"));
}
#[test]
fn explicit_standalone_bypasses_host_lookup() {
    let output = child()
        .env("QOS_RUNTIME_STANDALONE", "1")
        .env("QOS_RUNTIME_BINARY", "/must-not-run")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("RETURNED"));
}
#[test]
fn complete_inherited_connections_take_precedence_over_profile_and_binary() {
    let (a, _a_peer) = UnixStream::pair().unwrap();
    let (b, _b_peer) = UnixStream::pair().unwrap();
    let fds = [a.as_raw_fd(), b.as_raw_fd()];
    let mut command = child();
    command
        .env("QOS_SVC_COLLAB_FD", fds[0].to_string())
        .env("QOS_SVC_IDENTITY_FD", fds[1].to_string())
        .env("QOS_RUNTIME_PROFILE", "relative-invalid")
        .env("QOS_RUNTIME_BINARY", "/must-not-run");
    unsafe {
        command.pre_exec(move || {
            for fd in fds {
                libc::fcntl(fd, libc::F_SETFD, 0);
            }
            Ok(())
        });
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
#[test]
fn bootstrap_exec_preserves_original_arguments_and_working_directory() {
    let root = std::env::temp_dir().join(format!("qos-bootstrap-test-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let host = root.join("host");
    std::fs::write(&host, "#!/bin/sh\nprintf '%s\\n' \"$PWD\" \"$@\"\n").unwrap();
    std::fs::set_permissions(&host, std::fs::Permissions::from_mode(0o700)).unwrap();
    let output = child()
        .current_dir(&root)
        .env("QOS_RUNTIME_BINARY", &host)
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("--run-app\ndev.qos.bootstrap-test\n--\n"),
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
#[test]
fn aliased_inherited_descriptors_are_rejected_before_sdk_ownership() {
    let (socket, _peer) = UnixStream::pair().unwrap();
    let fd = socket.as_raw_fd();
    let mut command = child();
    command
        .env("QOS_SVC_COLLAB_FD", fd.to_string())
        .env("QOS_SVC_IDENTITY_FD", format!("0{fd}"));
    unsafe {
        command.pre_exec(move || {
            libc::fcntl(fd, libc::F_SETFD, 0);
            Ok(())
        });
    }
    let output = command.output().unwrap();
    assert_eq!(output.status.code(), Some(21));
}

#[test]
fn missing_developer_host_is_built_once_for_concurrent_launches() {
    let root = std::env::temp_dir().join(format!("qos-native-build-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("bin")).unwrap();
    let cargo = root.join("bin/cargo");
    std::fs::write(&cargo, "#!/bin/sh\n[ \"$2\" = \"--offline\" ] || exit 17\nprintf 'build\\n' >> \"$CARGO_TARGET_DIR/build-calls\"\n/bin/mkdir -p \"$CARGO_TARGET_DIR/debug\"\nprintf '#!/bin/sh\\nprintf BUILT\\\\n\\n' > \"$CARGO_TARGET_DIR/debug/qos-runtime-mcp\"\n/bin/chmod 700 \"$CARGO_TARGET_DIR/debug/qos-runtime-mcp\"\n").unwrap();
    std::fs::set_permissions(&cargo, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut children: Vec<_> = (0..3)
        .map(|_| {
            child()
                .env_remove("QOS_RUNTIME_BINARY")
                .env("PATH", root.join("bin"))
                .env("CARGO_TARGET_DIR", root.join("target"))
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    for child in children.drain(..) {
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("BUILT"));
    }
    assert_eq!(
        std::fs::read_to_string(root.join("target/build-calls")).unwrap(),
        "build\n"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn developer_build_failure_is_visible_and_does_not_run_standalone() {
    let root = std::env::temp_dir().join(format!("qos-native-build-fail-{}", std::process::id()));
    std::fs::create_dir_all(root.join("bin")).unwrap();
    let cargo = root.join("bin/cargo");
    std::fs::write(&cargo, "#!/bin/sh\nexit 9\n").unwrap();
    std::fs::set_permissions(&cargo, std::fs::Permissions::from_mode(0o700)).unwrap();
    let output = child()
        .env_remove("QOS_RUNTIME_BINARY")
        .env("PATH", root.join("bin"))
        .env("CARGO_TARGET_DIR", root.join("target"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(21));
    assert!(String::from_utf8_lossy(&output.stderr).contains("native runtime build failed"));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn inherited_datagram_is_rejected_as_wrong_sdk_transport() {
    let (socket, _peer) = std::os::unix::net::UnixDatagram::pair().unwrap();
    let (stream, _stream_peer) = UnixStream::pair().unwrap();
    let fds = [socket.as_raw_fd(), stream.as_raw_fd()];
    let mut command = child();
    command
        .env("QOS_SVC_COLLAB_FD", fds[0].to_string())
        .env("QOS_SVC_IDENTITY_FD", fds[1].to_string());
    unsafe {
        command.pre_exec(move || {
            for fd in fds {
                libc::fcntl(fd, libc::F_SETFD, 0);
            }
            Ok(())
        });
    }
    assert_eq!(command.output().unwrap().status.code(), Some(21));
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
