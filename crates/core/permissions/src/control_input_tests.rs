//! What invoking a control needs. A widget publishes its own action, and that action's contract
//! says what it does: a button's effect that reaches outside asks a guest first. The verbs an
//! accessibility surface publishes for a control are the person's own pointer and keyboard, which
//! only they invoke, so they need what reading needs.

use super::*;
use tilo_actions::{ActionSpec, OperationKind, ResourceSpec};

#[test]
fn a_buttons_own_effect_keeps_its_ask_while_the_screens_value_verb_needs_only_reading() {
    let person = crate::MemberId::person("ad".repeat(32));
    let editor = PermissionContext::member(&person, Role::Editor);
    let app = "so.tilo.notes";

    // A control's own effect reaches outside the workspace, so a guest editor is asked first.
    let effect = ActionSpec::operation(
        "invoke",
        "Send",
        OperationKind::Effect,
        "tilo.ui.control.invoke",
        &(),
    )
    .unwrap();
    assert_eq!(effect.contract.need().kind, ActionKind::Outside);
    let control =
        ResourceSpec::new("tilo.control:send", "tilo.control", "Send").with_action(effect.clone());
    assert_eq!(
        editor.invocation_answer(app, &control, &effect, None, None),
        Answer::Ask
    );

    // The accessibility surface's value verb is the person typing into a field. Only the person
    // at the device invokes it, and what the field writes is decided as those writes are, so the
    // guest is not asked as if the field reached out.
    let value = ActionSpec::operation(
        "set",
        "Set",
        OperationKind::Effect,
        tilo_actions::SEMANTIC_OPERATION_TYPE,
        &(),
    )
    .unwrap();
    assert_eq!(value.contract.need().kind, ActionKind::Outside);
    let screen =
        ResourceSpec::new("ui:title", "tilo.ui.control", "Title").with_action(value.clone());
    assert_eq!(
        editor.invocation_answer(app, &screen, &value, None, None),
        Answer::Allow
    );
}
