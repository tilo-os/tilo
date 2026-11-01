//! The backend on a session bus of its own, called as the portal frontend
//! calls it, and by connections that are not the frontend.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tilo_service::{Answer, Questions};
use tilo_test_env::PrivateBus;
use zbus::zvariant::{ObjectPath, OwnedValue};

use crate::auth::FRONTEND_BUS_NAME;
use crate::{BUS_NAME, OBJECT_PATH, Portal, PortalAuthenticator, PortalCaller, question_handle};

/// A session bus of its own, stopped on drop, which the beds here connect
/// to as an app, the frontend or a stranger.
pub(crate) struct SessionBus(PrivateBus);

impl SessionBus {
    pub(crate) fn start() -> Self {
        Self(PrivateBus::start())
    }

    pub(crate) async fn connect(&self) -> zbus::Connection {
        zbus::connection::Builder::address(self.0.address())
            .expect("address")
            .build()
            .await
            .expect("connected")
    }
}

/// The backend serving on its own connection, a frontend owning its name
/// and a stranger on the same bus.
struct Bed {
    _bus: SessionBus,
    /// The person's private state the backend keeps, gone with the bed.
    _state: tempfile::TempDir,
    questions: Questions,
    portal: Portal,
    frontend: zbus::Connection,
    stranger: zbus::Connection,
    backend: zbus::Connection,
}

async fn bed() -> Bed {
    bed_with(Arc::new(crate::NoCapture)).await
}

/// [`bed`], with `capture` standing in for the session's screenshots and
/// picks.
async fn bed_with(capture: Arc<dyn crate::Capture>) -> Bed {
    let bus = SessionBus::start();
    let backend = bus.connect().await;
    let questions = Questions::new();
    // This process is every connection's; the test names it as the
    // requester resolver names an app started outside the session.
    let own = std::process::id();
    let authenticator = PortalAuthenticator::session_bus(
        backend.clone(),
        Arc::new(|app: &str, _: Option<&str>| Some(PortalCaller::new(app))),
    )
    .with_identify(Arc::new(move |pid| {
        (pid == own).then(|| PortalCaller::new("/usr/bin/unsandboxed"))
    }));
    let state = tempfile::tempdir().expect("a private state directory");
    let store =
        crate::permission_store::PermissionStore::load(Some(state.path().join("permissions.json")));
    let portal = Portal::new(
        questions.clone(),
        authenticator,
        tilo_documents::Documents::new(),
    )
    .with_capture(capture)
    .with_consents(crate::consent::Consents::new(questions.clone(), None))
    .with_permissions(store);
    portal.serve(&backend).await.expect("served");
    let frontend = bus.connect().await;
    frontend
        .request_name(FRONTEND_BUS_NAME)
        .await
        .expect("the frontend's name");
    let stranger = bus.connect().await;
    Bed {
        _bus: bus,
        _state: state,
        questions,
        portal,
        frontend,
        stranger,
        backend,
    }
}

type Reply = (u32, HashMap<String, OwnedValue>);

async fn open_file(from: &zbus::Connection, request: &str) -> zbus::Result<Reply> {
    from.call_method(
        Some(BUS_NAME),
        OBJECT_PATH,
        Some("org.freedesktop.impl.portal.FileChooser"),
        "OpenFile",
        &(
            ObjectPath::try_from(request).expect("request path"),
            "org.example.Editor",
            "",
            "Open a note",
            HashMap::<String, OwnedValue>::new(),
        ),
    )
    .await?
    .body()
    .deserialize()
}

async fn close(from: &zbus::Connection, request: &str) -> zbus::Result<()> {
    from.call_method(
        Some(BUS_NAME),
        request,
        Some("org.freedesktop.impl.portal.Request"),
        "Close",
        &(),
    )
    .await
    .map(drop)
}

/// Wait until question `handle` is asked.
pub(crate) async fn asked(questions: &Questions, handle: &str) {
    for _ in 0..500 {
        if questions.question(handle).is_some() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("{handle} was never asked");
}

const REQUEST: &str = "/org/freedesktop/portal/desktop/request/1_9/t1";

#[tokio::test(flavor = "multi_thread")]
async fn a_file_chosen_in_the_sheet_reaches_the_app() {
    let bed = bed().await;
    let file = tempfile::NamedTempFile::new().expect("a file to choose");
    let uri = url::Url::from_file_path(file.path())
        .expect("absolute")
        .to_string();
    let handle = question_handle(REQUEST);
    let answering = async {
        asked(&bed.questions, &handle).await;
        let question = bed.questions.question(&handle).expect("asked");
        assert_eq!(question.kind, crate::file_chooser::FILE_CHOOSER_QUESTION);
        assert_eq!(question.app_id.as_deref(), Some("org.example.Editor"));
        assert!(bed.questions.answer(
            &handle,
            Answer {
                uris: vec![uri.clone()],
                ..Answer::default()
            }
        ));
    };
    let (reply, ()) = tokio::join!(open_file(&bed.frontend, REQUEST), answering);
    let (response, results) = reply.expect("replied");
    assert_eq!(response, 0);
    let uris: Vec<String> = results["uris"].clone().try_into().expect("uris");
    assert_eq!(uris, [uri]);
}

#[tokio::test(flavor = "multi_thread")]
async fn only_the_frontend_may_ask() {
    let bed = bed().await;
    let (response, _) = open_file(&bed.stranger, REQUEST).await.expect("replied");
    assert_eq!(response, 2, "a stranger's request fails");
    assert!(bed.questions.pending().is_empty(), "and asks nobody");

    let read = bed
        .stranger
        .call_method(
            Some(BUS_NAME),
            OBJECT_PATH,
            Some("org.freedesktop.impl.portal.Settings"),
            "Read",
            &("org.freedesktop.appearance", "color-scheme"),
        )
        .await;
    assert!(read.is_err(), "nor read the settings");
    let read = bed
        .frontend
        .call_method(
            Some(BUS_NAME),
            OBJECT_PATH,
            Some("org.freedesktop.impl.portal.Settings"),
            "Read",
            &("org.freedesktop.appearance", "color-scheme"),
        )
        .await;
    assert!(read.is_ok(), "the frontend reads them");
}

/// A session that picks the colour it was made with, taking no picture.
struct Palette(f64, f64, f64);

impl crate::Capture for Palette {
    fn screenshot(
        &self,
        _target: crate::CaptureTarget,
    ) -> crate::Pending<Result<std::path::PathBuf, crate::CaptureError>> {
        Box::pin(async {
            Err(crate::CaptureError::Failed(
                "this session takes no picture".to_owned(),
            ))
        })
    }

    fn color_at_pointer(&self) -> crate::Pending<Result<(f64, f64, f64), crate::CaptureError>> {
        let colour = (self.0, self.1, self.2);
        Box::pin(async move { Ok(colour) })
    }

    fn targets(&self) -> u32 {
        0
    }
}

async fn pick_color(from: &zbus::Connection, request: &str) -> zbus::Result<Reply> {
    from.call_method(
        Some(BUS_NAME),
        OBJECT_PATH,
        Some("org.freedesktop.impl.portal.Screenshot"),
        "PickColor",
        &(
            ObjectPath::try_from(request).expect("request path"),
            "org.example.Colour",
            "",
            HashMap::<String, OwnedValue>::new(),
        ),
    )
    .await?
    .body()
    .deserialize()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_colour_pick_is_answered_with_the_pixel_the_session_reads() {
    let bed = bed_with(Arc::new(Palette(0.25, 0.5, 1.0))).await;
    let (code, results) = pick_color(&bed.frontend, REQUEST).await.expect("replied");
    assert_eq!(code, 0);
    let (red, green, blue): (f64, f64, f64) = results["color"]
        .try_clone()
        .expect("clone")
        .try_into()
        .expect("three channels");
    assert_eq!((red, green, blue), (0.25, 0.5, 1.0));
    assert!(bed.questions.pending().is_empty(), "nobody was asked");
}

#[tokio::test(flavor = "multi_thread")]
async fn only_the_frontend_may_pick_a_colour() {
    let bed = bed_with(Arc::new(Palette(1.0, 1.0, 1.0))).await;
    let (code, results) = pick_color(&bed.stranger, REQUEST).await.expect("replied");
    assert_eq!(code, 2, "a stranger's request fails");
    assert!(results.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn closing_the_request_withdraws_the_question() {
    let bed = bed().await;
    let handle = question_handle(REQUEST);
    let closing = async {
        asked(&bed.questions, &handle).await;
        assert!(
            close(&bed.stranger, REQUEST).await.is_err(),
            "only the frontend that made it closes it"
        );
        assert!(bed.questions.question(&handle).is_some());
        close(&bed.frontend, REQUEST).await.expect("closed");
    };
    let (reply, ()) = tokio::join!(open_file(&bed.frontend, REQUEST), closing);
    assert_eq!(reply.expect("replied").0, 1, "cancelled");
    assert!(bed.questions.question(&handle).is_none(), "withdrawn");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_inhibition_holds_until_its_request_is_closed() {
    let bed = bed().await;
    let request = "/org/freedesktop/portal/desktop/request/1_9/hold";
    let mut options = HashMap::<String, OwnedValue>::new();
    options.insert(
        "reason".into(),
        OwnedValue::from(zbus::zvariant::Str::from("Playing a film")),
    );
    bed.frontend
        .call_method(
            Some(BUS_NAME),
            OBJECT_PATH,
            Some("org.freedesktop.impl.portal.Inhibit"),
            "Inhibit",
            &(
                ObjectPath::try_from(request).expect("request path"),
                "org.example.Player",
                "",
                crate::inhibit::IDLE,
                options,
            ),
        )
        .await
        .expect("held");
    assert_eq!(
        bed.portal.inhibitions().held(),
        [(
            request.to_owned(),
            "org.example.Player".to_owned(),
            crate::inhibit::IDLE
        )]
    );
    close(&bed.frontend, request).await.expect("closed");
    assert!(bed.portal.inhibitions().held().is_empty(), "lifted");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_departed_frontend_takes_its_requests_along() {
    let bed = bed().await;
    let handle = question_handle(REQUEST);
    let frontend = bed.frontend.clone();
    let call = tokio::spawn(async move { open_file(&frontend, REQUEST).await });
    asked(&bed.questions, &handle).await;
    call.abort();
    drop(bed.frontend);
    for _ in 0..500 {
        if bed.questions.question(&handle).is_none() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("the question outlived its frontend");
}

/// The request path the frontend makes for a call by `requester`.
fn request_of(requester: &zbus::Connection, token: &str) -> String {
    let name = requester.unique_name().expect("unique name");
    format!(
        "/org/freedesktop/portal/desktop/request/{}/{token}",
        name.trim_start_matches(':').replace('.', "_")
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn an_app_the_frontend_could_not_name_is_named_by_its_process() {
    let bed = bed().await;
    let request = request_of(&bed.stranger, "t1");
    let handle = question_handle(&request);
    let arguments = (
        ObjectPath::try_from(request.as_str()).expect("request path"),
        "",
        "",
        "Open",
        HashMap::<String, OwnedValue>::new(),
    );
    let call = bed.frontend.call_method(
        Some(BUS_NAME),
        OBJECT_PATH,
        Some("org.freedesktop.impl.portal.FileChooser"),
        "OpenFile",
        &arguments,
    );
    let answering = async {
        asked(&bed.questions, &handle).await;
        let question = bed.questions.question(&handle).expect("asked");
        assert_eq!(question.app_id.as_deref(), Some("/usr/bin/unsandboxed"));
        assert!(bed.questions.decline(&handle));
    };
    let (reply, ()) = tokio::join!(call, answering);
    let (response, _): Reply = reply.expect("replied").body().deserialize().expect("reply");
    assert_eq!(response, 1, "asked, then declined: not refused unasked");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_app_chosen_is_one_offered_when_the_person_answered() {
    let bed = bed().await;
    let request = "/org/freedesktop/portal/desktop/request/1_9/open";
    let handle = question_handle(request);
    let arguments = (
        ObjectPath::try_from(request).expect("request path"),
        "org.example.Editor",
        "",
        vec!["org.example.Viewer"],
        HashMap::<String, OwnedValue>::new(),
    );
    let choose = bed.frontend.call_method(
        Some(BUS_NAME),
        OBJECT_PATH,
        Some("org.freedesktop.impl.portal.AppChooser"),
        "ChooseApplication",
        &arguments,
    );
    let answering = async {
        asked(&bed.questions, &handle).await;
        let update = |from: zbus::Connection| async move {
            from.call_method(
                Some(BUS_NAME),
                OBJECT_PATH,
                Some("org.freedesktop.impl.portal.AppChooser"),
                "UpdateChoices",
                &(
                    ObjectPath::try_from(request).expect("request path"),
                    vec!["org.example.Viewer", "org.example.Painter"],
                ),
            )
            .await
        };
        assert!(
            update(bed.stranger.clone()).await.is_err(),
            "only its frontend"
        );
        update(bed.frontend.clone()).await.expect("updated");
        let question = bed.questions.question(&handle).expect("still asked");
        let offered: crate::app_chooser::AppChooserQuestion =
            serde_json::from_value(question.details).expect("details");
        assert_eq!(offered.choices.len(), 2);
        assert!(bed.questions.answer(
            &handle,
            Answer {
                choice: Some(1),
                ..Answer::default()
            }
        ));
    };
    let (reply, ()) = tokio::join!(choose, answering);
    let (response, results): Reply = reply.expect("replied").body().deserialize().expect("reply");
    assert_eq!(response, 0);
    let choice: String = results["choice"].clone().try_into().expect("choice");
    assert_eq!(choice, "org.example.Painter");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_screensaver_hold_is_published_and_ends_with_its_client() {
    let bed = bed().await;
    let player = bed.stranger.clone();
    let cookie: u32 = player
        .call_method(
            Some(crate::screensaver::BUS_NAME),
            "/org/freedesktop/ScreenSaver",
            Some("org.freedesktop.ScreenSaver"),
            "Inhibit",
            &("Film player", "Playing a film"),
        )
        .await
        .expect("held")
        .body()
        .deserialize()
        .expect("cookie");
    assert_ne!(cookie, 0);
    let held = bed.portal.inhibitions().held();
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].1, "Film player");
    let published = tilo_service::Service::resources(&bed.portal);
    assert_eq!(published[0].children.len(), 1, "the person sees the hold");
    drop(bed.stranger);
    drop(player);
    for _ in 0..500 {
        if bed.portal.inhibitions().held().is_empty() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("the hold outlived its client");
}

#[tokio::test(flavor = "multi_thread")]
async fn print_is_served_as_its_interface_declares() {
    let bed = bed().await;
    let print = zbus::Proxy::new(
        &bed.frontend,
        BUS_NAME,
        OBJECT_PATH,
        "org.freedesktop.impl.portal.Print",
    )
    .await
    .expect("proxy");
    let version: u32 = print.get_property("version").await.expect("version");
    assert_eq!(version, 3);
    let options = HashMap::<String, OwnedValue>::new();
    let arguments = (
        ObjectPath::try_from(REQUEST).expect("request path"),
        "org.example.Editor",
        "",
        "Letter",
        options.clone(),
        options.clone(),
        options,
    );
    let (response, _): Reply = bed
        .stranger
        .call_method(
            Some(BUS_NAME),
            OBJECT_PATH,
            Some("org.freedesktop.impl.portal.Print"),
            "PreparePrint",
            &arguments,
        )
        .await
        .expect("replied")
        .body()
        .deserialize()
        .expect("reply");
    assert_eq!(response, 2, "not the frontend: refused");
    let malformed = bed
        .frontend
        .call_method(
            Some(BUS_NAME),
            OBJECT_PATH,
            Some("org.freedesktop.impl.portal.Print"),
            "PreparePrint",
            &("not a request",),
        )
        .await;
    assert!(
        malformed.is_err(),
        "a body that does not decode is an error"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_monitor_hears_the_session_end_and_answers_it() {
    use futures_util::StreamExt as _;

    let bed = bed().await;
    let session = "/org/freedesktop/portal/desktop/session/1_9/watch";
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface("org.freedesktop.impl.portal.Inhibit")
        .expect("interface")
        .member("StateChanged")
        .expect("member")
        .build();
    let mut heard = zbus::MessageStream::for_match_rule(rule, &bed.frontend, None)
        .await
        .expect("stream");
    let mut next_state = async || -> (String, u32) {
        let message = tokio::time::timeout(Duration::from_secs(5), heard.next())
            .await
            .expect("heard in time")
            .expect("a signal")
            .expect("readable");
        let (path, state): (zbus::zvariant::OwnedObjectPath, HashMap<String, OwnedValue>) =
            message.body().deserialize().expect("state");
        let code: u32 = state["session-state"].clone().try_into().expect("state");
        (path.to_string(), code)
    };
    let arguments = (
        ObjectPath::try_from(REQUEST).expect("request path"),
        ObjectPath::try_from(session).expect("session path"),
        "org.example.Editor",
        "",
    );
    let created: u32 = bed
        .frontend
        .call_method(
            Some(BUS_NAME),
            OBJECT_PATH,
            Some("org.freedesktop.impl.portal.Inhibit"),
            "CreateMonitor",
            &arguments,
        )
        .await
        .expect("replied")
        .body()
        .deserialize()
        .expect("response");
    assert_eq!(created, 0);
    assert_eq!(next_state().await, (session.to_owned(), 1));
    bed.portal
        .set_session_state(&bed.backend, false, crate::SessionState::QueryEnd)
        .await;
    assert_eq!(next_state().await, (session.to_owned(), 2));
    assert_eq!(bed.portal.awaiting_query_end(), 1);
    let respond = |from: zbus::Connection| async move {
        from.call_method(
            Some(BUS_NAME),
            OBJECT_PATH,
            Some("org.freedesktop.impl.portal.Inhibit"),
            "QueryEndResponse",
            &(ObjectPath::try_from(session).expect("session path"),),
        )
        .await
    };
    assert!(
        respond(bed.stranger.clone()).await.is_err(),
        "not its frontend"
    );
    respond(bed.frontend.clone()).await.expect("answered");
    assert_eq!(bed.portal.awaiting_query_end(), 0);
}

const PERMISSION_STORE: &str = "org.freedesktop.impl.portal.PermissionStore";

#[tokio::test(flavor = "multi_thread")]
async fn only_the_frontend_may_keep_permissions() {
    use futures_util::StreamExt as _;

    let bed = bed().await;
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface(PERMISSION_STORE)
        .expect("interface")
        .member("Changed")
        .expect("member")
        .build();
    let mut heard = zbus::MessageStream::for_match_rule(rule, &bed.frontend, None)
        .await
        .expect("stream");

    let mut permissions = HashMap::new();
    permissions.insert("org.example.Editor".to_owned(), vec!["yes".to_owned()]);
    let arguments = (
        "documents",
        true,
        "one",
        permissions.clone(),
        OwnedValue::from(zbus::zvariant::Str::from("where")),
    );
    let set = |from: zbus::Connection| {
        let arguments = arguments.clone();
        async move {
            from.call_method(
                Some(BUS_NAME),
                OBJECT_PATH,
                Some(PERMISSION_STORE),
                "Set",
                &arguments,
            )
            .await
        }
    };
    let lookup = |from: zbus::Connection| async move {
        from.call_method(
            Some(BUS_NAME),
            OBJECT_PATH,
            Some(PERMISSION_STORE),
            "Lookup",
            &("documents", "one"),
        )
        .await
    };
    assert!(set(bed.stranger.clone()).await.is_err(), "not the frontend");
    assert!(
        lookup(bed.stranger.clone()).await.is_err(),
        "nor does a stranger read the store"
    );
    assert!(
        lookup(bed.frontend.clone()).await.is_err(),
        "the refused call kept nothing"
    );
    set(bed.frontend.clone())
        .await
        .expect("the frontend kept it");
    let (kept, data): (HashMap<String, Vec<String>>, OwnedValue) = lookup(bed.frontend.clone())
        .await
        .expect("kept")
        .body()
        .deserialize()
        .expect("entry");
    assert_eq!(kept, permissions);
    let value: String = data.try_into().expect("value");
    assert_eq!(value, "where");

    let change = tokio::time::timeout(Duration::from_secs(5), heard.next())
        .await
        .expect("heard in time")
        .expect("a signal")
        .expect("readable");
    let (table, id, deleted, data, changed): (
        String,
        String,
        bool,
        OwnedValue,
        HashMap<String, Vec<String>>,
    ) = change.body().deserialize().expect("change");
    assert_eq!(
        (table.as_str(), id.as_str(), deleted),
        ("documents", "one", false)
    );
    assert_eq!(changed, permissions);
    let value: String = data.try_into().expect("value");
    assert_eq!(value, "where");
}
