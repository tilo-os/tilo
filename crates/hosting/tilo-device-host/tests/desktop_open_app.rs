//! `OpenApp`: open an application without holding a desktop layout (an agent,
//! the command palette). The owner places it and creates the checked record
//! through the same factory as a person's OpenTab.
tilo_automerge_isolate::test_worker!();

use tilo_peer_state::{desktop_command::*, owner_commit::*};
use tilo_workspace_host::SessionOwner;

fn request(
    owner: &SessionOwner,
    command: DesktopCommand,
    heads: Vec<[u8; 32]>,
) -> OwnerCommitRequest {
    OwnerCommitRequest {
        room: owner.peer().room().into(),
        operation: CommitOperation::DesktopCommand {
            invocation: uuid::Uuid::new_v4(),
            caller: owner.peer().authority().clone(),
            command: BoundCommand {
                source: CommandSource {
                    room: owner.peer().room().into(),
                    heads,
                },
                command,
            },
        },
    }
}

fn heads(owner: &SessionOwner) -> Vec<[u8; 32]> {
    owner
        .peer()
        .admitted_room()
        .unwrap()
        .clone()
        .get_heads()
        .into_iter()
        .map(|h| h.0)
        .collect()
}

fn tree(owner: &SessionOwner) -> tilo_desktop_model::DesktopTree {
    let mut tree = tilo_desktop_model::DesktopTree::default();
    tilo_desktop_model::projection::apply_admitted_room(
        &mut tree,
        tilo_desktop_model::RoomId::parse(owner.peer().room()).unwrap(),
        owner.peer().admitted_room().unwrap(),
    )
    .unwrap();
    tree
}

fn notes_records(owner: &SessionOwner) -> Vec<String> {
    owner
        .peer()
        .rows()
        .into_iter()
        .filter(|row| row.app_id == "so.tilo.notes")
        .map(|row| row.record_id)
        .collect()
}

#[test]
fn open_app_places_a_new_window_and_creates_the_checked_record() {
    let root = tempfile::tempdir().unwrap();
    let mut owner = SessionOwner::initialize(&root.path().join("owner")).unwrap();
    let before = tree(&owner).workspaces[0]
        .strips
        .last()
        .unwrap()
        .window_groups
        .len();
    let existing = notes_records(&owner);
    let opening = request(
        &owner,
        DesktopCommand::OpenApp {
            app: "so.tilo.notes".into(),
        },
        heads(&owner),
    );
    let receipt = owner.commit_desktop_command(&opening).unwrap();
    receipt.validate_for(&opening).unwrap();

    let tree = tree(&owner);
    let strip = tree.workspaces[0].strips.last().unwrap();
    assert_eq!(strip.window_groups.len(), before + 1, "a new window group");
    let tab = strip.window_groups.last().unwrap().tabs[0].id;
    let record = ui::record::RecordId::for_tab(tab).to_string();
    let mut expected = existing.clone();
    expected.push(record.clone());
    if !existing.iter().any(String::is_empty) {
        // A native app process opens the package primary before any window.
        expected.push(String::new());
    }
    expected.sort();
    let mut actual = notes_records(&owner);
    actual.sort();
    assert_eq!(
        actual, expected,
        "the same checked record a person's OpenTab creates"
    );
    assert!(owner.peer().document("so.tilo.notes", &record).is_some());
}

#[test]
fn open_app_is_placement_independent_of_the_source_revision() {
    let root = tempfile::tempdir().unwrap();
    let mut owner = SessionOwner::initialize(&root.path().join("owner")).unwrap();
    let stale = heads(&owner);
    let existing = notes_records(&owner).len();
    // The desktop changes after the caller observed it.
    let strip = tree(&owner).workspaces[0].strips[0].id.0;
    owner
        .commit_desktop_command(&request(
            &owner,
            DesktopCommand::OpenTab {
                app: "so.tilo.calculator".into(),
                placement: TabPlacement::NewGroup { strip, after: None },
            },
            heads(&owner),
        ))
        .unwrap();
    // A positional command from the stale revision would be refused, but
    // opening an app is placed against the current topology.
    owner
        .commit_desktop_command(&request(
            &owner,
            DesktopCommand::OpenApp {
                app: "so.tilo.notes".into(),
            },
            stale,
        ))
        .unwrap();
    // The window's record, and the package primary the first open creates.
    assert_eq!(notes_records(&owner).len(), existing + 2);
}

#[test]
fn open_app_refuses_an_application_without_a_checked_factory() {
    let root = tempfile::tempdir().unwrap();
    let mut owner = SessionOwner::initialize(&root.path().join("owner")).unwrap();
    let before = heads(&owner);
    let existing = notes_records(&owner);
    assert!(
        owner
            .commit_desktop_command(&request(
                &owner,
                DesktopCommand::OpenApp {
                    app: "so.tilo.not-installed".into(),
                },
                heads(&owner),
            ))
            .is_err()
    );
    assert_eq!(heads(&owner), before, "a refusal changes nothing");
    assert_eq!(notes_records(&owner), existing);
}

#[test]
fn open_app_needs_no_source_heads() {
    // The broker's open-application action has no desktop layout to name.
    let root = tempfile::tempdir().unwrap();
    let mut owner = SessionOwner::initialize(&root.path().join("owner")).unwrap();
    let existing = notes_records(&owner).len();
    let opening = request(
        &owner,
        DesktopCommand::OpenApp {
            app: "so.tilo.notes".into(),
        },
        vec![],
    );
    owner
        .commit_desktop_command(&opening)
        .unwrap()
        .validate_for(&opening)
        .unwrap();
    // The window's record, and the package primary the first open creates.
    assert_eq!(notes_records(&owner).len(), existing + 2);
    // Positional commands still need their source revision.
    assert!(
        owner
            .commit_desktop_command(&request(
                &owner,
                DesktopCommand::CreateStrip { name: "x".into() },
                vec![],
            ))
            .is_err()
    );
}

#[test]
fn the_first_open_of_an_app_also_creates_its_package_primary() {
    let root = tempfile::tempdir().unwrap();
    let mut owner =
        SessionOwner::initialize_workspace(&root.path().join("owner"), "Guest").unwrap();
    let records = |owner: &SessionOwner| {
        let mut records = owner
            .peer()
            .rows()
            .into_iter()
            .filter(|row| row.app_id == "so.tilo.word-processor")
            .map(|row| row.record_id)
            .collect::<Vec<_>>();
        records.sort();
        records
    };
    assert!(records(&owner).is_empty());
    for _ in 0..2 {
        let opening = request(
            &owner,
            DesktopCommand::OpenApp {
                app: "so.tilo.word-processor".into(),
            },
            heads(&owner),
        );
        owner
            .commit_desktop_command(&opening)
            .unwrap()
            .validate_for(&opening)
            .unwrap();
    }
    let records = records(&owner);
    assert_eq!(
        records.len(),
        3,
        "one primary and two window records: {records:?}"
    );
    assert_eq!(records[0], "", "the package primary");
    assert!(records[1..].iter().all(|record| record.starts_with("tab:")));
}
