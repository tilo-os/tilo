//! A device admitting itself to a workspace: the one step every way in takes.
//!
//! No device signs another into a workspace unless its own person holds a role
//! on the signed member list. A device joining with an invitation signs its own
//! redemption (`Admit { invite, person }` of its own person, the invitation its
//! authority) and its own device row; a further device of a person the list
//! names signs its own device row, admissible because its certificate chains to
//! a root the list names. Whoever carries the change to the workspace (a holder
//! answering the join, a server keeping it for another person, a link to any of
//! its devices) passes those signed operations on byte for byte, and every
//! receiver judges them under their author
//! ([`crate::workspace::admit_workspace_changes`]).
//!
//! Each function here takes the workspace's governance as a join, membership
//! or entry answer carries it (encoded neighbour messages), writes the device's
//! own admission on top of it, signed by the device's key at `now`, and answers
//! the workspace changes to send: none when the workspace lists the device so
//! already. [`sign_own_admission`] and [`sign_further_device_row`] take any signer,
//! a page's too, which signs outside the turn; [`own_admission`] and
//! [`further_device_row`] are the same step for a key that signs at once. A caller
//! that must resend exactly what it sent keeps what it was answered.

use crate::StatePeer;
use crate::join::VerifiedRedemption;
use crate::owner::write::signed_turn;
use collab_types::signer::DeviceSigner;
use tilo_user_identity::DeviceCert;
use web_time::SystemTime;

/// Write `write` on `own`'s copy of the workspace as `governance` makes it,
/// signed by `key`, and answer the workspace changes it made: none when the
/// workspace holds what it would write already.
fn written(
    workspace: &str,
    governance: &[Vec<u8>],
    own: &DeviceCert,
    key: &dyn DeviceSigner,
    write: impl FnOnce(&mut StatePeer, &dyn DeviceSigner) -> Result<bool, String>,
    now: SystemTime,
) -> Result<Vec<Vec<u8>>, String> {
    if key.device_public_key() != own.device_public_key {
        return Err("a device signs only its own admission".into());
    }
    let mut peer = StatePeer::unanchored(workspace, own, governance, now)?;
    let heads = peer
        .workspace_heads()
        .ok_or("the workspace's governance is missing")?;
    if !write(&mut peer, key)? {
        return Ok(Vec::new());
    }
    Ok(peer.workspace_changes_since(&heads))
}

/// The changes admitting `redemption`'s device to `workspace`, signed by that
/// device's `key` ([`crate::PeerGovernanceMut::admit_named_member`]): its
/// redemption of the invitation, its device row, and `name`, the name it
/// chose, when one is given. A key its person made for an agent or a program
/// records its own grant beside them, under that name
/// ([`crate::PeerGovernanceMut::author_own_grant`]).
///
/// # Errors
/// The governance does not read, `key` is not the redeeming device's, or the
/// workspace refuses the redemption as every receiver would.
pub fn own_admission(
    workspace: &str,
    governance: &[Vec<u8>],
    redemption: &VerifiedRedemption,
    name: Option<&str>,
    key: &dyn DeviceSigner,
    now: SystemTime,
) -> Result<Vec<Vec<u8>>, String> {
    let own = redemption.certificate();
    // A key's grant is listed by the name it joined as, the name its person
    // approved it under.
    let joined_as = name;
    // A blank name is none given, and an agent's or a stand-in's
    // certificate names no person: the person stays as their invitation and
    // certificate name them.
    let name = name
        .filter(|name| !name.trim().is_empty())
        .filter(|_| crate::join_log::names_its_person(own));
    written(
        workspace,
        governance,
        own,
        key,
        |peer, key| {
            let admitted = peer
                .governance_mut()
                .admit_named_member(redemption, name, key, now)?;
            let granted = peer
                .governance_mut()
                .author_own_grant(own, joined_as, key, now)?;
            Ok(admitted || granted)
        },
        now,
    )
}

/// [`own_admission`] signed by `signer`, which may sign outside the turn.
///
/// # Errors
/// As [`own_admission`], or `signer` does not sign.
pub async fn sign_own_admission(
    workspace: &str,
    governance: &[Vec<u8>],
    redemption: &VerifiedRedemption,
    name: Option<&str>,
    signer: &dyn DeviceSigner,
    now: SystemTime,
) -> Result<Vec<Vec<u8>>, String> {
    signed_turn(signer, |signatures| {
        own_admission(workspace, governance, redemption, name, signatures, now)
    })
    .await
}

/// The changes admitting `own`, a further device of a person the workspace
/// lists, as that person's device: its own row, signed by its `key`
/// ([`crate::PeerGovernanceMut::admit_member_device`]), and a key's own grant
/// beside it ([`crate::PeerGovernanceMut::author_own_grant`]).
///
/// # Errors
/// The governance does not read, `key` is not `own`'s, or `own`'s person is
/// no member.
pub fn further_device_row(
    workspace: &str,
    governance: &[Vec<u8>],
    own: &DeviceCert,
    key: &dyn DeviceSigner,
    now: SystemTime,
) -> Result<Vec<Vec<u8>>, String> {
    written(
        workspace,
        governance,
        own,
        key,
        |peer, key| {
            let listed = peer.governance_mut().admit_member_device(own, key, now)?;
            let granted = peer
                .governance_mut()
                .author_own_grant(own, None, key, now)?;
            Ok(listed || granted)
        },
        now,
    )
}

/// The changes admitting `own` through `workspace`'s entry: its own row and
/// its entry admission, signed by its own key, the entry the governance
/// carries being the grant. No invitation is behind them and no host signed
/// them.
///
/// # Errors
/// The governance does not read, `key` is not `own`'s, the entry takes nobody
/// who asks, or the workspace refuses it as every receiver would.
pub fn entry_admission(
    workspace: &str,
    governance: &[Vec<u8>],
    own: &DeviceCert,
    request_id: [u8; 32],
    name: Option<&str>,
    key: &dyn DeviceSigner,
    now: SystemTime,
) -> Result<Vec<Vec<u8>>, String> {
    written(
        workspace,
        governance,
        own,
        key,
        |peer, key| {
            let entry = peer.governance().governed_entry();
            if entry.open_role().is_none() {
                return Err("this workspace's entry does not take people who ask".into());
            }
            let digest = crate::state::entry_digest(&entry)?;
            let admitted = peer
                .governance_mut()
                .admit_entered_member(own, digest, request_id, name, key, now)?;
            let granted = peer
                .governance_mut()
                .author_own_grant(own, name, key, now)?;
            Ok(admitted || granted)
        },
        now,
    )
}

/// [`entry_admission`] signed by `signer`, which may sign outside the turn.
///
/// # Errors
/// As [`entry_admission`], or `signer` does not sign.
pub async fn sign_entry_admission(
    workspace: &str,
    governance: &[Vec<u8>],
    own: &DeviceCert,
    request_id: [u8; 32],
    name: Option<&str>,
    signer: &dyn DeviceSigner,
    now: SystemTime,
) -> Result<Vec<Vec<u8>>, String> {
    signed_turn(signer, |signatures| {
        entry_admission(
            workspace, governance, own, request_id, name, signatures, now,
        )
    })
    .await
}

/// Whether `own`'s person is one the `governance` messages make a member of
/// `workspace`: a further device of theirs then comes in as theirs, and not
/// through the entry.
fn person_is_member(
    workspace: &str,
    governance: &[Vec<u8>],
    own: &DeviceCert,
    now: SystemTime,
) -> bool {
    crate::StatePeer::unanchored(workspace, own, governance, now).is_ok_and(|peer| {
        peer.governance()
            .governed_policy()
            .is_ok_and(|policy| policy.role_of(&hex::encode(own.user_public_key)).is_some())
    })
}

/// [`further_device_row`] signed by `signer`, which may sign outside the turn.
///
/// # Errors
/// As [`further_device_row`], or `signer` does not sign.
pub async fn sign_further_device_row(
    workspace: &str,
    governance: &[Vec<u8>],
    own: &DeviceCert,
    signer: &dyn DeviceSigner,
    now: SystemTime,
) -> Result<Vec<Vec<u8>>, String> {
    signed_turn(signer, |signatures| {
        further_device_row(workspace, governance, own, signatures, now)
    })
    .await
}

/// What `own`'s device signs to come into `workspace` when its host answers
/// that it may, on `governance`: its redemption of `invitation` under
/// `request_id`, named `name` when one is given, where it comes with an
/// invitation; its own row as a further device of someone the workspace has
/// in it otherwise.
///
/// # Errors
/// The invitation does not admit this device, the governance does not read,
/// or `signer` does not sign.
pub async fn sign_entry(
    workspace: &str,
    governance: &[Vec<u8>],
    (invitation, request_id): (Option<&[u8]>, [u8; 32]),
    name: Option<&str>,
    (own, signer): (&DeviceCert, &dyn DeviceSigner),
    now: SystemTime,
) -> Result<Vec<Vec<u8>>, String> {
    match invitation {
        Some(invitation) => {
            let redemption = crate::join::verify_connection_redemption(
                invitation,
                own,
                request_id,
                &tilo_user_identity::RevocationList::new(),
                crate::state::now_ns(now),
            )?;
            sign_own_admission(workspace, governance, &redemption, name, signer, now).await
        }
        // A device of a person the workspace has in it brings itself in as
        // theirs; a stranger comes in under the workspace's entry, naming the
        // entry the governance it was sent holds.
        None if person_is_member(workspace, governance, own, now) => {
            sign_further_device_row(workspace, governance, own, signer, now).await
        }
        None => {
            sign_entry_admission(workspace, governance, own, request_id, name, signer, now).await
        }
    }
}
