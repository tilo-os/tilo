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
