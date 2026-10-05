//! Where a tab's session runs: recorded with the session, opened at the first
//! sync, the device's own shell asked for once and never for an agent, and
//! the picker and environments panel on the real widget.

use std::cell::RefCell;
use std::rc::Rc;

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ROOT, ReadDoc};
use ui::widget::Widget;

use crate::sessions::{OpenState, Sessions};
use crate::target::{EnvironmentSummary, RecentTarget, SessionTarget};
use crate::{OpenError, Requester, TerminalWidget};

type Opened = Rc<RefCell<Vec<(SessionTarget, Requester)>>>;

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

fn recorded(doc: &AutoCommit) -> Option<SessionTarget> {
    doc.get(ROOT, crate::target::TARGET_KEY)
        .unwrap()
        .and_then(|(value, _)| value.into_string().ok())
        .and_then(|raw| SessionTarget::decode(&raw))
}

#[test]
fn a_new_tab_opens_the_most_recent_environment_and_records_it() {
    let dir = tempfile::tempdir().unwrap();
    let recent = RecentTarget::at(dir.path().join("recent.json"));
    recent.remember(&SessionTarget::environment("work"));
    let (mut widget, opened) = tab(recent, |_| Ok(()));
    let mut doc = AutoCommit::new();

    widget.sync(&mut doc, &ROOT);

    assert_eq!(
        opened.borrow().as_slice(),
        &[(SessionTarget::environment("work"), Requester::Person)]
    );
    assert_eq!(widget.session_state(), &OpenState::Open);
    assert_eq!(recorded(&doc), Some(SessionTarget::environment("work")));
    assert_eq!(
        <crate::resource::Terminal as ui::tilo_resource::Resource>::read(&doc)
            .unwrap()
            .target,
        Some(SessionTarget::environment("work")),
        "the target is part of the synced session record"
    );

    // A second sync does not open another session.
    widget.sync(&mut doc, &ROOT);
    assert_eq!(opened.borrow().len(), 1);
}

#[test]
fn reopening_a_session_goes_back_to_its_recorded_target_and_survives_a_new_process() {
    let (mut first, _) = tab(RecentTarget::none(), |_| Ok(()));
    let mut doc = AutoCommit::new();
    first.sync(&mut doc, &ROOT);
    first
        .switch_target(&mut doc, &ROOT, &SessionTarget::Host, Requester::Person)
        .unwrap();
    assert_eq!(recorded(&doc), Some(SessionTarget::Host));

    // Another device, or the next login, opens the same record.
    let mut reopened_doc = doc.fork();
    let (mut again, opened) = tab(RecentTarget::none(), |_| Ok(()));
    again.sync(&mut reopened_doc, &ROOT);
    assert_eq!(
        opened.borrow().as_slice(),
        &[(SessionTarget::Host, Requester::Person)]
    );
    assert_eq!(
        recorded(&reopened_doc),
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
    let mut doc = AutoCommit::new();
    widget.sync(&mut doc, &ROOT);
    let _ = widget.switch_target(&mut doc, &ROOT, &SessionTarget::Host, Requester::Person);
    assert_eq!(widget.session_state(), &OpenState::AwaitingAllow);

    let Some(ui::widget::AppHostIntent::AllowHostShell { reply }) = widget.take_session_intent()
    else {
        panic!("the tab must ask the shell for the person's allow");
    };
    widget.sync(&mut doc, &ROOT);
    assert_eq!(
        widget.session_state(),
        &OpenState::AwaitingAllow,
        "nothing opens before the person answers"
    );

    *allowed.borrow_mut() = true;
    reply.upgrade().unwrap().borrow_mut().answer = Some(true);
    widget.sync(&mut doc, &ROOT);
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
    let mut doc = AutoCommit::new();
    widget.sync(&mut doc, &ROOT);
    let _ = widget.switch_target(&mut doc, &ROOT, &SessionTarget::Host, Requester::Person);
    let Some(ui::widget::AppHostIntent::AllowHostShell { reply }) = widget.take_session_intent()
    else {
        panic!("expected an allow request");
    };
    reply.upgrade().unwrap().borrow_mut().answer = Some(false);
    widget.sync(&mut doc, &ROOT);
    assert!(matches!(widget.session_state(), OpenState::Failed(_)));
    assert!(widget.is_disconnected());
}

#[test]
fn agents_and_collaborators_never_open_the_device_shell() {
    let (mut widget, opened) = tab(RecentTarget::none(), |_| Ok(()));
    let mut doc = AutoCommit::new();
    widget.sync(&mut doc, &ROOT);
    let before = recorded(&doc);

    let err = widget
        .switch_target(&mut doc, &ROOT, &SessionTarget::Host, Requester::Agent)
        .unwrap_err();
    assert!(err.contains("agents"), "{err}");
    assert_eq!(recorded(&doc), before, "a refused target is not recorded");

    // Through the published operation, as an agent or a collaborator in a
    // shared room would reach it.
    let payload = serde_json::to_vec("host").unwrap();
    let op = ui::widget::ResourceOperation::local(
        ui::tilo_actions::InvocationId::new(),
        ui::tilo_actions::OperationKind::Effect,
        "terminal.target",
        &payload,
    );
    for actor in ["agent:helper", "0123abcd"] {
        let _scope = ui::invocation_scope::InvokerScope::enter(
            ui::tilo_actions::ActionInvoker::actor(actor),
        );
        assert!(
            Widget::start_operation(&mut widget, &op, &mut doc, &ROOT).is_err(),
            "{actor} must not open the device's own shell"
        );
    }
    assert!(
        opened.borrow().iter().all(|(target, _)| !target.is_host()),
        "nothing was ever opened on the host"
    );

    // An agent may move the session to another environment.
    let payload = serde_json::to_vec("environment:work").unwrap();
    let op = ui::widget::ResourceOperation::local(
        ui::tilo_actions::InvocationId::new(),
        ui::tilo_actions::OperationKind::Effect,
        "terminal.target",
        &payload,
    );
    let _scope = ui::invocation_scope::InvokerScope::enter(ui::tilo_actions::ActionInvoker::actor(
        "agent:helper",
    ));
    Widget::start_operation(&mut widget, &op, &mut doc, &ROOT).unwrap();
    assert_eq!(recorded(&doc), Some(SessionTarget::environment("work")));
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
    let host = ui::i18n::I18nManager::lookup("terminal-target-host", None);
    let manage = ui::i18n::I18nManager::lookup("terminal-target-manage", None);
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
        let request =
            crate::inner::ush_request(&[], &SessionTarget::Host, Requester::Person, None, (80, 24))
                .unwrap();
        let request: serde_json::Value = serde_json::from_slice(&request).unwrap();
        assert_eq!(request["target"], serde_json::json!({"kind": "host"}));
        assert_eq!(request["requested_by"], "person");
        assert!(
            crate::inner::ush_request(
                &[],
                &SessionTarget::Legacy { vm: "linux".into() },
                Requester::Person,
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
    let (mut widget, opened) = tab(RecentTarget::none(), |target| {
        if target.is_host() {
            Ok(())
        } else {
            Err(OpenError::NoRuntime(
                "no environment can be built on this device: no env-runtime image is installed"
                    .into(),
            ))
        }
    });
    let mut doc = AutoCommit::new();
    widget.sync(&mut doc, &ROOT);

    assert!(
        matches!(widget.session_state(), OpenState::NoRuntime(_)),
        "{:?}",
        widget.session_state()
    );
    let screen = widget.grid_text();
    assert!(
        screen.contains("env-runtime"),
        "the tab says why:\n{screen}"
    );
    assert!(
        screen.contains("Enter"),
        "the tab says what to do about it:\n{screen}"
    );
    assert!(
        widget.is_disconnected(),
        "the tab is not taken for a live shell"
    );
    assert_eq!(
        recorded(&doc),
        Some(SessionTarget::default_environment()),
        "nothing moves to the device's shell unasked"
    );

    // Enter takes the offer: the device's own shell, for the person, opened
    // at the next sync and recorded as where this session runs.
    press_enter(&mut widget, &mut doc);
    widget.sync(&mut doc, &ROOT);
    assert_eq!(widget.session_state(), &OpenState::Open);
    assert_eq!(
        opened.borrow().last(),
        Some(&(SessionTarget::Host, Requester::Person))
    );
    assert_eq!(recorded(&doc), Some(SessionTarget::Host));
}

#[test]
fn an_agent_is_told_why_and_never_offered_the_device_shell() {
    let (mut widget, opened) = tab(RecentTarget::none(), |_| {
        Err(OpenError::NoRuntime(
            "no env-runtime image is installed".into(),
        ))
    });
    let mut doc = AutoCommit::new();
    widget.sync(&mut doc, &ROOT);
    let err = widget
        .switch_target(
            &mut doc,
            &ROOT,
            &SessionTarget::default_environment(),
            Requester::Agent,
        )
        .unwrap_err();
    assert!(err.contains("env-runtime"), "{err}");
    assert!(
        matches!(widget.session_state(), OpenState::Failed(_)),
        "an agent's open offers nothing: {:?}",
        widget.session_state()
    );
    press_enter(&mut widget, &mut doc);
    widget.sync(&mut doc, &ROOT);
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

#[test]
fn a_relay_session_is_kept_and_re_attached_on_reopen_and_replaced_when_gone() {
    let attaches: Rc<RefCell<Vec<Option<String>>>> = Rc::default();
    let seen = attaches.clone();
    let session = "0123456789abcdef0123456789abcdef";
    let mut widget = TerminalWidget::spawn_headless().unwrap();
    widget.set_sessions(Sessions::scripted(
        RecentTarget::none(),
        move |_, _, attach| {
            seen.borrow_mut().push(attach.map(str::to_owned));
            if attach == Some("ffffffffffffffffffffffffffffffff") {
                return Err(OpenError::Refused("unknown session".into()));
            }
            let mut backend = crate::inner::TerminalBackend::for_test();
            backend.relay_session = Some(session.into());
            Ok(backend)
        },
    ));
    let mut doc = AutoCommit::new();
    widget.sync(&mut doc, &ROOT);
    widget
        .switch_target(&mut doc, &ROOT, &on_relay("work"), Requester::Person)
        .unwrap();
    let kept = |doc: &AutoCommit| {
        doc.get(ROOT, crate::target::RELAY_SESSION_KEY)
            .unwrap()
            .and_then(|(value, _)| value.into_string().ok())
    };
    assert_eq!(kept(&doc).as_deref(), Some(session));

    // Reopened — here or on another device — it re-attaches.
    let mut reopened = doc.fork();
    let (mut again, _) = tab(RecentTarget::none(), |_| Ok(()));
    let again_seen = attaches.clone();
    again.set_sessions(Sessions::scripted(
        RecentTarget::none(),
        move |_, _, attach| {
            again_seen.borrow_mut().push(attach.map(str::to_owned));
            Ok(crate::inner::TerminalBackend::for_test())
        },
    ));
    again.sync(&mut reopened, &ROOT);
    assert_eq!(attaches.borrow().last(), Some(&Some(session.to_owned())));

    // A session the relay no longer has is replaced by a new one.
    let mut stale = AutoCommit::new();
    stale
        .put(ROOT, crate::target::TARGET_KEY, on_relay("work").encode())
        .unwrap();
    stale
        .put(
            ROOT,
            crate::target::RELAY_SESSION_KEY,
            "ffffffffffffffffffffffffffffffff",
        )
        .unwrap();
    let before = attaches.borrow().len();
    widget.set_sessions(Sessions::scripted(RecentTarget::none(), {
        let seen = attaches.clone();
        move |target: &SessionTarget, _, attach: Option<&str>| {
            seen.borrow_mut().push(attach.map(str::to_owned));
            if attach.is_some() {
                return Err(OpenError::Refused("unknown session".into()));
            }
            let mut backend = crate::inner::TerminalBackend::for_test();
            if target.location().is_some_and(|l| !l.is_this_device()) {
                backend.relay_session = Some(session.into());
            }
            Ok(backend)
        }
    }));
    widget.sync(&mut stale, &ROOT);
    assert_eq!(
        attaches.borrow()[before..],
        [Some("ffffffffffffffffffffffffffffffff".to_owned()), None]
    );
    assert_eq!(widget.session_state(), &OpenState::Open);
    assert_eq!(kept(&stale).as_deref(), Some(session));

    // Back on this device, nothing is left to re-attach to.
    widget
        .switch_target(
            &mut stale,
            &ROOT,
            &SessionTarget::environment("work"),
            Requester::Person,
        )
        .unwrap();
    assert_eq!(kept(&stale), None);
}

#[test]
fn the_picker_offers_run_on_for_the_persons_relays_and_agents_name_one() {
    let (mut widget, opened) = tab(RecentTarget::none(), |_| Ok(()));
    let mut doc = AutoCommit::new();
    widget.sync(&mut doc, &ROOT);
    widget.open_target_menu();
    let labels = widget.target_menu_labels().unwrap();
    let run_on = ui::i18n::I18nManager::lookup("terminal-run-on", None);
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
    let here = ui::i18n::I18nManager::lookup("terminal-run-on-this-device", None);
    assert!(labels.contains(&format!("✓ {run_on} {here}")), "{labels:?}");
    assert!(labels.contains(&format!("{run_on} Home")), "{labels:?}");

    // An agent names the relay with the environment.
    let payload = serde_json::to_vec(&format!("environment:work@{RELAY}")).unwrap();
    let op = ui::widget::ResourceOperation::local(
        ui::tilo_actions::InvocationId::new(),
        ui::tilo_actions::OperationKind::Effect,
        "terminal.target",
        &payload,
    );
    let _scope = ui::invocation_scope::InvokerScope::enter(ui::tilo_actions::ActionInvoker::actor(
        "agent:helper",
    ));
    Widget::start_operation(&mut widget, &op, &mut doc, &ROOT).unwrap();
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
    let (mut widget, _) = tab(RecentTarget::none(), move |target| {
        if target.is_host() && !*gate.borrow() {
            Err(OpenError::NeedsAllow("not allowed".into()))
        } else {
            Ok(())
        }
    });
    let mut doc = AutoCommit::new();
    widget.sync(&mut doc, &ROOT);
    widget
        .switch_target(&mut doc, &ROOT, &SessionTarget::Host, Requester::Person)
        .unwrap();
    let intent = ui::widget::capability::capability_mut::<dyn ui::AppWidget>(&mut widget)
        .unwrap()
        .take_host_intent()
        .expect("native runtime must see the request");
    let ui::widget::AppHostIntent::AllowHostShell { reply } = intent else {
        panic!("expected host permission");
    };
    assert!(
        ui::widget::capability::capability_mut::<dyn ui::AppWidget>(&mut widget)
            .unwrap()
            .take_host_intent()
            .is_none()
    );
    *allowed.borrow_mut() = true;
    reply.upgrade().unwrap().borrow_mut().answer = Some(true);
    widget.sync(&mut doc, &ROOT);
    assert_eq!(widget.session_state(), &OpenState::Open);
}
