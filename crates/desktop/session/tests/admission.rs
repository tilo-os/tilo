//! A desktop session admits what it runs and what it does of its own: an app of the person's joins
//! the session's app wire, the session takes an action as the person, and setup runs its first
//! step. Each asks the device trust the session installs for itself, which admits this device from
//! its own certificate before the record of the person's devices has answered.
//!
//! The session here is a real one: a device's own `tilo-session` process for the app and the
//! action, and this package's session for setup, with a person's identity provisioned as a
//! computer's first use provisions one. No test double stands in for the trust.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and a device lives until its session ends"
)]

use std::time::Duration;

use serde_json::json;

use crate::common::people::{Person, shared_invitation};

/// The Tasks app, as its package names it: what the session starts at the person's own open.
const TASKS: &str = "so.tilo.tasks";

/// An app of the person's joins the session's app wire, the session takes an action of its own,
/// and setup runs its first step.
#[test]
fn a_session_admits_app_joins_its_own_actions_and_setup() {
    let ada = Person::new("Ada");
    let bob = Person::new("Bob");
    // The person's first device founds their own workspace, which this session hosts on its own
    // service: no daemon answers this session's reads, and no host has listed the device.
    let mut laptop = ada.device("Ada's laptop");
    // A session reads the apps it has when it starts, so the app is installed before it does.
    laptop.install_dev_apps();
    laptop.start();
    let workspace = tilo_test_env::wait_for("Ada's shell shows the workspace it made", || {
        laptop.showing()
    });
    laptop.wait_holding(&workspace);

    // 1. An app of the person's joins the workspace on the session's app wire. Its action is
    // published only once the broker admitted the join with the device's certificate.
    laptop.open(TASKS);
    tilo_test_env::wait_until("Tasks joins the app wire and publishes new_task", || {
        laptop.actions().iter().any(|row| {
            row.app_id == TASKS
                && row
                    .action
                    .as_ref()
                    .is_some_and(|action| action.address.action == "new_task")
        })
    });

    // 2. The session takes one of its own actions as the person: sharing the workspace it holds.
    // The broker asks the identity authority for this device's certificate, which is admitted.
    let invitation = shared_invitation(&laptop, "editor", bob.public);

    // The invitation is the session's: another of the person's devices joins with it.
    let mut bob_laptop = bob.device("Bob's laptop");
    bob_laptop.join(&invitation);
    bob_laptop.wait_holding(&workspace);

    // 3. Setup runs its first step in a session: the person is asked how to bring an identity to
    // this computer, and the choice they make moves setup to its next step.
    let mut home = tilo_test_env::TestHome::new().expect("a home");
    let identity_dir = home.config().join("tilo/identity");
    std::fs::create_dir_all(&identity_dir).expect("a directory for the identity");
    let system = home.root().join("system");
    home.env().set("TILO_IDENTITY_ROOT", &system);
    let ceremony = tilo_onboarding_session::ceremony::Ceremony::new_for_host_desktop(identity_dir);
    let mut session = crate::common::headless(tilo_session::Options {
        setup: Some(tilo_session::BeforeShell::Ceremony(Box::new(ceremony))),
        collab: false,
        ..tilo_session::Options::default()
    });
    session.step(Duration::from_millis(1)).expect("a frame");
    assert!(session.setup_up(), "setup stands in front of the shell");
    let choose = tilo_onboarding_session::ceremony::OnboardingEffect::CHOOSE.id;
    crate::common::until(&mut session, "setup publishes its first step", |session| {
        crate::common::publisher(session, choose).is_some()
    });
    let setup = crate::common::publisher(&session, choose)
        .expect("setup publishes its first step's ways forward");
    let arguments = json!({ "option": "existing" })
        .as_object()
        .expect("an object")
        .clone();
    session
        .invoke(&setup, choose, &arguments)
        .expect("setup takes the way the person chose");
    assert!(
        session.setup_up(),
        "setup still runs after the person's first step"
    );
    // The title the setup app shows over the ways to bring an identity this person already has.
    let existing = "Use your existing identity";
    crate::common::until(
        &mut session,
        "setup moves on to the person's existing identity",
        |session| {
            session
                .accessible_labels()
                .iter()
                .any(|label| label.contains(existing))
        },
    );
}
