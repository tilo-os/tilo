/// What every published action carries besides the fields a test is about:
/// its operation kind, parameters and contract.
fn published_operation(mut row: collab_capnp::exported_action::Builder<'_>) {
    row.set_operation_kind("view");
    row.set_params_json("[]");
    row.set_contract_json(serde_json::to_string(&tilo_actions::ActionContract::default()).unwrap());
}

#[cfg(feature = "client")]
#[test]
fn action_semantic_identity_is_optional_bounded_and_kind_exact() {
    let decode = |identity: Option<(&str, &str)>| {
        let mut message = capnp::message::Builder::new_default();
        let mut row = message.init_root::<collab_capnp::exported_action::Builder<'_>>();
        row.set_resource("opaque-alias");
        row.set_resource_kind("store.app");
        row.set_resource_label("A misleading label");
        published_operation(row.reborrow());
        if let Some((kind, key)) = identity {
            let mut out = row.reborrow().init_semantic_identity();
            out.set_kind(kind);
            out.set_key(key);
        }
        decode_exported_action(
            message
                .get_root_as_reader::<collab_capnp::exported_action::Reader<'_>>()
                .unwrap(),
        )
    };
    assert!(decode(None).unwrap().semantic_identity.is_none());
    let decoded = decode(Some(("store.app", "app:so.tilo.chat"))).unwrap();
    assert_eq!(decoded.resource, "opaque-alias");
    assert_eq!(decoded.semantic_identity.unwrap().key, "app:so.tilo.chat");
    assert!(decode(Some(("person", "app:so.tilo.chat"))).is_err());
    assert!(decode(Some(("store.app", ""))).is_err());
    assert!(decode(Some(("store.app", "key\nsecret"))).is_err());
    assert!(decode(Some(("store.app", &"x".repeat(4097)))).is_err());
    assert!(decode(Some((&"x".repeat(129), "key"))).is_err());
    assert!(decode(Some(("store.app", &"x".repeat(4096)))).is_ok());
}

#[test]
fn airdrop_extension_preserves_old_json_and_accepts_no_application_trust_claim() {
    use super::{NearbyTransferCommand as Command, NearbyTransferReply as Reply};
    assert_eq!(
        serde_json::to_string(&Command::Snapshot).unwrap(),
        r#"{"command":"snapshot"}"#
    );
    assert_eq!(
        serde_json::to_string(&Reply::Accepted).unwrap(),
        r#"{"result":"accepted"}"#
    );
    let command = r#"{"command":"send-air-drop","source_path":[47,116,109,112,47,102],"address":"127.0.0.1","port":443}"#;
    let parsed = serde_json::from_str::<Command>(command).unwrap();
    assert!(parsed.valid());
    for claimed in ["certificate_sha256", "verified", "person", "remember"] {
        let value = command.strip_suffix('}').unwrap().to_owned()
            + &format!(",\"{claimed}\":\"claimed\"}}");
        assert!(serde_json::from_str::<Command>(&value).is_err());
    }
    for address in ["receiver.example", "0.0.0.0", "239.1.1.1", "::"] {
        let mut value = serde_json::from_str::<serde_json::Value>(command).unwrap();
        value["address"] = address.into();
        assert!(!serde_json::from_value::<Command>(value).unwrap().valid());
    }
    let mut value = serde_json::from_str::<serde_json::Value>(command).unwrap();
    value["port"] = 0.into();
    assert!(!serde_json::from_value::<Command>(value).unwrap().valid());
}
use super::*;
use std::env;

fn clear_tilo_env() -> tilo_test_env::ScopedEnv {
    let mut scoped = tilo_test_env::ScopedEnv::new();
    for (key, _) in env::vars() {
        if key.starts_with("TILO_") {
            scoped.remove(&key);
        }
    }
    scoped
}

#[test]
fn try_connect_without_env_returns_service_fd_missing() {
    let _env = clear_tilo_env();
    // Not using `expect_err` because `CollabClient` intentionally
    // doesn't implement `Debug` (the underlying capnp client hook
    // isn't `Debug`).
    match try_connect() {
        Err(NotAvailable::ServiceFdMissing) => {}
        Err(other) => panic!("unexpected error: {other:?}"),
        Ok(_) => panic!("expected ServiceFdMissing"),
    }
}

#[test]
fn nearby_transfer_contract_is_bounded_and_rejects_impossible_progress() {
    // phase: nearby_transfer_contract_is_bounded_and_path_bytes_are_opaque
    {
        let command = NearbyTransferCommand::Send {
            target: NearbyTargetHandle::from_issued_bytes([7; 16]),
            source_path: vec![b'/', b't', b'm', b'p', b'/', 0xff],
        };
        assert!(command.valid());
        let encoded = serde_json::to_vec(&command).expect("encode nearby command");
        assert!(encoded.len() < MAX_NEARBY_TRANSFER_WIRE_BYTES);
        assert_eq!(
            serde_json::from_slice::<NearbyTransferCommand>(&encoded).expect("decode command"),
            command
        );

        assert!(
            !NearbyTransferCommand::Send {
                target: NearbyTargetHandle::from_issued_bytes([7; 16]),
                source_path: vec![b'a', 0, b'b'],
            }
            .valid()
        );
        assert!(
            NearbyTransferCommand::Decide {
                handle: NearbyTransferHandle::from_issued_bytes([8; 16]),
                accept: true,
                destination: Some(NearbyDestinationHandle::from_issued_bytes([9; 16])),
                remember_for_session: true,
            }
            .valid()
        );
        assert!(
            !NearbyTransferCommand::Decide {
                handle: NearbyTransferHandle::from_issued_bytes([8; 16]),
                accept: true,
                destination: None,
                remember_for_session: false,
            }
            .valid()
        );
        assert!(
            !NearbyTransferCommand::Send {
                target: NearbyTargetHandle::from_issued_bytes([7; 16]),
                source_path: vec![b'a'; MAX_NEARBY_TRANSFER_PATH_BYTES + 1],
            }
            .valid()
        );
    }
    // phase: nearby_transfer_snapshot_rejects_unbounded_or_impossible_progress
    {
        let target = NearbyTransferTarget {
            handle: NearbyTargetHandle::from_issued_bytes([3; 16]),
            label: "Travel laptop".into(),
            online: true,
        };
        let transfer = NearbyTransferStatus {
            handle: NearbyTransferHandle::from_issued_bytes([4; 16]),
            name: "Photos".into(),
            device_label: target.label.clone(),
            direction: NearbyTransferDirection::Sending,
            phase: NearbyTransferPhase::Transferring,
            transferred_bytes: 20,
            total_bytes: 100,
        };
        let reply = NearbyTransferReply::Snapshot {
            snapshot: NearbyTransferSnapshot {
                targets: vec![target],
                destinations: vec![NearbyTransferDestination {
                    handle: NearbyDestinationHandle::from_issued_bytes([5; 16]),
                    label: "Workspace files".into(),
                    remembered_for_session: false,
                }],
                transfers: vec![transfer.clone()],
            },
        };
        assert!(reply.valid());

        let mut impossible = transfer;
        impossible.transferred_bytes = 101;
        assert!(
            !NearbyTransferReply::Snapshot {
                snapshot: NearbyTransferSnapshot {
                    targets: Vec::new(),
                    destinations: Vec::new(),
                    transfers: vec![impossible],
                },
            }
            .valid()
        );
    }
}

#[test]
fn wifi_continuity_is_bounded_redacted_and_bound_to_a_local_profile() {
    // phase: wifi_continuity_contract_is_bounded_and_redacts_secrets
    {
        let command = WifiContinuityCommand::Share {
            ssid: "Studio".into(),
            security: PortableWifiSecurity::Wpa3,
            password: "correct horse battery staple".into(),
        };
        assert!(command.valid());
        assert!(!format!("{command:?}").contains("correct horse"));
        let encoded = serde_json::to_vec(&command).expect("encode Wi-Fi command");
        assert!(encoded.len() < MAX_WIFI_CONTINUITY_WIRE_BYTES);
        assert_eq!(
            serde_json::from_slice::<WifiContinuityCommand>(&encoded)
                .expect("decode Wi-Fi command"),
            command
        );

        assert!(
            !WifiContinuityCommand::Share {
                ssid: "Studio".into(),
                security: PortableWifiSecurity::Wpa2,
                password: "x".repeat(MAX_WIFI_PASSWORD_BYTES + 1),
            }
            .valid()
        );
        assert!(
            !WifiContinuityReply::Password {
                password: "x".repeat(MAX_WIFI_PASSWORD_BYTES + 1),
            }
            .valid()
        );
    }
    // phase: saved_wifi_profile_reference_requires_an_exact_local_uuid
    {
        assert!(
            WifiContinuityCommand::ShareSavedProfile {
                profile_uuid: "12345678-1234-1234-1234-123456789abc".into(),
            }
            .valid()
        );
        for invalid in [
            "profile-name",
            "../../secret",
            "12345678-1234-1234-1234-123456789abz",
        ] {
            assert!(
                !WifiContinuityCommand::ShareSavedProfile {
                    profile_uuid: invalid.into(),
                }
                .valid()
            );
        }
    }
}

#[test]
fn action_directory_and_view_metadata_round_trip() {
    // phase: action_directory_resource_presentation_round_trips
    {
        let mut message = capnp::message::Builder::new_default();
        {
            let mut page = message.init_root::<collab_capnp::action_directory_page::Builder<'_>>();
            page.set_revision("r1");
            page.set_exhausted(true);
            let mut rows = page.init_rows(1);
            let mut row = rows.reborrow().get(0);
            row.set_resource("totp:mail");
            row.set_resource_kind("totp");
            row.set_resource_label("Mail code");
            row.set_resource_presentation_json(r#"{"detail":"expires in 18 s","badge":"438 291"}"#);
        }
        let reader = message
            .get_root_as_reader::<collab_capnp::action_directory_page::Reader<'_>>()
            .expect("page reader");
        let decoded = decode_action_directory_page(reader).expect("page decodes");
        assert_eq!(
            decoded.rows[0].presentation,
            Some(tilo_actions::ResourcePresentation {
                detail: Some("expires in 18 s".into()),
                badge: Some("438 291".into()),
                preview: None,
            })
        );
    }
    // phase: exported_view_metadata_round_trips
    {
        let mut message = capnp::message::Builder::new_default();
        {
            let mut action = message.init_root::<collab_capnp::exported_action::Builder<'_>>();
            action.set_resource("notes:brief");
            action.set_action("show-editor");
            published_operation(action.reborrow());
            let mut view = action.init_view();
            view.set_id("editor");
            view.set_label("Editor");
            view.set_description("Edit this note");
            view.set_open_action("show-editor");
            view.set_primary(true);
            let mut range = view.reborrow().init_range();
            range.set_action("show-range");
            range.set_start_param("start");
            range.set_end_param("end");
            range.set_unit_param("unit");
            let mut mount = view.init_mount();
            mount.set_action("mount-editor");
            mount.set_capability_param("slot_capability");
            mount.set_view_param("slot_view");
        }
        let reader = message
            .get_root_as_reader::<collab_capnp::exported_action::Reader<'_>>()
            .expect("action reader");
        let decoded = decode_exported_action(reader).expect("action decodes");
        assert_eq!(
            decoded.view,
            Some(ExportedView {
                id: "editor".into(),
                label: "Editor".into(),
                description: Some("Edit this note".into()),
                open_action: "show-editor".into(),
                primary: true,
                range: Some(ExportedViewRange {
                    action: "show-range".into(),
                    start_param: "start".into(),
                    end_param: "end".into(),
                    unit_param: "unit".into()
                }),
                mount: Some(ExportedViewMount {
                    action: "mount-editor".into(),
                    capability_param: "slot_capability".into(),
                    view_param: "slot_view".into(),
                }),
            })
        );
    }
}

#[test]
fn a_rejection_relayed_through_several_hops_is_still_a_rejection() {
    let marked = format!("{LOCAL_DOCUMENT_REJECTED}{LOCAL_DOCUMENT_CONNECTION_REPORTS_NOTHING}");
    for hops in 0..3 {
        let relayed = format!("{}{marked}", "remote exception: ".repeat(hops));
        assert_eq!(
            local_document_rejection(&relayed),
            Some(LOCAL_DOCUMENT_CONNECTION_REPORTS_NOTHING),
            "{hops} hops"
        );
    }
    assert_eq!(
        local_document_rejection("remote exception: admission journal outcome unknown"),
        None,
        "an unmarked failure stays uncertain"
    );
}
