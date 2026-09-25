use super::{
    AppManifest, AppSpec, Frame, MAX_REQUEST_BYTES, parse_args, read_frame, serve,
    validate_manifest, write_reply,
};
use serde_json::json;
use std::{
    ffi::OsString,
    io::{BufReader, Cursor, Read},
    path::PathBuf,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

fn isolated_root(test_name: &str) -> Option<PathBuf> {
    if let Some(root) = std::env::var_os("QOS_MCP_PROCESS_TEST_ROOT") {
        return Some(PathBuf::from(root));
    }
    let root = tempfile::tempdir().unwrap();
    let profile = root.path().join("profile");
    let environment = root.path().join("environment");
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test_name, "--nocapture"])
        .env("QOS_MCP_PROCESS_TEST_ROOT", profile)
        .env("XDG_CONFIG_HOME", environment.join("config"))
        .env("XDG_DATA_HOME", environment.join("data"))
        .env("XDG_CACHE_HOME", environment.join("cache"))
        .env("XDG_STATE_HOME", environment.join("state"))
        .env("TILO_IDENTITY_ROOT", environment.join("identity"))
        .env("TILO_BLOB_STORE", environment.join("blobs"))
        .status()
        .unwrap();
    assert!(status.success());
    None
}

struct DelayedEof(Duration);
impl Read for DelayedEof {
    fn read(&mut self, _bytes: &mut [u8]) -> std::io::Result<usize> {
        std::thread::sleep(self.0);
        Ok(0)
    }
}

struct InputAfterFile {
    ready: PathBuf,
    input: Cursor<Vec<u8>>,
    waited: bool,
}

impl Read for InputAfterFile {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        if !self.waited {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !self.ready.exists() {
                if Instant::now() >= deadline {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "owned child did not publish its pid",
                    ));
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            self.waited = true;
        }
        self.input.read(bytes)
    }
}

struct BrokenWriter;

impl std::io::Write for BrokenWriter {
    fn write(&mut self, _bytes: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "injected MCP output failure",
        ))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn bounded_framing_recovers_after_an_oversized_line() {
    let mut bytes = vec![b'x'; MAX_REQUEST_BYTES + 1];
    bytes.extend_from_slice(b"\n{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n");
    let mut input = BufReader::new(bytes.as_slice());

    assert_eq!(read_frame(&mut input).unwrap(), Frame::TooLong);
    let Frame::Line(next) = read_frame(&mut input).unwrap() else {
        panic!("expected the complete request after the oversized line");
    };
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&next).unwrap()["method"],
        "ping"
    );
    assert_eq!(read_frame(&mut input).unwrap(), Frame::Eof);
}

#[test]
fn line_at_the_one_mib_limit_is_accepted() {
    let mut bytes = vec![b' '; MAX_REQUEST_BYTES];
    bytes.push(b'\n');
    let mut input = BufReader::new(bytes.as_slice());
    let Frame::Line(line) = read_frame(&mut input).unwrap() else {
        panic!("line at limit must be accepted");
    };
    assert_eq!(line.len(), MAX_REQUEST_BYTES);
}

#[test]
fn manifest_validation_precedes_launch_and_accepts_duplicate_ids() {
    let executable = std::env::current_exe().unwrap();
    let duplicates = AppManifest {
        apps: vec![
            AppSpec {
                id: "dev.qos.same".into(),
                executable: executable.clone(),
                args: vec![],
            },
            AppSpec {
                id: "dev.qos.same".into(),
                executable,
                args: vec!["--help".into()],
            },
        ],
    };
    assert_eq!(validate_manifest(duplicates).unwrap().apps.len(), 2);

    let invalid = AppManifest {
        apps: vec![
            AppSpec {
                id: "dev.qos.valid".into(),
                executable: std::env::current_exe().unwrap(),
                args: vec![],
            },
            AppSpec {
                id: "dev.qos.invalid".into(),
                executable: PathBuf::from("relative"),
                args: vec![],
            },
        ],
    };
    let error = validate_manifest(invalid).unwrap_err();
    assert!(error.to_string().contains("absolute"));
}

#[test]
fn cli_requires_profile_and_manifest_paths() {
    let parsed = parse_args([
        OsString::from("--profile"),
        OsString::from("/tmp/profile"),
        OsString::from("--apps"),
        OsString::from("/tmp/apps.json"),
    ])
    .unwrap();
    assert_eq!(parsed.profile, PathBuf::from("/tmp/profile"));
    assert_eq!(parsed.apps, PathBuf::from("/tmp/apps.json"));
    assert!(parse_args([OsString::from("--profile"), OsString::from("/tmp/profile")]).is_err());
    assert!(parse_args([OsString::from("--unknown")]).is_err());
}

#[test]
fn protocol_writer_emits_one_json_line_and_flushes() {
    #[derive(Default)]
    struct Output {
        bytes: Vec<u8>,
        flushes: usize,
    }
    impl std::io::Write for Output {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.flushes += 1;
            Ok(())
        }
    }
    let mut output = Output::default();
    write_reply(&mut output, &json!({"jsonrpc":"2.0","id":1,"result":{}})).unwrap();
    assert_eq!(output.flushes, 1);
    assert_eq!(output.bytes.last(), Some(&b'\n'));
    assert_eq!(
        output.bytes.iter().filter(|byte| **byte == b'\n').count(),
        1
    );
    serde_json::from_slice::<serde_json::Value>(&output.bytes).unwrap();
}

#[test]
fn zero_app_stdio_is_json_only_and_profile_contention_is_refused() {
    let Some(root) = isolated_root(
        "runtime_host::mcp_process::tests::zero_app_stdio_is_json_only_and_profile_contention_is_refused",
    ) else {
        return;
    };
    let profile_environment = [
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_CACHE_HOME",
        "XDG_STATE_HOME",
        "TILO_IDENTITY_ROOT",
        "TILO_BLOB_STORE",
    ]
    .map(|name| (name, std::env::var_os(name)));
    let manifest = validate_manifest(AppManifest { apps: vec![] }).unwrap();
    let requests = concat!(
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-06-18\"}}\n",
        "not json\n",
        "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"ping\"}\n"
    );
    let mut output = Vec::new();
    serve(
        &root,
        manifest,
        Cursor::new(requests.as_bytes().to_vec()),
        &mut output,
    )
    .unwrap();
    let replies: Vec<serde_json::Value> = String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(replies.len(), 3);
    assert_eq!(replies[0]["id"], 1);
    assert_eq!(replies[1]["error"]["code"], -32700);
    assert_eq!(replies[2]["id"], 2);
    for (name, expected) in &profile_environment {
        assert_eq!(std::env::var_os(name), *expected, "serve mutated {name}");
    }

    let held = crate::runtime_host::Session::start(&root).unwrap();
    let error = serve(
        &root,
        validate_manifest(AppManifest { apps: vec![] }).unwrap(),
        Cursor::new(Vec::new()),
        Vec::new(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("lock") || error.kind() == std::io::ErrorKind::WouldBlock);
    for (name, expected) in &profile_environment {
        assert_eq!(
            std::env::var_os(name),
            *expected,
            "contended serve mutated {name}"
        );
    }
    drop(held);
}

#[test]
fn eof_reaps_owned_children_and_early_exit_interrupts_idle_input() {
    let Some(root) = isolated_root(
        "runtime_host::mcp_process::tests::eof_reaps_owned_children_and_early_exit_interrupts_idle_input",
    ) else {
        return;
    };
    let pid_file = root.join("child.pid");
    let manifest = validate_manifest(AppManifest {
        apps: vec![AppSpec {
            id: "dev.qos.synthetic".into(),
            executable: PathBuf::from("/bin/sh"),
            args: vec![
                "-c".into(),
                format!("echo $$ > '{}'; exec sleep 30", pid_file.display()),
            ],
        }],
    })
    .unwrap();
    serve(
        &root,
        manifest,
        DelayedEof(Duration::from_millis(300)),
        Vec::new(),
    )
    .unwrap();
    let pid: i32 = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // SAFETY: signal 0 only checks whether the exact recorded child still exists.
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );

    let manifest = validate_manifest(AppManifest {
        apps: vec![AppSpec {
            id: "dev.qos.failure".into(),
            executable: PathBuf::from("/bin/sh"),
            args: vec!["-c".into(), "exit 17".into()],
        }],
    })
    .unwrap();
    let started = Instant::now();
    let error = serve(
        &root,
        manifest,
        DelayedEof(Duration::from_secs(5)),
        Vec::new(),
    )
    .unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(error.to_string().contains("exited"));
}

#[test]
fn output_failure_reaps_the_surviving_child_and_releases_the_profile() {
    let Some(root) = isolated_root(
        "runtime_host::mcp_process::tests::output_failure_reaps_the_surviving_child_and_releases_the_profile",
    ) else {
        return;
    };
    let pid_file = root.join("surviving-child.pid");
    let manifest = validate_manifest(AppManifest {
        apps: vec![AppSpec {
            id: "dev.qos.survivor".into(),
            executable: PathBuf::from("/bin/sh"),
            args: vec![
                "-c".into(),
                format!("echo $$ > '{}'; exec sleep 30", pid_file.display()),
            ],
        }],
    })
    .unwrap();
    let input = InputAfterFile {
        ready: pid_file.clone(),
        input: Cursor::new(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n".to_vec()),
        waited: false,
    };

    let error = serve(&root, manifest, input, BrokenWriter).unwrap_err();
    assert!(error.to_string().contains("injected MCP output failure"));
    let pid: i32 = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // SAFETY: signal 0 only checks whether the exact recorded child still exists.
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
    let reopened = crate::runtime_host::Session::start(&root).unwrap();
    drop(reopened);
}

#[test]
fn duplicate_app_ids_publish_as_distinct_normal_sdk_instances() {
    const TEST: &str = "runtime_host::mcp_process::tests::duplicate_app_ids_publish_as_distinct_normal_sdk_instances";
    if std::env::var_os("QOS_MCP_PUBLICATION_HELPER").is_some() {
        let mut manager = ui::collaboration::CollaborationManager::new();
        assert!(manager.is_connected());
        manager.publish_action_directory(ui::collaboration::local_app_id());
        loop {
            let _ = manager.poll();
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    let Some(root) = isolated_root(TEST) else {
        return;
    };
    let session = crate::runtime_host::Session::start(&root).unwrap();
    let executable = std::env::current_exe().unwrap();
    let mut children = Vec::new();
    for index in 0..2 {
        let log = std::fs::File::create(root.join(format!("helper-{index}.log"))).unwrap();
        let mut command = Command::new(&executable);
        command
            .args(["--exact", TEST, "--nocapture"])
            .env("QOS_MCP_PUBLICATION_HELPER", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone().unwrap()))
            .stderr(Stdio::from(log));
        children.push(session.launch("dev.qos.same", command).unwrap());
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    let roots = loop {
        let roots = session
            .broker
            .app_action_broker()
            .unwrap()
            .state()
            .action_directory_root_page(None, 8)
            .roots;
        if roots.len() == 2 || Instant::now() >= deadline {
            break roots;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    for child in &mut children {
        let _ = child.kill();
        let _ = child.wait();
    }
    assert_eq!(roots.len(), 2);
    assert!(roots.iter().all(|root| root.app_id == "dev.qos.same"));
    assert_ne!(roots[0].peer_id, roots[1].peer_id);
}
