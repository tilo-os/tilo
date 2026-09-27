//! Admitted canonical desktop requirements and retained portable host.
use automerge::{ROOT, ReadDoc};
use std::collections::BTreeMap;
#[cfg(any(target_arch = "wasm32", test))]
use tilo_desktop_model::apps::{AppCatalogue, OpenableApp};
use tilo_desktop_model::{DesktopTree, RoomId};
use tilo_peer_state::StatePeer;
use ui::application::ApplicationDefinition;
use ui::record::{RecordCheckout, RecordId};

/// The installed definitions, in the catalogue's order.
#[cfg(any(target_arch = "wasm32", test))]
pub(crate) fn installed_applications() -> Vec<&'static ApplicationDefinition> {
    crate::catalog::installed()
}
/// The apps bundled into the browser runtime, as the desktop lists and
/// validates opens.
#[cfg(any(target_arch = "wasm32", test))]
pub(crate) struct Bundled;

#[cfg(any(target_arch = "wasm32", test))]
impl AppCatalogue for Bundled {
    fn openable(&self) -> Vec<OpenableApp> {
        installed_applications()
            .into_iter()
            .map(|definition| OpenableApp {
                app_id: definition.app_id,
                title: definition.title,
            })
            .collect()
    }
}
/// Resolve an app id to its installed definition.
pub(crate) fn definition(app: &str) -> Option<&'static ApplicationDefinition> {
    crate::catalog::resolve(app).ok()
}
/// Whether a persisted app id names a package whose room record is its
/// package-primary route (Chat's workspace).
pub(crate) fn opens_package_primary(app: &str) -> bool {
    crate::catalog::scope(app) == Some(crate::catalog::RecordScope::PackagePrimary)
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
    pub tree: DesktopTree,
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
    let app_id = definition.app_id;
    let primary = opens_package_primary(app_id);
    match routes.get(&tab) {
        None => Ok(tilo_desktop::records::component_record(
            app_id,
            tab,
            definition.record_kind.clone(),
        )),
        Some(record) if record.is_empty() && primary => Ok(RecordCheckout::package_primary(
            definition.record_kind.clone(),
        )),
        Some(record) if !primary => Ok(RecordCheckout::new(
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
    let mut tree = DesktopTree::default();
    tilo_desktop_model::projection::apply_admitted_room(
        &mut tree,
        RoomId::parse(state.room()).ok_or("invalid room identity")?,
        doc,
    )?;
    let routes = tilo_desktop_model::apps::record_routes(doc);
    let mut required = Vec::new();
    let mut missing = Vec::new();
    let mut unsupported = Vec::new();
    let rows = state.rows();
    for workspace in &tree.workspaces {
        for strip in &workspace.strips {
            for group in &strip.window_groups {
                for tab in &group.tabs {
                    let Some(app) = tilo_desktop_model::apps::canonical_app_id(&tab.tile) else {
                        continue;
                    };
                    let Some(definition) = definition(&app) else {
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
                        app_id: definition.app_id.into(),
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
    use tilo_desktop_model::*;
    use tilo_peer_state::{SeedState, StatePeer};
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
        tilo_desktop_model::projection::project_owned_contents(
            &mut doc,
            &strips,
            5,
            None,
            tilo_desktop_model::projection::EmptyStrips::Skip,
        )
        .unwrap();
        tilo_desktop_model::apps::bind_tab_record(
            &mut doc,
            tilo_desktop_model::StripId((5 << 48) | 1),
            (5 << 48) | 2,
            (5 << 48) | 3,
            "tab:0000000000000001",
        )
        .map_err(|e| e.to_string())
        .unwrap();
        install(&mut peer, &key, &mut doc);
        let admission = super::admission(&peer).unwrap();
        assert_eq!(admission.required.len(), 2);
        assert_eq!(admission.missing.len(), 1);
        assert_eq!(admission.missing[0].app_id, "so.tilo.notes");
        assert!(!admission.complete());
        let mut strips = strips;
        strips[0].window_groups[0].tabs.pop();
        tilo_desktop_model::projection::project_owned_contents(
            &mut doc,
            &strips,
            5,
            None,
            tilo_desktop_model::projection::EmptyStrips::Skip,
        )
        .unwrap();
        install(&mut peer, &key, &mut doc);
        let admission = super::admission(&peer).unwrap();
        assert!(admission.complete());
        assert_eq!(admission.required.len(), 1);
        assert_eq!(admission.required[0].record_id, "tab:0000000000000001");
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
