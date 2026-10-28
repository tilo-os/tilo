//! Bluetooth pairing prompts, end to end, against a stand-in `BlueZ` on a
//! private peer-to-peer bus.
//!
//! The compositor's pairing service registers its `org.bluez.Agent1` with the
//! fake daemon as the default agent; the fake then calls the agent exactly as
//! `bluetoothd` does. Each question must reach the shell's modal dialog,
//! be withheld from captures, and return to the daemon only what the person
//! answered in that dialog — a click, typed digits, or nothing at all.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects,
    clippy::needless_pass_by_value,
    reason = "in a test these are the assertion, not a hazard; deadlines are seconds from \
              now, and the stand-in daemon's methods take D-Bus arguments by value"
)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use automerge::{AutoCommit, ROOT, ReadDoc};
use shell::bluetooth::{PairingPromptHandle, PairingService};
use shell::bluetooth_prompt::ENTRY_FIELD_ID;
use shell::shell::frame::FrameBuilder;
use tilo_bluez::{AGENT_CAPABILITY, AGENT_PATH, BluezClient, DeviceNames, PairingPrompt};
use tilo_lock::MutexRecover as _;
use vello::kurbo::{Point, Rect};
use winit::event::{ElementState, Modifiers, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey, SmolStr};
use zbus::connection::Builder;
use zbus::zvariant::ObjectPath;

const W: u32 = 1600;
const H: u32 = 900;
const GUID: &str = "0123456789abcdef0123456789abcdef";
const DEVICE: &str = "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF";
const DEVICE_NAME: &str = "Studio Keyboard";

/// What the stand-in daemon was asked to do.
type Calls = Arc<Mutex<Vec<String>>>;

fn record(calls: &Calls, call: String) {
    calls.lock_recover().push(call);
}

struct FakeAgentManager {
    calls: Calls,
}

#[zbus::interface(name = "org.bluez.AgentManager1")]
impl FakeAgentManager {
    fn register_agent(&self, agent: ObjectPath<'_>, capability: String) {
        record(&self.calls, format!("RegisterAgent {agent} {capability}"));
    }

    fn request_default_agent(&self, agent: ObjectPath<'_>) {
        record(&self.calls, format!("RequestDefaultAgent {agent}"));
    }

    fn unregister_agent(&self, agent: ObjectPath<'_>) {
        record(&self.calls, format!("UnregisterAgent {agent}"));
    }
}

struct FakeDevice {
    calls: Calls,
}

#[zbus::interface(name = "org.bluez.Device1")]
impl FakeDevice {
    fn cancel_pairing(&self) {
        record(&self.calls, "CancelPairing".into());
    }
}

/// The compositor's pairing service linked to a stand-in `bluetoothd`, and
/// a headless shell drawing its prompts.
struct Rig {
    rt: tokio::runtime::Runtime,
    bluez: zbus::Connection,
    calls: Calls,
    _service: PairingService,
    fb: FrameBuilder,
    doc: AutoCommit,
    focus: Option<taffy::NodeId>,
}

type Answer = zbus::Result<zbus::Message>;

impl Rig {
    fn new(timeout: Duration) -> Self {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("runtime");
        let calls = Calls::default();
        let handle = PairingPromptHandle::new();
        let (bluez, service) = rt.block_on(async {
            let (shell_socket, bluez_socket) = tokio::net::UnixStream::pair().expect("socket pair");
            let shell_side = Builder::unix_stream(shell_socket)
                .server(GUID)
                .expect("guid")
                .p2p();
            let bluez_side = Builder::unix_stream(bluez_socket)
                .p2p()
                .serve_at(
                    "/org/bluez",
                    FakeAgentManager {
                        calls: Arc::clone(&calls),
                    },
                )
                .expect("agent manager")
                .serve_at(
                    DEVICE,
                    FakeDevice {
                        calls: Arc::clone(&calls),
                    },
                )
                .expect("device");
            let (shell_conn, bluez) =
                futures_util::try_join!(shell_side.build(), bluez_side.build()).expect("p2p pair");
            let names = DeviceNames::new();
            names.set(DEVICE, DEVICE_NAME);
            let client = BluezClient::with_connection(shell_conn, names);
            let service = PairingService::start_with(client, handle.clone(), timeout)
                .await
                .expect("the agent registers with the daemon");
            (bluez, service)
        });
        // The dialog over the bare desktop: an app page behind it, painted
        // in the same ground as the capture placeholder, would make what
        // the dialog shows locally indistinguishable from the placeholder.
        let mut tree = shell::shell::state::WorkspaceTree::bootstrap_demo();
        for workspace in &mut tree.desktop.workspaces {
            for strip in &mut workspace.strips {
                strip.window_groups.clear();
            }
        }
        let mut fb = FrameBuilder::with_tree(tree, (W, H));
        fb.host_mut().set_pairing_prompt_handle(handle);
        let mut rig = Self {
            rt,
            bluez,
            calls,
            _service: service,
            fb,
            doc: AutoCommit::new(),
            focus: None,
        };
        // The first frame builds the whole desktop; take it before anything
        // is asked, so a question's clock measures only the dialog.
        rig.frame();
        rig
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock_recover().clone()
    }

    /// Call the agent as `bluetoothd` does, without waiting for the answer.
    fn ask<B>(&self, method: &'static str, body: B) -> tokio::task::JoinHandle<Answer>
    where
        B: serde::Serialize + zbus::zvariant::DynamicType + Send + Sync + 'static,
    {
        let bluez = self.bluez.clone();
        self.rt.spawn(async move {
            let agent =
                zbus::Proxy::new(&bluez, "org.bluez.Agent", AGENT_PATH, "org.bluez.Agent1").await?;
            agent.call_method(method, &body).await
        })
    }

    fn device() -> ObjectPath<'static> {
        ObjectPath::from_static_str_unchecked(DEVICE)
    }

    fn frame(&mut self) {
        self.fb.pump_apps(&mut self.doc, &ROOT);
        let _ = self.fb.paint_settled();
    }

    /// Paint until `done` holds, or fail naming `what`.
    fn until(&mut self, what: &str, done: impl Fn(&FrameBuilder) -> bool) {
        tilo_test_env::wait_until(what, || {
            self.frame();
            done(&self.fb)
        });
    }

    fn prompt(&self) -> Option<PairingPrompt> {
        self.fb
            .host()
            .pairing_prompt()
            .map(|view| view.request().prompt.clone())
    }

    fn wait_for_prompt(&mut self, expected: &PairingPrompt) {
        let expected = expected.clone();
        self.until(&format!("the prompt {expected:?}"), |fb| {
            fb.host()
                .pairing_prompt()
                .is_some_and(|view| view.request().prompt == expected)
        });
    }

    fn wait_for_no_prompt(&mut self) {
        self.until("the prompt to close", |fb| {
            fb.host().pairing_prompt().is_none()
        });
    }

    fn answer(&self, call: tokio::task::JoinHandle<Answer>) -> Answer {
        self.rt
            .block_on(tilo_test_env::within("the daemon to hear back", call))
            .expect("the call task finishes")
    }

    fn send(&mut self, event: &WindowEvent) {
        self.fb.dispatch_event(
            event,
            &Modifiers::default(),
            &mut ui::widget::Edits::lend(&mut self.doc),
            &ROOT,
            &mut self.focus,
        );
    }

    fn click(&mut self, test_id: &str) {
        let rect: Rect = *self
            .fb
            .widget_registry()
            .get(test_id)
            .unwrap_or_else(|| panic!("{test_id} is on screen"));
        let at: Point = rect.center();
        self.send(&ui::synthetic_input::cursor_moved(at.x, at.y));
        for state in [ElementState::Pressed, ElementState::Released] {
            self.send(&ui::synthetic_input::mouse_input(state, MouseButton::Left));
        }
    }

    fn key(&mut self, key: Key, text: Option<&str>) {
        for state in [ElementState::Pressed, ElementState::Released] {
            let event = ui::synthetic_input::key_event(
                key.clone(),
                text.filter(|_| state == ElementState::Pressed)
                    .map(SmolStr::new),
                state,
            );
            self.send(&ui::synthetic_input::keyboard_input(event));
            self.frame();
        }
    }

    fn type_text(&mut self, text: &str) {
        for character in text.chars() {
            let character = character.to_string();
            self.key(Key::Character(SmolStr::new(&character)), Some(&character));
        }
    }
}

fn rejected_as(answer: &Answer, name: &str) -> bool {
    matches!(answer, Err(zbus::Error::MethodError(error, _, _)) if error.as_str() == name)
}

#[test]
fn pairing_questions_are_answered_only_in_the_shells_protected_dialog() {
    // Every question here is answered; none may run into the unanswered
    // timeout, however slow the runner.
    let mut rig = Rig::new(tilo_test_env::hang_bound());

    // Phase `the_agent_is_the_default_agent`.
    assert_eq!(
        rig.calls(),
        vec![
            format!("RegisterAgent {AGENT_PATH} {AGENT_CAPABILITY}"),
            format!("RequestDefaultAgent {AGENT_PATH}"),
        ],
        "registered on the daemon it pairs through, and asked to be its default"
    );
    assert!(rig.prompt().is_none(), "nothing asked, nothing shown");

    // Phase `a_numeric_comparison_is_confirmed_in_the_dialog`.
    {
        let call = rig.ask("RequestConfirmation", (Rig::device(), 424_242_u32));
        let comparison = PairingPrompt::ConfirmPasskey { passkey: 424_242 };
        rig.wait_for_prompt(&comparison);
        let title = rig
            .fb
            .host()
            .pairing_prompt()
            .and_then(|view| {
                use ui::widget::Widget;
                view.accessibility().and_then(|node| node.name)
            })
            .unwrap_or_default();
        assert!(
            title.contains(DEVICE_NAME),
            "the dialog names the device: {title}"
        );
        // Captured, the dialog is only the placeholder; locally it is shown.
        super::capture_protection::assert_secret_prompts_withheld(
            &mut rig.fb,
            "the Bluetooth pairing dialog",
        );
        rig.click("bluetooth.pair.accept");
        rig.wait_for_no_prompt();
        assert!(
            rig.answer(call).is_ok(),
            "They match returns success to the daemon"
        );
        assert!(
            rig.fb.host().secret_prompt_rects().is_empty(),
            "no dialog, nothing withheld"
        );
    }

    // Phase `a_passkey_is_typed_into_the_dialog`.
    {
        let call = rig.ask("RequestPasskey", (Rig::device(),));
        rig.wait_for_prompt(&PairingPrompt::RequestPasskey);
        assert!(
            rig.fb.widget_registry().contains_key(ENTRY_FIELD_ID),
            "the entry field is on screen"
        );
        // Letters are not digits: the field refuses them.
        rig.type_text("65x4321");
        rig.key(Key::Named(NamedKey::Enter), None);
        rig.wait_for_no_prompt();
        let reply = rig.answer(call).expect("the typed passkey is returned");
        assert_eq!(reply.body().deserialize::<u32>().unwrap(), 654_321);
        assert!(
            rig.doc.keys(&ROOT).all(|key| !key.contains("entry")),
            "the typed code never enters a document"
        );
    }

    // Phase `declining_rejects_the_pairing`.
    {
        let call = rig.ask("RequestAuthorization", (Rig::device(),));
        rig.wait_for_prompt(&PairingPrompt::ConfirmPairing);
        rig.key(Key::Named(NamedKey::Escape), None);
        rig.wait_for_no_prompt();
        let answer = rig.answer(call);
        assert!(
            rejected_as(&answer, "org.bluez.Error.Rejected"),
            "Escape declines: {answer:?}"
        );
    }

    // Phase `a_displayed_code_follows_typed_digits_and_cancels_the_pairing`.
    {
        for entered in [0_u16, 1, 2, 3] {
            let answer =
                rig.answer(rig.ask("DisplayPasskey", (Rig::device(), 12_345_u32, entered)));
            assert!(answer.is_ok(), "a display call returns at once");
        }
        rig.wait_for_prompt(&PairingPrompt::DisplayPasskey {
            passkey: 12_345,
            entered: 3,
        });
        rig.click("bluetooth.pair.reject");
        rig.wait_for_no_prompt();
        tilo_test_env::wait_until("the pairing to be cancelled", || {
            rig.calls().iter().any(|call| call == "CancelPairing")
        });
    }

    // Phase `the_daemon_withdrawing_its_question_closes_the_dialog`.
    {
        let call = rig.ask("RequestConfirmation", (Rig::device(), 1_u32));
        rig.wait_for_prompt(&PairingPrompt::ConfirmPasskey { passkey: 1 });
        assert!(rig.answer(rig.ask("Cancel", ())).is_ok());
        rig.wait_for_no_prompt();
        let answer = rig.answer(call);
        drop(rig);
        assert!(
            answer.is_err(),
            "a withdrawn question is never answered yes: {answer:?}"
        );
    }
}

#[test]
fn an_unanswered_pairing_question_times_out_as_a_refusal() {
    let mut rig = Rig::new(Duration::from_secs(3));
    let call = rig.ask("RequestConfirmation", (Rig::device(), 7_u32));
    rig.wait_for_prompt(&PairingPrompt::ConfirmPasskey { passkey: 7 });
    let answer = rig.answer(call);
    assert!(
        rejected_as(&answer, "org.bluez.Error.Canceled"),
        "nobody answering is a cancelled pairing: {answer:?}"
    );
    rig.wait_for_no_prompt();
}
