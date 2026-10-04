use ui::headless::test_support::{
    assert_external_history_open, find_semantic, resource_snapshot, unversioned_driver,
};
use ui::history_mode::HistoryPresentation;
use ui::widget::Widget;

const EXPECTED_LIMITATION: &str = "Video Player does not retain versions of media or an edit journal; recent media is device-local viewing history. Restore is unavailable.";

fn controlled_root() -> Box<dyn Widget> {
    Box::new(crate::PlayerApp::history_test_root())
}

fn managed_driver_for(root: Box<dyn Widget>) -> ui::headless::Driver {
    unversioned_driver(ui::State::new(), root, (390, 750))
}

fn semantic_action_is_focused(driver: &ui::headless::Driver, action_target: &str) -> bool {
    find_semantic(driver, |node| {
        node.action_target.as_deref() == Some(action_target)
    })
    .expect("action target must be present in the accessibility tree")
    .state
    .focused
}

#[test]
fn controlled_real_root_declares_video_player_history() {
    let definition =
        ui::history_mode::capability::document_history_definition(controlled_root().as_ref())
            .expect("controlled real root declares framework History");
    assert_eq!(definition.app_id, "so.tilo.video-player");
    assert_eq!(definition.scopes.len(), 1);
    let scope = &definition.scopes[0];
    assert_eq!(scope.key, "media-versions");
    assert_eq!(scope.title, "Media versions (not recorded)");
    assert!(scope.content_schema.is_none());
    assert!(scope.presentation.preview_factory().is_none());
    let HistoryPresentation::External { reason } = scope.presentation else {
        panic!("media versions must be external and unversioned");
    };
    assert_eq!(reason, EXPECTED_LIMITATION);
}

#[test]
fn managed_history_is_truthful_and_returns_to_exact_local_media_state() {
    let mut driver = managed_driver_for(controlled_root());
    let live = driver.inspection_json().unwrap();
    let resources = resource_snapshot(&driver);
    let heads = driver.state_mut().doc.get_heads();
    assert!(live.contains("Hide library"));
    assert!(live.contains("Clear recent videos"));
    assert!(live.contains("Play Autumn reel"));
    assert!(live.contains("Play City study"));
    assert!(semantic_action_is_focused(
        &driver,
        "video-player.library.play.controlled-recent-city"
    ));
    assert!(resources.iter().any(|(id, label, _)| {
        id == "video-player.settings.current"
            && label == "Playback settings: 1×, autoplay off, repeat on"
    }));
    assert!(resources.iter().any(|(id, label, _)| {
        id == "video-player.media.current" && label == "Controlled history clip"
    }));
    assert!(resources.iter().any(|(id, label, _)| {
        id == "video-player.tracks.audio.736f757263652d37.617564696f2d31"
            && label == "Controlled stereo (selected)"
    }));

    driver.click_named("history-open").unwrap();
    assert_external_history_open(&driver, EXPECTED_LIMITATION);
    assert!(driver.targets().contains_key("history-open"));
    let history = driver.inspection_json().unwrap();
    assert!(!history.contains("Controlled history clip"));
    assert!(!history.contains("Autumn reel"));
    assert!(!history.contains("City study"));
    assert_eq!(driver.state_mut().doc.get_heads(), heads);

    driver.click_named("history-open").unwrap();
    let returned = driver.inspection_json().unwrap();
    assert!(returned.contains("Hide library"));
    assert!(returned.contains("Clear recent videos"));
    assert!(returned.contains("Play Autumn reel"));
    assert!(returned.contains("Play City study"));
    assert!(semantic_action_is_focused(
        &driver,
        "video-player.library.play.controlled-recent-city"
    ));
    assert_eq!(resource_snapshot(&driver), resources);
    assert_eq!(driver.state_mut().doc.get_heads(), heads);
}

#[test]
fn held_portal_result_survives_history_and_settles_once() {
    let (app, sender, observed_error) = crate::PlayerApp::history_test_with_pending_portal();
    let mut driver = managed_driver_for(Box::new(app));
    let resources = resource_snapshot(&driver);
    let heads = driver.state_mut().doc.get_heads();
    assert!(observed_error.borrow().is_none());

    driver.click_named("history-open").unwrap();
    assert!(
        observed_error.borrow().is_none(),
        "opening History itself does not settle the held request"
    );
    sender
        .send(ui::portal::PortalOutcome::Failed(
            "controlled portal failure".into(),
        ))
        .expect("History must not cancel the admitted portal request");
    assert!(
        observed_error.borrow().is_none(),
        "delivery alone does not manufacture authoritative app state"
    );
    driver.relayout();
    assert_eq!(
        observed_error.borrow().as_deref(),
        Some("controlled portal failure"),
        "live sync retains the exact admitted portal failure while History is open"
    );
    assert!(
        sender
            .send(ui::portal::PortalOutcome::Failed(
                "duplicate portal settlement".into(),
            ))
            .is_err(),
        "the admitted portal receiver settles exactly once"
    );
    assert_eq!(driver.state_mut().doc.get_heads(), heads);

    driver.click_named("history-open").unwrap();
    let returned = driver.inspection_json().unwrap();
    assert_eq!(
        observed_error.borrow().as_deref(),
        Some("controlled portal failure"),
        "returning to live keeps the exact original portal result"
    );
    assert!(returned.contains("Play Autumn reel"));
    assert!(returned.contains("Play City study"));
    assert_eq!(resource_snapshot(&driver), resources);
    assert_eq!(driver.state_mut().doc.get_heads(), heads);
}
