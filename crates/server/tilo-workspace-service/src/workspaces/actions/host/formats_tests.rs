//! A person's region and formats and their zone on their server: kept from their page, theirs
//! alone, and the zone their views are rendered in is the one they chose, else their browser's.

#![expect(
    clippy::significant_drop_tightening,
    reason = "the server's handle a test binds is kept until the test ends; dropping it earlier closes the server it drives"
)]

use serde_json::{Value, json};
use tilo_actions::remote::person_formats;
use tilo_actions::remote::{ActionCallError, Caller};
use tilo_server_protocol::WorkspaceId;
use tilo_user_identity::DeviceCert;

use super::super::ServiceWorkspaceActions;
use super::super::tests::{admit, caller, governed};
use super::vault_tests::ask_at;
use crate::workspaces::tests::{governor, test_host};

/// `action` of the person's formats, asked by `who` as their page asks it.
async fn ask(
    service: &ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    who: (&DeviceCert, &Caller),
    action: &str,
    argument: Value,
) -> Result<Value, ActionCallError> {
    ask_at(
        service,
        workspace_id,
        who,
        person_formats::target(action),
        argument,
    )
    .await
}

#[tokio::test]
async fn a_persons_formats_are_kept_for_them_alone() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, service) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let bo = admit(workspaces, &workspace, &owner, 2).await;
    let (as_ada, as_bo) = (caller(&ada), caller(&bo));

    let nothing = ask(
        &service,
        workspace_id,
        (&ada, &as_ada),
        person_formats::READ,
        json!({}),
    )
    .await
    .expect("Ada reads what is kept for her");
    assert_eq!(nothing, json!({}), "nothing is kept before her page says");

    // Her page sends the formats she chose in Region & Formats, as the settings section keeps
    // them; what is not a format is not kept.
    let kept = ask(
        &service,
        workspace_id,
        (&ada, &as_ada),
        person_formats::SET,
        json!({"formats": {"time_format": "H24", "date_format": "EU", "currency": "EUR"}}),
    )
    .await
    .expect("Ada keeps her formats");
    assert_eq!(kept["formats"]["time_format"], "H24");
    assert_eq!(kept["formats"]["date_format"], "EU");
    assert!(kept["formats"].get("currency").is_none(), "{kept}");
    let read = ask(
        &service,
        workspace_id,
        (&ada, &as_ada),
        person_formats::READ,
        json!({}),
    )
    .await
    .unwrap();
    assert_eq!(read, kept, "what she reads back is what was kept");

    // Bo reads and keeps his own, and Ada's stay as she left them.
    let his = ask(
        &service,
        workspace_id,
        (&bo, &as_bo),
        person_formats::READ,
        json!({}),
    )
    .await
    .unwrap();
    assert_eq!(his, json!({}), "Bo reads nothing of Ada's");
    ask(
        &service,
        workspace_id,
        (&bo, &as_bo),
        person_formats::SET,
        json!({"formats": {"time_format": "H12"}}),
    )
    .await
    .expect("Bo keeps his own");
    let (formats, _) = workspaces
        .person_region(&ada.user_public_key)
        .expect("Ada's are kept");
    assert_eq!(formats.time, tilo_formats::TimeStyle::H24);
    assert_eq!(formats.date, tilo_formats::DateStyle::Eu);
    let (formats, _) = workspaces.person_region(&bo.user_public_key).unwrap();
    assert_eq!(formats.time, tilo_formats::TimeStyle::H12);

    // No agent reaches them, whoever it acts for, and neither does Ada on Bo's device.
    let not_her = [
        Caller::delegated_agent("assistant", ada.user_public_key, ada.device_public_key),
        caller(&ada).delegated_to("assistant"),
    ];
    for asking in &not_her {
        let refused = ask(
            &service,
            workspace_id,
            (&ada, asking),
            person_formats::SET,
            json!({"formats": {"time_format": "H12"}}),
        )
        .await;
        assert!(
            matches!(refused, Err(ActionCallError::UnknownAction)),
            "{refused:?}"
        );
    }
    let refused = ask(
        &service,
        workspace_id,
        (&bo, &as_ada),
        person_formats::READ,
        json!({}),
    )
    .await;
    assert!(
        matches!(refused, Err(ActionCallError::UnknownAction)),
        "{refused:?}"
    );
    let (formats, _) = workspaces.person_region(&ada.user_public_key).unwrap();
    assert_eq!(formats.time, tilo_formats::TimeStyle::H24, "still hers");
}

#[tokio::test]
async fn the_browsers_zone_is_taken_until_the_person_chooses_one() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, service) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let as_ada = caller(&ada);
    let zone = || {
        workspaces
            .person_region(&ada.user_public_key)
            .and_then(|(_, zone)| zone)
    };
    let tell = |zone: &'static str| {
        ask(
            &service,
            workspace_id,
            (&ada, &as_ada),
            person_formats::BROWSER_ZONE,
            json!({"zone": zone}),
        )
    };
    let choose = |zone: Value| {
        ask(
            &service,
            workspace_id,
            (&ada, &as_ada),
            person_formats::CHOOSE_ZONE,
            json!({"zone": zone}),
        )
    };

    assert_eq!(zone(), None, "no zone before her page says");
    tell("Pacific/Auckland")
        .await
        .expect("her page says where its browser is");
    assert_eq!(zone().as_deref(), Some("Pacific/Auckland"));

    // A zone she chose wins, whatever her browser says after.
    let chosen = choose(json!("Europe/Vienna"))
        .await
        .expect("she chooses a zone");
    assert_eq!(chosen["chosenZone"], "Europe/Vienna");
    assert_eq!(chosen["browserZone"], "Pacific/Auckland");
    tell("America/New_York").await.unwrap();
    assert_eq!(zone().as_deref(), Some("Europe/Vienna"));

    // Given back, the browser's is taken again.
    choose(Value::Null)
        .await
        .expect("she follows her browser again");
    assert_eq!(zone().as_deref(), Some("America/New_York"));

    // Only a zone's name is kept as a zone.
    for not_a_zone in ["../../etc/passwd", "", "Somewhere far"] {
        let refused = tell(not_a_zone).await;
        assert!(
            matches!(refused, Err(ActionCallError::InvalidRequest { .. })),
            "{not_a_zone:?}: {refused:?}"
        );
    }
    assert_eq!(zone().as_deref(), Some("America/New_York"));
}
