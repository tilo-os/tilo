//! Read-only proof of a document actually retained by the scratch native owner.
use automerge::{ROOT, ReadDoc as _};
use std::{io::Read as _, path::PathBuf, sync::Arc, time::SystemTime};
use tilo_desktop_model::{RoomId, WorkspaceId};
use tilo_joined_room::{Change, NativeAuthorityStorage, NativeWorkspaceOwner, Store};

struct ReadOnlyStore {
    history: tilo_joined_room::HistoryContent,
}

impl ReadOnlyStore {
    fn for_state_file(path: &std::path::Path) -> Self {
        Self {
            history: tilo_joined_room::HistoryContent::for_state_file(path),
        }
    }
}

impl Store for ReadOnlyStore {
    fn read_history(&self, hash: [u8; 32]) -> Result<Vec<u8>, String> {
        self.history.read(hash)
    }

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
        Arc::new(ReadOnlyStore::for_state_file(path)),
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

fn inspect_admitted_person(route: &InspectionRoute) -> Result<String, Box<dyn std::error::Error>> {
    let owner = read_owner(&route.path)?;
    let now = SystemTime::now();
    owner
        .with_current(route.room, |view| {
            if view.peer().room() != route.room_text
                || view.peer().admitted_room().is_none()
                || view.host().is_some()
                || view.governor().device_public_key != view.device().device_public_key
            {
                return Err("exact current-device native owner is not admitted".into());
            }
            view.peer()
                .check_workspace_administration(view.device().device_public_key, now)?;
            let (_, _, person) = view
                .peer()
                .permissions(view.device().device_public_key, now)?;
            let expected = collab_types::caps::Principal::person_from_key(
                collab_types::Ed25519PublicKey::from_bytes(view.device().user_public_key),
            );
            if person != expected.id() {
                return Err("admitted Person differs from the current device certificate".into());
            }
            Ok(person)
        })
        .map_err(Into::into)
}

pub(super) fn check() -> Result<(), Box<dyn std::error::Error>> {
    let mode = std::env::args().nth(1);
    let enrollment = mode.as_deref() == Some("--check-native-enrollment");
    let InspectionRoute {
        path,
        room_text,
        record,
        room,
    } = inspection_route()?;
    if mode.as_deref() == Some("--inspect-native-person") {
        let person = inspect_admitted_person(&InspectionRoute {
            path,
            room_text,
            record,
            room,
        })?;
        println!("{}", serde_json::json!({"person": person}));
        return Ok(());
    }
    let now = SystemTime::now();
    let owner = read_owner(&path)?;
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
    use super::{ReadOnlyStore, matches_category};
    use automerge::{AutoCommit, ObjType, ROOT, transaction::Transactable as _};
    use tilo_joined_room::{Change, NativeAuthorityStorage, Store as _};

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
            ReadOnlyStore::for_state_file(std::path::Path::new("/unopened-proof/state.json"))
                .publish(
                    &NativeAuthorityStorage::default(),
                    Change::Pending,
                    &mut || installed = true
                )
                .is_err()
        );
        assert!(!installed);
    }

    #[test]
    fn inspector_reads_verified_history_only_beside_its_exact_state_file()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("state.json");
        let content = tilo_joined_room::HistoryContent::for_state_file(&path);
        let bytes = b"retained owner history";
        let hash = content.put(bytes)?;
        let inspector = ReadOnlyStore::for_state_file(&path);
        assert_eq!(inspector.read_history(hash)?, bytes);
        assert!(
            inspector
                .put_history(b"inspector must not publish history")
                .is_err()
        );
        assert!(inspector.read_history([0; 32]).is_err());
        let other = ReadOnlyStore::for_state_file(&directory.path().join("other/state.json"));
        assert!(other.read_history(hash).is_err());
        let address = blake3::Hash::from_bytes(hash).to_hex().to_string();
        let shard = address.get(..2).ok_or("invalid test content address")?;
        std::fs::write(
            directory.path().join("content").join(shard).join(address),
            b"corrupt",
        )?;
        assert!(inspector.read_history(hash).is_err());
        Ok(())
    }
}
