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
    fn rpc_clients_retire_before_the_native_writer_lease() {
        use ed25519_dalek::Signer as _;
        let Some(root) = isolated_root("rpc_clients_retire_before_the_native_writer_lease") else {
            return;
        };
        let session = Session::start(&root).unwrap();
        let (key, certificate) =
            tilo_identity_host::broker::trusted_current_device(&session.room).unwrap();
        let presentation = runtime_collab::CertifiedJoinIdentity {
            workspace_id: session.room.clone(),
            device_certificate: serde_json::to_vec(&certificate).unwrap(),
            proof: key
                .sign(&runtime_identity::collaboration_join_signing_bytes(
                    &session.room,
                ))
                .to_bytes()
                .to_vec(),
        }
        .encode()
        .unwrap();
        let endpoint = session.broker.capnp_socket().to_path_buf();
        let workspace_id = session.room.clone();
        let (ready, admitted) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel();
        let client = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(tokio::task::LocalSet::new().run_until(async move {
                let fd = runtime_portal::agent_transport::connect_endpoint(&endpoint).unwrap();
                let connection = runtime_portal::CapnpClient::from_connected_fd(
                    fd,
                    capnp_rpc::rpc_twoparty_capnp::Side::Client,
                )
                .unwrap();
                let broker = runtime_collab::CollabClient::from_raw(connection.bootstrap());
                let channel = tilo_test_env::within(
                    "the signed client to join its channel",
                    broker.join_channel(&workspace_id, &presentation),
                )
                .await
                .unwrap();
                ready.send(()).unwrap();
                // Deliberately keep the actual imported channel and RPC connection
                // while the client cannot consume EOF or send capability releases.
                tilo_test_env::recv("the test to release the client", &released);
                drop(channel);
                drop(broker);
            }));
        });
        let ready = admitted.recv_timeout(tilo_test_env::hang_bound());
        if let Err(error) = ready {
            let _ = release.send(());
            client.join().unwrap();
            panic!("the signed client never received its actual channel: {error}");
        }
        drop(session);
        let replacement = Session::start(&root);
        // Release before asserting so a failed lease assertion cannot orphan the peer.
        release.send(()).unwrap();
        client.join().unwrap();
        drop(replacement.expect("RPC retirement must release the native writer lease without client cooperation or retries"));
    }

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
                    // Renamed into place, so the file exists only once it
                    // holds the whole pid: the input waits for it to exist.
                    format!(
                        "echo $$ > '{0}.partial' && mv '{0}.partial' '{0}'; exec sleep 30",
                        pid_file.display()
                    ),
                ],
            }],
        })
        .unwrap();
        // EOF once the child is up, however long its start takes.
        serve(
            &root,
            manifest,
            InputAfterFile {
                ready: pid_file.clone(),
                input: Cursor::new(Vec::new()),
                waited: false,
            },
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
        // The input stays idle for the whole hang bound, so returning before
        // its EOF proves the child's exit interrupted the wait.
        let started = Instant::now();
        let error = serve(
            &root,
            manifest,
            DelayedEof(tilo_test_env::hang_bound()),
            Vec::new(),
        )
        .unwrap_err();
        assert!(
            started.elapsed() < tilo_test_env::hang_bound(),
            "serve waited for the idle input instead of the child's exit"
        );
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
                    // Renamed into place, so the file exists only once it
                    // holds the whole pid: the input waits for it to exist.
                    format!(
                        "echo $$ > '{0}.partial' && mv '{0}.partial' '{0}'; exec sleep 30",
                        pid_file.display()
                    ),
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
        // What the two apps said, so a missing directory names its cause.
        let said = (0..2)
            .map(|index| {
                std::fs::read_to_string(root.join(format!("helper-{index}.log")))
                    .unwrap_or_default()
            })
            .collect::<Vec<_>>();
        assert_eq!(roots.len(), 2, "each app publishes its directory: {said:?}");
        assert!(roots.iter().all(|root| root.app_id == "dev.tilo.same"));
        assert_ne!(roots[0].peer_id, roots[1].peer_id);
    }
    // A real broker lifecycle listener publishes a final snapshot during close.
    fn shutdown_saves_finish_before_the_profile_is_released() {
        use std::sync::{
            Arc, Mutex,
            atomic::{AtomicBool, Ordering},
            mpsc,
        };
        let Some(root) = isolated_root("shutdown_saves_finish_before_the_profile_is_released")
        else {
            return;
        };
        let session = Session::start(&root).unwrap();
        let tree = session.workspace_tree().downgrade();
        let path = root.join("workspace.json");
        let (started, saving) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let released = Mutex::new(Some(released));
        let listener: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            let tree = tree
                .upgrade()
                .expect("broker close still owns its workspace");
            let released = released.lock().unwrap().take().expect("one close callback");
            let started = started.clone();
            crate::shell::persist::queue_save(&tree.read(), &path, move || {
                started.send(()).unwrap();
                tilo_test_env::recv("the controller to release the shutdown save", &released);
            });
        });
        session.broker.local_session_lifecycle().watch(&listener);
        let returned = Arc::new(AtomicBool::new(false));
        let observed = returned.clone();
        let controller = std::thread::spawn(move || {
            // Closing a session under load can take longer than any fixed
            // budget before its final save starts; only a hang is a failure.
            tilo_test_env::recv("the shutdown save to start", &saving);
            // Keep the actual final writer occupied while checking retirement.
            std::thread::sleep(Duration::from_millis(100));
            let waited = !observed.load(Ordering::SeqCst);
            let locked = tilo_runtime::profile::lock(&root)
                .is_err_and(|error| error.kind() == std::io::ErrorKind::WouldBlock);
            release.send(()).unwrap();
            (root, waited, locked)
        });
        drop(session);
        returned.store(true, Ordering::SeqCst);
        let (root, waited, locked) = controller.join().unwrap();
        assert!(
            waited,
            "session returned while its shutdown snapshot was still running"
        );
        assert!(
            locked,
            "profile lock was released before the shutdown save completed"
        );
        // No retry: the replacement must immediately acquire both profile and file leases.
        drop(Session::start(&root).expect("completed retirement releases the native writer lease"));
    }
    fn identity_clients_retire_before_the_native_writer_lease() {
        use runtime_identity::wire::{FRAMING, HANDSHAKE_FRAMING, Handshake, Request, Response};
        use std::io::Write as _;
        use std::os::unix::net::UnixStream;
        let Some(root) = isolated_root("identity_clients_retire_before_the_native_writer_lease")
        else {
            return;
        };
        let session = Session::start(&root).unwrap();
        let mut client = UnixStream::connect(session.identity_socket()).unwrap();
        client
            .set_read_timeout(Some(tilo_test_env::hang_bound()))
            .unwrap();
        HANDSHAKE_FRAMING
            .write(
                &mut client,
                &serde_json::to_vec(&Handshake {
                    workspace_id: session.room.clone(),
                    app_id: "so.tilo.file-explorer".into(),
                    surface: None,
                })
                .unwrap(),
            )
            .unwrap();
        FRAMING
            .write_value(&mut client, &Request::Metadata)
            .unwrap();
        let reply: Response = FRAMING.read_value(&mut client).unwrap().unwrap();
        assert!(
            !matches!(reply, Response::Denied(_)),
            "the actual identity worker answered: {reply:?}"
        );
        let mut partial = UnixStream::connect(session.identity_socket()).unwrap();
        partial.write_all(&[0]).unwrap();
        drop(session);
        // The client's socket stays open: host retirement must own cancellation.
        let replacement = Session::start(&root)
            .expect("retired identity clients release the native writer lease without retries");
        assert!(
            FRAMING
                .read_value::<_, Response>(&mut client)
                .unwrap()
                .is_none(),
            "the previous session cannot keep serving identity requests"
        );
        drop(replacement);
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
    shutdown_saves_finish_before_the_profile_is_released();
    identity_clients_retire_before_the_native_writer_lease();
    rpc_clients_retire_before_the_native_writer_lease();
}
