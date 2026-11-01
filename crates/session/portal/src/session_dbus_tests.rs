//! The session-based interfaces on a session bus of their own, called as
//! the portal frontend calls them, with a session that captures and takes
//! input standing in for the compositor.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::StreamExt as _;
use serde_json::Map;
use tilo_lock::MutexRecover as _;
use tilo_service::{Answer, Questions, Service as _};
use zbus::zvariant::{ObjectPath, OwnedValue};

use crate::Published as _;
use crate::auth::FRONTEND_BUS_NAME;
use crate::dbus_tests::{SessionBus, asked};
use crate::remote_desktop::{InputInjector, RemoteInput, device_type};
use crate::streams::{
    CaptureSource, CaptureStreams, SourceKind, Stream, StreamRequest, cursor_mode,
};
use crate::{
    BUS_NAME, OBJECT_PATH, Pending, Portal, PortalAuthenticator, PortalCaller, question_handle,
};

/// Shares one output and one window, numbering its nodes.
#[derive(Default)]
struct Streams {
    started: Mutex<Vec<u32>>,
    stopped: Mutex<Vec<u32>>,
}

impl CaptureStreams for Streams {
    fn sources(&self) -> Vec<CaptureSource> {
        vec![
            CaptureSource {
                kind: SourceKind::Output,
                id: "DP-1".into(),
                app_id: None,
                label: "Desk".into(),
            },
            CaptureSource {
                kind: SourceKind::Window,
                id: "w1".into(),
                app_id: Some("org.example.Notes".into()),
                label: "Notes".into(),
            },
        ]
    }

    fn cursor_modes(&self) -> u32 {
        cursor_mode::HIDDEN | cursor_mode::EMBEDDED
    }

    fn start(&self, _request: StreamRequest) -> Pending<Result<Stream, String>> {
        let mut started = self.started.lock_recover();
        let node = u32::try_from(started.len()).map_or(40, |count| count.saturating_add(40));
        started.push(node);
        drop(started);
        Box::pin(async move {
            Ok(Stream {
                node_id: node,
                size: (1280, 720),
                position: Some((0, 0)),
            })
        })
    }

    fn stop(&self, node_id: u32) {
        self.stopped.lock_recover().push(node_id);
    }
}

#[derive(Default)]
struct Seat {
    taken: Mutex<Vec<RemoteInput>>,
}

impl InputInjector for Seat {
    fn available(&self) -> bool {
        true
    }

    fn inject(&self, input: RemoteInput) -> bool {
        self.taken.lock_recover().push(input);
        true
    }
}

struct Bed {
    _bus: SessionBus,
    questions: Questions,
    portal: Portal,
    streams: Arc<Streams>,
    seat: Arc<Seat>,
    frontend: zbus::Connection,
    stranger: zbus::Connection,
    _backend: zbus::Connection,
    _grants: tempfile::TempDir,
}

async fn bed() -> Bed {
    let bus = SessionBus::start();
    let backend = bus.connect().await;
    let questions = Questions::new();
    let streams = Arc::new(Streams::default());
    let seat = Arc::new(Seat::default());
    let grants = tempfile::tempdir().expect("directory");
    let authenticator = PortalAuthenticator::session_bus(
        backend.clone(),
        Arc::new(|app: &str, _: Option<&str>| Some(PortalCaller::new(app))),
    );
    let portal = Portal::new(
        questions.clone(),
        authenticator,
        tilo_documents::Documents::new(),
    )
    .with_consents(crate::consent::Consents::new(questions.clone(), None))
    .with_grants(crate::restore::Grants::load(Some(
        grants.path().join("restore.json"),
    )))
    .with_streams(Arc::clone(&streams) as Arc<dyn CaptureStreams>)
    .with_input(Arc::clone(&seat) as Arc<dyn InputInjector>);
    portal.serve(&backend).await.expect("served");
    let frontend = bus.connect().await;
    frontend
        .request_name(FRONTEND_BUS_NAME)
        .await
        .expect("the frontend's name");
    let stranger = bus.connect().await;
    Bed {
        _bus: bus,
        questions,
        portal,
        streams,
        seat,
        frontend,
        stranger,
        _backend: backend,
        _grants: grants,
    }
}

type Reply = (u32, HashMap<String, OwnedValue>);
type Options = HashMap<String, OwnedValue>;

const APP: &str = "org.example.Share";

fn request(name: &str) -> String {
    format!(
        "/org/freedesktop/portal/desktop/request/1_9/{}",
        name.replace('-', "_")
    )
}

fn session(name: &str) -> String {
    format!("/org/freedesktop/portal/desktop/session/1_9/{name}")
}

fn path(value: &str) -> ObjectPath<'_> {
    ObjectPath::try_from(value).expect("object path")
}

async fn call<B>(
    from: &zbus::Connection,
    interface: &str,
    method: &str,
    body: &B,
) -> zbus::Result<Reply>
where
    B: serde::Serialize + zbus::zvariant::DynamicType + Sync,
{
    from.call_method(Some(BUS_NAME), OBJECT_PATH, Some(interface), method, body)
        .await?
        .body()
        .deserialize()
}

const SCREENCAST: &str = "org.freedesktop.impl.portal.ScreenCast";
const REMOTE: &str = "org.freedesktop.impl.portal.RemoteDesktop";

async fn create(bed: &Bed, interface: &str, name: &str) {
    let (code, _) = call(
        &bed.frontend,
        interface,
        "CreateSession",
        &(
            path(&request(&format!("{name}-create"))),
            path(&session(name)),
            APP,
            Options::new(),
        ),
    )
    .await
    .expect("replied");
    assert_eq!(code, 0, "{name} created");
}

async fn select_sources(bed: &Bed, name: &str, options: Options) -> u32 {
    call(
        &bed.frontend,
        SCREENCAST,
        "SelectSources",
        &(
            path(&request(&format!("{name}-select"))),
            path(&session(name)),
            APP,
            options,
        ),
    )
    .await
    .expect("replied")
    .0
}

/// Start `interface`'s session `name`, answering its question with
/// `answer`; `None` when nothing is asked.
async fn start(bed: &Bed, interface: &str, name: &str, answer: Option<Answer>) -> Reply {
    let handle = request(&format!("{name}-start"));
    let question = question_handle(&handle);
    let target = session(name);
    let body = (path(&handle), path(&target), APP, "", Options::new());
    let starting = call(&bed.frontend, interface, "Start", &body);
    let answering = async {
        if let Some(answer) = answer {
            asked(&bed.questions, &question).await;
            assert!(bed.questions.answer(&question, answer));
        }
    };
    let (reply, ()) = tokio::join!(starting, answering);
    reply.expect("replied")
}

fn picking(uris: &[&str], remember: bool) -> Answer {
    Answer {
        uris: uris.iter().map(|uri| (*uri).to_owned()).collect(),
        remember,
        ..Answer::default()
    }
}

fn nodes(results: &HashMap<String, OwnedValue>) -> Vec<u32> {
    let streams: Vec<(u32, HashMap<String, OwnedValue>)> = results["streams"]
        .try_clone()
        .expect("clone")
        .try_into()
        .expect("a(ua{sv})");
    streams.into_iter().map(|(node, _)| node).collect()
}

async fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    for _ in 0..500 {
        if done() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("{what} never happened");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_shared_screen_is_published_until_the_person_stops_it() {
    let mut bed = bed().await;
    create(&bed, SCREENCAST, "cast").await;
    assert_eq!(select_sources(&bed, "cast", Options::new()).await, 0);
    let offered = async {
        let handle = question_handle(&request("cast-start"));
        asked(&bed.questions, &handle).await;
        let question = bed.questions.question(&handle).expect("asked");
        assert_eq!(question.kind, crate::screencast::SCREENCAST_QUESTION);
        let sources = question.details["sources"]
            .as_array()
            .expect("sources")
            .len();
        assert_eq!(sources, 1, "only the monitors the app asked for");
    };
    let (reply, ()) = tokio::join!(
        start(
            &bed,
            SCREENCAST,
            "cast",
            Some(picking(&["output:DP-1"], false))
        ),
        offered
    );
    let (code, results) = reply;
    assert_eq!(code, 0);
    assert_eq!(nodes(&results), [40]);
    assert!(!results.contains_key("restore_data"), "nothing to remember");

    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface("org.freedesktop.impl.portal.Session")
        .expect("interface")
        .member("Closed")
        .expect("member")
        .build();
    let mut closed = zbus::MessageStream::for_match_rule(rule, &bed.frontend, None)
        .await
        .expect("listening");
    let published = bed.portal.resources();
    let capture = published[0]
        .children
        .iter()
        .find(|child| child.kind == "portal.capture")
        .expect("the share is published")
        .id
        .as_str()
        .to_owned();
    assert!(bed.portal.read(&capture).is_some());
    bed.portal
        .invoke(&capture, "stop", &Map::new())
        .expect("stopped");
    let signal = tokio::time::timeout(Duration::from_secs(5), closed.next())
        .await
        .expect("the app hears it")
        .expect("a message")
        .expect("a signal");
    assert_eq!(
        signal.header().path().map(|p| p.as_str().to_owned()),
        Some(session("cast"))
    );
    assert!(bed.streams.stopped.lock_recover().contains(&40));
    assert!(bed.portal.poll(), "the published state changed");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_remembered_share_starts_again_without_asking() {
    let bed = bed().await;
    create(&bed, SCREENCAST, "first").await;
    let mut options = Options::new();
    options.insert("types".into(), OwnedValue::from(3_u32));
    options.insert("persist_mode".into(), OwnedValue::from(2_u32));
    assert_eq!(select_sources(&bed, "first", options).await, 0);
    let (code, results) = start(
        &bed,
        SCREENCAST,
        "first",
        Some(picking(&["window:w1"], true)),
    )
    .await;
    assert_eq!(code, 0);
    let persist: u32 = results["persist_mode"]
        .try_clone()
        .expect("clone")
        .try_into()
        .expect("u");
    assert_eq!(persist, 2);
    let restore = results["restore_data"].try_clone().expect("clone");

    create(&bed, SCREENCAST, "again").await;
    let mut options = Options::new();
    options.insert("types".into(), OwnedValue::from(3_u32));
    options.insert("restore_data".into(), restore);
    assert_eq!(select_sources(&bed, "again", options).await, 0);
    let (code, results) = start(&bed, SCREENCAST, "again", None).await;
    assert_eq!(code, 0, "no question asked");
    assert_eq!(nodes(&results).len(), 1);
    assert_eq!(bed.portal.grants().resources().len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_remote_desktop_drives_only_while_its_session_lives() {
    let bed = bed().await;
    create(&bed, REMOTE, "remote").await;
    let mut devices = Options::new();
    devices.insert("types".into(), OwnedValue::from(device_type::POINTER));
    let (code, _) = call(
        &bed.frontend,
        REMOTE,
        "SelectDevices",
        &(
            path(&request("remote-devices")),
            path(&session("remote")),
            APP,
            devices,
        ),
    )
    .await
    .expect("replied");
    assert_eq!(code, 0);
    let notify = |from: zbus::Connection| async move {
        from.call_method(
            Some(BUS_NAME),
            OBJECT_PATH,
            Some(REMOTE),
            "NotifyPointerMotion",
            &(path(&session("remote")), Options::new(), 1.0_f64, 2.0_f64),
        )
        .await
        .expect("sent");
    };
    notify(bed.frontend.clone()).await;
    assert!(bed.seat.taken.lock_recover().is_empty(), "not started yet");

    let (code, results) = start(&bed, REMOTE, "remote", Some(Answer::default())).await;
    assert_eq!(code, 0);
    let granted: u32 = results["devices"]
        .try_clone()
        .expect("clone")
        .try_into()
        .expect("u");
    assert_eq!(granted, device_type::POINTER);
    notify(bed.frontend.clone()).await;
    notify(bed.stranger.clone()).await;
    let keyboard = bed
        .frontend
        .call_method(
            Some(BUS_NAME),
            OBJECT_PATH,
            Some(REMOTE),
            "NotifyKeyboardKeycode",
            &(path(&session("remote")), Options::new(), 30_i32, 1_u32),
        )
        .await;
    assert!(keyboard.is_ok());
    assert_eq!(
        *bed.seat.taken.lock_recover(),
        [RemoteInput::PointerMotion { dx: 1.0, dy: 2.0 }],
        "only the frontend's pointer"
    );
    assert!(
        bed.portal.resources()[0]
            .children
            .iter()
            .any(|child| child.kind == "portal.remote-control"),
        "the person sees who drives the machine"
    );
    bed.frontend
        .call_method(
            Some(BUS_NAME),
            session("remote").as_str(),
            Some("org.freedesktop.impl.portal.Session"),
            "Close",
            &(),
        )
        .await
        .expect("closed");
    notify(bed.frontend.clone()).await;
    assert_eq!(
        bed.seat.taken.lock_recover().len(),
        1,
        "nothing after the close"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bound_shortcut_reaches_its_app_when_its_action_runs() {
    let mut bed = bed().await;
    create(&bed, "org.freedesktop.impl.portal.GlobalShortcuts", "keys").await;
    let mut talk = Options::new();
    talk.insert(
        "preferred_trigger".into(),
        OwnedValue::from(zbus::zvariant::Str::from("Ctrl+Alt+P")),
    );
    let shortcuts = vec![("talk".to_owned(), talk)];
    let handle = request("keys-bind");
    let target = session("keys");
    let body = (path(&handle), path(&target), shortcuts, "", Options::new());
    let binding = call(
        &bed.frontend,
        "org.freedesktop.impl.portal.GlobalShortcuts",
        "BindShortcuts",
        &body,
    );
    let answering = async {
        let question = question_handle(&handle);
        asked(&bed.questions, &question).await;
        assert!(bed.questions.answer(
            &question,
            Answer {
                choice: Some(crate::global_shortcuts::BIND_AS_PROPOSED),
                ..Answer::default()
            }
        ));
    };
    let (reply, ()) = tokio::join!(binding, answering);
    let (code, results) = reply.expect("replied");
    assert_eq!(code, 0);
    let bound: Vec<(String, HashMap<String, OwnedValue>)> = results["shortcuts"]
        .try_clone()
        .expect("clone")
        .try_into()
        .expect("a(sa{sv})");
    assert_eq!(bound[0].0, "talk");

    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface("org.freedesktop.impl.portal.GlobalShortcuts")
        .expect("interface")
        .member("Activated")
        .expect("member")
        .build();
    let mut activated = zbus::MessageStream::for_match_rule(rule, &bed.frontend, None)
        .await
        .expect("listening");
    let shortcut = bed.portal.resources()[0]
        .children
        .iter()
        .find(|child| child.kind == "portal.shortcut")
        .expect("published")
        .id
        .as_str()
        .to_owned();
    let reading = bed.portal.read(&shortcut).expect("read");
    assert_eq!(reading["trigger"]["canonical"], "CTRL+ALT+KEYP");
    bed.portal
        .invoke(&shortcut, "activate", &Map::new())
        .expect("pressed");
    let signal = tokio::time::timeout(Duration::from_secs(5), activated.next())
        .await
        .expect("the app hears it")
        .expect("a message")
        .expect("a signal");
    let (_, id, _, _): (ObjectPath<'_>, String, u64, Options) =
        signal.body().deserialize().expect("Activated");
    assert_eq!(id, "talk");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_input_capture_starts_on_the_persons_access_and_lists_the_screens() {
    let bed = bed().await;
    bed.portal
        .input_captures()
        .set_zones(vec![crate::input_capture::CaptureZone {
            width: 1920,
            height: 1080,
            x: 0,
            y: 0,
        }]);
    let created: Options = bed
        .frontend
        .call_method(
            Some(BUS_NAME),
            OBJECT_PATH,
            Some("org.freedesktop.impl.portal.InputCapture"),
            "CreateSession2",
            &(path(&session("kvm")), APP, Options::new()),
        )
        .await
        .expect("replied")
        .body()
        .deserialize()
        .expect("results");
    assert!(created.contains_key("capabilities"));
    let mut options = Options::new();
    options.insert("capabilities".into(), OwnedValue::from(3_u32));
    let handle = request("kvm-start");
    let target = session("kvm");
    let body = (path(&handle), path(&target), APP, "", options);
    let starting = call(
        &bed.frontend,
        "org.freedesktop.impl.portal.InputCapture",
        "Start",
        &body,
    );
    let answering = async {
        let question = question_handle(&handle);
        asked(&bed.questions, &question).await;
        let asked_for = bed.questions.question(&question).expect("asked");
        assert_eq!(asked_for.kind, crate::consent::ACCESS_QUESTION);
        assert!(bed.questions.answer(&question, Answer::default()));
    };
    let (reply, ()) = tokio::join!(starting, answering);
    assert_eq!(reply.expect("replied").0, 0);
    let (code, zones) = call(
        &bed.frontend,
        "org.freedesktop.impl.portal.InputCapture",
        "GetZones",
        &(
            path(&request("kvm-zones")),
            path(&session("kvm")),
            APP,
            Options::new(),
        ),
    )
    .await
    .expect("replied");
    assert_eq!(code, 0);
    let zones: Vec<(u32, u32, i32, i32)> = zones["zones"]
        .try_clone()
        .expect("clone")
        .try_into()
        .expect("a(uuii)");
    assert_eq!(zones, [(1920, 1080, 0, 0)]);
    wait_until("the capture is published", || {
        bed.portal.resources().first().is_some_and(|portal| {
            portal
                .children
                .iter()
                .any(|child| child.kind == "portal.remote-control")
        })
    })
    .await;
}
