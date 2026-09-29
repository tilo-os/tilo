//! Current-room command admission in the serialized durable owner.
use crate::SessionOwner;
mod namespace;
mod policy;
pub(crate) use policy::DesktopPolicyFence;
use tilo_action_broker::peer_actions::PolicyTime;
use tilo_desktop_model::{commands::*, *};
use tilo_peer_state::owner_commit::{CommitOperation, OwnerCommitReceipt, OwnerCommitRequest};
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
        if let DesktopCommand::ClaimNamespace { salt } = command.command {
            return self.commit_namespace_claim(request, binding, caller, salt, time);
        }
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
        // An older source revision cannot misplace what the owner places
        // against its current topology below.
        let placement_independent = command.command.is_placement_independent();
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
        let opens_app = matches!(command.command, DesktopCommand::OpenApp { .. });
        let mut revoked = Vec::new();
        let mut governed = if matches!(resolved, DesktopCommand::RecordEffectReceipt { .. }) {
            Some(Vec::new())
        } else if let Some((granted, removed)) = self
            .apply_app_action_command(workspace, caller, &resolved, now, &doc)
            .map_err(rejected)?
        {
            revoked = removed;
            Some(granted)
        } else {
            self.apply_permission_profile_command(workspace, caller, &resolved, now)
                .map_err(rejected)?
        };
        if governed.is_none() {
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
                    // An agent's or member's open runs a native app process, which
                    // opens its package primary before the window's record.
                    if opens_app
                        && !record.is_empty()
                        && let Some(primary) = application
                            .prepare_primary(self.peer(), caller, now)?
                            .into_new_document()
                    {
                        documents.push(primary);
                    }
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
        }
        if let DesktopCommand::OpenApp { app } = &command.command
            && let Some(authorship) = self.app_authorship(caller, app, now).map_err(rejected)?
        {
            governed.get_or_insert_with(Vec::new).push(authorship);
        }
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
        if let DesktopCommand::RecordEffectReceipt { receipt_json } = &resolved {
            record_effect_receipt(&mut doc, receipt_json).map_err(rejected)?;
        }
        if matches!(resolved, DesktopCommand::SavePermissionProfile { .. }) {
            tilo_desktop_model::projection::project_workspace_permission_profiles(
                &mut doc,
                &workspace.permission_profiles,
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
                tilo_peer_state::TransportOrigin::RelayDelta(
                    self.identity.native_cert.device_public_key,
                ),
                tilo_peer_state::NeighbourMsg::SyncResp {
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
        if let Some(capabilities) = governed.filter(|capabilities| !capabilities.is_empty()) {
            candidate
                .put_governed_capabilities(
                    &capabilities,
                    &self.identity.native,
                    crate::owner::policy_fence::sample(time).map_err(rejected)?,
                )
                .map_err(rejected)?;
        }
        if !revoked.is_empty() {
            candidate
                .remove_governed_capabilities(
                    &revoked,
                    &self.identity.native,
                    crate::owner::policy_fence::sample(time).map_err(rejected)?,
                )
                .map_err(rejected)?;
        }
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

/// Capabilities the governance writer records, and the ids it removes.
type GovernedChange = (Vec<collab_types::caps::CapOverride>, Vec<String>);

/// Record a member's effect receipt in the room document; a receipt already
/// recorded for the invocation must be the same one.
fn record_effect_receipt(doc: &mut AutoCommit, receipt_json: &str) -> Result<(), String> {
    use ui::automerge::{ObjType, ScalarValue, Value};
    let receipt: tilo_desktop_model::WorkspaceEffectReceipt =
        serde_json::from_str(receipt_json).map_err(|e| format!("not an effect receipt: {e}"))?;
    let redacted = receipt_json.as_bytes();
    let object = match doc
        .get(ROOT, "effect_receipts_v1")
        .map_err(|e| e.to_string())?
    {
        Some((Value::Object(ObjType::Map), object)) => object,
        Some(_) => return Err("invalid canonical effect receipt map".into()),
        None => doc
            .put_object(ROOT, "effect_receipts_v1", ObjType::Map)
            .map_err(|e| e.to_string())?,
    };
    let id = receipt.invocation.0.to_string();
    match doc.get(&object, &id).map_err(|e| e.to_string())? {
        Some((Value::Scalar(value), _)) if matches!(value.as_ref(), ScalarValue::Bytes(prior) if prior.as_slice() == redacted) =>
            {}
        Some(_) => return Err("effect receipt is immutable".into()),
        None => {
            doc.put(&object, &id, ScalarValue::Bytes(redacted.to_vec()))
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

impl SessionOwner {
    /// The capability letting `app` author the workspace, when an
    /// administrator opens it and it may not yet: as a person's own launcher
    /// grants an app it opens. `None` otherwise.
    fn app_authorship(
        &self,
        caller: &tilo_user_identity::DeviceCert,
        app: &str,
        now: std::time::SystemTime,
    ) -> Result<Option<collab_types::caps::CapOverride>, String> {
        use collab_types::caps::{
            Actor, CapDecision, CapOverride, ResourceRef, Scope, Subject, Verb, decide_capability,
        };
        let (role, capabilities, person) =
            self.peer().permissions(caller.device_public_key, now)?;
        let room = self.peer().room();
        let seconds = now
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .map_err(|_| "desktop policy time predates epoch")?
            .as_secs();
        let opener = Actor::person(Some(role), person).in_room(room);
        let decide = |actor: &Actor, verb| {
            decide_capability(
                actor,
                &ResourceRef::Workspace,
                verb,
                &capabilities,
                Some(seconds),
            ) == CapDecision::Allow
        };
        if !decide(&opener, Verb::Admin) || decide(&Actor::app(app).in_room(room), Verb::Write) {
            return Ok(None);
        }
        Ok(Some(
            CapOverride::grant(Subject::app(app), Scope::Workspace, Verb::Write)
                .with_id(format!("app-author-v1:{app}")),
        ))
    }

    /// Apply an app-action grant or revocation with the caller's own authority
    /// over workspace governance: `Some((granted, revoked))`, the capabilities
    /// the governance writer then records and the ids it removes; `None` when
    /// `command` is neither.
    fn apply_app_action_command(
        &self,
        workspace: &mut Workspace,
        caller: &tilo_user_identity::DeviceCert,
        command: &DesktopCommand,
        now: std::time::SystemTime,
        room: &AutoCommit,
    ) -> Result<Option<GovernedChange>, String> {
        use collab_types::caps::{
            Actor, CapOverride, Effect, Principal, ResourceRef, Scope, Subject, Verb,
        };
        let (action_id, app, target_app, action, lifetime) = match command {
            DesktopCommand::GrantAppAction {
                app,
                target_app,
                action,
                lifetime,
            } => (
                APP_ACTION_GRANT_ACTION_ID,
                app,
                target_app,
                action,
                Some(lifetime),
            ),
            DesktopCommand::RevokeAppAction {
                app,
                target_app,
                action,
            } => (APP_ACTION_REVOKE_ACTION_ID, app, target_app, action, None),
            _ => return Ok(None),
        };
        let (app, target_app, action) = (app.trim(), target_app.trim(), action.trim());
        if app.is_empty() || target_app.is_empty() || action.is_empty() {
            return Err("name the application, the other application and its action".into());
        }
        let (role, capabilities, person) =
            self.peer().permissions(caller.device_public_key, now)?;
        let actor = Actor::person(Some(role), person).in_room(self.peer().room());
        workspace.capabilities = capabilities;
        let seconds = now
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .map_err(|_| "desktop policy time predates epoch")?
            .as_secs();
        if !workspace.allows_at(
            &actor,
            &ResourceRef::Action {
                app: WORKSPACE_GOVERNANCE_APP_ID.into(),
                action: action_id.into(),
            },
            Verb::Admin,
            seconds,
        ) {
            return Err("the caller is not allowed to change what apps may use".into());
        }
        let scope = Scope::Action {
            app: target_app.to_owned(),
            action: action.to_owned(),
        };
        let subject = Subject::app(app);
        let Some(lifetime) = lifetime else {
            let revoked = tilo_peer_state::room::extract_governed_fields(room)
                .2
                .into_iter()
                .filter(|cap| {
                    cap.effect == Effect::Grant && cap.subject == subject && cap.scope == scope
                })
                .map(|cap| cap.id)
                .collect();
            return Ok(Some((Vec::new(), revoked)));
        };
        if target_app == WORKSPACE_GOVERNANCE_APP_ID {
            return Err("governing the workspace is assigned with a permission role".into());
        }
        match lifetime.trim().to_ascii_lowercase().as_str() {
            "always" => {}
            "session" => {
                return Err(
                    "a hosted workspace grants for always; revoke the grant to end it".into(),
                );
            }
            _ => return Err("lifetime must be session or always".into()),
        }
        let grantor = actor
            .principals
            .iter()
            .find(|principal| !matches!(principal, Principal::Room(_)))
            .cloned()
            .ok_or("the caller carries no identity to grant as")?;
        let id = workspace
            .grant_capability(CapOverride::grant(subject, scope, Verb::Write).granted_by(grantor));
        let granted = workspace
            .capabilities
            .iter()
            .find(|capability| capability.id == id)
            .cloned()
            .ok_or("the grant was not recorded")?;
        Ok(Some((vec![granted], Vec::new())))
    }

    /// Apply a permission-profile command with the caller's own authority over
    /// workspace governance. `Some` names the capabilities an assignment
    /// expands to, which the governance writer then records; `None` when
    /// `command` is not a permission-profile command.
    fn apply_permission_profile_command(
        &self,
        workspace: &mut Workspace,
        caller: &tilo_user_identity::DeviceCert,
        command: &DesktopCommand,
        now: std::time::SystemTime,
    ) -> Result<Option<Vec<collab_types::caps::CapOverride>>, String> {
        if !matches!(
            command,
            DesktopCommand::SavePermissionProfile { .. }
                | DesktopCommand::AssignPermissionProfile { .. }
        ) {
            return Ok(None);
        }
        let (role, capabilities, person) =
            self.peer().permissions(caller.device_public_key, now)?;
        let actor =
            collab_types::caps::Actor::person(Some(role), person).in_room(self.peer().room());
        workspace.capabilities = capabilities;
        let seconds = now
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .map_err(|_| "desktop policy time predates epoch")?
            .as_secs();
        match command {
            DesktopCommand::SavePermissionProfile { profile_json } => {
                workspace
                    .save_permission_profile_as_actor_at(
                        &actor,
                        decode_permission_profile(profile_json)?,
                        seconds,
                    )
                    .map_err(|error| error.to_string())?;
                Ok(Some(Vec::new()))
            }
            DesktopCommand::AssignPermissionProfile {
                profile,
                subject_kind,
                subject,
                expires_unix,
            } => {
                let (subject, expiry) =
                    permission_assignment(subject_kind, subject, *expires_unix)?;
                let ids = workspace
                    .assign_permission_profile_as_actor_at(
                        &actor, profile, &subject, expiry, seconds,
                    )
                    .map_err(|error| error.to_string())?;
                Ok(Some(
                    workspace
                        .capabilities
                        .iter()
                        .filter(|capability| ids.contains(&capability.id))
                        .cloned()
                        .collect(),
                ))
            }
            _ => Ok(None),
        }
    }
}

#[cfg(test)]
#[path = "desktop_commands/policy_tests.rs"]
mod policy_tests;
