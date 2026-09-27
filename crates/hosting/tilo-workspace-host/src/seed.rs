//! Initialize canonical app records only during explicit creation of a new session.
use tilo_peer_state::{SeedDocument, SeedState};
use tilo_user_identity::DeviceCert;
use ui::{
    portable_storage::PortableStorage,
    record::{RecordCheckout, RecordId, RecordOwners},
};

/// A new workspace has governance and topology custody, but no app records.
pub(crate) fn empty_workspace(
    room: &str,
    authority: &DeviceCert,
    roster: &[DeviceCert],
    name: &str,
) -> Result<SeedState, String> {
    use tilo_automerge::ApplyIncoming as _;
    use ui::automerge::{AutoCommit, Change, ObjType, ROOT, transaction::Transactable};

    let mut doc = AutoCommit::new().with_actor(ui::automerge::ActorId::from(
        authority.device_public_key.to_vec(),
    ));
    let changes = tilo_peer_state::seed_room(room, authority, roster)?
        .into_iter()
        .map(Change::from_bytes)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    doc.apply_incoming(changes).map_err(|e| e.to_string())?;
    doc.put(ROOT, "name", name).map_err(|e| e.to_string())?;
    doc.put_object(ROOT, "contents", ObjType::Map)
        .map_err(|e| e.to_string())?;
    let owners = doc
        .put_object(ROOT, "contents_owners_v1", ObjType::Map)
        .map_err(|e| e.to_string())?;
    let salt = rand::random::<u16>().max(1);
    doc.put(
        owners,
        format!("{salt:04x}"),
        hex::encode(authority.device_public_key),
    )
    .map_err(|e| e.to_string())?;
    doc.put(ROOT, "desktop_counter_v1", 1_u64)
        .map_err(|e| e.to_string())?;
    doc.commit();
    Ok(SeedState {
        room_changes: doc
            .get_changes(&[])
            .iter()
            .map(|c| c.raw_bytes().to_vec())
            .collect(),
        documents: vec![],
    })
}

/// Recordless sessions are complete only with initialized native custody and
/// no app tabs that would require missing records. Empty named strips are valid.
pub(crate) fn recordless_desktop_is_initialized(peer: &tilo_peer_state::StatePeer) -> bool {
    use ui::automerge::{ObjType, ROOT, ReadDoc, Value};

    let Some(doc) = peer.admitted_room() else {
        return false;
    };
    if !matches!(
        doc.get(ROOT, "contents"),
        Ok(Some((Value::Object(ObjType::Map), _)))
    ) {
        return false;
    }
    let Ok(Some((Value::Object(ObjType::Map), owners))) = doc.get(ROOT, "contents_owners_v1")
    else {
        return false;
    };
    let authority = hex::encode(peer.authority().device_public_key);
    let owns_namespace = doc.keys(&owners).any(|key| {
        u16::from_str_radix(&key, 16).is_ok_and(|salt| salt != 0)
            && doc
                .get(&owners, key)
                .ok()
                .flatten()
                .is_some_and(|(value, _)| value.as_str() == Some(authority.as_str()))
    });
    owns_namespace
        && tilo_desktop_model::projection::contents_of(doc)
            .iter()
            .all(|strip| strip.groups.iter().all(|group| group.tabs.is_empty()))
}

pub(crate) fn seed(
    room: &str,
    authority: &DeviceCert,
    roster: &[DeviceCert],
) -> Result<SeedState, String> {
    let cache = PortableStorage::new("native-session-initialize", room)?;
    let mut owners = RecordOwners::portable(cache);
    let mut documents = vec![];
    for (definition, checkout) in [
        (
            word_processor::definition(),
            RecordCheckout::new(
                RecordId::for_tab(1),
                word_processor::definition().record_kind.clone(),
            ),
        ),
        (
            chat::definition(),
            RecordCheckout::package_primary(chat::definition().record_kind.clone()),
        ),
    ] {
        let records = owners
            .open(definition.app_id, room)
            .map_err(|e| e.to_string())?;
        let mut reserved = records
            .reserve_record(&checkout)
            .map_err(|e| e.to_string())?;
        reserved
            .initialize(|doc| {
                if checkout.id.is_package_primary() {
                    chat::doc::ChatDoc::seed_starter(doc, "native", "Native workspace peer", "NW");
                    Ok(())
                } else {
                    word_processor::initialize_empty_document(doc)
                }
            })
            .map_err(|e| e.to_string())?;
        let bytes = records
            .read(&checkout.id, |doc| doc.clone().save())
            .ok_or("seed owner missing")?;
        documents.push(SeedDocument {
            app_id: definition.app_id.into(),
            record_id: if checkout.id.is_package_primary() {
                String::new()
            } else {
                checkout.id.as_str().into()
            },
            kind: definition.record_kind.as_str().into(),
            bytes,
        });
    }
    Ok(SeedState {
        room_changes: tilo_peer_state::seed_room(room, authority, roster)?,
        documents,
    })
}

/// Lay out the initial desktop topology (Writer, Notes and Chat) for a freshly
/// seeded room owned by the native authority. A room that already has
/// topology is left unchanged. Caller commits the candidate behind the same
/// signed aggregate barrier before publishing it.
pub(crate) fn bootstrap_desktop(
    peer: &mut tilo_peer_state::StatePeer,
    key: &ed25519_dalek::SigningKey,
) -> Result<bool, String> {
    use tilo_desktop_model::{AppKind, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth};
    use ui::automerge::{ObjType, ROOT, ReadDoc, transaction::Transactable};
    if key.verifying_key().to_bytes() != peer.authority().device_public_key {
        return Err("desktop topology requires pinned authority".into());
    }
    let mut doc = peer
        .admitted_room()
        .ok_or("room governance missing")?
        .clone();
    if doc
        .get(ROOT, "contents")
        .map_err(|e| e.to_string())?
        .is_some()
    {
        return Ok(false);
    }
    if peer
        .document("so.tilo.word-processor", "tab:0000000000000001")
        .is_none()
        || peer.document("so.tilo.chat", "").is_none()
    {
        return Err("canonical records missing; desktop topology cannot replace them".into());
    }
    let existing_owners = doc
        .get(ROOT, "contents_owners_v1")
        .map_err(|e| e.to_string())?
        .map(|(_, id)| id);
    let reserved = existing_owners
        .as_ref()
        .map(|id| {
            doc.keys(id)
                .filter_map(|key| u16::from_str_radix(&key, 16).ok())
                .collect::<std::collections::BTreeSet<_>>()
        })
        .unwrap_or_default();
    let start = u32::from(rand::random::<u16>());
    let salt = (0..u32::from(u16::MAX))
        .map(|offset| ((start + offset) % u32::from(u16::MAX) + 1) as u16)
        .find(|salt| !reserved.contains(salt))
        .ok_or("topology namespaces exhausted")?;
    let mut counter = 1;
    let mut mint = || tilo_desktop_model::mint_in_namespace(salt, &mut counter);
    let strip = mint()?;
    let writer_group = mint()?;
    let writer = mint()?;
    let notes_group = mint()?;
    let notes = mint()?;
    let chat_group = mint()?;
    let chat = mint()?;
    let definition = notes::definition();
    let app_id = definition.app_id;
    let checkout = ui::record::RecordCheckout::for_component_tab(
        app_id,
        notes,
        definition.record_kind.clone(),
    );
    let cache = PortableStorage::new("native-desktop-topology", peer.room())?;
    let records = RecordOwners::portable(cache)
        .open(app_id, peer.room())
        .map_err(|e| e.to_string())?;
    let mut reservation = records
        .reserve_record(&checkout)
        .map_err(|e| e.to_string())?;
    reservation
        .initialize(notes::notebook::initialize_empty)
        .map_err(|e| e.to_string())?;
    let bytes = records
        .read(&checkout.id, |doc| doc.clone().save())
        .ok_or("Notes owner missing")?;
    let tile = |id, title: &str, app: &str| Tab {
        id,
        title: title.into(),
        tile: TileContent::AppPlaceholder {
            kind: AppKind::Other(app.into()),
            label: title.into(),
        },
    };
    let strips = vec![Strip {
        id: StripId(strip),
        name: "Workspace".into(),
        window_groups: vec![
            WindowGroup {
                id: writer_group,
                tabs: vec![tile(writer, "Writer", "so.tilo.word-processor")],
                active_tab: writer,
                width: WindowWidth::Normal,
                maximized: false,
            },
            WindowGroup {
                id: notes_group,
                tabs: vec![tile(notes, "Notes", "so.tilo.notes")],
                active_tab: notes,
                width: WindowWidth::Normal,
                maximized: false,
            },
            WindowGroup {
                id: chat_group,
                tabs: vec![tile(chat, "Chat", "so.tilo.chat")],
                active_tab: chat,
                width: WindowWidth::Normal,
                maximized: false,
            },
        ],
    }];
    doc.set_actor(ui::automerge::ActorId::from(
        key.verifying_key().to_bytes().to_vec(),
    ));
    let owners = match existing_owners {
        Some(owners) => owners,
        None => doc
            .put_object(ROOT, "contents_owners_v1", ObjType::Map)
            .map_err(|e| e.to_string())?,
    };
    doc.put(
        owners,
        format!("{salt:04x}"),
        hex::encode(key.verifying_key().to_bytes()),
    )
    .map_err(|e| e.to_string())?;
    tilo_desktop_model::projection::project_owned_contents(
        &mut doc,
        &strips,
        salt,
        None,
        tilo_desktop_model::projection::EmptyStrips::Skip,
    )
    .map_err(|e| e.to_string())?;
    tilo_desktop_model::apps::bind_tab_record(
        &mut doc,
        tilo_desktop_model::StripId(strip),
        writer_group,
        writer,
        "tab:0000000000000001",
    )
    .map_err(|e| e.to_string())?;
    tilo_desktop_model::apps::bind_tab_record(
        &mut doc,
        tilo_desktop_model::StripId(strip),
        notes_group,
        notes,
        checkout.id.as_str(),
    )
    .map_err(|e| e.to_string())?;
    tilo_desktop_model::apps::bind_tab_record(
        &mut doc,
        tilo_desktop_model::StripId(strip),
        chat_group,
        chat,
        "",
    )
    .map_err(|e| e.to_string())?;
    doc.commit();
    let seed = SeedState {
        room_changes: doc
            .get_changes(&[])
            .iter()
            .map(|c| c.raw_bytes().to_vec())
            .collect(),
        documents: vec![SeedDocument {
            app_id: app_id.into(),
            record_id: checkout.id.as_str().into(),
            kind: definition.record_kind.as_str().into(),
            bytes,
        }],
    };
    let mut candidate = peer.clone();
    candidate.install_seed(seed, key, web_time::SystemTime::now())?;
    *peer = candidate;
    Ok(true)
}
