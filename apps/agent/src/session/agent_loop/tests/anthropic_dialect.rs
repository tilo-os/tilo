//! The Anthropic dialect of the scripted legacy test transport: the same
//! action loop with `tool_use`/`tool_result` blocks, and the catalogue's
//! own cache breakpoint.

use super::*;

/// Both dialects wait for the validated final batch, execute each call
/// exactly once and return each result under the original call id.
#[test]
fn anthropic_calls_wait_for_the_complete_turn_and_run_only_once() {
    let first = anthropic_turn(
        &[
            invoke("append-paragraph", &serde_json::json!({"text": "Hello"})),
            invoke("read-text", &serde_json::json!({})),
        ],
        8,
    );
    let turn_complete = first
        .iter()
        .position(|event| matches!(event, TurnEvent::Finished(_)))
        .unwrap();
    let mut f = paced_fixture(
        "write hello",
        World::default(),
        vec![first, anthropic_turn(&[finish("Wrote it.", true)], 8)],
        Some(Duration::from_millis(4)),
    );
    f.session.chooser = Chooser::scripted_dialect(
        Duration::from_secs(5),
        agent_provider_config::ProviderProtocol::Anthropic,
        Arc::new(Scripted(f.script.clone())),
    );
    f.session.run_task(&f.run).unwrap();

    let world = f.world.borrow();
    assert_eq!(world.blocks[0].1, "Hello");
    let actions = world
        .steps
        .iter()
        .map(|(action, _)| action.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        actions,
        ["append-paragraph", "read-text"],
        "each call ran once"
    );
    assert!(
        world.steps_at[0] >= turn_complete,
        "the first call ran at event {} before the final response ({turn_complete})",
        world.steps_at[0]
    );
    let bodies = f.script.bodies.lock().unwrap().clone();
    assert_eq!(bodies.len(), 2);
    assert!(bodies[0]["tools"][0].get("input_schema").is_some());
    let results = bodies[1]["messages"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    assert_eq!(results["role"], "user");
    let ids = results["content"]
        .as_array()
        .unwrap()
        .iter()
        .map(|block| {
            assert_eq!(block["type"], "tool_result");
            assert_ne!(block["content"], "Not handled.");
            block["tool_use_id"].as_str().unwrap().to_owned()
        })
        .collect::<Vec<_>>();
    assert_eq!(ids, ["toolu_00", "toolu_01"]);
    assert_eq!(
        terminal(&f.sent.borrow()),
        runtime_collab::TaskRunUpdate::Succeeded {
            message: "Wrote it.".into()
        }
    );
}

/// Anthropic caches only up to a breakpoint, so the catalogue closes a
/// text block of its own with one: the first request of two different
/// tasks is byte-identical through that block (system, tools,
/// catalogue), and the next task reads it back from the cache instead of
/// prefilling it. The request follows in its own block.
#[test]
fn anthropic_tasks_share_the_cached_catalogue_block() {
    let mut f = fixture(
        "ignored",
        World::default(),
        vec![
            anthropic_turn(
                &[invoke_replying(
                    "type-text",
                    &serde_json::json!({"text": "Hello chat."}),
                    "Posted hello.",
                )],
                64,
            ),
            anthropic_turn(&[(tools::LIST_APPLICATIONS, serde_json::json!({}))], 64),
            anthropic_turn(
                &[finish("I typed \"Hello chat.\" into the document.", true)],
                64,
            ),
        ],
    );
    f.session.chooser = Chooser::scripted_dialect(
        Duration::from_secs(5),
        agent_provider_config::ProviderProtocol::Anthropic,
        Arc::new(Scripted(f.script.clone())),
    );
    for title in ["post hello", "what did you just do?"] {
        let mut run = f.run.clone();
        run.title = title.into();
        bind_context(&mut run);
        f.session.run_task(&run).unwrap();
    }
    let bodies = f.script.bodies.lock().unwrap().clone();
    assert_eq!(bodies.len(), 3);
    // What Anthropic caches at the catalogue's breakpoint, in its render
    // order: tools, system, then the first user message through it.
    let prefix = |body: &Value| {
        serde_json::to_string(&(
            &body["tools"],
            &body["system"],
            &body["messages"][0]["content"][0],
        ))
        .unwrap()
    };
    for body in &bodies {
        assert_eq!(prefix(body), prefix(&bodies[0]));
        // The agent's one breakpoint; the gateway adds the rest.
        assert_eq!(body.to_string().matches("cache_control").count(), 1);
    }
    let first_turn = |body: &Value| body["messages"][0]["content"].as_array().unwrap().clone();
    let (first, second) = (first_turn(&bodies[0]), first_turn(&bodies[1]));
    assert_eq!(first.len(), 2);
    assert_eq!(second.len(), 2);
    let catalogue = &first[0];
    assert_eq!(catalogue["type"], "text");
    assert_eq!(
        catalogue["cache_control"],
        serde_json::json!({"type": "ephemeral"})
    );
    let catalogue = catalogue["text"].as_str().unwrap();
    assert!(
        catalogue.starts_with("Operations published right now:")
            && catalogue.contains(&op("type-text")),
        "{catalogue}"
    );
    assert!(!catalogue.contains("The request:"), "{catalogue}");
    assert!(first[1].get("cache_control").is_none());
    // What the task is and where it is come after the catalogue, in the block that changes.
    let task = first[1]["text"].as_str().unwrap();
    assert!(
        !task.contains("Limits:"),
        "a run nobody limited is told no limits: {task}"
    );
    assert!(task.ends_with("The request:\n\npost hello"), "{task}");
    let task = second[1]["text"].as_str().unwrap();
    assert!(
        task.ends_with("The request:\n\nwhat did you just do?"),
        "{task}"
    );
    // Within a task, the first turn is replayed unchanged.
    assert_eq!(bodies[2]["messages"][0], bodies[1]["messages"][0]);
}
