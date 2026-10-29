//! The backend on a session bus of its own, called as the portal frontend
//! calls it, and by connections that are not the frontend.

use std::collections::HashMap;
use std::io::BufRead as _;
use std::sync::Arc;
use std::time::Duration;

use tilo_service::{Answer, Questions};
use zbus::zvariant::{ObjectPath, OwnedValue};

use crate::auth::FRONTEND_BUS_NAME;
use crate::{BUS_NAME, OBJECT_PATH, Portal, PortalAuthenticator, PortalCaller, question_handle};

/// A session bus of its own, as `dbus-run-session` starts one, stopped on drop.
struct SessionBus {
    daemon: std::process::Child,
    address: String,
    _directory: tempfile::TempDir,
}

impl SessionBus {
    fn start() -> Self {
        let directory = tempfile::tempdir().expect("temporary directory");
        let mut daemon = std::process::Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .arg(format!(
                "--address=unix:path={}",
                directory.path().join("bus").display()
            ))
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("the session bus needs dbus-daemon, which the builder image installs");
        let mut address = String::new();
        std::io::BufReader::new(daemon.stdout.take().expect("piped"))
            .read_line(&mut address)
            .expect("dbus-daemon's address");
        assert!(!address.trim().is_empty(), "dbus-daemon printed no address");
        Self {
            daemon,
            address: address.trim().to_owned(),
            _directory: directory,
        }
    }

    async fn connect(&self) -> zbus::Connection {
        zbus::connection::Builder::address(self.address.as_str())
            .expect("address")
            .build()
            .await
            .expect("connected")
    }
}

impl Drop for SessionBus {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}

/// The backend serving on its own connection, a frontend owning its name
/// and a stranger on the same bus.
struct Bed {
    _bus: SessionBus,
    questions: Questions,
    portal: Portal,
    frontend: zbus::Connection,
    stranger: zbus::Connection,
    _backend: zbus::Connection,
}

async fn bed() -> Bed {
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
    let portal = Portal::new(questions.clone(), authenticator)
        .with_consents(crate::consent::Consents::new(questions.clone(), None));
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
        frontend,
        stranger,
        _backend: backend,
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
async fn asked(questions: &Questions, handle: &str) {
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
