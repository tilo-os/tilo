//! The actual public CLI, invoked by `SystemdApply`, must share the session's mount namespace.
//! The fixture reports kernel peer identity; it does not replace admin authorization policy.
#![cfg(target_os = "linux")]

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::{BufRead, BufReader, Write};
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::{MetadataExt, symlink};
    use std::os::unix::net::UnixListener;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output};
    use std::time::{Duration, Instant};

    use tilo_vmd::apply::{Apply, Session};
    use tilo_vmd::slots::Login;
    use tilo_vmd::systemd::SystemdApply;

    const CASE: &str = "tests::control_runs_the_real_cli_in_the_live_session_namespace";
    const ROLE: &str = "TILO_CONTROL_NAMESPACE_TEST_ROLE";
    const ROOT: &str = "TILO_CONTROL_NAMESPACE_TEST_ROOT";

    fn namespace(pid: u32) -> (u64, u64) {
        let metadata = fs::metadata(format!("/proc/{pid}/ns/mnt")).unwrap();
        (metadata.dev(), metadata.ino())
    }

    fn session(root: &Path, wrapper: Option<u32>) -> Session {
        Session {
            slot: "owned".into(),
            unit: "unused-login-unit".into(),
            user: "root".into(),
            units: Vec::new(),
            control_socket: root.join("default.sock"),
            stand_in: false,
            login: Some(Login {
                session: None,
                wrapper,
            }),
        }
    }

    fn wrapped(role: &str, root: &Path) -> Command {
        let mut command = Command::new("timeout");
        command.args([
            "30",
            "bwrap",
            "--die-with-parent",
            "--bind",
            "/",
            "/",
            "--proc",
            "/proc",
        ]);
        command.args(["--setenv", ROLE, role, "--setenv", ROOT]);
        command.arg(root);
        command.arg(std::env::current_exe().unwrap());
        command.args([CASE, "--exact", "--nocapture"]);
        command
    }

    fn serve(root: &Path) {
        let pid = std::process::id();
        // session_pid recognizes the real login wrapper, not an arbitrary process supplied by a caller.
        fs::write(format!("/proc/self/task/{pid}/comm"), "tilo-session").unwrap();
        let listener = UnixListener::bind(root.join("default.sock")).unwrap();
        fs::write(root.join("ready"), pid.to_string()).unwrap();
        let (mut stream, _) = listener.accept().unwrap();
        // SAFETY: a zeroed ucred contains only integer fields and is a valid output buffer.
        let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
        let mut length = u32::try_from(std::mem::size_of::<libc::ucred>()).unwrap();
        // SAFETY: both out-pointers address initialized live objects of the requested size.
        assert_eq!(
            unsafe {
                libc::getsockopt(
                    stream.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_PEERCRED,
                    (&raw mut credentials).cast(),
                    &raw mut length,
                )
            },
            0
        );
        let mut line = String::new();
        BufReader::new(stream.try_clone().unwrap())
            .read_line(&mut line)
            .unwrap();
        let request: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(request["op"], "state_dump");
        let peer = u32::try_from(credentials.pid).unwrap();
        let answer = serde_json::json!({"state": {
            "peer_uid": credentials.uid,
            "same_mount": namespace(peer) == namespace(pid),
            "session_namespace": namespace(pid),
            "peer_namespace": namespace(peer),
        }});
        writeln!(stream, "{answer}").unwrap();
    }

    fn check_output(output: &Output) {
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn invoke_subject(root: &Path, explicit: bool) -> serde_json::Value {
        let ready = root.join("ready");
        let child = wrapped("session", root).spawn().unwrap();
        let deadline = Instant::now().checked_add(Duration::from_secs(10)).unwrap();
        while !ready.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        if !ready.exists() {
            check_output(&child.wait_with_output().unwrap());
            panic!("the private session did not become observable");
        }
        let pid = fs::read_to_string(&ready).unwrap().parse().unwrap();
        assert_ne!(namespace(pid), namespace(std::process::id()));
        let mut args = Vec::new();
        if explicit {
            args.extend([
                "--socket".into(),
                root.join("default.sock").display().to_string(),
            ]);
        }
        args.push("state-dump".into());
        let mut selected_session = session(root, Some(pid));
        if explicit {
            selected_session.control_socket = root.join("unused-default.sock");
        }
        let result = SystemdApply::new(root).control(&selected_session, &args);
        // Collect/join the owned producer before assertions, including the failing baseline.
        let child_output = child.wait_with_output().unwrap();
        assert!(
            child_output.status.success(),
            "private session outcome: {child_output:?}; canonical control outcome: {result:?}"
        );
        let result = result.unwrap();
        assert_eq!(result.exit_code, 0, "{}", result.stderr);
        serde_json::from_str(&result.stdout).unwrap()
    }

    fn subject(root: &Path) {
        for explicit in [false, true] {
            let row = root.join(if explicit { "explicit" } else { "default" });
            fs::create_dir(&row).unwrap();
            let result = invoke_subject(&row, explicit);
            assert_eq!(result["peer_uid"], 0);
            assert_eq!(
                result["same_mount"], true,
                "SystemdApply must enter the actual session namespace before invoking the real CLI: {result}"
            );
        }
        let error = SystemdApply::new(root)
            .control(&session(root, None), &["state-dump".into()])
            .expect_err("a missing session must refuse rather than use the daemon's namespace");
        assert!(error.to_string().contains("live session"));
    }

    #[test]
    fn control_runs_the_real_cli_in_the_live_session_namespace() {
        if let Ok(role) = std::env::var(ROLE) {
            let root = PathBuf::from(std::env::var(ROOT).unwrap());
            if role == "session" {
                serve(&root);
            } else {
                subject(&root);
            }
            return;
        }
        let root = tempfile::tempdir().unwrap();
        // All mount capabilities and /usr substitutions belong to an owned private namespace.
        let bin = root.path().join("bin");
        fs::create_dir(&bin).unwrap();
        for entry in fs::read_dir("/usr/bin").unwrap() {
            let entry = entry.unwrap();
            if entry.file_name() != "tilo-control" {
                symlink(
                    Path::new("/control-original-bin").join(entry.file_name()),
                    bin.join(entry.file_name()),
                )
                .unwrap();
            }
        }
        symlink(env!("CARGO_BIN_EXE_tilo-control"), bin.join("tilo-control")).unwrap();
        let output = Command::new("timeout")
            .args([
                "45",
                "bwrap",
                "--unshare-pid",
                "--unshare-net",
                "--unshare-ipc",
                "--unshare-uts",
                "--die-with-parent",
                "--cap-add",
                "CAP_SYS_ADMIN",
                "--cap-add",
                "CAP_SETGID",
                "--cap-add",
                "CAP_SETUID",
                "--bind",
                "/",
                "/",
                "--dev",
                "/dev",
                "--proc",
                "/proc",
                "--ro-bind",
                "/usr/bin",
                "/control-original-bin",
                "--ro-bind",
            ])
            .arg(&bin)
            .arg("/usr/bin")
            .args(["--setenv", ROLE, "subject", "--setenv", ROOT])
            .arg(root.path())
            .arg(std::env::current_exe().unwrap())
            .args([CASE, "--exact", "--nocapture"])
            .output()
            .unwrap();
        check_output(&output);
    }
}
