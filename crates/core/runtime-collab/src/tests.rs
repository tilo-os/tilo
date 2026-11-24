/// An action published with `identity` as its semantic identity, as the
/// wire carries it.
fn published_with_identity(identity: Option<(&str, &str)>) -> Result<ExportedAction, String> {
    let mut action = serde_json::json!({
        "resource": "opaque-alias",
        "action": "open",
        "resource_kind": "store.app",
        "resource_label": "A misleading label",
        "action_label": "Open",
        "operation_kind": "View",
        "params": [],
        "contract": tilo_actions::ActionContract::default(),
        "input": ExportedInput::None,
    });
    if let Some((kind, key)) = identity {
        action["semantic_identity"] = serde_json::json!({"kind": kind, "key": key});
    }
    let action: ExportedAction =
        serde_json::from_value(action).map_err(|error| error.to_string())?;
    action.check()?;
    Ok(action)
}

tilo_test_env::scenario! {
    fn action_directory_and_view_metadata_round_trip;

    phase action_semantic_identity_is_optional_bounded_and_kind_exact;
    {
        let decode = published_with_identity;
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

    phase action_directory_resource_presentation_round_trips;
    {
        let page = serde_json::json!({
            "revision": "r1",
            "exhausted": true,
            "next_cursor": [],
            "stale_cursor": false,
            "budget_exhausted": false,
            "rows": [{
                "parent_resource": "",
                "resource": "totp:mail",
                "resource_kind": "totp",
                "resource_label": "Mail code",
                "presentation": {"detail": "expires in 18 s", "badge": "438 291"},
                "action": null,
                "descriptor": null,
                "has_children": false,
                "visibility": null,
            }],
        });
        let decoded: ActionDirectoryPage =
            read_directory_answer(&page.to_string(), 1 << 20, |page: &ActionDirectoryPage| {
                page.rows.iter().collect()
            })
            .expect("page decodes");
        assert_eq!(
            decoded.rows[0].presentation,
            Some(tilo_actions::ResourcePresentation {
                detail: Some("expires in 18 s".into()),
                badge: Some("438 291".into()),
                preview: None,
            })
        );
    }

    phase exported_view_metadata_round_trips;
    {
        let mut action = published_with_identity(None).unwrap();
        action.view = Some(ExportedView {
            id: "editor".into(),
            label: "Editor".into(),
            description: Some("Edit this note".into()),
            open_action: "show-editor".into(),
            primary: true,
            desktop: false,
            range: Some(ExportedViewRange {
                action: "show-range".into(),
                start_param: "start".into(),
                end_param: "end".into(),
                unit_param: "unit".into(),
            }),
            mount: Some(ExportedViewMount {
                action: "mount-editor".into(),
                capability_param: "slot_capability".into(),
                view_param: "slot_view".into(),
            }),
        });
        let json = serde_json::to_string(&action).unwrap();
        let decoded: ExportedAction = serde_json::from_str(&json).expect("action decodes");
        assert_eq!(decoded, action);
    }
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
    // `CollabClient` is not `Debug`, so no `expect_err`.
    match try_connect() {
        Err(NotAvailable::ServiceFdMissing) => {}
        Err(other) => panic!("unexpected error: {other:?}"),
        Ok(_) => panic!("expected ServiceFdMissing"),
    }
}

tilo_test_env::scenario! {
    fn wifi_continuity_is_bounded_redacted_and_bound_to_a_local_profile;

    phase wifi_continuity_contract_is_bounded_and_redacts_secrets;
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

    phase saved_wifi_profile_reference_requires_an_exact_local_uuid;
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
fn a_marked_rejection_is_a_rejection() {
    let marked = format!("{LOCAL_DOCUMENT_REJECTED}{LOCAL_DOCUMENT_CONNECTION_REPORTS_NOTHING}");
    assert_eq!(
        local_document_rejection(&marked),
        Some(LOCAL_DOCUMENT_CONNECTION_REPORTS_NOTHING),
    );
    assert_eq!(
        local_document_rejection("admission journal outcome unknown"),
        None,
        "an unmarked failure stays uncertain"
    );
}
