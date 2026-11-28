//! Every copy judges a write by the governance it was written under, whatever
//! order the write and the governance's changes reach it in; a change that
//! took its writer's standing away since judges it too, unless the change
//! kept it as written before.
use super::*;
use crate::fixture::at;

/// `message`, a write signed by `key`, naming `heads` as the governance it was
/// written under, as its writer's page drafts it.
fn under(mut message: NeighbourMsg, heads: &[[u8; 32]], key: &SigningKey) -> NeighbourMsg {
    let NeighbourMsg::AppDocument { envelope, .. } = &mut message else {
        panic!("a document write")
    };
    envelope.name_governance(heads);
    collab_types::sign_op(envelope, key).unwrap();
    message
}

/// The heads of the governance `peer` holds.
fn heads(peer: &StatePeer) -> Vec<[u8; 32]> {
    peer.governance().heads()
}

/// `message` arriving at `copy` from `from`, live or as a holder hands on
/// what it kept: taken in, or why not. A message held for what it waits for
/// is not taken in.
fn arrive(
    copy: &mut StatePeer,
    from: &DeviceCert,
    message: NeighbourMsg,
    kept: bool,
) -> Result<(), String> {
    let origin = if kept {
        TransportOrigin::RetainedServerDelta(from.device_public_key)
    } else {
        TransportOrigin::ServerDelta(from.device_public_key)
    };
    let report = copy
        .receive_from(origin, message, at())
        .map_err(|error| error.to_string())?;
    if report.deferred || report.rejected > 0 || !report.dropped.is_empty() || !report.changed {
        return Err(format!("not taken in: {report:?}"));
    }
    Ok(())
}

/// [`arrive`] as a holder hands on what it kept.
fn handed(copy: &mut StatePeer, from: &DeviceCert, message: NeighbourMsg) -> Result<(), String> {
    arrive(copy, from, message, true)
}

/// [`arrive`] live.
fn sent(copy: &mut StatePeer, from: &DeviceCert, message: NeighbourMsg) -> Result<(), String> {
    arrive(copy, from, message, false)
}

/// `message`, refused by `copy` for what it does, never held.
fn refused(copy: &mut StatePeer, from: &DeviceCert, message: NeighbourMsg) -> String {
    let refusal = sent(copy, from, message).unwrap_err();
    assert!(refusal.contains("forbidden"), "{refusal}");
    refusal
}

/// A new copy of the workspace made through the owner's first device: it
/// decides changes by the surfaces `surfaces` finds, and holds nothing yet.
fn empty_copy(surfaces: Arc<dyn SurfaceSource>) -> StatePeer {
    let (_, owner) = owner();
    let mut copy = StatePeer::new(WORKSPACE, owner, vec![], RevocationList::new(), at()).unwrap();
    copy.set_surfaces(Surfaces::from_source(surfaces));
    copy
}

/// `copy` takes in what `peer` publishes now: its governance, and its records
/// too when `records`.
fn take_in(copy: &mut StatePeer, peer: &mut StatePeer, records: bool) {
    let (owner_key, owner) = owner();
    for message in peer
        .publication_mut()
        .bootstrap_messages(&owner_key, at())
        .unwrap()
    {
        if records || !matches!(message, NeighbourMsg::AppDocument { .. }) {
            handed(copy, &owner, message).unwrap();
        }
    }
}

/// The record's value as `peer` holds it.
fn value(peer: &StatePeer) -> String {
    AutoCommit::load(&peer.document(APP, RECORD).unwrap())
        .unwrap()
        .get(ROOT, "value")
        .unwrap()
        .unwrap()
        .0
        .to_string()
        .trim_matches('"')
        .to_owned()
}

/// The guest's edit of the value to `to`, as its `seq`th write of the record,
/// made on the record as `peer` holds it and under the governance `peer`
/// holds.
fn edit(peer: &StatePeer, key: &SigningKey, to: &'static str, seq: u64) -> NeighbourMsg {
    under(
        guest_write_at(
            peer,
            key,
            move |doc| doc.put(ROOT, "value", to).unwrap(),
            edit_value(),
            None,
            seq,
        ),
        &heads(peer),
        key,
    )
}

/// An editor's workspace: what the owner's device holds once the editor wrote
/// `taken` and the owner then made them a viewer, the workspace as it was
/// before that, and two writes of the editor's made on that earlier state: one
/// beside the demotion, and one naming it.
struct Demotion {
    owner_copy: StatePeer,
    before: StatePeer,
    guest: DeviceCert,
    taken: NeighbourMsg,
    beside: NeighbourMsg,
    after: NeighbourMsg,
    /// A copy made before the editor wrote anything, holding nothing of it.
    early: StatePeer,
}

fn demotion() -> Demotion {
    let (mut owner_copy, key, guest) = workspace_with(Role::Editor);
    let (owner_key, _) = owner();
    let mut early = empty_copy(Arc::new(Source(true)));
    take_in(&mut early, &mut owner_copy, true);
    let taken = edit(&owner_copy, &key, "taken", 1);
    sent(&mut owner_copy, &guest, taken.clone()).expect("an editor edits");
    let before = owner_copy.clone();
    assert!(
        owner_copy
            .governance_mut()
            .set_member_role(guest.user_public_key, Role::Viewer, &owner_key, at())
            .unwrap()
    );
    let beside = edit(&before, &key, "beside", 2);
    let after = under(edit(&before, &key, "after", 3), &heads(&owner_copy), &key);
    Demotion {
        owner_copy,
        before,
        guest,
        taken,
        beside,
        after,
        early,
    }
}

tilo_test_env::scenario! {
    fn a_write_is_judged_by_the_governance_it_was_written_under;

    phase two_copies_taking_the_same_changes_in_opposite_orders_take_the_same_writes;
    // One copy takes the editor's write before their demotion reaches it, the
    // other the demotion first. Judged by what the copy holds when the write
    // arrives, the second would refuse what the first took in.
    {
        let story = demotion();
        let Demotion { guest, .. } = &story;
        let mut first = story.early.clone();
        sent(&mut first, guest, story.taken.clone()).expect("taken before the demotion");
        take_in(&mut first, &mut story.owner_copy.clone(), false);
        refused(&mut first, guest, story.after.clone());
        assert_eq!(value(&first), "taken");

        let mut second = story.early.clone();
        take_in(&mut second, &mut story.owner_copy.clone(), false);
        refused(&mut second, guest, story.after.clone());
        sent(&mut second, guest, story.taken.clone())
            .expect("a write the demotion kept is its editor's, after it as before");
        assert_eq!(value(&second), "taken");
        assert_eq!(
            AutoCommit::load(&first.document(APP, RECORD).unwrap()).unwrap().get_heads(),
            AutoCommit::load(&second.document(APP, RECORD).unwrap()).unwrap().get_heads(),
            "both copies took the same writes"
        );
    }

    phase a_demoted_member_writes_nothing_before_their_demotion_by_picking_old_parents;
    // The editor builds on the workspace as it was before their demotion. The
    // demotion's cut did not take that write, so every copy holding the
    // demotion judges it as written after it: a viewer's edit.
    {
        let mut story = demotion();
        let guest = story.guest.clone();
        refused(&mut story.owner_copy, &guest, story.beside.clone());
        // A copy that took the demotion first.
        let mut copy = story.early.clone();
        take_in(&mut copy, &mut story.owner_copy, false);
        sent(&mut copy, &guest, story.taken.clone()).unwrap();
        refused(&mut copy, &guest, story.beside.clone());
        // A copy made after the workspace started over from a base, as at the
        // next admission: the heads the write names went into the base.
        let mut based = story.owner_copy.clone();
        let (owner_key, owner) = owner();
        assert!(based.compact_workspace(&owner, &owner_key, at()).unwrap());
        let mut late = empty_copy(Arc::new(Source(true)));
        take_in(&mut late, &mut based, true);
        let refusal = handed(&mut late, &guest, story.beside.clone()).unwrap_err();
        assert!(refusal.contains("forbidden"), "{refusal}");
        assert_eq!(value(&late), "taken");
        // The editor's own copy from before the demotion took it: written
        // there, before the demotion reached it.
        sent(&mut story.before, &guest, story.beside).unwrap();
    }
}

/// The owner's send from the mailbox: a reach outside the workspace, which
/// its owner may make and an administrator must ask for.
fn send() -> Did {
    Did::Verb(VerbCall::new(ActionKey::new(APP, KIND, "send"), []))
}

/// The owner's `seq`th write: a send under the governance `peer` holds. The
/// owner's device founded the record, so its own numbers start past that.
fn owners_send(peer: &StatePeer, to: &'static str, seq: u64) -> NeighbourMsg {
    let (owner_key, _) = owner();
    under(
        guest_write_at(
            peer,
            &owner_key,
            move |doc| {
                doc.put(ROOT, "value", to).unwrap();
                let Some((_, outbox)) = doc.get(ROOT, "outbox").unwrap() else {
                    panic!("the mailbox keeps an outbox")
                };
                let entry = doc.put_object(&outbox, to, ObjType::Map).unwrap();
                let value = doc.put_object(&entry, "v", ObjType::Map).unwrap();
                doc.put(&value, "to", "ada@example.test").unwrap();
            },
            send(),
            None,
            seq,
        ),
        &heads(peer),
        &owner_key,
    )
}

tilo_test_env::scenario! {
    fn the_owner_a_workspace_was_handed_from_keeps_their_history;

    phase what_the_owner_wrote_before_the_hand_over_is_taken_after_it;
    // A server kept the workspace as its owner and reached outside it; the
    // workspace is then handed to its person, and the server administers it.
    // Its earlier sends stay its history on a copy that takes the hand-over
    // first; a send it makes now asks.
    {
        let (mut owner_copy, _, guest) = workspace_keeping(Role::Editor, &["outbox"]);
        owner_copy.set_surfaces(Surfaces::from_source(Arc::new(MailboxSource)));
        let (owner_key, owner) = owner();
        let mut copy = empty_copy(Arc::new(MailboxSource));
        take_in(&mut copy, &mut owner_copy, true);
        let history = owners_send(&owner_copy, "sent", 10);
        sent(&mut owner_copy, &owner, history.clone()).expect("its owner reaches outside");
        assert!(
            owner_copy
                .governance_mut()
                .transfer_ownership(guest.user_public_key, &owner_key, at())
                .unwrap()
        );
        let again = owners_send(&owner_copy, "again", 11);
        refused(&mut owner_copy.clone(), &owner, again);

        take_in(&mut copy, &mut owner_copy, false);
        assert_eq!(copy.owner(), guest.user_public_key);
        sent(&mut copy, &owner, history).expect("its history from before the hand-over");
        assert_eq!(value(&copy), "sent");
    }

    phase a_workspace_handed_over_and_copied_to_a_new_device_keeps_its_owners_history;
    // The hand-over is followed by the next admission, which starts the
    // governance over from a base: a device that copies the workspace then
    // holds a governance with none of the heads the old owner's writes name,
    // and is handed those writes from what a holder kept.
    {
        let (mut owner_copy, _, guest) = workspace_keeping(Role::Editor, &["outbox"]);
        owner_copy.set_surfaces(Surfaces::from_source(Arc::new(MailboxSource)));
        let (owner_key, owner) = owner();
        // The device holds the record as it was founded, nothing of its
        // governance's history after that.
        let mut device = empty_copy(Arc::new(MailboxSource));
        take_in(&mut device, &mut owner_copy.clone(), true);
        let history = owners_send(&owner_copy, "sent", 10);
        sent(&mut owner_copy, &owner, history.clone()).unwrap();
        owner_copy
            .governance_mut()
            .transfer_ownership(guest.user_public_key, &owner_key, at())
            .unwrap();
        assert!(owner_copy.compact_workspace(&owner, &owner_key, at()).unwrap());

        take_in(&mut device, &mut owner_copy.clone(), false);
        assert_eq!(device.owner(), guest.user_public_key);
        assert_eq!(value(&device), "seed");
        handed(&mut device, &owner, history).expect("the old owner's history");
        assert_eq!(value(&device), "sent");

        // A device that copies everything at once holds it too.
        let mut fresh = empty_copy(Arc::new(MailboxSource));
        take_in(&mut fresh, &mut owner_copy, true);
        assert_eq!(value(&fresh), "sent");
    }
}
