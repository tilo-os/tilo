use super::{AgentSessionBinding, AgentSessionReservation, NativeWorkspaceOwner, RoomId};
use crate::application_record::tests::{Fixture, fixture};
use collab_types::caps::{CapOverride, Scope, Subject, Verb};
use tilo_peer_state::TransportOrigin;

pub(crate) fn set_agent_membership(fixture: &Fixture, member: bool) {
    set_agent_scope(
        fixture,
        member.then_some(Scope::Room(crate::testing::ROOM.into())),
    );
}

fn set_agent_scope(fixture: &Fixture, scope: Option<Scope>) {
    let room = RoomId::parse(crate::testing::ROOM).unwrap();
    let messages = fixture
        .owner
        .with_current(room, |view| {
            let before = view.peer().clone();
            let mut candidate = before.clone();
            if let Some(scope) = scope {
                let capabilities = vec![
                    CapOverride::grant(Subject::agent("scribe"), scope, Verb::Write)
                        .with_id("session-test-agent"),
                ];
                candidate.put_governed_capabilities(&capabilities, &fixture.key, fixture.now)?;
            } else {
                candidate.remove_governed_capabilities(
                    &["session-test-agent".into()],
                    &fixture.key,
                    fixture.now,
                )?;
            }
            candidate.durable_mutation_messages(
                &before,
                &fixture.key,
                fixture.now,
                view.generation().checked_add(1).unwrap(),
            )
        })
        .unwrap();
    for message in messages {
        fixture
            .owner
            .receive(
                room,
                TransportOrigin::RetainedRelayDelta(fixture.certificate.device_public_key),
                message,
                fixture.now,
            )
            .unwrap();
    }
}

#[test]
fn native_agent_session_requires_membership_in_its_exact_room() {
    let fixture = fixture();
    let reservation = request(&fixture);
    set_agent_scope(
        &fixture,
        Some(Scope::Room("bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb".into())),
    );
    let path = fixture.directory.path().join("owner.json");
    let before = std::fs::read(&path).unwrap();
    assert!(
        fixture
            .owner
            .reserve_agent_session(reservation.clone(), fixture.now)
            .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);

    set_agent_scope(&fixture, None);
    set_agent_membership(&fixture, true);
    fixture
        .owner
        .reserve_agent_session(reservation, fixture.now)
        .expect("the same launch becomes eligible only after exact-room admission");
}

fn request(fixture: &Fixture) -> AgentSessionReservation {
    AgentSessionReservation {
        room: RoomId::parse(crate::testing::ROOM).unwrap(),
        caller: fixture.certificate.clone(),
        attestor: fixture.certificate.clone(),
        agent: "scribe".into(),
        launch: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa".into(),
        runtime_epoch: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb".into(),
        // Descriptive fixture digests, not a substitute for launcher custody.
        runtime_digest: [7; 32],
    }
}

fn generation(owner: &NativeWorkspaceOwner, room: RoomId) -> u64 {
    owner
        .with_current(room, |view| Ok(view.generation()))
        .unwrap()
}

fn assert_host_custody_is_private(owner: &NativeWorkspaceOwner, room: RoomId) {
    owner
        .with_current(room, |view| {
            use automerge::ReadDoc as _;
            assert!(
                view.peer()
                    .admitted_room()
                    .unwrap()
                    .get(automerge::ROOT, "agent_sessions")
                    .unwrap()
                    .is_none(),
                "host custody never enters the collaborator's replicated room"
            );
            Ok(())
        })
        .unwrap();
}

fn bind_exact_process(fixture: &Fixture, binding: &AgentSessionBinding) {
    fixture
        .owner
        .claim_agent_session(binding, &fixture.certificate, fixture.now)
        .unwrap();
    assert!(
        fixture
            .owner
            .revalidate_bound_agent_session(binding, &fixture.certificate, fixture.now)
            .is_err(),
        "a durable claim is not process custody"
    );
    assert!(
        fixture
            .owner
            .claim_agent_session(binding, &fixture.certificate, fixture.now)
            .is_err()
    );
    fixture
        .owner
        .bind_agent_session_process(binding, &fixture.certificate, [8; 32], fixture.now)
        .unwrap();
    fixture
        .owner
        .revalidate_bound_agent_session(binding, &fixture.certificate, fixture.now)
        .unwrap();
    let bound_generation = generation(&fixture.owner, binding.room);
    fixture
        .owner
        .bind_agent_session_process(binding, &fixture.certificate, [8; 32], fixture.now)
        .unwrap();
    assert_eq!(generation(&fixture.owner, binding.room), bound_generation);
    assert!(
        fixture
            .owner
            .bind_agent_session_process(binding, &fixture.certificate, [9; 32], fixture.now)
            .is_err()
    );
}

#[test]
fn native_agent_session_claim_is_durable_single_use_and_never_invents_a_task() {
    let fixture = fixture();
    let reservation = request(&fixture);
    let path = fixture.directory.path().join("owner.json");
    let bytes = std::fs::read(&path).unwrap();
    assert!(
        fixture
            .owner
            .reserve_agent_session(reservation.clone(), fixture.now)
            .is_err()
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        bytes,
        "an unadmitted agent writes nothing"
    );
    set_agent_membership(&fixture, true);
    let binding = fixture
        .owner
        .reserve_agent_session(reservation.clone(), fixture.now)
        .unwrap();
    let reserved_generation = generation(&fixture.owner, binding.room);
    assert_eq!(
        fixture
            .owner
            .reserve_agent_session(reservation.clone(), fixture.now)
            .unwrap(),
        binding
    );
    assert_eq!(
        generation(&fixture.owner, binding.room),
        reserved_generation
    );
    assert!(
        fixture
            .owner
            .workspace_task_snapshot(binding.room)
            .unwrap()
            .tasks
            .is_empty()
    );
    bind_exact_process(&fixture, &binding);
    assert_host_custody_is_private(&fixture.owner, binding.room);
    drop(fixture.owner);
    let reopened = crate::open_file(&path).unwrap();
    reopened
        .revalidate_agent_session(&binding, &fixture.certificate, fixture.now)
        .unwrap();
    assert!(
        reopened
            .claim_agent_session(&binding, &fixture.certificate, fixture.now)
            .is_err(),
        "reopening the disk must not mint a second executable launch"
    );
    reopened
        .close_agent_session(&binding, &fixture.certificate, fixture.now)
        .unwrap();
    reopened
        .with_current(binding.room, |view| {
            assert!(view.record.agent_sessions.is_empty());
            assert!(view.record.agent_session_history_root.is_some());
            Ok(())
        })
        .unwrap();
    assert!(
        reopened
            .revalidate_agent_session(&binding, &fixture.certificate, fixture.now)
            .is_err()
    );
    assert!(
        reopened
            .reserve_agent_session(reservation, fixture.now)
            .is_err()
    );
}

#[test]
fn native_agent_session_revocation_fences_work_but_allows_host_cleanup() {
    let fixture = fixture();
    set_agent_membership(&fixture, true);
    let reservation = request(&fixture);
    let binding = fixture
        .owner
        .reserve_agent_session(reservation, fixture.now)
        .unwrap();
    fixture
        .owner
        .claim_agent_session(&binding, &fixture.certificate, fixture.now)
        .unwrap();
    let mut changed = binding.clone();
    changed.runtime_digest = [11; 32];
    assert!(
        fixture
            .owner
            .revalidate_agent_session(&changed, &fixture.certificate, fixture.now)
            .is_err()
    );
    let mut forged = fixture.certificate.clone();
    forged.device_label = "changed certificate bytes".into();
    assert!(
        fixture
            .owner
            .revalidate_agent_session(&binding, &forged, fixture.now)
            .is_err()
    );
    let mut duplicate = request(&fixture);
    duplicate.launch = "cccccccc-cccc-4ccc-8ccc-cccccccccccc".into();
    assert!(
        fixture
            .owner
            .reserve_agent_session(duplicate, fixture.now)
            .is_err()
    );
    set_agent_membership(&fixture, false);
    assert!(
        fixture
            .owner
            .revalidate_agent_session(&binding, &fixture.certificate, fixture.now)
            .is_err()
    );
    assert!(
        fixture
            .owner
            .bind_agent_session_process(&binding, &fixture.certificate, [8; 32], fixture.now)
            .is_err()
    );
    fixture
        .owner
        .close_agent_session(&binding, &fixture.certificate, fixture.now)
        .unwrap();
    let mut forged_host = fixture.certificate.clone();
    forged_host.device_label = "not the retained host certificate".into();
    assert!(
        fixture
            .owner
            .close_agent_session(&binding, &forged_host, fixture.now)
            .is_err(),
        "an archived claim remains private host custody"
    );
    fixture
        .owner
        .close_agent_session(&binding, &fixture.certificate, fixture.now)
        .unwrap();
}
