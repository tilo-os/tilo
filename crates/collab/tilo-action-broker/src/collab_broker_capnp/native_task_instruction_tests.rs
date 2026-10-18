//! Private dispatch must bind the actual body, not merely carry a public reference.
use super::bound_private_instruction;
use crate::services::NativeTaskPrivateInstructions;
use tilo_desktop_model::WorkspaceTaskControl;

#[test]
fn bound_instruction_requires_exact_owner_private_body() {
    let body = "Keep the complete private goal.\nIncluding this second line.\n";
    let control = WorkspaceTaskControl {
        private_instruction_ref: Some(
            tilo_peer_state::workspace_task::private_instruction_reference(body),
        ),
        ..Default::default()
    };
    assert!(bound_private_instruction(&control, None, "Task").is_err());
    let mut private = NativeTaskPrivateInstructions {
        instruction: Some(body.into()),
        agent_request: None,
        directions: Default::default(),
    };
    assert_eq!(
        bound_private_instruction(&control, Some(&private), "Task").unwrap(),
        body
    );
    private.instruction = Some(body.trim_end().into());
    assert!(bound_private_instruction(&control, Some(&private), "Task").is_err());
}

#[test]
fn public_control_cannot_supply_private_instruction_fallback() {
    let control = WorkspaceTaskControl {
        instruction: Some("private legacy instruction".into()),
        ..Default::default()
    };
    assert!(bound_private_instruction(&control, None, "Public title").is_err());
    assert_eq!(
        bound_private_instruction(&WorkspaceTaskControl::default(), None, "Public title").unwrap(),
        "Public title",
    );
}
