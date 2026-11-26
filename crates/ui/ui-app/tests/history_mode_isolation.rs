//! A managed record's live effects under History: what a preview discards, what a retained
//! control holds, and what housekeeping never waits for.
use automerge::{ROOT, transaction::Transactable};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};
use tilo_ui::history_mode::testing::*;
use tilo_ui::history_mode::*;
use tilo_ui::{State, headless::Driver, widget::Text};
use tilo_ui_base::widget::ActionEmitter;

#[test]
fn preview_discards_unadmitted_local_intents_but_polls_an_already_admitted_effect() {
    let emitter = Rc::new(RefCell::new(ActionEmitter::new(
        tilo_actions::ActionBinding::unit_for(&resource(), "export").unwrap(),
    )));
    let starts = Rc::new(Cell::new(0));
    let finish = Rc::new(Cell::new(false));
    let completions = Rc::new(Cell::new(0));
    let local_events = Rc::new(Cell::new(0));
    let owner = LiveOwner {
        resource_id: "live",
        type_id: "fixture.export",
        text: Text::new("Live owner"),
        emitter: emitter.clone(),
        starts: starts.clone(),
        finish: finish.clone(),
        completions: completions.clone(),
        local_events: local_events.clone(),
        collab_events: Rc::new(Cell::new(0)),
        collab_wake_handles: Rc::new(RefCell::new(Vec::new())),
        bracket: Rc::new(RefCell::new(None)),
        undo_request: Rc::new(Cell::new(None)),
        active: None,
        manual: Rc::new(RefCell::new(None)),
        confirmation: tilo_actions::ConfirmationPolicy::None,
        prompted: false,
        mutable_identity: None,
    };
    let mut state = State::new();
    state.doc.put(ROOT, "body", "first").unwrap();
    state.commit_action(None, None);
    let mut driver = Driver::from_managed_root(
        state,
        ManagedRoot::new(Box::new(owner), registration()),
        (900, 650),
    )
    .unwrap();
    emitter.borrow_mut().emit(()).unwrap();
    driver.key("x");
    assert_eq!(
        starts.get(),
        1,
        "real local host api admitted the first effect"
    );
    emitter.borrow_mut().emit(()).unwrap();
    driver.click_named("history-open").unwrap();
    assert!(
        emitter.borrow_mut().take().is_none(),
        "opening History moves the exact pre-entry queued intent into host ownership"
    );
    let retained = driver.pending_history_controls();
    assert_eq!(retained.len(), 1);
    assert_eq!(retained[0].invocation.address.resource, "live");
    assert_eq!(retained[0].invocation.address.action, "export");
    assert_eq!(
        retained[0].status,
        tilo_ui::history_mode::pending_controls::PendingControlStatus::Suspended
    );
    driver
        .perform_named("activity_timeline", &tilo_ui_base::a11y::Action::Focus)
        .unwrap();
    driver.key("End");
    driver.key("Enter");
    assert!(driver.targets().contains_key("history-preview"));
    let events = local_events.get();
    emitter.borrow_mut().emit(()).unwrap();
    finish.set(true);
    driver.key("x");
    driver.set_modifiers(&["Ctrl".into()]);
    driver.key("z");
    driver.set_modifiers(&[]);
    assert_eq!(
        local_events.get(),
        events,
        "hidden live keyboard and undo routes are absent"
    );
    assert_eq!(starts.get(), 1);
    assert_eq!(
        completions.get(),
        1,
        "admitted effect remains attached to the live owner"
    );
    assert!(driver.click_named("live-owner").is_err());
    driver.click_named("history-back-live").unwrap();
    let retained = driver.pending_history_controls();
    assert_eq!(retained.len(), 1);
    assert_eq!(
        retained[0].status,
        tilo_ui::history_mode::pending_controls::PendingControlStatus::Ready
    );
    assert_eq!(
        starts.get(),
        1,
        "return to live does not admit retained work"
    );
    assert_eq!(completions.get(), 1, "return does not replay entered work");
    let stale = driver
        .pending_history_control_token(retained[0].id)
        .expect("current exact continuation token");
    driver.click_named("history-open").unwrap();
    assert!(driver.continue_pending_history_control(&stale).is_none());
    driver.click_named("history-open").unwrap();
    assert!(driver.continue_pending_history_control(&stale).is_none());
    let retained = driver.pending_history_controls();
    let token = driver
        .pending_history_control_token(retained[0].id)
        .expect("fresh continuation after re-entry");
    assert_eq!(
        driver.continue_pending_history_control(&token),
        Some(tilo_ui::history_mode::pending_controls::PendingControlStatus::Consumed)
    );
    assert!(
        driver.pending_history_controls().is_empty(),
        "confirmed item is consumed: {:?}",
        driver.pending_history_controls()
    );
    assert_eq!(starts.get(), 2, "explicit continuation enters once");
    assert!(driver.continue_pending_history_control(&token).is_none());
    driver.key("y");
    assert_eq!(
        starts.get(),
        2,
        "preview cannot leave a queued local effect to run on return"
    );
    assert!(local_events.get() > events, "normal live input resumes");
}

#[test]
fn retained_phrase_confirmation_requires_fresh_visible_input_and_preserves_arguments() {
    let emitter = Rc::new(RefCell::new(ActionEmitter::new(
        tilo_actions::ActionBinding::unit_for(&resource(), "export").unwrap(),
    )));
    let manual = Rc::new(RefCell::new(Some(tilo_actions::ControlInvocation {
        address: tilo_actions::ActionAddress::new("phrase-owner", "export"),
        arguments: [(
            tilo_actions::PROMPTED_PARAM.to_owned(),
            serde_json::Value::String("alpha payload".into()),
        )]
        .into_iter()
        .collect(),
    })));
    let starts = Rc::new(Cell::new(0));
    let owner = LiveOwner {
        resource_id: "phrase-owner",
        type_id: "fixture.phrase-export",
        text: Text::new("Phrase owner"),
        emitter,
        starts: starts.clone(),
        finish: Rc::new(Cell::new(false)),
        completions: Rc::new(Cell::new(0)),
        local_events: Rc::new(Cell::new(0)),
        collab_events: Rc::new(Cell::new(0)),
        collab_wake_handles: Rc::new(RefCell::new(Vec::new())),
        bracket: Rc::new(RefCell::new(None)),
        undo_request: Rc::new(Cell::new(None)),
        active: None,
        manual: manual.clone(),
        confirmation: tilo_actions::ConfirmationPolicy::Phrase {
            phrase: "ERASE phrase-owner".into(),
        },
        prompted: true,
        mutable_identity: None,
    };
    let mut driver = Driver::from_managed_root(
        State::new(),
        ManagedRoot::new(Box::new(owner), registration()),
        (900, 650),
    )
    .unwrap();

    driver.click_named("history-open").unwrap();
    assert_eq!(driver.pending_history_controls().len(), 1);
    driver.click_named("history-open").unwrap();
    // The device is there for the press that asks, after History returned: a
    // peer from the first frame would have run the live control instead.
    driver.connect_peer();
    driver.relayout();
    assert_eq!(starts.get(), 0, "Back to live remains inert");
    assert_eq!(
        driver.pending_history_controls()[0].status,
        tilo_ui::history_mode::pending_controls::PendingControlStatus::Ready
    );
    let held = &driver.pending_history_controls()[0].invocation;
    assert_eq!(
        held.arguments.get(tilo_actions::PROMPTED_PARAM),
        Some(&serde_json::Value::String("alpha payload".into())),
        "the held call keeps the arguments it was queued with"
    );
    assert!(
        driver.targets().contains_key("history-pending-1"),
        "Continue is public after return: {:?}",
        driver.targets().keys().collect::<Vec<_>>()
    );
    assert_eq!(driver.asking(), None, "nothing is asked before the press");

    // Continue asks the device, as a press whose action asks does: it names
    // the action and shows neither internal owners nor raw arguments.
    driver.click_named("history-pending-1").unwrap();
    assert_eq!(starts.get(), 0, "Continue asks, it does not run the effect");
    let (asked, name) = driver.asking().expect("Continue waits on the device's ask");
    assert_eq!(asked.action, "export");
    assert!(
        name.contains("Export"),
        "the ask names the action it asks about: {name:?}"
    );
    assert!(
        !name.contains("alpha payload") && !name.contains("component "),
        "internal owners and raw arguments are never shown: {name:?}"
    );

    // A phrase that is not the one declared is no yes: nothing of the call runs.
    assert!(
        driver.confirm_asked_typing(Some("wrong")).is_some(),
        "the person answered the ask"
    );
    assert_eq!(starts.get(), 0, "wrong phrase cannot attest confirmation");
    assert_eq!(driver.asking(), None, "the answered ask is done");
    assert!(
        driver.pending_history_controls().is_empty(),
        "the asked call is the device's; History holds it no longer: {:?}",
        driver.pending_history_controls()
    );

    // A fresh press asks again, and the exact phrase admits the call once.
    *manual.borrow_mut() = Some(tilo_actions::ControlInvocation {
        address: tilo_actions::ActionAddress::new("phrase-owner", "export"),
        arguments: [(
            tilo_actions::PROMPTED_PARAM.to_owned(),
            serde_json::Value::String("alpha payload".into()),
        )]
        .into_iter()
        .collect(),
    });
    driver.key("x");
    assert_eq!(
        starts.get(),
        0,
        "a fresh press whose action asks runs nothing before the yes"
    );
    let (again, again_name) = driver.asking().expect("the fresh press asks again");
    assert_eq!(again.action, "export");
    assert!(
        again_name.contains("Export"),
        "the ask names the action it asks about: {again_name:?}"
    );
    assert!(
        driver
            .confirm_asked_typing(Some("ERASE phrase-owner"))
            .is_some()
    );
    assert_eq!(starts.get(), 1, "fresh exact phrase admits once");
    assert_eq!(driver.asking(), None);
}

/// Press History's own control in the app, then let the window take the frame:
/// no frame runs between what the live tree queued and the press, so History
/// seals it instead of a frame dispatching it first.
fn press_history_control(driver: &mut Driver) {
    let target = driver
        .app_mut()
        .build_a11y_tree_update(1.0)
        .nodes
        .iter()
        .find(|(_, node)| node.label() == Some("History"))
        .map(|(id, _)| *id)
        .expect("History's own control is exported");
    driver
        .app_mut()
        .dispatch_a11y_action(target, &tilo_ui_base::a11y::Action::Activate);
    driver.relayout();
}

#[test]
fn retained_control_refuses_changed_target_handler_missing_or_ambiguous_identity() {
    let mutations: [fn(&MutableContinuationIdentity); 4] = [
        |identity| identity.target.set(2),
        |identity| identity.handler.set(2),
        |identity| identity.supported.set(false),
        |identity| identity.duplicate_resource.set(true),
    ];
    for mutate in mutations {
        let identity = Rc::new(MutableContinuationIdentity {
            target: Cell::new(1),
            handler: Cell::new(1),
            supported: Cell::new(true),
            duplicate_resource: Cell::new(false),
            housekeeping: Cell::new(false),
        });
        let (mut driver, starts, _emitter) = retained_identity_fixture(identity.clone());
        // A press of History's own control, before any frame the driver might
        // serve first: History takes the open request before it runs what the
        // live tree queued, so the control is sealed rather than dispatched.
        press_history_control(&mut driver);
        press_history_control(&mut driver);
        let item = driver.pending_history_controls().remove(0);
        let token = driver.pending_history_control_token(item.id).unwrap();
        mutate(&identity);
        let status = driver.continue_pending_history_control(&token).unwrap();
        assert!(
            matches!(
                status,
                tilo_ui::history_mode::pending_controls::PendingControlStatus::Refused(_)
            ),
            "changed or unsupported exact identity must remain visibly refused: {status:?}"
        );
        assert_eq!(starts.get(), 0);
        assert_eq!(driver.pending_history_controls().len(), 1);
    }
}

/// A person confirms each held action they asked for; housekeeping an app emits while History shows a
/// past version is never held, announced or confirmed, and does not run then: the app does it again on
/// its own.
#[test]
fn housekeeping_is_never_held_for_review() {
    let identity = Rc::new(MutableContinuationIdentity {
        target: Cell::new(1),
        handler: Cell::new(1),
        supported: Cell::new(true),
        duplicate_resource: Cell::new(false),
        housekeeping: Cell::new(true),
    });
    let (mut driver, starts, _emitter) = retained_identity_fixture(identity);
    driver.click_named("history-open").unwrap();
    assert!(
        driver.pending_history_controls().is_empty(),
        "housekeeping is not held while History shows"
    );
    driver.click_named("history-open").unwrap();
    assert!(driver.pending_history_controls().is_empty());
    assert!(!driver.targets().contains_key("history-pending-cancel-1"));
    assert_eq!(starts.get(), 0, "dropped, not run behind the person's back");
}

#[test]
fn actual_cancel_callback_removes_retained_item_and_future_live_control_runs() {
    let identity = Rc::new(MutableContinuationIdentity {
        target: Cell::new(1),
        handler: Cell::new(1),
        supported: Cell::new(true),
        duplicate_resource: Cell::new(false),
        housekeeping: Cell::new(false),
    });
    let (mut driver, starts, emitter) = retained_identity_fixture(identity);
    driver.click_named("history-open").unwrap();
    driver.click_named("history-open").unwrap();
    driver.click_named("history-pending-cancel-1").unwrap();
    assert!(driver.pending_history_controls().is_empty());
    assert_eq!(starts.get(), 0);

    emitter.borrow_mut().emit(()).unwrap();
    driver.key("x");
    assert_eq!(
        starts.get(),
        1,
        "ordinary future live control keeps its semantics"
    );
}
