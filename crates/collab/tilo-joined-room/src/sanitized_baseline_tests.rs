//! Signed replacement replay through the file-backed native owner.
use super::tests::{enrolled, queue, request};
use crate::testing::ROOM;
use ed25519_dalek::SigningKey;
use neighbour_wire::NeighbourMsg;
use tilo_desktop_model::WorkspaceTaskStatus;
use tilo_peer_state::TransportOrigin;
use tilo_peer_state::workspace_task::{WorkspaceTaskAuthority, WorkspaceTaskCommand};

fn frame(baseline: &tilo_peer_state::SanitizedRoomBaseline) -> NeighbourMsg {
    NeighbourMsg::SanitizedRoomBaseline {
        room: ROOM.into(),
        baseline: postcard::to_allocvec(baseline).unwrap(),
    }
}

#[test]
fn installed_baseline_retry_preserves_newer_public_state_and_commit_after_reopen() {
    let (directory, owner, room, now) = enrolled();
    let person = owner
        .with_current(room, |view| {
            Ok(WorkspaceTaskAuthority::Person {
                caller: view.device().clone(),
            })
        })
        .unwrap();
    let key = SigningKey::from_bytes(&[61; 32]);
    owner
        .commit_workspace_task_command_with_key(
            request(
                person.clone(),
                queue("First task", "private first instruction"),
            ),
            &key,
            now,
        )
        .unwrap();
    let baseline = owner
        .with_current(room, |view| {
            Ok(view.record.sanitized_baseline.clone().unwrap())
        })
        .unwrap();
    let marker = "public-task-after-checkpoint";
    owner
        .commit_workspace_task_command_with_key(
            request(person, queue(marker, "private later instruction")),
            &key,
            now,
        )
        .unwrap();
    drop(owner);
    let owner = crate::open_file(&directory.path().join("rooms.json")).unwrap();
    let before = owner
        .with_current(room, |view| {
            Ok((
                view.peer().durable()?,
                view.generation(),
                crate::testing::entries(view)?,
            ))
        })
        .unwrap();
    for origin in [
        TransportOrigin::RetainedRelayDelta(baseline.governor),
        TransportOrigin::RetainedNativePublication {
            publisher: baseline.governor,
        },
    ] {
        let report = owner.receive(room, origin, frame(&baseline), now).unwrap();
        assert!(report.summary().duplicate);
        assert!(!report.summary().changed);
    }
    for origin in [
        TransportOrigin::RetainedRelayDelta([55; 32]),
        TransportOrigin::UntrustedPresence(baseline.governor),
    ] {
        assert!(owner.receive(room, origin, frame(&baseline), now).is_err());
    }
    let mut forged = baseline.clone();
    *forged.signature.first_mut().unwrap() ^= 1;
    assert!(
        owner
            .receive(
                room,
                TransportOrigin::RetainedRelayDelta(baseline.governor),
                frame(&forged),
                now
            )
            .is_err()
    );
    let owner = crate::open_file(&directory.path().join("rooms.json")).unwrap();
    owner
        .with_current(room, |view| {
            assert_eq!(
                (
                    view.peer().durable()?,
                    view.generation(),
                    crate::testing::entries(view)?
                ),
                before
            );
            assert!(
                owner
                    .workspace_task_snapshot(room)?
                    .tasks
                    .iter()
                    .any(|task| task.title == marker)
            );
            Ok(())
        })
        .unwrap();
}

fn start_review(
    source: &crate::NativeWorkspaceOwner,
    room: crate::RoomId,
    now: std::time::SystemTime,
    instruction: &str,
) -> (WorkspaceTaskAuthority, String) {
    let person = source
        .with_current(room, |view| {
            Ok(WorkspaceTaskAuthority::Person {
                caller: view.device().clone(),
            })
        })
        .unwrap();
    let host = source
        .with_current(room, |view| {
            Ok(WorkspaceTaskAuthority::Host {
                attestor: view.host().unwrap().clone(),
            })
        })
        .unwrap();
    source
        .commit_workspace_task_command(request(person.clone(), queue("Review", instruction)), now)
        .unwrap();
    let task = source.workspace_task_snapshot(room).unwrap().tasks[0]
        .id
        .clone();
    source
        .commit_workspace_task_command(
            request(
                host,
                WorkspaceTaskCommand::StartRun {
                    task_id: task.clone(),
                    agent_id: "scribe".into(),
                },
            ),
            now,
        )
        .unwrap();

    (person, task)
}

fn publish_legacy_report(
    owner: &crate::NativeWorkspaceOwner,
    room: crate::RoomId,
    now: std::time::SystemTime,
    marker: &str,
) -> (WorkspaceTaskAuthority, String, serde_json::Value) {
    let mut candidate = owner.with_current(room, |view| Ok(view.clone())).unwrap();
    let person = WorkspaceTaskAuthority::Person {
        caller: candidate.device().clone(),
    };
    let host = WorkspaceTaskAuthority::Host {
        attestor: candidate.host().unwrap().clone(),
    };
    // Bypass today's pre-CRDT report adapter to model an older persisted room.
    let (_, result) = candidate
        .peer
        .workspace_mut()
        .apply_workspace_task_command(
            uuid::Uuid::new_v4(),
            [11; 32],
            &person,
            &WorkspaceTaskCommand::Queue {
                title: "Public automatic title".into(),
                assigned_agent: "scribe".into(),
                route: tilo_desktop_model::WorkspaceTaskRoute::Model,
                resource: None,
            },
            now,
        )
        .unwrap();
    let tilo_desktop_model::WorkspaceTaskMutationResult::Queued { task_id } = result else {
        panic!("queue result")
    };
    candidate
        .peer
        .workspace_mut()
        .apply_workspace_task_command(
            uuid::Uuid::new_v4(),
            [12; 32],
            &host,
            &WorkspaceTaskCommand::StartRun {
                task_id: task_id.clone(),
                agent_id: "scribe".into(),
            },
            now,
        )
        .unwrap();
    candidate
        .peer
        .workspace_mut()
        .apply_workspace_task_command(
            uuid::Uuid::new_v4(),
            [13; 32],
            &host,
            &WorkspaceTaskCommand::Abandon {
                task_id: task_id.clone(),
                run_epoch: 1,
                reason: marker.into(),
            },
            now,
        )
        .unwrap();
    let original_event = serde_json::to_value(
        tilo_peer_state::workspace_task::snapshot(candidate.peer.admitted_room().unwrap())
            .unwrap()
            .tasks
            .iter()
            .find(|task| task.id == task_id)
            .unwrap()
            .audit
            .last()
            .unwrap(),
    )
    .unwrap();
    candidate.record.peer = candidate.peer.durable().unwrap();
    candidate.record.generation = candidate.record.generation.checked_add(1).unwrap();
    owner.publish_room(room, candidate, None).unwrap();
    (person, task_id, original_event)
}

fn reenroll_running_receiver(
    source: &crate::NativeWorkspaceOwner,
    receiver: &crate::NativeWorkspaceOwner,
    room: crate::RoomId,
    now: std::time::SystemTime,
    person: &WorkspaceTaskAuthority,
    task: &str,
    key: &SigningKey,
) -> tilo_peer_state::SanitizedRoomBaseline {
    // Recovery retains the original signed enrollment identity and host pins.
    let intent = source
        .with_current(room, |view| {
            crate::VerifiedNativeEnrollmentIntent::restore(&view.record.enrollment_intent)
        })
        .unwrap();
    let (_, bootstrap, _, _, _) = crate::testing::raw_owner_fixture_at(now);
    source
        .commit_workspace_task_command_with_key(
            request(
                person.clone(),
                WorkspaceTaskCommand::DirectionForRun {
                    task_id: task.to_owned(),
                    run_epoch: 1,
                    after_revision: 0,
                    text: "private steer".into(),
                },
            ),
            key,
            now,
        )
        .unwrap();
    let (baseline, messages) = source
        .with_current(room, |view| {
            let baseline = view.record.sanitized_baseline.clone().unwrap();
            let checkpoint = baseline.verify_anchored_document(
                &baseline.room,
                baseline.governor,
                &baseline.anchor(),
            )?;
            assert!(
                tilo_peer_state::workspace_task::controls_of(&checkpoint)?[task]
                    .directions
                    .is_empty()
            );
            let mut peer = view.peer().clone();
            let mut messages = vec![frame(&baseline)];
            messages.extend(peer.durable_bootstrap_messages(key, now, view.generation())?);
            Ok((baseline, messages))
        })
        .unwrap();
    let mut current = bootstrap;
    current.generation = current.generation.checked_add(1).unwrap();
    current.governance_messages = messages
        .into_iter()
        .map(|message| postcard::to_allocvec(&message).unwrap())
        .collect();
    let pending = receiver.prepare_reenrollment(&intent, now).unwrap();
    receiver
        .install(
            &crate::testing::finalize(intent, pending, current).unwrap(),
            now,
        )
        .unwrap();
    let running = receiver.workspace_task_snapshot(room).unwrap();
    assert_eq!(
        running
            .tasks
            .iter()
            .find(|value| value.id == task)
            .unwrap()
            .status,
        WorkspaceTaskStatus::InProgress
    );
    assert_eq!(running.controls[task].run_epoch, 1);
    assert_eq!(
        running.controls[task].directions[0].text,
        tilo_peer_state::workspace_task::private_instruction_reference("private steer")
    );

    baseline
}

#[test]
fn reenrolled_receiver_baseline_retry_cannot_resurrect_a_stopped_run() {
    let (directory, source, room, now) = enrolled();
    // Preserve the enrolled predecessor before the source starts its run.
    let receiver_path = directory.path().join("receiver.json");
    std::fs::copy(directory.path().join("rooms.json"), &receiver_path).unwrap();
    let receiver = crate::open_file(&receiver_path).unwrap();
    let (person, task) = start_review(&source, room, now, "owner-private-review");
    let key = SigningKey::from_bytes(&[61; 32]);
    let baseline = reenroll_running_receiver(&source, &receiver, room, now, &person, &task, &key);

    // Stop on the second host after recovery. Replay is authenticated but
    // must not reinstall the old running checkpoint or lose the Stop receipt.
    let stop = request(
        person,
        WorkspaceTaskCommand::CancelForRun {
            task_id: task.clone(),
            run_epoch: 1,
            after_revision: 1,
        },
    );
    receiver
        .commit_workspace_task_command_with_key(stop.clone(), &key, now)
        .unwrap();
    let stopped = synchronize_stopped_run(&source, &receiver, room, now, &task, &baseline);
    drop(receiver);
    let receiver = crate::open_file(&receiver_path).unwrap();
    let before = receiver
        .with_current(room, |view| {
            Ok((
                view.peer().durable()?,
                view.generation(),
                crate::testing::entries(view)?,
            ))
        })
        .unwrap();
    for origin in [
        TransportOrigin::RetainedRelayDelta(baseline.governor),
        TransportOrigin::RetainedNativePublication {
            publisher: baseline.governor,
        },
    ] {
        let report = receiver
            .receive(room, origin, frame(&baseline), now)
            .unwrap();
        assert!(report.summary().duplicate);
        assert!(!report.summary().changed);
    }
    for origin in [
        TransportOrigin::RetainedRelayDelta([55; 32]),
        TransportOrigin::UntrustedPresence(baseline.governor),
    ] {
        assert!(
            receiver
                .receive(room, origin, frame(&baseline), now)
                .is_err()
        );
    }
    let mut forged = baseline.clone();
    *forged.signature.first_mut().unwrap() ^= 1;
    assert!(
        receiver
            .receive(
                room,
                TransportOrigin::RetainedRelayDelta(baseline.governor),
                frame(&forged),
                now,
            )
            .is_err()
    );
    receiver
        .with_current(room, |view| {
            assert_eq!(
                (
                    view.peer().durable()?,
                    view.generation(),
                    crate::testing::entries(view)?
                ),
                before
            );
            Ok(())
        })
        .unwrap();
    receiver
        .commit_workspace_task_command_with_key(stop, &key, now)
        .unwrap();
    let reopened = crate::open_file(&receiver_path).unwrap();
    assert_eq!(reopened.workspace_task_snapshot(room).unwrap(), stopped);
}

fn synchronize_stopped_run(
    source: &crate::NativeWorkspaceOwner,
    receiver: &crate::NativeWorkspaceOwner,
    room: crate::RoomId,
    now: std::time::SystemTime,
    task: &str,
    baseline: &tilo_peer_state::SanitizedRoomBaseline,
) -> tilo_peer_state::workspace_task::WorkspaceTaskSnapshot {
    let stale_running = source
        .with_current(room, |view| Ok(view.peer().room_message()))
        .unwrap();
    let (stopped_baseline, stopped_message) = receiver
        .with_current(room, |view| {
            Ok((
                view.record.sanitized_baseline.clone().unwrap(),
                view.peer().room_message(),
            ))
        })
        .unwrap();
    // The checkpoint predates the direction. Recovery must recognize the
    // separately signed direction custody, not hash its digest as new prose
    // and reset history against this host's divergent Automerge predecessor.
    assert_eq!(&stopped_baseline, baseline);
    receiver
        .with_current(room, |view| {
            assert!(
                view.record
                    .private_task_text
                    .private_references()
                    .is_empty()
            );
            Ok(())
        })
        .unwrap();
    source
        .receive(
            room,
            TransportOrigin::RetainedRelayDelta(stopped_baseline.governor),
            frame(&stopped_baseline),
            now,
        )
        .unwrap();
    source
        .receive(
            room,
            TransportOrigin::RetainedRelayDelta(baseline.governor),
            stopped_message,
            now,
        )
        .unwrap();
    let stopped = receiver.workspace_task_snapshot(room).unwrap();
    let source_stopped = source.workspace_task_snapshot(room).unwrap();
    assert_eq!(source_stopped.tasks, stopped.tasks);
    assert_eq!(
        source_stopped.controls[task].run_epoch,
        stopped.controls[task].run_epoch
    );
    for (current, expected) in [(source, &source_stopped), (receiver, &stopped)] {
        current
            .receive(
                room,
                TransportOrigin::RetainedRelayDelta(baseline.governor),
                stale_running.clone(),
                now,
            )
            .unwrap();
        assert_eq!(&current.workspace_task_snapshot(room).unwrap(), expected);
    }
    assert_eq!(
        stopped
            .tasks
            .iter()
            .find(|value| value.id == task)
            .unwrap()
            .status,
        WorkspaceTaskStatus::Cancelled
    );
    stopped
}

#[test]
fn historical_automatic_reports_are_removed_from_file_backed_baseline() {
    let (directory, owner, room, now) = enrolled();
    let marker = "historical private host report with exact whitespace\n  retained";
    let (person, task_id, original_event) = publish_legacy_report(&owner, room, now, marker);
    let key = SigningKey::from_bytes(&[61; 32]);
    owner
        .commit_workspace_task_command_with_key(
            request(person, queue("Next task", "private next instruction")),
            &key,
            now,
        )
        .unwrap();
    drop(owner);
    let owner = crate::open_file(&directory.path().join("rooms.json")).unwrap();
    let snapshot = owner.workspace_task_snapshot(room).unwrap();
    let task = snapshot
        .tasks
        .iter()
        .find(|task| task.id == task_id)
        .unwrap();
    assert_eq!(task.title, "Public automatic title");
    assert_eq!(
        task.audit.last().unwrap().note.as_deref(),
        Some("abandoned")
    );
    owner
        .with_current(room, |view| {
            assert!(
                view.record.private_task_text.historical_reports[&task_id]
                    .contains(&original_event)
            );
            assert!(view.record.private_task_text.reports.is_empty());
            let baseline = view.record.sanitized_baseline.as_ref().unwrap();
            let mut doc = baseline.verify_anchored_document(
                ROOM,
                view.governor().device_public_key,
                &baseline.anchor(),
            )?;
            assert_eq!(doc.get_changes(&[]).len(), 1);
            let mut public = crate::testing::entries(view)?;
            public.extend(crate::testing::projection(view)?);
            public.push(baseline.document.clone());
            public.extend(
                doc.get_changes(&[])
                    .iter()
                    .map(|change| change.raw_bytes().to_vec()),
            );
            for bytes in public {
                assert!(
                    !bytes
                        .windows(marker.len())
                        .any(|part| part == marker.as_bytes())
                );
            }
            crate::publication::validate(view)?;
            Ok(())
        })
        .unwrap();
}

#[test]
fn failed_signed_migration_keeps_the_committed_predecessor_visible() {
    use tilo_files::native::atomic::test_support::{Checkpoint, with_failure};
    let (directory, owner, room, now) = enrolled();
    let person = owner
        .with_current(room, |view| {
            Ok(WorkspaceTaskAuthority::Person {
                caller: view.device().clone(),
            })
        })
        .unwrap();
    let before = owner
        .with_current(room, |view| Ok((view.peer().durable()?, view.generation())))
        .unwrap();
    assert!(
        with_failure(Checkpoint::BeforePublish, || {
            owner.commit_workspace_task_command_with_key(
                request(person, queue("Review", "owner-only-instruction")),
                &SigningKey::from_bytes(&[61; 32]),
                now,
            )
        })
        .is_err()
    );
    let reopened = crate::open_file(&directory.path().join("rooms.json")).unwrap();
    for current in [&owner, &reopened] {
        current
            .with_current(room, |view| {
                assert!(view.record.sanitized_baseline.is_none());
                assert_eq!((view.peer().durable()?, view.generation()), before);
                Ok(())
            })
            .unwrap();
        assert!(
            current
                .workspace_task_snapshot(room)
                .unwrap()
                .tasks
                .is_empty()
        );
    }
}

#[test]
fn private_directions_and_public_titles_survive_signed_baseline_and_reopen() {
    for fenced in [false, true] {
        let (directory, owner, room, now) = enrolled();
        let (person, task) = start_review(&owner, room, now, "owner-only-initial-instruction");
        let marker = "owner-conversation-private-marker-74f6";
        let key = SigningKey::from_bytes(&[61; 32]);
        for after_revision in 0..2 {
            let reopened = crate::open_file(&directory.path().join("rooms.json")).unwrap();
            let command = if fenced {
                WorkspaceTaskCommand::DirectionForRun {
                    task_id: task.clone(),
                    run_epoch: 1,
                    after_revision,
                    text: marker.into(),
                }
            } else {
                WorkspaceTaskCommand::Direction {
                    task_id: task.clone(),
                    text: marker.into(),
                }
            };
            reopened
                .commit_workspace_task_command_with_key(request(person.clone(), command), &key, now)
                .unwrap();
        }
        let reopened = crate::open_file(&directory.path().join("rooms.json")).unwrap();
        let title = "Inspect the proposal";
        let instruction = "owner-private-proposal-instruction-82c4";
        reopened
            .commit_workspace_task_command_with_key(
                request(person.clone(), queue(title, instruction)),
                &key,
                now,
            )
            .unwrap();
        let reopened = crate::open_file(&directory.path().join("rooms.json")).unwrap();
        let private = reopened
            .workspace_task_private_instructions(room, &task, 1, &person, now)
            .unwrap()
            .unwrap();
        assert_eq!(private.directions.len(), 2);
        assert_eq!(private.directions[&1], marker);
        assert_eq!(private.directions[&2], marker);
        let public = reopened.workspace_task_snapshot(room).unwrap();
        let titled = public
            .tasks
            .iter()
            .find(|task| task.title == title)
            .unwrap();
        assert_eq!(
            reopened
                .workspace_task_private_instructions(room, &titled.id, 0, &person, now)
                .unwrap()
                .unwrap()
                .instruction
                .as_deref(),
            Some(instruction)
        );
        reopened
            .with_current(room, |view| {
                let mut document = view.peer().admitted_room().unwrap().clone();
                let canonical = tilo_peer_state::workspace_task::snapshot(&document)?;
                assert!(canonical.controls[&task].instruction.is_none());
                assert!(canonical.controls[&titled.id].instruction.is_none());
                for direction in &canonical.controls[&task].directions {
                    assert_eq!(
                        direction.text,
                        tilo_peer_state::workspace_task::private_instruction_reference(marker)
                    );
                }
                assert_eq!(canonical.controls[&task].directions.len(), 2);
                let mut public_bytes = vec![document.save()];
                public_bytes.extend(
                    document
                        .get_changes(&[])
                        .iter()
                        .map(|change| change.raw_bytes().to_vec()),
                );
                public_bytes.extend(crate::testing::entries(view)?);
                public_bytes.extend(crate::testing::projection(view)?);
                for secret in [marker, instruction, "owner-only-initial-instruction"] {
                    for bytes in &public_bytes {
                        assert!(
                            !bytes
                                .windows(secret.len())
                                .any(|bytes| bytes == secret.as_bytes())
                        );
                    }
                }
                assert_first_signed_checkpoint(view);
                crate::publication::validate(view)?;
                Ok(())
            })
            .unwrap();
    }
}

fn assert_first_signed_checkpoint(view: &crate::NativeCommittedView) {
    let baseline = view.record.sanitized_baseline.as_ref().unwrap();
    assert_eq!(baseline.generation, 1);
    baseline
        .verify_anchored_document(ROOM, view.governor().device_public_key, &baseline.anchor())
        .unwrap();
}

#[test]
fn stored_room_without_private_text_fields_keeps_enrollment_metadata() {
    let (_directory, owner, room, _) = enrolled();
    owner.with_current(room, |view| {
        let mut old = serde_json::to_value(&view.record).unwrap();
        let object = old.as_object_mut().unwrap();
        object.remove("private_task_text");
        object.remove("sanitized_baseline");
        // Historical admission data is not a new private-text authority source.
        object.insert("task_admission".into(), serde_json::json!({"instructions": {"obsolete": {"instruction": "not migrated by serde"}}}));
        let restored: crate::record::StoredRoom = serde_json::from_value(old).unwrap();
        assert!(restored.sanitized_baseline.is_none());
        assert_eq!(serde_json::to_value(&restored.private_task_text).unwrap(), serde_json::json!({"instructions": {}}));
        let mut expected = serde_json::to_value(&view.record).unwrap();
        expected.as_object_mut().unwrap().remove("sanitized_baseline");
        let mut actual = serde_json::to_value(&restored).unwrap();
        actual.as_object_mut().unwrap().remove("sanitized_baseline");
        assert_eq!(actual, expected);
        crate::enrollment::restore(room, &restored, std::time::SystemTime::now())?;
        Ok(())
    }).unwrap();
}
