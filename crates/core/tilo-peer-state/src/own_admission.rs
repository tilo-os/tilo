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
//! [`sign_entry`] and [`sign_further_device_row`] also take what the device signed into the
//! workspace before, as its store keeps it, and build on it ([`written`]).

use crate::StatePeer;
use crate::join::VerifiedRedemption;
use crate::owner::write::signed_turn;
use collab_types::signer::DeviceSigner;
use tilo_user_identity::DeviceCert;
use web_time::SystemTime;

/// One key's turn to sign itself into one workspace: while a caller has it, no other signing by
/// that key there starts. A daemon brings one agent key into a workspace for each of its
/// connections that opens, and a server is handed a workspace while its session signs into it:
/// each reads what the key signed there before from its store, signs on top of it and keeps what
/// it signed, and two that overlapped would both read nothing and both sign the key's first
/// change. The turn is let go when what it gives is dropped.
pub async fn signing_turn(workspace: &str, device: [u8; 32]) -> tokio::sync::OwnedMutexGuard<()> {
    type Turns =
        std::collections::HashMap<(String, [u8; 32]), std::sync::Weak<tokio::sync::Mutex<()>>>;
    static TURNS: std::sync::LazyLock<parking_lot::Mutex<Turns>> =
        std::sync::LazyLock::new(Default::default);
    let turn = {
        let mut turns = TURNS.lock();
        turns.retain(|_, turn| turn.strong_count() > 0);
        let key = (workspace.to_owned(), device);
        if let Some(turn) = turns.get(&key).and_then(std::sync::Weak::upgrade) {
            turn
        } else {
            let turn = std::sync::Arc::new(tokio::sync::Mutex::new(()));
            turns.insert(key, std::sync::Arc::downgrade(&turn));
            turn
        }
    };
    turn.lock_owned().await
}

/// Why a device signs nothing on governance its own earlier admissions do not take on.
const SIGNED_ELSEWHERE: &str = "this device signed itself into the workspace already, on \
     governance the copy it signs on now does not build on: it signs once that copy holds it";

/// Write `write` on `own`'s copy of the workspace as `governance` makes it,
/// signed by `key`, and answer the workspace changes it made: none when the
/// workspace holds what it would write already.
///
/// `earlier` is what the device signed into the workspace before, as its store keeps it
/// ([`StatePeer::own_admissions`]). A device can be signed in twice: a server its person adds is
/// handed their workspace and signs its row on the copy it was handed, while its own session
/// redeems an invitation on what the person's device answered it, or signed in once before it
/// restarted. Each signs on the governance it was sent, and where that lacks the other's change
/// both are the key's first change: two changes under one number of one writer, which every
/// copy that meets both refuses. What `governance` lacks of `earlier` is taken in first and
/// answered with what is written: a copy that lists the device by it signs nothing new, and the
/// copy it is answered to takes the device's row in.
fn written(
    workspace: &str,
    (governance, earlier): (&[Vec<u8>], &[Vec<u8>]),
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
    if !earlier.is_empty() && !peer.governance().lists_device(own) {
        let hashes = earlier
            .iter()
            .map(|bytes| automerge::Change::try_from(bytes.as_slice()).map(|change| change.hash()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        // Read as one with the governance, as a copy that met both reads them: what the earlier
        // admissions build on comes with them, and may be any member's.
        let mut with_earlier = governance.to_vec();
        with_earlier.push(
            postcard::to_stdvec(&crate::NeighbourMsg::SyncResp {
                workspace: workspace.to_owned(),
                changes: earlier.to_vec(),
            })
            .map_err(|error| error.to_string())?,
        );
        peer = StatePeer::unanchored(workspace, own, &with_earlier, now)
            .map_err(|error| format!("{SIGNED_ELSEWHERE}: {error}"))?;
        if !hashes.iter().all(|hash| peer.workspace_holds_change(hash)) {
            return Err(SIGNED_ELSEWHERE.into());
        }
    }
    write(&mut peer, key)?;
    Ok(peer.workspace_changes_since(&heads))
}

/// One run of [`own_admission`] under `key`, as [`signed_turn`] makes it.
pub(crate) fn own_admission_turn(
    workspace: &str,
    signed_on: (&[Vec<u8>], &[Vec<u8>]),
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
        signed_on,
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
    own_admission_turn(workspace, (governance, &[]), redemption, name, key, now)
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
        own_admission_turn(
            workspace,
            (governance, &[]),
            redemption,
            name,
            signatures,
            now,
        )
    })
    .await
}

/// One run of [`further_device_row`] under `key`, as [`signed_turn`] makes it.
pub(crate) fn further_device_row_turn(
    workspace: &str,
    signed_on: (&[Vec<u8>], &[Vec<u8>]),
    own: &DeviceCert,
    key: &dyn DeviceSigner,
    now: SystemTime,
) -> Result<Vec<Vec<u8>>, String> {
    written(
        workspace,
        signed_on,
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
    further_device_row_turn(workspace, (governance, &[]), own, key, now)
}

/// The changes admitting `own` through `workspace`'s entry: its own row and
/// its entry admission, signed by its own key, the entry the governance
/// carries being the grant. No invitation is behind them and no host signed
/// them.
///
/// # Errors
/// The governance does not read, `key` is not `own`'s, the entry takes nobody
/// who asks, or the workspace refuses it as every receiver would.
pub(crate) fn entry_admission_turn(
    workspace: &str,
    signed_on: (&[Vec<u8>], &[Vec<u8>]),
    own: &DeviceCert,
    request_id: [u8; 32],
    name: Option<&str>,
    key: &dyn DeviceSigner,
    now: SystemTime,
) -> Result<Vec<Vec<u8>>, String> {
    written(
        workspace,
        signed_on,
        own,
        key,
        |peer, key| {
            let entry = peer.governance().workspace_entry();
            if entry.open_role().is_none() {
                return Err("this workspace's entry does not take people who ask".into());
            }
            let digest = crate::state::entry_digest(&entry)?;
            // As through an invitation: a key's grant is listed by the name it
            // asked under, and a blank name is none given, so a visitor who has
            // not named themselves yet still comes in.
            let joined_as = name;
            let name = name
                .filter(|name| !name.trim().is_empty())
                .filter(|_| crate::join_log::names_its_person(own));
            let admitted = peer
                .governance_mut()
                .admit_entered_member(own, digest, request_id, name, key, now)?;
            let granted = peer
                .governance_mut()
                .author_own_grant(own, joined_as, key, now)?;
            Ok(admitted || granted)
        },
        now,
    )
}

/// The changes admitting `own` through `workspace`'s entry, signed by
/// `signer`, which may sign outside the turn.
///
/// # Errors
/// The governance does not read, the entry takes nobody who asks, the
/// workspace refuses it as every receiver would, or `signer` does not sign.
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
        entry_admission_turn(
            workspace,
            (governance, &[]),
            own,
            request_id,
            name,
            signatures,
            now,
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
            .member_policy()
            .is_ok_and(|policy| policy.role_of(&hex::encode(own.user_public_key)).is_some())
    })
}

/// [`further_device_row`] signed by `signer`, which may sign outside the turn.
///
/// # Errors
/// As [`further_device_row`], or `signer` does not sign.
pub async fn sign_further_device_row(
    workspace: &str,
    (governance, earlier): (&[Vec<u8>], &[Vec<u8>]),
    own: &DeviceCert,
    signer: &dyn DeviceSigner,
    now: SystemTime,
) -> Result<Vec<Vec<u8>>, String> {
    signed_turn(signer, |signatures| {
        further_device_row_turn(workspace, (governance, earlier), own, signatures, now)
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
    (governance, earlier): (&[Vec<u8>], &[Vec<u8>]),
    (invitation, request_id): (Option<&[u8]>, [u8; 32]),
    name: Option<&str>,
    (own, signer): (&DeviceCert, &dyn DeviceSigner),
    now: SystemTime,
) -> Result<Vec<Vec<u8>>, String> {
    let signed_on = (governance, earlier);
    match invitation {
        Some(invitation) => {
            let redemption = crate::join::verify_connection_redemption(
                invitation,
                own,
                request_id,
                &tilo_user_identity::RevocationList::new(),
                crate::state::now_ns(now),
            )?;
            signed_turn(signer, |signatures| {
                own_admission_turn(workspace, signed_on, &redemption, name, signatures, now)
            })
            .await
        }
        // A device of a person the workspace has in it brings itself in as
        // theirs; a stranger comes in under the workspace's entry, naming the
        // entry the governance it was sent holds.
        None if person_is_member(workspace, governance, own, now) => {
            sign_further_device_row(workspace, signed_on, own, signer, now).await
        }
        None => {
            signed_turn(signer, |signatures| {
                entry_admission_turn(workspace, signed_on, own, request_id, name, signatures, now)
            })
            .await
        }
    }
}

/// Why a device signs nothing a host answered with: the change the answer names is not the one
/// the device asked for. A device signs only the change it chose, never one its host chose.
pub const NOT_ASKED: &str = "the host answered with a change this device did not ask for: it \
     signs only the change it asked for, and signed nothing";

/// The changes `own`'s device signs to make `command`, a governance change it asked its host
/// for and checked it asked for ([`crate::asked_signing`]), on `governance`, the workspace's
/// governance as the host answered it
/// ([`crate::PeerWorkspaceMut::apply_workspace_membership_command`]). What it signs is decided
/// under its own role and grants by every device that takes it in; the host only carries it.
///
/// # Errors
/// The governance does not read, `signer` is not `own`'s or does not sign, or the workspace
/// refuses the change as every receiver would.
pub async fn sign_command(
    workspace: &str,
    governance: &[Vec<u8>],
    command: &crate::workspace_membership::WorkspaceMembershipCommand,
    (own, signer): (&DeviceCert, &dyn DeviceSigner),
    now: SystemTime,
) -> Result<Vec<Vec<u8>>, String> {
    signed_turn(signer, |key| {
        written(
            workspace,
            (governance, &[]),
            own,
            key,
            |peer, key| {
                peer.workspace_mut()
                    .apply_workspace_membership_command(own, command, key, now)
            },
            now,
        )
    })
    .await
}
