use super::super::{HttpSessions, tests::Fixture};
use super::*;
use crate::https::tests::held_poll_test_request;
use tilo_relay::delivery_v3::http_proof::{Request, RequestProof};

fn capture(
    f: &Fixture,
    registry: &mut HttpSessions,
    polls: &mut Polls,
    original: &crate::https::BufferedRequest,
) -> (AdmissionTicket, [u8; 16]) {
    let (id, digest) = f.handshake(registry, [93; 32]);
    let request = Request::new("GET", original.target(), b"").unwrap();
    let proof = RequestProof::sign(
        id,
        digest,
        &request,
        f.now,
        f.now + 30_000_000_000,
        &f.owner.identity.native,
    )
    .unwrap();
    let verified = registry
        .admit(&f.owner, &f.current, &proof, &request, f.now)
        .unwrap();
    let ticket = polls.capture(&verified, original, 1).unwrap();
    drop(verified);
    // Capturing a continuation never returns the consumed request nonce to service.
    assert!(
        registry
            .admit(&f.owner, &f.current, &proof, &request, f.now)
            .is_err()
    );
    (ticket, id)
}
#[test]
fn cancellation_keeps_original_request_charge_until_actor_retires_poll() {
    let f = Fixture::new();
    let mut registry = HttpSessions::new(&f.owner, &f.current, f.now).unwrap();
    let mut polls = Polls::new();
    let target = "/peer/v3/events?mode=poll";
    let (request, client, budget) = held_poll_test_request(target);
    let (ticket, _) = capture(&f, &mut registry, &mut polls, &request);
    let charged = budget.available_bytes();
    let id = polls.park(ticket, request).unwrap();
    drop(client);
    assert_eq!(budget.available_bytes(), charged);
    polls.prune_cancelled();
    assert!(polls.take(id).is_none());
    assert_eq!(budget.available_bytes(), budget.idle_available_bytes());
}
#[test]
fn admitted_request_cannot_be_replaced_when_parked() {
    let f = Fixture::new();
    let mut registry = HttpSessions::new(&f.owner, &f.current, f.now).unwrap();
    let mut polls = Polls::new();
    let (original, _original_client, _) = held_poll_test_request("/peer/v3/events?mode=poll");
    let (ticket, _) = capture(&f, &mut registry, &mut polls, &original);
    let (replacement, _client, budget) =
        held_poll_test_request("/peer/v3/events?cursor=ab&mode=poll");
    assert_eq!(
        polls.park(ticket, replacement).unwrap_err(),
        PollError::Invalid
    );
    assert_eq!(budget.available_bytes(), budget.idle_available_bytes());
}
#[test]
fn wake_is_take_once_and_replacement_generation_is_rejected() {
    let f = Fixture::new();
    let mut registry = HttpSessions::new(&f.owner, &f.current, f.now).unwrap();
    let mut polls = Polls::new();
    let target = "/peer/v3/events?mode=poll";
    let (request, _client, budget) = held_poll_test_request(target);
    let (ticket, session_id) = capture(&f, &mut registry, &mut polls, &request);
    let id = polls.park(ticket, request).unwrap();
    let pending = polls.take(id).unwrap();
    assert!(polls.take(id).is_none());
    // Parent registry still owns the only session. Revalidate through its actual context.
    let identity = registry.live[&session_id].identity;
    let guard = f.current.read().unwrap();
    let context = registry
        .context(&f.owner, guard.view(), identity, f.now)
        .unwrap();
    let session = registry
        .live
        .get_mut(&session_id)
        .unwrap()
        .admission
        .revalidate(&context.verification())
        .unwrap();
    assert_eq!(pending.check_session(session, 1, f.now), Ok(()));
    assert_eq!(
        pending.check_session(session, 2, f.now),
        Err(PollError::Replaced)
    );
    assert_eq!(
        pending.check_session(session, 1, session.expires_at_ns()),
        Err(PollError::Replaced)
    );
    drop(pending);
    assert_eq!(budget.available_bytes(), budget.idle_available_bytes());
}
#[test]
fn detach_fences_pending_work_and_releases_its_original_lease() {
    let f = Fixture::new();
    let mut registry = HttpSessions::new(&f.owner, &f.current, f.now).unwrap();
    let mut polls = Polls::new();
    let target = "/peer/v3/events?mode=poll";
    let (request, _client, budget) = held_poll_test_request(target);
    let (ticket, session) = capture(&f, &mut registry, &mut polls, &request);
    let id = polls.park(ticket, request).unwrap();
    polls.fence(session);
    assert!(polls.take(id).is_none());
    assert_eq!(budget.available_bytes(), budget.idle_available_bytes());
}

#[test]
fn bounded_scheduler_reparks_original_deadline_and_cursor_without_reauthentication() {
    let f = Fixture::new();
    let mut registry = HttpSessions::new(&f.owner, &f.current, f.now).unwrap();
    let mut polls = Polls::new();
    let target = "/peer/v3/events?cursor=0025ff&mode=poll";
    let (request, _client, budget) = held_poll_test_request(target);
    let deadline = request.deadline();
    let charged = budget.available_bytes();
    let (ticket, _) = capture(&f, &mut registry, &mut polls, &request);
    let id = polls.park(ticket, request).unwrap();
    assert_eq!(polls.wake_ids(), vec![id]);
    let margin = std::time::Duration::from_millis(500);
    let wake = polls.next_deadline(margin).unwrap();
    assert!(wake < deadline);
    assert_eq!(deadline - wake, margin);
    let pending = polls.take(id).unwrap();
    assert_eq!(pending.cursor().unwrap().as_bytes(), &[0, 37, 255]);
    assert!(polls.wake_ids().is_empty());
    assert_eq!(polls.repark(pending).unwrap(), id);
    assert_eq!(polls.next_deadline(margin), Some(wake));
    assert_eq!(budget.available_bytes(), charged);
    let request = polls.take(id).unwrap().into_request();
    assert_eq!(request.deadline(), deadline);
    drop(request);
    assert_eq!(budget.available_bytes(), budget.idle_available_bytes());
}

#[test]
fn identical_request_bytes_and_deadline_cannot_replace_original_cancellation_owner() {
    let f = Fixture::new();
    let mut registry = HttpSessions::new(&f.owner, &f.current, f.now).unwrap();
    let mut polls = Polls::new();
    let target = "/peer/v3/events?mode=poll";
    let (original, _client, _) = held_poll_test_request(target);
    let (ticket, _) = capture(&f, &mut registry, &mut polls, &original);
    let (replacement, _other_client, _) =
        crate::https::tests::held_poll_test_request_with_deadline(target, original.deadline());
    assert_eq!(
        polls.park(ticket, replacement).unwrap_err(),
        PollError::Invalid
    );
}
#[test]
fn same_consumer_holds_one_poll_newer_supersedes_older_across_sessions() {
    let f = Fixture::new();
    let mut registry = HttpSessions::new(&f.owner, &f.current, f.now).unwrap();
    let mut polls = Polls::new();
    let target = "/peer/v3/events?mode=poll";
    let (first, mut first_client, _) = held_poll_test_request(target);
    let (ticket, first_session) = capture(&f, &mut registry, &mut polls, &first);
    let first_id = polls.park(ticket, first).unwrap();
    let (second, mut second_client, _) = held_poll_test_request(target);
    let (ticket, second_session) = capture(&f, &mut registry, &mut polls, &second);
    assert_ne!(first_session, second_session);
    // The newer poll is held; the superseded one is answered (empty), never dropped.
    let second_id = polls.park(ticket, second).unwrap();
    assert_ne!(first_id, second_id);
    assert_eq!(polls.wake_ids(), vec![second_id]);
    assert!(
        first_client.answered(),
        "superseded poll must receive an empty answer"
    );
    assert!(!second_client.answered());
}
