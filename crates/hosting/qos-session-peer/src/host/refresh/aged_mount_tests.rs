use super::*;
use crate::action_host::scoped_fixture::ReopenedAppFixture;
use qos_peer_state::{
    NeighbourMsg,
    desktop_command::{BoundCommand, CommandSource, DesktopCommand, TabPlacement},
    owner_commit::{CommitOperation, OwnerCommitRequest},
};
use std::time::{Duration, Instant};

// This deliberately costs five real minutes. It uses no test clock or edited
// signature, and does not qualify the still-closed normal worker installation.
#[tokio::test(flavor = "current_thread")]
async fn aged_restart_refreshes_before_notes_and_writer_mount() {
    let root = tempfile::Builder::new()
        .prefix(".qos-aged-startup-")
        .tempdir_in(std::env::current_dir().unwrap())
        .unwrap();
    let path = root.path().join("owner");
    let mut owner = SessionOwner::initialize(&path).unwrap();
    for app in ["so.tilo.notes", "so.tilo.word-processor"] {
        if owner.peer().rows().iter().any(|row| row.app_id == app) {
            continue;
        }
        let mut tree = qos_desktop::WorkspaceTree::default();
        qos_desktop::projection::RoomProjection::apply_admitted_room(
            &mut tree,
            uuid::Uuid::parse_str(owner.peer().room()).unwrap().into(),
            owner.peer().admitted_room().unwrap(),
        )
        .unwrap();
        let strip = tree.workspaces[0].strips[0].id.0;
        owner
            .commit_desktop_command(&OwnerCommitRequest {
                room: owner.peer().room().into(),
                operation: CommitOperation::DesktopCommand {
                    invocation: uuid::Uuid::new_v4(),
                    caller: owner.peer().authority().clone(),
                    command: BoundCommand {
                        source: CommandSource {
                            room: owner.peer().room().into(),
                            heads: owner
                                .peer()
                                .admitted_room()
                                .unwrap()
                                .clone()
                                .get_heads()
                                .into_iter()
                                .map(|h| h.0)
                                .collect(),
                        },
                        command: DesktopCommand::OpenTab {
                            app: app.into(),
                            placement: TabPlacement::NewGroup { strip, after: None },
                        },
                    },
                },
            })
            .unwrap();
    }
    let records = ["so.tilo.notes", "so.tilo.word-processor"].map(|app| {
        let record = owner
            .peer()
            .rows()
            .into_iter()
            .find(|row| row.app_id == app)
            .unwrap()
            .record_id;
        let document = owner.peer().document(app, &record).unwrap();
        (app, record, document)
    });
    let generation = owner.committed_generation();
    let original = owner
        .outgoing()
        .iter()
        .map(|entry| entry.bytes.to_vec())
        .collect::<Vec<_>>();
    drop(owner);
    let age_started = Instant::now();
    eprintln!(
        "aging genuine persisted governor checkpoint for 301 seconds; persistent owner path={}; wall start={:?}",
        path.display(),
        web_time::SystemTime::now()
    );
    tokio::time::sleep(Duration::from_secs(301)).await;
    assert!(age_started.elapsed() > Duration::from_secs(300));
    let mut owner = SessionOwner::open(&path).unwrap();
    assert_eq!(owner.committed_generation(), generation);
    assert_eq!(
        owner
            .outgoing()
            .iter()
            .map(|entry| entry.bytes.to_vec())
            .collect::<Vec<_>>(),
        original
    );
    let now = web_time::SystemTime::now();
    let expired = original
        .iter()
        .filter_map(|bytes| postcard::from_bytes::<NeighbourMsg>(bytes).ok())
        .filter(|message| matches!(message, NeighbourMsg::AppDocument { .. }))
        .collect::<Vec<_>>();
    assert!(expired.len() >= records.len());
    assert!(
        expired
            .iter()
            .all(|message| !qos_peer_state::outbox::fresh(message, now))
    );
    let committed = CommittedAuthority::new(&owner);
    before_action_mount(&mut owner, &committed, None).unwrap();
    let refreshed_generation = owner.committed_generation();
    assert!(refreshed_generation > generation);
    for (app, record, document) in &records {
        assert_eq!(owner.peer().document(app, record).unwrap(), *document);
    }
    let disk = std::fs::read(path.join("session.bin")).unwrap();
    let mut fixture = ReopenedAppFixture::new(root, owner, committed);
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        fixture.pump().await.unwrap();
        let notes = fixture.action_live(records[0].0, &records[0].1, "import-markdown");
        let writer = fixture.action_live(records[1].0, &records[1].1, "save-as");
        if notes
            && writer
            && fixture.settled().unwrap()
            && records
                .iter()
                .all(|(app, record, _)| fixture.replayed(app, record))
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "aged startup never reached both actual action surfaces and settled exact receipts"
        );
    }
    assert_eq!(
        fixture.owner.committed_generation(),
        refreshed_generation,
        "startup replay must perform no new owner write"
    );
    assert_eq!(std::fs::read(path.join("session.bin")).unwrap(), disk);
    for (app, record, document) in &records {
        assert_eq!(
            fixture.owner.peer().document(app, record).unwrap(),
            *document
        );
    }
    fixture.close().unwrap();
    eprintln!(
        "aged restart mounted Notes and Writer after {:?} real elapsed time",
        age_started.elapsed()
    );
}
