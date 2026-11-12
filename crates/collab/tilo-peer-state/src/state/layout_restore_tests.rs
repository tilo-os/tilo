//! A device's stored state from before the compact governance layout opens
//! with its governance brought up, once.
use super::*;
use crate::workspace::layout::tests::{crowded, document, earlier};

/// `durable`, a state this build wrote, as a build before the compact
/// governance layout stored it: base version 4 around `governance`.
fn stored_before(durable: &[u8], governance: &[u8]) -> Vec<u8> {
    let (envelope, _) = postcard::take_from_bytes::<u32>(durable).unwrap();
    let before = |mut value: DurableV4| {
        value.base.version = GOVERNANCE_LAYOUT_BEFORE;
        value.base.workspace_bytes = Some(governance.to_vec());
        value
    };
    match envelope {
        4 => postcard::to_stdvec(&before(postcard::from_bytes(durable).unwrap())).unwrap(),
        5 => {
            let value: DurableV5<'_> = postcard::from_bytes(durable).unwrap();
            postcard::to_stdvec(&DurableV5 {
                version: value.version,
                base: before(value.base),
                exact: value.exact,
            })
            .unwrap()
        }
        other => panic!("fixture: envelope {other}"),
    }
}

#[test]
fn a_state_stored_before_the_compact_governance_opens_brought_up() {
    let (peer, _, now, guests) = crowded(3);
    let owner = peer.owner_certificates()[0].clone();
    let workspace = peer.workspace_id().to_owned();
    let stored = stored_before(
        &peer.durable_whole().unwrap(),
        &earlier(&document(&peer)).save(),
    );
    assert_eq!(
        durable_base_version(&stored).unwrap(),
        GOVERNANCE_LAYOUT_BEFORE
    );
    let open = |bytes: &[u8]| {
        StatePeer::restore(
            bytes,
            &workspace,
            owner.clone(),
            vec![owner.clone()],
            RevocationList::new(),
            now,
        )
        .unwrap()
    };
    let restored = open(&stored);
    assert!(restored.governance_brought_up());
    for guest in &guests {
        assert_eq!(
            restored
                .permissions(guest.device_public_key, now)
                .unwrap()
                .role,
            Role::Editor,
            "a guest the door let in stays in"
        );
    }
    let generation = |peer: &StatePeer| {
        crate::workspace::base::generation(peer.core.workspace_doc.as_deref().unwrap())
    };
    assert_eq!(generation(&restored), generation(&peer) + 2);
    // Written again, it is this layout, and nothing is brought up twice.
    let again = open(&restored.durable().unwrap());
    assert!(!again.governance_brought_up());
    assert_eq!(generation(&again), generation(&restored));

    // The device that governs it starts it over from a base newer than any
    // a member's device brought up for itself.
    let mut governing = again;
    assert!(governing.start_governance_over().unwrap());
    assert_eq!(generation(&governing), generation(&restored) + 1);
}
