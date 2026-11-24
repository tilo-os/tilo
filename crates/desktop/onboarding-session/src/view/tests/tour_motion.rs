//! A tour restored after a restart under reduced motion comes to rest: the page it opens on
//! does not replay the entrance the tour plays for a page it moves to itself.

use super::*;

#[test]
fn a_restored_tour_rests_under_reduced_motion() {
    let directory = TempDir::new().expect("identity directory");
    write_fixture_identity(directory.path());
    let device = Device::new();
    device.reached(Stage::TourOrganise);
    let mut view = device.restarted(directory.path());

    assert_eq!(view.tour_page, Some(TourPage::Organise));
    assert!(
        view.take_focus_reset(),
        "a restored page must place keyboard focus inside its rebuilt controls"
    );
    assert!(!view.take_focus_reset(), "the focus edge is one-shot");

    let theme = Theme::dark().with_reduce_motion(true);
    // A host draws the page until it rests; an entrance animation would keep asking for more.
    let _ = lay_out_as_root(&mut view, 1280.0, 800.0, &theme);
    assert!(
        !view.needs_animation_frame(),
        "restoring a tour must not restart entrance animation under reduced motion"
    );

    let mut document = AutoCommit::new();
    press(&view, &mut document, KEY_TOUR_NEXT);
    sync_settled(&mut view, &document);
    assert_eq!(view.tour_page, Some(TourPage::Collaborate));
    assert_eq!(device.step(), Some(Stage::TourCollaborate));
    assert!(
        view.take_focus_reset(),
        "keyboard navigation must move focus into the newly restored stage"
    );
    let _ = lay_out_as_root(&mut view, 1280.0, 800.0, &theme);
    assert!(
        !view.needs_animation_frame(),
        "the next stage arrives in one frame under reduced motion"
    );
}
