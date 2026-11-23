//! Membership changes this device writes itself: its people, their roles,
//! the workspace's grants and its agents.
use crate::owner::{NativeWorkspaceOwner, WorkspaceId};
use crate::owner_commit::{CommitOperation, OwnerCommitReceipt, OwnerCommitRequest};
use crate::workspace_membership::WorkspaceMembershipCommand;
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
    /// its ownership of its key, outside the owner's turn: the workspace's admission decides it against the caller's
    /// grants, here and on every device that receives it. A device of the
    /// workspace's owner publishes the governance as a checkpoint; any other
    /// member publishes the changes it made, which every key holder admits
    /// as it admits every member's. Exact retries converge; changed
    /// invocation reuse never changes policy.
    pub async fn commit_workspace_membership_command(
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
            let id = receipt_id(&caller, invocation, &command)?;
            let receipts = &candidate.record.workspace_memberships;
            let kept = receipts.get(&id).map(|kept| kept.receipt.clone());
            if let Some(replayed) = replayed(kept.as_ref(), &binding, &command)? {
                return Ok(replayed);
            }
            if kept.is_none() && receipts.len() >= MAX_COMMANDS {
                return Err("native membership ownership capacity exhausted".into());
            }
            // What this device signs, it publishes: a device of the workspace's owner
            // its governance as a checkpoint, carrying its durable state; any
            // other member the changes it made, which every key holder admits.
            let writes_checkpoint = candidate
                .peer
                .signed_by_governor(&candidate.device().device_public_key);
            let previous = candidate.peer.clone();
            let (receipt, record) = candidate.change(|live, record| {
                let before = live.workspace_heads().unwrap_or_default();
                // A device that changes the workspace's governance drops the names it keeps for
                // nobody first, as its host does: a removal and a naming merged from two devices
                // leave one, and governance holding it reads nowhere, so no change is made on it.
                live.governance_mut().drop_names_of_nobody(key, now)?;
                live.workspace_mut()
                    .apply_workspace_membership_command(&caller, &command, key, now)?;
                let next_generation = record
                    .generation
                    .checked_add(1)
                    .ok_or("native membership generation exhausted")?;
                if writes_checkpoint {
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
                }
                // What the command changed in the workspace's governance: what
                // a member publishes for the workspace's holders to admit.
                let changes = live.workspace_changes_since(&before);
                key.settled()?;
                // A lease's claim renewed to the same deadline changes nothing:
                // its receipt stays the one its publication is bound to.
                if let Some(kept) = kept.filter(|_| changes.is_empty()) {
                    return Ok(kept);
                }
                record.generation = next_generation;
                let receipt = OwnerCommitReceipt {
                    binding: binding.clone(),
                    generation: next_generation,
                    aggregate_hash: live.durable_hash()?,
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
                } else if !changes.is_empty() {
                    crate::owner::publication::add_member_changes(
                        live,
                        record,
                        changes,
                        receipt.binding.clone(),
                        now,
                    )?;
                }
                Ok(receipt)
            })?;
            self.publish_document(workspace, &candidate, record, None)?;
            Ok(receipt)
        })
        .await
    }
}

/// The key a command's receipt is kept under. A lease's claim is renewed
/// for as long as the work goes on: its latest claim is the one receipt kept
/// for it, so renewing never piles up receipts.
fn receipt_id(
    caller: &tilo_user_identity::DeviceCert,
    invocation: uuid::Uuid,
    command: &WorkspaceMembershipCommand,
) -> Result<String, String> {
    match command {
        WorkspaceMembershipCommand::Lease { id, .. } => {
            serde_json::to_string(&(caller.device_public_key, "lease", id))
        }
        WorkspaceMembershipCommand::KeepDone { id, .. } => {
            serde_json::to_string(&(caller.device_public_key, "done", id))
        }
        WorkspaceMembershipCommand::AnswerSchedule {
            automation,
            trigger,
            ..
        } => serde_json::to_string(&(caller.device_public_key, "schedule", automation, trigger)),
        _ => serde_json::to_string(&(caller.device_public_key, invocation)),
    }
    .map_err(|error| error.to_string())
}

/// The receipt an exact retry of `binding` is answered with, `kept` under
/// its command's key. Another command under an invocation's key is refused;
/// a lease's next claim replaces the one kept.
fn replayed(
    kept: Option<&OwnerCommitReceipt>,
    binding: &crate::owner_commit::CommitBinding,
    command: &WorkspaceMembershipCommand,
) -> Result<Option<OwnerCommitReceipt>, String> {
    match kept {
        Some(kept) if kept.binding == *binding => Ok(Some(kept.clone())),
        Some(_)
            if !matches!(
                command,
                WorkspaceMembershipCommand::Lease { .. }
                    | WorkspaceMembershipCommand::AnswerSchedule { .. }
                    | WorkspaceMembershipCommand::KeepDone { .. }
            ) =>
        {
            Err("membership invocation was already bound to another command".into())
        }
        _ => Ok(None),
    }
}

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
                tilo_action_policy::Grantee::agent("scribe"),
                tilo_action_policy::TreePath::workspace(crate::owner::testing::WORKSPACE),
                tilo_action_policy::KindSet::READ,
                tilo_action_policy::Answer::Allow,
                tilo_action_policy::Grantee::person("owner"),
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
                        .iter()
                        .filter(|grant| {
                            grant.grantee == tilo_action_policy::Grantee::agent("scribe")
                        })
                        .cloned()
                        .collect())
                },
            )
            .unwrap()
    }

    #[tokio::test]
    async fn membership_commit_is_canonical_durable_and_exact_retry_is_idempotent() {
        let fixture = fixture();
        let request = request(&fixture, agent_grant());
        assert!(agent_caps(&fixture.owner, &fixture).is_empty());
        let receipt = fixture
            .owner
            .commit_workspace_membership_command(request.clone(), &fixture.key, fixture.now)
            .await
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
            .await
            .unwrap();
        assert_eq!(retry.generation, receipt.generation);
        assert_eq!(retry.aggregate_hash, receipt.aggregate_hash);
        let mut changed = request;
        if let CommitOperation::WorkspaceMembershipCommand { command, .. } = &mut changed.operation
        {
            *command = WorkspaceMembershipCommand::Revoke {
                grant: tilo_action_policy::GrantId::new("another"),
            };
        }
        assert!(
            reopened
                .commit_workspace_membership_command(changed, &fixture.key, fixture.now,)
                .await
                .is_err()
        );
        assert_eq!(agent_caps(&reopened, &fixture), caps);
    }

    #[tokio::test]
    async fn revoke_takes_the_grant_back_and_a_wrong_signer_never_mutates() {
        let fixture = fixture();
        fixture
            .owner
            .commit_workspace_membership_command(
                request(&fixture, agent_grant()),
                &fixture.key,
                fixture.now,
            )
            .await
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
                .await
                .is_err()
        );
        assert_eq!(agent_caps(&fixture.owner, &fixture), before);
        fixture
            .owner
            .commit_workspace_membership_command(removal, &fixture.key, fixture.now)
            .await
            .unwrap();
        assert!(agent_caps(&fixture.owner, &fixture).is_empty());
        let reopened =
            crate::owner::open_file(&fixture.directory.path().join("owner.json")).unwrap();
        assert!(agent_caps(&reopened, &fixture).is_empty());
    }

    /// A grant the session records is a member command of the workspace
    /// ([`WorkspaceMembershipCommand::Member`]): the commit answers, and the governance every device
    /// decides against carries the grant.
    #[tokio::test]
    async fn a_member_command_carries_a_grant_into_the_policy() {
        let fixture = fixture();
        let workspace = crate::owner::testing::WORKSPACE;
        let person = hex::encode(fixture.certificate.user_public_key);
        let grant = tilo_action_policy::Grant::new(
            "app-action:clear",
            tilo_action_policy::Grantee::person(&person),
            tilo_action_policy::TreePath::workspace(workspace),
            tilo_action_policy::KindSet::ALL,
            tilo_action_policy::Answer::Allow,
            tilo_action_policy::Grantee::person(&person),
        );
        let command = WorkspaceMembershipCommand::Member {
            change: tilo_action_policy::Change::Grant(grant),
        };
        fixture
            .owner
            .commit_workspace_membership_command(
                request(&fixture, command),
                &fixture.key,
                fixture.now,
            )
            .await
            .unwrap();
        let policy = fixture
            .owner
            .with_current(WorkspaceId::parse(workspace).unwrap(), |view| {
                view.peer().governance().governed_policy()
            })
            .unwrap();
        assert!(
            policy
                .grants
                .iter()
                .any(|held| held.id.as_str() == "app-action:clear"),
            "the grant is in force: {policy:?}"
        );
    }

    #[tokio::test]
    async fn changed_caller_certificate_cannot_borrow_governor_signing_ownership() {
        let fixture = fixture();
        let mut add = request(&fixture, agent_grant());
        if let CommitOperation::WorkspaceMembershipCommand { caller, .. } = &mut add.operation {
            caller.device_label.push_str(" forged");
        }
        assert!(
            fixture
                .owner
                .commit_workspace_membership_command(add, &fixture.key, fixture.now,)
                .await
                .is_err()
        );
        assert!(agent_caps(&fixture.owner, &fixture).is_empty());
    }

    #[tokio::test]
    async fn durable_membership_retires_to_a_reopenable_governor_checkpoint() {
        let mut fixture = fixture();
        let workspace = WorkspaceId::parse(crate::owner::testing::WORKSPACE).unwrap();
        let add = request(&fixture, agent_grant());
        fixture
            .owner
            .commit_workspace_membership_command(add, &fixture.key, fixture.now)
            .await
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
                    TransportOrigin::RetainedServerDelta(fixture.certificate.device_public_key),
                    message,
                )
                .unwrap();
        }
        assert_eq!(
            fixture.governor.agent_grants("scribe").unwrap(),
            ["agent-member:scribe"]
        );
    }

    #[tokio::test]
    async fn a_device_that_does_not_govern_publishes_its_own_changes_until_governance_carries_them()
    {
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
            .await
            .unwrap();
        let caps = agent_caps(&fixture.owner, &fixture);
        assert_eq!(caps.len(), 1, "written here at once");
        let frames = outgoing(&fixture.owner);
        assert_eq!(
            frames.len(),
            1,
            "one workspace delta to publish: {frames:?}"
        );
        let message: NeighbourMsg = postcard::from_bytes(&frames[0]).unwrap();
        assert!(matches!(message, NeighbourMsg::SyncResp { .. }));
        fixture
            .governor
            .receive(
                TransportOrigin::RetainedServerDelta(fixture.certificate.device_public_key),
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
                    TransportOrigin::RetainedServerDelta(governor),
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

    /// A member device that does work for a workspace — keeps its VM going — claims
    /// the work's lease as any member writes: its claim reaches the
    /// workspace's governor, which reads it as the holder, and renewing it keeps
    /// one receipt rather than one per renewal.
    #[tokio::test]
    async fn a_member_device_claims_a_lease_and_the_workspaces_governor_reads_it() {
        let fixture = crate::owner::fixture::member_device_fixture();
        let workspace = WorkspaceId::parse(crate::owner::testing::WORKSPACE).unwrap();
        let claim = |ttl_ms| {
            request(
                &fixture,
                WorkspaceMembershipCommand::Lease {
                    id: crate::vm_lease("default"),
                    ttl_ms,
                    rank: crate::unranked(),
                },
            )
        };
        let receipts = |owner: &NativeWorkspaceOwner| {
            owner
                .with_current(workspace, |view| {
                    Ok(view.record.workspace_memberships.len())
                })
                .unwrap()
        };
        for _ in 0..3 {
            fixture
                .owner
                .commit_workspace_membership_command(claim(60_000), &fixture.key, fixture.now)
                .await
                .unwrap();
        }
        assert_eq!(receipts(&fixture.owner), 1, "renewals keep one receipt");
        let holder = fixture
            .owner
            .with_current(workspace, |view| {
                Ok(view
                    .peer()
                    .workspace()
                    .lease(&crate::vm_lease("default"), fixture.now)
                    .holder)
            })
            .unwrap();
        assert_eq!(
            holder.map(|claim| claim.device),
            Some(fixture.certificate.device_public_key)
        );

        let mut governor = fixture.governor;
        let frames = fixture
            .owner
            .with_outgoing(workspace, fixture.now, |_, outgoing| {
                Ok(outgoing
                    .entries()
                    .map(|entry| entry.bytes().to_vec())
                    .collect::<Vec<_>>())
            })
            .unwrap();
        assert_eq!(
            frames.len(),
            1,
            "one workspace delta to publish: {frames:?}"
        );
        for frame in frames {
            let message: NeighbourMsg = postcard::from_bytes(&frame).unwrap();
            governor
                .receive(
                    TransportOrigin::RetainedServerDelta(fixture.certificate.device_public_key),
                    message,
                )
                .unwrap();
        }
        let held = governor
            .peer()
            .workspace()
            .lease(&crate::vm_lease("default"), fixture.now);
        assert_eq!(
            held.holder.map(|claim| claim.device),
            Some(fixture.certificate.device_public_key),
            "the workspace's governor reads the member's claim: {held:?}"
        );
    }

    #[tokio::test]
    async fn failed_membership_publication_never_changes_the_committed_policy() {
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
                .await
                .is_err()
        );
        assert!(agent_caps(&fixture.owner, &fixture).is_empty());
        let retained = crate::owner::open_file(&retained).unwrap();
        assert!(agent_caps(&retained, &fixture).is_empty());
    }

    #[tokio::test]
    async fn a_shortcut_with_a_step_is_committed_and_read_back() {
        use crate::workspace_membership::SettingsChange;
        use tilo_automation::ShortcutChange;

        let fixture = fixture();
        let workspace = crate::owner::testing::WORKSPACE;
        // What the Shortcuts app saves: a shortcut whose step carries its kind
        // flattened into the step's own map.
        let mut shortcut: tilo_automation::Shortcut = serde_json::from_value(serde_json::json!({
            "id": "tidy",
            "name": "Tidy",
            "steps": [{
                "id": "list",
                "step": "invoke",
                "action": {"app": "so.tilo.notes", "kind": "notes.folder", "action": "list"},
                "resource": {"fixed": "notes.folder:inbox"},
            }],
        }))
        .expect("a shortcut with a step");
        crate::workspace::sign_saved_shortcut(
            workspace,
            &mut shortcut,
            &fixture.certificate,
            &fixture.key,
        )
        .unwrap();
        let save = request(
            &fixture,
            WorkspaceMembershipCommand::Settings(SettingsChange::Shortcuts(ShortcutChange::Save(
                Box::new(shortcut.clone()),
            ))),
        );
        let receipt = fixture
            .owner
            .commit_workspace_membership_command(save.clone(), &fixture.key, fixture.now)
            .await
            .expect("the host binds a shortcut whose step has its kind flattened");
        receipt.validate_for(&save).unwrap();
        let held = fixture
            .owner
            .with_current(
                WorkspaceId::parse(workspace).expect("a workspace id"),
                |view| Ok(view.peer().workspace().workspace_shortcuts()),
            )
            .unwrap();
        assert_eq!(
            held,
            vec![tilo_automation::SavedShortcut::encode(&shortcut)],
            "the workspace holds the step the person saved"
        );
    }
}
