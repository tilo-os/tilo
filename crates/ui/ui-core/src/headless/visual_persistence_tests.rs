//! Visual waits service the writer, but never substitute submission for its ACK.

use super::*;
use crate::application::{LaunchTarget, RestoreInput};
use crate::application_test_support::{
    self as probe, ProbeHarness, runner_inputs, runner_permissions, runner_probe_definition,
};
use crate::resource::{DocumentCheckout, ResourceOwners, ResourceSaveStatus};
use automerge::{ROOT, ReadDoc, transaction::Transactable};

struct Fixture {
    host: ProbeHarness,
    frame: FrameHarness,
    checkout: DocumentCheckout,
}

#[expect(
    clippy::unwrap_used,
    reason = "fixture setup and edits must fail loudly before the production-path assertions"
)]
impl Fixture {
    fn new(host: ProbeHarness) -> Self {
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
            frame,
            checkout,
        }
    }

    /// An edit the window made, as its actions commit one.
    fn edit(&self) {
        self.host
            .records
            .with(&self.checkout.id, |doc| {
                doc.put(ROOT, "visual-wait", "edited while waiting")
                    .unwrap();
                doc.commit();
            })
            .unwrap();
    }

    fn status(&self) -> ResourceSaveStatus {
        self.host.records.save_status(&self.checkout.id).unwrap()
    }
}

/// What a process that did not write it reads back from the workspace.
#[expect(
    clippy::unwrap_used,
    reason = "the reopened record must read back or the assertion has nothing to compare"
)]
fn reopened_value(host: &ProbeHarness, checkout: &DocumentCheckout) -> String {
    let mut owners = ResourceOwners::writing(host.workspace.clone());
    let records = owners.open(probe::APP, probe::WORKSPACE).unwrap();
    records.open_existing_resource_checked(checkout).unwrap();
    records
        .read(&checkout.id, |doc| {
            doc.get(ROOT, "visual-wait")
                .unwrap()
                .unwrap()
                .0
                .as_str()
                .unwrap()
                .to_owned()
        })
        .unwrap()
}

#[test]
fn visual_wait_submits_and_samples_a_save_before_its_first_readiness_check() {
    let mut fixture = Fixture::new(ProbeHarness::new());
    fixture.edit();
    fixture.frame.relayout();
    assert_eq!(fixture.status(), ResourceSaveStatus::Saving);
    assert!(fixture.frame.root.has_pending_visual_work());
    fixture
        .frame
        .wait_for_visuals(crate::time::Duration::ZERO)
        .unwrap();
    assert_eq!(fixture.status(), ResourceSaveStatus::Saved);
    assert!(!fixture.frame.root.has_pending_visual_work());
}

#[test]
fn visual_wait_does_not_confuse_writer_submission_with_acknowledgement() {
    let mut fixture = Fixture::new(ProbeHarness::holding_writes());
    fixture.edit();
    assert!(
        fixture
            .frame
            .wait_for_visuals(crate::time::Duration::ZERO)
            .is_err()
    );
    assert_eq!(fixture.status(), ResourceSaveStatus::Saving);
    let writes = fixture.host.held_writes();
    assert_eq!(
        writes.len(),
        1,
        "the wait must offer the actual dirty record"
    );
    assert_eq!(writes[0].id, fixture.checkout.id.route());
    assert!(fixture.frame.root.has_pending_visual_work());
    // The workspace takes the write the wait offered, and the store hears the answer.
    // A close's flush waits for the workspace, so it takes the write up.
    fixture.host.records.try_flush().unwrap();
    // The answer is taken up by the window's own passes, which service the store's wake.
    fixture.frame.persist_tick();
    fixture.frame.service_pending_owner_models().unwrap();
    fixture.frame.persist_tick();
    assert_eq!(fixture.status(), ResourceSaveStatus::Saved);
    fixture
        .frame
        .wait_for_visuals(tilo_test_env::hang_bound())
        .unwrap();
    assert_eq!(fixture.status(), ResourceSaveStatus::Saved);
    assert!(!fixture.frame.root.has_pending_visual_work());
    // Reopen only the bytes the wait actually submitted, not the live writer's cache or a
    // close/flush that could conceal a missing tick.
    assert_eq!(
        reopened_value(&fixture.host, &fixture.checkout),
        "edited while waiting"
    );
}

#[test]
fn waiting_services_edits_arriving_after_the_initial_idle_pass() {
    let mut fixture = Fixture::new(ProbeHarness::new());
    let records = fixture.host.records.clone();
    let id = fixture.checkout.id.clone();
    let mut edited = false;
    assert!(fixture.frame.wait_until(tilo_test_env::hang_bound(), |_| {
        if !edited {
            edited = true;
            records
                .with(&id, |doc| {
                    doc.put(ROOT, "late", true).unwrap();
                    doc.commit();
                })
                .unwrap();
            assert_eq!(
                records.save_status(&id).unwrap(),
                ResourceSaveStatus::Saving
            );
            return false;
        }
        records.save_status(&id).unwrap() == ResourceSaveStatus::Saved
    }));
    assert!(!fixture.frame.root.has_pending_visual_work());
}
