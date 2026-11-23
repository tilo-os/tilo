//! `tilo do set-dark-mode` names one action and runs it without asking, and `tilo do
//! settings/appearance#set-mode dark` sets one setting by its address, as a script sets any.
//!
//! Settings publishes the person's appearance under its own provider (`tilo.settings`).
//! The session's own provider lists the resources its services publish, and Settings is
//! one of them: offered there too, the same action reached `tilo do` twice, and every
//! suggestion was spelled alike, so it could not be told apart or run.

use std::sync::mpsc;
use std::time::Duration;

use tilo_control::client::ControlSocket;
use tilo_control::front::{self, Io, Spelling};

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

/// Run `tilo do NAME WORDS…` against the session at `socket`: its exit, what it said and what it
/// complained of. The command and the session take turns: the control socket answers on the
/// session's pass, so the command runs on its own thread while this one steps.
fn tilo_do(
    session: &mut tilo_session::Session,
    socket: &std::path::Path,
    name: &str,
    words: &[&str],
) -> (u8, String, String) {
    let (done, answered) = mpsc::channel();
    let socket = socket.to_owned();
    let name = name.to_owned();
    let words: Vec<String> = words.iter().map(|&word| word.to_owned()).collect();
    std::thread::spawn(move || {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let exit = {
            let mut io = Io {
                out: &mut out,
                err: &mut err,
            };
            front::run(
                &[ControlSocket::Named(socket)],
                &name,
                &words,
                false,
                &Spelling::tilo(),
                &mut io,
            )
        };
        done.send((
            exit,
            String::from_utf8_lossy(&out).into_owned(),
            String::from_utf8_lossy(&err).into_owned(),
        ))
        .expect("the test listens");
    });
    crate::common::wait_for(session, "tilo do to answer", |_| answered.try_recv().ok())
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

#[test]
fn tilo_do_set_dark_mode_runs_the_one_action_without_asking() {
    let _settled = ui::motion::testing::settled();
    let root = crate::common::private_home();
    let socket = root.path().join("test-control.sock");
    let mut session = settings_session(root.path(), &socket);
    let (exit, out, err) = tilo_do(&mut session, &socket, "set-dark-mode", &["on=false"]);
    assert_eq!(
        exit, 0,
        "it runs the one action it found, asking nothing: out {out:?}, err {err:?}"
    );
    assert!(err.is_empty(), "it asks nothing of the person: {err:?}");
    // The action it found is Settings' own: the person's appearance is light now.
    reads(
        &mut session,
        APPEARANCE,
        "mode",
        &serde_json::json!("light"),
    );
}

/// A script sets any setting with `tilo do` and the setting's own setter: one of the person's
/// settings and one of the device's, each named by its address.
#[test]
fn tilo_do_sets_a_persons_setting_and_a_devices_by_their_setters() {
    let _settled = ui::motion::testing::settled();
    let root = crate::common::private_home();
    let socket = root.path().join("test-control.sock");
    let mut session = settings_session(root.path(), &socket);
    let power = session
        .resources()
        .into_iter()
        .map(|resource| resource.id.as_str().to_owned())
        .find(|id| id.starts_with("devices/") && id.ends_with("/settings/power"))
        .expect("the session publishes the device's power settings");

    for (resource, setter, value, field, wanted) in [
        (
            APPEARANCE,
            "set-mode",
            "dark",
            "mode",
            serde_json::json!("dark"),
        ),
        (
            power.as_str(),
            "set-lock-minutes",
            "12",
            "lock_minutes",
            serde_json::json!(12),
        ),
    ] {
        let (exit, out, err) = tilo_do(
            &mut session,
            &socket,
            &format!("{resource}#{setter}"),
            &[value],
        );
        assert_eq!(
            exit, 0,
            "{resource}#{setter} runs: out {out:?}, err {err:?}"
        );
        reads(&mut session, resource, field, &wanted);
    }
}

/// A process outside the session (the on-screen keyboard keeping its size) sets its own
/// preference as `settings_store::outside` does: the setter at its app's address, run through the
/// control socket.
#[test]
fn a_process_outside_sets_its_own_preference_through_the_control_socket() {
    let _settled = ui::motion::testing::settled();
    let root = crate::common::private_home();
    let socket = root.path().join("test-control.sock");
    let mut session = settings_session(root.path(), &socket);
    let (done, answered) = mpsc::channel();
    std::thread::spawn(move || {
        let target = tilo_actions::remote::RemoteActionTarget {
            provider: tilo_actions::remote::settings_resources::PROVIDER.to_owned(),
            address: tilo_actions::ActionAddress::new("settings/apps/so.tilo.osk", "set-size"),
        };
        done.send(front::call_target(
            &[ControlSocket::Named(socket)],
            target,
            "set-size",
            &serde_json::json!({"value": "large"}),
            &Spelling::tilo(),
        ))
        .expect("the test listens");
    });
    let answer = crate::common::wait_for(&mut session, "the setter to answer", |_| {
        answered.try_recv().ok()
    });
    assert!(answer.is_ok(), "the keyboard's size is set: {answer:?}");
    reads(
        &mut session,
        "settings/apps/so.tilo.osk",
        "size",
        &serde_json::json!("large"),
    );
}
