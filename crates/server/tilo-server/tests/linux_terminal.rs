//! A hosted workspace's Terminal session runs on a terminal for the device that opened it.
//!
//! The host's shells open the session's shell on a terminal and hand its stream to the device
//! that asked; that device dials the host's terminal streams and presents the stream's id.
//! Another device presenting the same id is refused, and the stream stays the first one's. What
//! is typed reaches the shell and what it draws comes back, whether the shell runs on the host
//! itself or on the device that takes its Linux turn, which the host splices to as its own
//! device. A device that leaves finds the session where it was when it comes back by the
//! session's id, within the keep; the shell's exit ends the stream, and the session with it.
//!
//! Every command line typed is recorded by the host as it ends, once, with where it ran, how
//! it ended and what it printed, a device attached or not; an open that comes back to the
//! session records nothing more.

#![allow(
    clippy::significant_drop_tightening,
    reason = "the host, the member and their routers stay up until the test ends"
)]

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use iroh::endpoint::Connection;
use iroh::{Endpoint, EndpointAddr};
use tilo_actions::remote::environment_commands::Person;
use tilo_agent_session::shell::{Opened, Opening, Recorder, ShellRan, WorkspaceShell as _};
use tilo_server::agents::TerminalShells;
use tilo_server::agents::{HostShells, ShellsAt};
use tilo_server::machine_agents::MachineAgentsServer;
use tilo_user_identity::DeviceCapabilities;
use ush::environment_terminals::quic::{Receiving, Sending, dial};
use ush::environment_terminals::{
    ALPN, EnvironmentTerminals, FromTerminal, NOT_HANDED, ToTerminal, wire,
};
use ush::hosted_vm::test_host::TestHost;

use super::linux_turn::{address_of, member_at, placed, stop};
use crate::common::{TestUser, device_endpoint};

/// How long one step of a device's use of its terminal may take.
const PATIENCE: Duration = Duration::from_secs(60);

/// A host's terminal streams over `shells`, on an endpoint of their own.
struct Streams {
    _router: iroh::protocol::Router,
    address: EndpointAddr,
}

async fn streams(shells: &Arc<HostShells>) -> Streams {
    let host = TestUser::fresh("host").enrol("terminals", DeviceCapabilities::member());
    let endpoint = device_endpoint(&host).await;
    let address = member_at(host.device_pubkey(), &address_of(&endpoint));
    let terminals = TerminalShells::default();
    terminals.serve_for(Arc::clone(shells));
    let router = iroh::protocol::Router::builder(endpoint)
        .accept(ALPN, terminals.route())
        .spawn();
    Streams {
        _router: router,
        address,
    }
}

/// Who opens the terminals: whom what the shell makes records as its author.
fn ada() -> Person {
    Person {
        name: "Ada Lovelace".into(),
        address: "ada@eu1.tilo.sh".into(),
    }
}

/// What the host recorded of an open's session: the runs, in the order they ended.
#[derive(Clone, Default)]
struct Recorded(Arc<Mutex<Vec<ShellRan>>>);

impl Recorded {
    fn recorder(&self) -> Recorder {
        let runs = Arc::clone(&self.0);
        Arc::new(move |ran| runs.lock().unwrap().push(ran))
    }

    fn runs(&self) -> Vec<ShellRan> {
        self.0.lock().unwrap().clone()
    }

    /// The runs, once there are `count` of them.
    async fn once(&self, count: usize) -> Vec<ShellRan> {
        let deadline = Instant::now().checked_add(PATIENCE).unwrap();
        loop {
            let runs = self.runs();
            if runs.len() >= count {
                return runs;
            }
            assert!(
                Instant::now() < deadline,
                "{count} runs never recorded: {runs:?}"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

/// A run recorded of `command`, ended with `exit`, having printed `printed`.
fn is_run(ran: &ShellRan, command: &str, exit: i32, printed: &str) {
    assert_eq!(ran.command, command, "{ran:?}");
    assert_eq!(ran.exit, exit, "{ran:?}");
    let output = String::from_utf8_lossy(&ran.output);
    assert!(output.contains(printed), "{printed:?} printed: {ran:?}");
    assert!(
        !output.contains(command),
        "the line as typed is no output: {ran:?}"
    );
    assert!(
        !ran.cwd.is_empty() && ran.started_ms <= ran.ended_ms,
        "{ran:?}"
    );
}

/// Open `workspace`'s session `session` for `device`, coming back to `attach` if named, its
/// runs recorded to `recorded`.
async fn open(
    shells: &Arc<HostShells>,
    (workspace, session): (&str, &str),
    device: [u8; 32],
    attach: Option<&str>,
    recorded: &Recorded,
) -> Result<Opened, String> {
    let shells = Arc::clone(shells);
    let opening = Opening {
        workspace: workspace.to_owned(),
        session: session.to_owned(),
        device,
        cols: 100,
        rows: 30,
        person: Some(ada()),
        attach: attach.map(str::to_owned),
    };
    let record = recorded.recorder();
    let opener = tilo_actions::remote::Caller::local_person(hex::encode([1_u8; 32]));
    tokio::task::spawn_blocking(move || shells.open(&opening, &opener, record))
        .await
        .unwrap()
}

/// `step`, a device's use of its terminal, on a thread of its own, within [`PATIENCE`].
async fn within<T: Send + 'static>(step: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::time::timeout(PATIENCE, tokio::task::spawn_blocking(step))
        .await
        .expect("the terminal answers in time")
        .unwrap()
}

/// A device's terminal on a stream it took: what it drew so far, read past what was looked for.
struct Screen {
    _connection: Connection,
    to: Sending,
    from: Receiving,
    drawn: Vec<u8>,
    seen: usize,
}

impl Screen {
    /// Dial the host at `address` from `endpoint` and present `stream`: the screen, and how
    /// the host answered.
    fn take(endpoint: &Endpoint, address: &EndpointAddr, stream: &str) -> (Self, wire::Welcome) {
        let runtime = tokio::runtime::Handle::current();
        let (connection, mut to, mut from) = dial(endpoint, address.clone(), &runtime).unwrap();
        wire::write_hello(
            &mut to,
            &wire::Hello {
                stream: stream.to_owned(),
            },
        )
        .unwrap();
        let welcome = wire::read_welcome(&mut from)
            .unwrap()
            .expect("the host answers the hello");
        let screen = Self {
            _connection: connection,
            to,
            from,
            drawn: Vec::new(),
            seen: 0,
        };
        (screen, welcome)
    }

    fn type_now(&mut self, text: &str) {
        wire::write_to(&mut self.to, &ToTerminal::In(text.as_bytes().to_vec())).unwrap();
    }

    /// Wait for the shell's prompt (its default ends `$ `, or `# ` for root), then type
    /// `text`.
    fn type_at_prompt(&mut self, text: &str) {
        self.until_one(&["$ ", "# "]);
        self.type_now(text);
    }

    /// Wait until the terminal draws `needle`.
    fn until(&mut self, needle: &str) {
        self.until_one(&[needle]);
    }

    /// Wait until the terminal draws one of `needles`.
    fn until_one(&mut self, needles: &[&str]) {
        loop {
            let unseen = self.drawn.get(self.seen..).unwrap_or_default();
            let found = needles.iter().find_map(|needle| {
                unseen
                    .windows(needle.len())
                    .position(|window| window == needle.as_bytes())
                    .map(|at| at.saturating_add(needle.len()))
            });
            if let Some(past) = found {
                self.seen = self.seen.saturating_add(past);
                return;
            }
            let needle = needles;
            let text = String::from_utf8_lossy(unseen).into_owned();
            match wire::read_from(&mut self.from).unwrap() {
                Some(FromTerminal::Out(bytes)) => self.drawn.extend(bytes),
                other => panic!("{other:?} before {needle:?}: {text:?}"),
            }
        }
    }

    /// How the stream ended, past what it drew first.
    fn ending(&mut self) -> Option<FromTerminal> {
        loop {
            match wire::read_from(&mut self.from).unwrap() {
                Some(FromTerminal::Out(_)) => {}
                other => return other,
            }
        }
    }
}

/// Two people's devices: the one that opens the terminal, and one that did not.
struct Devices {
    opener: [u8; 32],
    opening: Endpoint,
    other: Endpoint,
}

async fn devices() -> Devices {
    let laptop = TestUser::fresh("Ada").enrol("laptop", DeviceCapabilities::member());
    let phone = TestUser::fresh("Grace").enrol("phone", DeviceCapabilities::member());
    Devices {
        opener: laptop.device_pubkey(),
        opening: device_endpoint(&laptop).await,
        other: device_endpoint(&phone).await,
    }
}

/// The session `opened` names, taken by `devices`: another device is refused the stream,
/// which stays the opener's, and the opener types into the shell and sees it answer.
async fn the_opener_alone_types_into_it(
    streams: &Streams,
    devices: &Devices,
    opened: &Opened,
) -> Screen {
    let (other, opening) = (devices.other.clone(), devices.opening.clone());
    let (address, opened) = (streams.address.clone(), opened.clone());
    within(move || {
        let (_, refused) = Screen::take(&other, &address, &opened.stream);
        assert_eq!(
            refused,
            wire::Welcome::Refused {
                reason: NOT_HANDED.into()
            },
            "a device that did not open it is refused"
        );
        let (mut screen, welcome) = Screen::take(&opening, &address, &opened.stream);
        assert_eq!(
            welcome,
            wire::Welcome::Opened {
                session: opened.session
            },
            "the stream is still the opener's"
        );
        screen.type_at_prompt(&format!("{TYPED}\r"));
        screen.until("hello");
        screen
    })
    .await
}

/// The first command line typed.
const TYPED: &str = "mkdir sub; cd sub; echo he\"\"llo";

fn is_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_terminal_on_the_device_running_the_shell_is_spliced_to_the_device_that_opened_it() {
    let placed = placed().await;
    let shells = Arc::new(HostShells::at(ShellsAt::Devices(Arc::clone(
        &placed.shells,
    ))));
    let streams = streams(&shells).await;
    let devices = devices().await;
    let workspace = placed.workspace.clone();

    let (recorded, again) = (Recorded::default(), Recorded::default());
    let opened = open(&shells, (&workspace, "s1"), devices.opener, None, &recorded)
        .await
        .unwrap();
    assert!(
        is_id(&opened.stream) && is_id(&opened.session),
        "{opened:?}"
    );
    let mut screen = the_opener_alone_types_into_it(&streams, &devices, &opened).await;
    is_run(&recorded.once(1).await[0], TYPED, 0, "hello");

    // The device leaves while a command runs; it is recorded all the same, and the session
    // is kept on the VM host, to come back to by its id.
    within(move || {
        screen.type_at_prompt("sleep 1; echo lat\"\"er\r");
        screen.until("sleep 1");
        drop(screen);
    })
    .await;
    is_run(
        &recorded.once(2).await[1],
        "sleep 1; echo lat\"\"er",
        0,
        "later",
    );
    assert!(
        open(
            &shells,
            (&workspace, "s2"),
            devices.opener,
            Some(&opened.session),
            &again
        )
        .await
        .is_err(),
        "a session's id takes no other session back"
    );
    let back = open(
        &shells,
        (&workspace, "s1"),
        devices.opener,
        Some(&opened.session),
        &again,
    )
    .await
    .unwrap();
    assert_eq!(back.session, opened.session);
    assert_ne!(
        back.stream, opened.stream,
        "each open hands a stream of its own"
    );
    let (opening, address) = (devices.opening.clone(), streams.address.clone());
    let ended = within(move || {
        let (mut screen, welcome) = Screen::take(&opening, &address, &back.stream);
        assert!(
            matches!(welcome, wire::Welcome::Opened { .. }),
            "{welcome:?}"
        );
        screen.type_now("echo \"at:$PWD\"\r");
        screen.until("at:");
        screen.until("/sub");
        screen.type_at_prompt("exit 5\r");
        screen.ending()
    })
    .await;
    assert_eq!(
        ended,
        Some(FromTerminal::Exited(5)),
        "the shell's exit ends the stream"
    );
    let runs = recorded.once(3).await;
    is_run(&runs[2], "echo \"at:$PWD\"", 0, "/sub");
    assert!(runs[2].cwd.ends_with("/sub"), "{runs:?}");
    // Each run once, by the open that started the shell; `exit` is no run.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(recorded.runs().len(), 3, "{:?}", recorded.runs());
    assert!(again.runs().is_empty(), "{:?}", again.runs());

    // Its shell exited: there is no session to come back to.
    let deadline = Instant::now().checked_add(PATIENCE).unwrap();
    while open(
        &shells,
        (&workspace, "s1"),
        devices.opener,
        Some(&opened.session),
        &again,
    )
    .await
    .is_ok()
    {
        assert!(Instant::now() < deadline, "the ended session is still kept");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    drop((streams, shells));
    stop(placed).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_terminal_on_the_host_itself_is_the_device_that_opened_it() {
    let state = tempfile::tempdir().unwrap();
    // A host selling the workspace's VM, which keeps what each program it starts was given.
    let vms = TestHost::start(state.path());
    vms.add_card("acct-1");
    let shells = Arc::new(HostShells::at(ShellsAt::Here(
        EnvironmentTerminals::new(Arc::new(ush::workspace_shell::WorkspaceShells::new(
            state.path().join("shells"),
            Some(vms.target("acct-1")),
        ))),
        Arc::new(MachineAgentsServer::new(None)),
    )));
    let streams = streams(&shells).await;
    let devices = devices().await;

    let recorded = Recorded::default();
    let opened = open(&shells, ("ws1", "s1"), devices.opener, None, &recorded)
        .await
        .unwrap();
    let mut screen = the_opener_alone_types_into_it(&streams, &devices, &opened).await;
    let ended = within(move || {
        screen.type_at_prompt("git status\r");
        screen.until("ran git status");
        screen.type_at_prompt("false\r");
        screen.type_at_prompt("exit 3\r");
        screen.ending()
    })
    .await;
    assert_eq!(ended, Some(FromTerminal::Exited(3)));

    // The program the typed command started in the VM commits as the one who opened it.
    let launched = vms.launched();
    let (_, env) = launched
        .iter()
        .find(|(argv, _)| argv.first().is_some_and(|program| program == "git"))
        .unwrap_or_else(|| panic!("git started in the VM: {launched:?}"));
    for (name, value) in [
        ("GIT_AUTHOR_NAME", "Ada Lovelace"),
        ("GIT_COMMITTER_NAME", "Ada Lovelace"),
        ("GIT_AUTHOR_EMAIL", "ada@eu1.tilo.sh"),
        ("GIT_COMMITTER_EMAIL", "ada@eu1.tilo.sh"),
    ] {
        assert!(
            env.iter().any(|(key, set)| key == name && set == value),
            "{name}={value} in {env:?}"
        );
    }

    let runs = recorded.once(3).await;
    is_run(&runs[0], TYPED, 0, "hello");
    assert_eq!(
        (runs[1].command.as_str(), runs[1].exit),
        ("git status", 0),
        "{runs:?}"
    );
    assert_eq!(
        (runs[2].command.as_str(), runs[2].exit),
        ("false", 1),
        "{runs:?}"
    );
    assert!(runs[2].cwd.ends_with("/sub"), "{runs:?}");
    assert_eq!(runs.len(), 3, "each run once; `exit` is no run: {runs:?}");
}
