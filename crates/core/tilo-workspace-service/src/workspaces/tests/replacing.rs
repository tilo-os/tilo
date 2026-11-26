//! A record whose type names its readers, held in the document every member reads, is moved
//! into its rule's document when the workspace opens: its author reads it there, another member
//! reads nothing of it, and the record's history is its author's alone.

use super::*;
use collab_types::store::Resource;
use collab_types::store::wire::Command;

/// The app and type of the draft the tests misplace.
const APP: &str = "test.readers";
const DRAFT: &str = "tl.test.readers.Draft";

/// The compiled surface of the app's draft, whose type names its readers when `ruled`.
fn surface(ruled: bool) -> Vec<u8> {
    let mut resource = serde_json::json!({
        "kind": DRAFT,
        "label": "Draft",
        "version": 1,
        "fields": [
            {"path": "subject", "type": "text"},
            {"path": "sent", "type": "bool"},
        ],
        "verbs": [{"id": "type", "kind": "undoable"}],
    });
    if ruled {
        resource["readers"] = serde_json::json!({"author": {"until": "sent"}});
    }
    let file = serde_json::json!({"resources": [resource]});
    collab_types::surface::canonical(&file).into_bytes()
}

/// The person the device this service signs as belongs to: whose draft the tests write, as the
/// device may write only for its own person.
fn host_person(workspaces: &Workspaces) -> String {
    hex::encode(workspaces.signing_device().cert.user_public_key)
}

/// The draft "draft" of the app, subject "Quarterly plan", unsent, written where every member
/// reads it as the host's person, as a workspace whose app did not yet name its readers kept
/// one: written through the workspace's signed history, as a device's write of it.
async fn write_misplaced_draft(workspaces: &Workspaces, workspace: &str) -> Result<(), String> {
    use collab_types::store::{Cell, Value};
    use std::collections::BTreeMap;
    let state = workspaces
        .workspace(workspace)
        .await
        .map_err(|error| error.to_string())?;
    // Drafted and kept under one hold of the workspace, as every write of the device is: drafted
    // again when the keys it needs were waited for with the hold let go.
    let stamp = tilo_resource_store::Stamp {
        verb: "type".to_owned(),
        // This device's write, as its own person: a device writes for no other member.
        invoker: host_person(workspaces),
        ..tilo_resource_store::Stamp::default()
    };
    let command = Command::Put {
        kind: DRAFT.to_owned(),
        id: "draft".to_owned(),
        resource: Resource {
            readers: collab_types::EVERY_MEMBER.to_owned(),
            parent: None,
            order: None,
            fields: BTreeMap::from([
                (
                    "subject".to_owned(),
                    Cell::One(Value::from("Quarterly plan")),
                ),
                ("sent".to_owned(), Cell::One(Value::Bool(false))),
            ]),
        },
        stamp,
    };
    let command = &command;
    crate::workspaces::wanted_keys::write_with_keys(&state, move |mut hosted| async move {
        let writes = tilo_resource_store::wire::draft(
            &mut workspaces.store_of(&mut hosted),
            command.clone(),
        )
        .map_err(|refused| refused.error.to_string())?;
        workspaces
            .keep_writes(hosted, workspaces.signing_device(), APP, writes, None)
            .await
            .map(|_| ())
    })
    .await
}

/// Whether `who` is answered the draft "draft", through the one read rule every host answers by.
async fn draft_read_by(workspaces: &Workspaces, workspace: &str, who: &str) -> bool {
    let Ok(state) = workspaces.workspace(workspace).await else {
        return false;
    };
    let mut hosted = state.lock().await;
    let store = workspaces.store_of(&mut hosted);
    let get = Command::Get {
        kind: DRAFT.to_owned(),
        id: "draft".to_owned(),
    };
    matches!(
        tilo_resource_store::wire::read(&store, &get, &tilo_resource_store::wire::reads_as(who)),
        Some(collab_types::store::wire::Reply::Resource(Some(_)))
    )
}

#[tokio::test]
async fn a_misplaced_draft_is_moved_into_its_rules_document() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = hosting_device();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let created = workspaces.create("Mail").await.unwrap();
    let workspace = created.workspace.clone();

    // The draft is written before its type named its readers: every member reads its document.
    crate::workspaces::fixtures::declare_app(&server, APP, &surface(false));
    write_misplaced_draft(&workspaces, &workspace)
        .await
        .expect("the draft is written");

    // Its type names them now: the store reads it as the rule says, where it is held, and knows
    // it is held where the rule does not place it.
    crate::workspaces::fixtures::declare_app(&server, APP, &surface(true));
    {
        let state = workspaces.workspace(&workspace).await.unwrap();
        let mut hosted = state.lock().await;
        let store = workspaces.store_of(&mut hosted);
        assert!(
            store.misplaced(DRAFT, "draft"),
            "the draft is held where every member reads it, and its rule names one"
        );
        assert_eq!(
            store.readers_of(DRAFT, "draft"),
            Some(host_person(&workspaces)),
            "the rule names its author"
        );
    }

    // Opened, the workspace moves it into the document its rule gives.
    workspaces
        .replace_misplaced(&workspace)
        .await
        .expect("the misplaced draft is moved");
    {
        let state = workspaces.workspace(&workspace).await.unwrap();
        let mut hosted = state.lock().await;
        let store = workspaces.store_of(&mut hosted);
        assert!(
            !store.misplaced(DRAFT, "draft"),
            "the draft is moved into the document its rule gives"
        );
        assert_eq!(
            store.readers_of(DRAFT, "draft"),
            Some(host_person(&workspaces)),
            "and reads as its rule says"
        );
    }
    assert!(
        draft_read_by(&workspaces, &workspace, &host_person(&workspaces)).await,
        "its author reads it"
    );
    assert!(
        !draft_read_by(&workspaces, &workspace, "bo").await,
        "another member reads nothing of it"
    );
}

/// A repair derives from what it moves — its actor, moment and invocation come from the record,
/// the heads it is held at and the device's person — so a device drafting it twice writes the
/// one change, and a workspace that takes both holds one copy of it. Another device's repair of
/// the same record lands under the same key in the same document, where one copy stands.
#[tokio::test]
async fn a_repair_taken_in_twice_leaves_one_copy() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = hosting_device();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let created = workspaces.create("Mail").await.unwrap();
    let workspace = created.workspace.clone();
    crate::workspaces::fixtures::declare_app(&server, APP, &surface(false));
    write_misplaced_draft(&workspaces, &workspace)
        .await
        .expect("the draft is written");
    crate::workspaces::fixtures::declare_app(&server, APP, &surface(true));

    let state = workspaces.workspace(&workspace).await.unwrap();
    let mut hosted = state.lock().await;
    let mut store = workspaces.store_of(&mut hosted);
    let invoker = host_person(&workspaces);
    let first = crate::workspaces::replacing::repair_writes(&mut store, DRAFT, "draft", &invoker)
        .expect("the repair drafts");
    let second = crate::workspaces::replacing::repair_writes(&mut store, DRAFT, "draft", &invoker)
        .expect("the repair drafts again");
    let hashes = |writes: &[tilo_resource_store::Written]| -> Vec<String> {
        writes
            .iter()
            .map(|write| hex::encode(write.change.hash()))
            .collect()
    };
    assert_eq!(
        hashes(&first),
        hashes(&second),
        "a second drafting of one repair is the one change"
    );
    // The same change taken in twice is taken once: the second repair adds nothing.
    for writes in [&first, &second] {
        for write in writes {
            store
                .receive(
                    &write.doc,
                    &write.readers,
                    vec![tilo_resource_store::Received {
                        change: write.change.clone(),
                        author: [1; 32],
                        claimed: None,
                    }],
                    &|_: &tilo_resource_store::Proposal<'_>| true,
                )
                .expect("the repair is taken in");
        }
    }
    assert!(
        !store.misplaced(DRAFT, "draft"),
        "the workspace holds the draft in the document its rule gives"
    );
    assert_eq!(
        store.index().ids_of(DRAFT).collect::<Vec<_>>(),
        vec!["draft"],
        "one copy of it"
    );
}
