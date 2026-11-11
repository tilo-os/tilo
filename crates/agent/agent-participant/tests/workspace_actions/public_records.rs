//! The host's action service in a public workspace — one listed in its host's
//! directory, which anyone may walk into.
//!
//! Such a workspace replays a device only its governance and the shared
//! desktop, and sends every other record once that device opens it, saying so
//! in the `Ready` that ends the replay. The service that answers a page's or an
//! agent's `tilo_read` and `tilo_do` joins the workspace as the host's own
//! device, so what a member wrote after the service's session joined reached it
//! not at all: a request naming that record was refused as one the workspace
//! has none of, and nothing could be read or done there.
//!
//! A member of the public workspace writes a note, and the host's service reads
//! it by id and acts on it: it lists the record's actions, the note the member
//! made named by the title they gave it, and makes another note there, whose
//! change the member's page then sees. A record the workspace holds none of is
//! still refused as absent, once the service heard the host say so.

use super::resource_support::{admit_person, open_admitted_peer};
use super::*;
use tilo_actions::remote::{CataloguePage, ReceiptStatus};
use tilo_peer_state::join::MemberRole;
use tilo_relay_protocol::directory::Listing;
use tilo_relay_protocol::workspaces::WorkspaceRole;
use tilo_user_identity::DeviceCapabilities;

const NOTES: &str = "so.tilo.notes";

/// The title the member's own note holds.
const TITLE: &str = "Kickoff";

/// The address `replica` publishes `action` under.
fn address_of(replica: &Replica, action: &str) -> ActionAddress {
    replica
        .survey()
        .into_iter()
        .find(|descriptor| descriptor.address.action == action)
        .unwrap_or_else(|| panic!("the record publishes {action}"))
        .address
}

/// The host's catalogue of `provider`, asked again while the service is still
/// joining the workspace: a host that has not joined yet says so, and the
/// caller asks again, as a page does.
async fn catalogue(page: &mut Browser, provider: &str) -> Result<CataloguePage, ActionHostError> {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let query = CatalogueQuery {
            provider: Some(provider.to_owned()),
            max_results: 256,
            ..CatalogueQuery::default()
        };
        match page.catalogue(query).await {
            Err(ActionHostError::Unavailable { reason }) => {
                assert!(
                    Instant::now() < deadline,
                    "the host never joined the workspace: {reason}"
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            other => return other,
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_hosts_action_service_reads_and_acts_on_a_public_workspaces_records() {
    // The workspace, public: anyone may walk in, and its host sends a device
    // its records when it opens them.
    let (owner, _) = tilo_user_identity::UserIdentity::generate("Owner", 1);
    let host = enrol(
        &owner,
        "lounge-host",
        tilo_user_identity::cloud_enrollment::host_capabilities(),
    );
    let state = support::scratch_dir();
    let hosted = start(&owner, &host, state.path()).await;
    let workspaces = hosted.relay.workspaces().unwrap();
    let lounge = workspaces.create("Lounge").await.unwrap();
    workspaces
        .publish(
            &lounge.workspace,
            &host.cert,
            Listing {
                description: "Everyone".into(),
                default_role: WorkspaceRole::Editor,
            },
        )
        .await
        .unwrap();
    let workspace = lounge.workspace.clone();

    // A member of the workspace wrote a note there.
    let (member_root, _) = tilo_user_identity::UserIdentity::generate("Member", 2);
    let member_device = enrol(&member_root, "member-browser", DeviceCapabilities::member());
    admit_person(
        &hosted,
        &host,
        &workspace,
        &member_device,
        MemberRole::Editor,
    )
    .await;
    let mut member = open_admitted_peer(&hosted, &member_device, &host, &workspace).await;
    let mut note =
        common::open_in(&workspace, NOTES, "member", ReplicaActor::Owner, Seed::New).unwrap();
    let new_note = address_of(&note, "new-note");
    note.invoke(
        &new_note,
        serde_json::json!({ "title": TITLE }).as_object().unwrap(),
        false,
    )
    .unwrap();
    let route = DocumentRoute {
        app_id: NOTES.into(),
        cluster_id: workspace.clone(),
        record: note.wire_record().to_owned(),
    };
    support::written(&member.peer, &route, &note.take_mutation().unwrap()).await;
    let provider = record_provider(&route);

    // The host's own action service never had the note: its session was sent
    // the workspace's governance and desktop, and nothing else until asked.
    let page = catalogue(&mut member, &provider)
        .await
        .expect("the host serves the record the member wrote");
    // Read: the rows are what the note's own app publishes over the document
    // the member wrote, at the note they made, so the host holds their
    // document and not an empty one of its own.
    assert!(
        !page.rows.is_empty(),
        "the note publishes its actions to the member"
    );
    let named = page.rows.iter().any(|row| {
        [
            Some(&row.resource.kind),
            row.action.as_ref().map(|action| &action.resource_label),
        ]
        .into_iter()
        .flatten()
        .any(|label| label.contains(TITLE))
    });
    assert!(named, "the host lists the note the member wrote: {page:?}");

    // Act: the host makes a note of its own in the member's record, and the
    // member's page is sent the change as an ordinary durable one.
    let receipt = member
        .invoke(request(
            &provider,
            new_note.clone(),
            InvokeArgument::Json(
                serde_json::json!({ "title": "Second" })
                    .as_object()
                    .cloned()
                    .unwrap(),
            ),
        ))
        .await;
    assert!(
        matches!(receipt.status, ReceiptStatus::Completed { .. }),
        "the host made a note in the member's record: {receipt:?}"
    );
    let deadline = tilo_test_env::hang_deadline();
    loop {
        member.pump();
        let authored = member
            .documents
            .get(&route)
            .into_iter()
            .flatten()
            .any(|checkpoint| checkpoint.0 == host.cert.device_public_key);
        if authored || Instant::now() >= deadline {
            assert!(
                authored,
                "the host's change reached the member's page in their record"
            );
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // A record the workspace holds none of is still absent: the host asks,
    // is answered it holds none of it, and refuses rather than making one.
    let missing = DocumentRoute {
        app_id: NOTES.into(),
        cluster_id: workspace.clone(),
        record: ui::record::RecordId::minted().as_str().to_owned(),
    };
    let refused = catalogue(&mut member, &record_provider(&missing)).await;
    assert!(
        matches!(
            refused,
            Err(ActionHostError::UnknownAction | ActionHostError::NotPermitted)
        ),
        "a record the workspace has none of is refused: {refused:?}"
    );
    member.pump();
    assert!(
        !member.documents.contains_key(&missing),
        "the host founded a record the workspace has none of"
    );

    member.peer.close();
    hosted.agents.shutdown();
    drop(hosted.web);
    hosted.relay.shutdown().await;
}
