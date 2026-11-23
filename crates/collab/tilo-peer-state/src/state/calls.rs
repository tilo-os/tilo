//! What a workspace allows in each of its calls.
//!
//! The owner, and whoever the workspace grants `call`, end a call, restart it, deny a person or
//! let them back, and set what everyone in it may do ([`collab_types::calls::CallChange`]). The
//! workspace document keeps one rule per channel, so every member reads the same rule, and every
//! receiver weighs each change by its writer's grants, as it weighs every change to who may do
//! what. No device runs a call: who is in one is what the devices in it say of themselves, and
//! every member's live session judges their media by the rule. A call's media is sealed under the
//! key group of its rule's generation, whose keys only the devices of the people the rule admits
//! are given ([`StatePeer::document_key_holders`]).

use std::collections::BTreeMap;

use automerge::{ROOT, ReadDoc, transaction::Transactable};
use collab_types::calls::{CallChange, CallRule};
use collab_types::signer::DeviceSigner;
use tilo_action_policy::roles::governance;
use tilo_user_identity::DeviceCert;

use super::facades::PeerGovernance;
use super::{StatePeer, SystemTime, err};
use crate::workspace_membership::WorkspaceMembershipCommand;

/// The map of the workspace document that keeps what each call allows, by channel.
pub(crate) const CALLS: &str = "calls_v1";

/// What each call allows, by channel, and the person each member device is.
pub type CallGate = (BTreeMap<String, CallRule>, BTreeMap<[u8; 32], String>);

impl StatePeer {
    /// Apply a call command; `None` for every other command.
    pub(super) fn apply_call_command(
        &mut self,
        caller: &DeviceCert,
        command: &WorkspaceMembershipCommand,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Option<Result<bool, String>> {
        let WorkspaceMembershipCommand::Call { channel, change } = command else {
            return None;
        };
        Some(self.apply_call_change(caller, channel, change, key, now))
    }

    fn apply_call_change(
        &mut self,
        caller: &DeviceCert,
        channel: &str,
        change: &CallChange,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        let member = self.membership(caller.device_public_key, now)?;
        member
            .policy
            .check_action(
                &member.authority(),
                governance::CALL,
                &tilo_action_policy::Context::at_time(now),
                false,
            )
            .map_err(|refusal| refusal.to_string())?;
        let before_change = self.governance().call(channel);
        let rule = before_change.changed(change)?;
        if rule == before_change {
            return Ok(false);
        }
        let mut doc = self
            .core
            .workspace_doc
            .clone()
            .ok_or("this device holds no workspace document")?;
        let before = doc.get_heads();
        doc.set_actor(self.workspace_actor_of(key));
        // A workspace founded before calls kept rules has no map for them: the first rule makes it.
        let calls = match doc.get(ROOT, CALLS).map_err(err)? {
            Some((_, calls)) => calls,
            None => doc
                .put_object(ROOT, CALLS, automerge::ObjType::Map)
                .map_err(err)?,
        };
        doc.put(
            &calls,
            channel,
            automerge::ScalarValue::Bytes(serde_json::to_vec(&rule).map_err(err)?),
        )
        .map_err(err)?;
        self.write_governance(doc, &before, key, now)
    }
}

impl PeerGovernance<'_> {
    /// What the call in `channel` allows now: what the workspace keeps for it, or everything for
    /// a channel nobody changed.
    #[must_use]
    pub fn call(self, channel: &str) -> CallRule {
        let Some(doc) = self.peer.core.workspace_doc.as_ref() else {
            return CallRule::default();
        };
        let Ok(Some((_, calls))) = doc.get(ROOT, CALLS) else {
            return CallRule::default();
        };
        read_rule(&**doc, &calls, channel).unwrap_or_default()
    }

    /// What each call allows, by channel, and the person each member device is: what a live
    /// session judges the workspace's call media by
    /// (`tilo_live::live_session::LiveSession::set_calls`).
    ///
    /// # Errors
    /// The workspace's members do not read.
    pub fn call_gate(self, now: SystemTime) -> Result<CallGate, String> {
        let list = self.governed_members(now)?;
        let persons = list
            .members
            .iter()
            .filter(|member| !member.removed)
            .filter_map(|member| {
                let person = member.id.person_id()?.to_owned();
                let devices = list.devices.get(&member.id)?;
                Some(devices.iter().map(move |device| (*device, person.clone())))
            })
            .flatten()
            .collect();
        Ok((self.calls(), persons))
    }

    /// What each call the workspace changed allows, by channel.
    #[must_use]
    pub fn calls(self) -> BTreeMap<String, CallRule> {
        let Some(doc) = self.peer.core.workspace_doc.as_ref() else {
            return BTreeMap::new();
        };
        let Ok(Some((_, calls))) = doc.get(ROOT, CALLS) else {
            return BTreeMap::new();
        };
        doc.keys(&calls)
            .filter_map(|channel| Some((channel.clone(), read_rule(&**doc, &calls, &channel)?)))
            .collect()
    }
}

/// The rule kept under `channel`, when it reads as one.
fn read_rule(doc: &impl ReadDoc, calls: &automerge::ObjId, channel: &str) -> Option<CallRule> {
    match doc.get(calls, channel).ok()?? {
        (automerge::Value::Scalar(value), _) => match value.as_ref() {
            automerge::ScalarValue::Bytes(bytes) => tilo_failure::ignored!(
                "a rule is only ever written from a CallRule, so bytes that read as none are no rule",
                serde_json::from_slice(bytes).ok()
            ),
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use collab_types::calls::{CallChange, Directions, media_document};
    use collab_types::scope::MEDIA_APP;

    use crate::StatePeer;
    use crate::workspace_membership::WorkspaceMembershipCommand as Command;

    /// A call's media keys go to the devices of the people its rule admits, in the rule's current
    /// generation only: denied, a person's devices hold none of the next generation's keys and
    /// leave the earlier one's group; let back, they hold the generation after, and never that of
    /// the time they were left out; and an ended call's keys go to nobody.
    #[test]
    fn a_calls_media_keys_go_only_to_the_people_its_rule_admits() {
        let (mut peer, key, _, device, now) = crate::admission_tests::seeded();
        let person = device.user_public_key;
        let holders = |peer: &StatePeer, generation| {
            peer.document_key_holders(MEDIA_APP, &media_document("standup", generation), now)
                .unwrap()
        };
        let everyone = holders(&peer, 0);
        assert!(everyone.contains(&device.device_public_key));
        assert!(
            peer.requires_document_keys(MEDIA_APP, &media_document("standup", 0), now)
                .unwrap(),
            "a call's media seals under keys of its own"
        );
        assert!(
            holders(&peer, 1).is_empty(),
            "no keys of a generation ahead of the rule"
        );
        let change = |peer: &mut StatePeer, change| {
            let command = Command::Call {
                channel: "standup".into(),
                change,
            };
            assert_eq!(
                peer.workspace_mut()
                    .apply_workspace_membership_command(&device, &command, &key, now),
                Ok(true)
            );
        };
        change(
            &mut peer,
            CallChange::Deny {
                person,
                denied: Directions::BOTH,
            },
        );
        assert_eq!(peer.governance().call("standup").generation, 1);
        assert!(
            holders(&peer, 1).is_empty(),
            "the denied person holds none of its keys"
        );
        assert!(
            holders(&peer, 0).is_empty(),
            "and the earlier group moves on past them"
        );
        change(&mut peer, CallChange::Allow { person });
        assert_eq!(
            holders(&peer, 2),
            everyone,
            "let back, they hold the next keys"
        );
        assert!(
            holders(&peer, 1).is_empty(),
            "and none of the keys of what was said while they were left out"
        );
        change(&mut peer, CallChange::End);
        assert!(
            holders(&peer, 3).is_empty(),
            "an ended call's keys go to nobody"
        );
    }
}
