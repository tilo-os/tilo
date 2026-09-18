//! Initialize canonical app records only during explicit creation of a new session.
use qos_peer_state::{SeedDocument, SeedState};
use qos_user_identity::DeviceCert;
use ui::{
    portable_storage::PortableStorage,
    record::{RecordCheckout, RecordId, RecordOwners},
};
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
        room_changes: qos_peer_state::seed_room(room, authority, roster)?,
        documents,
    })
}

/// One-time migration for the durable native authority. Existing records and
/// existing topology are never recreated. Caller commits the candidate behind
/// the same signed aggregate barrier before publishing its checkpoint.
pub(crate) fn bootstrap_desktop(
    peer: &mut qos_peer_state::StatePeer,
    key: &ed25519_dalek::SigningKey,
) -> Result<bool, String> {
    use qos_desktop::{
        AppKind, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth,
        projection::RoomProjection,
    };
    use ui::automerge::{ObjType, ROOT, ReadDoc, transaction::Transactable};
    if key.verifying_key().to_bytes() != peer.authority().device_public_key {
        return Err("desktop migration requires pinned authority".into());
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
        .document("dev.qos.word-processor", "tab:0000000000000001")
        .is_none()
        || peer.document("dev.qos.chat", "").is_none()
    {
        return Err(
            "legacy canonical records missing; desktop migration cannot replace them".into(),
        );
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
    let mut mint = || qos_desktop::identity::mint_in_namespace(salt, &mut counter);
    let strip = mint()?;
    let document_group = mint()?;
    let writer = mint()?;
    let notes = mint()?;
    let chat_group = mint()?;
    let chat = mint()?;
    let definition = notes::definition();
    let checkout = qos_desktop::records::component_record(
        definition.app_id,
        notes,
        definition.record_kind.clone(),
    );
    let cache = PortableStorage::new("native-desktop-migration", peer.room())?;
    let records = RecordOwners::portable(cache)
        .open(definition.app_id, peer.room())
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
                id: document_group,
                tabs: vec![
                    tile(writer, "Writer", "dev.qos.word-processor"),
                    tile(notes, "Notes", "dev.qos.notes"),
                ],
                active_tab: writer,
                width: WindowWidth::Normal,
                maximized: false,
            },
            WindowGroup {
                id: chat_group,
                tabs: vec![tile(chat, "Chat", "dev.qos.chat")],
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
    RoomProjection::project_owned_contents(&mut doc, &strips, salt, None);
    RoomProjection::bind_record(
        &mut doc,
        strip,
        document_group,
        writer,
        "tab:0000000000000001",
    )?;
    RoomProjection::bind_record(&mut doc, strip, chat_group, chat, "")?;
    doc.commit();
    let seed = SeedState {
        room_changes: doc
            .get_changes(&[])
            .iter()
            .map(|c| c.raw_bytes().to_vec())
            .collect(),
        documents: vec![SeedDocument {
            app_id: definition.app_id.into(),
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
