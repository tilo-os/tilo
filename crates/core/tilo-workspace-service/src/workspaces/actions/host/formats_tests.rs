//! A person's region and formats and their zone, kept from their page by a server of theirs in
//! their own workspace: theirs alone, and the zone their views are rendered in is the one they
//! chose, else their browser's.

use serde_json::{Value, json};
use tilo_actions::remote::formats;
use tilo_actions::remote::{ActionCallError, Caller};
use tilo_server_protocol::WorkspaceId;
use tilo_user_identity::DeviceCert;

use super::super::ServiceWorkspaceActions;
use super::super::tests::{admit, caller, hosted};
use super::vault_tests::ask_at;
use crate::workspaces::region::{REGION, Region};
use crate::workspaces::tests::{hosting_device, test_host};

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
        formats::target(action),
        argument,
    )
    .await
}

/// Whether a folder named `name` is anywhere under `dir`.
fn holds_folder(dir: &std::path::Path, name: &str) -> bool {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .any(|entry| {
            let path = entry.path();
            path.is_dir() && (entry.file_name() == name || holds_folder(&path, name))
        })
}

#[tokio::test]
async fn a_persons_formats_are_a_resource_for_them_alone() {
    let dir = tempfile::tempdir().unwrap();
    let (key, ada) = hosting_device();
    let server = test_host(dir.path(), &key, &ada).await;
    let workspaces = server.workspaces();
    // The device that made the root founds the person's own workspace, where their region is.
    crate::workspaces::person_workspace::found(workspaces)
        .await
        .expect("the person's own workspace");
    let (workspace, workspace_id, service) = hosted(workspaces).await;
    let bo = admit(workspaces, &workspace, &ada, 2).await;
    let (as_ada, as_bo) = (caller(&ada), caller(&bo));

    let nothing = ask(
        &service,
        workspace_id,
        (&ada, &as_ada),
        formats::READ,
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
        formats::SET,
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
        formats::READ,
        json!({}),
    )
    .await
    .unwrap();
    assert_eq!(read, kept, "what she reads back is what was kept");

    // It is a typed resource of her own workspace, with a history and an author, and no file of
    // the server's.
    let own = tilo_server_protocol::servers::person_workspace_id(&ada.user_public_key);
    let region = crate::workspaces::person_workspace::read_on::<Region>(workspaces, &own, REGION)
        .await
        .expect("the resource reads");
    assert_eq!(region.time_format, "H24");
    assert!(
        !holds_folder(dir.path(), "people"),
        "the server keeps no store of people beside its workspaces"
    );

    // Bo, another member of the workspace they share, reads none of it and keeps nothing here:
    // this server is not his.
    for (action, argument) in [
        (formats::READ, json!({})),
        (formats::SET, json!({"formats": {"time_format": "H12"}})),
    ] {
        let refused = ask(&service, workspace_id, (&bo, &as_bo), action, argument).await;
        assert!(
            matches!(&refused, Err(ActionCallError::Refused { .. })),
            "{action}: {refused:?}"
        );
    }
    assert_eq!(workspaces.region(&bo.user_public_key).await, None);
    let (formats, _) = workspaces
        .region(&ada.user_public_key)
        .await
        .expect("Ada's are kept");
    assert_eq!(formats.time, tilo_formats::TimeStyle::H24);
    assert_eq!(formats.date, tilo_formats::DateStyle::Eu);

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
            formats::SET,
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
        formats::READ,
        json!({}),
    )
    .await;
    assert!(
        matches!(refused, Err(ActionCallError::UnknownAction)),
        "{refused:?}"
    );
    let (formats, _) = workspaces.region(&ada.user_public_key).await.unwrap();
    assert_eq!(formats.time, tilo_formats::TimeStyle::H24, "still hers");
}

#[tokio::test]
async fn the_browsers_zone_is_taken_until_the_person_chooses_one() {
    let dir = tempfile::tempdir().unwrap();
    let (key, ada) = hosting_device();
    let server = test_host(dir.path(), &key, &ada).await;
    let workspaces = server.workspaces();
    // The device that made the root founds the person's own workspace, where their region is.
    crate::workspaces::person_workspace::found(workspaces)
        .await
        .expect("the person's own workspace");
    let (_, workspace_id, service) = hosted(workspaces).await;
    let as_ada = caller(&ada);
    let zone = || async {
        workspaces
            .region(&ada.user_public_key)
            .await
            .and_then(|(_, zone)| zone)
    };
    let tell = |zone: &'static str| {
        ask(
            &service,
            workspace_id,
            (&ada, &as_ada),
            formats::BROWSER_ZONE,
            json!({"zone": zone}),
        )
    };
    let choose = |zone: Value| {
        ask(
            &service,
            workspace_id,
            (&ada, &as_ada),
            formats::CHOOSE_ZONE,
            json!({"zone": zone}),
        )
    };

    assert_eq!(zone().await, None, "no zone before her page says");
    tell("Pacific/Auckland")
        .await
        .expect("her page says where its browser is");
    assert_eq!(zone().await.as_deref(), Some("Pacific/Auckland"));

    // A zone she chose wins, whatever her browser says after.
    let chosen = choose(json!("Europe/Vienna"))
        .await
        .expect("she chooses a zone");
    assert_eq!(chosen["chosenZone"], "Europe/Vienna");
    assert_eq!(chosen["browserZone"], "Pacific/Auckland");
    tell("America/New_York").await.unwrap();
    assert_eq!(zone().await.as_deref(), Some("Europe/Vienna"));

    // Given back, the browser's is taken again.
    choose(Value::Null)
        .await
        .expect("she follows her browser again");
    assert_eq!(zone().await.as_deref(), Some("America/New_York"));

    // Only a zone's name is kept as a zone.
    for not_a_zone in ["../../etc/passwd", "", "Somewhere far"] {
        let refused = tell(not_a_zone).await;
        assert!(
            matches!(refused, Err(ActionCallError::Refused { .. })),
            "{not_a_zone:?}: {refused:?}"
        );
    }
    assert_eq!(zone().await.as_deref(), Some("America/New_York"));
}
