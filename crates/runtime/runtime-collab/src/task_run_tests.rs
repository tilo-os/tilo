use super::{TaskRunEnvelope, TaskRunPlanning, TaskRunUpdate};

#[test]
fn task_run_wire_is_typed_and_rejects_unknown_fields() {
    let envelope = TaskRunEnvelope {
        version: 1,
        task_id: "task:1".into(),
        title: "Review proposal".into(),
        run_id: 7,
        lease: "broker-lease".into(),
        planning: TaskRunPlanning::RulesOnly,
        execution_budget: None,
        action_context: serde_json::json!({"purpose": "Review proposal"}),
    };
    let json = serde_json::to_string(&envelope).unwrap();
    assert_eq!(
        serde_json::from_str::<TaskRunEnvelope>(&json).unwrap(),
        envelope
    );
    assert!(serde_json::from_str::<TaskRunEnvelope>(
        r#"{"version":1,"task_id":"t","title":"x","run_id":1,"lease":"l","planning":"rules_only","action_context":{},"path":"/tmp"}"#
    )
    .is_err());
    assert!(serde_json::from_str::<TaskRunEnvelope>(
        r#"{"version":1,"task_id":"t","title":"x","run_id":1,"lease":"l","planning":"unattended","action_context":{}}"#
    )
    .is_err());
    assert_eq!(
        serde_json::from_str::<TaskRunUpdate>(r#"{"kind":"progress","message":"checking"}"#)
            .unwrap(),
        TaskRunUpdate::Progress {
            message: "checking".into()
        }
    );
}
