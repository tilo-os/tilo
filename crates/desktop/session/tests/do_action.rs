//! `tilo do settings/appearance#set-mode dark` sets one setting by its path through the session's
//! control socket, as a script sets any, and a process outside the session sets its own
//! preference at its app's address the same way.

use std::sync::mpsc;
use std::time::Duration;

use tilo_control::client::ControlSocket;
use tilo_control::front::{self, Spelling};

/// The appearance section, as the settings service publishes it.
const APPEARANCE: &str = "settings/appearance";

/// A session with the settings service, answering its control socket at `socket`, once it
/// offers its settings.
fn settings_session(root: &std::path::Path, socket: &std::path::Path) -> tilo_session::Session {
    let mut session = crate::common::headless(tilo_session::Options {
        services: vec![Box::new(settings_sections::service::SettingsService::new())],
        test_control: Some(socket.to_owned()),
        ..crate::common::api_options(root)
    });
    // The session offers its settings on a pass of its own; `tilo do` then finds them.
    for _ in 0..4 {
        session.step(Duration::from_millis(1)).expect("a pass");
    }
    session
}

/// The field `field` of `resource` as the session reads it, once it reads `wanted`.
fn reads(
    session: &mut tilo_session::Session,
    resource: &str,
    field: &str,
    wanted: &serde_json::Value,
) {
    let read = crate::common::wait_for(session, "the setting to change", |session| {
        session.settle_services();
        let read = session.read(resource)?.get(field).cloned()?;
        (read == *wanted).then_some(read)
    });
    assert_eq!(read, *wanted, "{resource}.{field}");
}

/// A settings section's action `verb` on `resource` with `args`, run through the session's
/// control socket at `socket` as `tilo do` and a process outside the session run one: what it
/// answered. The command and the session take turns, so it runs on its own thread while this one
/// steps.
fn run_at(
    session: &mut tilo_session::Session,
    socket: &std::path::Path,
    (resource, verb): (&str, &str),
    args: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let (done, answered) = mpsc::channel();
    let socket = socket.to_owned();
    let target = tilo_actions::remote::RemoteActionTarget {
        provider: tilo_actions::remote::settings_resources::PROVIDER.to_owned(),
        address: tilo_actions::ActionAddress::new(resource, verb),
    };
    let verb = verb.to_owned();
    std::thread::spawn(move || {
        let sockets = [ControlSocket::Named(socket)];
        let answer = front::call_target(&sockets, target, &verb, &args, &Spelling::tilo());
        done.send(answer).expect("the test listens");
    });
    crate::common::wait_for(session, "the setting's action to answer", |_| {
        answered.try_recv().ok()
    })
}

/// `tilo do` runs a settings section's action by its path, as a script runs any: the person's
/// appearance by its switch or its setter, a label read as the value it stands for, and one of
/// the device's settings, each set as the session reads it. Nothing is asked.
#[test]
fn tilo_do_sets_a_persons_setting_and_a_devices_by_their_paths() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let root = crate::common::private_home();
    let socket = root.path().join("test-control.sock");
    let mut session = settings_session(root.path(), &socket);
    let power = session
        .resources()
        .into_iter()
        .map(|resource| resource.id.as_str().to_owned())
        .find(|id| id.starts_with("devices/") && id.ends_with("/settings/power"))
        .expect("the session publishes the device's power settings");
    for (resource, verb, args, field, wanted) in [
        (
            APPEARANCE,
            "set-dark-mode",
            serde_json::json!({"on": false}),
            "mode",
            serde_json::json!("light"),
        ),
        (
            APPEARANCE,
            "set-mode",
            serde_json::json!({"value": "dark"}),
            "mode",
            serde_json::json!("dark"),
        ),
        (
            power.as_str(),
            "set-lock-minutes",
            serde_json::json!({"value": 12}),
            "lock_minutes",
            serde_json::json!(12),
        ),
    ] {
        let answer = run_at(&mut session, &socket, (resource, verb), args);
        assert!(
            answer.is_ok(),
            "{resource}#{verb} runs, asking nothing: {answer:?}"
        );
        reads(&mut session, resource, field, &wanted);
    }
}

/// A process outside the session (the on-screen keyboard keeping its size) sets its own
/// preference as `settings_store::outside` does: the setter at its app's address, run through the
/// control socket.
#[test]
fn a_process_outside_sets_its_own_preference_through_the_control_socket() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let root = crate::common::private_home();
    let socket = root.path().join("test-control.sock");
    let mut session = settings_session(root.path(), &socket);
    let answer = run_at(
        &mut session,
        &socket,
        ("settings/apps/so.tilo.osk", "set-size"),
        serde_json::json!({"value": "large"}),
    );
    assert!(answer.is_ok(), "the keyboard's size is set: {answer:?}");
    reads(
        &mut session,
        "settings/apps/so.tilo.osk",
        "size",
        &serde_json::json!("large"),
    );
}
