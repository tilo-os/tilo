//! Receiver speculation against a real native host publication: a member's
//! forwarded change is shown at once, kept out of durable cuts, promoted when
//! the host's certified durable publication covers it, and withdrawn when it
//! never becomes durable.
use super::*;
use crate::peer_delivery::{CompleteEvent, admission::CutAdmission, incoming::IncomingBudget};
use automerge::{ReadDoc, transaction::Transactable};
use ed25519_dalek::SigningKey;
use qos_peer_state::TransportOrigin;
use tilo_relay::delivery_v3::state::{EventKind, canonical_item_digest};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};
use web_time::{Duration, SystemTime, UNIX_EPOCH};

const ROOM: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const APP: &str = "test.app";
const RECORD: &str = "tab:1";
const T0: u64 = 1_000_000_000_000;

fn time() -> SystemTime {
    UNIX_EPOCH + Duration::from_nanos(T0)
}
fn cert(root: &SigningKey, key: &SigningKey) -> DeviceCert {
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "speculation fixture".into(),
        capabilities: DeviceCapabilities::primary(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, root).unwrap();
    cert
}

/// Native host (governor) plus a second author device, both on the roster.
struct Room {
    host_key: SigningKey,
    host: StatePeer,
    member_key: SigningKey,
    member: StatePeer,
    governor: DeviceCert,
    roster: Vec<DeviceCert>,
    generation: u64,
}
impl Room {
    fn new() -> Self {
        let host_key = SigningKey::from_bytes(&[2; 32]);
        let governor = cert(&SigningKey::from_bytes(&[1; 32]), &host_key);
        let member_key = SigningKey::from_bytes(&[4; 32]);
        // A second device of the room owner: an ordinary author, not the host.
        let member_cert = cert(&SigningKey::from_bytes(&[1; 32]), &member_key);
        let roster = vec![governor.clone(), member_cert];
        let mut host = StatePeer::new(
            ROOM,
            governor.clone(),
            roster.clone(),
            RevocationList::new(),
            time(),
        )
        .unwrap();
        let mut doc = automerge::AutoCommit::new();
        doc.put(automerge::ROOT, "value", "v1").unwrap();
        doc.commit();
        host.install_seed(
            qos_peer_state::SeedState {
                room_changes: qos_peer_state::seed_room(ROOM, &governor, &roster).unwrap(),
                documents: vec![qos_peer_state::SeedDocument {
                    app_id: APP.into(),
                    record_id: RECORD.into(),
                    kind: "test".into(),
                    bytes: doc.save(),
                }],
            },
            &host_key,
            time(),
        )
        .unwrap();
        // The native host admits delivery items as the selected v5 writer.
        host.select_admission_v5().unwrap();
        let mut room = Self {
            host_key,
            member: StatePeer::new(
                ROOM,
                governor.clone(),
                roster.clone(),
                RevocationList::new(),
                time(),
            )
            .unwrap(),
            host,
            member_key,
            governor,
            roster,
            generation: 1,
        };
        let baseline = room.baseline();
        room.member = cut(&room.member, &baseline).unwrap();
        room
    }
    fn consumer(&self) -> StatePeer {
        StatePeer::new(
            ROOM,
            self.governor.clone(),
            self.roster.clone(),
            RevocationList::new(),
            time(),
        )
        .unwrap()
    }
    fn baseline(&mut self) -> Vec<(EventKind, Vec<u8>)> {
        events(
            self.host
                .bootstrap_messages(&self.host_key, time())
                .unwrap(),
        )
    }
    /// The member authors one change (the exact bytes the host forwards).
    fn member_edit(&mut self, key: &str, value: &str) -> Vec<u8> {
        let mut doc =
            automerge::AutoCommit::load(&self.member.document(APP, RECORD).unwrap()).unwrap();
        let before = doc.get_heads();
        doc.put(automerge::ROOT, key, value).unwrap();
        doc.commit();
        let message = self
            .member
            .publish_incremental(
                APP,
                RECORD,
                doc.save_after(&before),
                &self.member_key,
                time(),
            )
            .unwrap();
        postcard::to_stdvec(&message).unwrap()
    }
    /// Host admission and certified durable publication (what replay carries).
    fn commit(&mut self, canonicals: &[&[u8]]) -> Vec<(EventKind, Vec<u8>)> {
        let before = self.host.clone();
        let member = self.member_key.verifying_key().to_bytes();
        for canonical in canonicals {
            self.host
                .receive_from_report(
                    TransportOrigin::RelayDelta(member),
                    postcard::from_bytes(canonical).unwrap(),
                    time(),
                )
                .unwrap();
        }
        self.generation += 1;
        let messages = self
            .host
            .durable_mutation_messages(&before, &self.host_key, time(), self.generation)
            .unwrap();
        events(messages)
    }
    /// A host-authored durable edit that does not include any member change.
    fn host_edit(&mut self, value: &str) -> Vec<(EventKind, Vec<u8>)> {
        let mut doc =
            automerge::AutoCommit::load(&self.host.document(APP, RECORD).unwrap()).unwrap();
        let before = doc.get_heads();
        doc.put(automerge::ROOT, "host", value).unwrap();
        doc.commit();
        let message = self
            .host
            .publish_incremental(APP, RECORD, doc.save_after(&before), &self.host_key, time())
            .unwrap();
        events(vec![message])
    }
}
fn events(messages: Vec<NeighbourMsg>) -> Vec<(EventKind, Vec<u8>)> {
    messages
        .iter()
        .map(|message| {
            let kind = match message {
                NeighbourMsg::SyncResp { .. } => EventKind::Governance,
                _ => EventKind::State,
            };
            (kind, postcard::to_stdvec(message).unwrap())
        })
        .collect()
}
fn budget() -> IncomingBudget {
    IncomingBudget::new(64 * 1024 * 1024).unwrap()
}
/// Durable delivery: one complete native cut (as `install_cut`).
fn cut(state: &StatePeer, events: &[(EventKind, Vec<u8>)]) -> Result<StatePeer, String> {
    let incoming = budget();
    let mut cut = CutAdmission::begin(state, incoming.clone(), time())?;
    for (ordinal, (kind, bytes)) in events.iter().enumerate() {
        cut.push(
            CompleteEvent {
                ordinal: ordinal as u32,
                kind: *kind,
                digest: canonical_item_digest(bytes),
                bytes: incoming.copy(bytes)?,
            },
            time(),
        )?;
    }
    cut.finish_complete(time())
}
/// Forwarded change admission (as `peer_live_admit`).
fn live(state: &StatePeer, canonical: &[u8]) -> Result<StatePeer, String> {
    let mut cut = CutAdmission::begin(state, budget(), time())?;
    cut.push_retained(canonical, time())?;
    cut.finish_complete(time())
}
fn get(state: &StatePeer, key: &str) -> Option<String> {
    let doc = automerge::AutoCommit::load(&state.document(APP, RECORD)?).ok()?;
    doc.get(automerge::ROOT, key)
        .ok()
        .flatten()
        .map(|(value, _)| value.to_str().unwrap_or_default().to_owned())
}
fn item(canonical: &[u8], at: u64) -> Speculative {
    Speculative {
        digest: canonical_item_digest(canonical),
        canonical: canonical.to_vec(),
        admitted_at_ns: at,
    }
}
/// A receiver that has installed the durable baseline.
fn receiver(room: &mut Room) -> StatePeer {
    let baseline = room.baseline();
    cut(&room.consumer(), &baseline).unwrap()
}

#[test]
fn forwarded_change_is_visible_at_once_and_promoted_when_durable_state_covers_it() {
    let mut room = Room::new();
    let durable = receiver(&mut room);
    let change = room.member_edit("live", "typed");
    let mut speculation = Speculation::default();
    let view = speculation
        .admit(&durable, item(&change, T0), live)
        .unwrap();
    assert_eq!(
        get(&view, "live").as_deref(),
        Some("typed"),
        "visible before durable replay"
    );
    assert!(speculation.is_active());
    // Durable cuts build on the basis, never on the view.
    let basis = speculation.take_basis().unwrap();
    assert_eq!(basis.durable().unwrap(), durable.durable().unwrap());
    assert_eq!(get(&basis, "live"), None);
    let replay = room.commit(&[&change]);
    let next = cut(&basis, &replay).unwrap();
    let rebased = speculation.rebase(&next, T0 + 1, live);
    assert_eq!(
        rebased.promoted.len(),
        1,
        "host-certified coverage promotes it"
    );
    assert!(rebased.withdrawn.is_empty());
    assert!(rebased.state.is_none(), "view equals durable state again");
    assert!(!speculation.is_active());
    assert_eq!(get(&next, "live").as_deref(), Some("typed"));
}

#[test]
fn an_unconfirmed_change_survives_unrelated_durable_cuts_then_is_withdrawn_at_its_deadline() {
    let mut room = Room::new();
    let durable = receiver(&mut room);
    let change = room.member_edit("live", "never durable");
    let mut speculation = Speculation::default();
    speculation
        .admit(&durable, item(&change, T0), live)
        .unwrap();

    // The host commits something else (the forwarded change's barrier failed).
    let basis = speculation.take_basis().unwrap();
    let other = cut(&basis, &room.host_edit("h1")).unwrap();
    let rebased = speculation.rebase(&other, T0 + 1_000_000, live);
    assert!(rebased.promoted.is_empty() && rebased.withdrawn.is_empty());
    let view = rebased
        .state
        .expect("still shown on top of the new durable state");
    assert_eq!(get(&view, "live").as_deref(), Some("never durable"));
    assert_eq!(get(&view, "host").as_deref(), Some("h1"));
    assert_eq!(get(&other, "live"), None, "durable state never contains it");
    assert!(speculation.is_active());

    // Not yet due.
    assert!(
        speculation
            .expire(T0 + CONFIRM_DEADLINE_NS - 1, live)
            .is_none()
    );
    assert_eq!(
        speculation.next_deadline_ns(),
        Some(T0 + CONFIRM_DEADLINE_NS)
    );
    // Due: withdrawn, and the state rebuilds from durable state alone.
    let expired = speculation.expire(T0 + CONFIRM_DEADLINE_NS, live).unwrap();
    assert_eq!(expired.withdrawn.len(), 1);
    assert_eq!(
        expired.withdrawn[0].route(),
        Some((APP.into(), RECORD.into()))
    );
    let state = expired.state.unwrap();
    assert_eq!(get(&state, "live"), None);
    assert_eq!(get(&state, "host").as_deref(), Some("h1"));
    assert!(!speculation.is_active());
}

#[test]
fn a_late_retry_after_withdrawal_arrives_through_durable_replay() {
    let mut room = Room::new();
    let durable = receiver(&mut room);
    let change = room.member_edit("live", "retried");
    let mut speculation = Speculation::default();
    speculation
        .admit(&durable, item(&change, T0), live)
        .unwrap();
    let expired = speculation.expire(T0 + CONFIRM_DEADLINE_NS, live).unwrap();
    let state = expired.state.unwrap();
    assert_eq!(get(&state, "live"), None);
    // The author's retry is committed later; replay alone restores it.
    let replay = room.commit(&[&change]);
    let next = cut(&state, &replay).unwrap();
    assert_eq!(get(&next, "live").as_deref(), Some("retried"));
}

#[test]
fn dependent_changes_rebase_in_order_and_withdraw_together() {
    let mut room = Room::new();
    let durable = receiver(&mut room);
    let first = room.member_edit("a", "1");
    let second = room.member_edit("b", "2");
    let mut speculation = Speculation::default();
    let view = speculation.admit(&durable, item(&first, T0), live).unwrap();
    let view = speculation
        .admit(&view, item(&second, T0 + 5), live)
        .unwrap();
    assert_eq!(get(&view, "b").as_deref(), Some("2"));
    // Only the first commits durably; the second stays speculative on top.
    let basis = speculation.take_basis().unwrap();
    let next = cut(&basis, &room.commit(&[&first])).unwrap();
    let rebased = speculation.rebase(&next, T0 + 10, live);
    assert_eq!(rebased.promoted.len(), 1);
    assert_eq!(speculation.outstanding().len(), 1);
    assert_eq!(
        get(rebased.state.as_ref().unwrap(), "b").as_deref(),
        Some("2")
    );
    // Duplicate forwarding of an outstanding change is refused.
    assert!(
        speculation
            .admit(&next, item(&second, T0 + 11), live)
            .is_err()
    );
}

#[test]
fn local_publication_promotes_and_capacity_bounds_outstanding_changes() {
    let mut room = Room::new();
    let durable = receiver(&mut room);
    let change = room.member_edit("live", "seen");
    let mut speculation = Speculation::default();
    speculation
        .admit(&durable, item(&change, T0), live)
        .unwrap();
    assert_eq!(speculation.promote_all().len(), 1);
    assert!(!speculation.is_active() && speculation.outstanding().is_empty());
    assert!(speculation.take_basis().is_none());
    // A refused change leaves no basis behind.
    assert!(
        speculation
            .admit(&durable, item(b"garbage", T0), live)
            .is_err()
    );
    assert!(!speculation.is_active());
    // Capacity: never more than MAX_SPECULATIVE outstanding.
    let mut full = Speculation::default();
    let accept = |state: &StatePeer, _: &[u8]| Ok(state.clone());
    for n in 0..MAX_SPECULATIVE {
        full.admit(&durable, item(&(n as u64).to_le_bytes(), T0), accept)
            .unwrap();
    }
    assert!(full.admit(&durable, item(b"one more", T0), accept).is_err());
    // A failed durable cut restores the basis.
    let basis = full.take_basis().unwrap();
    assert!(!full.is_active());
    full.restore_basis(basis);
    assert!(full.is_active());
}

/// Heads of the record-cache entry after a durable install: the cache holds
/// the view's record and installing `state` merges into it.
fn cached_heads(view: &StatePeer, state: &StatePeer) -> Vec<String> {
    let mut cached = automerge::AutoCommit::load(&view.document(APP, RECORD).unwrap()).unwrap();
    let mut installed = automerge::AutoCommit::load(&state.document(APP, RECORD).unwrap()).unwrap();
    cached.merge(&mut installed).unwrap();
    cached.get_heads().iter().map(ToString::to_string).collect()
}
/// The storage-batch check a cursor journal must pass
/// (`peer_storage_batch::validate_records`).
fn journal_matches(state: &StatePeer, heads: &[String]) -> bool {
    crate::peer_journal::Journal::from_state(state, vec![])
        .unwrap()
        .match_document_heads(APP, RECORD, heads)
        .is_some()
}

#[test]
fn a_durable_cut_settles_outstanding_changes_so_its_cursor_journal_matches_the_cached_records() {
    // Regression (slow links): the second forwarded change arrived before the
    // durable cut of the first. Keeping it speculative across the cut staged
    // a cursor journal without it while the cached record carried it, so the
    // batch failed "cursor journal does not match exact record basis" and the
    // guest reconnected every few seconds without ever persisting again.
    let mut room = Room::new();
    let durable = receiver(&mut room);
    let first = room.member_edit("a", "1");
    let second = room.member_edit("b", "2");
    let mut speculation = Speculation::default();
    let view = speculation.admit(&durable, item(&first, T0), live).unwrap();
    let view = speculation
        .admit(&view, item(&second, T0 + 5), live)
        .unwrap();
    let basis = speculation.take_basis().unwrap();
    let next = cut(&basis, &room.commit(&[&first])).unwrap();
    assert!(
        !journal_matches(&next, &cached_heads(&view, &next)),
        "the bare cut's journal lacks the outstanding change the cached record carries"
    );

    let settled = speculation.settle(next, T0 + 10, live);
    assert_eq!(
        settled.promoted.len(),
        2,
        "covered, then persisted with the cut"
    );
    assert!(settled.withdrawn.is_empty());
    assert!(!speculation.is_active() && speculation.outstanding().is_empty());
    assert!(journal_matches(
        &settled.state,
        &cached_heads(&view, &settled.state)
    ));
    assert_eq!(get(&settled.state, "b").as_deref(), Some("2"));
    // Its durable replay later is an ordinary duplicate.
    let later = cut(&settled.state, &room.commit(&[&second])).unwrap();
    assert_eq!(get(&later, "b").as_deref(), Some("2"));
}

#[test]
fn a_durable_cut_withdraws_expired_changes_and_restore_undoes_a_failed_install() {
    let mut room = Room::new();
    let durable = receiver(&mut room);
    let change = room.member_edit("live", "never durable");
    let mut speculation = Speculation::default();
    speculation
        .admit(&durable, item(&change, T0), live)
        .unwrap();
    let basis = speculation.take_basis().unwrap();
    let other = cut(&basis, &room.host_edit("h1")).unwrap();

    // The install fails: the overlay comes back exactly as before.
    let previous = speculation.outstanding().to_vec();
    let _ = speculation.settle(other.clone(), T0 + 1, live);
    speculation.restore(basis.clone(), previous);
    assert!(speculation.is_active() && speculation.outstanding().len() == 1);
    assert!(speculation.take_basis().is_some());

    // Past its deadline the cut withdraws it instead of persisting it.
    let settled = speculation.settle(other, T0 + CONFIRM_DEADLINE_NS, live);
    assert_eq!(settled.withdrawn.len(), 1);
    assert!(settled.promoted.is_empty());
    assert_eq!(get(&settled.state, "live"), None);
    assert_eq!(get(&settled.state, "host").as_deref(), Some("h1"));
    assert!(!speculation.is_active());
}

// ---- Author-published forwards (Writer live typing) -------------------------

/// The Writer's paragraph text object `p` in `state`'s record, if present.
fn paragraph(state: &StatePeer) -> Option<String> {
    let doc = automerge::AutoCommit::load(&state.document(APP, RECORD)?).ok()?;
    let (_, id) = doc.get(automerge::ROOT, "p").ok().flatten()?;
    doc.text(&id).ok()
}
fn splice(doc: &mut automerge::AutoCommit, at: usize, text: &str) {
    let (_, id) = doc.get(automerge::ROOT, "p").unwrap().unwrap();
    doc.splice_text(&id, at, 0, text).unwrap();
}
impl Room {
    /// The member types into its own copy (the exact bytes it publishes).
    fn member_types(&mut self, at: usize, text: &str) -> Vec<u8> {
        let mut doc =
            automerge::AutoCommit::load(&self.member.document(APP, RECORD).unwrap()).unwrap();
        let before = doc.get_heads();
        splice(&mut doc, at, text);
        doc.commit();
        let message = self
            .member
            .publish_incremental(
                APP,
                RECORD,
                doc.save_after(&before),
                &self.member_key,
                time(),
            )
            .unwrap();
        postcard::to_stdvec(&message).unwrap()
    }
    /// A host-authored durable edit of the document (not including member changes).
    fn host_doc(
        &mut self,
        edit: impl FnOnce(&mut automerge::AutoCommit),
    ) -> Vec<(EventKind, Vec<u8>)> {
        let mut doc =
            automerge::AutoCommit::load(&self.host.document(APP, RECORD).unwrap()).unwrap();
        let before = doc.get_heads();
        edit(&mut doc);
        doc.commit();
        let message = self
            .host
            .publish_incremental(APP, RECORD, doc.save_after(&before), &self.host_key, time())
            .unwrap();
        events(vec![message])
    }
}

#[test]
fn an_author_published_frame_carries_the_exact_outbox_bytes_and_is_admitted_like_a_host_forward() {
    let mut room = Room::new();
    let durable = receiver(&mut room);
    let change = room.member_edit("live", "typed");
    let (digest, record, frame) = crate::peer_live::author_signed_change(&change).unwrap();
    assert_eq!(
        digest,
        canonical_item_digest(&change),
        "de-duplicates with the host forward"
    );
    assert_eq!((record.app.as_str(), record.record.as_str()), (APP, RECORD));
    assert!(frame.len() <= MAX_STREAM_FRAME_BYTES);
    // The receiver's decoder yields the author's bytes unchanged.
    let (decoded, route, canonical) = crate::peer_live::signed_change(&frame).unwrap();
    assert_eq!(
        (decoded, route, canonical.as_slice()),
        (digest, record, change.as_slice())
    );
    let mut speculation = Speculation::default();
    let view = speculation
        .admit(&durable, item(&canonical, T0), live)
        .unwrap();
    assert_eq!(get(&view, "live").as_deref(), Some("typed"));
    // Governance and other non-document items are never author-forwarded.
    for (kind, bytes) in room.baseline() {
        if kind == EventKind::Governance {
            assert!(crate::peer_live::author_signed_change(&bytes).is_none());
        }
    }
    assert!(crate::peer_live::author_signed_change(b"").is_none());
    assert!(crate::peer_live::author_signed_change(&vec![0; MAX_STREAM_FRAME_BYTES]).is_none());
}

#[test]
fn author_forward_records_map_exactly_like_the_host() {
    use crate::peer_live::durable_live_record;
    let primary = durable_live_record("so.tilo.chat", "").unwrap();
    assert_eq!(primary.record, crate::live_view::PACKAGE_PRIMARY_RECORD);
    assert!(
        durable_live_record("so.tilo.chat", crate::live_view::PACKAGE_PRIMARY_RECORD).is_none()
    );
    assert!(durable_live_record("", "tab:1").is_none());
    assert!(durable_live_record(&"a".repeat(129), "tab:1").is_none());
    assert!(durable_live_record("app", &"r".repeat(129)).is_none());
    assert_eq!(durable_live_record("app", "tab:1").unwrap().record, "tab:1");
}

#[test]
fn concurrent_same_paragraph_typing_converges_without_duplicates_through_live_and_durable_paths() {
    let mut room = Room::new();
    // A shared paragraph both people have durably.
    let seed = room.host_doc(|doc| {
        let p = doc
            .put_object(automerge::ROOT, "p", automerge::ObjType::Text)
            .unwrap();
        doc.splice_text(&p, 0, 0, "Hello world").unwrap();
    });
    room.member = cut(&room.member, &seed).unwrap();
    let durable = cut(&receiver(&mut room), &seed).unwrap();
    assert_eq!(paragraph(&durable).as_deref(), Some("Hello world"));

    // The member types two keystrokes (two publications) at the same offset
    // where the host concurrently types.
    let first = room.member_types(5, "X");
    let second = room.member_types(6, "Z");
    let mut speculation = Speculation::default();
    let view = speculation.admit(&durable, item(&first, T0), live).unwrap();
    let view = speculation
        .admit(&view, item(&second, T0 + 16_000_000), live)
        .unwrap();
    assert_eq!(
        paragraph(&view).as_deref(),
        Some("HelloXZ world"),
        "shown one hop after typing"
    );

    // The host's concurrent edit in the same paragraph lands durably first.
    let basis = speculation.take_basis().unwrap();
    let concurrent = cut(&basis, &room.host_doc(|doc| splice(doc, 5, "Y"))).unwrap();
    let rebased = speculation.rebase(&concurrent, T0 + 20_000_000, live);
    assert!(rebased.withdrawn.is_empty());
    let view = rebased.state.expect("both typists are shown");
    let merged = paragraph(&view).unwrap();
    assert_eq!(
        merged.matches('X').count() + merged.matches('Y').count() + merged.matches('Z').count(),
        3
    );
    assert!(
        merged.contains("XZ"),
        "the member's run stays contiguous: {merged}"
    );

    // The host then commits the member's changes: promoted, identical to the
    // host's own converged text, no duplicated characters.
    let basis = speculation.take_basis().unwrap();
    let confirmed = cut(&basis, &room.commit(&[&first, &second])).unwrap();
    let rebased = speculation.rebase(&confirmed, T0 + 30_000_000, live);
    assert_eq!(rebased.promoted.len(), 2);
    assert!(rebased.state.is_none() && !speculation.is_active());
    assert_eq!(paragraph(&confirmed), Some(merged.clone()));
    assert_eq!(paragraph(&confirmed), paragraph(&room.host));
    assert_eq!(merged.len(), "Hello world".len() + 3);
}
