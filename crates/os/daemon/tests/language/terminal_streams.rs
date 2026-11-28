//! A workspace's Terminal session on the device that runs its shell: the stream it hands out is
//! taken by the device that asked and nobody else, a device that leaves comes back to the session
//! by its id, and every command line typed is recorded on the session, once.

#![allow(
    clippy::significant_drop_tightening,
    reason = "the screen and the connection stay up until the test ends"
)]

use std::time::{Duration, Instant};

use iroh::{Endpoint, EndpointAddr};
use serde_json::json;
use tilo_actions::{ActionAddress, InvocationUpdate};
use ush::environment_terminals::{
    FromTerminal, NOT_HANDED, ToTerminal,
    quic::{Receiving, Sending, dial},
    wire,
};

use crate::language::host;

/// How long one step of a device's use of its terminal may take.
const PATIENCE: Duration = Duration::from_secs(60);
/// The command line typed at the prompt.
const TYPED: &str = "mkdir sub; cd sub; echo he\"\"llo";

/// A device's terminal on a stream it took: what it drew so far.
struct Screen {
    _connection: iroh::endpoint::Connection,
    to: Sending,
    from: Receiving,
    drawn: Vec<u8>,
    seen: usize,
}

impl Screen {
    /// Dial `address` from `endpoint` and present `stream`: the screen, and how the host
    /// answered.
    fn take(endpoint: &Endpoint, address: &EndpointAddr, stream: &str) -> (Self, wire::Welcome) {
        let runtime = tokio::runtime::Handle::current();
        let (connection, mut to, mut from) = dial(endpoint, address.clone(), &runtime)
            .unwrap_or_else(|error| panic!("the host's terminal streams answer: {error}"));
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
        (
            Self {
                _connection: connection,
                to,
                from,
                drawn: Vec::new(),
                seen: 0,
            },
            welcome,
        )
    }

    fn type_now(&mut self, text: &str) {
        wire::write_to(&mut self.to, &ToTerminal::In(text.as_bytes().to_vec())).unwrap();
    }

    /// Wait for the shell's prompt (its default ends `$ `, or `# ` for root), then type `text`.
    fn type_at_prompt(&mut self, text: &str) {
        self.until_one(&["$ ", "# "]);
        self.type_now(text);
    }

    /// Wait until the terminal draws one of `needles`.
    fn until_one(&mut self, needles: &[&str]) {
        loop {
            let unseen = self.drawn.get(self.seen..).unwrap_or_default().to_vec();
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
            let text = String::from_utf8_lossy(&unseen).into_owned();
            match wire::read_from(&mut self.from).unwrap() {
                Some(FromTerminal::Out(bytes)) => self.drawn.extend(bytes),
                other => panic!("{other:?} before {needles:?}: {text:?}"),
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

/// A device's use of its terminal, on a thread of its own, within [`PATIENCE`].
async fn within<T: Send + 'static>(step: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::time::timeout(PATIENCE, tokio::task::spawn_blocking(step))
        .await
        .expect("the terminal answers in time")
        .unwrap()
}

fn is_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The stream and session an open answered, or why it did not.
fn opened(
    update: Result<InvocationUpdate, tilo_actions::remote::ActionCallError>,
) -> (String, String) {
    let InvocationUpdate::Succeeded { output, .. } =
        update.unwrap_or_else(|refused| panic!("the session's shell opens: {refused:?}"))
    else {
        panic!("the session's shell opens");
    };
    let value = output.value.unwrap_or_default();
    let text = |name: &str| value[name].as_str().map(str::to_owned);
    let stream = text("stream").expect("a stream");
    let session = text("session").expect("a session");
    assert!(is_id(&stream) && is_id(&session), "{value}");
    (stream, session)
}

/// Whether the workspace's Terminal session records `needle`, asked again until it does.
async fn terminal_holds(host: &host::Host, needle: &str) -> bool {
    let deadline = Instant::now().checked_add(PATIENCE).unwrap();
    loop {
        if crate::language::agent_fixture::terminal_holds(host, needle).await {
            return true;
        }
        if Instant::now() >= deadline {
            let held = crate::language::agent_fixture::terminal_commands(&host.agent()).await;
            panic!("the session never records {needle:?}; it holds {held:?}");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[test]
fn a_members_terminal_stream_is_its_own_and_every_line_typed_is_recorded_once() {
    host::run_with(&["terminal"], |host| async move {
        let workspace = host.workspace().to_owned();
        let laptop = host.member("laptop", &workspace).await;
        let phone = host.member("phone", &workspace).await;
        // The workspace's own session, the one every Terminal window shows: the Terminal's root
        // `session`, whose id its package's ids lock fixes.
        let provider = ui::apps::TERMINAL;
        let open = ActionAddress::new("tl.so.tilo.terminal.Session/root-vgkud5dv", "open-terminal");
        let (stream, session) = opened(
            laptop
                .call(provider, open.clone(), &json!({"cols": 100, "rows": 30}))
                .await,
        );
        let address = host.daemon().endpoint_addr();

        // The device that did not open it is refused the stream, which stays the opener's.
        let (phone_endpoint, address_of_phone) = (phone.endpoint().clone(), address.clone());
        let refused_stream = stream.clone();
        let refused = within(move || {
            let (_, welcome) = Screen::take(&phone_endpoint, &address_of_phone, &refused_stream);
            welcome
        })
        .await;
        assert_eq!(
            refused,
            wire::Welcome::Refused {
                reason: NOT_HANDED.into()
            },
            "a device that did not open it is refused"
        );

        // The opener types into the shell and sees it answer, and the line is recorded.
        let (laptop_endpoint, address_of_laptop) = (laptop.endpoint().clone(), address.clone());
        let (stream_typed, session_typed) = (stream.clone(), session.clone());
        let screen = within(move || {
            let (mut screen, welcome) =
                Screen::take(&laptop_endpoint, &address_of_laptop, &stream_typed);
            assert_eq!(
                welcome,
                wire::Welcome::Opened {
                    session: session_typed
                },
                "the stream is still the opener's"
            );
            screen.type_at_prompt(&format!("{TYPED}\r"));
            screen.until_one(&["hello"]);
            screen
        })
        .await;
        assert!(
            terminal_holds(host.as_ref(), TYPED).await,
            "the command line typed is recorded on the session"
        );

        // The device leaves; its session is kept, and it comes back to it by its id.
        within(move || drop(screen)).await;
        let (back_stream, back_session) = opened(
            laptop
                .call(
                    provider,
                    open.clone(),
                    &json!({"cols": 100, "rows": 30, "attach": session}),
                )
                .await,
        );
        assert_eq!(back_session, session, "the same session comes back");
        assert_ne!(back_stream, stream, "each open hands a stream of its own");

        // Its shell exits, and the stream ends with it.
        let (laptop_endpoint, address_of_laptop) = (laptop.endpoint().clone(), address.clone());
        let ended = within(move || {
            let (mut screen, welcome) =
                Screen::take(&laptop_endpoint, &address_of_laptop, &back_stream);
            assert!(
                matches!(welcome, wire::Welcome::Opened { .. }),
                "{welcome:?}"
            );
            screen.type_now("echo \"at:$PWD\"\r");
            screen.until_one(&["at:"]);
            screen.until_one(&["/sub"]);
            screen.type_at_prompt("exit 5\r");
            screen.ending()
        })
        .await;
        assert_eq!(
            ended,
            Some(FromTerminal::Exited(5)),
            "the shell's exit ends the stream"
        );
        assert!(
            terminal_holds(host.as_ref(), "at:$PWD").await,
            "the second line is recorded too"
        );

        // Its shell exited: there is no session to come back to.
        let again = laptop
            .call(
                provider,
                open,
                &json!({"cols": 100, "rows": 30, "attach": session}),
            )
            .await;
        assert!(
            again.is_err(),
            "the ended session takes nobody back: {again:?}"
        );
    });
}
