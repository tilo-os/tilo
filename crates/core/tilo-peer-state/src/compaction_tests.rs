//! A device takes a workspace's history only as its members' devices signed
//! it: a member's write reaches it unchanged through a device that is no
//! member, and a history it holds nothing of yet starts at the workspace's
//! founding or at a compaction its owner or an administrator signed.
use super::*;
use crate::workspace::compaction::{self, CompactionRefusal};

use crate::fixture::{at, primary_cert};
/// A copy of the workspace on a device that has just joined it through the
/// owner's device: it holds nothing yet.
fn newcomer() -> StatePeer {
    let (_, owner) = owner();
    let mut copy = StatePeer::new(WORKSPACE, owner, vec![], RevocationList::new(), at()).unwrap();
    copy.set_surfaces(Surfaces::from_source(Arc::new(Source(true))));
    copy
}

/// `message` as a holder hands on what it kept, sent from `from`'s device:
/// taken in, or why not.
fn handed(copy: &mut StatePeer, from: [u8; 32], message: NeighbourMsg) -> Result<(), String> {
    let report = copy
        .receive_from(TransportOrigin::RetainedServerDelta(from), message, at())
        .map_err(|error| error.to_string())?;
    if report.deferred || report.rejected > 0 || !report.dropped.is_empty() || !report.changed {
        return Err(format!("not taken in: {report:?}"));
    }
    Ok(())
}

/// The workspace document `peer` holds, as it would send it whole.
fn document_of(peer: &StatePeer) -> AutoCommit {
    let NeighbourMsg::SyncResp { changes, .. } = peer.workspace_message() else {
        panic!("a workspace document")
    };
    crate::workspace::candidate(None, &changes).unwrap()
}

/// `doc`'s whole history, as a workspace message of [`WORKSPACE`].
fn sent(doc: &AutoCommit) -> NeighbourMsg {
    NeighbourMsg::SyncResp {
        changes: doc
            .clone()
            .get_changes(&[])
            .into_iter()
            .map(crate::workspace::sent_change)
            .collect(),
        workspace: WORKSPACE.into(),
    }
}

/// Whether anything `doc` ever held, in any change of its history, holds `text`.
fn ever_held(doc: &AutoCommit, text: &str) -> bool {
    doc.clone()
        .save_nocompress()
        .windows(text.len())
        .any(|window| window == text.as_bytes())
}

/// A saved grant set named `name`.
fn preset(name: &str) -> tilo_action_policy::Preset {
    tilo_action_policy::Preset {
        name: name.into(),
        description: String::new(),
        rules: Vec::new(),
    }
}

tilo_test_env::scenario! {
    fn a_device_takes_a_history_only_as_members_signed_it;

    phase a_device_that_is_no_member_carries_a_members_write_unchanged;
    // A server keeping the workspace holds no device on its member list: it
    // keeps a member's write as the bytes it was sent and hands them on. The
    // receiver takes the write as its author's, by the author's signature,
    // and refuses any byte the carrier changed.
    {
        let (mut peer, key, guest) = workspace_with(Role::Editor);
        let (owner_key, owner) = owner();
        let mut copy = newcomer();
        for message in peer
            .publication_mut()
            .bootstrap_messages(&owner_key, at())
            .unwrap()
        {
            handed(&mut copy, owner.device_public_key, message).unwrap();
        }
        let write = guest_write_at(
            &peer,
            &key,
            |doc| doc.put(ROOT, "value", "carried").unwrap(),
            edit_value(),
            None,
            1,
        );
        let sent_bytes = postcard::to_stdvec(&write).unwrap();
        let kept = sent_bytes.clone();
        let carried: NeighbourMsg = postcard::from_bytes(&kept).unwrap();
        assert_eq!(postcard::to_stdvec(&carried).unwrap(), sent_bytes);
        let NeighbourMsg::AppDocument { envelope, .. } = &carried else {
            panic!("a document write")
        };
        collab_types::verify_op(envelope).expect("the member signed what was carried");
        let author = *envelope.author.as_bytes();
        assert_eq!(author, guest.device_public_key, "the member is its author");

        let mut tampered = carried.clone();
        let NeighbourMsg::AppDocument { envelope: altered, .. } = &mut tampered else {
            panic!("a document write")
        };
        let last = altered.payload.len() - 1;
        altered.payload[last] ^= 1;
        let mut refusing = copy.clone();
        assert!(handed(&mut refusing, author, tampered).is_err(), "a changed byte is refused");
        assert_eq!(value(&refusing), "\"seed\"");

        handed(&mut copy, author, carried).expect("the member's write, carried unchanged");
        assert_eq!(value(&copy), "\"carried\"");
    }

    phase a_newcomer_refuses_a_history_that_starts_anywhere_else;
    // The device a newcomer joined through hands it a history: one founded by
    // someone else, one starting from a base no one signed, or from a base a
    // member who administers nothing signed. Each is refused.
    {
        let (mut peer, key, guest) = workspace_with(Role::Editor);
        let (_, owner) = owner();

        let stranger_key = SigningKey::from_bytes(&[62; 32]);
        let stranger = primary_cert(&SigningKey::from_bytes(&[61; 32]), &stranger_key, "stranger");
        let mut founded_elsewhere = StatePeer::new(
            WORKSPACE,
            stranger.clone(),
            vec![stranger.clone()],
            RevocationList::new(),
            at(),
        )
        .unwrap();
        crate::fixture::found(
            &mut founded_elsewhere,
            (&stranger_key, &stranger),
            std::slice::from_ref(&stranger),
            Vec::new(),
            at(),
        );
        assert!(
            handed(
                &mut newcomer(),
                owner.device_public_key,
                founded_elsewhere.workspace_message(),
            )
            .is_err(),
            "a founding that lists none of the newcomer's devices administers nothing there"
        );
        // A history whose founding someone other than its owner wrote is refused whoever
        // it lists.
        let mut forged = document_of(&peer);
        forged.put(ROOT, "owner", hex::encode(stranger.user_public_key)).unwrap();
        forged.commit();
        assert_eq!(
            compaction::verified_start(&forged, WORKSPACE),
            Err(CompactionRefusal::NotFounded)
        );
        let refusal = handed(&mut newcomer(), owner.device_public_key, sent(&forged)).unwrap_err();
        assert!(refusal.contains(&CompactionRefusal::NotFounded.to_string()), "{refusal}");

        let held = document_of(&peer);
        let unsigned = crate::workspace::base::rebase(
            &held,
            peer.workspace_actor_for(&owner.device_public_key),
        )
        .unwrap();
        let refusal = handed(&mut newcomer(), owner.device_public_key, sent(&unsigned)).unwrap_err();
        assert!(refusal.contains(&CompactionRefusal::Unsigned.to_string()), "{refusal}");

        let by_an_editor = compaction::compact(
            &held,
            peer.workspace_actor_for(&guest.device_public_key),
            (&guest, &key),
            WORKSPACE,
            1,
        )
        .unwrap();
        assert_eq!(
            compaction::compacted_by(&by_an_editor, WORKSPACE, &owner.user_public_key),
            Err(CompactionRefusal::NotAdministrator)
        );
        let refusal =
            handed(&mut newcomer(), owner.device_public_key, sent(&by_an_editor)).unwrap_err();
        assert!(refusal.contains(&CompactionRefusal::NotAdministrator.to_string()), "{refusal}");
        // A device holding the history refuses it as the newer base too.
        let mut holder = newcomer();
        handed(&mut holder, owner.device_public_key, peer.workspace_message()).unwrap();
        let refusal = handed(&mut holder, guest.device_public_key, sent(&by_an_editor)).unwrap_err();
        assert!(refusal.contains(&CompactionRefusal::NotAdministrator.to_string()), "{refusal}");
        assert!(
            peer.compact_workspace(&guest, &key, at()).is_err(),
            "an editor's device makes no compaction"
        );
    }

    phase a_newcomer_replays_from_an_administrators_compaction_without_what_was_withdrawn;
    // The owner saves a grant set and withdraws it, and makes the member an
    // administrator; the administrator's device compacts the workspace. A
    // device that joins afterwards takes the compaction, made and signed by
    // the administrator, and nothing of the withdrawn set.
    {
        let (mut peer, key, guest) = workspace_with(Role::Editor);
        let (owner_key, owner) = owner();
        assert!(
            peer.governance_mut()
                .put_saved_presets(&[preset("Secret plan")], &owner_key, at())
                .unwrap()
        );
        assert!(
            peer.governance_mut()
                .put_saved_presets(&[], &owner_key, at())
                .unwrap()
        );
        assert!(
            peer.governance_mut()
                .declare_member(guest.user_public_key, crate::join::Role::Editor, true, None, &owner_key, at())
                .unwrap()
        );
        assert!(ever_held(&document_of(&peer), "Secret plan"));
        assert!(peer.compact_workspace(&guest, &key, at()).unwrap());

        let mut copy = newcomer();
        handed(&mut copy, owner.device_public_key, peer.workspace_message())
            .expect("the administrator's compaction, carried by the device it joined through");
        let held = document_of(&copy);
        assert_eq!(
            compaction::compacted_by(&held, WORKSPACE, &owner.user_public_key),
            Ok(guest.user_public_key),
            "the history starts from the administrator's signed compaction"
        );
        assert!(!ever_held(&held, "Secret plan"), "the withdrawn set never reaches it");
        assert!(copy.governance().saved_presets().is_empty());
    }
}

tilo_test_env::scenario! {
    fn a_change_of_a_replaced_history_never_refuses_the_new_historys_number;

    phase the_new_change_ahead_of_its_parent_waits_and_the_old_one_goes;
    // A reader catching up from what its holder retained takes changes of the
    // history a base replaced after it took the base: each waits for a change
    // of that history, which never comes. The base's history numbers its
    // writers' changes again from the base, under each writer's actor of the
    // base's generation, and its change at a waiting old change's number can
    // itself come ahead of what it builds on. It is taken in to wait, never
    // refused, and the old one goes.
    {
        let (mut peer, _, _) = workspace_with(Role::Editor);
        let (owner_key, owner) = owner();
        let replaced = peer.workspace_actor_for(&owner.device_public_key);
        for name in ["One", "Two", "Three", "Four"] {
            assert!(
                peer.governance_mut()
                    .put_saved_presets(&[preset(name)], &owner_key, at())
                    .unwrap()
            );
        }
        let old = document_of(&peer).get_changes(&[]);
        assert!(peer.compact_workspace(&owner, &owner_key, at()).unwrap());
        let mut reader = newcomer();
        handed(&mut reader, owner.device_public_key, peer.workspace_message()).unwrap();
        let base = document_of(&peer).get_heads();
        let actor = peer.workspace_actor_for(&owner.device_public_key);
        assert_ne!(actor, replaced, "the base's generation numbers the device apart");
        for name in ["Five", "Six", "Seven"] {
            assert!(
                peer.governance_mut()
                    .put_saved_presets(&[preset(name)], &owner_key, at())
                    .unwrap()
            );
        }
        let new = document_of(&peer).get_changes(&base);
        let ahead = new.get(1..).expect("the base's history goes on");
        let numbered = ahead
            .iter()
            .find(|change| change.actor_id() == &actor)
            .expect("the owner's device writes the base's history");
        let superseded = old
            .iter()
            .find(|change| change.actor_id() == &replaced && change.seq() == numbered.seq())
            .expect("the history the base replaced holds the same number")
            .clone();
        let one = |changes: Vec<automerge::Change>| NeighbourMsg::SyncResp {
            changes: changes
                .into_iter()
                .map(crate::workspace::sent_change)
                .collect(),
            workspace: WORKSPACE.into(),
        };
        let from = TransportOrigin::RetainedServerDelta(owner.device_public_key);
        reader
            .receive_from(from, one(vec![superseded]), at())
            .expect("the old change, as a reader catching up replays it");
        reader
            .receive_from(from, one(ahead.to_vec()), at())
            .expect("the base's change at the old one's number, ahead of its parent");
        // The history it builds on, as the holder sends it once more.
        reader
            .receive_from(from, one(new.clone()), at())
            .expect("the base's history whole");
        assert_eq!(reader.workspace_heads(), peer.workspace_heads());
        let names = |peer: &StatePeer| {
            peer.governance()
                .saved_presets()
                .into_iter()
                .map(|preset| preset.name)
                .collect::<Vec<_>>()
        };
        assert_eq!(names(&reader), names(&peer));
    }
}
