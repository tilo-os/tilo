//! Owner changes during the actual Session planning pipeline.
use super::*;

#[test]
fn steer_during_model_response_discards_old_mutation_arguments() {
    let mut f = native_run_fixture(
        "Review request",
        World::default(),
        vec![
            turn(
                &[invoke(
                    "append-paragraph",
                    &serde_json::json!({"text":"obsolete mutation"}),
                )],
                24,
            ),
            turn(
                &[invoke(
                    "append-paragraph",
                    &serde_json::json!({"text":"revised mutation"}),
                )],
                24,
            ),
            turn(&[finish("Updated", true)], 24),
        ],
    );
    let mut changed = f.world.borrow().native_control.clone().unwrap();
    changed.latest_revision = 1;
    changed.directions = vec![runtime_collab::TaskDirection {
        revision: 1,
        author: "person".into(),
        text: "Use revised mutation instead".into(),
        issued_at: 1,
    }];
    f.world.borrow_mut().control_after_model = Some(changed);
    f.session.run_task(&f.run).unwrap();
    let world = f.world.borrow();
    assert_eq!(world.steps.len(), 1);
    assert_eq!(world.steps[0].1["text"], "revised mutation");
    assert!(
        !f.script.bodies.lock().unwrap()[0]
            .to_string()
            .contains("Use revised")
    );
    assert!(
        f.script.bodies.lock().unwrap()[1]
            .to_string()
            .contains("Use revised")
    );
    assert!(matches!(
        native_terminal(&f.sent.borrow()),
        runtime_collab::TaskRunUpdate::Succeeded { .. }
    ));
}

#[test]
fn failed_response_never_marks_prepared_directions_applied() {
    let mut f = native_run_fixture(
        "Review request",
        World::default(),
        vec![vec![TurnEvent::Finished(Err(
            "model transport failed".into()
        ))]],
    );
    {
        let mut world = f.world.borrow_mut();
        let control = world.native_control.as_mut().unwrap();
        control.latest_revision = 1;
        control.directions.push(runtime_collab::TaskDirection {
            revision: 1,
            author: "person".into(),
            text: "Saved direction".into(),
            issued_at: 1,
        });
    }
    f.session.run_task(&f.run).unwrap();
    assert!(
        f.script.bodies.lock().unwrap()[0]
            .to_string()
            .contains("Saved direction")
    );
    assert_eq!(f.world.borrow().native_applied_revision, 0);
    assert!(!f.sent.borrow().iter().any(|request| matches!(
        request,
        CollabRequest::ReportTaskRun {
            update: runtime_collab::TaskRunUpdate::DirectionsApplied { .. },
            ..
        }
    )));
}
