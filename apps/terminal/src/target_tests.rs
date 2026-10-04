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
    app.with_surface(|terminal: &mut TerminalWidget| terminal.set_sessions(sessions))
        .expect("the terminal surface is hosted");
    app.settle();
    opened
}

/// Where the session record says the session runs.
fn recorded(app: &App) -> Option<SessionTarget> {
    app.state().target
}

/// Drive the hosted terminal.
fn on_tab<T>(app: &mut App, visit: impl FnOnce(&mut TerminalWidget) -> T) -> T {
    let out = app
        .with_surface(visit)
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
    let mut doc = AutoCommit::new();
    widget.sync(&mut doc, &ROOT);
    let _ = widget.switch_target(&SessionTarget::Host, Requester::Person);
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
    let _ = widget.switch_target(&SessionTarget::Host, Requester::Person);
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
        let payload = serde_json::to_vec(target).unwrap();
        let op = ui::widget::ResourceOperation::local(
            ui::tilo_actions::InvocationId::new(),
            ui::tilo_actions::OperationKind::Effect,
            "terminal.target",
            &payload,
        );
        let _scope = ui::invocation_scope::InvokerScope::enter(
            ui::tilo_actions::ActionInvoker::actor(actor),
        );
        on_tab(app, |tab| {
            Widget::start_operation(tab, &op, &mut AutoCommit::new(), &ROOT).map(|_| ())
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
    let host = ui::i18n::I18nManager::lookup("terminal-target-host", None);
    let manage = ui::i18n::I18nManager::lookup("terminal-target-manage", None);
    assert_eq!(labels[0], "Default");
    assert_eq!(labels[1], "✓ Work", "the current target is marked");
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
