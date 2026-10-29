//! Membership changes this device writes itself: its people, their roles,
//! the room's grants and its agents.
use crate::{NativeWorkspaceOwner, RoomId};
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use tilo_owner_commit::{CommitOperation, OwnerCommitReceipt, OwnerCommitRequest};
use web_time::SystemTime;

const MAX_COMMANDS: usize = 4096;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct StoredMembershipReceipt {
    pub(crate) receipt: OwnerCommitReceipt,
}

impl NativeWorkspaceOwner {
    /// Commit a typed membership command as this device, signed with its
    /// own `key`: the room's admission decides it against the caller's
    /// grants, here and on every device that receives it. A device that
    /// governs the room publishes its governance as a checkpoint; any
    /// other member publishes the changes it made, which the room's host
    /// admits as it admits every member's. Exact retries converge; changed
    /// invocation reuse never changes policy.
    ///
    /// # Errors
    /// Refuses a key that is not this device's, a change the caller's
    /// grants do not allow, a change only the room's governor commits,
    /// exhausted custody or failed durable publication.
    pub fn commit_workspace_membership_command(
        &self,
        request: OwnerCommitRequest,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<OwnerCommitReceipt, String> {
        use tilo_peer_state::workspace_membership::WorkspaceMembershipCommand as Command;
        let binding = request.binding()?;
        let room = RoomId::parse(&request.room).ok_or("invalid native membership room")?;
        let CommitOperation::WorkspaceMembershipCommand {
            invocation,
            caller,
            command,
        } = request.operation
        else {
            return Err("unsupported native membership owner commit family".into());
        };
        let _turn = self.0.mutation.lock();
        let mut candidate = self.with_current(room, |view| Ok(view.clone()))?;
        if key.verifying_key().to_bytes() != candidate.device().device_public_key {
            return Err("membership mutation is not signed by this device".into());
        }
        let governs = candidate.device() == candidate.governor();
        if !governs
            && !matches!(
                command,
                Command::Member { .. }
                    | Command::Settings(_)
                    | Command::Grant { .. }
                    | Command::Revoke { .. }
                    | Command::AddAgent { .. }
                    | Command::RemoveAgent { .. }
                    | Command::GrantAppAuthorship { .. }
                    | Command::Share { .. }
                    | Command::Unshare { .. }
            )
        {
            return Err("only the room's governor commits this change".into());
        }
        let id = serde_json::to_string(&(caller.device_public_key, invocation))
            .map_err(|error| error.to_string())?;
        let before = candidate.peer.room_heads().unwrap_or_default();
        // Always revalidate current authority, including retries. Apply to a
        // clone so retry validation cannot publish another mutation.
        let mut peer = candidate.peer.clone();
        peer.workspace_mut()
            .apply_workspace_membership_command(&caller, &command, key, now)?;
        if let Some(existing) = candidate.record.workspace_memberships.get(&id) {
            if existing.receipt.binding != binding {
                return Err("membership invocation was already bound to another command".into());
            }
            return Ok(existing.receipt.clone());
        }
        if candidate.record.workspace_memberships.len() >= MAX_COMMANDS {
            return Err("native membership custody capacity exhausted".into());
        }
        let next_generation = candidate
            .generation()
            .checked_add(1)
            .ok_or("native membership generation exhausted")?;
        let written = if governs {
            // Its durable state includes the signed room policy and admitted
            // graph; no legacy cache or relay acceptance stands in for it.
            let messages = peer.publication_mut().durable_mutation_messages(
                &candidate.peer,
                key,
                now,
                next_generation,
            )?;
            for message in &messages {
                if matches!(message, neighbour_wire::NeighbourMsg::AppDocument { .. }) {
                    let outgoing =
                        crate::publication::prepare(&crate::publication::request(message)?)?;
                    crate::publication::reserve(&candidate, &outgoing)?;
                    crate::publication::append_local(&mut candidate, outgoing)?;
                }
            }
            Vec::new()
        } else {
            peer.room_changes_since(&before)
        };
        candidate.peer = peer;
        candidate.record.peer = candidate.peer.durable()?;
        candidate.record.generation = next_generation;
        let receipt = OwnerCommitReceipt {
            binding,
            generation: next_generation,
            aggregate_hash: *blake3::hash(&candidate.record.peer).as_bytes(),
        };
        candidate.record.workspace_memberships.insert(
            id,
            StoredMembershipReceipt {
                receipt: receipt.clone(),
            },
        );
        if governs {
            crate::publication::replace_governance_checkpoint(
                &mut candidate,
                receipt.binding.clone(),
                now,
            )?;
        } else if !written.is_empty() {
            crate::publication::add_member_changes(
                &mut candidate,
                written,
                receipt.binding.clone(),
                now,
            )?;
        }
        self.publish_room(room, candidate, None)?;
        Ok(receipt)
    }
}

#[cfg(test)]
#[path = "workspace_app_authorship_tests.rs"]
mod app_authorship_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application_record::tests::{Fixture, fixture};
    use neighbour_wire::NeighbourMsg;
    use tilo_action_policy::roles::AgentPreset;
    use tilo_peer_state::TransportOrigin;
    use tilo_peer_state::workspace_membership::WorkspaceMembershipCommand;

    fn request(fixture: &Fixture, command: WorkspaceMembershipCommand) -> OwnerCommitRequest {
        OwnerCommitRequest {
            room: crate::testing::ROOM.into(),
            operation: CommitOperation::WorkspaceMembershipCommand {
                invocation: uuid::Uuid::new_v4(),
                caller: fixture.certificate.clone(),
                command,
            },
        }
    }

    fn agent_caps(
        owner: &NativeWorkspaceOwner,
        fixture: &Fixture,
    ) -> Vec<tilo_action_policy::Grant> {
        owner
            .with_current(RoomId::parse(crate::testing::ROOM).unwrap(), |view| {
                let authority = view
                    .peer()
                    .permissions(fixture.certificate.device_public_key, fixture.now)?;
                Ok(authority
                    .policy
                    .grants
                    .into_iter()
                    .filter(|grant| grant.subject == tilo_action_policy::Subject::agent("scribe"))
                    .collect())
            })
            .unwrap()
    }

    #[test]
    fn membership_commit_is_canonical_durable_and_exact_retry_is_idempotent() {
        let fixture = fixture();
        let request = request(
            &fixture,
            WorkspaceMembershipCommand::AddAgent {
                agent: "scribe".into(),
                preset: AgentPreset::Observer,
            },
        );
        assert!(agent_caps(&fixture.owner, &fixture).is_empty());
        let receipt = fixture
            .owner
            .commit_workspace_membership_command(request.clone(), &fixture.key, fixture.now)
            .unwrap();
        receipt.validate_for(&request).unwrap();
        let caps = agent_caps(&fixture.owner, &fixture);
        assert_eq!(caps.len(), 2, "reading the room, and no governing it");
        assert_eq!(
            caps[0].node,
            tilo_action_policy::TreePath::room(crate::testing::ROOM)
        );
        assert_eq!(caps[0].kinds, tilo_action_policy::KindSet::READ);
        assert_eq!(caps[0].answer, tilo_action_policy::Answer::Allow);
        let reopened = crate::open_file(&fixture.directory.path().join("owner.json")).unwrap();
        assert_eq!(agent_caps(&reopened, &fixture), caps);
        let retry = reopened
            .commit_workspace_membership_command(request.clone(), &fixture.key, fixture.now)
            .unwrap();
        assert_eq!(retry.generation, receipt.generation);
        assert_eq!(retry.aggregate_hash, receipt.aggregate_hash);
        let mut changed = request;
        if let CommitOperation::WorkspaceMembershipCommand { command, .. } = &mut changed.operation
        {
            *command = WorkspaceMembershipCommand::RemoveAgent {
                agent: "scribe".into(),
            };
        }
        assert!(
            reopened
                .commit_workspace_membership_command(changed, &fixture.key, fixture.now,)
                .is_err()
        );
        assert_eq!(agent_caps(&reopened, &fixture), caps);
    }

    #[test]
    fn removal_derives_all_agent_grants_and_wrong_signer_never_mutates() {
        let fixture = fixture();
        fixture
            .owner
            .commit_workspace_membership_command(
                request(
                    &fixture,
                    WorkspaceMembershipCommand::AddAgent {
                        agent: "scribe".into(),
                        preset: AgentPreset::Operator,
                    },
                ),
                &fixture.key,
                fixture.now,
            )
            .unwrap();
        let before = agent_caps(&fixture.owner, &fixture);
        assert!(!before.is_empty());
        let removal = request(
            &fixture,
            WorkspaceMembershipCommand::RemoveAgent {
                agent: "scribe".into(),
            },
        );
        assert!(
            fixture
                .owner
                .commit_workspace_membership_command(
                    removal.clone(),
                    &SigningKey::from_bytes(&[240; 32]),
                    fixture.now,
                )
                .is_err()
        );
        assert_eq!(agent_caps(&fixture.owner, &fixture), before);
        fixture
            .owner
            .commit_workspace_membership_command(removal, &fixture.key, fixture.now)
            .unwrap();
        assert!(agent_caps(&fixture.owner, &fixture).is_empty());
        let reopened = crate::open_file(&fixture.directory.path().join("owner.json")).unwrap();
        assert!(agent_caps(&reopened, &fixture).is_empty());
    }

    #[test]
    fn changed_caller_certificate_cannot_borrow_governor_signing_custody() {
        let fixture = fixture();
        let mut add = request(
            &fixture,
            WorkspaceMembershipCommand::AddAgent {
                agent: "scribe".into(),
                preset: AgentPreset::Observer,
            },
        );
        if let CommitOperation::WorkspaceMembershipCommand { caller, .. } = &mut add.operation {
            caller.device_label.push_str(" forged");
        }
        assert!(
            fixture
                .owner
                .commit_workspace_membership_command(add, &fixture.key, fixture.now,)
                .is_err()
        );
        assert!(agent_caps(&fixture.owner, &fixture).is_empty());
    }

    #[test]
    fn durable_membership_reconciles_to_a_reopenable_governor_checkpoint() {
        let mut fixture = fixture();
        let room = RoomId::parse(crate::testing::ROOM).unwrap();
        let add = request(
            &fixture,
            WorkspaceMembershipCommand::AddAgent {
                agent: "scribe".into(),
                preset: AgentPreset::Contributor,
            },
        );
        fixture
            .owner
            .commit_workspace_membership_command(add, &fixture.key, fixture.now)
            .unwrap();
        fixture
            .owner
            .reconcile_outgoing(room, &fixture.key, fixture.now)
            .unwrap();

        let reopened = crate::open_file(&fixture.directory.path().join("owner.json")).unwrap();
        let frames = reopened
            .with_outgoing(room, fixture.now, |_, outgoing| {
                Ok(outgoing
                    .entries()
                    .map(|entry| entry.bytes().to_vec())
                    .collect::<Vec<_>>())
            })
            .unwrap();
        assert!(
            !frames.is_empty(),
            "reconciliation must retain a checkpoint"
        );
        for frame in frames {
            let message: NeighbourMsg = postcard::from_bytes(&frame).unwrap();
            fixture
                .governor
                .receive(
                    TransportOrigin::RetainedRelayDelta(fixture.certificate.device_public_key),
                    message,
                )
                .unwrap();
        }
        assert_eq!(
            fixture.governor.agent_grants("scribe").unwrap(),
            [
                "agent-member:scribe:contributor",
                "agent-member:scribe:contributor:comments",
                "agent-member:scribe:contributor:no-governance",
                "agent-member:scribe:contributor:reactions",
            ]
        );
    }

    #[test]
    fn a_device_that_does_not_govern_publishes_its_own_changes_until_governance_carries_them() {
        let mut fixture = crate::application_record::tests::owner_device_fixture();
        let room = RoomId::parse(crate::testing::ROOM).unwrap();
        let outgoing = |owner: &NativeWorkspaceOwner| {
            owner
                .with_outgoing(room, fixture.now, |_, outgoing| {
                    Ok(outgoing
                        .entries()
                        .map(|entry| entry.bytes().to_vec())
                        .collect::<Vec<_>>())
                })
                .unwrap()
        };
        let add = request(
            &fixture,
            WorkspaceMembershipCommand::AddAgent {
                agent: "scribe".into(),
                preset: AgentPreset::Observer,
            },
        );
        fixture
            .owner
            .commit_workspace_membership_command(add, &fixture.key, fixture.now)
            .unwrap();
        let caps = agent_caps(&fixture.owner, &fixture);
        assert_eq!(caps.len(), 2, "written here at once");
        let frames = outgoing(&fixture.owner);
        assert_eq!(frames.len(), 1, "one room delta to publish");
        let message: NeighbourMsg = postcard::from_bytes(&frames[0]).unwrap();
        assert!(matches!(message, NeighbourMsg::SyncResp { .. }));
        fixture
            .governor
            .receive(
                TransportOrigin::RetainedRelayDelta(fixture.certificate.device_public_key),
                message,
            )
            .unwrap();
        let mut granted = fixture.governor.agent_grants("scribe").unwrap();
        granted.sort();
        let mut held: Vec<_> = caps
            .iter()
            .map(|grant| grant.id.as_str().to_owned())
            .collect();
        held.sort();
        assert_eq!(
            granted, held,
            "the room's host admits what the device wrote"
        );

        let reopened = crate::open_file(&fixture.directory.path().join("owner.json")).unwrap();
        assert_eq!(outgoing(&reopened).len(), 1, "kept until it is admitted");
        fixture.governor.refresh_checkpoint().unwrap();
        let governor = fixture.governor.governor().device_public_key;
        for (bytes, _) in fixture.governor.outgoing() {
            reopened
                .receive(
                    room,
                    TransportOrigin::RetainedRelayDelta(governor),
                    postcard::from_bytes(bytes).unwrap(),
                    fixture.now,
                )
                .unwrap();
        }
        assert!(
            outgoing(&reopened).is_empty(),
            "the room's governance carries it"
        );
        assert_eq!(agent_caps(&reopened, &fixture), caps);
    }

    #[test]
    fn failed_membership_publication_never_changes_the_committed_policy() {
        let fixture = fixture();
        let add = request(
            &fixture,
            WorkspaceMembershipCommand::AddAgent {
                agent: "scribe".into(),
                preset: AgentPreset::Operator,
            },
        );
        let path = fixture.directory.path().join("owner.json");
        std::fs::rename(&path, fixture.directory.path().join("retained-owner.json")).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(
            fixture
                .owner
                .commit_workspace_membership_command(add, &fixture.key, fixture.now,)
                .is_err()
        );
        assert!(agent_caps(&fixture.owner, &fixture).is_empty());
        let retained =
            crate::open_file(&fixture.directory.path().join("retained-owner.json")).unwrap();
        assert!(agent_caps(&retained, &fixture).is_empty());
    }
}
