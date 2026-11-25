//! A capture: Chat's window with an agent's reply under the persona its record keeps, and a
//! member's face beside the words they said, in both themes. The words are staged through the
//! app's own actions, the agent's reply authored by the agent; the pictures are the addresses
//! the workspace's member list carries for them.

#![cfg(target_os = "linux")]

use std::rc::Rc;

use tilo_app_window::AppWindow;
use tilo_render_protocol::patch::local::pair;
use tilo_render_protocol::patch::{self as wire, ToApp, Transport};
use tilolang_runtime::rich::RichValue;
use tilolang_runtime::{Actor, InvokeOutcome, Value};
use tilolang_tilo::background::Waker;
use tilolang_tilo::host::{Host, RootView};
use tilolang_tilo::instance::{Config, Instance, Mode, Platform};
use tilolang_tilo::names::{Installed, Told};
use tilolang_tilo::runtime::ids::MemberId;
use ui::headless::Driver;

const CHAT: &str = "so.tilo.chat";
const SOURCE: &str = include_str!("../../../../apps/tl/chat/app.tl");
const IDS: &str = include_str!("../../../../apps/tl/chat/app.ids");
/// A member's picture, and the agent's persona.
const MEMBER: &str = ui::headless::FIXTURE_PICTURE;
const PERSONA: &str = ui::headless::FIXTURE_PICTURE;

/// The Chat package, and the clock the capture shows.
struct Chat;

impl Platform for Chat {
    fn load(&self, host: &Host) -> u64 {
        host.load_builtin(SOURCE, IDS).expect("the Chat package");
        0
    }

    fn phrase(&self, id: &str, _: &[(String, String)]) -> String {
        id.to_owned()
    }

    fn now(&self) -> String {
        "2026-11-23T09:00:00Z".to_owned()
    }

    fn waker(&self) -> Waker {
        std::sync::Arc::new(|| {})
    }
}

/// `action` of Chat run as `actor`, with `args`.
fn call(instance: &Instance, actor: &Actor, action: &str, args: Vec<Value>) -> Value {
    let host = instance.host().expect("the host");
    let id = host
        .package(CHAT)
        .expect("the package")
        .meta
        .actions
        .iter()
        .find(|declared| declared.name == action)
        .unwrap_or_else(|| panic!("no action {action}"))
        .id
        .clone();
    match host
        .runtime()
        .invoke_action(actor, (id.as_str(), args))
        .unwrap_or_else(|error| panic!("{action}: {error}"))
    {
        InvokeOutcome::Done(value) => value,
        other => panic!("{action}: {other:?}"),
    }
}

/// The channel made first.
fn channel(instance: &Instance) -> tilolang_runtime::ResourceRef {
    let host = instance.host().expect("the host");
    let root = host.root_of(CHAT).expect("the Chat root");
    match host.runtime().read(&root, "channels").expect("channels") {
        Value::List(channels) => match channels.into_iter().next() {
            Some(Value::Ref(channel)) => Rc::unwrap_or_clone(channel.target),
            other => panic!("a channel: {other:?}"),
        },
        other => panic!("channels: {other:?}"),
    }
}

/// Chat's window as its app sends it, its channel open, in both themes, saved beside `out`.
#[test]
fn a_chat_reply_by_an_agent_with_a_persona_and_a_members_face_in_both_themes() {
    let (app_end, mut window_end) = pair();
    let names = Rc::new(Told::new("ada".into(), Rc::new(Installed)));
    names.tell(
        "Ada".to_owned(),
        vec![
            (MemberId::agent("scout", "leo"), "Scout".to_owned()),
            (MemberId::person("leo"), "Leo".to_owned()),
        ],
        Vec::new(),
        vec![
            (MemberId::agent("scout", "leo"), PERSONA.to_owned()),
            (MemberId::person("leo"), MEMBER.to_owned()),
        ],
    );
    let mut instance = Instance::new(
        Config {
            app: CHAT.to_owned(),
            me: "ada".into(),
            mode: Mode::Live,
            device: None,
            store: tilolang_tilo::resources::ResourceStore::in_memory(),
            root: RootView::default(),
            sink: None,
            link_home: None,
        },
        Rc::new(Chat),
        names,
        Box::new(app_end),
    );
    instance.sync();
    let ada = Actor::from(MemberId::person("ada"));
    let leo = Actor::from(MemberId::person("leo"));
    let scout = Actor::from(MemberId::agent("scout", "leo"));
    let root = instance
        .host()
        .expect("the host")
        .root_of(CHAT)
        .expect("root");
    call(
        &instance,
        &ada,
        "new_channel",
        vec![Value::resource(&root), Value::text("design")],
    );
    let channel = channel(&instance);
    let say = |from: &Actor, words: &str| {
        call(
            &instance,
            from,
            "send",
            vec![
                Value::resource(&channel),
                Value::Rich(RichValue::from_text(words)),
                Value::none(),
            ],
        )
    };
    say(&ada, "Lunch is at noon. Can you book a table, Scout?");
    say(&leo, "I can be there by half past, if that helps.");
    say(&scout, "Booked the window table for four at noon, Ada.");
    // The window asks for the channel, as a press on its place does.
    window_end
        .send(
            wire::encode_batch(&[ToApp::Open {
                resource: wire::ResourceRef {
                    id: channel.id_string(),
                    ty: channel.ty.clone(),
                },
            }])
            .expect("a batch"),
        )
        .expect("the window asks");
    instance.update();

    let window = AppWindow::new(Box::new(window_end), Vec::new());
    let mut harness = Driver::new_with_theme(
        ui::State::new(),
        Box::new(window),
        (1280, 800),
        ui::Theme::dark(),
    );
    harness.relayout();
    // The captures stand beside the build, as the other shots do: a source tree read from a
    // test may be read-only.
    let out = std::env::current_exe()
        .expect("the test's binary")
        .parent()
        .expect("its directory")
        .parent()
        .expect("the profile's directory")
        .parent()
        .expect("the build's directory")
        .join("chat-persona-shot");
    std::fs::create_dir_all(&out).expect("the captures' directory");
    harness.render_png(&out.join("chat-persona-dark.png"));
    harness
        .relayout_themed((1280, 800), ui::Theme::light())
        .expect("the light frame");
    harness.render_png(&out.join("chat-persona-light.png"));
    for theme in ["dark", "light"] {
        let path = out.join(format!("chat-persona-{theme}.png"));
        assert!(
            std::fs::metadata(&path).is_ok_and(|held| held.len() > 0),
            "the {theme} capture is written: {}",
            path.display()
        );
    }
}
