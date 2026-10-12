//! Actual enrolled owner, signed agent admission, async durable task commands.
use super::*;
use crate::shell::native_workspace_owner::{self, NativeWorkspaceOwner};
use std::time::{Duration, SystemTime};
use tilo_desktop::activity::TaskReviewCommand;
use tilo_peer_state::owner_commit::{CommitOperation, OwnerCommitRequest};
use tilo_peer_state::workspace_task::{
    WorkspaceTaskAuthority, WorkspaceTaskCommand, WorkspaceTaskCommandId,
};

fn admit(owner: &NativeWorkspaceOwner, room: crate::shell::state::RoomId) {
    let key = ed25519_dalek::SigningKey::from_bytes(&[61; 32]);
    let now = SystemTime::now();
    let messages = owner
        .with_current(room, |view| {
            let before = view.peer().clone();
            let mut next = before.clone();
            next.put_governed_grants(
                &tilo_action_policy::roles::AgentPreset::Operator.grants(
                    &room.to_string(),
                    "scribe",
                    &tilo_action_policy::Subject::Role(tilo_action_policy::Role::Owner),
                ),
                &key,
                now,
            )?;
            next.durable_mutation_messages(
                &before,
                &key,
                now,
                view.generation().checked_add(1).unwrap(),
            )
        })
        .unwrap();
    for message in messages {
        owner
            .receive(
                room,
                tilo_peer_state::TransportOrigin::RetainedRelayDelta(
                    key.verifying_key().to_bytes(),
                ),
                message,
                now,
            )
            .unwrap();
    }
}
fn task(
    owner: &NativeWorkspaceOwner,
    room: crate::shell::state::RoomId,
    caller: &tilo_user_identity::DeviceCert,
) -> String {
    let task_id = queued_native_task(owner, room, caller);
    owner
        .commit_workspace_task_command(
            OwnerCommitRequest {
                room: room.to_string(),
                operation: CommitOperation::WorkspaceTaskCommand {
                    invocation: uuid::Uuid::new_v4(),
                    authority: WorkspaceTaskAuthority::Person {
                        caller: caller.clone(),
                    },
                    command: WorkspaceTaskCommand::ApproveRun {
                        task_id: task_id.clone(),
                        agent_id: "scribe".into(),
                        expected_run_epoch: 0,
                        after_revision: 0,
                    },
                },
            },
            SystemTime::now(),
        )
        .unwrap();
    task_id
}
fn queued_native_task(
    owner: &NativeWorkspaceOwner,
    room: crate::shell::state::RoomId,
    caller: &tilo_user_identity::DeviceCert,
) -> String {
    let queue = uuid::Uuid::new_v4();
    owner
        .commit_workspace_task_command(
            OwnerCommitRequest {
                room: room.to_string(),
                operation: CommitOperation::WorkspaceTaskCommand {
                    invocation: queue,
                    authority: WorkspaceTaskAuthority::Person {
                        caller: caller.clone(),
                    },
                    command: WorkspaceTaskCommand::Queue {
                        title: "Review release notes".into(),
                        assigned_agent: "scribe".into(),
                        route: crate::shell::state::WorkspaceTaskRoute::Model,
                        resource: None,
                    },
                },
            },
            SystemTime::now(),
        )
        .unwrap();
    let result = owner
        .workspace_task_result(
            room,
            WorkspaceTaskCommandId {
                device: caller.device_public_key,
                invocation: queue,
            },
        )
        .unwrap()
        .unwrap();
    let tilo_desktop_model::WorkspaceTaskMutationResult::Queued { task_id } = result else {
        panic!("queue result")
    };
    task_id
}
fn wait(service: &NativeTaskReview, invocation: uuid::Uuid) -> Result<(), String> {
    let started = Instant::now();
    loop {
        if let Some(result) = service.poll(invocation) {
            return result;
        }
        if started.elapsed() >= Duration::from_secs(5) {
            return Err("owner command did not resolve".into());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}
#[test]
fn native_review_last_handle_releases_the_owner_and_state_writer_before_returning() {
    let (directory, owner, _room, _, _) = native_workspace_owner::test_enrolled_owner();
    let witness = owner.downgrade();
    let review = NativeTaskReview::new(native_workspace_owner::tree_of(&owner), None).unwrap();
    let retained = review.clone();
    drop(owner);
    crate::shell::persist::wait_for_saves();
    drop(review);
    assert!(
        witness.upgrade().is_some(),
        "a retained review handle must keep its native worker owner alive"
    );
    drop(retained);
    assert!(
        witness.upgrade().is_none(),
        "the final review handle must finish its worker before returning"
    );
    let path = directory.path().join("state.json");
    let mut restored = crate::shell::persist::load_tree_from(&path).unwrap();
    restored.persist_path = Some(path);
    let reopened = native_workspace_owner::open(restored.shared()).unwrap();
    assert!(reopened.health().is_ok());
}

#[test]
fn native_review_preserves_authorship_and_refuses_stale_or_foreign_commands() {
    let (_directory, owner, room, _, _) = native_workspace_owner::test_enrolled_owner();
    admit(&owner, room);
    let caller = owner
        .with_current(room, |view| Ok(view.device().clone()))
        .unwrap();
    let task_id = task(&owner, room, &caller);
    let service = NativeTaskReview::start(
        worker::Worker::new(
            owner.clone(),
            Arc::new(
                crate::shell::broker_services::NativeOwnerCommitService::new(
                    native_workspace_owner::tree_of(&owner),
                    None,
                ),
            ),
        )
        .with_certificate(caller),
    )
    .unwrap();
    let request = TaskReviewRequest {
        room: room.to_string(),
        observed_actor: owner.workspace_task_snapshot(room).unwrap().tasks[0]
            .created_by
            .clone(),
        invocation: uuid::Uuid::new_v4(),
        command: TaskReviewCommand::Steer {
            task_id: task_id.clone(),
            run_epoch: 1,
            after_revision: 0,
            text: "Keep compatibility examples".into(),
        },
    };
    let mut another_actor = request.clone();
    another_actor.invocation = uuid::Uuid::new_v4();
    another_actor.observed_actor = "a different person".into();
    service.submit(another_actor.clone()).unwrap();
    assert!(wait(&service, another_actor.invocation).is_err());
    assert!(
        owner.workspace_task_snapshot(room).unwrap().controls[&task_id]
            .directions
            .is_empty()
    );
    service.submit(request.clone()).unwrap();
    wait(&service, request.invocation).unwrap();
    let snapshot = owner.workspace_task_snapshot(room).unwrap();
    assert_eq!(snapshot.controls[&task_id].directions.len(), 1);
    assert_eq!(
        snapshot.controls[&task_id].directions[0].author,
        snapshot.tasks[0].created_by
    );
    assert_eq!(
        snapshot.controls[&task_id].directions[0].text,
        "Keep compatibility examples"
    );
    service.submit(request.clone()).unwrap();
    wait(&service, request.invocation).unwrap();
    let mut stale = request.clone();
    stale.invocation = uuid::Uuid::new_v4();
    service.submit(stale.clone()).unwrap();
    assert!(wait(&service, stale.invocation).is_err());
    assert_eq!(owner.workspace_task_snapshot(room).unwrap(), snapshot);
    let mut foreign = request;
    foreign.invocation = uuid::Uuid::new_v4();
    foreign.room = uuid::Uuid::new_v4().to_string();
    service.submit(foreign.clone()).unwrap();
    assert!(wait(&service, foreign.invocation).is_err());
    assert_eq!(owner.workspace_task_snapshot(room).unwrap(), snapshot);
}

struct PaneHost(NativeTaskReview);
impl tilo_desktop::activity::ActivityHost for PaneHost {
    fn local_user(&self) -> String {
        "isolated reviewer".into()
    }
    fn task_review_required(&self, _room: &str) -> bool {
        true
    }
    fn task_review_snapshot(
        &self,
        room: &str,
    ) -> Option<Result<std::sync::Arc<TaskReviewSnapshot>, String>> {
        self.0.snapshot(room)
    }
    fn submit_task_review(&self, request: TaskReviewRequest) -> Result<(), String> {
        self.0.submit(request)
    }
    fn poll_task_review(&self, invocation: uuid::Uuid) -> Option<Result<(), String>> {
        self.0.poll(invocation)
    }
}
#[test]
fn native_review_cold_room_metadata_does_not_enable_legacy_mutations() {
    let (_directory, owner, enrolled_room, _, _) = native_workspace_owner::test_enrolled_owner();
    let caller = owner
        .with_current(enrolled_room, |view| Ok(view.device().clone()))
        .unwrap();
    let review = NativeTaskReview::start(
        worker::Worker::new(
            owner.clone(),
            Arc::new(
                crate::shell::broker_services::NativeOwnerCommitService::new(
                    native_workspace_owner::tree_of(&owner),
                    None,
                ),
            ),
        )
        .with_certificate(caller),
    )
    .unwrap();
    let room = uuid::Uuid::new_v4().to_string();
    assert!(
        review.requires_room(&room),
        "an unobserved room must not fall back to unguarded model mutations before the owner-classification read"
    );
}

#[test]
fn native_review_binds_owner_read_and_commit_to_one_selected_certificate() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let (_directory, owner, room, _, _) = native_workspace_owner::test_enrolled_owner();
    admit(&owner, room);
    let caller = owner
        .with_current(room, |view| Ok(view.device().clone()))
        .unwrap();
    let task_id = task(&owner, room, &caller);
    let actor = owner.workspace_task_snapshot(room).unwrap().tasks[0]
        .created_by
        .clone();
    let resolutions = Arc::new(AtomicUsize::new(0));
    let next = resolutions.clone();
    let review = NativeTaskReview::start(
        worker::Worker::new(
            owner.clone(),
            Arc::new(
                crate::shell::broker_services::NativeOwnerCommitService::new(
                    native_workspace_owner::tree_of(&owner),
                    None,
                ),
            ),
        )
        .with_certificate_resolver(move |_| {
            if next.fetch_add(1, Ordering::SeqCst) == 0 {
                Ok(caller.clone())
            } else {
                Err("selected identity changed after observation".into())
            }
        }),
    )
    .unwrap();
    let request = TaskReviewRequest {
        room: room.to_string(),
        invocation: uuid::Uuid::new_v4(),
        observed_actor: actor.clone(),
        command: TaskReviewCommand::Steer {
            task_id: task_id.clone(),
            run_epoch: 1,
            after_revision: 0,
            text: "Retain the reviewed person's attribution".into(),
        },
    };
    review.submit(request.clone()).unwrap();
    wait(&review, request.invocation).unwrap();
    let snapshot = owner.workspace_task_snapshot(room).unwrap();
    assert_eq!(snapshot.controls[&task_id].directions.len(), 1);
    assert_eq!(snapshot.controls[&task_id].directions[0].author, actor);
    assert_eq!(
        snapshot.controls[&task_id].directions[0].text,
        "Retain the reviewed person's attribution"
    );
    assert!(
        resolutions.load(Ordering::SeqCst) >= 2,
        "the follow-up freshness read must independently observe the changed selection"
    );
    assert!(review.snapshot(&room.to_string()).unwrap().is_err());
}

#[test]
fn native_review_renders_signed_owner_evidence_in_three_themes() {
    let (_directory, owner, room, _, _) = native_workspace_owner::test_enrolled_owner();
    admit(&owner, room);
    let caller = owner
        .with_current(room, |view| Ok(view.device().clone()))
        .unwrap();
    let task_id = task(&owner, room, &caller);
    let tree = native_workspace_owner::tree_of(&owner);
    let review = NativeTaskReview::start(
        worker::Worker::new(
            owner,
            Arc::new(
                crate::shell::broker_services::NativeOwnerCommitService::new(tree.clone(), None),
            ),
        )
        .with_certificate(caller),
    )
    .unwrap();
    let started = Instant::now();
    loop {
        if let Some(Ok(snapshot)) = review.snapshot(&room.to_string()) {
            assert_eq!(snapshot.controls[&task_id].run_epoch, 1);
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "native evidence did not become available"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let scratch = tempfile::tempdir().unwrap();
    let output = std::env::var_os("TILO_TASK_REVIEW_EVIDENCE")
        .map_or_else(|| scratch.path().to_owned(), std::path::PathBuf::from);
    std::fs::create_dir_all(&output).unwrap();
    let mut renderer = ui::offscreen::OffscreenRenderer::new(900, 900);
    for (name, mut theme) in [
        ("light", ui::Theme::light()),
        ("dark", ui::Theme::dark()),
        ("high-contrast-200", ui::Theme::high_contrast()),
    ] {
        if name == "high-contrast-200" {
            theme = theme.with_text_scale(2.0);
        }
        capture_review_theme(&review, &tree, &mut renderer, &theme, (&output, name));
    }
}

fn capture_review_theme(
    review: &NativeTaskReview,
    tree: &crate::shell::state::WorkspaceTreeHandle,
    renderer: &mut ui::offscreen::OffscreenRenderer,
    theme: &ui::Theme,
    destination: (&std::path::Path, &str),
) {
    use vello::kurbo::{Affine, Rect};
    let (output, name) = destination;
    let mut pane = tilo_desktop::activity::RoomActivityPane::new();
    pane.set_host(std::rc::Rc::new(PaneHost(review.clone())));
    pane.refresh(&mut *tree.write());
    pane.set_view(tilo_desktop::activity::ActivityView::Tasks);
    pane.layout_window(Rect::new(0.0, 0.0, 900.0, 900.0));
    let mut text = ui::text_render::TextRenderer::new();
    let mut scene = vello::Scene::new();
    pane.draw(
        &mut scene,
        Affine::IDENTITY,
        &mut text,
        theme,
        &ui::presence::PresenceCtx::empty(),
    );
    let snapshot = pane.accessibility_snapshot();
    // This key belongs to the portable desktop catalog, not shell i18n.
    let steer_key = "activity-task-steer";
    let steer_name = tilo_desktop::i18n::t(steer_key);
    let steer = snapshot
        .children
        .iter()
        .position(|node| node.name.as_deref() == Some(&steer_name))
        .expect("native steer action must be visible");
    let rect = pane.accessibility_rect_for_path(&[0, steer]).unwrap();
    assert_eq!(rect.intersect(Rect::new(0.0, 0.0, 900.0, 900.0)), rect);
    renderer.render_png(
        &scene,
        vello::peniko::Color::TRANSPARENT,
        output.join(format!("{name}-task.png")),
    );
    let click = winit::event::WindowEvent::MouseInput {
        device_id: winit::event::DeviceId::dummy(),
        state: winit::event::ElementState::Pressed,
        button: winit::event::MouseButton::Left,
    };
    pane.handle_event(
        &click,
        1.0,
        &winit::event::Modifiers::default(),
        Some(rect.center()),
        &mut *tree.write(),
    );
    assert!(
        pane.perform_task_direction_action(&ui::a11y::Action::SetValue(
            "Keep compatibility examples and ask before removing APIs".into()
        ))
    );
    scene.reset();
    pane.draw(
        &mut scene,
        Affine::IDENTITY,
        &mut text,
        theme,
        &ui::presence::PresenceCtx::empty(),
    );
    assert!(
        pane.accessibility_snapshot()
            .children
            .iter()
            .any(|node| node.role == ui::a11y::Role::TextInput)
    );
    renderer.render_png(
        &scene,
        vello::peniko::Color::TRANSPARENT,
        output.join(format!("{name}-direction.png")),
    );
}

#[test]
fn native_task_review_rechecks_a_signed_demotion_after_discovery() {
    let (_directory, owner, room, _, _) = native_workspace_owner::test_enrolled_owner();
    admit(&owner, room);
    let caller = owner
        .with_current(room, |view| Ok(view.device().clone()))
        .unwrap();
    let task_id = task(&owner, room, &caller);
    let tree = native_workspace_owner::tree_of(&owner);
    let review = NativeTaskReview::start(
        worker::Worker::new(
            owner.clone(),
            Arc::new(crate::shell::broker_services::NativeOwnerCommitService::new(tree, None)),
        )
        .with_certificate(caller.clone()),
    )
    .unwrap();
    let key = ed25519_dalek::SigningKey::from_bytes(&[61; 32]);
    let now = SystemTime::now();
    let messages = owner
        .with_current(room, |view| {
            let before = view.peer().clone();
            let mut next = before.clone();
            next.set_member_role(
                caller.user_public_key,
                tilo_peer_state::join::MemberRole::Viewer,
                &key,
                now,
            )?;
            next.durable_mutation_messages(
                &before,
                &key,
                now,
                view.generation().checked_add(1).unwrap(),
            )
        })
        .unwrap();
    for message in messages {
        owner
            .receive(
                room,
                tilo_peer_state::TransportOrigin::RetainedRelayDelta(
                    key.verifying_key().to_bytes(),
                ),
                message,
                now,
            )
            .unwrap();
    }
    let before = owner.workspace_task_snapshot(room).unwrap();
    let request = TaskReviewRequest {
        room: room.to_string(),
        observed_actor: owner.workspace_task_snapshot(room).unwrap().tasks[0]
            .created_by
            .clone(),
        invocation: uuid::Uuid::new_v4(),
        command: TaskReviewCommand::Cancel {
            task_id,
            run_epoch: 1,
            after_revision: 0,
        },
    };
    review.submit(request.clone()).unwrap();
    assert!(wait(&review, request.invocation).is_err());
    assert_eq!(owner.workspace_task_snapshot(room).unwrap(), before);
    assert!(
        !review
            .snapshot(&room.to_string())
            .unwrap()
            .unwrap()
            .writable
    );
}

#[test]
fn native_cancel_keeps_pending_instructions_and_survives_actual_state_reopen() {
    let (directory, owner, room, _, _) = native_workspace_owner::test_enrolled_owner();
    admit(&owner, room);
    let caller = owner
        .with_current(room, |view| Ok(view.device().clone()))
        .unwrap();
    let task_id = task(&owner, room, &caller);
    let tree = native_workspace_owner::tree_of(&owner);
    let review = NativeTaskReview::start(
        worker::Worker::new(
            owner.clone(),
            Arc::new(crate::shell::broker_services::NativeOwnerCommitService::new(tree, None)),
        )
        .with_certificate(caller),
    )
    .unwrap();
    let direction = TaskReviewRequest {
        room: room.to_string(),
        observed_actor: owner.workspace_task_snapshot(room).unwrap().tasks[0]
            .created_by
            .clone(),
        invocation: uuid::Uuid::new_v4(),
        command: TaskReviewCommand::Steer {
            task_id: task_id.clone(),
            run_epoch: 1,
            after_revision: 0,
            text: "Keep compatibility examples".into(),
        },
    };
    review.submit(direction.clone()).unwrap();
    wait(&review, direction.invocation).unwrap();
    let cancel = TaskReviewRequest {
        room: room.to_string(),
        observed_actor: owner.workspace_task_snapshot(room).unwrap().tasks[0]
            .created_by
            .clone(),
        invocation: uuid::Uuid::new_v4(),
        command: TaskReviewCommand::Cancel {
            task_id: task_id.clone(),
            run_epoch: 1,
            after_revision: 1,
        },
    };
    review.submit(cancel.clone()).unwrap();
    wait(&review, cancel.invocation).unwrap();
    assert!(
        owner
            .with_task_action_admission(room, &task_id, "scribe", 1, 1, || -> Result<(), String> {
                panic!("cancelled task admitted another action")
            })
            .is_err()
    );
    drop(review);
    drop(owner);
    crate::shell::persist::wait_for_saves();
    let path = directory.path().join("state.json");
    let mut restored = crate::shell::persist::load_tree_from(&path).unwrap();
    restored.persist_path = Some(path);
    let reopened = native_workspace_owner::open(restored.shared()).unwrap();
    let snapshot = reopened.workspace_task_snapshot(room).unwrap();
    assert_eq!(
        snapshot.tasks[0].status,
        crate::shell::state::WorkspaceTaskStatus::Cancelled
    );
    let control = &snapshot.controls[&task_id];
    assert!(control.active_run.is_none());
    assert_eq!(control.directions[0].text, "Keep compatibility examples");
    assert!(control.directions[0].applied.is_none());
}

#[test]
fn native_queued_approval_renders_accessible_controls_in_three_themes() {
    let (_directory, owner, room, _, _) = native_workspace_owner::test_enrolled_owner();
    admit(&owner, room);
    let caller = owner
        .with_current(room, |view| Ok(view.device().clone()))
        .unwrap();
    let task_id = queued_native_task(&owner, room, &caller);
    let tree = native_workspace_owner::tree_of(&owner);
    let review = NativeTaskReview::start(
        worker::Worker::new(
            owner,
            Arc::new(
                crate::shell::broker_services::NativeOwnerCommitService::new(tree.clone(), None),
            ),
        )
        .with_certificate(caller),
    )
    .unwrap();
    let started = Instant::now();
    loop {
        if let Some(Ok(snapshot)) = review.snapshot(&room.to_string()) {
            assert_eq!(snapshot.controls[&task_id].run_epoch, 0);
            assert!(snapshot.controls[&task_id].active_run.is_none());
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "queued owner evidence unavailable"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let scratch = tempfile::tempdir().unwrap();
    let output = std::env::var_os("TILO_TASK_REVIEW_EVIDENCE")
        .map_or_else(|| scratch.path().to_owned(), std::path::PathBuf::from);
    std::fs::create_dir_all(&output).unwrap();
    let mut renderer = ui::offscreen::OffscreenRenderer::new(900, 900);
    for (name, theme) in [
        ("light", ui::Theme::light()),
        ("dark", ui::Theme::dark()),
        (
            "high-contrast-200",
            ui::Theme::high_contrast().with_text_scale(2.0),
        ),
    ] {
        capture_queued_approval(&review, &tree, &mut renderer, &theme, (&output, name));
    }
}
fn capture_queued_approval(
    review: &NativeTaskReview,
    tree: &crate::shell::state::WorkspaceTreeHandle,
    renderer: &mut ui::offscreen::OffscreenRenderer,
    theme: &ui::Theme,
    destination: (&std::path::Path, &str),
) {
    use vello::kurbo::{Affine, Rect};
    let mut pane = tilo_desktop::activity::RoomActivityPane::new();
    pane.set_host(std::rc::Rc::new(PaneHost(review.clone())));
    pane.refresh(&mut *tree.write());
    pane.set_view(tilo_desktop::activity::ActivityView::Tasks);
    pane.layout_window(Rect::new(0.0, 0.0, 900.0, 900.0));
    let mut text = ui::text_render::TextRenderer::new();
    let mut scene = vello::Scene::new();
    pane.draw(
        &mut scene,
        Affine::IDENTITY,
        &mut text,
        theme,
        &ui::presence::PresenceCtx::empty(),
    );
    let snapshot = pane.accessibility_snapshot();
    let approve_name = tilo_desktop::i18n::t("activity-task-approve-run");
    let approve = snapshot
        .children
        .iter()
        .position(|node| node.name.as_deref() == Some(&approve_name))
        .expect("native approval action must be visible");
    let rect = pane.accessibility_rect_for_path(&[0, approve]).unwrap();
    assert_eq!(rect.intersect(Rect::new(0.0, 0.0, 900.0, 900.0)), rect);
    renderer.render_png(
        &scene,
        vello::peniko::Color::TRANSPARENT,
        destination.0.join(format!("{}-approve.png", destination.1)),
    );
}
