//! Two desktops of one person holding the same freshly seeded workspace, with
//! no server between them: the shape the workspace state's tests need to prove
//! something works in an ad-hoc group.

use crate::owner::testing::receive;
use crate::{NeighbourMsg, SeedState, StatePeer, seed_workspace};
use ed25519_dalek::SigningKey;
use tilo_user_identity::{DeviceCert, RevocationList};

use crate::fixture::{at, primary_cert};
pub(crate) const WORKSPACE: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";

/// Two desktops, their keys and the fixture instant.
pub(crate) fn desktops() -> (StatePeer, SigningKey, StatePeer, SigningKey) {
    let root = SigningKey::from_bytes(&[1; 32]);
    let left_key = SigningKey::from_bytes(&[2; 32]);
    let right_key = SigningKey::from_bytes(&[3; 32]);
    let left_cert = primary_cert(&root, &left_key, "left");
    let right_cert = primary_cert(&root, &right_key, "right");
    let roster = vec![left_cert.clone(), right_cert.clone()];
    let seed = seed_workspace(WORKSPACE, &left_cert, &roster).unwrap();
    let mut left = StatePeer::new(
        WORKSPACE,
        left_cert,
        roster.clone(),
        RevocationList::new(),
        at(),
    )
    .unwrap();
    left.install_seed(
        SeedState {
            workspace_changes: seed.clone(),
            documents: Vec::new(),
        },
        &left_key,
        at(),
    )
    .unwrap();
    let mut right =
        StatePeer::new(WORKSPACE, right_cert, roster, RevocationList::new(), at()).unwrap();
    right
        .install_seed(
            SeedState {
                workspace_changes: seed,
                documents: Vec::new(),
            },
            &right_key,
            at(),
        )
        .unwrap();
    (left, left_key, right, right_key)
}

/// The certificate `peer`'s own device presents.
pub(crate) fn cert_of(peer: &StatePeer) -> DeviceCert {
    peer.certificate(&peer.core.origin, at()).unwrap().clone()
}

/// Hand `from`'s workspace changes since what `to` holds to `to`.
pub(crate) fn sync(from: &StatePeer, to: &mut StatePeer) {
    let from_cert = cert_of(from);
    let changes = from.workspace_changes_since(&to.workspace_heads().unwrap_or_default());
    receive(
        to,
        &from_cert,
        NeighbourMsg::SyncResp {
            changes,
            workspace: WORKSPACE.into(),
        },
    )
    .unwrap();
}
