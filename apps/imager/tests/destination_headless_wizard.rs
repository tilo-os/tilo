//! The installer on the live medium, hosted as the imager app hosts it: its
//! effects reached through the app's own door, with the availability, the
//! items they are bound to and the confirmation they ask for. The console
//! and the wizard's controls reach the same handler (the surface's own
//! tests walk the whole wizard through both).

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test assertions"
)]

use imager::ImagerShell;
use imager::setup::destination::effects::DestinationEffect;
use ui::framework::testing::TestApp;

fn status(app: &mut TestApp<ImagerShell>) -> serde_json::Value {
    app.effect(&DestinationEffect::Status)
        .unwrap()
        .value
        .unwrap()
}

#[test]
fn the_installer_answers_its_effects_through_the_app_door() {
    let mut app = TestApp::<ImagerShell>::launched(&["--destination"]).unwrap();
    assert_eq!(status(&mut app)["step"], "keyboard");

    // A control that is not on screen is not available either, and says why.
    let off_step = app.effect(&DestinationEffect::SetHostname {
        hostname: "lab-1".into(),
    });
    assert!(off_step.is_err(), "{off_step:?}");

    app.effect(&DestinationEffect::SetLayout {
        layout: "English (US)".into(),
    })
    .unwrap();
    app.effect(&DestinationEffect::Next).unwrap();
    assert_eq!(status(&mut app)["step"], "network");
    app.effect(&DestinationEffect::Next).unwrap();
    assert_eq!(status(&mut app)["step"], "accounts");

    // Online accounts can be skipped without any private input.
    app.effect(&DestinationEffect::ContinueOffline).unwrap();
    assert_eq!(status(&mut app)["step"], "disk");
    assert!(
        app.effect(&DestinationEffect::ContinueOffline).is_err(),
        "the accounts step is behind the installer now"
    );

    // A disk is chosen on the disk itself, as the console names it.
    let disk = status(&mut app)["storage"]["disks"][1]["device"]
        .as_str()
        .unwrap()
        .to_owned();
    let select = DestinationEffect::SelectDisk {
        device: disk.clone(),
    };
    app.call_on(&select.published_at(), "select-disk", serde_json::json!({}))
        .unwrap();
    assert_eq!(status(&mut app)["storage"]["selected"], disk);
}

#[test]
fn begin_is_offered_only_on_the_review_and_asks_for_the_target() {
    let mut app = TestApp::<ImagerShell>::launched(&["--destination"]).unwrap();
    let begin = DestinationEffect::Begin {
        target: "/dev/sda".into(),
        allocation: String::new(),
    };
    assert!(
        app.effect(&begin).is_err(),
        "begin is never offered unbound, where its target could be typed"
    );
    assert!(
        app.call_on(&begin.published_at(), "begin", serde_json::json!({}))
            .is_err(),
        "nothing is reviewed yet"
    );
}
