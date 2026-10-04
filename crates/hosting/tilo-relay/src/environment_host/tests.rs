//! Sessions, re-attachment, quotas, tenancy and reaping, against an
//! in-memory guest.

use std::sync::Mutex;

use tilo_relay_protocol::environment::DirectoryGrant;
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream, ReadHalf, WriteHalf};
use tokio::sync::oneshot;

use super::*;

#[test]
fn worker_without_environments_needs_no_local_guest_backend() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("members");
    let key = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
    let worker = worker_environments(None, &root, &key, &temp.path().join("missing-key")).unwrap();
    assert!(worker.host.is_none() && worker.legacy.is_none());
    assert!(!root.exists());
}

#[test]
fn configured_local_environment_host_requires_a_supported_backend() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("members");
    let key = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
    let config = tilo_relay_protocol::config::EnvironmentsConfig {
        capacity: 3,
        member_sessions: 2,
        ..Default::default()
    };
    let result = worker_environments(Some(&config), &root, &key, &temp.path().join("missing-key"));
    #[cfg(target_os = "linux")]
    {
        let worker = result.unwrap();
        let host = worker.host.expect("Linux retains the ush backend");
        assert_eq!(host.config().capacity, 3);
        assert_eq!(host.config().quota.max_sessions, 2);
    }
    #[cfg(not(target_os = "linux"))]
    {
        let error = result
            .err()
            .expect("unsupported hosting must fail explicitly");
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        assert!(error.to_string().contains("Linux"));
        assert!(!root.exists(), "refusal must not create a member area");
    }
}

/// A guest that prints a banner, then echoes its input.
pub(crate) struct EchoLauncher {
    pub(crate) sizes: Arc<Mutex<Vec<PtySize>>>,
    /// The tools each record has on the worker, as a supervisor would
    /// leave them after building one.
    pub(crate) tools: Arc<Mutex<Vec<ToolImage>>>,
    pub(crate) launched: Arc<Mutex<Vec<(MemberIdentity, PathBuf)>>>,
}

impl EchoLauncher {
    pub(crate) fn new() -> Self {
        Self {
            sizes: Arc::default(),
            tools: Arc::default(),
            launched: Arc::default(),
        }
    }
}

struct EchoControl {
    stop: Mutex<Option<oneshot::Sender<()>>>,
    sizes: Arc<Mutex<Vec<PtySize>>>,
}

impl GuestControl for EchoControl {
    fn resize(&self, size: PtySize) -> io::Result<()> {
        self.sizes.lock().unwrap().push(size);
        Ok(())
    }

    fn terminate(&self) {
        let stop = self.stop.lock().unwrap().take();
        if let Some(stop) = stop {
            let _ = stop.send(());
        }
    }

    fn exit_code(&self) -> Option<i32> {
        Some(0)
    }
}

impl GuestLauncher for EchoLauncher {
    fn current_tools(
        &self,
        _member: &MemberContext,
        _record: &EnvironmentRecordRef,
    ) -> Option<Vec<ToolImage>> {
        Some(self.tools.lock().unwrap().clone())
    }

    fn launch(&self, member: &MemberContext, request: &GuestRequest) -> Result<Guest, LaunchError> {
        self.launched
            .lock()
            .unwrap()
            .push((member.identity, member.area.root().to_path_buf()));
        let (host_side, mut guest_side) = tokio::io::duplex(1 << 20);
        let (output, input) = tokio::io::split(host_side);
        let (stop, mut stopped) = oneshot::channel();
        let banner = format!("{}$ ", request.spec.name);
        tokio::spawn(async move {
            let _ = guest_side.write_all(banner.as_bytes()).await;
            let mut buffer = [0; 1024];
            loop {
                tokio::select! {
                    _ = &mut stopped => break,
                    read = guest_side.read(&mut buffer) => match read {
                        Ok(0) | Err(_) => break,
                        Ok(read) => {
                            let echoed = buffer.get(..read).unwrap_or_default().to_vec();
                            if guest_side.write_all(&echoed).await.is_err() {
                                break;
                            }
                        }
                    },
                }
            }
        });
        Ok(Guest {
            output: Box::pin(output),
            input: Box::pin(input),
            control: Box::new(EchoControl {
                stop: Mutex::new(Some(stop)),
                sizes: Arc::clone(&self.sizes),
            }),
        })
    }
}

#[derive(Default)]
pub(crate) struct RecordingMeter(pub(crate) Mutex<Vec<MeterEvent>>);

impl Metering for RecordingMeter {
    fn record(&self, event: MeterEvent) {
        self.0.lock().unwrap().push(event);
    }
}

pub(crate) fn spec() -> EnvironmentSpec {
    EnvironmentSpec {
        name: "dev".into(),
        runtime: tilo_relay_protocol::environment::RuntimeImages {
            release: "2026.10".into(),
            images: [(
                tilo_relay_protocol::environment::this_architecture().to_owned(),
                "tapp:01".to_owned(),
            )]
            .into(),
        },
        tools: Vec::new(),
        grants: vec![DirectoryGrant {
            path: "/home/alice/Projects".into(),
            writable: true,
        }],
        home: HomePolicy::Persistent,
    }
}

pub(crate) fn open_request() -> SessionRequest {
    SessionRequest::Open {
        record: EnvironmentRecordRef {
            workspace_id: 7,
            record_id: "env-1".into(),
        },
        spec: spec(),
        size: PtySize { cols: 80, rows: 24 },
        term: None,
    }
}

fn member(user: u8, device: u8) -> MemberIdentity {
    MemberIdentity {
        user: [user; 32],
        device: [device; 32],
    }
}

struct Fixture {
    host: EnvironmentHost,
    meter: Arc<RecordingMeter>,
    launcher: Arc<EchoLauncher>,
    _root: tempfile::TempDir,
}

fn fixture(quota: MemberQuota, idle_timeout: Duration) -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let meter = Arc::new(RecordingMeter::default());
    let launcher = Arc::new(EchoLauncher::new());
    let host = EnvironmentHost::new(
        EnvironmentHostConfig {
            root: root.path().to_path_buf(),
            quota,
            idle_timeout,
            capacity: 8,
        },
        Arc::clone(&launcher) as Arc<dyn GuestLauncher>,
        Arc::clone(&meter) as Arc<dyn Metering>,
    );
    Fixture {
        host,
        meter,
        launcher,
        _root: root,
    }
}

/// A terminal attached through `host.serve` over an in-memory stream.
struct Terminal {
    stream: DuplexStream,
    serving: tokio::task::JoinHandle<Result<(), EnvironmentStreamError>>,
}

impl Terminal {
    fn connect(
        host: &EnvironmentHost,
        who: MemberIdentity,
        session: SessionId,
        request: SessionRequest,
    ) -> Self {
        let (stream, server) = tokio::io::duplex(1 << 20);
        let host = host.clone();
        let serving = tokio::spawn(async move {
            let (mut receive, mut send): (ReadHalf<DuplexStream>, WriteHalf<DuplexStream>) =
                tokio::io::split(server);
            host.serve(who, session, request, &mut send, &mut receive)
                .await
        });
        Self { stream, serving }
    }

    async fn next(&mut self) -> Option<ServerFrame> {
        tilo_test_env::within("the next environment frame", read_message(&mut self.stream))
            .await
            .unwrap()
    }

    async fn send(&mut self, frame: &ClientFrame) {
        write_message(&mut self.stream, frame).await.unwrap();
    }

    /// Read output until `expected` has arrived in full.
    async fn expect_output(&mut self, expected: &[u8]) {
        let mut seen = Vec::new();
        while !seen.ends_with(expected) {
            match self.next().await {
                Some(ServerFrame::Output(bytes)) => seen.extend(bytes),
                other => panic!("expected output, got {other:?}"),
            }
        }
    }

    async fn finish(self) {
        drop(self.stream);
        tilo_test_env::within("the environment stream to end", self.serving)
            .await
            .unwrap()
            .unwrap();
    }
}

#[tokio::test]
async fn a_session_outlives_its_terminal_and_reattaches_with_scrollback() {
    let fixture = fixture(MemberQuota::default(), Duration::from_secs(3600));
    let alice = member(1, 10);
    let session = [5; 16];
    let mut first = Terminal::connect(&fixture.host, alice, session, open_request());
    assert_eq!(
        first.next().await,
        Some(ServerFrame::Opened {
            session,
            resumed: false,
            scrollback: Vec::new(),
        })
    );
    first.expect_output(b"dev$ ").await;
    first
        .send(&ClientFrame::Input(b"echo one\n".to_vec()))
        .await;
    first.expect_output(b"echo one\n").await;
    first.send(&ClientFrame::Detach).await;
    first.finish().await;
    assert!(fixture.host.has_session(&session));

    // The guest keeps running and its output keeps accumulating while
    // nobody is attached; a re-attach from another of Alice's devices sees
    // all of it first.
    let other_device = member(1, 11);
    let mut second = Terminal::connect(
        &fixture.host,
        other_device,
        session,
        SessionRequest::Attach {
            session,
            size: PtySize {
                cols: 132,
                rows: 43,
            },
        },
    );
    let Some(ServerFrame::Opened {
        resumed: true,
        scrollback,
        ..
    }) = second.next().await
    else {
        panic!("re-attach must open with the scrollback");
    };
    assert_eq!(scrollback, b"dev$ echo one\n");
    assert!(
        fixture.launcher.sizes.lock().unwrap().contains(&PtySize {
            cols: 132,
            rows: 43
        }),
        "a re-attach takes the new terminal's size"
    );
    second.send(&ClientFrame::Input(b"two\n".to_vec())).await;
    second.expect_output(b"two\n").await;

    // A third attachment supersedes the second.
    let mut third = Terminal::connect(
        &fixture.host,
        alice,
        session,
        SessionRequest::Attach {
            session,
            size: PtySize { cols: 80, rows: 24 },
        },
    );
    assert!(matches!(
        third.next().await,
        Some(ServerFrame::Opened { resumed: true, .. })
    ));
    assert_eq!(second.next().await, Some(ServerFrame::Superseded));
    second.finish().await;

    third.send(&ClientFrame::Terminate).await;
    assert_eq!(
        third.next().await,
        Some(ServerFrame::Exited { code: Some(0) })
    );
    third.finish().await;
    tilo_test_env::within("the ended session to be forgotten", async {
        while fixture.host.has_session(&session) {
            tokio::task::yield_now().await;
        }
    })
    .await;
    let events = fixture.meter.0.lock().unwrap().clone();
    assert_eq!(
        events.first(),
        Some(&MeterEvent::SessionOpened {
            member: alice,
            session
        })
    );
    assert!(events.iter().any(
        |event| matches!(event, MeterEvent::SessionClosed { member, .. } if *member == alice)
    ));
}

#[tokio::test]
async fn another_member_can_neither_attach_nor_see_the_session() {
    let fixture = fixture(MemberQuota::default(), Duration::from_secs(3600));
    let session = [6; 16];
    let mut alice = Terminal::connect(&fixture.host, member(1, 10), session, open_request());
    assert!(matches!(
        alice.next().await,
        Some(ServerFrame::Opened { .. })
    ));
    let mut bob = Terminal::connect(
        &fixture.host,
        member(2, 20),
        session,
        SessionRequest::Attach {
            session,
            size: PtySize { cols: 80, rows: 24 },
        },
    );
    assert!(matches!(
        bob.next().await,
        Some(ServerFrame::Refused {
            reason: Refusal::NotOwner,
            ..
        })
    ));
    bob.finish().await;
    let mut nobody = Terminal::connect(
        &fixture.host,
        member(2, 20),
        [9; 16],
        SessionRequest::Attach {
            session: [9; 16],
            size: PtySize { cols: 80, rows: 24 },
        },
    );
    assert!(matches!(
        nobody.next().await,
        Some(ServerFrame::Refused {
            reason: Refusal::UnknownSession,
            ..
        })
    ));
    nobody.finish().await;
    // Alice's terminal was not disturbed.
    alice.expect_output(b"dev$ ").await;
    // Each member's guest is launched in their own area.
    let launched = fixture.launcher.launched.lock().unwrap().clone();
    assert_eq!(launched.len(), 1);
    assert!(launched[0].1.ends_with(hex::encode([1u8; 16])));
}

#[tokio::test]
async fn member_quotas_refuse_sessions_and_disk() {
    let fixture = fixture(
        MemberQuota {
            max_sessions: 1,
            disk_bytes: 1024,
            ..MemberQuota::default()
        },
        Duration::from_secs(3600),
    );
    let alice = member(1, 10);
    let mut first = Terminal::connect(&fixture.host, alice, [1; 16], open_request());
    assert!(matches!(
        first.next().await,
        Some(ServerFrame::Opened { .. })
    ));
    let mut second = Terminal::connect(&fixture.host, alice, [2; 16], open_request());
    assert!(matches!(
        second.next().await,
        Some(ServerFrame::Refused {
            reason: Refusal::Quota(QuotaKind::Sessions),
            ..
        })
    ));
    second.finish().await;
    // Another member is not limited by Alice's sessions, only by their disk.
    let bob = member(2, 20);
    let area = MemberArea::of(&fixture.host.config().root, &bob.user);
    std::fs::create_dir_all(area.files()).unwrap();
    std::fs::write(area.files().join("big"), vec![0; 2048]).unwrap();
    let mut bobs = Terminal::connect(&fixture.host, bob, [3; 16], open_request());
    assert!(matches!(
        bobs.next().await,
        Some(ServerFrame::Refused {
            reason: Refusal::Quota(QuotaKind::Disk),
            ..
        })
    ));
    bobs.finish().await;
    std::fs::remove_file(area.files().join("big")).unwrap();
    let mut bobs = Terminal::connect(&fixture.host, bob, [3; 16], open_request());
    assert!(matches!(
        bobs.next().await,
        Some(ServerFrame::Opened { .. })
    ));
    assert_eq!(fixture.host.member_sessions(&alice.user), 1);
    assert_eq!(fixture.host.member_sessions(&bob.user), 1);
    let refused: Vec<_> = fixture
        .meter
        .0
        .lock()
        .unwrap()
        .iter()
        .filter_map(|event| match event {
            MeterEvent::QuotaRefused { member, quota } => Some((*member, *quota)),
            _ => None,
        })
        .collect();
    assert_eq!(
        refused,
        vec![(alice, QuotaKind::Sessions), (bob, QuotaKind::Disk)]
    );
}

#[tokio::test]
async fn detached_quiet_sessions_are_reaped_and_attached_ones_are_not() {
    let fixture = fixture(MemberQuota::default(), Duration::from_secs(2));
    let alice = member(1, 10);
    let mut kept = Terminal::connect(&fixture.host, alice, [1; 16], open_request());
    assert!(matches!(
        kept.next().await,
        Some(ServerFrame::Opened { .. })
    ));
    let mut left = Terminal::connect(&fixture.host, alice, [2; 16], open_request());
    assert!(matches!(
        left.next().await,
        Some(ServerFrame::Opened { .. })
    ));
    left.send(&ClientFrame::Detach).await;
    left.finish().await;

    // Just detached: not idle yet.
    assert!(fixture.host.reap_idle().is_empty());
    let reaped = tilo_test_env::within("the detached session to go idle", async {
        loop {
            let reaped = fixture.host.reap_idle();
            if !reaped.is_empty() {
                return reaped;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    assert_eq!(reaped, vec![[2; 16]]);
    tilo_test_env::within("the reaped session to end", async {
        while fixture.host.has_session(&[2; 16]) {
            tokio::task::yield_now().await;
        }
    })
    .await;
    // The attached session is never idle, however long it is quiet.
    assert!(fixture.host.has_session(&[1; 16]));
    assert!(fixture.host.reap_idle().is_empty());
}

#[test]
fn scrollback_keeps_the_newest_bytes_within_its_bound() {
    let slot = Slot {
        id: [0; 16],
        owner: member(1, 1),
        record: EnvironmentRecordRef {
            workspace_id: 1,
            record_id: "r".into(),
        },
        known_tools: Mutex::new(Vec::new()),
        opened_at: Instant::now(),
        input: tokio::sync::Mutex::new(Box::pin(tokio::io::sink())),
        control: Box::new(EchoControl {
            stop: Mutex::new(None),
            sizes: Arc::default(),
        }),
        shared: Mutex::new(SlotShared {
            scrollback: VecDeque::new(),
            attachment: None,
            next_generation: 0,
            last_activity: Instant::now(),
            ended: false,
            pending_tools: Vec::new(),
        }),
    };
    let chunk = vec![b'a'; MAX_PTY_CHUNK_BYTES];
    for _ in 0..(MAX_SCROLLBACK_BYTES / MAX_PTY_CHUNK_BYTES + 2) {
        slot.output(&chunk);
    }
    slot.output(b"tail");
    let scrollback: Vec<u8> = slot.lock().scrollback.iter().copied().collect();
    assert_eq!(scrollback.len(), MAX_SCROLLBACK_BYTES);
    assert!(scrollback.ends_with(b"tail"));
}

#[tokio::test]
async fn tools_built_on_the_worker_reach_the_members_terminal() {
    let fixture = fixture(MemberQuota::default(), Duration::from_secs(3600));
    let alice = member(1, 10);
    let session = [4; 16];
    let tool = |id: &str| ToolImage {
        packages: vec!["pandoc".into()],
        images: [(
            tilo_relay_protocol::environment::this_architecture().to_owned(),
            id.to_owned(),
        )]
        .into(),
    };
    let mut first = Terminal::connect(&fixture.host, alice, session, open_request());
    assert!(matches!(
        first.next().await,
        Some(ServerFrame::Opened { .. })
    ));
    first.expect_output(b"dev$ ").await;
    // `pkg install` in the session built a tool on the worker.
    fixture.launcher.tools.lock().unwrap().push(tool("tapp:01"));
    assert_eq!(fixture.host.refresh_tools().await, 1);
    assert_eq!(fixture.host.refresh_tools().await, 0, "told once");
    assert_eq!(
        first.next().await,
        Some(ServerFrame::ToolBuilt {
            record: EnvironmentRecordRef {
                workspace_id: 7,
                record_id: "env-1".into(),
            },
            tool: tool("tapp:01"),
        })
    );
    first.send(&ClientFrame::Detach).await;
    first.finish().await;
    // Built while nobody is attached: told to the next terminal.
    fixture.launcher.tools.lock().unwrap().push(tool("tapp:02"));
    assert_eq!(fixture.host.refresh_tools().await, 1);
    let mut second = Terminal::connect(
        &fixture.host,
        alice,
        session,
        SessionRequest::Attach {
            session,
            size: PtySize { cols: 80, rows: 24 },
        },
    );
    assert!(matches!(
        second.next().await,
        Some(ServerFrame::Opened { .. })
    ));
    assert!(matches!(
        second.next().await,
        Some(ServerFrame::ToolBuilt { tool: built, .. }) if built == tool("tapp:02")
    ));
}
