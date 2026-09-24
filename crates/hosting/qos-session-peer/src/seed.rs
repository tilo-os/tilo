//! Initialize canonical app records only during explicit creation of a new session.
use qos_peer_state::{SeedDocument, SeedState};
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
    use ui::automerge::{AutoCommit, Change, ObjType, ROOT, transaction::Transactable};

    let mut doc = AutoCommit::new().with_actor(ui::automerge::ActorId::from(
        authority.device_public_key.to_vec(),
    ));
    let changes = qos_peer_state::seed_room(room, authority, roster)?
        .into_iter()
        .map(Change::from_bytes)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    doc.apply_changes(changes).map_err(|e| e.to_string())?;
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
    // This is current topology, so legacy app migrations must never seed it.
    doc.put(ROOT, "writer_notes_split_v1", 1_u64)
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
pub(crate) fn recordless_desktop_is_initialized(peer: &qos_peer_state::StatePeer) -> bool {
    use qos_desktop::projection::RoomProjection;
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
                .is_some_and(|(value, _)| value.to_str() == Some(authority.as_str()))
    });
    owns_namespace
        && RoomProjection::contents_of(doc)
            .iter()
            .all(|strip| strip.groups.iter().all(|group| group.tabs.is_empty()))
}

struct LegacyWriterNotesSplit {
    strips: Vec<qos_desktop::Strip>,
    salt: u16,
    strip: u64,
    writer_group: u64,
    notes_group: u64,
    notes_tab: u64,
    notes_record: String,
    next_counter: u64,
}

fn legacy_writer_notes_split(
    doc: &ui::automerge::AutoCommit,
    key: &ed25519_dalek::SigningKey,
) -> Result<Option<LegacyWriterNotesSplit>, String> {
    use qos_desktop::{AppKind, TileContent, identity, projection::RoomProjection};
    use ui::automerge::{ROOT, ReadDoc};

    let owners = doc
        .get(ROOT, "contents_owners_v1")
        .map_err(|e| e.to_string())?
        .map(|(_, id)| id)
        .ok_or("desktop identity custody is missing")?;
    let authority = hex::encode(key.verifying_key().to_bytes());
    let owned_salts = doc
        .keys(&owners)
        .filter_map(|entry| {
            let salt = u16::from_str_radix(&entry, 16).ok()?;
            let (value, _) = doc.get(&owners, &entry).ok()??;
            (salt != 0 && value.to_str() == Some(authority.as_str())).then_some(salt)
        })
        .collect::<std::collections::BTreeSet<_>>();
    let routes = RoomProjection::record_routes(doc);
    let mut strips = RoomProjection::contents_of(doc)
        .into_iter()
        .map(RoomProjection::hydrate_strip)
        .collect::<Vec<_>>();
    let app_is = |tile: &TileContent, expected: &str| match tile {
        TileContent::AppPlaceholder {
            kind: AppKind::Other(app),
            ..
        } => app == expected,
        _ => false,
    };
    let mut matched = None;
    for (strip_index, strip) in strips.iter().enumerate() {
        for (group_index, group) in strip.window_groups.iter().enumerate() {
            if group.tabs.len() != 2 {
                continue;
            }
            let writer = group
                .tabs
                .iter()
                .position(|tab| app_is(&tab.tile, "so.tilo.word-processor"));
            let notes = group
                .tabs
                .iter()
                .position(|tab| app_is(&tab.tile, "so.tilo.notes"));
            if writer.is_some() && notes.is_some() {
                if matched.is_some() {
                    return Err("multiple legacy Writer and Notes groups".into());
                }
                matched = Some((strip_index, group_index, notes.unwrap()));
            }
        }
    }
    let Some((strip_index, group_index, notes_index)) = matched else {
        return Ok(None);
    };
    let salt = identity::peer_salt_of(strips[strip_index].window_groups[group_index].id);
    if !owned_salts.contains(&salt) {
        return Err("legacy Writer and Notes group is not owned by native authority".into());
    }
    if identity::peer_salt_of(strips[strip_index].id.0) != salt
        || strips[strip_index].window_groups[group_index]
            .tabs
            .iter()
            .any(|tab| identity::peer_salt_of(tab.id) != salt)
    {
        return Err("legacy Writer and Notes identities cross topology owners".into());
    }
    let maximum = strips
        .iter()
        .flat_map(|strip| {
            std::iter::once(strip.id.0).chain(strip.window_groups.iter().flat_map(|group| {
                std::iter::once(group.id).chain(group.tabs.iter().map(|tab| tab.id))
            }))
        })
        .filter(|id| identity::peer_salt_of(*id) == salt)
        .map(identity::counter_of)
        .max()
        .unwrap_or(0);
    let stored_counter = doc
        .get(ROOT, "desktop_counter_v1")
        .map_err(|e| e.to_string())?
        .and_then(|(value, _)| value.to_u64())
        .unwrap_or(1);
    let mut counter = stored_counter.max(
        maximum
            .checked_add(1)
            .ok_or("topology identities exhausted")?,
    );
    let notes_group = identity::mint_in_namespace(salt, &mut counter)?;
    let fresh_notes_tab = identity::mint_in_namespace(salt, &mut counter)?;
    let writer_group = strips[strip_index].window_groups[group_index].id;
    let notes = strips[strip_index].window_groups[group_index]
        .tabs
        .remove(notes_index);
    let old_notes_tab = notes.id;
    let notes_record = routes.get(&old_notes_tab).cloned().unwrap_or_else(|| {
        qos_desktop::records::component_record(
            "so.tilo.notes",
            old_notes_tab,
            notes::definition().record_kind.clone(),
        )
        .id
        .as_str()
        .to_owned()
    });
    let mut notes = notes;
    notes.id = fresh_notes_tab;
    strips[strip_index].window_groups.insert(
        group_index + 1,
        qos_desktop::WindowGroup {
            id: notes_group,
            active_tab: fresh_notes_tab,
            tabs: vec![notes],
            width: qos_desktop::WindowWidth::Normal,
            maximized: false,
        },
    );
    Ok(Some(LegacyWriterNotesSplit {
        strip: strips[strip_index].id.0,
        strips,
        salt,
        writer_group,
        notes_group,
        notes_tab: fresh_notes_tab,
        notes_record,
        next_counter: counter,
    }))
}
pub(crate) fn seed(
    room: &str,
    authority: &DeviceCert,
    roster: &[DeviceCert],
) -> Result<SeedState, String> {
    let cache = PortableStorage::new("native-session-initialize", room)?;
    let mut owners = RecordOwners::portable(cache);
    let mut documents = vec![];
    for (durable_app_id, definition, checkout) in [
        (
            "so.tilo.word-processor",
            word_processor::definition(),
            RecordCheckout::new(
                RecordId::for_tab(1),
                word_processor::definition().record_kind.clone(),
            ),
        ),
        (
            "so.tilo.chat",
            chat::definition(),
            RecordCheckout::package_primary(chat::definition().record_kind.clone()),
        ),
    ] {
        let records = owners
            .open(durable_app_id, room)
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
            app_id: durable_app_id.into(),
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

/// One-time migration for the durable native authority. The Notes record and
/// unrelated topology retain their identities; Notes receives fresh topology
/// ids so its old bound route is never rebound. Caller commits the candidate
/// behind the same signed aggregate barrier before publishing it.
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
        if doc
            .get(ROOT, "writer_notes_split_v1")
            .map_err(|e| e.to_string())?
            .and_then(|(value, _)| value.to_u64())
            == Some(1)
        {
            return Ok(false);
        }
        let Some(split) = legacy_writer_notes_split(&doc, key)? else {
            return Ok(false);
        };
        if peer
            .document("so.tilo.word-processor", "tab:0000000000000001")
            .is_none()
            || peer
                .document("so.tilo.notes", &split.notes_record)
                .is_none()
        {
            return Err("legacy Writer or Notes record missing during topology migration".into());
        }
        RoomProjection::project_owned_contents(&mut doc, &split.strips, split.salt, None);
        RoomProjection::bind_record(
            &mut doc,
            split.strip,
            split.notes_group,
            split.notes_tab,
            &split.notes_record,
        )?;
        // The original group and its Writer binding remain at the same ids.
        debug_assert_ne!(split.writer_group, split.notes_group);
        doc.put(ROOT, "writer_notes_split_v1", 1_u64)
            .map_err(|e| e.to_string())?;
        doc.put(ROOT, "desktop_counter_v1", split.next_counter)
            .map_err(|e| e.to_string())?;
        doc.commit();
        let seed = SeedState {
            room_changes: doc
                .get_changes(&[])
                .iter()
                .map(|c| c.raw_bytes().to_vec())
                .collect(),
            documents: vec![],
        };
        let mut candidate = peer.clone();
        candidate.install_seed(seed, key, web_time::SystemTime::now())?;
        *peer = candidate;
        return Ok(true);
    }
    if peer
        .document("so.tilo.word-processor", "tab:0000000000000001")
        .is_none()
        || peer.document("so.tilo.chat", "").is_none()
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
    let writer_group = mint()?;
    let writer = mint()?;
    let notes_group = mint()?;
    let notes = mint()?;
    let chat_group = mint()?;
    let chat = mint()?;
    let definition = notes::definition();
    let durable_app_id = "so.tilo.notes";
    let checkout = qos_desktop::records::component_record(
        durable_app_id,
        notes,
        definition.record_kind.clone(),
    );
    let cache = PortableStorage::new("native-desktop-migration", peer.room())?;
    let records = RecordOwners::portable(cache)
        .open(durable_app_id, peer.room())
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
    doc.put(ROOT, "writer_notes_split_v1", 1_u64)
        .map_err(|e| e.to_string())?;
    RoomProjection::project_owned_contents(&mut doc, &strips, salt, None);
    RoomProjection::bind_record(
        &mut doc,
        strip,
        writer_group,
        writer,
        "tab:0000000000000001",
    )?;
    RoomProjection::bind_record(&mut doc, strip, notes_group, notes, checkout.id.as_str())?;
    RoomProjection::bind_record(&mut doc, strip, chat_group, chat, "")?;
    doc.commit();
    let seed = SeedState {
        room_changes: doc
            .get_changes(&[])
            .iter()
            .map(|c| c.raw_bytes().to_vec())
            .collect(),
        documents: vec![SeedDocument {
            app_id: durable_app_id.into(),
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
