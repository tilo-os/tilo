//! Membership changes this device writes itself: its people, their roles,
//! the workspace's grants and its agents.
use crate::owner::{NativeWorkspaceOwner, WorkspaceId};
use crate::owner_commit::{CommitOperation, OwnerCommitReceipt, OwnerCommitRequest};
use collab_types::signer::DeviceSigner;
use serde::{Deserialize, Serialize};
use web_time::SystemTime;

const MAX_COMMANDS: usize = 4096;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct StoredMembershipReceipt {
    pub(crate) receipt: OwnerCommitReceipt,
}

impl NativeWorkspaceOwner {
    /// Commit a typed membership command as this device, signed by `signer`,
    /// its custody of its key, outside the owner's turn: the workspace's admission decides it against the caller's
    /// grants, here and on every device that receives it. A device of the
    /// workspace's owner publishes the governance as a checkpoint; any other
    /// member publishes the changes it made, which every participant admits
    /// as it admits every member's. Exact retries converge; changed
    /// invocation reuse never changes policy.
    pub fn commit_workspace_membership_command(
        &self,
        request: OwnerCommitRequest,
        signer: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<OwnerCommitReceipt, String> {
        let binding = request.binding()?;
        let workspace =
            WorkspaceId::parse(&request.workspace).ok_or("invalid native membership workspace")?;
        let CommitOperation::WorkspaceMembershipCommand {
            invocation,
            caller,
            command,
        } = request.operation
        else {
            return Err("unsupported native membership owner commit family".into());
        };
        crate::owner::write::signed_turn(signer, |key| {
            let _turn = self.0.mutation.lock();
            let candidate = self.view(workspace)?;
            if key.device_public_key() != candidate.device().device_public_key {
                return Err("membership mutation is not signed by this device".into());
            }
            let id = serde_json::to_string(&(caller.device_public_key, invocation))
                .map_err(|error| error.to_string())?;
            if let Some(existing) = candidate.record.workspace_memberships.get(&id) {
                if existing.receipt.binding != binding {
                    return Err("membership invocation was already bound to another command".into());
                }
                return Ok(existing.receipt.clone());
            }
            if candidate.record.workspace_memberships.len() >= MAX_COMMANDS {
                return Err("native membership custody capacity exhausted".into());
            }
            // What this device signs, it publishes: a device of the workspace's owner
            // its governance as a checkpoint, carrying its durable state; any
            // other member the changes it made, which every participant admits.
            let writes_checkpoint = candidate
                .peer
                .signed_by_owner(&candidate.device().device_public_key);
            let previous = candidate.peer.clone();
            let (receipt, record) = candidate.change(|live, record| {
                let before = live.workspace_heads().unwrap_or_default();
                live.workspace_mut()
                    .apply_workspace_membership_command(&caller, &command, key, now)?;
                let next_generation = record
                    .generation
                    .checked_add(1)
                    .ok_or("native membership generation exhausted")?;
                let written = if writes_checkpoint {
                    let messages = live.publication_mut().durable_mutation_messages(
                        &previous,
                        key,
                        now,
                        next_generation,
                    )?;
                    for message in &messages {
                        if matches!(message, neighbour_wire::NeighbourMsg::AppDocument { .. }) {
                            let outgoing = crate::owner::publication::prepare(
                                &crate::owner::publication::request(message)?,
                            )?;
                            crate::owner::publication::append_local(record, outgoing)?;
                        }
                    }
                    Vec::new()
                } else {
                    live.workspace_changes_since(&before)
                };
                key.settled()?;
                record.generation = next_generation;
                let receipt = OwnerCommitReceipt {
                    binding: binding.clone(),
                    generation: next_generation,
                    aggregate_hash: *blake3::hash(&live.durable()?).as_bytes(),
                };
                record.workspace_memberships.insert(
                    id.clone(),
                    StoredMembershipReceipt {
                        receipt: receipt.clone(),
                    },
                );
                if writes_checkpoint {
                    crate::owner::publication::replace_governance_checkpoint(
                        live,
                        record,
                        receipt.binding.clone(),
                        now,
                    )?;
                } else if !written.is_empty() {
                    crate::owner::publication::add_member_changes(
                        live,
                        record,
                        written,
                        receipt.binding.clone(),
                        now,
                    )?;
                }
                Ok(receipt)
            })?;
            self.publish_record(workspace, &candidate, record, None)?;
            Ok(receipt)
        })
    }
}

#[cfg(test)]
#[path = "workspace_app_authorship_tests.rs"]
mod app_authorship_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TransportOrigin;
    use crate::owner::fixture::{Fixture, fixture};
    use crate::workspace_membership::WorkspaceMembershipCommand;
    use neighbour_wire::NeighbourMsg;

    fn request(fixture: &Fixture, command: WorkspaceMembershipCommand) -> OwnerCommitRequest {
        OwnerCommitRequest {
            workspace: crate::owner::testing::WORKSPACE.into(),
            operation: CommitOperation::WorkspaceMembershipCommand {
                invocation: uuid::Uuid::new_v4(),
                caller: fixture.certificate.clone(),
                command,
            },
        }
    }

    /// The grant making `scribe` read the workspace, given by its person.
    fn agent_grant() -> WorkspaceMembershipCommand {
        WorkspaceMembershipCommand::Grant {
            grant: tilo_action_policy::Grant::new(
                "agent-member:scribe",
                tilo_action_policy::Subject::agent("scribe"),
                tilo_action_policy::TreePath::workspace(crate::owner::testing::WORKSPACE),
                tilo_action_policy::KindSet::READ,
                tilo_action_policy::Answer::Allow,
                tilo_action_policy::Subject::person("owner"),
            ),
        }
    }

    fn agent_caps(
        owner: &NativeWorkspaceOwner,
        fixture: &Fixture,
    ) -> Vec<tilo_action_policy::Grant> {
        owner
            .with_current(
                WorkspaceId::parse(crate::owner::testing::WORKSPACE).unwrap(),
                |view| {
                    let authority = view
                        .peer()
                        .permissions(fixture.certificate.device_public_key, fixture.now)
                        .map_err(|error| error.to_string())?;
                    Ok(authority
                        .policy
                        .grants
                        .into_iter()
                        .filter(|grant| {
                            grant.subject == tilo_action_policy::Subject::agent("scribe")
                        })
                        .collect())
                },
            )
            .unwrap()
    }

    #[test]
    fn membership_commit_is_canonical_durable_and_exact_retry_is_idempotent() {
        let fixture = fixture();
        let request = request(&fixture, agent_grant());
        assert!(agent_caps(&fixture.owner, &fixture).is_empty());
        let receipt = fixture
            .owner
            .commit_workspace_membership_command(request.clone(), &fixture.key, fixture.now)
            .unwrap();
        receipt.validate_for(&request).unwrap();
        let caps = agent_caps(&fixture.owner, &fixture);
        assert_eq!(caps.len(), 1, "the grant it was given");
        assert_eq!(
            caps[0].node,
            tilo_action_policy::TreePath::workspace(crate::owner::testing::WORKSPACE)
        );
        assert_eq!(caps[0].kinds, tilo_action_policy::KindSet::READ);
        assert_eq!(caps[0].answer, tilo_action_policy::Answer::Allow);
        let reopened =
            crate::owner::open_file(&fixture.directory.path().join("owner.json")).unwrap();
        assert_eq!(agent_caps(&reopened, &fixture), caps);
        let retry = reopened
            .commit_workspace_membership_command(request.clone(), &fixture.key, fixture.now)
            .unwrap();
        assert_eq!(retry.generation, receipt.generation);
        assert_eq!(retry.aggregate_hash, receipt.aggregate_hash);
        let mut changed = request;
        if let CommitOperation::WorkspaceMembershipCommand { command, .. } = &mut changed.operation
        {
            *command = WorkspaceMembershipCommand::RetiredRemoveAgent {
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
    fn revoke_takes_the_grant_back_and_a_wrong_signer_never_mutates() {
        let fixture = fixture();
        fixture
            .owner
            .commit_workspace_membership_command(
                request(&fixture, agent_grant()),
                &fixture.key,
                fixture.now,
            )
            .unwrap();
        let before = agent_caps(&fixture.owner, &fixture);
        assert!(!before.is_empty());
        let removal = request(
            &fixture,
            WorkspaceMembershipCommand::Revoke {
                grant: tilo_action_policy::GrantId::new("agent-member:scribe"),
            },
        );
        assert!(
            fixture
                .owner
                .commit_workspace_membership_command(
                    removal.clone(),
                    &ed25519_dalek::SigningKey::from_bytes(&[240; 32]),
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
        let reopened =
            crate::owner::open_file(&fixture.directory.path().join("owner.json")).unwrap();
        assert!(agent_caps(&reopened, &fixture).is_empty());
    }

    #[test]
    fn changed_caller_certificate_cannot_borrow_governor_signing_custody() {
        let fixture = fixture();
        let mut add = request(&fixture, agent_grant());
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
    fn durable_membership_retires_to_a_reopenable_governor_checkpoint() {
        let mut fixture = fixture();
        let workspace = WorkspaceId::parse(crate::owner::testing::WORKSPACE).unwrap();
        let add = request(&fixture, agent_grant());
        fixture
            .owner
            .commit_workspace_membership_command(add, &fixture.key, fixture.now)
            .unwrap();
        fixture.owner.retire_outgoing(workspace).unwrap();

        let reopened =
            crate::owner::open_file(&fixture.directory.path().join("owner.json")).unwrap();
        let frames = reopened
            .with_outgoing(workspace, fixture.now, |_, outgoing| {
                Ok(outgoing
                    .entries()
                    .map(|entry| entry.bytes().to_vec())
                    .collect::<Vec<_>>())
            })
            .unwrap();
        assert!(!frames.is_empty(), "retirement must retain a checkpoint");
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
            ["agent-member:scribe"]
        );
    }

    #[test]
    fn a_device_that_does_not_govern_publishes_its_own_changes_until_governance_carries_them() {
        let mut fixture = crate::owner::fixture::member_device_fixture();
        let workspace = WorkspaceId::parse(crate::owner::testing::WORKSPACE).unwrap();
        let outgoing = |owner: &NativeWorkspaceOwner| {
            owner
                .with_outgoing(workspace, fixture.now, |_, outgoing| {
                    Ok(outgoing
                        .entries()
                        .map(|entry| entry.bytes().to_vec())
                        .collect::<Vec<_>>())
                })
                .unwrap()
        };
        let add = request(&fixture, agent_grant());
        fixture
            .owner
            .commit_workspace_membership_command(add, &fixture.key, fixture.now)
            .unwrap();
        let caps = agent_caps(&fixture.owner, &fixture);
        assert_eq!(caps.len(), 1, "written here at once");
        let frames = outgoing(&fixture.owner);
        assert_eq!(frames.len(), 1, "one workspace delta to publish");
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
            "the workspace's host admits what the device wrote"
        );

        let reopened =
            crate::owner::open_file(&fixture.directory.path().join("owner.json")).unwrap();
        assert_eq!(outgoing(&reopened).len(), 1, "kept until it is admitted");
        fixture.governor.refresh_checkpoint().unwrap();
        let governor = fixture.governor.workspace_host().device_public_key;
        for (bytes, _) in fixture.governor.outgoing() {
            reopened
                .receive(
                    workspace,
                    TransportOrigin::RetainedRelayDelta(governor),
                    postcard::from_bytes(bytes).unwrap(),
                    fixture.now,
                )
                .unwrap();
        }
        assert!(
            outgoing(&reopened).is_empty(),
            "the workspace's governance carries it"
        );
        assert_eq!(agent_caps(&reopened, &fixture), caps);
    }

    #[test]
    fn failed_membership_publication_never_changes_the_committed_policy() {
        let fixture = fixture();
        let add = request(&fixture, agent_grant());
        let path = fixture.directory.path().join("owner.json");
        let parts = crate::owner::file_store::parts_of(&path);
        let retained = fixture.directory.path().join("retained-owner.json");
        // The record's index and the workspace files it names move together.
        std::fs::rename(&parts, crate::owner::file_store::parts_of(&retained)).unwrap();
        // A publication that cannot write its index fails.
        std::fs::create_dir(&parts).unwrap();
        std::fs::create_dir(crate::owner::file_store::index_of(&parts)).unwrap();
        assert!(
            fixture
                .owner
                .commit_workspace_membership_command(add, &fixture.key, fixture.now,)
                .is_err()
        );
        assert!(agent_caps(&fixture.owner, &fixture).is_empty());
        let retained = crate::owner::open_file(&retained).unwrap();
        assert!(agent_caps(&retained, &fixture).is_empty());
    }
}
