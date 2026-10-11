// Component-only owner-shaped observations, never a seeded live/model path.

fn observed_tasks(host: &TaskControlHost, applied: u64, executing: bool) {
    use ui::agent_task_frontier::{WorkspaceTaskExecution, WorkspaceTaskExecutionPhase};
    let mut task = serde_json::json!({
        "task_id": "task-current", "title": "Review the release",
        "assigned_agent": "scribe", "active_agent": "scribe",
        "status": "in_progress", "run_epoch": 7,
        "after_revision": 5, "applied_revision": applied,
        "route": "model", "cancellation_requested": false
    });
    if executing {
        task["execution"] = serde_json::to_value(WorkspaceTaskExecution {
            run_epoch: 7,
            sequence: 2,
            phase: WorkspaceTaskExecutionPhase::Executing,
            updated_at: 12,
        })
        .unwrap();
    }
    let mut reads = host.visual_task_reads.lock().unwrap();
    reads.insert("read-task".into(), {
        let mut read = task.clone();
        read["generation"] = serde_json::json!(9 + applied);
        read
    });
    reads.insert(
        "read-current-tasks".into(),
        serde_json::json!({
            "generation": 9 + applied, "complete": true, "total": 1, "tasks": [task],
            "pending_complete": true, "pending_total": 1,
            "pending": [{"task_id":"task-queued", "title":"Check accessibility",
                "assigned_agent":"scribe", "status":"queued", "route":"model"}]
        }),
    );
    drop(reads);
}

fn execution_presence() -> ui::presence::PresenceCtx {
    let mut presence = ui::presence::PresenceCtx::empty();
    presence.apply_test_collaboration_snapshot(&[
        ui::collaboration::Collaborator {
            id: "u_alice".into(),
            name: "Alice".into(),
            colour: None,
            is_agent: false,
            is_self: true,
        },
        ui::collaboration::Collaborator {
            id: "agent:scribe".into(),
            name: "Scribe".into(),
            colour: None,
            is_agent: true,
            is_self: false,
        },
    ]);
    presence
}

fn execution_fixture() -> (ChatApp, AutoCommit, Arc<TaskControlHost>) {
    let mut doc = AutoCommit::new();
    ChatDoc::seed_starter(&mut doc, "u_alice", "Alice", "AL");
    let mut app = peer("u_alice", "Alice", "AL", 0);
    app.adopt_room(&execution_presence());
    let host = Arc::new(TaskControlHost::default());
    app.agent_task_actions = ui::remote_actions::RemoteActions::with_host(
        Arc::clone(&host) as Arc<dyn ActionHost + Send + Sync>,
        Caller::local_app("so.tilo.chat").in_room("ws_home"),
    );
    app.reload(&doc);
    app.on_hit(
        Hit::Agent("scribe".into()),
        &mut ui::widget::Edits::lend(&mut doc),
    );
    app.poll_agent_tasks();
    app.on_hit(
        Hit::AgentIntent(AgentComposerIntent::Steer),
        &mut ui::widget::Edits::lend(&mut doc),
    );
    type_str(&mut app, &mut doc, "Include keyboard evidence");
    app.on_hit(Hit::Send, &mut ui::widget::Edits::lend(&mut doc));
    app.poll_agent_tasks();
    assert_eq!(
        host.requests()
            .iter()
            .filter(|request| request.target.address.action == "direct")
            .count(),
        1
    );
    type_str(&mut app, &mut doc, "Also check compact navigation");
    app.composer_main.select(5, 10);
    (app, doc, host)
}

fn assert_observation(app: &ChatApp, applied: bool, executing: bool) {
    use ui::agent_task_frontier::AgentDirectionState;
    let tasks = app
        .direct_agent_tasks()
        .expect("original Agent task context");
    let tasks = tasks.borrow();
    let expected = if applied {
        AgentDirectionState::Applied {
            task_id: "task-current".into(),
            run_epoch: 7,
            revision: 5,
        }
    } else {
        AgentDirectionState::Saved {
            task_id: "task-current".into(),
            run_epoch: 7,
            revision: 5,
        }
    };
    assert_eq!(tasks.direction_state(), &expected);
    let status = crate::render_chat::agent_task_status_lines(&tasks, false);
    assert_eq!(
        status.primary,
        if executing {
            "Executing: Review the release"
        } else {
            "Status unknown: Review the release"
        }
    );
    assert_eq!(
        status.receipt.as_deref(),
        Some(if applied {
            "Direction applied"
        } else {
            "Direction saved"
        })
    );
    assert_eq!(status.queue.as_deref(), Some("1 queued task"));
}

fn write_execution_frame(
    app: &mut ChatApp,
    size: &str,
    state: &str,
    name: &str,
    theme: &ui::Theme,
) {
    let Some(output) = std::env::var_os("TILO_CHAT_EXECUTION_SHOTS") else {
        return;
    };
    let output = std::path::PathBuf::from(output);
    std::fs::create_dir_all(&output).unwrap();
    let mut scene = vello::Scene::new();
    let mut text = ui::text_render::TextRenderer::new();
    text.set_text_scale(theme.text_scale);
    app.draw(
        &mut scene,
        vello::kurbo::Affine::IDENTITY,
        &mut text,
        theme,
        &execution_presence(),
    );
    let width = u32::try_from(ui::num::usize_of_f64(app.rect.width())).unwrap();
    let height = u32::try_from(ui::num::usize_of_f64(app.rect.height())).unwrap();
    ui::offscreen::OffscreenRenderer::new(width, height).render_png(
        &scene,
        theme.tokens.colors.surfaces.content,
        output.join(format!("{state}-{size}-{name}.png")),
    );
}

fn write_native_execution_frame(
    app: &mut ChatApp,
    id: &str,
    name: &str,
    theme: &ui::Theme,
    presence: &ui::presence::PresenceCtx,
) {
    let Some(output) = std::env::var_os("TILO_CHAT_EXECUTION_SHOTS") else {
        return;
    };
    let (_, content) = native_agent_float_frame(app, id, theme, presence);
    let mut scene = vello::Scene::new();
    let mut text = ui::text_render::TextRenderer::new();
    text.set_text_scale(theme.text_scale);
    assert!(ui::widget::FloatingSurfaceWidgetExt::draw_floating_surface(
        app,
        id,
        &mut scene,
        vello::kurbo::Affine::translate(-content.origin().to_vec2()),
        &mut text,
        theme,
        presence,
    ));
    let width = u32::try_from(ui::num::usize_of_f64(content.width())).unwrap();
    let height = u32::try_from(ui::num::usize_of_f64(content.height())).unwrap();
    ui::offscreen::OffscreenRenderer::new(width, height).render_png(
        &scene,
        theme.tokens.colors.surfaces.content,
        std::path::PathBuf::from(output)
            .join(format!("executing-applied-detached-compact-{name}.png")),
    );
}

#[test]
fn actual_detached_executing_and_applied_keep_task_receipts_and_private_steer_editor() {
    for (name, theme) in [
        ("light", ui::Theme::light()),
        ("dark", ui::Theme::dark()),
        (
            "high-contrast-200",
            ui::Theme::high_contrast().with_text_scale(2.0),
        ),
    ] {
        let (mut app, mut doc, host) = execution_fixture();
        let presence = execution_presence();
        render_frame_with_theme_and_presence(&mut app, 1600.0, 900.0, &theme, &presence);
        let draft = app.composer_main.private_plain();
        let selection = app.composer_main.selection();
        for (state, executing, applied) in [
            ("reference-saved", false, false),
            ("executing-saved", true, false),
            ("executing-applied", true, true),
        ] {
            observed_tasks(&host, if applied { 5 } else { 4 }, executing);
            app.direct_agent_tasks().unwrap().borrow_mut().refresh();
            app.poll_agent_tasks();
            assert_observation(&app, applied, executing);
            write_execution_frame(&mut app, "normal", state, name, &theme);
        }
        // This is the actual compact host allocation from the native journey,
        // not a primary Chat render with a compact filename.
        render_frame_with_theme_and_presence(&mut app, 344.0, 472.0, &theme, &presence);
        ui::widget::FloatingSurfaceWidgetExt::set_floating_surface_bounds(
            &mut app,
            vello::kurbo::Rect::new(0.0, 0.0, 800.0, 600.0),
        );
        app.on_hit(Hit::AgentWindow, &mut ui::widget::Edits::lend(&mut doc));
        let request =
            ui::widget::FloatingSurfaceWidgetExt::take_floating_surface_request(&mut app).unwrap();
        let rect = request.initial_rect.unwrap();
        adopt_agent_surface(&mut app, &request.id, rect);
        tilo_test_env::wait_for("the actual compact conversation settles", || {
            if app
                .agent_surfaces
                .values()
                .any(|surface| surface.placement.is_handoff_animating())
            {
                // Resolving the presentation arms the production settle clock.
                ui::widget::FloatingSurfaceWidgetExt::floating_surface_presentations(&app);
                return None;
            }
            ui::widget::FloatingSurfaceWidgetExt::floating_surface_presentations(&app)
                .into_iter()
                .find(|p| p.id == request.id && p.content_rect.size() == request.preferred_size)
        });
        let (scene, content) = native_agent_float_frame(&mut app, &request.id, &theme, &presence);
        assert!(!scene.encoding().is_empty());
        assert!(content.width() <= 344.0 && content.height() <= 472.0);
        let channel = ChatDoc::agent_conversation_channel_id("ws_home", "scribe");
        let view = &mut app.agent_surfaces.get_mut(&channel).unwrap().view;
        assert_observation(view, true, true);
        assert_eq!(view.composer_main.private_plain(), draft);
        assert_eq!(view.composer_main.selection(), selection);
        assert_hit_in_view(view, "Steer editor", |hit| matches!(hit, Hit::FocusMain));
        assert_hit_in_view(view, "Save direction", |hit| matches!(hit, Hit::Send));
        write_native_execution_frame(&mut app, &request.id, name, &theme, &presence);
    }
}
