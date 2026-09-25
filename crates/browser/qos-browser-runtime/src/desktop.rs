//! Admitted canonical desktop requirements and retained portable host.
use automerge::{ROOT, ReadDoc};
use qos_desktop::{RoomId, WorkspaceTree, projection::RoomProjection};
use qos_peer_state::StatePeer;
use std::collections::BTreeMap;
use ui::application::ApplicationDefinition;
use ui::record::{RecordCheckout, RecordId};

pub(crate) fn installed_applications() -> [(&'static str, &'static ApplicationDefinition); 4] {
    [
        word_processor::definition(),
        chat::definition(),
        notes::definition(),
        calculator::definition(),
    ]
    .map(|definition| (definition.app_id, definition))
}
pub(crate) fn definitions() -> [&'static ApplicationDefinition; 4] {
    installed_applications().map(|(_, definition)| definition)
}
/// Resolve a persisted app id to its installed definition.
///
/// The returned durable id is the persisted spelling itself: tabs, record
/// routes and admitted rows written before the `so.tilo.*` rename keep their
/// original id, and only the definition lookup goes through the shared
/// first-party rename table.
pub(crate) fn installed_application(
    app: &str,
) -> Option<(&'static str, &'static ApplicationDefinition)> {
    installed_applications()
        .into_iter()
        .find_map(|(current, definition)| {
            std::iter::once(current)
                .chain(ui::application::first_party_app_id_lookup_candidates(
                    current,
                ))
                .find(|candidate| *candidate == app)
                .map(|durable| (durable, definition))
        })
}
/// Whether a persisted app id names the Chat package, whose room record is
/// its package-primary route.
pub(crate) fn is_chat(app: &str) -> bool {
    ui::application::lookup_compatible_first_party_app_ids(app, chat::definition().app_id)
}
#[cfg(any(target_arch = "wasm32", test))]
pub(crate) fn definition(app: &str) -> Option<&'static ApplicationDefinition> {
    installed_application(app).map(|(_, definition)| definition)
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Requirement {
    pub app_id: String,
    pub record_id: String,
}
pub(crate) struct Admission {
    #[cfg(any(target_arch = "wasm32", test))]
    pub admitted_room: automerge::AutoCommit,
    #[cfg(any(target_arch = "wasm32", test))]
    pub source_heads: Vec<[u8; 32]>,
    #[cfg(any(target_arch = "wasm32", test))]
    pub tree: WorkspaceTree,
    #[cfg(any(target_arch = "wasm32", test))]
    pub routes: BTreeMap<u64, String>,
    #[cfg(any(target_arch = "wasm32", test))]
    pub required: Vec<Requirement>,
    pub missing: Vec<Requirement>,
    #[cfg(any(target_arch = "wasm32", test))]
    pub unsupported: Vec<String>,
    pub(crate) pending: usize,
}
impl Admission {
    pub fn complete(&self) -> bool {
        self.missing.is_empty() && self.pending == 0
    }
}
pub(crate) fn checkout(
    definition: &'static ApplicationDefinition,
    tab: u64,
    routes: &BTreeMap<u64, String>,
) -> Result<RecordCheckout, String> {
    let (durable_app_id, _) = installed_application(definition.app_id)
        .ok_or("application is not in the installed durable catalog")?;
    let chat = is_chat(durable_app_id);
    match routes.get(&tab) {
        None => Ok(qos_desktop::records::component_record(
            durable_app_id,
            tab,
            definition.record_kind.clone(),
        )),
        Some(record) if record.is_empty() && chat => Ok(RecordCheckout::package_primary(
            definition.record_kind.clone(),
        )),
        Some(record) if !chat => Ok(RecordCheckout::new(
            RecordId::parse(record).ok_or("invalid canonical ordinary record route")?,
            definition.record_kind.clone(),
        )),
        _ => Err("invalid canonical package-primary record route".into()),
    }
}
pub(crate) fn admission(state: &StatePeer) -> Result<Admission, String> {
    let doc = state
        .admitted_room()
        .ok_or("waiting for admitted room governance")?;
    if doc
        .get(ROOT, "contents")
        .map_err(|e| e.to_string())?
        .is_none()
    {
        return Err("waiting for canonical desktop topology".into());
    }
    let mut tree = WorkspaceTree::default();
    RoomProjection::apply_admitted_room(
        &mut tree,
        RoomId::parse(state.room()).ok_or("invalid room identity")?,
        doc,
    )?;
    let routes = RoomProjection::record_routes(doc);
    let mut required = Vec::new();
    let mut missing = Vec::new();
    let mut unsupported = Vec::new();
    let rows = state.rows();
    for workspace in &tree.workspaces {
        for strip in &workspace.strips {
            for group in &strip.window_groups {
                for tab in &group.tabs {
                    let Some(app) = qos_desktop::resolver::canonical_app_id(&tab.tile) else {
                        continue;
                    };
                    let Some((durable_app_id, definition)) = installed_application(&app) else {
                        if !unsupported.contains(&app) {
                            unsupported.push(app);
                        }
                        continue;
                    };
                    let checkout = checkout(definition, tab.id, &routes)?;
                    let record = if checkout.id.is_package_primary() {
                        String::new()
                    } else {
                        checkout.id.as_str().into()
                    };
                    let requirement = Requirement {
                        app_id: durable_app_id.into(),
                        record_id: record,
                    };
                    if !required.contains(&requirement) {
                        if !rows.iter().any(|row| {
                            row.app_id == requirement.app_id
                                && row.record_id == requirement.record_id
                                && !row.heads.is_empty()
                        }) {
                            missing.push(requirement.clone());
                        }
                        required.push(requirement);
                    }
                }
            }
        }
    }
    Ok(Admission {
        #[cfg(any(target_arch = "wasm32", test))]
        admitted_room: doc.clone(),
        #[cfg(any(target_arch = "wasm32", test))]
        source_heads: doc
            .clone()
            .get_heads()
            .into_iter()
            .map(|head| head.0)
            .collect(),
        #[cfg(any(target_arch = "wasm32", test))]
        tree,
        #[cfg(any(target_arch = "wasm32", test))]
        routes,
        #[cfg(any(target_arch = "wasm32", test))]
        required,
        missing,
        #[cfg(any(target_arch = "wasm32", test))]
        unsupported,
        pending: state.pending_count(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::{AutoCommit, ObjType, ROOT, transaction::Transactable};
    use ed25519_dalek::SigningKey;
    use qos_desktop::{projection::RoomProjection, *};
    use qos_peer_state::{SeedState, StatePeer};
    fn state() -> (StatePeer, SigningKey) {
        use tilo_user_identity::{
            DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert,
        };
        let root = SigningKey::from_bytes(&[61; 32]);
        let key = SigningKey::from_bytes(&[62; 32]);
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: "owner".into(),
            capabilities: DeviceCapabilities::primary(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, &root).unwrap();
        let room = "ffffffff-ffff-4fff-8fff-ffffffffffff";
        let seed = crate::fixture_seed(room, &cert, &[cert.clone()]).unwrap();
        let mut peer = StatePeer::new(
            room,
            cert.clone(),
            vec![cert],
            RevocationList::new(),
            web_time::SystemTime::now(),
        )
        .unwrap();
        peer.install_seed(seed, &key, web_time::SystemTime::now())
            .unwrap();
        (peer, key)
    }
    fn install(peer: &mut StatePeer, key: &SigningKey, doc: &mut AutoCommit) {
        doc.commit();
        peer.install_seed(
            SeedState {
                room_changes: doc
                    .get_changes(&[])
                    .iter()
                    .map(|c| c.raw_bytes().to_vec())
                    .collect(),
                documents: vec![],
            },
            key,
            web_time::SystemTime::now(),
        )
        .unwrap();
    }
    #[test]
    fn required_records_follow_admitted_topology_including_missing_notes_and_removal() {
        let (mut peer, key) = state();
        let mut doc = peer.admitted_room().unwrap().clone();
        let strips = vec![Strip {
            id: StripId((5 << 48) | 1),
            name: "Documents".into(),
            window_groups: vec![WindowGroup {
                id: (5 << 48) | 2,
                active_tab: (5 << 48) | 3,
                width: WindowWidth::Normal,
                maximized: false,
                tabs: vec![
                    Tab {
                        id: (5 << 48) | 3,
                        title: "Renamed Writer".into(),
                        tile: TileContent::AppPlaceholder {
                            kind: AppKind::Other("so.tilo.word-processor".into()),
                            label: "Anything".into(),
                        },
                    },
                    Tab {
                        id: (5 << 48) | 4,
                        title: "Notes".into(),
                        tile: TileContent::AppPlaceholder {
                            kind: AppKind::Other("so.tilo.notes".into()),
                            label: "Notes".into(),
                        },
                    },
                ],
            }],
        }];
        RoomProjection::project_owned_contents(&mut doc, &strips, 5, None);
        RoomProjection::bind_record(
            &mut doc,
            (5 << 48) | 1,
            (5 << 48) | 2,
            (5 << 48) | 3,
            "tab:0000000000000001",
        )
        .unwrap();
        install(&mut peer, &key, &mut doc);
        let admission = super::admission(&peer).unwrap();
        assert_eq!(admission.required.len(), 2);
        assert_eq!(admission.missing.len(), 1);
        assert_eq!(admission.missing[0].app_id, "so.tilo.notes");
        assert!(!admission.complete());
        let mut strips = strips;
        strips[0].window_groups[0].tabs.pop();
        RoomProjection::project_owned_contents(&mut doc, &strips, 5, None);
        install(&mut peer, &key, &mut doc);
        let admission = super::admission(&peer).unwrap();
        assert!(admission.complete());
        assert_eq!(admission.required.len(), 1);
        assert_eq!(admission.required[0].record_id, "tab:0000000000000001");
    }
    #[test]
    fn pre_rename_workspace_keeps_its_persisted_ids_and_stays_admitted() {
        use tilo_user_identity::{
            DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert,
        };
        let root = SigningKey::from_bytes(&[63; 32]);
        let key = SigningKey::from_bytes(&[64; 32]);
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: "legacy owner".into(),
            capabilities: DeviceCapabilities::primary(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, &root).unwrap();
        let room = "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee";
        // Rows written before the rename keep their retired `dev.qos.*` ids.
        let mut seed = crate::fixture_seed(room, &cert, &[cert.clone()]).unwrap();
        for document in &mut seed.documents {
            document.app_id = document.app_id.replacen("so.tilo.", "dev.qos.", 1);
        }
        let mut peer = StatePeer::new(
            room,
            cert.clone(),
            vec![cert],
            RevocationList::new(),
            web_time::SystemTime::now(),
        )
        .unwrap();
        peer.install_seed(seed, &key, web_time::SystemTime::now())
            .unwrap();
        let mut doc = peer.admitted_room().unwrap().clone();
        let tab = |id, app: &str| Tab {
            id,
            title: app.into(),
            tile: TileContent::AppPlaceholder {
                kind: AppKind::Other(app.into()),
                label: app.into(),
            },
        };
        let strips = vec![Strip {
            id: StripId((6 << 48) | 1),
            name: "Legacy".into(),
            window_groups: vec![WindowGroup {
                id: (6 << 48) | 2,
                active_tab: (6 << 48) | 3,
                width: WindowWidth::Normal,
                maximized: false,
                tabs: vec![
                    tab((6 << 48) | 3, "dev.qos.word-processor"),
                    tab((6 << 48) | 4, "dev.qos.chat"),
                ],
            }],
        }];
        RoomProjection::project_owned_contents(&mut doc, &strips, 6, None);
        RoomProjection::bind_record(
            &mut doc,
            (6 << 48) | 1,
            (6 << 48) | 2,
            (6 << 48) | 3,
            "tab:0000000000000001",
        )
        .unwrap();
        RoomProjection::bind_record(&mut doc, (6 << 48) | 1, (6 << 48) | 2, (6 << 48) | 4, "")
            .unwrap();
        install(&mut peer, &key, &mut doc);
        let admission = super::admission(&peer).unwrap();
        assert!(admission.complete(), "{:?}", admission.missing);
        let mut required = admission
            .required
            .iter()
            .map(|row| (row.app_id.as_str(), row.record_id.as_str()))
            .collect::<Vec<_>>();
        required.sort_unstable();
        assert_eq!(
            required,
            [
                ("dev.qos.chat", ""),
                ("dev.qos.word-processor", "tab:0000000000000001")
            ]
        );
        assert_eq!(
            super::installed_application("dev.qos.chat").map(|(id, _)| id),
            Some("dev.qos.chat")
        );
        assert!(super::installed_application("dev.qos.desktop").is_none());
    }
    #[test]
    fn empty_admitted_desktop_is_ready_but_unprojected_room_is_waiting() {
        let (mut peer, key) = state();
        assert!(admission(&peer).is_err());
        let mut doc = peer.admitted_room().unwrap().clone();
        doc.put_object(ROOT, "contents", ObjType::Map).unwrap();
        install(&mut peer, &key, &mut doc);
        let admission = super::admission(&peer).unwrap();
        assert!(admission.complete());
        assert!(admission.required.is_empty());
        assert_eq!(
            admission.admitted_room.clone().get_heads(),
            peer.admitted_room().unwrap().clone().get_heads()
        );
        assert_eq!(
            admission.source_heads,
            admission
                .admitted_room
                .clone()
                .get_heads()
                .iter()
                .map(|head| head.0)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn durable_chat_empty_route_uses_package_primary_checkout() {
        let tab = 17;
        let routes = BTreeMap::from([(tab, String::new())]);
        let checkout = super::checkout(chat::definition(), tab, &routes).unwrap();
        assert!(checkout.id.is_package_primary());
    }
}
