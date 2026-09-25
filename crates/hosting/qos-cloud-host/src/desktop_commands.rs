//! The Cloud host's desktop commands: open, close and arrange applications
//! through the Store owner, for people (the browser desktop, "Open Notes")
//! and agent tasks (the same published open action).
//!
//! Mirrors the native `SessionOwner::commit_desktop_command_current`: the
//! caller's current membership and desktop permission, the room's topology
//! namespace of this host, the checked canonical record factory
//! (`qos_session_peer::canonical_records`) and one Store commit of the new
//! record together with its desktop tab. The durable result is kept as an
//! invocation receipt, so a repeated request is answered from it and an
//! agent task's opened records can be recovered from it
//! ([`Store::task_opened_records`]).
use super::*;
use automerge::ReadDoc as _;
use collab_types::caps::{Actor, CapDecision, ResourceRef, Verb, decide_capability};
use qos_action_broker::services::DesktopOpener;
use qos_desktop::session_commands::{self, DesktopCommand, TabLocation, TabPlacement};
use qos_peer_state::desktop_command::{
    DesktopCommandError, DesktopCommandId, DesktopCommandReceipt, DesktopRefusal, OpenedWindow,
};
use qos_peer_state::owner_commit::{CommitOperation, OwnerCommitReceipt, OwnerCommitRequest};

/// Receipt output kind of a committed desktop command.
const DESKTOP_RECEIPT_KIND: &str = qos_peer_state::desktop_command::RECEIPT_KIND;

fn rejected(reason: impl Into<String>) -> DesktopCommandError {
    DesktopCommandError::Rejected(reason.into())
}

impl Store {
    /// Commit one desktop command for its authenticated caller. `opener` is
    /// the broker's revalidated agent task identity when an enrolled agent
    /// runs it (never read from the request).
    pub(crate) fn commit_desktop_command(
        &mut self,
        request: &OwnerCommitRequest,
        opener: Option<&DesktopOpener>,
    ) -> Result<OwnerCommitReceipt, DesktopCommandError> {
        let prepared = self.prepare_desktop_command(request, opener);
        let (state, binding) = match prepared {
            Ok(Prepared::Replay(receipt)) => return Ok(receipt),
            Ok(Prepared::Commit(state, binding)) => (state, binding),
            Err(error) if is_store_failure(&error) => {
                return Err(DesktopCommandError::Uncertain(error.to_string()));
            }
            Err(error) => return Err(rejected(error.to_string())),
        };
        self.commit(*state)
            .map_err(|error| DesktopCommandError::Uncertain(error.to_string()))?;
        let receipt = self
            .desktop_receipt(binding)
            .map_err(|error| DesktopCommandError::Uncertain(error.to_string()))?;
        receipt
            .validate_for(request)
            .map_err(DesktopCommandError::Uncertain)?;
        Ok(receipt)
    }

    /// One desktop command as the terminal invocation update its issuer
    /// sees, person or agent alike: the receipt carrying the command id and
    /// the windows it opened, or a refusal carrying the id and its reason.
    /// An uncertain owner outcome is not a refusal: it stays a Store failure.
    pub(crate) fn desktop_command_update(
        &mut self,
        request: &OwnerCommitRequest,
        opener: Option<&DesktopOpener>,
    ) -> Result<qos_actions::InvocationUpdate> {
        let CommitOperation::DesktopCommand { invocation, .. } = &request.operation else {
            bail!("not a desktop command")
        };
        let invocation = qos_actions::InvocationId(*invocation);
        let id = DesktopCommandId::of(request).ok_or_else(|| anyhow!("not a desktop command"))?;
        match self.commit_desktop_command(request, opener) {
            Ok(receipt) => Ok(qos_actions::InvocationUpdate::Succeeded {
                invocation,
                output: qos_actions::ActionOutput {
                    value: Some(
                        self.desktop_command_receipt(&id)
                            .unwrap_or_else(|| {
                                DesktopCommandReceipt::new(&id, receipt.generation, Vec::new())
                            })
                            .to_value(),
                    ),
                    resources: Vec::new(),
                },
            }),
            Err(DesktopCommandError::Rejected(message)) => {
                Ok(qos_actions::InvocationUpdate::Failed {
                    invocation,
                    error: qos_actions::ActionFailure {
                        code: qos_peer_state::desktop_command::REJECTED_CODE.into(),
                        details: Some(DesktopRefusal::classify(&message).details(&id)),
                        message,
                        retryable: false,
                    },
                })
            }
            Err(DesktopCommandError::Uncertain(reason)) => {
                Err(anyhow::Error::new(crate::StoreUnavailable).context(reason))
            }
        }
    }

    /// The committed receipt of command `id`, from this Store's own receipts.
    pub(crate) fn desktop_command_receipt(
        &self,
        id: &DesktopCommandId,
    ) -> Option<DesktopCommandReceipt> {
        let (_, receipt) = self.state.receipts.get(&id.to_string())?;
        let output = receipt
            .output
            .as_ref()
            .filter(|output| output["kind"] == DESKTOP_RECEIPT_KIND)?;
        let opened = output["opened"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|opened| serde_json::from_value::<OpenedWindow>(opened.clone()).ok())
            .collect();
        Some(DesktopCommandReceipt::new(
            id,
            output["generation"].as_u64()?,
            opened,
        ))
    }

    fn desktop_receipt(
        &self,
        binding: qos_peer_state::owner_commit::CommitBinding,
    ) -> Result<OwnerCommitReceipt> {
        Ok(OwnerCommitReceipt {
            binding,
            generation: self.state.generation,
            aggregate_hash: *blake3::hash(&serde_json::to_vec(&self.state)?).as_bytes(),
        })
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one owner turn, mirrors the native owner"
    )]
    fn prepare_desktop_command(
        &self,
        request: &OwnerCommitRequest,
        opener: Option<&DesktopOpener>,
    ) -> Result<Prepared> {
        self.ensure_reconciled()?;
        let binding = request.binding().map_err(anyhow::Error::msg)?;
        let CommitOperation::DesktopCommand {
            invocation,
            caller,
            command,
        } = &request.operation
        else {
            bail!("not a desktop command")
        };
        if request.room != self.state.room || command.source.room != self.state.room {
            bail!("desktop command room mismatch")
        }
        // The command id is the receipt scope: a retry of the same id is
        // answered from its receipt, never applied twice.
        let scope = DesktopCommandId {
            device: caller.device_public_key,
            invocation: *invocation,
        }
        .to_string();
        let digest = blake3::hash(&serde_json::to_vec(request)?)
            .to_hex()
            .to_string();
        if let Some((prior, receipt)) = self.state.receipts.get(&scope) {
            let generation = receipt
                .output
                .as_ref()
                .filter(|output| output["kind"] == DESKTOP_RECEIPT_KIND)
                .and_then(|output| output["generation"].as_u64());
            return match generation {
                Some(generation) if prior == &digest => Ok(Prepared::Replay(OwnerCommitReceipt {
                    binding,
                    generation,
                    aggregate_hash: *blake3::hash(&serde_json::to_vec(&self.state)?).as_bytes(),
                })),
                _ => bail!("invocation identity reused with a different desktop command"),
            };
        }
        let now_ns = self.policy_now_ns()?;
        let now = now_time(now_ns);
        let (before, mut candidate) = self.peer_transition_at(now_ns)?;
        let key = self.key();
        let host = hex::encode(key.verifying_key().to_bytes());
        if candidate.authority().device_public_key != key.verifying_key().to_bytes()
            || candidate.room() != self.state.room
            || !candidate.certificates().contains(caller)
        {
            bail!("desktop caller or host is no longer admitted")
        }
        // The caller's current rights. An agent task acts with the certified
        // device of the person who spawned it, capped by the agent's own
        // standing: a blocked agent opens nothing.
        let (role, caps, person) = candidate
            .permissions(caller.device_public_key, now)
            .map_err(anyhow::Error::msg)?;
        if let Some(opener) = opener {
            use qos_action_broker::policy::WorkspacePolicy as _;
            let tree =
                crate::agent_owner::workspace_tree(&candidate).map_err(anyhow::Error::msg)?;
            let workspace = tree
                .workspaces
                .first()
                .ok_or_else(|| anyhow!("workspace missing"))?;
            if workspace.role_of_agent_at(&opener.agent, now_ns / 1_000_000_000)
                == Some(collab_types::Role::Denied)
            {
                bail!("agent is blocked in this workspace")
            }
            if !matches!(command.command, DesktopCommand::OpenApp { .. }) {
                bail!("agent tasks may only open applications")
            }
        }
        let actor = Actor::person(Some(role), &person).in_room(self.state.room.clone());
        let verb = if matches!(command.command, DesktopCommand::RenameWorkspace { .. }) {
            Verb::Admin
        } else {
            Verb::Write
        };
        if decide_capability(
            &actor,
            &ResourceRef::Action {
                app: session_commands::APP_ID.into(),
                action: session_commands::ACTION.into(),
            },
            verb,
            &caps,
            Some(now_ns / 1_000_000_000),
        ) != CapDecision::Allow
        {
            bail!("caller cannot change this desktop")
        }
        let mut doc = candidate
            .admitted_room()
            .cloned()
            .ok_or_else(|| anyhow!("room governance missing"))?;
        let base_heads = doc.get_heads();
        let mut current: Vec<[u8; 32]> = base_heads.iter().map(|head| head.0).collect();
        let mut expected = command.source.heads.clone();
        current.sort_unstable();
        expected.sort_unstable();
        // OpenApp is placement-independent: it is placed below against the
        // current topology, so an older source revision cannot misplace it.
        if current != expected
            && !matches!(command.command, DesktopCommand::OpenApp { .. })
            && !session_commands::desktop_source_matches(&doc, &command.source.heads)
                .map_err(anyhow::Error::msg)?
        {
            bail!("desktop source revision is stale")
        }
        let owners = doc
            .get(automerge::ROOT, "contents_owners_v1")?
            .ok_or_else(|| anyhow!("desktop identity custody is missing"))?
            .1;
        let salts = {
            doc.keys(&owners)
                .filter_map(|entry| {
                    let salt = u16::from_str_radix(&entry, 16).ok()?;
                    let (value, _) = doc.get(&owners, &entry).ok()??;
                    (salt != 0 && value.to_str() == Some(host.as_str())).then_some(salt)
                })
                .collect::<std::collections::BTreeSet<_>>()
        };
        let salt = salts
            .first()
            .copied()
            .ok_or_else(|| anyhow!("host has no desktop topology namespace"))?;
        let mut tree = qos_desktop::WorkspaceTree::default();
        qos_desktop::projection::RoomProjection::apply_admitted_room(
            &mut tree,
            qos_desktop::RoomId::parse(&self.state.room).ok_or_else(|| anyhow!("invalid room"))?,
            &doc,
        )
        .map_err(anyhow::Error::msg)?;
        let workspace = tree
            .workspaces
            .first_mut()
            .ok_or_else(|| anyhow!("workspace missing"))?;
        let owned = |id: u64| salts.contains(&qos_desktop::identity::peer_salt_of(id));
        let resolved = match &command.command {
            DesktopCommand::OpenApp { app } => DesktopCommand::OpenTab {
                app: app.clone(),
                placement: session_commands::open_app_placement(workspace, owned)
                    .map_err(anyhow::Error::msg)?,
            },
            other => other.clone(),
        };
        check_owned_targets(&resolved, owned)?;
        let maximum = workspace
            .strips
            .iter()
            .flat_map(|strip| {
                std::iter::once(strip.id.0).chain(strip.window_groups.iter().flat_map(|group| {
                    std::iter::once(group.id).chain(group.tabs.iter().map(|tab| tab.id))
                }))
            })
            .filter(|id| qos_desktop::identity::peer_salt_of(*id) == salt)
            .map(qos_desktop::identity::counter_of)
            .max()
            .unwrap_or(0);
        let stored = doc
            .get(automerge::ROOT, "desktop_counter_v1")?
            .and_then(|(value, _)| value.to_u64())
            .unwrap_or(1);
        let mut counter = stored.max(maximum + 1);
        let mut documents = vec![];
        let mut opened = vec![];
        let mut routes = qos_desktop::projection::RoomProjection::record_routes(&doc);
        session_commands::apply_command(
            workspace,
            &resolved,
            &mut || qos_desktop::identity::mint_in_namespace(salt, &mut counter),
            &mut |app, id| {
                let application =
                    qos_session_peer::canonical_records::resolve(app).ok_or_else(|| {
                        "application has no installed checked record factory".to_owned()
                    })?;
                let definition = application.definition();
                let prepared = application.prepare_for_tab(&candidate, caller, id, now)?;
                let record = prepared.record().to_owned();
                if let Some(document) = prepared.into_new_document() {
                    documents.push(document);
                }
                opened.push((app.to_owned(), record.clone(), id));
                routes.insert(id, record);
                Ok((
                    definition.title.into(),
                    qos_desktop::TileContent::AppPlaceholder {
                        kind: qos_desktop::AppKind::Other(app.into()),
                        label: definition.title.into(),
                    },
                ))
            },
        )
        .map_err(anyhow::Error::msg)?;
        // Each opened window with its committed strip and group, so the
        // issuer binds its optimistic window to the exact committed tab.
        let opened = opened
            .into_iter()
            .map(|(app, record, tab)| {
                let (strip, group) = workspace
                    .strips
                    .iter()
                    .flat_map(|strip| strip.window_groups.iter().map(move |g| (strip, g)))
                    .find(|(_, group)| group.tabs.iter().any(|t| t.id == tab))
                    .map(|(strip, group)| (strip.id.0, group.id))
                    .ok_or_else(|| anyhow!("opened window is not on the desktop"))?;
                Ok(serde_json::to_value(OpenedWindow {
                    app_id: app,
                    record_id: record,
                    strip,
                    group,
                    tab,
                })?)
            })
            .collect::<Result<Vec<_>>>()?;
        doc.set_actor(automerge::ActorId::from(
            key.verifying_key().to_bytes().to_vec(),
        ));
        for salt in &salts {
            qos_desktop::projection::RoomProjection::project_owned_command_contents(
                &mut doc,
                &workspace.strips,
                *salt,
            );
        }
        for strip in &workspace.strips {
            for group in &strip.window_groups {
                for tab in &group.tabs {
                    if let Some(record) = routes.get(&tab.id) {
                        qos_desktop::projection::RoomProjection::bind_record(
                            &mut doc, strip.id.0, group.id, tab.id, record,
                        )
                        .map_err(anyhow::Error::msg)?;
                    }
                }
            }
        }
        doc.put(automerge::ROOT, "desktop_counter_v1", counter)?;
        if matches!(resolved, DesktopCommand::RenameWorkspace { .. }) {
            doc.put(automerge::ROOT, "name", &workspace.name)?;
        }
        doc.commit();
        // Checked records enter the candidate before their topology. Their
        // genesis is not published on its own: the durable mutation below
        // publishes each new record's single covered checkpoint, and an
        // extra uncovered one ahead of it would stall members.
        for (app, record, bytes) in documents {
            candidate
                .publish_document(&app, &record, bytes, &key, now)
                .map_err(anyhow::Error::msg)?;
        }
        candidate
            .receive_from(
                TransportOrigin::RelayDelta(key.verifying_key().to_bytes()),
                NeighbourMsg::SyncResp {
                    room: self.state.room.clone(),
                    changes: doc
                        .get_changes(&base_heads)
                        .iter()
                        .map(|change| change.raw_bytes().to_vec())
                        .collect(),
                },
                now,
            )
            .map_err(anyhow::Error::msg)?;
        let generation = self.next_generation()?;
        let mut state = self.state.clone();
        state.outbox.extend(durable_mutation_publications(
            &mut candidate,
            &before,
            &key,
            now,
            generation,
        )?);
        state
            .bound
            .as_mut()
            .ok_or_else(|| anyhow!("workspace no longer enrolled"))?
            .peer = candidate.durable().map_err(anyhow::Error::msg)?;
        state.receipts.insert(
            scope.clone(),
            (
                digest,
                ActionReceipt {
                    invocation_id: invocation.to_string(),
                    status: "completed".into(),
                    error: None,
                    output: Some(serde_json::json!({
                        "kind": DESKTOP_RECEIPT_KIND,
                        "command": scope,
                        "generation": generation,
                        "opened": opened,
                        "agent": opener.map(|o| o.agent.clone()),
                        "task": opener.map(|o| o.task.clone()),
                    })),
                },
            ),
        );
        Ok(Prepared::Commit(Box::new(state), binding))
    }

    /// Commit the durable receipt of one Effect invocation into the room's
    /// `effect_receipts_v1` map, as the native owner does. Immutable: the
    /// same bytes again are a no-op, different bytes are refused.
    pub(crate) fn commit_effect_receipt(
        &mut self,
        request: &OwnerCommitRequest,
    ) -> Result<OwnerCommitReceipt, String> {
        use automerge::{ObjType, ScalarValue, Value};
        let binding = request.binding()?;
        let CommitOperation::EffectReceipt {
            invocation,
            redacted,
        } = &request.operation
        else {
            return Err("not an effect receipt".into());
        };
        if request.room != self.state.room {
            return Err("effect receipt room mismatch".into());
        }
        let receipt: qos_desktop::WorkspaceEffectReceipt =
            serde_json::from_slice(redacted).map_err(|e| e.to_string())?;
        if receipt.invocation.0 != *invocation {
            return Err("effect receipt invocation mismatch".into());
        }
        let now_ns = self.policy_now_ns().map_err(|e| e.to_string())?;
        let now = now_time(now_ns);
        let (before, mut candidate) = self.peer_transition_at(now_ns).map_err(|e| e.to_string())?;
        let key = self.key();
        let mut doc = candidate
            .admitted_room()
            .cloned()
            .ok_or("room governance missing")?;
        let base_heads = doc.get_heads();
        doc.set_actor(automerge::ActorId::from(
            key.verifying_key().to_bytes().to_vec(),
        ));
        let object = match doc
            .get(automerge::ROOT, "effect_receipts_v1")
            .map_err(|e| e.to_string())?
        {
            Some((Value::Object(ObjType::Map), object)) => object,
            Some(_) => return Err("invalid canonical effect receipt map".into()),
            None => doc
                .put_object(automerge::ROOT, "effect_receipts_v1", ObjType::Map)
                .map_err(|e| e.to_string())?,
        };
        let id = invocation.to_string();
        match doc.get(&object, &id).map_err(|e| e.to_string())? {
            Some((Value::Scalar(value), _)) if matches!(value.as_ref(), ScalarValue::Bytes(prior) if prior == redacted) =>
            {
                return self.desktop_receipt(binding).map_err(|e| e.to_string());
            }
            Some(_) => return Err("effect receipt is immutable".into()),
            None => doc
                .put(&object, &id, ScalarValue::Bytes(redacted.clone()))
                .map_err(|e| e.to_string())?,
        }
        doc.commit();
        candidate.receive_from(
            TransportOrigin::RelayDelta(key.verifying_key().to_bytes()),
            NeighbourMsg::SyncResp {
                room: self.state.room.clone(),
                changes: doc
                    .get_changes(&base_heads)
                    .iter()
                    .map(|change| change.raw_bytes().to_vec())
                    .collect(),
            },
            now,
        )?;
        let generation = self.next_generation().map_err(|e| e.to_string())?;
        let mut state = self.state.clone();
        state.outbox.extend(
            durable_mutation_publications(&mut candidate, &before, &key, now, generation)
                .map_err(|e| e.to_string())?,
        );
        state
            .bound
            .as_mut()
            .ok_or("workspace no longer enrolled")?
            .peer = candidate.durable()?;
        self.commit(state).map_err(|e| e.to_string())?;
        let receipt = self.desktop_receipt(binding).map_err(|e| e.to_string())?;
        receipt.validate_for(request)?;
        Ok(receipt)
    }

    /// Records the agent task `task` opened through the desktop's open
    /// action, from this Store's own committed desktop receipts.
    pub(crate) fn task_opened_records(
        &self,
        task: &str,
    ) -> Vec<qos_agent_control_protocol::OpenedRecord> {
        self.state
            .receipts
            .values()
            .filter_map(|(_, receipt)| receipt.output.as_ref())
            .filter(|output| output["kind"] == DESKTOP_RECEIPT_KIND && output["task"] == task)
            .filter_map(|output| output["opened"].as_array())
            .flatten()
            .filter_map(|opened| {
                Some(qos_agent_control_protocol::OpenedRecord {
                    app_id: opened["appId"].as_str()?.to_owned(),
                    record_id: opened["recordId"].as_str()?.to_owned(),
                })
            })
            .collect()
    }
}

enum Prepared {
    Replay(OwnerCommitReceipt),
    Commit(Box<State>, qos_peer_state::owner_commit::CommitBinding),
}

/// Never touch another topology owner's namespace (native peers, other
/// hosts): every strip, group and tab a command names must be this host's.
fn check_owned_targets(command: &DesktopCommand, owned: impl Fn(u64) -> bool) -> Result<()> {
    let own = |id: u64| {
        if owned(id) {
            Ok(())
        } else {
            Err(anyhow!("target belongs to another topology owner"))
        }
    };
    let placement = |p: &TabPlacement| -> Result<()> {
        match p {
            TabPlacement::EmptyWorkspace => {}
            TabPlacement::InGroup {
                strip,
                group,
                after,
            } => {
                own(*strip)?;
                own(*group)?;
                if let Some(id) = after {
                    own(*id)?;
                }
            }
            TabPlacement::NewGroup { strip, after } => {
                own(*strip)?;
                if let Some(id) = after {
                    own(*id)?;
                }
            }
        }
        Ok(())
    };
    let location = |l: &TabLocation| -> Result<()> {
        own(l.strip)?;
        own(l.group)?;
        own(l.tab)
    };
    match command {
        DesktopCommand::OpenApp { .. } => bail!("open-app must be resolved to a placement"),
        DesktopCommand::OpenTab { placement: p, .. } => placement(p)?,
        DesktopCommand::CloseTab { location: l } | DesktopCommand::DuplicateTab { location: l } => {
            location(l)?;
        }
        DesktopCommand::ReorderTab {
            location: l,
            before,
        }
        | DesktopCommand::SplitTab {
            location: l,
            before,
        } => {
            location(l)?;
            if let Some(id) = before {
                own(*id)?;
            }
        }
        DesktopCommand::ReorderGroup {
            strip,
            group,
            before,
        } => {
            own(*strip)?;
            own(*group)?;
            if let Some(id) = before {
                own(*id)?;
            }
        }
        DesktopCommand::MoveTab {
            location: l,
            placement: p,
        } => {
            location(l)?;
            placement(p)?;
        }
        DesktopCommand::RenameStrip { strip, .. } | DesktopCommand::RemoveStrip { strip } => {
            own(*strip)?;
        }
        DesktopCommand::CreateStrip { .. } | DesktopCommand::RenameWorkspace { .. } => {}
    }
    Ok(())
}

/// The broker's desktop command service over the Cloud Store owner.
pub(crate) struct CloudDesktopCommands {
    pub(crate) room: String,
    pub(crate) owner: std::sync::Arc<crate::agent_runtime_adapter::RuntimeOwner>,
}
impl CloudDesktopCommands {
    fn run(
        &self,
        request: OwnerCommitRequest,
        opener: Option<DesktopOpener>,
    ) -> qos_action_broker::services::DesktopCommandFuture {
        let owner = self.owner.clone();
        Box::pin(async move {
            let mut store = owner.store().map_err(DesktopCommandError::Uncertain)?;
            store.commit_desktop_command(&request, opener.as_ref())
        })
    }
}
impl qos_action_broker::services::DesktopCommands for CloudDesktopCommands {
    fn room(&self) -> &str {
        &self.room
    }
    fn execute(
        &self,
        request: OwnerCommitRequest,
        _direct_delivery: bool,
    ) -> qos_action_broker::services::DesktopCommandFuture {
        self.run(request, None)
    }
    fn execute_for_agent(
        &self,
        request: OwnerCommitRequest,
        opener: DesktopOpener,
    ) -> qos_action_broker::services::DesktopCommandFuture {
        self.run(request, Some(opener))
    }
    fn openable_applications(&self) -> Vec<(String, String)> {
        openable_applications()
    }
    fn receipt(&self, request: &OwnerCommitRequest) -> Option<DesktopCommandReceipt> {
        let id = DesktopCommandId::of(request)?;
        self.owner.store().ok()?.desktop_command_receipt(&id)
    }
}

/// Catalogue descriptors of the Cloud desktop for people: "Open <App>" per
/// openable application, and the browser desktop's layout command.
pub(crate) fn desktop_descriptors(revision: &str) -> Vec<serde_json::Value> {
    use qos_agent_control_protocol::{
        OPEN_APP_ACTION, OPEN_APP_RESOURCE_KIND, OPEN_APP_RESOURCE_PREFIX,
    };
    // Write, answered with the receipt naming the command id and windows.
    let write = qos_actions::ActionContract {
        permission: qos_actions::PermissionRequirement::Write,
        result: qos_actions::ResultSpec::Json {
            schema: DesktopCommandReceipt::schema(),
        },
        ..Default::default()
    };
    let mut descriptors: Vec<_> = openable_applications()
        .into_iter()
        .map(|(app, title)| {
            serde_json::json!({
                "resource": format!("{OPEN_APP_RESOURCE_PREFIX}{app}"),
                "action": OPEN_APP_ACTION,
                "revision": revision,
                "resource_kind": OPEN_APP_RESOURCE_KIND,
                "resource_label": title,
                "action_label": format!("Open {title}"),
                "description": format!("Open {title} in a new window with a new document"),
                "operation_kind": qos_actions::OperationKind::Effect,
                "params": [],
                "contract": write,
            })
        })
        .collect();
    descriptors.push(serde_json::json!({
        "resource": session_commands::RESOURCE,
        "action": session_commands::ACTION,
        "revision": revision,
        "resource_kind": "desktop",
        "resource_label": "Desktop",
        "action_label": "Change desktop",
        "description": "Typed native current-room desktop command",
        "operation_kind": qos_actions::OperationKind::Effect,
        "params": [
            qos_actions::ActionParam::required("command", qos_actions::ParamType::Text, "Exact source-bound desktop command"),
            qos_actions::ActionParam::optional("committed_desktop_v2", qos_actions::ParamType::Boolean, "Receive the committed desktop immediately; defaults to false"),
        ],
        "contract": write,
    }));
    descriptors
}

/// The owner request of a person's certified desktop invocation: an
/// "Open <App>" action, or the browser desktop's source-bound layout command.
pub(crate) fn person_request(
    room: &str,
    invocation: qos_actions::InvocationId,
    caller: &DeviceCert,
    resource: &str,
    action: &str,
    argument: Option<&serde_json::Value>,
) -> Result<OwnerCommitRequest> {
    use qos_peer_state::desktop_command::{BoundCommand, CommandSource};
    let command = if let Some(app) = resource
        .strip_prefix(qos_agent_control_protocol::OPEN_APP_RESOURCE_PREFIX)
        .filter(|_| action == qos_agent_control_protocol::OPEN_APP_ACTION)
    {
        if argument
            .is_some_and(|argument| !argument.as_object().is_some_and(serde_json::Map::is_empty))
        {
            bail!("opening an application takes no arguments")
        }
        if !openable_applications()
            .iter()
            .any(|(openable, _)| openable == app)
        {
            bail!("application cannot be opened here")
        }
        BoundCommand {
            source: CommandSource {
                room: room.into(),
                heads: Vec::new(),
            },
            command: DesktopCommand::OpenApp { app: app.into() },
        }
    } else if resource == session_commands::RESOURCE && action == session_commands::ACTION {
        let encoded = argument
            .and_then(|argument| argument.get("command"))
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| anyhow!("desktop command must preserve exact serialized IDs"))?;
        if encoded.len() > 32 * 1024 {
            bail!("desktop command exceeds bounded size")
        }
        serde_json::from_str(encoded)?
    } else {
        bail!("unsupported desktop action")
    };
    Ok(OwnerCommitRequest {
        room: room.into(),
        operation: CommitOperation::DesktopCommand {
            invocation: invocation.0,
            caller: caller.clone(),
            command,
        },
    })
}

/// Every application with an installed checked record factory.
pub(crate) fn openable_applications() -> Vec<(String, String)> {
    qos_session_peer::canonical_records::installed()
        .map(|entry| {
            (
                entry.app_id().to_owned(),
                entry.definition().title.to_owned(),
            )
        })
        .collect()
}

#[cfg(test)]
#[path = "desktop_commands/tests.rs"]
mod tests;
