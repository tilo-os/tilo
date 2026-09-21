//! Current-room command admission in the serialized durable owner.
use crate::SessionOwner;
use qos_desktop::{projection::RoomProjection, session_commands::*, *};
use qos_peer_state::owner_commit::{CommitOperation, OwnerCommitReceipt, OwnerCommitRequest};
use ui::automerge::{AutoCommit, ChangeHash, ROOT, ReadDoc, Value, transaction::Transactable};

pub(crate) fn checked_application(
    app: &str,
) -> Option<&'static ui::application::ApplicationDefinition> {
    match app {
        "dev.qos.word-processor" => Some(word_processor::definition()),
        "dev.qos.notes" => Some(notes::definition()),
        "dev.qos.calculator" => Some(calculator::definition()),
        "dev.qos.chat" => Some(chat::definition()),
        _ => None,
    }
}

/// Room heads also advance for receipts, enrollment and document durability.
/// Those changes must not invalidate a gesture against an unchanged desktop.
/// Compare the original admitted desktop with the current one, without
/// rebinding the command or weakening the current caller/target checks.
fn desktop_source_matches(doc: &AutoCommit, heads: &[[u8; 32]]) -> Result<bool, String> {
    let heads = heads.iter().copied().map(ChangeHash).collect::<Vec<_>>();
    if heads.is_empty()
        || heads
            .iter()
            .any(|head| doc.get_change_by_hash(head).is_none())
    {
        return Ok(false);
    }
    for key in [
        "room",
        "owner",
        "name",
        "contents",
        "contents_owners_v1",
        // Also prevents an old launch from being reused after its tab closes.
        "desktop_counter_v1",
    ] {
        let previous = doc
            .get_all_at(ROOT, key, &heads)
            .map_err(|e| e.to_string())?;
        let current = doc.get_all(ROOT, key).map_err(|e| e.to_string())?;
        if previous != current {
            return Ok(false);
        }
        for (value, object) in current {
            if matches!(value, Value::Object(_))
                && doc
                    .hydrate(&object, Some(&heads))
                    .map_err(|e| e.to_string())?
                    != doc.hydrate(&object, None).map_err(|e| e.to_string())?
            {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

impl SessionOwner {
    /// Host-local broker capability. Definite validation refusals never enter
    /// the durability barrier; failures of that barrier remain uncertain.
    pub fn commit_desktop_command(
        &mut self,
        request: &OwnerCommitRequest,
    ) -> Result<OwnerCommitReceipt, DesktopCommandError> {
        let rejected = DesktopCommandError::Rejected;
        let binding = request.binding().map_err(rejected)?;
        let CommitOperation::DesktopCommand {
            caller, command, ..
        } = &request.operation
        else {
            return Err(rejected("not a desktop command".into()));
        };
        if request.room != self.peer().room()
            || !self.permits_publication()
            || !self.peer().certificates().contains(caller)
        {
            return Err(rejected("caller or room is no longer admitted".into()));
        }
        let (role, caps, person) = self
            .peer()
            .permissions(caller.device_public_key, web_time::SystemTime::now())
            .map_err(rejected)?;
        let actor =
            collab_types::caps::Actor::person(Some(role), &person).in_room(request.room.clone());
        let target = collab_types::caps::ResourceRef::Action {
            app: APP_ID.into(),
            action: ACTION.into(),
        };
        if collab_types::caps::decide_capability(
            &actor,
            &target,
            collab_types::caps::Verb::Write,
            &caps,
            Some(
                web_time::SystemTime::now()
                    .duration_since(web_time::SystemTime::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
            ),
        ) != collab_types::caps::CapDecision::Allow
        {
            return Err(rejected("caller cannot change desktop topology".into()));
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
        if current != expected
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
                (salt != 0 && value.to_str() == Some(owner.as_str())).then_some(salt)
            })
            .collect::<std::collections::BTreeSet<_>>();
        let salt = salts
            .first()
            .copied()
            .ok_or_else(|| rejected("native owner has no topology namespace".into()))?;
        let mut tree = WorkspaceTree::default();
        RoomProjection::apply_admitted_room(
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
        let own = |id: u64| {
            if salts.contains(&qos_desktop::identity::peer_salt_of(id)) {
                Ok(())
            } else {
                Err(rejected("target belongs to another topology owner".into()))
            }
        };
        let placement = |p: &TabPlacement| -> Result<(), DesktopCommandError> {
            match p {
                TabPlacement::EmptyWorkspace => (),
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
        let location = |l: &TabLocation| -> Result<(), DesktopCommandError> {
            own(l.strip)?;
            own(l.group)?;
            own(l.tab)
        };
        match &command.command {
            DesktopCommand::OpenTab { placement: p, .. } => placement(p)?,
            DesktopCommand::CloseTab { location: l }
            | DesktopCommand::DuplicateTab { location: l } => location(l)?,
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
                own(*strip)?
            }
            DesktopCommand::CreateStrip { .. } => (),
            DesktopCommand::RenameWorkspace { .. } => {
                if collab_types::caps::decide_capability(
                    &actor,
                    &target,
                    collab_types::caps::Verb::Admin,
                    &caps,
                    Some(
                        web_time::SystemTime::now()
                            .duration_since(web_time::SystemTime::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs(),
                    ),
                ) != collab_types::caps::CapDecision::Allow
                {
                    return Err(rejected(
                        "workspace naming requires room administration".into(),
                    ));
                }
            }
        }
        let maximum = workspace
            .strips
            .iter()
            .flat_map(|s| {
                std::iter::once(s.id.0).chain(
                    s.window_groups
                        .iter()
                        .flat_map(|g| std::iter::once(g.id).chain(g.tabs.iter().map(|t| t.id))),
                )
            })
            .filter(|id| qos_desktop::identity::peer_salt_of(*id) == salt)
            .map(qos_desktop::identity::counter_of)
            .max()
            .unwrap_or(0);
        let stored = doc
            .get(ROOT, "desktop_counter_v1")
            .map_err(|e| rejected(e.to_string()))?
            .and_then(|(v, _)| v.to_u64())
            .unwrap_or(1);
        let mut counter = stored.max(maximum + 1);
        let mut documents = vec![];
        let mut routes = RoomProjection::record_routes(&doc);
        qos_desktop::session_commands::apply_command(
            workspace,
            &command.command,
            &mut || qos_desktop::identity::mint_in_namespace(salt, &mut counter),
            &mut |app, id| {
                let definition = checked_application(app).ok_or_else(|| {
                    "application has no installed checked record factory".to_owned()
                })?;
                let initialize: fn(&mut ui::automerge::AutoCommit) -> Result<(), String> = match app
                {
                    "dev.qos.word-processor" => word_processor::initialize_empty_document,
                    "dev.qos.notes" => notes::notebook::initialize_empty,
                    "dev.qos.calculator" => calculator::doc::initialize_empty,
                    "dev.qos.chat" => {
                        |_| Err("existing package-primary Chat record required".into())
                    }
                    _ => unreachable!("checked application factory must be initialized"),
                };
                let checkout =
                    qos_desktop::records::component_record(app, id, definition.record_kind.clone());
                let record = if checkout.id.is_package_primary() {
                    String::new()
                } else {
                    checkout.id.as_str().to_string()
                };
                self.peer().check_document_read(
                    caller.device_public_key,
                    app,
                    &record,
                    web_time::SystemTime::now(),
                )?;
                if !checkout.id.is_package_primary() {
                    if self.peer().document(app, &record).is_some() {
                        return Err("new tab identity already has retained record custody".into());
                    }
                    let cache = ui::portable_storage::PortableStorage::new(
                        "desktop-command",
                        &request.room,
                    )?;
                    let records = ui::record::RecordOwners::portable(cache)
                        .open(app, &request.room)
                        .map_err(|e| e.to_string())?;
                    records
                        .reserve_record(&checkout)
                        .map_err(|e| e.to_string())?
                        .initialize(initialize)
                        .map_err(|e| e.to_string())?;
                    let bytes = records
                        .read(&checkout.id, |doc| doc.clone().save())
                        .ok_or("checked new record custody missing")?;
                    documents.push((app.to_string(), record.clone(), bytes));
                } else if self.peer().document(app, "").is_none() {
                    return Err("package-primary record missing".into());
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
            RoomProjection::project_owned_command_contents(&mut doc, &workspace.strips, salt);
        }
        for strip in &workspace.strips {
            for group in &strip.window_groups {
                for tab in &group.tabs {
                    if let Some(record) = routes.get(&tab.id) {
                        RoomProjection::bind_record(&mut doc, strip.id.0, group.id, tab.id, record)
                            .map_err(rejected)?;
                    }
                }
            }
        }
        doc.put(ROOT, "desktop_counter_v1", counter)
            .map_err(|e| rejected(e.to_string()))?;
        if matches!(command.command, DesktopCommand::RenameWorkspace { .. }) {
            doc.put(ROOT, "name", &workspace.name)
                .map_err(|e| rejected(e.to_string()))?;
        }
        doc.commit();
        let mut candidate = self.peer().clone();
        // Checked records enter the candidate before their topology is admitted.
        for (app, record, bytes) in documents {
            candidate
                .publish_document(
                    &app,
                    &record,
                    bytes,
                    &self.identity.native,
                    web_time::SystemTime::now(),
                )
                .map_err(rejected)?;
        }
        candidate
            .receive_from(
                qos_peer_state::TransportOrigin::RelayDelta(
                    self.identity.native_cert.device_public_key,
                ),
                qos_peer_state::NeighbourMsg::SyncResp {
                    room: request.room.clone(),
                    changes: doc
                        .get_changes(&base_heads)
                        .iter()
                        .map(|c| c.raw_bytes().to_vec())
                        .collect(),
                },
                web_time::SystemTime::now(),
            )
            .map_err(rejected)?;
        let committed = self
            .commit_mutation_candidate(candidate, self.directory().to_vec())
            .map_err(|e| DesktopCommandError::Uncertain(e.to_string()))?;
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
