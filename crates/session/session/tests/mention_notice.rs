//! That the person is told when someone mentions them in a workspace's record, and that choosing
//! the notice opens the record in the app that keeps it.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the session lives for its whole test"
)]

use std::fs;
use std::rc::Rc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use automerge::marks::{ExpandMark, Mark};
use automerge::transaction::Transactable as _;
use automerge::{AutoCommit, ObjType, ROOT};
use durable_store::{StorageKey, WriteOp};
use tilo_actions::ActionAddress;
use tilo_actions::remote::{RemoteActionTarget, application_provider, application_provider_record};
use tilo_desktop_search::providers::PublishedActions;
use tilo_session::platform::Store;
use tilo_session::platform::notice::OPEN_ACTION;
use tilo_session::{AppActions, Options, Records, SearchOptions};
use ui::mentions::MARK;
use ui::record::{RecordId, RecordKind};

use crate::common::{PERSON, headless};

/// The app whose record a mention is written in.
const PROBE: &str = "so.tilo.probe";
/// The record's route, as its app names it.
const RECORD: &str = "tab:00000000000000a1";
/// Who mentions the person in it.
const BEA: &str = "bea";
/// What the record says, the mention of the person in it.
const SAID: &str = "Ask @Pat about the date";
/// The record's kind, as its document names it.
const KIND: &str = "probe.notebook";

/// The document of the record: one page whose paragraph mentions `person` where its text names
/// them.
fn notebook(workspace: &str, person: &str) -> AutoCommit {
    let mut doc = AutoCommit::new();
    ui::record::init(
        &mut doc,
        &RecordId::parse(RECORD).expect("a record id"),
        &RecordKind::new(KIND),
        workspace,
    );
    let pages = doc.put_object(ROOT, "pages", ObjType::Map).unwrap();
    let entry = doc.put_object(&pages, "p1", ObjType::Map).unwrap();
    doc.put(&entry, "k", "a").unwrap();
    let page = doc.put_object(&entry, "v", ObjType::Map).unwrap();
    doc.put(&page, "title", "Launch plan").unwrap();
    let body = doc.put_object(&page, "body", ObjType::Map).unwrap();
    let blocks = doc.put_object(&body, "blocks", ObjType::Map).unwrap();
    let block = doc.put_object(&blocks, "b7", ObjType::Text).unwrap();
    doc.splice_text(&block, 0, 0, SAID).unwrap();
    let length = SAID.chars().count();
    doc.mark(
        &block,
        Mark::new("author".into(), BEA, 0, length),
        ExpandMark::None,
    )
    .unwrap();
    // "Ask @Pat about the date": the run the mention covers.
    doc.mark(
        &block,
        Mark::new(MARK.into(), person, 4, 8),
        ExpandMark::None,
    )
    .unwrap();
    doc.commit();
    doc
}

/// The store's key of `app`'s [`RECORD`] in `workspace`.
fn key_of(app: &str, workspace: &str) -> StorageKey {
    StorageKey::record(
        app,
        workspace,
        RecordId::parse(RECORD).expect("a record id").storage_key(),
    )
}

/// Write `doc` as `app`'s record of `workspace`, through the store the session reads: the write an
/// app of the workspace makes.
fn write_record(store: &Store, app: &str, workspace: &str, doc: &mut AutoCommit) {
    let key = key_of(app, workspace);
    let lease = store.lease(&key).expect("the record's key is this app's");
    let bytes = doc.save();
    let actor: Vec<u8> = doc.get_actor().to_bytes().to_vec();
    store
        .write(&key, WriteOp::Replace(bytes), Some(&actor))
        .expect("the record is written");
    store.flush(Some(&key)).expect("the record is kept");
    drop(lease);
}

/// What the person's opens of an app asked for, as the session's own action seam receives them.
#[derive(Default)]
struct Opens(
    Mutex<
        Vec<(
            RemoteActionTarget,
            serde_json::Map<String, serde_json::Value>,
        )>,
    >,
);

impl AppActions for Opens {
    fn published(&self, _workspace: &str) -> Vec<PublishedActions> {
        Vec::new()
    }

    fn invoke(
        &self,
        target: RemoteActionTarget,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), tilo_actions::DoorError> {
        self.0.lock().expect("the opens").push((target, arguments));
        Ok(())
    }

    fn cancel(&self, _: &str, _: &str, _: &str) -> Result<(), tilo_actions::DoorError> {
        Err(tilo_actions::DoorError::Unknown(ActionAddress::new("", "")))
    }

    fn open_uri(&self, _: &str, _: &str) -> Result<(), tilo_actions::DoorError> {
        Err(tilo_actions::DoorError::Unknown(ActionAddress::new("", "")))
    }

    fn start_host_app(&self, _: &str) -> Result<(), tilo_actions::DoorError> {
        Err(tilo_actions::DoorError::Unknown(ActionAddress::new("", "")))
    }

    fn jobs(&self) -> Vec<tilo_desktop_search::providers::jobs::JobEntry> {
        Vec::new()
    }
}

impl Opens {
    /// The opens asked for since the last call.
    fn taken(
        &self,
    ) -> Vec<(
        RemoteActionTarget,
        serde_json::Map<String, serde_json::Value>,
    )> {
        std::mem::take(&mut *self.0.lock().expect("the opens"))
    }
}

/// What the session asked of its apps when the person chose the notice: the record's app, in the
/// workspace that holds it, on the record and at the place the notice named.
fn opening_in(
    workspace: &str,
    (target, arguments): (
        RemoteActionTarget,
        serde_json::Map<String, serde_json::Value>,
    ),
) {
    assert_eq!(
        application_provider(&target.provider),
        Some((workspace, PROBE)),
        "the app that keeps the record, in the workspace that holds it: {target:?}"
    );
    assert_eq!(
        application_provider_record(&target.provider),
        Some(
            RecordId::parse(RECORD)
                .expect("a record id")
                .storage_key()
                .as_str()
        ),
        "on the record the notice names: {target:?}"
    );
    assert_eq!(
        target.address,
        ActionAddress::new(format!("{KIND}/pages/p1"), OPEN_ACTION),
        "at the mention's own place in it"
    );
    assert!(arguments.is_empty(), "the open carries nothing else");
}

/// A home whose device has an identity, and the app a record of the test's is kept by: the state a
/// development session starts in.
fn home_with_an_identity() -> tilo_test_env::TestHome {
    let mut home = tilo_test_env::TestHome::new().expect("a home");
    // The identity `dist/dev/nested-session.sh` mints before it starts its session: a throwaway whose
    // certificate lets this device govern its own workspaces, so the desk's workspace is one this
    // session holds.
    let identity = home.config().join("tilo/identity");
    fs::create_dir_all(&identity).expect("the identity's directory");
    let root = home.root().join("system");
    home.env().set("TILO_IDENTITY_ROOT", &root);
    tilo_identity_host::provisioning::ensure_throwaway_test_identity(
        &identity,
        &tilo_identity_host::provisioning::collaboration_identity_paths(),
        tilo_identity_host::provisioning::IdentityStorage::Files,
    )
    .expect("the throwaway identity");
    crate::common::probe_app(&mut home, PROBE, "Probe");
    home
}

#[test]
fn a_mention_in_a_record_is_told_and_choosing_it_opens_the_record() {
    let _settled = ui::motion::testing::settled();
    let home = home_with_an_identity();
    // The workspaces' records, in this device's data: what the session's activity and its mentions
    // follow, and what this test writes an app's record through.
    let records = Records::of(home.data().to_path_buf());
    let store = records.store();
    let opens = Rc::new(Opens::default());
    let options = Options {
        person: PERSON.to_owned(),
        collab: true,
        apps: Some(Rc::clone(&opens) as Rc<dyn AppActions>),
        state: Some(home.state().to_path_buf()),
        action_plane: tilo_session::ActionPlane::At(home.runtime().join("plane.sock")),
        search: SearchOptions {
            records: Some(records),
            ..SearchOptions::default()
        },
        ..Options::default()
    };
    // Who sits here, as the session's own seat reads it: this device's identity names them, and a
    // mention of anyone else is not theirs.
    let person = tilo_session::identity::seated_person(&options);
    let mut session = headless(options);
    session.step(Duration::from_millis(1)).expect("a frame");
    let workspace = session
        .handle()
        .read(|tree| {
            tree.active_workspace()
                .map(|workspace| workspace.id().to_string())
        })
        .expect("the desk shows a workspace");

    // An app of the workspace writes a record that mentions the person, and they are told.
    let written = Instant::now();
    write_record(
        &store,
        PROBE,
        &workspace,
        &mut notebook(&workspace, &person),
    );
    let notice = crate::common::wait_for(
        &mut session,
        "the person is told of the mention",
        |session| {
            session
                .notifications()
                .feed()
                .banners
                .into_iter()
                .find(|banner| banner.notification.body == SAID)
        },
    );
    let told = written.elapsed();
    assert!(
        notice.notification.title.contains("mentioned you"),
        "the notice says the person was mentioned: {:?}",
        notice.notification.title
    );
    assert_eq!(
        notice.notification.app_name.as_deref(),
        Some("Probe"),
        "the app that keeps it"
    );
    assert!(
        notice.actions.iter().any(|(key, _)| key == OPEN_ACTION),
        "the notice offers the open: {:?}",
        notice.actions
    );
    assert!(opens.taken().is_empty(), "nothing opens until they choose");

    // They choose it: the record opens in the app that keeps it, in the workspace that holds it.
    let chosen = Instant::now();
    session
        .notifications()
        .invoke_action(notice.notification.id, OPEN_ACTION);
    let opening = crate::common::wait_for(&mut session, "the record opens", |_| {
        opens.taken().into_iter().next()
    });
    let opened = chosen.elapsed();
    opening_in(&workspace, opening);
    eprintln!("mention told in {told:?}, chosen and opened in {opened:?}");
}
