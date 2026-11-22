//! `tilo do set-dark-mode` names one action and runs it without asking.
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
const APPEARANCE: &str = "settings:settings.appearance";

#[test]
fn tilo_do_set_dark_mode_runs_the_one_action_without_asking() {
    let _settled = ui::motion::testing::settled();
    let root = crate::common::private_home();
    let socket = root.path().join("test-control.sock");
    let mut session = crate::common::headless(tilo_session::Options {
        services: vec![Box::new(tilo_settings_service::SettingsService::new())],
        test_control: Some(socket.clone()),
        ..crate::common::api_options(root.path())
    });
    // The session offers its settings on a pass of its own; `tilo do` then finds them.
    for _ in 0..4 {
        session.step(Duration::from_millis(1)).expect("a pass");
    }
    // The command and the session take turns: the control socket answers on the session's
    // pass, so the command runs on its own thread while this one steps.
    let (done, answered) = mpsc::channel();
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
                "set-dark-mode",
                &["on=false".to_owned()],
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
    let (exit, out, err) =
        crate::common::wait_for(&mut session, "tilo do set-dark-mode to answer", |_| {
            answered.try_recv().ok()
        });
    assert_eq!(
        exit, 0,
        "it runs the one action it found, asking nothing: out {out:?}, err {err:?}"
    );
    assert!(err.is_empty(), "it asks nothing of the person: {err:?}");
    // The action it found is Settings' own: the person's appearance is light now.
    let mode = crate::common::wait_for(&mut session, "the appearance to change", |session| {
        session.settle_services();
        session.read(APPEARANCE)?["mode"]
            .as_str()
            .map(str::to_owned)
    });
    assert_eq!(mode, "light", "set-dark-mode ran as it was typed");
}
