//! A workspace's governance sent to a device that holds part of it already: only the changes
//! that do not build into the heads it holds.
//!
//! A device that enters a workspace takes its governance once, signs its own admission on it, and
//! then proves itself a member at the same host. The proof's answers carry the governance again,
//! and a member list of a few hundred people is a third of a megabyte each time. The device names
//! the heads of what it holds ([`heads`]); the host answers with the rest ([`trimmed`]), or with
//! the whole where it does not hold every head named, as when the list moved to a newer base
//! meanwhile; and the device puts the two together ([`received`]).
//!
//! Governance travels as encoded neighbour messages, each a `SyncResp` of the workspace
//! document's changes ([`crate::StatePeer::workspace_message`]).

use std::collections::{BTreeMap, BTreeSet};

use crate::join::JoinTransportResponse;
use neighbour_wire::NeighbourMsg;

/// The workspace `messages` are governance of, and their changes in order, decoded.
fn changes_of(messages: &[Vec<u8>]) -> Result<(String, Vec<automerge::Change>), String> {
    let unreadable = |error: &dyn std::fmt::Display| {
        format!("the workspace's governance does not read: {error}")
    };
    let mut workspace = None;
    let mut changes = Vec::new();
    for message in messages {
        let NeighbourMsg::SyncResp {
            changes: carried,
            workspace: named,
        } = postcard::from_bytes(message).map_err(|error| unreadable(&error))?
        else {
            return Err(
                "the workspace's governance holds a message that is not its changes".into(),
            );
        };
        if workspace.get_or_insert_with(|| named.clone()) != &named {
            return Err("the workspace's governance is two workspaces'".into());
        }
        for bytes in carried {
            changes.push(
                automerge::Change::try_from(bytes.as_slice())
                    .map_err(|error| unreadable(&error))?,
            );
        }
    }
    let workspace = workspace.ok_or("the workspace's governance is empty")?;
    Ok((workspace, changes))
}

/// `changes` of `workspace` as one governance message.
fn message(workspace: String, changes: Vec<automerge::Change>) -> Result<Vec<u8>, String> {
    postcard::to_stdvec(&NeighbourMsg::SyncResp {
        changes: changes
            .into_iter()
            .map(crate::workspace::sent_change)
            .collect(),
        workspace,
    })
    .map_err(|error| format!("the workspace's governance does not encode: {error}"))
}

/// The heads of the governance `messages` hold: the changes no other among them builds on.
///
/// # Errors
///
/// They are not a workspace's changes.
pub fn heads(messages: &[Vec<u8>]) -> Result<Vec<[u8; 32]>, String> {
    let (_, changes) = changes_of(messages)?;
    let built_on: BTreeSet<_> = changes
        .iter()
        .flat_map(|change| change.deps().to_vec())
        .collect();
    let heads: BTreeSet<[u8; 32]> = changes
        .iter()
        .map(automerge::Change::hash)
        .filter(|hash| !built_on.contains(hash))
        .map(|hash| hash.0)
        .collect();
    Ok(heads.into_iter().collect())
}

/// The governance `messages` hold, less what builds into `heads`: `None` where one of `heads` is
/// not among them, and the device that named them is sent the whole.
///
/// # Errors
///
/// They are not a workspace's changes, or what is left does not encode.
pub fn since(messages: &[Vec<u8>], heads: &[[u8; 32]]) -> Result<Option<Vec<Vec<u8>>>, String> {
    let (workspace, changes) = changes_of(messages)?;
    let deps: BTreeMap<[u8; 32], Vec<[u8; 32]>> = changes
        .iter()
        .map(|change| {
            (
                change.hash().0,
                change.deps().iter().map(|dep| dep.0).collect(),
            )
        })
        .collect();
    let mut held = BTreeSet::new();
    let mut reaching: Vec<[u8; 32]> = heads.to_vec();
    while let Some(hash) = reaching.pop() {
        if !held.insert(hash) {
            continue;
        }
        let Some(built_on) = deps.get(&hash) else {
            return Ok(None);
        };
        reaching.extend(built_on.iter().copied());
    }
    let rest = changes
        .into_iter()
        .filter(|change| !held.contains(&change.hash().0))
        .collect();
    Ok(Some(vec![message(workspace, rest)?]))
}

/// `held`, governance a device holds, with `added`, the changes it was sent since it, as one
/// message: added changes it holds already are left out.
///
/// # Errors
///
/// Either is not a workspace's changes, they are two workspaces', or the result does not encode.
pub fn merged(held: &[Vec<u8>], added: &[Vec<u8>]) -> Result<Vec<Vec<u8>>, String> {
    let (workspace, mut changes) = changes_of(held)?;
    if !added.is_empty() {
        let (named, more) = changes_of(added)?;
        if named != workspace {
            return Err("the governance sent is another workspace's".to_owned());
        }
        let mut known: BTreeSet<_> = changes.iter().map(automerge::Change::hash).collect();
        changes.extend(
            more.into_iter()
                .filter(|change| known.insert(change.hash())),
        );
    }
    Ok(vec![message(workspace, changes)?])
}

/// `held`, governance a device holds, with `changes`, the workspace changes it signed on top of
/// it and the workspace took in: what it holds now.
///
/// # Errors
///
/// As [`merged`].
pub fn with_own(held: &[Vec<u8>], changes: &[Vec<u8>]) -> Result<Vec<Vec<u8>>, String> {
    let (workspace, _) = changes_of(held)?;
    let own = postcard::to_stdvec(&NeighbourMsg::SyncResp {
        changes: changes.to_vec(),
        workspace,
    })
    .map_err(|error| format!("the workspace's governance does not encode: {error}"))?;
    merged(held, &[own])
}

/// The governance an answer carries, where it carries any.
fn governance_of(response: &mut JoinTransportResponse) -> Option<&mut Vec<Vec<u8>>> {
    match response {
        JoinTransportResponse::Challenge(answer) => Some(&mut answer.governance),
        JoinTransportResponse::MembershipChallenge(answer) => Some(&mut answer.governance),
        JoinTransportResponse::Complete(bootstrap)
        | JoinTransportResponse::MembershipComplete(bootstrap) => {
            Some(&mut bootstrap.governance_messages)
        }
        JoinTransportResponse::OwnerMembershipComplete(bootstrap) => {
            Some(&mut bootstrap.governance_messages)
        }
        JoinTransportResponse::EntryList { governance } => Some(governance),
        JoinTransportResponse::Inspection(_)
        | JoinTransportResponse::Error { .. }
        | JoinTransportResponse::ResolvedInvitation { .. }
        | JoinTransportResponse::Since { .. } => None,
    }
}

/// `response`, the host's answer to a request asked since `heads`, with the governance it carries
/// cut to what does not build into them, as [`JoinTransportResponse::Since`]: the whole, under
/// no heads, where the governance does not hold every one of them.
#[must_use]
pub fn trimmed(mut response: JoinTransportResponse, heads: Vec<[u8; 32]>) -> JoinTransportResponse {
    let mut heads = heads;
    if let Some(governance) = governance_of(&mut response) {
        match since(governance, &heads) {
            Ok(Some(rest)) => *governance = rest,
            Ok(None) => heads.clear(),
            Err(message) => response = JoinTransportResponse::Error { code: 500, message },
        }
    }
    JoinTransportResponse::Since {
        heads,
        response: Box::new(response),
    }
}

/// The host's answer to a request this device asked since the heads of `held`, the governance it
/// holds, with that governance put back into what the answer carries: the answer as the request
/// alone would have had it.
///
/// # Errors
///
/// The answer was cut since other heads than `held`'s, or the governance does not read.
pub fn received(
    response: JoinTransportResponse,
    held: &[Vec<u8>],
) -> Result<JoinTransportResponse, String> {
    let JoinTransportResponse::Since {
        heads: since_heads,
        response,
    } = response
    else {
        return Ok(response);
    };
    let mut response = *response;
    if since_heads.is_empty() {
        return Ok(response);
    }
    if since_heads != heads(held)? {
        return Err("the host answered since governance this device does not hold".to_owned());
    }
    if let Some(governance) = governance_of(&mut response) {
        *governance = merged(held, governance)?;
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::transaction::Transactable as _;

    fn governance(
        doc: &mut automerge::AutoCommit,
        since: &[automerge::ChangeHash],
    ) -> Vec<Vec<u8>> {
        vec![message("w".to_owned(), doc.get_changes(since)).unwrap()]
    }

    fn write(doc: &mut automerge::AutoCommit, key: &str) {
        doc.put(automerge::ROOT, key, 1).unwrap();
        doc.commit();
    }

    /// A device that holds part of the governance is sent the rest, and puts the two together
    /// into the whole the host holds; one that names a head the host does not hold is sent the
    /// whole.
    #[test]
    fn a_device_is_sent_only_what_it_lacks() {
        let mut doc = automerge::AutoCommit::new();
        for key in ["a", "b", "c"] {
            write(&mut doc, key);
        }
        let held = governance(&mut doc, &[]);
        let held_heads = heads(&held).unwrap();
        assert_eq!(held_heads, vec![doc.get_heads()[0].0]);
        let before = doc.get_heads();
        write(&mut doc, "d");
        let whole = governance(&mut doc, &[]);

        let rest = since(&whole, &held_heads).unwrap().unwrap();
        assert_eq!(
            rest,
            governance(&mut doc, &before),
            "only the change after the heads"
        );
        let together = merged(&held, &rest).unwrap();
        assert_eq!(heads(&together).unwrap(), heads(&whole).unwrap());
        assert_eq!(changes_of(&together).unwrap().1.len(), 4);

        assert_eq!(
            since(&whole, &[[9; 32]]).unwrap(),
            None,
            "an unknown head gets the whole"
        );

        let answered = trimmed(
            JoinTransportResponse::EntryList {
                governance: whole.clone(),
            },
            held_heads,
        );
        let JoinTransportResponse::EntryList { governance: back } =
            received(answered, &held).unwrap()
        else {
            panic!("the answer is the list");
        };
        assert_eq!(heads(&back).unwrap(), heads(&whole).unwrap());

        let whole_again = trimmed(
            JoinTransportResponse::EntryList {
                governance: whole.clone(),
            },
            vec![[9; 32]],
        );
        assert_eq!(
            received(whole_again, &held).unwrap(),
            JoinTransportResponse::EntryList { governance: whole }
        );
    }
}
