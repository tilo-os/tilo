//! A run's day: what its keeper lets it spend, what the gateway charged,
//! and the run stopping before it plans again.

use super::*;

/// The turn's answer, priced as the model gateway charged it.
fn priced(events: Vec<TurnEvent>, cost_micros: u64) -> Vec<TurnEvent> {
    events
        .into_iter()
        .map(|event| match event {
            TurnEvent::Finished(Ok(body)) => {
                let mut answer: Value = serde_json::from_str(&body).unwrap();
                answer["cost_micros"] = serde_json::json!(cost_micros);
                TurnEvent::Finished(Ok(answer.to_string()))
            }
            other => other,
        })
        .collect()
}

/// What a run was charged, as its host heard it.
fn charges(sent: &[CollabRequest]) -> Vec<u64> {
    sent.iter()
        .filter_map(|request| match request {
            CollabRequest::ReportTaskRun {
                update: runtime_collab::TaskRunUpdate::Spent { cost_micros },
                ..
            } => Some(*cost_micros),
            _ => None,
        })
        .collect()
}

#[test]
fn a_run_stops_at_its_keepers_limit_for_the_day_and_says_so() {
    let mut f = fixture(
        "write a paragraph about the ocean",
        World::default(),
        vec![
            priced(
                turn(
                    &[invoke(
                        "append-paragraph",
                        &serde_json::json!({"text": "The ocean holds most of its life."}),
                    )],
                    8,
                ),
                3_000_000,
            ),
            turn(&[finish("Done.", true)], 8),
        ],
    );
    f.run.spend = Some(runtime_collab::TaskRunSpend {
        day: ui::clock::today().to_string(),
        limit_micros: Some(5_000_000),
        spent_micros: 4_000_000,
    });
    f.session.run_task(&f.run).unwrap();

    assert_eq!(
        f.script.bodies.lock().unwrap().len(),
        1,
        "the day's limit stops the next call before it is made"
    );
    let sent = f.sent.borrow();
    assert_eq!(charges(&sent), vec![3_000_000], "the run's total, once");
    assert_eq!(
        terminal(&sent),
        runtime_collab::TaskRunUpdate::Failed {
            message: "agent-1 has used today's $5; ask its keeper to raise it".into()
        }
    );
}

#[test]
fn a_run_without_a_limit_spends_unmetered() {
    let mut f = fixture(
        "write a paragraph about the ocean",
        World::default(),
        vec![
            priced(
                turn(
                    &[invoke(
                        "append-paragraph",
                        &serde_json::json!({"text": "The ocean holds most of its life."}),
                    )],
                    8,
                ),
                3_000_000,
            ),
            turn(&[finish("Done.", true)], 8),
        ],
    );
    f.run.spend = None;
    f.session.run_task(&f.run).unwrap();
    assert_eq!(
        f.script.bodies.lock().unwrap().len(),
        2,
        "no limit, no stop"
    );
    assert!(charges(&f.sent.borrow()).is_empty(), "nothing to count");
}
