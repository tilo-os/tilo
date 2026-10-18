//! Paint provenance is exercised from actual native admission and broker fanout.
use super::*;
use ui::vello;

fn assert_original_paint(f: &Fixture, original: &tilo_actions::SemanticInteraction) {
    assert!(original.paint_origin.is_some());
    // Render the actually admitted interaction through the production painter.
    // Merely serializing invented geometry would not exercise its visibility gate.
    let mut presence = ui::PresenceCtx::empty();
    presence.apply_interaction(original.clone()).unwrap();
    let mut scene = vello::Scene::new();
    let mut text = ui::text_render::TextRenderer::new();
    let theme = ui::Theme::light();
    let mut paint = ui::paint::PaintCtx::new(
        &mut scene,
        vello::kurbo::Affine::IDENTITY,
        &mut text,
        &theme,
    );
    let geometry = presence
        .draw_agent_action_marker(
            &mut paint,
            original,
            vello::kurbo::Rect::new(80.0, 80.0, 220.0, 120.0),
            vello::kurbo::Rect::new(0.0, 0.0, 800.0, 600.0),
            "notes:fixture",
            ui::time::now(),
        )
        .expect("an actually admitted interaction paints its real marker");
    assert!(geometry.bounds.area() > 0.0);
    // The SDK's actual painted metadata transports the correlation intact.
    let painted = ui::PaintedActionMarker {
        interaction: original.clone(),
        viewport: [0.0, 0.0, 800.0, 600.0],
        marker: [10.0, 10.0, 30.0, 30.0],
        locus: [10.0, 30.0, 90.0, 60.0],
        protected: [10.0, 10.0, 90.0, 60.0],
    };
    let painted: ui::PaintedActionMarker =
        serde_json::from_slice(&serde_json::to_vec(&painted).unwrap()).unwrap();
    let resolve =
        |room: &str, app: &str, actor: &str, interaction: &tilo_actions::SemanticInteraction| {
            f.state
                .certified_painted_task(room, app, actor, interaction)
        };
    let room_id = room().to_string();
    assert_eq!(
        resolve(&room_id, "so.tilo.notes", "scribe", &painted.interaction),
        Some((f.task.clone(), 1))
    );
    for (room, app, agent) in [
        ("other-room", "so.tilo.notes", "scribe"),
        (room_id.as_str(), "so.tilo.chat", "scribe"),
        (room_id.as_str(), "so.tilo.notes", "other-agent"),
    ] {
        assert!(resolve(room, app, agent, original).is_none());
    }
    let mut missing = original.clone();
    missing.paint_origin = None;
    assert!(resolve(&room_id, "so.tilo.notes", "scribe", &missing).is_none());
    let mut forged = original.clone();
    forged.paint_origin = Some(uuid::Uuid::new_v4());
    assert!(resolve(&room_id, "so.tilo.notes", "scribe", &forged).is_none());
    let mut foreign = original.clone();
    foreign.resource.id = "notes:other".into();
    assert!(resolve(&room_id, "so.tilo.notes", "scribe", &foreign).is_none());
    foreign = original.clone();
    foreign.invocation = tilo_actions::InvocationId::new();
    assert!(resolve(&room_id, "so.tilo.notes", "scribe", &foreign).is_none());
    foreign = original.clone();
    foreign.group.as_mut().unwrap().id = "task:other".into();
    assert!(resolve(&room_id, "so.tilo.notes", "scribe", &foreign).is_none());
}

async fn paint_separate_task(
    f: &Fixture,
    agent: &channel_handle::Client,
    replacement: &runtime_collab::TaskRunEnvelope,
) -> String {
    // The same Agent can also take a different task. Correlation must
    // remain task-and-run exact, not an Agent's latest-work pointer.
    report(
        agent,
        replacement,
        runtime_collab::TaskRunUpdate::Failed {
            message: "Replacement completed its attempt".into(),
        },
    )
    .await
    .unwrap();
    f.person_command(WorkspaceTaskCommand::QueueInstruction {
        title: "Separate task".into(),
        instruction: "Append another note".into(),
        assigned_agent: "scribe".into(),
        route: tilo_desktop_model::WorkspaceTaskRoute::Model,
    })
    .unwrap();
    let other = f
        .native
        .owner
        .workspace_task_snapshot(room())
        .unwrap()
        .tasks
        .into_iter()
        .find(|task| task.id != f.task)
        .unwrap()
        .id;
    f.person_command(WorkspaceTaskCommand::ApproveRun {
        task_id: other.clone(),
        agent_id: "scribe".into(),
        expected_run_epoch: 0,
        after_revision: 0,
    })
    .unwrap();
    let other_run = delivered(agent).await;
    assert_eq!(other_run.task_id, other);
    invoke(agent, &other_run, uuid::Uuid::new_v4())
        .await
        .unwrap();

    other
}

async fn retry_run(
    f: &Fixture,
    agent: &channel_handle::Client,
    run: &runtime_collab::TaskRunEnvelope,
) -> DeliveredRun {
    report(
        agent,
        run,
        runtime_collab::TaskRunUpdate::Failed {
            message: "Retry required".into(),
        },
    )
    .await
    .unwrap();
    f.person_command(WorkspaceTaskCommand::Retry {
        task_id: f.task.clone(),
        run_epoch: 1,
    })
    .unwrap();
    f.person_command(WorkspaceTaskCommand::ApproveRun {
        task_id: f.task.clone(),
        agent_id: "scribe".into(),
        expected_run_epoch: 1,
        after_revision: 0,
    })
    .unwrap();
    delivered(agent).await
}

#[tokio::test(flavor = "current_thread")]
async fn old_painted_origin_never_resolves_to_a_replacement_run() {
    Box::pin(tokio::task::LocalSet::new().run_until(async {
        let f = Fixture::new();
        f.approve();
        let agent = f.channel(100, true);
        let run = delivered(&agent).await;
        let (target, _stream) = notes_target(&f).await;
        let _context = target
            .subscribe_context_request()
            .send()
            .promise
            .await
            .unwrap();
        grant_notes(&f);
        let captured = Arc::new(Mutex::new(Vec::new()));
        let sink = captured.clone();
        let _subscription = f.fanout.subscribe_rooms(
            vec![room().to_string()],
            Box::new(move |event, _| {
                if let CollabEvent::RemotePresenceUpdate {
                    payload: PresencePayload::Interaction(interaction),
                    ..
                } = event
                {
                    sink.lock().unwrap().push(interaction.as_ref().clone());
                }
            }),
        );
        let invocation = uuid::Uuid::new_v4();
        invoke(&agent, &run, invocation).await.unwrap();
        let original = captured
            .lock()
            .unwrap()
            .last()
            .cloned()
            .expect("admission must publish presence");
        assert_original_paint(&f, &original);
        let resolve = |room: &str,
                       app: &str,
                       actor: &str,
                       interaction: &tilo_actions::SemanticInteraction| {
            f.state
                .certified_painted_task(room, app, actor, interaction)
        };
        let room_id = room().to_string();
        let replacement = retry_run(&f, &agent, &run).await;
        // A replacement run is a distinct invocation. Reusing the old
        // UUID correctly fails the global invocation identity fence.
        // First-arriving/reconnected old pixels still keep epoch one.
        let paints_before_conflict = captured.lock().unwrap().len();
        assert!(invoke(&agent, &replacement, invocation).await.is_err());
        assert_eq!(captured.lock().unwrap().len(), paints_before_conflict);
        let replacement_invocation = uuid::Uuid::new_v4();
        assert_ne!(replacement_invocation, invocation);
        invoke(&agent, &replacement, replacement_invocation)
            .await
            .unwrap();
        let newer = captured.lock().unwrap().last().cloned().unwrap();
        assert_ne!(newer.paint_origin, original.paint_origin);
        assert_eq!(
            resolve(&room_id, "so.tilo.notes", "scribe", &newer),
            Some((f.task.clone(), 2))
        );
        assert_eq!(
            resolve(&room_id, "so.tilo.notes", "scribe", &original),
            Some((f.task.clone(), 1))
        );
        let other = paint_separate_task(&f, &agent, &replacement).await;
        let other_paint = captured.lock().unwrap().last().cloned().unwrap();
        assert_eq!(
            resolve(&room_id, "so.tilo.notes", "scribe", &other_paint),
            Some((other, 1))
        );
        assert_eq!(
            resolve(&room_id, "so.tilo.notes", "scribe", &original),
            Some((f.task.clone(), 1))
        );
        assert_eq!(
            resolve(&room_id, "so.tilo.notes", "scribe", &newer),
            Some((f.task.clone(), 2))
        );

        // Losing retained evidence refuses rather than selecting current work.
        f.state.painted_task_origins.lock().unwrap().clear();
        assert!(resolve(&room_id, "so.tilo.notes", "scribe", &original).is_none());
        assert!(resolve(&room_id, "so.tilo.notes", "scribe", &newer).is_none());
        drop(f);
    }))
    .await;
}
