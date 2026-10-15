//! Visual waits service the writer, but never substitute submission for its ACK.

use super::*;
use crate::application::{LaunchTarget, RestoreInput};
use crate::application_test_support::{
    self as probe, ProbeHost, runner_inputs, runner_permissions, runner_probe_definition,
};
use crate::record::{RecordCheckout, RecordSaveStatus};
use automerge::{ROOT, ReadDoc, transaction::Transactable};
use std::rc::Rc;

struct Fixture {
    host: ProbeHost,
    backend: Rc<durable_store::MemoryStore>,
    frame: FrameHarness,
    checkout: RecordCheckout,
}

#[expect(
    clippy::unwrap_used,
    reason = "fixture setup and edits must fail loudly before the production-path assertions"
)]
impl Fixture {
    fn new(backend: durable_store::MemoryStore) -> Self {
        let backend = Rc::new(backend);
        let mut host = ProbeHost::new();
        let mut owners = crate::record::RecordOwners::with_store(backend.clone());
        host.records = owners.open(probe::APP, probe::ROOM).unwrap();
        host.views = Rc::new(
            crate::application_view::InstanceViewStore::with_store(
                backend.clone(),
                probe::DEVICE,
                probe::ROOM,
            )
            .unwrap(),
        );
        host.owners = owners;
        let definition = runner_probe_definition();
        let component = definition
            .instantiate(
                host.prepare_with_permissions(
                    definition,
                    11,
                    &LaunchTarget::New,
                    runner_permissions(),
                )
                .unwrap(),
                LaunchTarget::New,
                RestoreInput::None,
            )
            .unwrap();
        let checkout = component.record().clone();
        let frame = FrameHarness::from_component(component, (400, 300), runner_inputs()).unwrap();
        Self {
            host,
            backend,
            frame,
            checkout,
        }
    }

    fn edit(&self) {
        self.host
            .records
            .with(&self.checkout.id, |doc| {
                doc.put(ROOT, "visual-wait", "edited while waiting")
                    .unwrap();
            })
            .unwrap();
    }

    fn status(&self) -> RecordSaveStatus {
        self.host.records.save_status(&self.checkout.id).unwrap()
    }
}

#[test]
fn visual_wait_submits_and_samples_a_save_before_its_first_readiness_check() {
    let mut fixture = Fixture::new(durable_store::MemoryStore::new());
    fixture.edit();
    fixture.frame.relayout();
    assert_eq!(fixture.status(), RecordSaveStatus::Saving);
    assert!(fixture.frame.root.has_pending_visual_work());
    // Immediate ACKs need no park. A zero bound still runs the ordinary
    // initial frame/idle/frame sequence, including the real record writer.
    fixture
        .frame
        .wait_for_visuals(crate::time::Duration::ZERO)
        .unwrap();
    assert_eq!(fixture.status(), RecordSaveStatus::Saved);
    assert!(!fixture.frame.root.has_pending_visual_work());
}

#[test]
fn visual_wait_does_not_confuse_writer_submission_with_acknowledgement() {
    let mut fixture = Fixture::new(durable_store::MemoryStore::queued());
    fixture.edit();
    assert!(
        fixture
            .frame
            .wait_for_visuals(crate::time::Duration::ZERO)
            .is_err()
    );
    assert_eq!(fixture.status(), RecordSaveStatus::Saving);
    let writes = fixture.backend.drain();
    assert_eq!(
        writes.len(),
        1,
        "the wait must offer the actual dirty record"
    );
    assert!(fixture.frame.root.has_pending_visual_work());
    let committed = Rc::new(durable_store::MemoryStore::new());
    for write in writes {
        use durable_store::DurableStore as _;
        committed
            .write(&write.key, write.op, write.writer.as_deref())
            .unwrap();
        fixture.backend.acknowledge(write.revision, Ok(())).unwrap();
    }
    fixture
        .frame
        .wait_for_visuals(crate::time::Duration::ZERO)
        .unwrap();
    assert_eq!(fixture.status(), RecordSaveStatus::Saved);
    assert!(!fixture.frame.root.has_pending_visual_work());
    // Reopen only the bytes the wait actually submitted, not the live
    // writer's cache or a close/flush that could conceal a missing tick.
    let mut owners = crate::record::RecordOwners::with_store(committed);
    let reopened = owners.open(probe::APP, probe::ROOM).unwrap();
    reopened
        .open_existing_record_checked(&fixture.checkout)
        .unwrap();
    let value = reopened
        .read(&fixture.checkout.id, |doc| {
            doc.get(ROOT, "visual-wait")
                .unwrap()
                .unwrap()
                .0
                .as_str()
                .unwrap()
                .to_owned()
        })
        .unwrap();
    assert_eq!(value, "edited while waiting");
}

#[test]
fn waiting_services_edits_arriving_after_the_initial_idle_pass() {
    let mut fixture = Fixture::new(durable_store::MemoryStore::new());
    let records = fixture.host.records.clone();
    let id = fixture.checkout.id.clone();
    let mut edited = false;
    assert!(fixture.frame.wait_until(crate::HANG_BOUND, |_| {
        if !edited {
            edited = true;
            records
                .with(&id, |doc| doc.put(ROOT, "late", true).unwrap())
                .unwrap();
            assert_eq!(records.save_status(&id).unwrap(), RecordSaveStatus::Saving);
            return false;
        }
        records.save_status(&id).unwrap() == RecordSaveStatus::Saved
    }));
    assert!(!fixture.frame.root.has_pending_visual_work());
}
