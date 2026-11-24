//! A trigger of this device fires the shortcuts that name it, once per switch, and never on the
//! first look, which only learns what is so.

use super::*;
use serde_json::json;
use tilo_automation::Shortcut;

fn armed_with(trigger: &TriggerSpec) -> Armed {
    let shortcut: Shortcut = serde_json::from_value(json!({
        "id": "tidy",
        "name": "Tidy",
        "inputs": [{"name": "app_id", "ty": "text", "description": "The app"}],
        "steps": [],
        "triggers": [trigger],
    }))
    .expect("shortcut JSON");
    let workspace = tilo_desktop_model::WorkspaceId::new();
    Armed {
        workspace: workspace.to_string(),
        shortcut,
        saver: "person".into(),
        agent: None,
        starter: [1; 32],
        described: tilo_desktop_model::Workspace::new(workspace, "Home".to_owned()),
    }
}

#[test]
fn a_chord_fires_the_shortcuts_that_name_it() {
    let armed = [armed_with(&TriggerSpec::Keyboard {
        chord: "Super+Shift+N".into(),
    })];
    let pressed = KeyChord::parse("Super+Shift+N").expect("a chord");
    let other = KeyChord::parse("Super+Shift+M").expect("a chord");
    let fired = chord_pressed(&armed, &pressed);
    assert_eq!(fired.len(), 1, "{fired:?}");
    assert_eq!(fired[0].saver, "person", "it starts for its saver");
    assert!(chord_pressed(&armed, &other).is_empty());
}

/// The device's dispatch as a pressed chord reaches it: every call it is sent, and who sent it.
#[derive(Default)]
struct Dispatch(std::sync::Mutex<Vec<(tilo_actions::remote::Caller, InvokeRequest)>>);

impl ActionProvider for Dispatch {
    fn query(
        &self,
        _: &tilo_actions::remote::Caller,
        _: tilo_actions::remote::CatalogueQuery,
    ) -> tilo_actions::remote::Pending<
        Result<tilo_actions::remote::CataloguePage, tilo_actions::remote::ActionCallError>,
    > {
        tilo_actions::remote::Pending::ready(Err(
            tilo_actions::remote::ActionCallError::UnknownAction,
        ))
    }

    fn invoke(
        &self,
        caller: &tilo_actions::remote::Caller,
        request: InvokeRequest,
    ) -> tilo_actions::remote::Pending<
        Result<tilo_actions::remote::InvocationReceipt, tilo_actions::remote::ActionCallError>,
    > {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((caller.clone(), request));
        tilo_actions::remote::Pending::ready(Err(
            tilo_actions::remote::ActionCallError::NotPermitted,
        ))
    }

    fn observe(
        &self,
        _: &tilo_actions::remote::Caller,
        _: &InvocationId,
    ) -> Option<tilo_actions::remote::InvocationState> {
        None
    }

    fn cancel(
        &self,
        _: &tilo_actions::remote::Caller,
        _: &InvocationId,
    ) -> tilo_actions::remote::Pending<Result<(), tilo_actions::remote::ActionCallError>> {
        tilo_actions::remote::Pending::ready(Ok(()))
    }
}

/// A pressed chord runs its shortcut through the device's dispatch, as the person who saved it
/// in its workspace, under the firing's id: the call the dispatch keeps as the shortcut's asked
/// record when a step needs their yes. It says the key started it, so its history does too.
#[test]
fn a_pressed_chord_runs_its_shortcut_through_the_dispatch_as_its_saver() {
    let pressed = TriggerSpec::Keyboard {
        chord: "Super+Shift+N".into(),
    };
    let armed = [armed_with(&pressed)];
    let fired = chord_pressed(&armed, &KeyChord::parse("Super+Shift+N").expect("a chord"));
    let dispatch = Dispatch::default();
    run_fired(&dispatch, &fired[0]);
    let calls = dispatch
        .0
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    assert_eq!(calls.len(), 1, "one press, one call");
    let (caller, call) = &calls[0];
    assert_eq!(
        tilo_workspace_service::apps::automation::run_target(&call.target.address),
        Some("tidy"),
        "the shortcut's run"
    );
    assert_eq!(
        call.invocation.0, fired[0].invocation.0,
        "under the firing's id"
    );
    assert_eq!(caller.workspace(), Some(armed[0].workspace.as_str()));
    assert_eq!(caller.acting().person_id(), Some("person"), "as its saver");
    let shortcut: Shortcut = serde_json::from_value(json!({
        "id": "tidy", "name": "Tidy", "steps": [], "triggers": [pressed],
    }))
    .expect("shortcut JSON");
    assert_eq!(
        tilo_workspace_service::apps::automation::run_trigger(call.context.as_ref(), &shortcut),
        tilo_automation::InvocationTrigger::Fired(pressed),
        "the key that started it, chord and all"
    );
}

#[test]
fn an_app_opened_fires_once_and_never_on_the_first_look() {
    let armed = [armed_with(&TriggerSpec::AppOpened {
        app_id: "so.tilo.notes".into(),
    })];
    let notes: BTreeSet<String> = BTreeSet::from(["so.tilo.notes".to_owned()]);
    assert!(
        apps_opened(&armed, None, &notes).is_empty(),
        "an app running at the first look was not opened then"
    );
    let fired = apps_opened(&armed, Some(BTreeSet::new()), &notes);
    assert_eq!(fired.len(), 1, "{fired:?}");
    assert_eq!(fired[0].inputs.get("app_id"), Some(&json!("so.tilo.notes")));
    assert!(
        apps_opened(&armed, Some(notes.clone()), &notes).is_empty(),
        "still running"
    );
}

#[test]
fn a_focus_mode_fires_on_the_switch_it_names() {
    let armed = [armed_with(&TriggerSpec::FocusMode {
        mode: FOCUS_DO_NOT_DISTURB.into(),
        transition: FocusTransition::Enter,
    })];
    let off = FocusState::default();
    let on = FocusState {
        do_not_disturb: true,
        focus: false,
    };
    assert!(
        focus_switched(&armed, None, on).is_empty(),
        "the first look only learns"
    );
    assert_eq!(focus_switched(&armed, Some(off), on).len(), 1);
    assert!(
        focus_switched(&armed, Some(on), off).is_empty(),
        "leaving is not entering"
    );
    assert!(
        focus_switched(&armed, Some(on), on).is_empty(),
        "nothing switched"
    );
}
