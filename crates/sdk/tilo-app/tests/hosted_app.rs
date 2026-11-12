//! An app the session's document host serves: its views show the readings the
//! session publishes, and its edits are submitted over the app's one
//! connection, attributed to the person's gesture
//! (`docs/architecture/apps.md` §8 step 2, `one-app-model.md` §4).
//!
//! The tests here run the app's own framework root over a stand-in host that
//! answers in process — what the session's document host is — so what they
//! prove is the app's whole view side: the tree the session's state draws,
//! and the action its controls submit.

use std::cell::RefCell;
use std::rc::Rc;

use tilo_app::framework::{Answers, Ask};
use tilo_app::prelude::*;

/// One channel's page, as an app of the session's document host holds it.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Channel {
    pub name: String,
    pub videos: Vec<String>,
}

#[resource(
    kind = "test.hosted-channel",
    label = "Channel",
    app = "so.tilo.test-hosted"
)]
impl Channel {
    /// Subscribe to the channel.
    #[action]
    pub fn subscribe(&mut self, subs: String) {
        if !self.videos.contains(&subs) {
            self.videos.push(subs);
        }
    }
}

/// The host that keeps the channel: what it publishes, and every action it
/// admitted with who asked for it.
#[derive(Default)]
struct ChannelHost {
    channel: RefCell<Channel>,
    /// Every action the door took, with whether it was the person's gesture.
    asked: RefCell<Vec<(String, bool)>>,
}

impl ChannelHost {
    fn holding(name: &str, videos: &[&str]) -> Rc<Self> {
        let host = Rc::new(Self::default());
        *host.channel.borrow_mut() = Channel {
            name: name.to_owned(),
            videos: videos.iter().map(|video| (*video).to_owned()).collect(),
        };
        host
    }

    /// The reading the host publishes: the channel as its model's JSON.
    fn published(&self) -> serde_json::Value {
        serde_json::json!({
            "name": self.channel.borrow().name,
            "videos": self.channel.borrow().videos,
        })
    }
}

/// The host behind a connection the test also reads.
#[derive(Clone)]
struct HostAnswers(Rc<ChannelHost>);

impl Answers for HostAnswers {
    fn reading(&self, resource: &str) -> Option<serde_json::Value> {
        (resource == "test.hosted-channel").then(|| self.0.published())
    }

    fn submit(&self, ask: &Ask) -> Result<(), String> {
        if ask.address.resource != "test.hosted-channel" || ask.address.action != "subscribe" {
            return Err("no such action is offered".to_owned());
        }
        let Some(subs) = ask.arguments.get("subs").and_then(|subs| subs.as_str()) else {
            return Err("the action needs what to subscribe".to_owned());
        };
        self.0.channel.borrow_mut().videos.push(subs.to_owned());
        self.0
            .asked
            .borrow_mut()
            .push((ask.address.action.clone(), ask.in_person));
        Ok(())
    }
}

struct HostedChannel;

impl ResourceApp for HostedChannel {
    type Resource = Channel;
    const ID: &'static str = "so.tilo.test-hosted";
    const TITLE: &'static str = "Channel";
    const HOSTED_BY_SESSION: bool = true;

    fn view(cx: &mut ViewCx<Channel>) -> Element {
        let name = cx.text(|channel: &Channel| channel.name.clone());
        Box::new(Column::new().push(Box::new(name)).push(Box::new(
            cx.button("Subscribe", Channel::SUBSCRIBE.with("subs".to_owned())),
        )))
    }
}

#[test]
fn the_window_shows_the_records_the_session_serves() {
    let host = ChannelHost::holding("Marques Brownlee", &["The M4 review"]);
    let mut app =
        TestApp::<HostedChannel>::hosted("test.hosted-channel", HostAnswers(Rc::clone(&host)))
            .expect("the app opens over the session's host");
    app.frame();
    let texts = app.texts();
    assert!(
        texts.iter().any(|text| text == "Marques Brownlee"),
        "the window shows the session's reading: {texts:?}"
    );
}

#[test]
fn an_edit_in_the_view_commits_through_the_session_as_the_person() {
    let host = ChannelHost::holding("Marques Brownlee", &[]);
    let mut app =
        TestApp::<HostedChannel>::hosted("test.hosted-channel", HostAnswers(Rc::clone(&host)))
            .expect("the app opens over the session's host");
    app.frame();
    // What a control's press hands the framework: the view's dispatcher
    // submits the typed action, and over a connection the session's door
    // takes it as the person's gesture.
    app.submit(Channel::SUBSCRIBE.with("subs".to_owned()))
        .expect("the view's dispatcher takes the edit");
    assert_eq!(
        host.asked.borrow().as_slice(),
        &[("subscribe".to_owned(), true)],
        "the session's door took the edit as the person's gesture"
    );
    assert_eq!(
        host.channel.borrow().videos,
        ["subs"],
        "the session committed the edit to the record it keeps"
    );
}

#[test]
fn a_press_on_a_control_commits_through_the_session() {
    let host = ChannelHost::holding("Marques Brownlee", &[]);
    let mut app =
        TestApp::<HostedChannel>::hosted("test.hosted-channel", HostAnswers(Rc::clone(&host)))
            .expect("the app opens over the session's host");
    app.frame();
    // A real press: the pointer moves onto the Subscribe control, presses and
    // releases, the widget emits its invocation and the root drains its
    // queue. Nothing here calls the dispatcher directly.
    app.harness()
        .click_named("subscribe")
        .expect("the Subscribe control is on screen");
    assert_eq!(
        host.asked.borrow().as_slice(),
        &[("subscribe".to_owned(), true)],
        "the press reached the session's door as the person's gesture"
    );
    assert_eq!(
        host.channel.borrow().videos,
        ["subs"],
        "the session committed the edit to the record it keeps"
    );
}

#[test]
fn the_app_process_opens_no_document() {
    // The app over a connection holds no record: everything it shows and
    // everything it changes is the session's, read and written over its one
    // connection.
    let host = ChannelHost::holding("Session's channel", &[]);
    let mut app =
        TestApp::<HostedChannel>::hosted("test.hosted-channel", HostAnswers(Rc::clone(&host)))
            .expect("the app opens over the session's host");
    assert!(
        app.open_records().is_empty(),
        "an app the session serves opens no record of its own: {:?}",
        app.open_records()
    );
    app.frame();
}

#[test]
fn only_a_launch_the_session_hosts_stays_out_of_its_workspace() {
    // The session names the record it keeps in the launch: that process opens none and joins no
    // workspace. A launch it does not host keeps the record itself and joins its workspace, so the
    // workspace's people and agents reach it as any app's.
    let spec = tilo_app::framework::launch_spec::<HostedChannel>();
    let args = tilo_app::launch::LaunchArgs::component();
    let mut env = tilo_test_env::ScopedEnv::new();
    env.remove(ui_core::application::HOSTED_RESOURCE_ENV);
    assert!(spec.attaches_runtime(&args));
    env.set(
        ui_core::application::HOSTED_RESOURCE_ENV,
        "test.hosted-channel",
    );
    assert!(!spec.attaches_runtime(&args));
}
