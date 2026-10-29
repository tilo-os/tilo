//! The desktop services a headless session starts on a session bus of its
//! own: an app's file chooser request, forwarded by the portal frontend,
//! is published as the session's question and answered through its door.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test fixtures: a failed step is the assertion"
)]

use std::collections::HashMap;
use std::io::BufRead as _;

use serde_json::{Map, Value, json};
use tilo_actions::ResourceSpec;
use tilo_compositor::OutputConfig;
use tilo_session::desktop_services::{Bus, Buses, DesktopServices};
use tilo_session::{Options, Session};
use zbus::zvariant::{ObjectPath, OwnedValue};

/// A session bus of its own, as `dbus-run-session` starts one, stopped on drop.
struct SessionBus {
    daemon: std::process::Child,
    address: String,
    _directory: tempfile::TempDir,
}

impl SessionBus {
    fn start() -> Self {
        let directory = tempfile::tempdir().unwrap();
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
        std::io::BufReader::new(daemon.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        assert!(!address.trim().is_empty(), "dbus-daemon printed no address");
        Self {
            daemon,
            address: address.trim().to_owned(),
            _directory: directory,
        }
    }
}

impl Drop for SessionBus {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}

const REQUEST: &str = "/org/freedesktop/portal/desktop/request/1_9/t1";

fn published(resources: &[ResourceSpec], id: &str) -> bool {
    resources
        .iter()
        .any(|spec| spec.id.as_str() == id || published(&spec.children, id))
}

#[test]
fn a_portal_file_chooser_request_is_the_sessions_question() {
    let bus = SessionBus::start();
    let desktop = DesktopServices::start(&Buses {
        session: Bus::Address(bus.address.clone()),
        system: Bus::None,
    })
    .unwrap();
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            services: desktop.services(),
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", 1280, 720)],
    )
    .expect("session");

    // The portal frontend, as xdg-desktop-portal forwards an app's call.
    let runtime = desktop.runtime();
    let frontend = runtime.block_on(async {
        let frontend = zbus::connection::Builder::address(bus.address.as_str())
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
    let mut question = None;
    for _ in 0..2000 {
        session.step(std::time::Duration::from_millis(1)).unwrap();
        question = session.read(&resource);
        if question.is_some() {
            break;
        }
    }
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
