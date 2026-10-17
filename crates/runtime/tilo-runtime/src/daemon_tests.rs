use super::{
    DaemonStart, Mode, Observation, STAGES_FILE, StartingProcess, StartupLimits, current_stage,
    parse_args, wait_until_ready,
};
use std::{
    ffi::OsString,
    io,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[test]
fn default_admission_activates_a_live_owner_and_retries_after_its_exit() {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    struct Endpoint {
        active: Arc<AtomicBool>,
        stop: Arc<AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }
    impl Drop for Endpoint {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            if let Some(thread) = self.thread.take() {
                thread.join().unwrap();
            }
        }
    }
    let directory = tempfile::tempdir().unwrap();
    let listener = UnixListener::bind(directory.path().join("native.sock")).unwrap();
    listener.set_nonblocking(true).unwrap();
    let active = Arc::new(AtomicBool::new(false));
    let stop = Arc::new(AtomicBool::new(false));
    let (serving, stopping) = (active.clone(), stop.clone());
    let endpoint = Endpoint {
        active,
        stop,
        thread: Some(std::thread::spawn(move || {
            while !stopping.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_read_timeout(Some(Duration::from_secs(1)))
                            .unwrap();
                        let mut request = String::new();
                        BufReader::new(stream.try_clone().unwrap())
                            .read_line(&mut request)
                            .unwrap();
                        let request: serde_json::Value = serde_json::from_str(&request).unwrap();
                        assert_eq!(
                            request,
                            serde_json::json!({"native":"activate", "app_id":"so.tilo.fixture"})
                        );
                        writeln!(
                            stream,
                            "{}",
                            serde_json::json!({"activated": serving.load(Ordering::SeqCst)})
                        )
                        .unwrap();
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    Err(error) => panic!("activation endpoint failed: {error}"),
                }
            }
        })),
    };
    let grace = Duration::from_millis(60);
    let first = super::default_launch_guard(directory.path(), "so.tilo.fixture", grace)
        .unwrap()
        .unwrap();
    endpoint.active.store(true, Ordering::SeqCst);
    assert!(
        super::default_launch_guard(directory.path(), "so.tilo.fixture", grace)
            .unwrap()
            .is_none(),
        "a second healthy launch attaches without replacing its owner"
    );
    endpoint.active.store(false, Ordering::SeqCst);
    let refusal =
        super::default_launch_guard(directory.path(), "so.tilo.fixture", grace).unwrap_err();
    assert_eq!(refusal.kind(), io::ErrorKind::WouldBlock);
    assert!(refusal.to_string().contains("owner was retained"));
    drop(first);
    // The stale PID left in the lock file never counts as a live owner.
    let retry = super::default_launch_guard(directory.path(), "so.tilo.fixture", grace)
        .unwrap()
        .expect("owner exit releases the kernel lease for a fresh launch");
    drop(retry);
    drop(endpoint);
}

const LIMITS: StartupLimits = StartupLimits {
    stall: Duration::from_millis(300),
    cap: Duration::from_secs(10),
    poll: Duration::from_millis(5),
};

/// A process that progresses until `progress_for`, then goes quiet. Only
/// the signal at `moving` changes; the others stay constant or unreadable.
struct Fake {
    started: Instant,
    progress_for: Duration,
    moving: usize,
    ready_after: Option<Duration>,
    exit_after: Option<Duration>,
    stopped: bool,
}

impl Fake {
    fn new(progress_for: Duration) -> Self {
        Self {
            started: Instant::now(),
            progress_for,
            moving: 0,
            ready_after: None,
            exit_after: None,
            stopped: false,
        }
    }
}

impl StartingProcess for Fake {
    type Ready = &'static str;
    fn ready(&mut self) -> Option<&'static str> {
        self.ready_after
            .is_some_and(|after| self.started.elapsed() >= after)
            .then_some("ready")
    }
    fn exited(&mut self) -> io::Result<Option<String>> {
        Ok(self
            .exit_after
            .is_some_and(|after| self.started.elapsed() >= after)
            .then(|| "exit status: 3".to_owned()))
    }
    fn observe(&mut self) -> Observation {
        let elapsed = self.started.elapsed();
        let (stage, value) = if elapsed < self.progress_for {
            (
                "warming up",
                u64::try_from(elapsed.as_millis() / 10).unwrap(),
            )
        } else {
            ("waiting on the lock", u64::MAX)
        };
        let mut signals = vec![("steady", Some(7)), ("absent", None), ("busy", Some(0))];
        signals[self.moving].1 = Some(value);
        Observation {
            stage: stage.to_owned(),
            signals,
        }
    }
    fn stop(&mut self) {
        self.stopped = true;
    }
    fn diagnostics(&self) -> String {
        "last output: fake tail".to_owned()
    }
}

#[test]
fn slow_start_that_keeps_progressing_is_waited_for() {
    let mut fake = Fake::new(Duration::from_secs(60));
    // Several stall windows long: only continued progress keeps it alive.
    fake.ready_after = Some(Duration::from_millis(1500));
    let started = Instant::now();
    assert_eq!(wait_until_ready(&mut fake, LIMITS).unwrap(), "ready");
    assert!(started.elapsed() >= Duration::from_millis(1500));
    assert!(!fake.stopped);
}

#[test]
fn stalled_start_fails_at_the_stall_timeout_naming_its_stage() {
    let mut fake = Fake::new(Duration::from_millis(200));
    let started = Instant::now();
    let error = wait_until_ready(&mut fake, LIMITS).unwrap_err();
    let elapsed = started.elapsed();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    let message = error.to_string();
    assert!(
        message.contains("stalled while waiting on the lock"),
        "{message}"
    );
    assert!(message.contains("fake tail"), "{message}");
    // Each signal says when it last moved, so a log shows which went quiet.
    assert!(message.contains("steady last changed"), "{message}");
    assert!(message.contains("absent unreadable"), "{message}");
    assert!(message.contains("busy unchanged since launch"), "{message}");
    assert!(elapsed >= Duration::from_millis(500), "{elapsed:?}");
    assert!(elapsed < LIMITS.cap, "{elapsed:?}");
    assert!(fake.stopped);
}

#[test]
fn progress_in_any_single_signal_keeps_the_start_alive() {
    let mut fake = Fake::new(Duration::from_secs(60));
    fake.moving = 2;
    fake.ready_after = Some(Duration::from_millis(1500));
    assert_eq!(wait_until_ready(&mut fake, LIMITS).unwrap(), "ready");
    assert!(!fake.stopped);
}

#[test]
fn exited_start_fails_immediately_with_its_output() {
    let mut fake = Fake::new(Duration::from_secs(60));
    fake.exit_after = Some(Duration::from_millis(20));
    let started = Instant::now();
    let message = wait_until_ready(&mut fake, LIMITS).unwrap_err().to_string();
    assert!(started.elapsed() < LIMITS.stall);
    assert!(
        message.contains("exited while warming up (exit status: 3)"),
        "{message}"
    );
    assert!(message.contains("fake tail"), "{message}");
}

#[test]
fn start_that_never_finishes_fails_at_the_cap() {
    let mut fake = Fake::new(Duration::from_secs(60));
    let limits = StartupLimits {
        cap: Duration::from_millis(800),
        ..LIMITS
    };
    let error = wait_until_ready(&mut fake, limits).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(
        error
            .to_string()
            .contains("within 0.8 s (still warming up; steady last changed"),
        "{error}"
    );
    assert!(fake.stopped);
}

fn daemon_start(profile: &std::path::Path, script: &str) -> DaemonStart {
    let child = Command::new("/bin/sh")
        .args(["-c", script])
        .current_dir(profile)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(profile.join("daemon.log")).unwrap())
        .spawn()
        .unwrap();
    DaemonStart::new(profile, child)
}

#[test]
fn real_child_exit_reports_its_stderr() {
    let profile = tempfile::tempdir().unwrap();
    let mut start = daemon_start(profile.path(), "echo 'identity: boom' >&2; exit 3");
    let message = wait_until_ready(&mut start, LIMITS)
        .unwrap_err()
        .to_string();
    assert!(message.contains("exit status: 3"), "{message}");
    assert!(message.contains("identity: boom"), "{message}");
}

#[test]
fn named_startup_steps_are_progress_and_a_stall_names_the_last_one() {
    let profile = tempfile::tempdir().unwrap();
    // Names a step every 0.2 s for several stall windows, without output,
    // then blocks without using CPU, reading, writing or naming a step.
    let mut start = daemon_start(
        profile.path(),
        &format!(
            "for step in 1 2 3 4 5 6 7 8; do sleep 0.2; echo \"step $step\" >> {STAGES_FILE}; \
             done; exec sleep 60"
        ),
    );
    let pid = start.child.id();
    let limits = StartupLimits {
        stall: Duration::from_secs(1),
        ..LIMITS
    };
    let started = Instant::now();
    let error = wait_until_ready(&mut start, limits).unwrap_err();
    let message = error.to_string();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(message.contains("stalled while step 8"), "{message}");
    assert!(
        started.elapsed() >= Duration::from_millis(2600),
        "{message}"
    );
    assert!(message.contains("startup step last changed"), "{message}");
    assert!(message.contains("wrote nothing"), "{message}");
    #[cfg(target_os = "linux")]
    {
        // What the stalled process was blocked on.
        assert!(message.contains("threads: sleep S"), "{message}");
        // The stalled child was killed and reaped.
        assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
    }
    // Only Linux exposes a process tree's CPU time and thread states; elsewhere
    // the step names alone are the progress signal.
    #[cfg(not(target_os = "linux"))]
    {
        assert!(message.contains("CPU time unreadable"), "{message}");
        // The stalled child was killed and reaped.
        // SAFETY: signal 0 only checks whether the exact child still exists.
        assert_eq!(
            unsafe { libc::kill(i32::try_from(pid).unwrap(), 0) },
            -1,
            "{message}"
        );
    }
}

#[test]
fn the_current_stage_is_the_last_named_step() {
    assert_eq!(current_stage(b""), "loading the runtime");
    assert_eq!(current_stage(b"one\ntwo\n\n"), "two");
    assert_eq!(current_stage("x".repeat(500).as_bytes()).len(), 200);
}

#[cfg(target_os = "linux")]
#[test]
fn cpu_time_of_a_live_process_is_readable() {
    assert!(super::cpu_ticks(std::process::id()).is_some());
    assert!(super::io_bytes(std::process::id()).is_some());
}

/// A launcher that waits on a wrapper still sees the worker it started.
#[cfg(target_os = "linux")]
#[test]
fn a_process_tree_counts_its_descendants_cpu_time_and_runnable_threads() {
    use std::os::unix::process::CommandExt;
    let mut wrapper = Command::new("/bin/sh")
        .args(["-c", "sh -c 'while :; do :; done' & wait"])
        .process_group(0)
        .stdin(Stdio::null())
        .spawn()
        .unwrap();
    let pid = wrapper.id();
    let deadline = tilo_test_env::hang_deadline();
    let (mut sample, mut own) = (super::sample_tree(pid, false).unwrap(), 0);
    while sample.cpu_ticks <= own && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
        own = super::cpu_ticks(pid).unwrap();
        sample = super::sample_tree(pid, false).unwrap();
    }
    let described = super::sample_tree(pid, true).unwrap();
    // SAFETY: the wrapper leads its own process group, which holds only
    // itself and the worker it started.
    unsafe {
        libc::killpg(i32::try_from(pid).unwrap(), libc::SIGKILL);
    }
    let _ = wrapper.wait();
    assert!(
        sample.cpu_ticks > own,
        "the tree never counted its busy descendant's CPU time within the hang bound: {sample:?}, wrapper {own}"
    );
    assert!(described.threads.len() >= 2, "{described:?}");
    assert!(
        described.held_by_machine || described.cpu_ticks > sample.cpu_ticks,
        "{described:?}"
    );
}

/// Without per-task child lists, children are found by their parent pid.
#[cfg(target_os = "linux")]
#[test]
fn parent_pids_find_a_spawned_child() {
    let mut child = Command::new("/bin/sleep").arg("30").spawn().unwrap();
    let found = super::parent_pids().contains(&(child.id(), std::process::id()));
    let _ = child.kill();
    let _ = child.wait();
    assert!(found);
}

#[test]
fn owner_action_is_bounded_and_exclusive_with_other_modes() {
    let args = |list: &[&str]| parse_args(list.iter().map(OsString::from));
    let parsed = args(&["--profile", "/run/example-profile", "--owner-action", "{}"]).unwrap();
    assert_eq!(
        parsed.mode,
        Mode::OwnerAction {
            request: "{}".into()
        }
    );
    for invalid in [
        vec!["--owner-action"],
        vec!["--owner-action", ""],
        vec!["--owner-action", "{}", "--daemon"],
        vec!["--owner-action", "{}", "--connect"],
        vec!["--owner-action", "{}", "--dev"],
        vec!["--owner-action", "{}", "--standalone"],
    ] {
        assert!(args(&invalid).is_err(), "{invalid:?}");
    }
    assert!(args(&["--owner-action", &"x".repeat(32 * 1024 + 1)]).is_err());
}

#[test]
fn dev_mode_is_an_explicit_daemon_flag() {
    let args = |list: &[&str]| parse_args(list.iter().map(OsString::from));
    let dev = args(&["--profile", "/tmp/profile", "--daemon", "--dev"]).unwrap();
    assert_eq!((dev.mode, dev.dev), (Mode::Daemon, true));
    assert!(
        !args(&["--profile", "/tmp/profile", "--daemon"])
            .unwrap()
            .dev
    );
    assert!(args(&["--profile", "/tmp/profile", "--connect", "--dev"]).is_err());
    assert!(args(&["--profile", "/tmp/profile", "--daemon", "--dev", "--dev"]).is_err());
}

/// A client that names a profile chose a standalone runtime itself; one that
/// names none is routed by its login (`session_door::route`).
#[test]
fn standalone_is_the_clients_own_explicit_choice_for_connect() {
    let args = |list: &[&str]| parse_args(list.iter().map(OsString::from));
    assert!(args(&["--connect", "--standalone"]).unwrap().standalone);
    assert!(
        args(&["--profile", "/tmp/profile", "--connect"])
            .unwrap()
            .standalone,
        "a named profile is a standalone runtime's"
    );
    assert!(args(&["--profile", "/tmp/profile", "--daemon", "--standalone"]).is_err());
    assert!(args(&["--connect", "--standalone", "--standalone"]).is_err());
}

#[test]
fn admission_daemon_requires_an_explicit_non_root_owner_and_no_dev_bypass() {
    let args = |list: &[&str]| parse_args(list.iter().map(OsString::from));
    let parsed = args(&["--profile", "/run/admission", "--admission-daemon", "1000"]).unwrap();
    assert_eq!(parsed.mode, Mode::AdmissionDaemon { owner_uid: 1000 });
    for invalid in [
        vec!["--admission-daemon", "1000"],
        vec!["--profile", "/run/admission", "--admission-daemon"],
        vec!["--profile", "/run/admission", "--admission-daemon", "0"],
        vec![
            "--profile",
            "/run/admission",
            "--admission-daemon",
            "unknown",
        ],
        vec![
            "--profile",
            "/run/admission",
            "--admission-daemon",
            "1000",
            "--dev",
        ],
        vec![
            "--profile",
            "/run/admission",
            "--admission-daemon",
            "1000",
            "--daemon",
        ],
    ] {
        assert!(args(&invalid).is_err(), "{invalid:?}");
    }
}

/// One page resource with an `open` view action, offered by publisher `7`,
/// and every invocation the host was asked to run.
#[derive(Clone, Default)]
struct PageHost {
    offered: std::sync::Arc<std::sync::atomic::AtomicBool>,
    invoked: std::sync::Arc<std::sync::Mutex<Vec<tilo_actions::remote::InvokeRequest>>>,
}

impl tilo_actions::remote::ActionHost for PageHost {
    fn query(
        &self,
        _caller: &tilo_actions::remote::Caller,
        query: tilo_actions::remote::CatalogueQuery,
    ) -> tilo_actions::remote::Pending<
        Result<tilo_actions::remote::CataloguePage, tilo_actions::remote::ActionHostError>,
    > {
        use tilo_actions::remote::{CatalogueProvider, CatalogueResource, CatalogueRow};
        let rows: Vec<CatalogueRow> = if self.offered.load(std::sync::atomic::Ordering::SeqCst) {
            vec![CatalogueRow {
                provider: "7".into(),
                app_id: "so.tilo.notes".into(),
                parent_resource: String::new(),
                resource: CatalogueResource {
                    id: "page:1".into(),
                    kind: "notes.page".into(),
                    label: "Travel plans".into(),
                    presentation: None,
                },
                action: Some(tilo_actions::ActionDescriptor {
                    semantic_identity: None,
                    address: tilo_actions::ActionAddress::new("page:1", "open"),
                    resource_kind: "notes.page".into(),
                    resource_label: "Travel plans".into(),
                    action_label: "Open note".into(),
                    description: None,
                    locus: tilo_actions::ExecutionLocus::ClientLocal,
                    kind: tilo_actions::OperationKind::View,
                    contract: tilo_actions::ActionContract::view(),
                    input: tilo_actions::InputSpec::None,
                    params: Vec::new(),
                    view: None,
                }),
                has_children: false,
                visibility: None,
            }]
        } else {
            Vec::new()
        };
        let mut page = tilo_actions::remote::CataloguePage::from_rows("1".into(), rows, &query);
        if query.provider.is_none() {
            page.providers = vec![CatalogueProvider {
                provider: "7".into(),
                app_id: "so.tilo.notes".into(),
                room: "room".into(),
                record_id: String::new(),
                browsable: true,
            }];
        }
        tilo_actions::remote::Pending::ready(Ok(page))
    }

    fn invoke(
        &self,
        _caller: &tilo_actions::remote::Caller,
        request: tilo_actions::remote::InvokeRequest,
    ) -> tilo_actions::remote::Pending<
        Result<tilo_actions::remote::InvocationReceipt, tilo_actions::remote::ActionHostError>,
    > {
        let invocation = request.invocation;
        self.invoked.lock().unwrap().push(request);
        tilo_actions::remote::Pending::ready(Ok(tilo_actions::remote::InvocationReceipt {
            invocation,
            status: tilo_actions::remote::ReceiptStatus::Accepted,
        }))
    }

    fn observe(
        &self,
        _caller: &tilo_actions::remote::Caller,
        _id: &tilo_actions::InvocationId,
    ) -> Option<tilo_actions::remote::InvocationState> {
        None
    }

    fn cancel(
        &self,
        _caller: &tilo_actions::remote::Caller,
        _id: &tilo_actions::InvocationId,
    ) -> Result<(), tilo_actions::remote::ActionHostError> {
        Err(tilo_actions::remote::ActionHostError::UnknownInvocation)
    }
}

#[test]
fn a_result_another_process_was_handed_opens_through_the_action_host() {
    let host = PageHost::default();
    host.offered
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let search = std::sync::Arc::new(tilo_os_search::OsSearch::new(
        std::sync::Arc::new(host.clone()),
        tilo_actions::remote::Caller::local_person("owner").in_room("room"),
    ));
    let runtime = tilo_rt::try_app().unwrap();
    runtime.handle().block_on(search.refresh()).unwrap();
    let entry = search.search("travel", 1)[0].id;

    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("native.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let serving = {
        let (host, search) = (host.clone(), std::sync::Arc::clone(&search));
        std::thread::spawn(move || {
            for _ in 0..2 {
                let (stream, _) = listener.accept().unwrap();
                super::serve_connection(
                    stream,
                    &std::sync::Arc::new(crate::mcp::McpServer::new(
                        host.clone(),
                        "room".into(),
                        "owner".into(),
                    )),
                    &super::Status {
                        version: 1,
                        pid: 1,
                        room: "room".into(),
                    },
                    None,
                    &search,
                )
                .unwrap();
            }
        })
    };

    // What the app a Spotlight click woke sends: the daemon opens it.
    tilo_host_search::open(&socket, &entry.to_string()).unwrap();
    let invoked = host.invoked.lock().unwrap().clone();
    assert_eq!(invoked.len(), 1);
    assert_eq!(
        invoked[0].target.address,
        tilo_actions::ActionAddress::new("page:1", "open")
    );

    // Access is revoked before the next walk: the daemon re-checks and refuses.
    host.offered
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let refused = tilo_host_search::open(&socket, &entry.to_string()).unwrap_err();
    assert!(refused.to_string().contains("NotOffered"), "{refused}");
    assert_eq!(host.invoked.lock().unwrap().len(), 1, "nothing more ran");
    serving.join().unwrap();
}

/// The daemon's native socket answers one session's MCP requests as they
/// finish, not in the order asked: a `tilo_status` and a `tilo_cancel`
/// answer while two invokes of the same connection wait.
#[test]
fn native_socket_serves_parallel_requests() {
    use crate::mcp_http::tests::settle_tests::{GatedHost, overlapping_lines};
    let host = GatedHost::default();
    let search = std::sync::Arc::new(tilo_os_search::OsSearch::new(
        std::sync::Arc::new(host.clone()),
        tilo_actions::remote::Caller::local_person("owner").in_room("room"),
    ));
    let (client, served) = std::os::unix::net::UnixStream::pair().unwrap();
    let serving = {
        let host = host.clone();
        std::thread::spawn(move || {
            super::serve_connection(
                served,
                &std::sync::Arc::new(crate::mcp::McpServer::new(
                    host,
                    "room".into(),
                    "owner".into(),
                )),
                &super::Status {
                    version: 1,
                    pid: 1,
                    room: "room".into(),
                },
                None,
                &search,
            )
        })
    };
    overlapping_lines(&host, &client);
    serving.join().unwrap().unwrap();
}

/// The `--connect` proxy hands each reply to the client as it arrives, while
/// the connection stays open: a client that waits for one answer before it
/// asks the next must not wait for a reply that is sitting in a buffer.
#[test]
fn the_proxy_delivers_each_reply_before_the_stream_ends() {
    /// A writer that records what reached its destination: only flushed bytes.
    #[derive(Default)]
    struct Client {
        pending: Vec<u8>,
        delivered: Vec<u8>,
    }
    impl io::Write for Client {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.pending.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            self.delivered.append(&mut self.pending);
            Ok(())
        }
    }
    /// The daemon's side: one reply, then a stream that is still open.
    struct OpenStream(Vec<Vec<u8>>);
    impl io::Read for OpenStream {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let Some(chunk) = self.0.pop() else {
                return Err(io::Error::new(io::ErrorKind::WouldBlock, "still open"));
            };
            buffer[..chunk.len()].copy_from_slice(&chunk);
            Ok(chunk.len())
        }
    }
    let mut client = Client::default();
    let reply = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\n".to_vec();
    let mut daemon = OpenStream(vec![reply[16..].to_vec(), reply[..16].to_vec()]);
    let still_open = super::relay(&mut daemon, &mut client).unwrap_err();
    assert_eq!(still_open.kind(), io::ErrorKind::WouldBlock);
    assert_eq!(
        client.delivered, reply,
        "the whole reply reached the client while the stream is open"
    );
}
