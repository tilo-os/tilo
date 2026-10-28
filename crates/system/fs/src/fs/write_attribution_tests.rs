//! The versions this mount makes of the paths the desktop session leases
//! carry the agent it names in the signed record, beside the device's own
//! signature.

use super::*;
use collab_types::Ed25519PublicKey;
use ed25519_dalek::SigningKey;
use fs_protocol::FileActor;
use iroh::endpoint::{Endpoint, presets};
use uuid::Uuid;

fn actor_and_signer() -> (FileActor, SigningKey) {
    let person = SigningKey::from_bytes(&[0x41; 32]);
    let device = SigningKey::from_bytes(&[0x52; 32]);
    (
        FileActor {
            person: Ed25519PublicKey(person.verifying_key().to_bytes()),
            device: Ed25519PublicKey(device.verifying_key().to_bytes()),
        },
        device,
    )
}

/// The desktop session names the agent an action ran for. The versions
/// this mount then makes of the leased paths carry the agent in the
/// signed record, beside the device's own signature, and Files' history
/// read returns it with them. Paths the lease does not name stay the
/// person's.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_leased_writes_carry_their_agent_in_signed_history() {
    use fs_protocol::files::{FileHistoryReadQueryV1, FileHistoryReadValueV1};
    let dir = tempfile::tempdir().unwrap();
    let workspace_id = Uuid::from_u128(0x52);
    let endpoint = Endpoint::bind(presets::Minimal).await.unwrap();
    let store = Arc::new(crate::store::ContentStore::memory());
    let namespace = Arc::new(Mutex::new(Namespace::new("host").unwrap()));
    let history =
        Arc::new(crate::history::FileHistoryStore::open(dir.path().join("history.json")).unwrap());
    let repository = Arc::new(
        crate::record_repository::RecordRepository::open(dir.path().join("records")).unwrap(),
    );
    let (actor, signer) = actor_and_signer();
    let publisher = Arc::new(
        crate::record_publisher::RecordPublisher::new(repository, workspace_id, actor, signer)
            .unwrap(),
    );
    let attributions = Arc::new(crate::write_attribution::WriteAttributions::default());
    let hydrator = Arc::new(Hydrator::new(endpoint, store));
    let heuristics = Arc::new(Heuristics::new(hydrator.clone()));
    let fs = QuickFS::new(namespace, hydrator, heuristics)
        .with_checked_history_workspace(workspace_id)
        .with_history(history, actor)
        .unwrap()
        .with_record_publisher(publisher)
        .unwrap()
        .with_write_attributions(Arc::clone(&attributions));
    let scribe = fs_protocol::FileAttributionV1 {
        actor: "agent:scribe".into(),
        label: "Scribe".into(),
        color: [10, 20, 30],
    };
    attributions
        .grant(
            vec!["/Reports".into()],
            scribe.clone(),
            std::time::Duration::from_secs(30),
        )
        .unwrap();

    let (folder, _) = fs
        .create_empty_dir(crate::namespace::ROOT_INO, "Reports")
        .unwrap();
    let (report, _) = fs.create_empty_file(folder, "q3.txt").unwrap();
    fs.commit_content(report, b"written for Ada").unwrap();
    // A rename inside the leased folder is Scribe's too.
    fs.rename_named(folder, "q3.txt", folder, "q4.txt").unwrap();
    let (notes, _) = fs
        .create_empty_file(crate::namespace::ROOT_INO, "notes.txt")
        .unwrap();

    let page = |ino| match fs
        .read_file_history(ino, FileHistoryReadQueryV1::Page { cursor: None })
        .unwrap()
    {
        FileHistoryReadValueV1::Page(page) => page,
        other @ FileHistoryReadValueV1::Version { .. } => panic!("not a page: {other:?}"),
    };
    let report = page(report);
    assert_eq!(report.versions.len(), 3, "created, written, then renamed");
    assert_eq!(
        report.attributions.len(),
        3,
        "all were Scribe's: {report:?}"
    );
    for attributed in &report.attributions {
        assert_eq!(attributed.attribution, scribe);
        let version = report
            .versions
            .iter()
            .find(|version| version.id == attributed.version_id)
            .expect("an attribution names a version on its page");
        assert_eq!(
            version.actor, actor,
            "the device still signs the version, for its person"
        );
    }
    assert_eq!(page(folder).attributions.len(), 1, "the folder too");
    assert!(
        page(notes).attributions.is_empty(),
        "a path the lease does not name stays the person's"
    );
}
