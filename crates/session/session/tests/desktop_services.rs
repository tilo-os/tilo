//! The desktop services a headless session starts on a session bus of its
//! own: an app's file chooser request, forwarded by the portal frontend,
//! is published as the session's question and answered through its door.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the session \
              outlives every call made into it"
)]

use std::collections::HashMap;
use std::rc::Rc;

use serde_json::{Map, Value, json};
use tilo_actions::ResourceSpec;
use tilo_compositor::OutputConfig;
use tilo_session::desktop_services::{Bus, Buses, DesktopServices};
use tilo_session::{Options, Session};
use tilo_test_env::PrivateBus;
use zbus::zvariant::{ObjectPath, OwnedValue};

const REQUEST: &str = "/org/freedesktop/portal/desktop/request/1_9/t1";

fn published(resources: &[ResourceSpec], id: &str) -> bool {
    resources
        .iter()
        .any(|spec| spec.id.as_str() == id || published(&spec.children, id))
}

#[test]
fn a_portal_file_chooser_request_is_the_sessions_question() {
    let bus = PrivateBus::start();
    let wake = tilo_service::LoopWake::default();
    let (side, link) = tilo_session::portal_link::channel(wake.handle());
    let desktop = Rc::new(
        DesktopServices::start(
            &Buses {
                session: Bus::Address(bus.address().to_owned()),
                system: Bus::None,
            },
            &wake.handle(),
            std::sync::Arc::new(tilo_portal::NoCapture),
            &side,
            None,
        )
        .unwrap(),
    );
    let portal = desktop
        .portal()
        .map(|serving| link.with_captures(serving.input_captures().clone()));
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            services: desktop.services(),
            wake,
            desktop: Some(Rc::clone(&desktop)),
            portal,
            collab: false,
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", 1280, 720)],
    )
    .expect("session");

    // The portal frontend, as xdg-desktop-portal forwards an app's call.
    let runtime = desktop.runtime();
    let frontend = runtime.block_on(async {
        let frontend = zbus::connection::Builder::address(bus.address())
            .unwrap()
            .build()
            .await
            .unwrap();
        frontend
            .request_name("org.freedesktop.portal.Desktop")
            .await
            .unwrap();
        frontend
    });
    let call = runtime.spawn({
        async move {
            let reply = frontend
                .call_method(
                    Some(tilo_portal::BUS_NAME),
                    tilo_portal::OBJECT_PATH,
                    Some("org.freedesktop.impl.portal.FileChooser"),
                    "OpenFile",
                    &(
                        ObjectPath::try_from(REQUEST).unwrap(),
                        "org.example.Editor",
                        "",
                        "Open a note",
                        HashMap::<String, OwnedValue>::new(),
                    ),
                )
                .await
                .unwrap();
            reply
                .body()
                .deserialize::<(u32, HashMap<String, OwnedValue>)>()
                .unwrap()
        }
    });

    let resource = format!("question:portal:{REQUEST}");
    // The question wakes the loop: a turn that would wait a minute ends
    // when it is asked.
    let mut question = None;
    let asked = std::time::Instant::now();
    for _ in 0..20 {
        session.step(std::time::Duration::from_secs(60)).unwrap();
        question = session.read(&resource);
        if question.is_some() {
            break;
        }
    }
    assert!(
        asked.elapsed() < std::time::Duration::from_secs(60),
        "the question woke the session"
    );
    let question = question.expect("the request is published as a question");
    assert_eq!(question["kind"], json!("portal.file-chooser"));
    assert_eq!(question["app_id"], json!("org.example.Editor"));
    assert_eq!(question["details"]["title"], json!("Open a note"));
    assert!(published(&session.resources(), &resource));

    let file = tempfile::NamedTempFile::new().unwrap();
    let uri = url::Url::from_file_path(file.path()).unwrap().to_string();
    let mut arguments = Map::new();
    arguments.insert("uris".into(), Value::Array(vec![json!(uri)]));
    session
        .invoke(&resource, "answer", &arguments)
        .expect("the sheet's answer");

    let (response, results) = runtime.block_on(call).unwrap();
    assert_eq!(response, 0, "the app got its file");
    let uris: Vec<String> = results["uris"].clone().try_into().unwrap();
    assert_eq!(uris, [uri]);
    assert!(session.read(&resource).is_none(), "answered, it is gone");
}

/// The control assistive technology names `name` under `node`, by target.
fn control(node: &ui::a11y::AccessNode, name: &str) -> Option<String> {
    if node.name.as_deref() == Some(name)
        && let Some(target) = &node.action_target
    {
        return Some(target.clone());
    }
    node.children.iter().find_map(|child| control(child, name))
}

/// Steps `session` until `asked` answers, for two seconds of session time at
/// most.
fn until<T>(session: &mut Session, mut asked: impl FnMut(&mut Session) -> Option<T>) -> Option<T> {
    for _ in 0..2000 {
        session.step(std::time::Duration::from_millis(1)).unwrap();
        if let Some(answer) = asked(session) {
            return Some(answer);
        }
    }
    None
}

/// An app's notification with its own image is shown by the banners and
/// kept in the centre; the button the person presses on the banner reaches
/// the app.
#[test]
fn an_apps_notification_is_a_banner_whose_action_reaches_the_app() {
    let bus = PrivateBus::start();
    let wake = tilo_service::LoopWake::default();
    let (side, link) = tilo_session::portal_link::channel(wake.handle());
    let desktop = Rc::new(
        DesktopServices::start(
            &Buses {
                session: Bus::Address(bus.address().to_owned()),
                system: Bus::None,
            },
            &wake.handle(),
            std::sync::Arc::new(tilo_portal::NoCapture),
            &side,
            None,
        )
        .unwrap(),
    );
    let portal = desktop
        .portal()
        .map(|serving| link.with_captures(serving.input_captures().clone()));
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            services: desktop.services(),
            wake,
            desktop: Some(Rc::clone(&desktop)),
            portal,
            collab: false,
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", 1280, 720)],
    )
    .expect("session");

    let runtime = desktop.runtime();
    let (app, id, mut invoked) = runtime.block_on(async {
        let app = zbus::connection::Builder::address(bus.address())
            .unwrap()
            .build()
            .await
            .unwrap();
        let proxy = zbus::Proxy::new(
            &app,
            "org.freedesktop.Notifications",
            "/org/freedesktop/Notifications",
            "org.freedesktop.Notifications",
        )
        .await
        .unwrap();
        let invoked = proxy.receive_signal("ActionInvoked").await.unwrap();
        let red = [255_u8, 0, 0, 255].repeat(4);
        let image = zbus::zvariant::Value::from(zbus::zvariant::Structure::from((
            2_i32, 2_i32, 8_i32, true, 8_i32, 4_i32, red,
        )));
        let hints = HashMap::from([("image-data", image)]);
        let reply = proxy
            .call_method(
                "Notify",
                &(
                    "Mail",
                    0_u32,
                    "",
                    "New mail",
                    "From Ada",
                    vec!["archive", "Archive"],
                    hints,
                    0_i32,
                ),
            )
            .await
            .unwrap();
        let id: u32 = reply.body().deserialize().unwrap();
        (app, id, invoked)
    });

    let archive = until(&mut session, |session| {
        session
            .banners()
            .shown()
            .accessibility
            .as_ref()
            .and_then(|node| control(node, "Archive"))
    })
    .expect("the banner shows with its button");
    assert_eq!(session.banners().shown().banners.len(), 1);

    // The app hears what the person chose while the session runs.
    let (heard, hear) = std::sync::mpsc::channel();
    runtime.spawn(async move {
        use futures_util::StreamExt as _;
        if let Some(signal) = invoked.next().await {
            let _ = heard.send(signal.body().deserialize::<(u32, String)>().unwrap());
        }
    });
    session
        .banners()
        .command(tilo_notification_banners::Command::Activate(archive));
    let chosen = until(&mut session, |_| hear.try_recv().ok());
    drop(app);
    assert_eq!(chosen, Some((id, "archive".to_owned())));
}
