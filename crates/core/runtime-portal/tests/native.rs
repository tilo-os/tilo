#![cfg(unix)]
use std::{
    os::fd::AsRawFd,
    os::unix::{fs::PermissionsExt, net::UnixStream, process::CommandExt},
    process::Command,
};
use tilo_test_env::SpawnUnderEnvLock as _;

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
        .env_remove("TILO_RUNTIME_CONNECTION_OWNER_PID")
        .env_remove("TILO_RUNTIME_BINARY")
        .env_remove("XDG_CURRENT_DESKTOP");
    command
}

tilo_test_env::scenario! {
    fn host_lookup;

    phase source_profile_host_lookup_keeps_installed_and_desktop_boundaries;
    {
        let root = std::env::temp_dir().join(format!("tilo-host-profiles-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir_all(root.join("target/thin-app")).unwrap();
        std::fs::create_dir_all(root.join("target/debug")).unwrap();
        std::fs::create_dir_all(root.join("crates/core/tilo-runtime")).unwrap();
        let app = root.join("target/thin-app/ebook");
        std::fs::copy(std::env::current_exe().unwrap(), &app).unwrap();
        let host = root.join("target/debug/tilo-runtime");
        std::fs::write(
            &host,
            "#!/bin/sh\nprintf 'SOURCE HOST\\n'; printf '%s\\n' \"$@\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&host, std::fs::Permissions::from_mode(0o700)).unwrap();
        let output = child_at(&app).env("PATH", &root).output_locked().unwrap();
        assert!(
            !output.status.success(),
            "unmarked directories are not a checkout"
        );
        std::fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
        std::fs::write(
            root.join("crates/core/tilo-runtime/Cargo.toml"),
            "[package]\n",
        )
        .unwrap();
        let output = child_at(&app).env("PATH", &root).output_locked().unwrap();
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
            .output_locked()
            .unwrap();
        assert!(
            String::from_utf8_lossy(&output.stdout)
                .contains("SOURCE HOST\n--run-app\ndev.tilo.bootstrap-test"),
            "the desktop attaches to the same host: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        std::fs::remove_file(&host).unwrap();
        let output = child_at(&app).env("PATH", &root).output_locked().unwrap();
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("cargo build --locked -p tilo-daemon")
        );
        std::fs::write(&host, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&host, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::create_dir(root.join("bin")).unwrap();
        let installed = root.join("bin/ebook");
        std::fs::hard_link(&app, &installed).unwrap();
        let output = child_at(&installed)
            .env("PATH", &root)
            .output_locked()
            .unwrap();
        assert!(
            !output.status.success(),
            "an installed app never searches Cargo profiles"
        );
        assert!(String::from_utf8_lossy(&output.stderr).contains("beside the app or on PATH"));
        std::fs::remove_dir_all(root).unwrap();
    }

    phase the_host_is_tilo_runtime_on_the_tilo_desktop_and_off_it;
    {
        let root = std::env::temp_dir().join(format!("tilo-host-choice-test-{}", std::process::id()));
        let installed = root.join("installed");
        let empty = root.join("empty");
        for directory in [&installed, &empty] {
            std::fs::create_dir_all(directory).unwrap();
        }
        let host = |directory: &std::path::Path, name: &str| {
            let path = directory.join(name);
            std::fs::write(&path, format!("#!/bin/sh\necho HOST {name}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
            path
        };
        host(&installed, "tilo-runtime");
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
            command.output_locked().unwrap()
        };
        // The re-executed test prints libtest's progress before the host runs, with no line break
        // between: the host's answer is read from where its marker stands, not from a line of its own.
        let chose = |output: &std::process::Output| {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let (_, answer) = stdout.split_once("HOST ")?;
            Some(answer.lines().next().unwrap_or_default().to_owned())
        };
        // A case that went wrong says what the child did, not only which case it was.
        let why = |output: &std::process::Output| {
            format!(
                "status {:?}, out {:?}, err {:?}",
                output.status.code(),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        };

        for desktop in [None, Some("GNOME"), Some("tilo")] {
            let chosen = run(&installed, desktop, None);
            assert_eq!(
                chose(&chosen).as_deref(),
                Some("tilo-runtime"),
                "case {desktop:?}: {}",
                why(&chosen)
            );
        }
        let missing = run(&empty, Some("tilo"), None);
        assert_eq!(missing.status.code(), Some(21), "case no host: {}", why(&missing));
        assert!(
            String::from_utf8_lossy(&missing.stderr).contains("install tilo-runtime"),
            "case no host: {}",
            why(&missing)
        );
        for desktop in [None, Some("tilo")] {
            let chosen = run(&installed, desktop, Some(&explicit));
            assert_eq!(
                chose(&chosen).as_deref(),
                Some("explicit-host"),
                "case explicit {desktop:?}"
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }
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

tilo_test_env::scenario! {
    fn bootstrap_attachment;

    phase invalid_inherited_connections_fail_closed;
    {
        // A malformed inherited connection never starts another host.
        let output = child()
            .env("TILO_SVC_COLLAB_FD", "bad")
            .env("TILO_RUNTIME_BINARY", "/must-not-run")
            .output_locked()
            .unwrap();
        assert_eq!(output.status.code(), Some(21), "case malformed");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("inherited"),
            "case malformed"
        );

        {
            let (socket, _peer) = UnixStream::pair().unwrap();
            let fd = socket.as_raw_fd();
            let mut command = child();
            command
                .env("TILO_SVC_COLLAB_FD", fd.to_string())
                .env("TILO_SVC_IDENTITY_FD", format!("0{fd}"));
            inherit(&mut command, vec![fd]);
            let output = command.output_locked().unwrap();
            assert_eq!(output.status.code(), Some(21), "case aliased");
        }

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
                command.output_locked().unwrap().status.code(),
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
            .env_remove("TILO_SVC_IDENTITY_FD");
        assert_eq!(
            current.output_locked().unwrap().status.code(),
            Some(21),
            "case current provenance"
        );
        for owner in ["unknown", "0"] {
            let output = child()
                .env("TILO_RUNTIME_CONNECTION_OWNER_PID", owner)
                .output_locked()
                .unwrap();
            assert_eq!(output.status.code(), Some(21), "case owner {owner}");
            assert!(
                String::from_utf8_lossy(&output.stderr).contains("inherited"),
                "case owner {owner}"
            );
        }
    }

    phase bootstrap_attaches_inherited_or_through_the_host;
    {
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
            let output = command.output_locked().unwrap();
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
            let output = command.output_locked().unwrap();
            assert!(
                output.status.success(),
                "case only {service}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    phase bootstrap_exec_names_only_the_app_and_hides_ancestor_descriptors;
    {
        {
            let root = std::env::temp_dir().join(format!("tilo-bootstrap-test-{}", std::process::id()));
            std::fs::create_dir_all(&root).unwrap();
            let host = root.join("host");
            std::fs::write(&host, "#!/bin/sh\nprintf '%s\\n' \"$PWD\" \"$@\"\n").unwrap();
            std::fs::set_permissions(&host, std::fs::Permissions::from_mode(0o700)).unwrap();
            let output = child()
                .current_dir(&root)
                .env("TILO_RUNTIME_BINARY", &host)
                .output_locked()
                .unwrap();
            assert!(output.status.success(), "case host exec");
            let stdout = String::from_utf8(output.stdout).unwrap();
            // The host is asked for the app, and no program of this process's: what runs is the
            // app this computer has installed.
            assert!(
                stdout.contains("--run-app\ndev.tilo.bootstrap-test"),
                "{stdout}"
            );
            assert!(!stdout.contains("\n--\n"), "{stdout}");
            assert!(!stdout.contains("--exact"), "{stdout}");
            assert!(stdout.contains(root.to_str().unwrap()));
            assert!(!stdout.contains("RETURNED"));
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    phase bootstrap_hides_ancestor_metadata_and_a_stale_connection_attaches;
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
        assert!(
            command.status_locked().unwrap().success(),
            "case ancestor hidden"
        );

        let root = std::env::temp_dir().join(format!("tilo-bootstrap-attach-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let host = root.join("host");
        std::fs::write(&host, "#!/bin/sh\nprintf 'ATTACHED\\n'\n").unwrap();
        std::fs::set_permissions(&host, std::fs::Permissions::from_mode(0o700)).unwrap();
        let output = child()
            .env(
                "TILO_RUNTIME_CONNECTION_OWNER_PID",
                std::process::id().to_string(),
            )
            .env("TILO_SVC_COLLAB_FD", "closed-ancestor-fd")
            .env("TILO_RUNTIME_BINARY", &host)
            .output_locked()
            .unwrap();
        assert!(
            output.status.success(),
            "case stale connection attaches: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("ATTACHED"),
            "a stale descriptor name is not a connection this process received"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}

tilo_test_env::scenario! {
    fn a_second_instance_is_asked_of_the_daemon;

    phase an_app_asks_over_the_native_connection_the_launcher_handed_it;
    {
        use std::io::{BufRead as _, BufReader, Write as _};
        let (ours, theirs) = UnixStream::pair().unwrap();
        let fd = std::os::fd::IntoRawFd::into_raw_fd(ours);
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_SVC_NATIVE_FD", fd.to_string());
        env.set("TILO_WORKSPACE_ID", "workspace-one");
        let answering = std::thread::spawn(move || {
            let mut reader = BufReader::new(theirs);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            reader
                .into_inner()
                .write_all(b"{\"started\":true}\n")
                .unwrap();
            line
        });
        runtime_portal::native::launch_another_instance("dev.tilo.second")
            .expect("the daemon started the instance");
        let request: serde_json::Value =
            serde_json::from_str(&answering.join().unwrap()).expect("the request the app sent");
        assert_eq!(
            request,
            serde_json::json!({
                "native": "launch",
                "app_id": "dev.tilo.second",
                "workspace": "workspace-one",
                "presentation": false,
            })
        );
        // SAFETY: the call borrowed this descriptor and never owned it; nothing else uses it now.
        assert_eq!(unsafe { libc::close(fd) }, 0);
    }

    phase a_refusal_says_why_and_an_unlaunched_program_says_how_to_run;
    {
        use std::io::{BufRead as _, BufReader, Write as _};
        let (ours, theirs) = UnixStream::pair().unwrap();
        let fd = std::os::fd::IntoRawFd::into_raw_fd(ours);
        {
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("TILO_SVC_NATIVE_FD", fd.to_string());
            env.set("TILO_WORKSPACE_ID", "workspace-one");
            let answering = std::thread::spawn(move || {
                let mut reader = BufReader::new(theirs);
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                reader
                    .into_inner()
                    .write_all(
                        b"{\"error\":\"dev.tilo.second is not installed on this computer\"}\n",
                    )
                    .unwrap();
            });
            let refused = runtime_portal::native::launch_another_instance("dev.tilo.second")
                .expect_err("the daemon refused the launch");
            answering.join().unwrap();
            assert!(
                refused.to_string().contains("is not installed on this computer"),
                "{refused}"
            );
        }
        // SAFETY: the call borrowed this descriptor and never owned it; nothing else uses it now.
        assert_eq!(unsafe { libc::close(fd) }, 0);
        let mut env = tilo_test_env::ScopedEnv::new();
        env.remove("TILO_SVC_NATIVE_FD");
        env.remove("TILO_WORKSPACE_ID");
        let refused = runtime_portal::native::launch_another_instance("dev.tilo.second")
            .expect_err("a program the launcher did not start has no instance to ask for");
        assert!(
            refused
                .to_string()
                .contains("not an app this computer launched"),
            "{refused}"
        );
        assert!(
            refused
                .to_string()
                .contains("--run-app dev.tilo.second"),
            "{refused}"
        );
    }
}
