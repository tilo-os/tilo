//! Read-only proof of a document actually retained by the scratch native owner.
use automerge::{ROOT, ReadDoc as _};
use std::{io::Read as _, path::PathBuf, sync::Arc, time::SystemTime};
use tilo_desktop_model::{RoomId, WorkspaceId};
use tilo_joined_room::{Change, NativeAuthorityStorage, NativeWorkspaceOwner, Store};

struct ReadOnlyStore;

impl Store for ReadOnlyStore {
    fn healthy(&self) -> bool {
        true
    }

    fn uncertain(&self, _room: RoomId) -> bool {
        false
    }

    fn slot_room(&self, _slot: WorkspaceId) -> Option<RoomId> {
        None
    }

    fn publish(
        &self,
        _next: &NativeAuthorityStorage,
        _change: Change<'_>,
        _install: &mut dyn FnMut(),
    ) -> Result<(), String> {
        Err("native proof inspector refuses every publication".into())
    }
}

fn matches_category(bytes: &[u8], room: &str, name: &str) -> Result<bool, String> {
    let document = automerge::Automerge::load(bytes).map_err(|error| error.to_string())?;
    let Some((automerge::Value::Object(automerge::ObjType::Map), categories)) = document
        .get(ROOT, "categories")
        .map_err(|error| error.to_string())?
    else {
        return Ok(false);
    };
    let mut matches = 0usize;
    for category in document.map_range(&categories, ..) {
        let workspace = document
            .get(category.id(), "workspace")
            .map_err(|error| error.to_string())?;
        let label = document
            .get(category.id(), "name")
            .map_err(|error| error.to_string())?;
        if workspace.is_some_and(|(value, _)| value.as_str() == Some(room))
            && label.is_some_and(|(value, _)| value.as_str() == Some(name))
        {
            matches = matches.checked_add(1).ok_or("category count overflow")?;
        }
    }
    Ok(matches == 1)
}

fn check_retained_publication(
    outgoing: &tilo_joined_room::NativeOutgoing<'_>,
    document: &[u8],
    room: &str,
    record: &str,
) -> Result<(), String> {
    let mut retained = automerge::AutoCommit::new();
    for entry in outgoing.entries() {
        if let Ok(tilo_peer_state::NeighbourMsg::AppDocument {
            app_id,
            record_id,
            envelope,
            ..
        }) = postcard::from_bytes(entry.bytes())
            && app_id == "so.tilo.chat"
            && record_id == record
        {
            let mutation = collab_types::DocumentMutation::decode(&envelope.payload)
                .map_err(|error| error.to_string())?;
            tilo_peer_state::document::merge_committed(&mut retained, &mutation)
                .map_err(|error| format!("outgoing document cannot be admitted: {error:?}"))?;
        }
    }
    let committed = automerge::Automerge::load(document).map_err(|error| error.to_string())?;
    let mut expected_heads = committed.get_heads();
    let mut retained_heads = retained.get_heads();
    expected_heads.sort_unstable();
    retained_heads.sort_unstable();
    if retained_heads != expected_heads
        || !matches_category(&retained.save(), room, "Lifecycle proof")?
    {
        return Err("exact document has no retained outgoing publication".into());
    }
    Ok(())
}

fn read_owner(path: &std::path::Path) -> Result<NativeWorkspaceOwner, Box<dyn std::error::Error>> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(u64::try_from(tilo_joined_room::MAX_STATE_BYTES)?.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() > tilo_joined_room::MAX_STATE_BYTES {
        return Err("scratch state exceeds normal owner bound".into());
    }
    let mut state: serde_json::Value = serde_json::from_slice(&bytes)?;
    let authority: NativeAuthorityStorage = serde_json::from_value(
        state
            .get_mut("native_authority")
            .ok_or("actual native owner record is absent")?
            .take(),
    )?;
    Ok(NativeWorkspaceOwner::restore(
        Arc::new(ReadOnlyStore),
        authority,
        SystemTime::now(),
    )?)
}

struct InspectionRoute {
    path: PathBuf,
    room_text: String,
    record: String,
    room: RoomId,
}

fn inspection_route() -> Result<InspectionRoute, Box<dyn std::error::Error>> {
    let mut arguments = std::env::args_os().skip(2);
    let path = PathBuf::from(arguments.next().ok_or("missing scratch state path")?);
    let room_text = arguments
        .next()
        .and_then(|value| value.into_string().ok())
        .ok_or("missing room")?;
    let record = arguments
        .next()
        .and_then(|value| value.into_string().ok())
        // The SDK's normal package-primary route is the empty record id.
        // Inspect that exact published route; never manufacture a surrogate.
        .filter(|value| value.len() <= 4096 && !value.chars().any(char::is_control))
        .ok_or("missing bounded record")?;
    if arguments.next().is_some()
        || !path.is_absolute()
        || std::env::var_os("TILO_STATE_PATH").map(PathBuf::from) != Some(path.clone())
    {
        return Err("inspector must read its exact scratch session state file".into());
    }
    let room = RoomId::parse(&room_text).ok_or("invalid canonical room")?;
    Ok(InspectionRoute {
        path,
        room_text,
        record,
        room,
    })
}

/// Whether the proof agent is admitted to the room, able to change it, by a
/// live grant on the room itself.
fn exact_agent_membership(policy: &tilo_action_policy::RoomPolicy, now: u64) -> bool {
    use tilo_action_policy::{ActionKind, Answer, Authority, Subject};
    let agent = Subject::agent("bootstrap-proof");
    let cx = tilo_desktop_model::context_at(now);
    let room = policy.node();
    policy.grants_of(&agent).any(|grant| {
        grant.node == room
            && grant.answer == Answer::Allow
            && grant.kinds.contains(ActionKind::Undoable)
            && grant.is_live(&cx)
    }) && Authority::person(policy.party_of(agent))
        .decide(&room, ActionKind::Undoable, &cx)
        .answer
        == Answer::Allow
}

fn check_agent_membership(
    owner: &NativeWorkspaceOwner,
    room: RoomId,
    room_text: &str,
    now: SystemTime,
) -> Result<(), Box<dyn std::error::Error>> {
    let generation = owner.with_outgoing(room, now, |view, _| {
        let member = view
            .peer()
            .permissions(view.device().device_public_key, now)?;
        let seconds = now
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_err(|_| "owner policy clock is unavailable")?
            .as_secs();
        if member.policy.room != room_text || !exact_agent_membership(&member.policy, seconds) {
            return Err("exact agent membership was not durably admitted".into());
        }
        Ok(view.generation())
    })?;
    println!(
        "{}",
        serde_json::json!({
            "room": room_text, "generation": generation,
            "agent": "bootstrap-proof", "membership_durable": true,
            "managed_agent_launched": false,
        })
    );
    Ok(())
}

fn native_record_observation(
    owner: &NativeWorkspaceOwner,
    room: RoomId,
    room_text: &str,
    record: &str,
    now: SystemTime,
) -> Result<serde_json::Value, String> {
    owner.with_outgoing(room, now, |view, _| {
        view.peer().check_document_read(
            view.device().device_public_key,
            "so.tilo.chat",
            record,
            now,
        )?;
        let member = view
            .peer()
            .permissions(view.device().device_public_key, now)?;
        let sandbox = view.peer().governed_sandbox()?;
        let seconds = now
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_err(|_| "owner policy clock is unavailable")?
            .as_secs();
        let app_write_permitted = collab_types::caps::decide_capability(
            &collab_types::caps::Actor::app("so.tilo.chat").in_room(room_text),
            &collab_types::caps::ResourceRef::Workspace,
            collab_types::caps::Access::Write,
            &sandbox,
            Some(seconds),
        ) == collab_types::caps::CapDecision::Allow;
        let person_write_permitted = member.allows(
            &member.policy.node(),
            tilo_action_policy::ActionKind::Undoable,
            now,
        );
        let document = view
            .peer()
            .document("so.tilo.chat", record)
            .map(|bytes| automerge::Automerge::load(&bytes))
            .transpose()
            .map_err(|error| error.to_string())?;
        let workspace_bound = document
            .as_ref()
            .map(|document| -> Result<bool, String> {
                let workspaces = document
                    .get(ROOT, "workspaces")
                    .map_err(|error| error.to_string())?;
                match workspaces {
                    Some((automerge::Value::Object(automerge::ObjType::Map), map)) => Ok(document
                        .get(&map, room_text)
                        .map_err(|error| error.to_string())?
                        .is_some()),
                    _ => Ok(false),
                }
            })
            .transpose()?
            .unwrap_or(false);
        Ok(serde_json::json!({
            "room": room_text, "app": "so.tilo.chat", "record": record,
            "generation": view.generation(), "record_exists": document.is_some(),
            "workspace_bound": workspace_bound,
            "app_write_permitted": app_write_permitted,
            "person_write_permitted": person_write_permitted,
            "workspace_administration_permitted": view.peer()
                .check_workspace_administration(view.device().device_public_key, now).is_ok(),
        }))
    })
}

pub(super) fn check() -> Result<(), Box<dyn std::error::Error>> {
    let enrollment = std::env::args().nth(1).as_deref() == Some("--check-native-enrollment");
    let record_status = std::env::args().nth(1).as_deref() == Some("--check-native-chat-record");
    let agent_membership = std::env::args().nth(1).as_deref() == Some("--check-native-agent");
    let InspectionRoute {
        path,
        room_text,
        record,
        room,
    } = inspection_route()?;
    let now = SystemTime::now();
    let owner = read_owner(&path)?;
    if agent_membership {
        return check_agent_membership(&owner, room, &room_text, now);
    }
    if record_status {
        let observation = native_record_observation(&owner, room, &room_text, &record, now)?;
        println!("{observation}");
        return Ok(());
    }
    let generation = owner.with_outgoing(room, now, |view, outgoing| {
        if enrollment {
            if view.peer().room() != room_text
                || view.peer().admitted_room().is_none()
                || view.host().is_some()
                || view.governor().device_public_key != view.device().device_public_key
            {
                return Err("exact current-device native owner is not admitted".into());
            }
            view.peer()
                .check_workspace_administration(view.device().device_public_key, now)?;
            return Ok(view.generation());
        }
        let tilo_joined_room::DocumentRead::Captured(document) = view.read_document(
            view.device().device_public_key,
            "so.tilo.chat",
            &record,
            None,
            4 * 1024 * 1024,
            now,
        )?
        else {
            return Err("inspector did not receive an actual canonical document".into());
        };
        if !matches_category(&document.bytes, &room_text, "Lifecycle proof")? {
            return Err("exact Category was not durably admitted".into());
        }
        check_retained_publication(&outgoing, &document.bytes, &room_text, &record)?;
        Ok(view.generation())
    })?;
    let observation = if enrollment {
        serde_json::json!({
            "room": room_text, "generation": generation, "admitted": true,
            "governor_is_current_device": true, "pinned_host": false,
            "workspace_administration_permitted": true,
        })
    } else {
        serde_json::json!({
            "room": room_text, "app": "so.tilo.chat", "record": record,
            "generation": generation, "category_durable": true,
            "outgoing_publication_retained": true,
        })
    };
    println!("{observation}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{ReadOnlyStore, exact_agent_membership, matches_category};
    use automerge::{AutoCommit, ObjType, ROOT, transaction::Transactable as _};
    use tilo_joined_room::{Change, NativeAuthorityStorage, Store as _};

    #[test]
    fn membership_requires_live_exact_room_subject_and_restriction() {
        use tilo_action_policy::roles::AgentPreset;
        use tilo_action_policy::{Answer, Grant, KindSet, Lifetime, Role, RoomPolicy, Subject};
        let owner = Subject::Role(Role::Owner);
        let room = |agent: &str, preset: AgentPreset| {
            let mut policy = RoomPolicy::unowned("room", &owner);
            policy.grants.extend(preset.grants("room", agent, &owner));
            policy
        };
        let member = room("bootstrap-proof", AgentPreset::Operator);
        assert!(exact_agent_membership(&member, 10));
        let mut elsewhere = RoomPolicy::unowned("room", &owner);
        elsewhere.grants.extend(AgentPreset::Operator.grants(
            "other-room",
            "bootstrap-proof",
            &owner,
        ));
        assert!(!exact_agent_membership(&elsewhere, 10));
        let mut expired = member.clone();
        for grant in &mut expired.grants {
            grant.lifetime = Lifetime::Until(10_000);
        }
        assert!(!exact_agent_membership(&expired, 11));
        assert!(!exact_agent_membership(
            &room("another-agent", AgentPreset::Operator),
            10
        ));
        assert!(
            !exact_agent_membership(&room("bootstrap-proof", AgentPreset::Observer), 10),
            "reading the room is not the membership the proof needs"
        );
        let mut app_only = RoomPolicy::unowned("room", &owner);
        app_only.grants.push(Grant::new(
            "app-only",
            Subject::agent("bootstrap-proof"),
            app_only.node().app("so.tilo.chat"),
            KindSet::ALL,
            Answer::Allow,
            owner.clone(),
        ));
        assert!(!exact_agent_membership(&app_only, 10));
        let mut restricted = member;
        restricted.grants.push(Grant::new(
            "restricted",
            Subject::agent("bootstrap-proof"),
            restricted.node(),
            KindSet::UNDOABLE,
            Answer::Deny,
            owner,
        ));
        assert!(!exact_agent_membership(&restricted, 10));
    }

    #[test]
    fn inspector_requires_exact_unique_room_category_and_refuses_writes() {
        let mut document = AutoCommit::new();
        assert!(!matches_category(&document.save(), "room", "Lifecycle proof").unwrap());
        let categories = document
            .put_object(ROOT, "categories", ObjType::Map)
            .unwrap();
        let category = document
            .put_object(&categories, "category", ObjType::Map)
            .unwrap();
        document.put(&category, "workspace", "room").unwrap();
        document.put(&category, "name", "Lifecycle proof").unwrap();
        let bytes = document.save();
        assert!(matches_category(&bytes, "room", "Lifecycle proof").unwrap());
        assert!(!matches_category(&bytes, "other-room", "Lifecycle proof").unwrap());
        assert!(!matches_category(&bytes, "room", "different").unwrap());
        let duplicate = document
            .put_object(&categories, "duplicate", ObjType::Map)
            .unwrap();
        document.put(&duplicate, "workspace", "room").unwrap();
        document.put(&duplicate, "name", "Lifecycle proof").unwrap();
        assert!(!matches_category(&document.save(), "room", "Lifecycle proof").unwrap());
        assert!(matches_category(&[0xff], "room", "Lifecycle proof").is_err());
        let mut installed = false;
        assert!(
            ReadOnlyStore
                .publish(
                    &NativeAuthorityStorage::default(),
                    Change::Pending,
                    &mut || installed = true
                )
                .is_err()
        );
        assert!(!installed);
    }
}
