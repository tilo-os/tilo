//! Current-room command admission in the serialized durable owner.
use crate::SessionOwner;
mod policy;
pub(crate) use policy::DesktopPolicyFence;
use tilo_action_broker::peer_actions::PolicyTime;
use tilo_desktop_model::{commands::*, *};
use tilo_room::owner_commit::{CommitOperation, OwnerCommitReceipt, OwnerCommitRequest};
use ui::automerge::{AutoCommit, ROOT, ReadDoc, transaction::Transactable};

pub(crate) fn checked_application(
    app: &str,
) -> Option<&'static ui::application::ApplicationDefinition> {
    crate::canonical_records::resolve(app).map(|entry| entry.definition())
}

fn desktop_source_matches(doc: &AutoCommit, heads: &[[u8; 32]]) -> Result<bool, String> {
    tilo_desktop_model::commands::desktop_source_matches(doc, heads)
}

impl SessionOwner {
    pub(crate) fn commit_desktop_command_with_policy_time(
        &mut self,
        request: &OwnerCommitRequest,
        time: &PolicyTime,
    ) -> Result<OwnerCommitReceipt, DesktopCommandError> {
        self.commit_desktop_command_current(request, Some(time))
    }
    /// Host-local broker capability. Definite validation refusals never enter
    /// the durability barrier; failures of that barrier remain uncertain.
    pub fn commit_desktop_command(
        &mut self,
        request: &OwnerCommitRequest,
    ) -> Result<OwnerCommitReceipt, DesktopCommandError> {
        self.commit_desktop_command_current(request, None)
    }
    fn commit_desktop_command_current(
        &mut self,
        request: &OwnerCommitRequest,
        time: Option<&PolicyTime>,
    ) -> Result<OwnerCommitReceipt, DesktopCommandError> {
        self.require_owner_resource_scope()
            .map_err(|e| DesktopCommandError::Rejected(e.to_string()))?;
        let rejected = DesktopCommandError::Rejected;
        let binding = request.binding().map_err(rejected)?;
        let now = crate::owner::policy_fence::sample(time).map_err(rejected)?;
        let (caller, command) = policy::current_caller(self, request, now).map_err(rejected)?;
        if let Some(receipt) = self.replay_desktop_creation_current(request, now, time)? {
            return Ok(receipt);
        }
        policy::topology(self, caller, command, now).map_err(rejected)?;
        let mut doc = self
            .peer()
            .admitted_room()
            .ok_or_else(|| rejected("room governance missing".into()))?
            .clone();
        let base_heads = doc.get_heads();
        let mut current = doc.get_heads().into_iter().map(|h| h.0).collect::<Vec<_>>();
        let mut expected = command.source.heads.clone();
        current.sort_unstable();
        expected.sort_unstable();
        // OpenApp is placement-independent: the owner places it against the
        // current topology below, so an older source revision cannot misplace it.
        let placement_independent = matches!(command.command, DesktopCommand::OpenApp { .. });
        if current != expected
            && !placement_independent
            && !desktop_source_matches(&doc, &command.source.heads).map_err(rejected)?
        {
            return Err(rejected("desktop source revision is stale".into()));
        }
        let owners = doc
            .get(ROOT, "contents_owners_v1")
            .map_err(|e| rejected(e.to_string()))?
            .ok_or_else(|| rejected("desktop identity custody is missing".into()))?
            .1;
        let owner = hex::encode(self.identity.native_cert.device_public_key);
        let salts = doc
            .keys(&owners)
            .filter_map(|key| {
                let salt = u16::from_str_radix(&key, 16).ok()?;
                let (value, _) = doc.get(&owners, &key).ok()??;
                (salt != 0 && value.as_str() == Some(owner.as_str())).then_some(salt)
            })
            .collect::<std::collections::BTreeSet<_>>();
        let salt = salts
            .first()
            .copied()
            .ok_or_else(|| rejected("native owner has no topology namespace".into()))?;
        let mut tree = DesktopTree::default();
        tilo_desktop_model::projection::apply_admitted_room(
            &mut tree,
            RoomId::parse(&request.room).ok_or_else(|| rejected("invalid room".into()))?,
            &doc,
        )
        .map_err(rejected)?;
        let workspace = tree
            .workspaces
            .first_mut()
            .ok_or_else(|| rejected("workspace missing".into()))?;
        // Never mutate another native peer's namespace, including process-local
        // surface notices. Owning room governance does not transfer those IDs.
        let owned = |id: u64| salts.contains(&tilo_desktop_model::peer_salt_of(id));
        let resolved = match &command.command {
            // The saver is the admitted caller, whatever the JSON claimed.
            DesktopCommand::SaveShortcut { shortcut_json } => {
                let (_, _, person) = self
                    .peer()
                    .permissions(caller.device_public_key, now)
                    .map_err(rejected)?;
                DesktopCommand::SaveShortcut {
                    shortcut_json: stamp_saver(shortcut_json, &person).map_err(rejected)?,
                }
            }
            other => resolve_open_app(workspace, other.clone(), owned).map_err(rejected)?,
        };
        check_owned(&resolved, owned).map_err(rejected)?;
        let stored = doc
            .get(ROOT, "desktop_counter_v1")
            .map_err(|e| rejected(e.to_string()))?
            .and_then(|(v, _)| v.as_u64())
            .unwrap_or(1);
        let mut ids =
            tilo_desktop_model::IdMinter::resume(salt, namespace_counter(workspace, salt, stored));
        let mut creation = None;
        let mut documents = vec![];
        let mut routes = tilo_desktop_model::apps::record_routes(&doc);
        tilo_desktop_model::commands::apply_command(
            workspace,
            &resolved,
            &mut || ids.mint_in_namespace(),
            &mut |app, id| {
                let application = crate::canonical_records::resolve(app).ok_or_else(|| {
                    "application has no installed checked record factory".to_owned()
                })?;
                let definition = application.definition();
                let now = crate::owner::policy_fence::sample(time)?;
                policy::current_caller(self, request, now)?;
                policy::topology(self, caller, command, now)?;
                creation = self.prepare_desktop_creation(request, app)?;
                let prepared = application.prepare_for_tab(self.peer(), caller, id, now)?;
                let record = prepared.record().to_owned();
                if let Some(document) = prepared.into_new_document() {
                    documents.push(document);
                }
                routes.insert(id, record);
                Ok((
                    definition.title.into(),
                    TileContent::AppPlaceholder {
                        kind: AppKind::Other(app.into()),
                        label: definition.title.into(),
                    },
                ))
            },
        )
        .map_err(rejected)?;
        doc.set_actor(ui::automerge::ActorId::from(
            self.identity.native_cert.device_public_key.to_vec(),
        ));
        for salt in salts {
            tilo_desktop_model::projection::project_owned_contents(
                &mut doc,
                &workspace.strips,
                salt,
                None,
                tilo_desktop_model::projection::EmptyStrips::Retain,
            )
            .map_err(|e| rejected(e.to_string()))?;
        }
        for strip in &workspace.strips {
            for group in &strip.window_groups {
                for tab in &group.tabs {
                    if let Some(record) = routes.get(&tab.id) {
                        tilo_desktop_model::apps::bind_tab_record(
                            &mut doc,
                            tilo_desktop_model::StripId(strip.id.0),
                            group.id,
                            tab.id,
                            record,
                        )
                        .map_err(|e| e.to_string())
                        .map_err(rejected)?;
                    }
                }
            }
        }
        doc.put(
            ROOT,
            tilo_desktop_model::projection::ROOM_DESKTOP_COUNTER_KEY,
            ids.next_counter(),
        )
        .map_err(|e| rejected(e.to_string()))?;
        if matches!(command.command, DesktopCommand::RenameWorkspace { .. }) {
            doc.put(ROOT, "name", &workspace.name)
                .map_err(|e| rejected(e.to_string()))?;
        }
        if matches!(
            resolved,
            DesktopCommand::SaveShortcut { .. } | DesktopCommand::DeleteShortcut { .. }
        ) {
            tilo_desktop_model::projection::project_workspace_shortcuts(
                &mut doc,
                &workspace.shortcuts,
            )
            .map_err(|e| rejected(e.to_string()))?;
        }
        doc.commit();
        let mut candidate = self.peer().clone();
        // Checked records enter the candidate before their topology is admitted.
        let mut creation_publications = vec![];
        for (app, record, bytes) in documents {
            let publication = candidate
                .publish_document(
                    &app,
                    &record,
                    bytes,
                    &self.identity.native,
                    crate::owner::policy_fence::sample(time).map_err(rejected)?,
                )
                .map_err(rejected)?;
            creation_publications.push(publication);
        }
        candidate
            .receive_from(
                tilo_room::TransportOrigin::RelayDelta(self.identity.native_cert.device_public_key),
                tilo_room::NeighbourMsg::SyncResp {
                    room: request.room.clone(),
                    changes: doc
                        .get_changes(&base_heads)
                        .iter()
                        .map(|c| c.raw_bytes().to_vec())
                        .collect(),
                },
                crate::owner::policy_fence::sample(time).map_err(rejected)?,
            )
            .map_err(rejected)?;
        let committed = self
            .commit_desktop_creation_candidate(
                candidate,
                creation,
                &creation_publications,
                request,
                time,
            )
            .map_err(|e| {
                if let Some(refused) = e.downcast_ref::<crate::owner::OwnerPolicyRefused>() {
                    DesktopCommandError::Rejected(refused.0.clone())
                } else {
                    DesktopCommandError::Uncertain(e.to_string())
                }
            })?;
        let receipt = OwnerCommitReceipt {
            binding,
            generation: committed.generation,
            aggregate_hash: hex::decode(committed.aggregate_hash)
                .map_err(|e| DesktopCommandError::Uncertain(e.to_string()))?
                .try_into()
                .map_err(|_| DesktopCommandError::Uncertain("invalid aggregate hash".into()))?,
        };
        receipt
            .validate_for(request)
            .map_err(DesktopCommandError::Uncertain)?;
        Ok(receipt)
    }
}

#[cfg(test)]
#[path = "desktop_commands/policy_tests.rs"]
mod policy_tests;
