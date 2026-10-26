//! Where a tab's session runs: recorded with the session, opened at the first
//! sync, the device's own shell asked for once and never for an agent, and
//! the picker and environments panel on the real widget.

use std::cell::RefCell;
use std::rc::Rc;

use automerge::{AutoCommit, ROOT};
use ui::widget::Widget;

use crate::sessions::{OpenState, Sessions};
use crate::target::{EnvironmentSummary, RecentTarget, SessionTarget};
use crate::{OpenError, Requester, TerminalWidget};

type Opened = Rc<RefCell<Vec<(SessionTarget, Requester)>>>;

#[test]
fn unopened_terminal_never_accepts_reviewed_input_into_sink() {
    let (mut widget, opened) = tab(RecentTarget::none(), |_| Ok(()));
    assert_eq!(widget.session_state(), &OpenState::Unopened);
    assert_eq!(widget.inspect_session()["lifecycle"], "unopened");
    assert_eq!(widget.inspect_session()["input_available"], false);
    assert!(widget.try_write_input(b"reviewed command\r").is_err());
    assert!(
        widget
            .perform(crate::resource::TerminalEffect::Send {
                text: "reviewed command".into(),
            })
            .is_err(),
        "an unattached sink must never report Send completion"
    );
    assert!(
        opened.borrow().is_empty(),
        "inspection/input must not open a session"
    );
}

#[test]
fn inspect_session_reports_production_open_failed_and_no_runtime_without_output() {
    use crate::resource::TerminalEffect;
    // Keep the controlled transport open until this test releases it. An
    // empty reader would race its real EOF against the inspection assertion.
    struct Reader(std::sync::mpsc::Receiver<()>);
    impl std::io::Read for Reader {
        fn read(&mut self, _buffer: &mut [u8]) -> std::io::Result<usize> {
            let _ = self.0.recv();
            Ok(0)
        }
    }
    for (answer, expected, reason) in [
        (Ok(()), "open", None),
        (
            Err(OpenError::Failed("test startup failure".into())),
            "failed",
            Some("test startup failure"),
        ),
        (
            Err(OpenError::NoRuntime("test runtime absent".into())),
            "no_runtime",
            Some("test runtime absent"),
        ),
    ] {
        let mut app = App::new().expect("terminal");
        let (held_transport, receiver) = std::sync::mpsc::channel();
        let mut receiver = Some(receiver);
        let opens = Rc::new(RefCell::new(0));
        let count = opens.clone();
        let sessions = Sessions::scripted(RecentTarget::none(), move |_, _, _| {
            *count.borrow_mut() += 1;
            answer.clone()?;
            let mut backend = crate::inner::TerminalBackend::for_test();
            backend.reader = Box::new(Reader(receiver.take().expect("one open")));
            Ok(backend)
        });
        app.with_surface(|terminal: &mut crate::TerminalView| terminal.set_sessions(sessions))
            .expect("terminal surface");
        app.settle();
        let before = *opens.borrow();
        let output = app
            .effect(&TerminalEffect::InspectSession)
            .expect("read-only inspection");
        let value = output.value.expect("typed inspection result");
        assert_eq!(value["lifecycle"], expected);
        assert_eq!(value["startup_reason"].as_str(), reason);
        assert_eq!(value["input_available"], expected == "open");
        if expected == "open" {
            assert!(
                value["session"]
                    .as_str()
                    .and_then(|id| uuid::Uuid::parse_str(id).ok())
                    .is_some()
            );
        }
        assert_eq!(*opens.borrow(), before, "Inspect must not open or restart");
        assert!(
            value.get("output").is_none(),
            "explicit shared Read owns terminal contents"
        );
        assert_eq!(
            value["host_access_requirement"],
            "requires_person_owner_grant"
        );
        drop(held_transport);
    }
}

/// A tab whose opens are recorded and answered by `answer`.
fn tab(
    recent: RecentTarget,
    mut answer: impl FnMut(&SessionTarget) -> Result<(), OpenError> + 'static,
) -> (TerminalWidget, Opened) {
    let opened: Opened = Rc::default();
    let log = opened.clone();
    let mut widget = TerminalWidget::spawn_headless().unwrap();
    widget.set_sessions(Sessions::scripted(recent, move |target, by, _attach| {
        log.borrow_mut().push((target.clone(), by));
        answer(target).map(|()| crate::inner::TerminalBackend::for_test())
    }));
    (widget, opened)
}

type App = ui::framework::testing::TestApp<crate::TerminalShell>;

/// Give the terminal `app` hosts on its session record a session source
/// whose opens are recorded and answered by `answer`, and run the sync that
/// opens its session.
fn host_tab(
    app: &mut App,
    recent: RecentTarget,
    mut answer: impl FnMut(&SessionTarget) -> Result<(), OpenError> + 'static,
) -> Opened {
    let opened: Opened = Rc::default();
    let log = opened.clone();
    let sessions = Sessions::scripted(recent, move |target, by, _attach| {
        log.borrow_mut().push((target.clone(), by));
        answer(target).map(|()| crate::inner::TerminalBackend::for_test())
    });
    app.with_surface(|terminal: &mut crate::TerminalView| terminal.set_sessions(sessions))
        .expect("the terminal surface is hosted");
    app.settle();
    opened
}

/// Where the session record says the session runs.
fn recorded(app: &App) -> Option<SessionTarget> {
    app.state().session_target()
}

/// Drive the hosted terminal.
fn on_tab<T>(app: &mut App, visit: impl FnOnce(&mut TerminalWidget) -> T) -> T {
    let out = app
        .with_surface(|view: &mut crate::TerminalView| visit(view))
        .expect("the terminal surface is hosted");
    app.settle();
    out
}

#[test]
fn a_new_tab_opens_the_most_recent_environment_and_records_it() {
    let dir = tempfile::tempdir().unwrap();
    let recent = RecentTarget::at(dir.path().join("recent.json"));
    recent.remember(&SessionTarget::environment("work"));
    let mut app = App::new().unwrap();
    let opened = host_tab(&mut app, recent, |_| Ok(()));

    assert_eq!(
        opened.borrow().as_slice(),
        &[(SessionTarget::environment("work"), Requester::Person)]
    );
    assert_eq!(
        on_tab(&mut app, |tab| tab.session_state().clone()),
        OpenState::Open
    );
    assert_eq!(
        recorded(&app),
        Some(SessionTarget::environment("work")),
        "the target is part of the synced session record"
    );

    // Another sync does not open another session.
    on_tab(&mut app, |_| ());
    assert_eq!(opened.borrow().len(), 1);
}

#[test]
fn reopening_a_session_goes_back_to_its_recorded_target_and_survives_a_new_process() {
    let mut app = App::new().unwrap();
    host_tab(&mut app, RecentTarget::none(), |_| Ok(()));
    on_tab(&mut app, |tab| {
        tab.switch_target(&SessionTarget::Host, Requester::Person)
    })
    .unwrap();
    assert_eq!(recorded(&app), Some(SessionTarget::Host));

    // The next login opens the same record.
    let mut app = app.reopen().unwrap();
    let opened = host_tab(&mut app, RecentTarget::none(), |_| Ok(()));
    assert_eq!(
        opened.borrow().as_slice(),
        &[(SessionTarget::Host, Requester::Person)]
    );
    assert_eq!(
        recorded(&app),
        Some(SessionTarget::Host),
        "binding the new process clears the old screen, not where it runs"
    );
}

#[test]
fn the_device_shell_is_asked_for_once_and_opens_when_allowed() {
    let allowed = Rc::new(RefCell::new(false));
    let gate = allowed.clone();
    let (mut widget, opened) = tab(RecentTarget::none(), move |target| {
        if target.is_host() && !*gate.borrow() {
            Err(OpenError::NeedsAllow("not allowed yet".into()))
        } else {
            Ok(())
        }
    });
    let doc = AutoCommit::new();
    widget.sync(&doc, &ROOT);
    let _ = widget.switch_target(&SessionTarget::Host, Requester::Person);
    assert_eq!(widget.session_state(), &OpenState::AwaitingAllow);
    assert_eq!(widget.inspect_session()["lifecycle"], "awaiting_allow");
    assert_eq!(widget.inspect_session()["input_available"], false);
    assert!(
        widget
            .try_write_input(b"must not reach previous process")
            .is_err()
    );

    let Some(ui::widget::AppHostIntent::AllowHostShell { reply }) = widget.take_session_intent()
    else {
        panic!("the tab must ask the shell for the person's allow");
    };
    widget.sync(&doc, &ROOT);
    assert_eq!(
        widget.session_state(),
        &OpenState::AwaitingAllow,
        "nothing opens before the person answers"
    );

    *allowed.borrow_mut() = true;
    reply.upgrade().unwrap().borrow_mut().answer = Some(true);
    widget.sync(&doc, &ROOT);
    assert_eq!(widget.session_state(), &OpenState::Open);
    let log = opened.borrow();
    assert_eq!(log.last(), Some(&(SessionTarget::Host, Requester::Person)));
}

#[test]
fn a_declined_allow_leaves_the_tab_saying_so() {
    let (mut widget, _) = tab(RecentTarget::none(), |target| {
        if target.is_host() {
            Err(OpenError::NeedsAllow("not allowed yet".into()))
        } else {
            Ok(())
        }
    });
    let doc = AutoCommit::new();
    widget.sync(&doc, &ROOT);
    let _ = widget.switch_target(&SessionTarget::Host, Requester::Person);
    let Some(ui::widget::AppHostIntent::AllowHostShell { reply }) = widget.take_session_intent()
    else {
        panic!("expected an allow request");
    };
    reply.upgrade().unwrap().borrow_mut().answer = Some(false);
    widget.sync(&doc, &ROOT);
    assert!(matches!(widget.session_state(), OpenState::Failed(_)));
    assert!(widget.is_disconnected());
}

#[test]
fn agents_and_collaborators_never_open_the_device_shell() {
    let mut app = App::new().unwrap();
    let opened = host_tab(&mut app, RecentTarget::none(), |_| Ok(()));
    let before = recorded(&app);

    let err = on_tab(&mut app, |tab| {
        tab.switch_target(&SessionTarget::Host, Requester::Agent)
    })
    .unwrap_err();
    assert!(err.contains("agents"), "{err}");
    assert_eq!(recorded(&app), before, "a refused target is not recorded");

    // Through the published operation, as an agent or a collaborator in a
    // shared room would reach it.
    let operate = |app: &mut App, actor: &str, target: &str| {
        let _scope = ui::invocation_scope::InvokerScope::enter(
            ui::tilo_actions::ActionInvoker::actor(actor),
        );
        on_tab(app, |tab| {
            tab.perform(crate::resource::TerminalEffect::OpenIn {
                target: target.into(),
            })
        })
    };
    for actor in ["agent:helper", "0123abcd"] {
        assert!(
            operate(&mut app, actor, "host").is_err(),
            "{actor} must not open the device's own shell"
        );
    }
    assert!(
        opened.borrow().iter().all(|(target, _)| !target.is_host()),
        "nothing was ever opened on the host"
    );

    // An agent may move the session to another environment.
    operate(&mut app, "agent:helper", "environment:work").unwrap();
    assert_eq!(recorded(&app), Some(SessionTarget::environment("work")));
    assert_eq!(
        opened.borrow().last(),
        Some(&(SessionTarget::environment("work"), Requester::Agent))
    );
}

#[test]
fn the_picker_lists_environments_the_host_and_legacy_vms_only_when_reported() {
    let (mut widget, _) = tab(RecentTarget::none(), |_| Ok(()));
    widget.show_target(
        SessionTarget::environment("work"),
        vec![
            EnvironmentSummary {
                key: "default".into(),
                name: "Default".into(),
                tools: vec![],
                grants: vec![],
            },
            EnvironmentSummary {
                key: "work".into(),
                name: "Work".into(),
                tools: vec!["pandoc".into()],
                grants: vec![("/var/home/you/src".into(), true)],
            },
        ],
    );
    widget.open_target_menu();
    let labels = widget.target_menu_labels().unwrap();
    let host = crate::i18n::t("terminal-target-host");
    let manage = crate::i18n::t("terminal-target-manage");
    assert_eq!(labels[0], "Default");
    assert_eq!(labels[1], "Work");
    assert_eq!(
        widget.target_menu_checked(),
        Some(vec![false, true, false, false]),
        "the current target is checked"
    );
    assert_eq!(labels[2], host);
    assert_eq!(labels.last(), Some(&manage));
    assert!(!labels.iter().any(|l| l.contains("linux")));

    widget.set_legacy_vms(vec!["linux".into()]);
    widget.open_target_menu();
    let labels = widget.target_menu_labels().unwrap();
    assert!(labels.iter().any(|l| l.starts_with("linux")), "{labels:?}");
}

#[test]
fn supervisor_replies_say_what_to_do_next() {
    assert_eq!(
        crate::inner::refusal(br#"{"result":"host-not-allowed","reason":"ask"}"#),
        OpenError::NeedsAllow("ask".into())
    );
    assert_eq!(
        crate::inner::refusal(br#"{"result":"denied","reason":"no"}"#),
        OpenError::Refused("no".into())
    );
    let list = crate::sessions::parse_directory_reply(
        r#"{"result":"environments","environments":[
            {"key":"default","name":"Default","tools":["git"],"grants":[["/src",true]]},
            {"key":"../x","name":"Bad"}]}"#,
    )
    .unwrap();
    assert_eq!(list.len(), 1, "a key no record may have is dropped");
    assert_eq!(list[0].tools, vec!["git".to_string()]);
    assert_eq!(
        crate::sessions::parse_directory_reply(r#"{"result":"denied","reason":"nope"}"#),
        Err("nope".into())
    );
    #[cfg(unix)]
    {
        let request = crate::inner::ush_request(
            &[],
            &SessionTarget::Host,
            Requester::Person,
            None,
            Some("0123456789abcdef"),
            (80, 24),
        )
        .unwrap();
        let request: serde_json::Value = serde_json::from_slice(&request).unwrap();
        assert_eq!(request["target"], serde_json::json!({"kind": "host"}));
        assert_eq!(request["requested_by"], "person");
        assert_eq!(request["runs"], "0123456789abcdef");
        assert!(
            crate::inner::ush_request(
                &[],
                &SessionTarget::Legacy { vm: "linux".into() },
                Requester::Person,
                None,
                None,
                (80, 24)
            )
            .is_err(),
            "a legacy VM is not this device's supervisor's to serve"
        );
    }
}

#[test]
fn targets_are_spelled_out_for_agents() {
    assert_eq!(
        crate::parse_target_argument("environment"),
        Some(SessionTarget::default_environment())
    );
    assert_eq!(
        crate::parse_target_argument("environment:work"),
        Some(SessionTarget::environment("work"))
    );
    assert_eq!(
        crate::parse_target_argument("legacy:linux"),
        Some(SessionTarget::Legacy { vm: "linux".into() })
    );
    assert_eq!(
        crate::parse_target_argument("host"),
        Some(SessionTarget::Host)
    );
    for bad in ["", "environment:../etc", "vm:linux", "legacy:"] {
        assert_eq!(crate::parse_target_argument(bad), None, "{bad}");
    }
}

#[test]
fn legacy_vms_appear_once_the_legacy_vm_service_has_made_one() {
    let dir = tempfile::tempdir().unwrap();
    assert!(crate::sessions::legacy_vms_under(dir.path()).is_empty());
    let linux = dir.path().join("tilo/legacy/linux");
    std::fs::create_dir_all(&linux).unwrap();
    assert!(
        crate::sessions::legacy_vms_under(dir.path()).is_empty(),
        "a directory without a disk is not a VM"
    );
    std::fs::write(linux.join("disk.qcow2"), b"").unwrap();
    assert_eq!(
        crate::sessions::legacy_vms_under(dir.path()),
        vec!["linux".to_string()]
    );
}

fn press_enter(widget: &mut TerminalWidget, doc: &mut AutoCommit) {
    crate::tests::send_key_without_text(
        widget,
        doc,
        winit::keyboard::Key::Named(winit::keyboard::NamedKey::Enter),
    );
}

#[test]
fn without_a_runtime_the_tab_says_why_and_offers_the_device_shell() {
    let mut app = App::new().unwrap();
    let opened = host_tab(&mut app, RecentTarget::none(), |target| {
        if target.is_host() {
            Ok(())
        } else {
            Err(OpenError::NoRuntime(
                "no environment can be built on this device: no env-runtime image is installed"
                    .into(),
            ))
        }
    });
    let state = on_tab(&mut app, |widget| widget.session_state().clone());
    assert!(matches!(state, OpenState::NoRuntime(_)), "{state:?}");
    let screen = on_tab(&mut app, |widget| widget.grid_text());
    assert!(
        screen.contains("env-runtime"),
        "the tab says why:\n{screen}"
    );
    assert!(
        screen.contains("Enter"),
        "the tab says what to do about it:\n{screen}"
    );
    assert!(
        on_tab(&mut app, |widget| widget.is_disconnected()),
        "the tab is not taken for a live shell"
    );
    assert_eq!(
        recorded(&app),
        Some(SessionTarget::default_environment()),
        "nothing moves to the device's shell unasked"
    );

    // Enter takes the offer through the real hosted event and settlement path.
    app.harness().key("Enter");
    app.settle();
    assert_eq!(
        on_tab(&mut app, |widget| widget.session_state().clone()),
        OpenState::Open
    );
    assert_eq!(
        opened.borrow().last(),
        Some(&(SessionTarget::Host, Requester::Person))
    );
    assert_eq!(recorded(&app), Some(SessionTarget::Host));
}

#[test]
fn an_agent_is_told_why_and_never_offered_the_device_shell() {
    let (mut widget, opened) = tab(RecentTarget::none(), |_| {
        Err(OpenError::NoRuntime(
            "no env-runtime image is installed".into(),
        ))
    });
    let mut doc = AutoCommit::new();
    widget.sync(&doc, &ROOT);
    let err = widget
        .switch_target(&SessionTarget::default_environment(), Requester::Agent)
        .unwrap_err();
    assert!(err.contains("env-runtime"), "{err}");
    assert!(
        matches!(widget.session_state(), OpenState::Failed(_)),
        "an agent's open offers nothing: {:?}",
        widget.session_state()
    );
    press_enter(&mut widget, &mut doc);
    widget.sync(&doc, &ROOT);
    assert!(
        opened.borrow().iter().all(|(target, _)| !target.is_host()),
        "{:?}",
        opened.borrow()
    );
}

const RELAY: &str = "abababababababababababababababababababababababababababababababab";

fn on_relay(record: &str) -> SessionTarget {
    SessionTarget::environment(record).at(crate::target::Location::Relay {
        relay: RELAY.into(),
    })
}

/// Give the hosted terminal a session source that records every relay
/// session it is asked to re-attach to and answers with `answer`.
fn attach_with(
    app: &mut App,
    attaches: &Rc<RefCell<Vec<Option<String>>>>,
    mut answer: impl FnMut(
        &SessionTarget,
        Option<&str>,
    ) -> Result<crate::inner::TerminalBackend, OpenError>
    + 'static,
) {
    let seen = attaches.clone();
    let sessions = Sessions::scripted(RecentTarget::none(), move |target, _, attach| {
        seen.borrow_mut().push(attach.map(str::to_owned));
        answer(target, attach)
    });
    app.with_surface(|terminal: &mut crate::TerminalView| terminal.set_sessions(sessions))
        .expect("the terminal surface is hosted");
    app.settle();
}

#[test]
fn a_relay_session_is_kept_and_re_attached_on_reopen_and_replaced_when_gone() {
    let attaches: Rc<RefCell<Vec<Option<String>>>> = Rc::default();
    let session = "0123456789abcdef0123456789abcdef";
    let on_the_relay = move |target: &SessionTarget, _: Option<&str>| {
        let mut backend = crate::inner::TerminalBackend::for_test();
        if target.location().is_some_and(|l| !l.is_this_device()) {
            backend.relay_session = Some(session.into());
        }
        Ok(backend)
    };
    let mut app = App::new().unwrap();
    attach_with(&mut app, &attaches, on_the_relay);
    on_tab(&mut app, |tab| {
        tab.switch_target(&on_relay("work"), Requester::Person)
    })
    .unwrap();
    assert_eq!(app.state().relay(), Some(session));

    // Reopened, it re-attaches.
    let mut app = app.reopen().unwrap();
    attach_with(&mut app, &attaches, on_the_relay);
    assert_eq!(attaches.borrow().last(), Some(&Some(session.to_owned())));

    // A session the relay no longer has is replaced by a new one.
    let gone = "ffffffffffffffffffffffffffffffff";
    app.invoke(crate::resource::Terminal::RECORD_RELAY_SESSION.with(Some(gone.to_owned())))
        .unwrap();
    let mut app = app.reopen().unwrap();
    let before = attaches.borrow().len();
    attach_with(&mut app, &attaches, move |target, attach| {
        if attach.is_some() {
            return Err(OpenError::Refused("unknown session".into()));
        }
        on_the_relay(target, attach)
    });
    assert_eq!(attaches.borrow()[before..], [Some(gone.to_owned()), None]);
    assert_eq!(
        on_tab(&mut app, |tab| tab.session_state().clone()),
        OpenState::Open
    );
    assert_eq!(app.state().relay(), Some(session));

    // Back on this device, nothing is left to re-attach to.
    on_tab(&mut app, |tab| {
        tab.switch_target(&SessionTarget::environment("work"), Requester::Person)
    })
    .unwrap();
    assert_eq!(app.state().relay(), None);
}

#[test]
fn the_picker_offers_run_on_for_the_persons_relays_and_agents_name_one() {
    let (mut widget, opened) = tab(RecentTarget::none(), |_| Ok(()));
    let doc = AutoCommit::new();
    widget.sync(&doc, &ROOT);
    widget.open_target_menu();
    let labels = widget.target_menu_labels().unwrap();
    let run_on = crate::i18n::t("terminal-run-on");
    assert!(
        !labels.iter().any(|l| l.contains(&run_on)),
        "without relays there is nowhere else to run: {labels:?}"
    );

    widget.set_relays(vec![crate::target::RelaySummary {
        relay: RELAY.into(),
        label: "Home".into(),
    }]);
    widget.open_target_menu();
    let labels = widget.target_menu_labels().unwrap();
    let here = crate::i18n::t("terminal-run-on-this-device");
    let checked = widget.target_menu_checked().unwrap();
    let row = |label: String| labels.iter().position(|l| *l == label);
    let this_device = row(format!("{run_on} {here}")).expect("run on this device");
    let home = row(format!("{run_on} Home")).expect("run on the relay");
    assert!(
        checked[this_device] && !checked[home],
        "where it runs carries the menu's check, not a glyph: {labels:?}"
    );

    // An agent names the relay with the environment.
    let _scope = ui::invocation_scope::InvokerScope::enter(ui::tilo_actions::ActionInvoker::actor(
        "agent:helper",
    ));
    widget
        .perform(crate::resource::TerminalEffect::OpenIn {
            target: format!("environment:work@{RELAY}"),
        })
        .unwrap();
    assert_eq!(
        opened.borrow().last(),
        Some(&(on_relay("work"), Requester::Agent))
    );
    assert_eq!(crate::parse_target_argument("host@ab"), None);
    assert_eq!(
        crate::parse_target_argument("environment:work@home"),
        None,
        "a relay is named by its endpoint id"
    );
}

#[test]
fn native_terminal_requests_the_same_person_confirmed_host_shell_allow() {
    let mut sessions = crate::sessions::Sessions::standalone();
    assert!(sessions.request_allow());
    let ui::widget::AppHostIntent::AllowHostShell { reply } = sessions.take_intent().unwrap()
    else {
        panic!("expected host shell permission");
    };
    reply.upgrade().unwrap().borrow_mut().answer = Some(false);
    assert_eq!(sessions.take_allow_answer(), Some(false));
    assert_eq!(sessions.take_allow_answer(), None);
}

#[test]
fn native_widget_exposes_the_permission_request_and_retries_after_the_answer() {
    let allowed = Rc::new(RefCell::new(false));
    let gate = allowed.clone();
    let mut app = App::new().unwrap();
    let opened = host_tab(&mut app, RecentTarget::none(), move |target| {
        if target.is_host() && !*gate.borrow() {
            Err(OpenError::NeedsAllow("not allowed".into()))
        } else {
            Ok(())
        }
    });
    on_tab(&mut app, |widget| {
        let tree = widget.accessibility().unwrap();
        let picker = tree
            .children
            .iter()
            .find(|node| node.name.as_deref() == Some("Default environment"))
            .expect("the current target must be an accessible picker");
        assert_eq!(picker.role, ui::a11y::Role::Button);
        assert!(widget.perform_semantic_action(
            picker.action_target.as_deref().unwrap(),
            &ui::a11y::Action::Activate,
            &mut ui::widget::Edits::lend(&mut AutoCommit::new()),
            &ROOT,
        ));
        let tree = widget.accessibility().unwrap();
        let menu = tree
            .children
            .iter()
            .find(|node| node.role == ui::a11y::Role::Menu)
            .unwrap();
        let host = menu
            .children
            .iter()
            .find(|node| node.name.as_deref() == Some("Your shell on this device"))
            .unwrap();
        assert!(widget.perform_semantic_action(
            host.action_target.as_deref().unwrap(),
            &ui::a11y::Action::Activate,
            &mut ui::widget::Edits::lend(&mut AutoCommit::new()),
            &ROOT,
        ));
    });
    assert_eq!(
        on_tab(&mut app, |widget| widget.session_state().clone()),
        OpenState::AwaitingAllow
    );
    let versions = app.versions().len();
    let intent = app
        .harness()
        .take_host_intent()
        .expect("the canonical native app adapter must see the request");
    let ui::widget::AppHostIntent::AllowHostShell { reply } = intent else {
        panic!("expected host permission");
    };
    assert!(
        app.harness().take_host_intent().is_none(),
        "the host consumes the request once"
    );
    assert_eq!(
        app.versions().len(),
        versions,
        "consuming host intent does not write the record"
    );
    *allowed.borrow_mut() = true;
    reply.upgrade().unwrap().borrow_mut().answer = Some(true);
    app.settle();
    assert_eq!(
        on_tab(&mut app, |widget| widget.session_state().clone()),
        OpenState::Open
    );
    assert_eq!(
        opened.borrow().last(),
        Some(&(SessionTarget::Host, Requester::Person))
    );
    assert_eq!(recorded(&app), Some(SessionTarget::Host));
}

#[test]
fn canonical_host_permission_refusal_does_not_open_or_retry_the_session() {
    let mut app = App::new().unwrap();
    let opened = host_tab(&mut app, RecentTarget::none(), |target| {
        if target.is_host() {
            Err(OpenError::NeedsAllow("not allowed".into()))
        } else {
            Ok(())
        }
    });
    on_tab(&mut app, |widget| {
        widget.switch_target(&SessionTarget::Host, Requester::Person)
    })
    .unwrap();
    let Some(ui::widget::AppHostIntent::AllowHostShell { reply }) =
        app.harness().take_host_intent()
    else {
        panic!("expected canonical host permission");
    };
    let attempts = opened.borrow().len();
    reply.upgrade().unwrap().borrow_mut().answer = Some(false);
    app.settle();
    assert!(matches!(
        on_tab(&mut app, |widget| widget.session_state().clone()),
        OpenState::Failed(_)
    ));
    assert!(on_tab(&mut app, |widget| widget.is_disconnected()));
    assert!(app.harness().take_host_intent().is_none());
    app.settle();
    assert_eq!(
        opened.borrow().len(),
        attempts,
        "refusal never retries the host session"
    );
}

/// A refused device shell is not the tab's record: restored at the next
/// login, the tab opens where a new one would and asks nothing. A Terminal
/// left on the refused shell put the same prompt up at every login.
#[test]
fn a_refused_device_shell_is_not_where_the_next_session_opens() {
    let refuse_host = |target: &SessionTarget| {
        if target.is_host() {
            Err(OpenError::NeedsAllow("not allowed".into()))
        } else {
            Ok(())
        }
    };
    let mut app = App::new().unwrap();
    host_tab(&mut app, RecentTarget::none(), refuse_host);
    on_tab(&mut app, |widget| {
        widget.switch_target(&SessionTarget::Host, Requester::Person)
    })
    .unwrap();
    assert_eq!(recorded(&app), Some(SessionTarget::Host));
    let Some(ui::widget::AppHostIntent::AllowHostShell { reply }) =
        app.harness().take_host_intent()
    else {
        panic!("expected the person to be asked");
    };
    reply.upgrade().unwrap().borrow_mut().answer = Some(false);
    app.settle();
    assert!(matches!(
        on_tab(&mut app, |widget| widget.session_state().clone()),
        OpenState::Failed(_)
    ));
    assert_eq!(
        recorded(&app),
        Some(SessionTarget::default_environment()),
        "the refusal hands the record back to where a new tab opens"
    );

    let mut app = app.reopen().unwrap();
    let opened = host_tab(&mut app, RecentTarget::none(), refuse_host);
    assert_eq!(
        opened.borrow().as_slice(),
        &[(SessionTarget::default_environment(), Requester::Person)]
    );
    assert!(
        app.harness().take_host_intent().is_none(),
        "the next login asks nothing"
    );
}

#[test]
fn trusted_shell_selection_enters_normal_permission_without_agent_identity_relabeling() {
    let mut app = App::new().unwrap();
    let opened = host_tab(&mut app, RecentTarget::none(), |target| {
        if target.is_host() {
            Err(OpenError::NeedsAllow("not allowed".into()))
        } else {
            Ok(())
        }
    });
    assert!(app.harness().request_device_shell());
    assert_eq!(
        on_tab(&mut app, |widget| widget.session_state().clone()),
        OpenState::AwaitingAllow
    );
    let intent = app
        .harness()
        .take_host_intent()
        .expect("ordinary host permission flow");
    let ui::widget::AppHostIntent::AllowHostShell { reply } = intent else {
        panic!("expected permission request");
    };
    reply.upgrade().unwrap().borrow_mut().answer = Some(false);
    app.settle();
    assert!(
        !opened
            .borrow()
            .iter()
            .any(|(target, by)| target.is_host() && *by == Requester::Agent)
    );
}

#[test]
fn approved_owner_host_command_uses_real_pty_and_stale_or_agent_input_is_refused() {
    use ui::floating_surface::DeviceShellOperation as O;
    let allowed = Rc::new(std::cell::Cell::new(false));
    let gate = allowed.clone();
    let sessions = owner_fixture_sessions(gate);
    let mut app = App::new().unwrap();
    app.with_surface(|terminal: &mut crate::TerminalView| terminal.set_sessions(sessions))
        .unwrap();
    app.settle();
    let mounted = mounted_device_shell_operation(&mut app, &O::Inspect);
    assert_eq!(mounted["target"]["kind"], "environment");
    let selected = app.harness().device_shell_operation(&O::Select).unwrap();
    assert_eq!(selected["lifecycle"], "awaiting_allow");
    let ui::widget::AppHostIntent::AllowHostShell { reply } =
        app.harness().take_host_intent().unwrap()
    else {
        panic!("normal permission request");
    };
    allowed.set(true);
    reply.upgrade().unwrap().borrow_mut().answer = Some(true);
    app.settle();
    let state = app.harness().device_shell_operation(&O::Inspect).unwrap();
    assert_eq!(state["lifecycle"], "open");
    let session = state["session"].as_str().unwrap().to_owned();
    assert!(
        app.harness()
            .device_shell_operation(&O::Run {
                session: uuid::Uuid::new_v4().to_string(),
                command: "printf stale".into()
            })
            .is_err()
    );
    {
        let _scope = ui::invocation_scope::InvokerScope::enter(
            ui::tilo_actions::ActionInvoker::actor("agent:helper"),
        );
        let result = app
            .with_surface(|terminal: &mut crate::TerminalView| {
                terminal.perform(crate::resource::TerminalEffect::Send {
                    text: "printf forbidden".into(),
                })
            })
            .unwrap();
        assert!(result.is_err());
    }
    let submitted = app
        .harness()
        .device_shell_operation(&O::Run {
            session: session.clone(),
            command: "printf 'owner-pty-ok\\n'".into(),
        })
        .unwrap();
    assert_eq!(submitted["status"], "submitted");
    wait_for_owner_shell(&mut app, &session, false);
    app.harness()
        .device_shell_operation(&O::Run {
            session: session.clone(),
            command: "exit".into(),
        })
        .unwrap();
    wait_for_owner_shell(&mut app, &session, true);
    assert!(
        app.harness()
            .device_shell_operation(&O::Run {
                session: session.clone(),
                command: "printf ended".into()
            })
            .is_err()
    );
    let retained = app
        .harness()
        .device_shell_operation(&O::Read {
            session: session.clone(),
        })
        .unwrap();
    let mounted_retained = mounted_device_shell_operation(
        &mut app,
        &O::Read {
            session: session.clone(),
        },
    );
    assert_eq!(
        mounted_retained, retained,
        "mounted view reads the same actual ended PTY"
    );
    assert!(
        retained["text"]
            .as_str()
            .unwrap()
            .lines()
            .any(|line| line.trim() == "owner-pty-ok"),
        "ended output must remain readable"
    );
    assert!(
        app.harness()
            .device_shell_operation(&O::Read {
                session: uuid::Uuid::new_v4().to_string()
            })
            .is_err()
    );
}

fn mounted_device_shell_operation(
    app: &mut App,
    operation: &ui::floating_surface::DeviceShellOperation,
) -> serde_json::Value {
    app.with_surface(|view: &mut crate::TerminalView| {
        ui::AppWidget::device_shell_operation(view, operation)
    })
    .unwrap()
    .unwrap()
}

fn wait_for_owner_shell(app: &mut App, session: &str, ended: bool) {
    use ui::floating_surface::DeviceShellOperation as O;
    let deadline = tilo_test_env::hang_deadline();
    loop {
        app.settle();
        let observed = if ended {
            app.harness().device_shell_operation(&O::Inspect).unwrap()["transport_disconnected"]
                == true
                && on_tab(app, |widget| widget.pty_output_drained())
        } else {
            let output = app
                .harness()
                .device_shell_operation(&O::Read {
                    session: session.into(),
                })
                .unwrap();
            output["text"]
                .as_str()
                .unwrap()
                .lines()
                .any(|line| line.trim() == "owner-pty-ok")
        };
        if observed {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "actual owner PTY output/exit did not arrive"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

fn owner_fixture_sessions(gate: Rc<std::cell::Cell<bool>>) -> Sessions {
    Sessions::scripted(RecentTarget::none(), move |target, by, _| {
        assert_eq!(by, Requester::Person);
        if !target.is_host() {
            return Err(OpenError::NoRuntime("fixture runtime absent".into()));
        }
        if !gate.get() {
            return Err(OpenError::NeedsAllow(
                "fixture owner consent required".into(),
            ));
        }
        let command = interactive_bash();
        crate::inner::TerminalBackend::with_command(command)
            .map_err(|error| OpenError::Failed(error.to_string()))
    })
}

/// An interactive bash without user startup files. Resolved through `PATH`:
/// bash lives at `/usr/bin/bash` on merged-`/usr` Linux but only at
/// `/bin/bash` on macOS.
fn interactive_bash() -> portable_pty::CommandBuilder {
    let mut command = portable_pty::CommandBuilder::new("bash");
    command.args(["--noprofile", "--norc", "-i"]);
    command
}

#[test]
fn published_read_follows_actual_pty_output_into_the_shared_session() {
    use crate::resource::TerminalEffect;
    let sessions = Sessions::scripted(RecentTarget::none(), |target, _, _| {
        assert_eq!(target, &SessionTarget::default_environment());
        let mut command = interactive_bash();
        command.env("PS1", "[Default fixture]$ ");
        crate::inner::TerminalBackend::with_command(command)
            .map_err(|error| OpenError::Failed(error.to_string()))
    });
    let mut app = App::new().unwrap();
    app.with_surface(|terminal: &mut crate::TerminalView| terminal.set_sessions(sessions))
        .unwrap();
    app.settle();
    let inspected = app.effect(&TerminalEffect::InspectSession).unwrap();
    let session = inspected.value.unwrap()["session"].clone();
    app.with_surface(|terminal: &mut crate::TerminalView| {
        terminal.try_write_input(b"printf 'TILO_%s\\n' OWNED_PTY_OUTPUT\r")
    })
    .unwrap()
    .unwrap();
    let deadline = tilo_test_env::hang_deadline();
    let mut observed = false;
    while std::time::Instant::now() < deadline {
        app.settle();
        let read = app
            .call("read-screen", serde_json::json!({}))
            .unwrap()
            .unwrap();
        let output = read["output"].as_str().unwrap();
        assert_eq!(read["session"], session);
        let lines: Vec<_> = output.lines().collect();
        if let Some(index) = lines
            .iter()
            .position(|line| *line == "TILO_OWNED_PTY_OUTPUT")
            && lines[index + 1..].contains(&"[Default fixture]$")
        {
            let local = app
                .with_surface(|terminal: &mut crate::TerminalView| terminal.grid_text())
                .unwrap();
            assert!(local.lines().any(|line| line == "TILO_OWNED_PTY_OUTPUT"));
            observed = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    app.with_surface(|terminal: &mut crate::TerminalView| terminal.try_write_input(b"exit\r"))
        .unwrap()
        .unwrap();
    wait_for_owner_shell(&mut app, session.as_str().unwrap(), true);
    assert!(observed, "canonical retained PTY output/prompt absent");
}

/// A tab whose sessions run an interactive bash on a real PTY.
fn bash_sessions() -> Sessions {
    Sessions::scripted(RecentTarget::none(), |_, _, _| {
        crate::inner::TerminalBackend::with_command(interactive_bash())
            .map_err(|error| OpenError::Failed(error.to_string()))
    })
}

/// Settle `app` until `done`, within the hang bound.
fn settle_until(app: &mut App, what: &str, mut done: impl FnMut(&mut App) -> bool) {
    let deadline = tilo_test_env::hang_deadline();
    while !done(app) {
        assert!(std::time::Instant::now() < deadline, "{what}");
        app.settle();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[test]
fn exiting_the_shell_closes_its_window_and_keeps_nothing_behind() {
    // A tab the shell shows in its own process, through the window it lent.
    let window = ui::hosted_window::HostedWindow::new();
    let mut app = App::new().unwrap();
    let lent = window.clone();
    app.with_surface(|terminal: &mut crate::TerminalView| {
        terminal.show_in_window(Some(lent));
        terminal.set_sessions(bash_sessions());
    })
    .unwrap();
    app.settle();
    assert_eq!(
        on_tab(&mut app, |widget| widget.session_state().clone()),
        OpenState::Open
    );
    assert!(!window.take_close(), "a running shell keeps its window");

    on_tab(&mut app, |widget| widget.try_write_input(b"exit\r")).unwrap();
    settle_until(&mut app, "the exited shell closes its window", |_| {
        window.take_close()
    });
    app.settle();
    on_tab(&mut app, |widget| {
        assert!(widget.shell_exited());
        assert_eq!(widget.lifecycle_banner(), None, "no ended-session banner");
        let tree = widget.accessibility().unwrap();
        assert!(
            !tree.children.iter().any(|node| node
                .name
                .as_deref()
                .is_some_and(|name| name.contains("Session ended"))),
            "nothing says the session ended: the window is closing"
        );
    });

    // A window in a process of its own closes its tab through the desktop.
    let _ = ui::shell_tabs::take_requests();
    let mut app = App::new().unwrap();
    app.with_surface(|terminal: &mut crate::TerminalView| {
        terminal.show_in_window(None);
        terminal.set_sessions(bash_sessions());
    })
    .unwrap();
    app.settle();
    on_tab(&mut app, |widget| widget.try_write_input(b"exit\r")).unwrap();
    let mut asked = Vec::new();
    settle_until(&mut app, "the exited shell closes its tab", |_| {
        asked.extend(ui::shell_tabs::take_requests());
        asked.contains(&ui::shell_tabs::TabRequest::CloseThis)
    });
}

#[test]
fn a_session_the_tab_replaced_or_dropped_closes_nothing() {
    let window = ui::hosted_window::HostedWindow::new();
    let (mut widget, _) = tab(RecentTarget::none(), |_| Ok(()));
    widget.show_in_window(Some(window.clone()));
    let replaced = widget.next_session_exit();
    let current = widget.next_session_exit();
    replaced.exited();
    assert!(
        !window.take_close(),
        "the shell of a replaced session ended"
    );
    current.exited();
    assert!(window.take_close(), "the shell the tab shows exited");

    let dropped = widget.next_session_exit();
    drop(widget);
    dropped.exited();
    assert!(
        !window.take_close(),
        "a closed tab's shell ending closes nothing"
    );
}

#[test]
fn the_target_picker_is_in_the_window_bar_and_the_content_has_no_chrome() {
    let window = ui::hosted_window::HostedWindow::new();
    let mut app = App::new().unwrap();
    let lent = window.clone();
    app.with_surface(|terminal: &mut crate::TerminalView| terminal.show_in_window(Some(lent)))
        .unwrap();
    let opened = host_tab(&mut app, RecentTarget::none(), |_| Ok(()));

    // The window's title in its bar is where the shell runs.
    let (_, toolbar) = window.toolbar_since(None).expect("the slots are declared");
    let location = toolbar
        .and_then(|toolbar| toolbar.location)
        .expect("the window bar names the target");
    assert_eq!(location.label, crate::i18n::t("terminal-target-default"));
    assert!(!location.editable, "a target is picked, not typed");
    let labels: Vec<&str> = location.choices.iter().map(|c| c.label.as_str()).collect();
    let host = crate::i18n::t("terminal-target-host");
    let manage = crate::i18n::t("terminal-target-manage");
    assert!(labels.contains(&host.as_str()), "{labels:?}");
    assert_eq!(
        labels.last(),
        Some(&manage.as_str()),
        "a new one is made there"
    );
    assert_eq!(
        location
            .choices
            .iter()
            .filter(|choice| choice.current)
            .map(|choice| choice.label.as_str())
            .collect::<Vec<_>>(),
        vec![location.label.as_str()],
        "the picker marks the current target"
    );

    // The content holds only the terminal: no chip, no header above the grid.
    on_tab(&mut app, |widget| {
        assert!(
            widget.content_top_inset().abs() < f64::EPSILON,
            "no band above the grid: {}",
            widget.content_top_inset()
        );
        let tree = widget.accessibility().unwrap();
        assert!(
            !tree
                .children
                .iter()
                .any(|node| node.role == ui::a11y::Role::Button
                    && node.name.as_deref() == Some(location.label.as_str())),
            "the content draws no target picker of its own"
        );
    });

    // Choosing the device's shell in the bar's picker moves the session there.
    let choice = location
        .choices
        .iter()
        .find(|choice| choice.label == host)
        .expect("the device's shell is offered");
    window.deliver(ui::window_toolbar::ToolbarEvent::Commit {
        text: choice.value.clone(),
    });
    app.settle();
    app.settle();
    assert_eq!(
        opened.borrow().last(),
        Some(&(SessionTarget::Host, Requester::Person))
    );
    assert_eq!(recorded(&app), Some(SessionTarget::Host));
    let (_, toolbar) = window.toolbar_since(None).expect("the slots");
    assert_eq!(
        toolbar
            .and_then(|toolbar| toolbar.location)
            .map(|l| l.label),
        Some(host),
        "the title follows the session"
    );
}

#[test]
fn a_shell_that_could_not_start_says_so_in_the_window_with_a_retry() {
    let attempts = Rc::new(std::cell::Cell::new(0));
    let count = attempts.clone();
    let mut app = App::new().unwrap();
    let opened = host_tab(&mut app, RecentTarget::none(), move |_| {
        count.set(count.get() + 1);
        if count.get() == 1 {
            Err(OpenError::Failed("the relay cannot be reached".into()))
        } else {
            Ok(())
        }
    });
    let retry = on_tab(&mut app, |widget| {
        assert_eq!(widget.start_failure(), Some("the relay cannot be reached"));
        assert_eq!(widget.lifecycle_banner(), None, "the reason is said once");
        let tree = widget.accessibility().unwrap();
        let failure = tree
            .children
            .iter()
            .find(|node| node.description.as_deref() == Some("the relay cannot be reached"))
            .expect("the window says why plainly");
        let retry = failure
            .children
            .iter()
            .find(|node| node.role == ui::a11y::Role::Button)
            .expect("with a retry");
        assert_eq!(
            retry.name.as_deref(),
            Some(crate::i18n::t("terminal-start-retry").as_str())
        );
        retry.action_target.clone().unwrap()
    });
    assert!(on_tab(&mut app, |widget| widget.perform_semantic_action(
        &retry,
        &ui::a11y::Action::Activate,
        &mut ui::widget::Edits::lend(&mut AutoCommit::new()),
        &ROOT,
    )));
    assert_eq!(
        on_tab(&mut app, |widget| widget.session_state().clone()),
        OpenState::Open
    );
    assert_eq!(attempts.get(), 2);
    let opened = opened.borrow();
    assert_eq!(opened[0], opened[1], "the retry opens the same target");
}

/// A remote shell's session the supervisor handed over, as the supervisor
/// holds it: the far end of its terminal output, and of its lease.
#[cfg(unix)]
type Handed = Rc<
    RefCell<
        Vec<(
            std::os::unix::net::UnixStream,
            std::os::unix::net::UnixStream,
        )>,
    >,
>;

/// End the newest handed-over session the way the supervisor does: say how
/// it ended on its lease, then close it and the terminal.
#[cfg(unix)]
fn end_remote(handed: &Handed, report: &str) {
    use std::io::Write;
    let (output, lease) = handed
        .borrow_mut()
        .pop()
        .expect("a session was handed over");
    (&lease).write_all(report.as_bytes()).unwrap();
    lease.shutdown(std::net::Shutdown::Both).unwrap();
    drop(output);
}

// An ending connection to a remote shell is not the shell exiting: the
// window says it is reconnecting, reconnects (automatically, and on Try
// again), and only the remote shell's exit closes it.
#[cfg(unix)]
#[test]
fn a_lost_connection_to_a_remote_shell_reconnects_and_only_its_exit_closes_the_window() {
    let session = "0123456789abcdef0123456789abcdef";
    let handed: Handed = Rc::default();
    let reachable = Rc::new(std::cell::Cell::new(true));
    let attaches: Rc<RefCell<Vec<Option<String>>>> = Rc::default();
    let window = ui::hosted_window::HostedWindow::new();
    let mut app = App::new().unwrap();
    let lent = window.clone();
    app.with_surface(|terminal: &mut crate::TerminalView| terminal.show_in_window(Some(lent)))
        .unwrap();
    let keep = handed.clone();
    let up = reachable.clone();
    attach_with(&mut app, &attaches, move |target, _| {
        if target.location().is_some_and(|l| !l.is_this_device()) && !up.get() {
            return Err(OpenError::Failed("the relay cannot be reached".into()));
        }
        let (output, supervisor_output) = std::os::unix::net::UnixStream::pair().unwrap();
        let (lease, supervisor_lease) = std::os::unix::net::UnixStream::pair().unwrap();
        keep.borrow_mut()
            .push((supervisor_output, supervisor_lease));
        Ok(crate::inner::TerminalBackend::for_test_remote(
            output, lease, session,
        ))
    });
    on_tab(&mut app, |tab| {
        tab.switch_target(&on_relay("work"), Requester::Person)
    })
    .unwrap();
    assert_eq!(
        on_tab(&mut app, |tab| tab.session_state().clone()),
        OpenState::Open
    );

    // The connection to the relay is lost, and the relay stays out of reach.
    reachable.set(false);
    end_remote(
        &handed,
        "{\"ended\":\"lost\",\"reason\":\"the relay went quiet\"}\n",
    );
    settle_until(&mut app, "the tab notices the lost connection", |app| {
        on_tab(app, |tab| tab.reconnecting().is_some())
    });
    assert!(!window.take_close(), "a lost connection keeps the window");
    let retry = on_tab(&mut app, |tab| {
        assert_eq!(
            tab.reconnect_banner(),
            Some(crate::i18n::t("terminal-reconnecting")),
            "the window says, quietly, that it is reconnecting"
        );
        assert_eq!(
            tab.lifecycle_banner(),
            None,
            "and nothing about a session ending"
        );
        assert_eq!(tab.start_failure(), None, "and no failure");
        let tree = tab.accessibility().unwrap();
        let group = tree
            .children
            .iter()
            .find(|node| {
                node.name.as_deref() == Some(crate::i18n::t("terminal-reconnecting").as_str())
            })
            .expect("reconnecting reaches assistive technology");
        group.children[0].action_target.clone().unwrap()
    });
    let activate = |app: &mut App, target: &str| {
        assert!(on_tab(app, |tab| tab.perform_semantic_action(
            target,
            &ui::a11y::Action::Activate,
            &mut ui::widget::Edits::lend(&mut AutoCommit::new()),
            &ROOT,
        )));
    };

    // Try again while the relay is out of reach: still reconnecting, to the
    // same session.
    let before = attaches.borrow().len();
    activate(&mut app, &retry);
    assert_eq!(
        attaches.borrow().get(before).cloned(),
        Some(Some(session.to_owned())),
        "it re-attaches to the session it lost"
    );
    assert!(on_tab(&mut app, |tab| tab.reconnecting().is_some()));
    assert!(!window.take_close());
    assert_eq!(app.state().relay(), Some(session), "the session is kept");

    // The relay is back: Try again re-attaches.
    reachable.set(true);
    activate(&mut app, &retry);
    assert_eq!(
        on_tab(&mut app, |tab| tab.session_state().clone()),
        OpenState::Open
    );
    assert!(on_tab(&mut app, |tab| tab.reconnecting().is_none()));
    assert_eq!(attaches.borrow().last(), Some(&Some(session.to_owned())));

    // The remote shell exits: its window closes.
    end_remote(&handed, "{\"ended\":\"exited\"}\n");
    settle_until(
        &mut app,
        "the remote shell's exit closes its window",
        |_| window.take_close(),
    );
}

#[test]
fn a_supervisors_report_names_how_a_remote_session_ended() {
    use crate::inner::SessionEnd;
    assert_eq!(
        SessionEnd::from_report(br#"{"ended":"exited"}"#),
        SessionEnd::Exited
    );
    assert_eq!(
        SessionEnd::from_report(br#"{"ended":"lost","reason":"quiet"}"#),
        SessionEnd::Lost("quiet".into())
    );
    assert_eq!(
        SessionEnd::from_report(br#"{"ended":"superseded"}"#),
        SessionEnd::Superseded
    );
    assert_eq!(
        SessionEnd::from_report(b"not a report"),
        SessionEnd::Exited,
        "a lease closing is the shell exiting"
    );
}

#[cfg(unix)]
mod inherited_control_wire;
#[test]
fn native_resource_terminal_inspector_is_acknowledged_through_the_mounted_root() {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;
    use ui::floating_surface::{
        DeviceShellOperation, FloatingSurfaceClientMessage, FloatingSurfaceHostMessage,
        FloatingSurfaceServiceClient,
    };

    ui::framework::testing::drive_live_window::<crate::TerminalShell, _>(|app| {
        let (app_side, mut host_side) = UnixStream::pair().unwrap();
        host_side.set_nonblocking(true).unwrap();
        app.test_floating_service_connection(
            FloatingSurfaceServiceClient::from_test_stream(app_side).unwrap(),
        );
        for request in [81, 82] {
            let mut message =
                serde_json::to_vec(&FloatingSurfaceHostMessage::DeviceShellOperation {
                    request,
                    operation: DeviceShellOperation::Inspect,
                })
                .unwrap();
            message.push(b'\n');
            host_side.write_all(&message).unwrap();
            assert!(app.test_pump_floating_service());
            let mut received = Vec::new();
            assert_eq!(
                host_side.read_to_end(&mut received).unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock
            );
            let outcomes = received
                .split(|byte| *byte == b'\n')
                .filter(|line| !line.is_empty())
                .filter_map(|line| {
                    match serde_json::from_slice::<FloatingSurfaceClientMessage>(line).unwrap() {
                        FloatingSurfaceClientMessage::DeviceShellOutcome { request, result } => {
                            Some((request, result))
                        }
                        _ => None,
                    }
                })
                .collect::<Vec<_>>();
            assert_eq!(
                outcomes.len(),
                1,
                "one exact response on the original connection"
            );
            assert_eq!(outcomes[0].0, request);
            let value = outcomes[0]
                .1
                .as_ref()
                .expect("native mounted Terminal Inspector must be available");
            assert!(value["lifecycle"].is_string());
            assert!(value["input_available"].is_boolean());
            assert!(value["transport_disconnected"].is_boolean());
            assert!(
                !app.test_pump_floating_service(),
                "idle service polling must not replay the operation"
            );
        }
    })
    .unwrap();
}
