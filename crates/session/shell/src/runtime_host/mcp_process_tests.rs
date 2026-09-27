use crate::runtime_host::Session;
use std::{
    ffi::OsStr,
    io::{Cursor, Read},
    path::PathBuf,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

/// The merged test whose phases each need a fresh process; libtest selects it
/// by this exact path, so a phase's own (nested) name would select nothing.
const PROCESS_TEST: &str =
    "runtime_host::mcp_process_tests::mcp_process_owns_and_reaps_its_children";
/// Names the one phase an isolated child process runs.
const PROCESS_PHASE: &str = "TILO_MCP_PROCESS_TEST_PHASE";

/// In the parent, runs `phase` in a fresh child process and returns `None`;
/// in that child, returns its isolated root for `phase` only and `None` for
/// every other phase, so the child runs exactly the selected phase.
fn isolated_root(phase: &str) -> Option<PathBuf> {
    if let Some(root) = std::env::var_os("TILO_MCP_PROCESS_TEST_ROOT") {
        return (std::env::var_os(PROCESS_PHASE).as_deref() == Some(OsStr::new(phase)))
            .then(|| PathBuf::from(root));
    }
    let root = tempfile::tempdir().unwrap();
    let profile = root.path().join("profile");
    let environment = root.path().join("environment");
    let log = root.path().join("child.stdout");
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", PROCESS_TEST, "--nocapture"])
        .env(PROCESS_PHASE, phase)
        .env("TILO_MCP_PROCESS_TEST_ROOT", profile)
        .env("XDG_CONFIG_HOME", environment.join("config"))
        .env("XDG_DATA_HOME", environment.join("data"))
        .env("XDG_CACHE_HOME", environment.join("cache"))
        .env("XDG_STATE_HOME", environment.join("state"))
        .env("TILO_IDENTITY_ROOT", environment.join("identity"))
        // A file, not a pipe: an owned app process that outlives a failing
        // phase cannot block the parent's read.
        .stdout(std::fs::File::create(&log).unwrap())
        .status()
        .unwrap();
    let stdout = std::fs::read_to_string(&log).unwrap();
    assert!(status.success(), "{phase}: {stdout}");
    // libtest exits 0 when `--exact` selects nothing; require the phase ran.
    assert!(
        stdout.contains("test result: ok. 1 passed"),
        "{phase}: the isolated child ran no test: {stdout}"
    );
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
            let deadline = tilo_test_env::hang_deadline();
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

use tilo_runtime::mcp_process::{AppManifest, AppSpec, validate_manifest};

fn serve<R, W>(
    profile: &std::path::Path,
    manifest: tilo_runtime::mcp_process::ValidatedManifest,
    reader: R,
    writer: W,
) -> std::io::Result<()>
where
    R: Read + Send + 'static,
    W: std::io::Write,
{
    tilo_runtime::mcp_process::serve::<Session, _, _>(profile, manifest, reader, writer)
}

/// A helper app's collaboration manager once it has joined the broker.
/// Construction returns while the connection is still being made; the
/// helper polls until it settles.
fn connected_manager() -> ui::collaboration::CollaborationManager {
    let mut manager = ui::collaboration::CollaborationManager::new();
    let started = Instant::now();
    while manager.state() == ui::collaboration::CollaborationState::Connecting
        && started.elapsed() < tilo_test_env::hang_bound()
    {
        let _ = manager.poll();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(manager.is_connected(), "{:?}", manager.state());
    manager
}

#[test]
#[allow(
    clippy::too_many_lines,
    clippy::significant_drop_tightening,
    reason = "each phase keeps one former test's body verbatim, and the duplicate-app phase holds its session and broker handles for the whole scenario on purpose"
)]
fn mcp_process_owns_and_reaps_its_children() {
    // Phase `zero_app_stdio_is_json_only_and_profile_contention_is_refused`.
    fn zero_app_stdio_is_json_only_and_profile_contention_is_refused() {
        let Some(root) =
            isolated_root("zero_app_stdio_is_json_only_and_profile_contention_is_refused")
        else {
            return;
        };
        let profile_environment = [
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "XDG_CACHE_HOME",
            "XDG_STATE_HOME",
            "TILO_IDENTITY_ROOT",
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

        let held = Session::start(&root).unwrap();
        let error = serve(
            &root,
            validate_manifest(AppManifest { apps: vec![] }).unwrap(),
            Cursor::new(Vec::new()),
            Vec::new(),
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("lock") || error.kind() == std::io::ErrorKind::WouldBlock
        );
        for (name, expected) in &profile_environment {
            assert_eq!(
                std::env::var_os(name),
                *expected,
                "contended serve mutated {name}"
            );
        }
        drop(held);
    }
    // Phase `eof_reaps_owned_children_and_early_exit_interrupts_idle_input`.
    fn eof_reaps_owned_children_and_early_exit_interrupts_idle_input() {
        let Some(root) =
            isolated_root("eof_reaps_owned_children_and_early_exit_interrupts_idle_input")
        else {
            return;
        };
        let pid_file = root.join("child.pid");
        let manifest = validate_manifest(AppManifest {
            apps: vec![AppSpec {
                id: "dev.tilo.synthetic".into(),
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
                id: "dev.tilo.failure".into(),
                executable: PathBuf::from("/bin/sh"),
                args: vec!["-c".into(), "exit 17".into()],
            }],
        })
        .unwrap();
        // The input stays idle far longer than any loaded start-up, so returning
        // well before its EOF proves the child's exit interrupted the wait.
        let started = Instant::now();
        let error = serve(
            &root,
            manifest,
            DelayedEof(Duration::from_secs(120)),
            Vec::new(),
        )
        .unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(60));
        assert!(error.to_string().contains("exited"));
    }
    // Phase `output_failure_reaps_the_surviving_child_and_releases_the_profile`.
    fn output_failure_reaps_the_surviving_child_and_releases_the_profile() {
        let Some(root) =
            isolated_root("output_failure_reaps_the_surviving_child_and_releases_the_profile")
        else {
            return;
        };
        let pid_file = root.join("surviving-child.pid");
        let manifest = validate_manifest(AppManifest {
            apps: vec![AppSpec {
                id: "dev.tilo.survivor".into(),
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
        let reopened = Session::start(&root).unwrap();
        drop(reopened);
    }
    // Phase `duplicate_app_ids_publish_as_distinct_normal_sdk_instances`.
    fn duplicate_app_ids_publish_as_distinct_normal_sdk_instances() {
        const PHASE: &str = "duplicate_app_ids_publish_as_distinct_normal_sdk_instances";
        if std::env::var_os("TILO_MCP_PUBLICATION_HELPER").is_some() {
            let mut manager = connected_manager();
            manager.publish_action_directory(ui::collaboration::local_app_id());
            loop {
                let _ = manager.poll();
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        let Some(root) = isolated_root(PHASE) else {
            return;
        };
        let session = Session::start(&root).unwrap();
        let executable = std::env::current_exe().unwrap();
        let mut children = Vec::new();
        for index in 0..2 {
            let log = std::fs::File::create(root.join(format!("helper-{index}.log"))).unwrap();
            let mut command = Command::new(&executable);
            command
                .args(["--exact", PROCESS_TEST, "--nocapture"])
                .env("TILO_MCP_PUBLICATION_HELPER", "1")
                .stdin(Stdio::null())
                .stdout(Stdio::from(log.try_clone().unwrap()))
                .stderr(Stdio::from(log));
            children.push(session.launch("dev.tilo.same", command).unwrap());
        }
        let deadline = tilo_test_env::hang_deadline();
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
        assert!(roots.iter().all(|root| root.app_id == "dev.tilo.same"));
        assert_ne!(roots[0].peer_id, roots[1].peer_id);
    }
    // Helper role: a publication helper re-enters this test and must go
    // straight to its publishing loop, which never returns.
    if std::env::var_os("TILO_MCP_PUBLICATION_HELPER").is_some() {
        duplicate_app_ids_publish_as_distinct_normal_sdk_instances();
    }
    // Phase `zero_app_stdio_is_json_only_and_profile_contention_is_refused` (defined above).
    zero_app_stdio_is_json_only_and_profile_contention_is_refused();
    // Phase `eof_reaps_owned_children_and_early_exit_interrupts_idle_input` (defined above).
    eof_reaps_owned_children_and_early_exit_interrupts_idle_input();
    // Phase `output_failure_reaps_the_surviving_child_and_releases_the_profile` (defined above).
    output_failure_reaps_the_surviving_child_and_releases_the_profile();
    // Phase `duplicate_app_ids_publish_as_distinct_normal_sdk_instances` (defined above).
    duplicate_app_ids_publish_as_distinct_normal_sdk_instances();
}
