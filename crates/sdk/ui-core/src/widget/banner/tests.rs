use super::*;
use crate::headless::FrameHarness;
use crate::state::State;
use crate::widget::Column;
use std::cell::Cell;
use std::rc::Rc;

fn content_hits(content: &BannerContent<'_>, width: f64) -> (f64, BannerHit) {
    let theme = Theme::dark();
    let mut text = TextRenderer::new();
    let height = Banner::height(content, width, &mut text, &theme);
    let mut scene = Scene::new();
    let rect = Rect::new(0.0, 0.0, width, height);
    let hit = Banner::paint(
        &mut PaintCtx::new(&mut scene, Affine::IDENTITY, &mut text, &theme),
        rect,
        content,
        Point::new(-1.0, -1.0),
    );
    (height, hit)
}

#[test]
fn kinds_pick_their_icon_and_ink() {
    let theme = Theme::dark();
    assert_eq!(
        BannerKind::Error.color(&theme),
        theme.tokens.colors.semantic.danger
    );
    assert_eq!(
        BannerKind::Warning.color(&theme),
        theme.tokens.colors.semantic.warning
    );
    assert_eq!(
        BannerKind::Neutral.color(&theme),
        theme.text_primary,
        "a neutral notice has no hue"
    );
    assert_eq!(BannerKind::Success.default_icon(), Icon::Check);
    assert_eq!(
        Banner::info("Heads up").with_icon(Icon::Bell).icon(),
        Icon::Bell
    );
    assert_eq!(Banner::info("Heads up").icon(), Icon::Info);
}

#[test]
fn actions_sit_inside_the_box_and_wrap_under_the_message_when_narrow() {
    let content = BannerContent::new(BannerKind::Error, "Couldn't update the forecast")
        .with_message(Some("Check your connection and try again."))
        .action("Try again")
        .secondary_action("Details")
        .dismissible(true);
    let (wide_h, wide) = content_hits(&content, 900.0);
    let action = wide.action.expect("the action is laid out");
    let secondary = wide
        .secondary_action
        .expect("the second action is laid out");
    let dismiss = wide.dismiss.expect("the dismiss control is laid out");
    for r in [action, secondary, dismiss] {
        assert!(
            wide.rect.contains(r.origin()) && r.x1 <= wide.rect.x1 && r.y1 <= wide.rect.y1,
            "{r:?} leaves the banner {:?}",
            wide.rect
        );
    }
    assert!(secondary.x1 < action.x0, "the main action is at the edge");
    assert!(action.x1 < dismiss.x0, "dismiss stays trailing");
    assert!(
        (action.center().y - wide.rect.center().y).abs() < 1.0,
        "beside the text the actions sit on the banner's centre line"
    );

    let (narrow_h, narrow) = content_hits(&content, 360.0);
    let action = narrow.action.unwrap();
    assert!(narrow_h > wide_h, "wrapped actions add a row");
    assert!(
        action.y0 > narrow.rect.center().y,
        "a narrow banner puts its actions under the message"
    );
    assert!(action.y1 <= narrow.rect.y1, "and still inside its box");
}

#[test]
fn height_grows_with_text_scale_and_long_messages() {
    let theme = Theme::dark();
    let mut text = TextRenderer::new();
    let message = "Tilo found your identity metadata, but its private key is unavailable from the desktop keyring. Unlock the keyring and retry.";
    let short = BannerContent::new(BannerKind::Warning, "Your identity needs recovery");
    let long = short.with_message(Some(message));
    let short_h = Banner::height(&short, 600.0, &mut text, &theme);
    let long_h = Banner::height(&long, 600.0, &mut text, &theme);
    assert!(long_h > short_h);
    assert!(short_h >= f64::from(theme.tokens.metrics.panel_row_height));
    let scaled = Banner::height(&long, 600.0, &mut text, &theme.clone().with_text_scale(2.0));
    assert!(scaled > long_h);
}

fn driver(root: Box<dyn Widget>) -> FrameHarness {
    let mut driver = FrameHarness::new(State::new(), root, (640, 400));
    driver.relayout();
    driver
}

#[test]
fn the_retained_banner_holds_its_buttons_and_dismisses_through_its_control() {
    let dismissed = Rc::new(Cell::new(0));
    let seen = dismissed.clone();
    let retried = Rc::new(Cell::new(0));
    let retry = retried.clone();
    let root = Column::new()
        .push(Box::new(
            Banner::warning("You're offline")
                .with_message("Changes will sync when you reconnect.")
                .with_id("offline")
                .with_action(
                    Button::new("offline.retry", "Try again").on_effect(move || {
                        retry.set(retry.get() + 1);
                    }),
                )
                .on_dismiss(move || seen.set(seen.get() + 1)),
        ))
        .push(Box::new(crate::widget::Text::new("Below").with_id("below")));
    let mut d = driver(Box::new(root));
    let targets = d.targets();
    let banner = targets["offline"];
    let retry_rect = targets["offline.retry"];
    let dismiss_rect = targets["offline.dismiss"];
    assert!(
        banner.contains(retry_rect.center()),
        "the action is inside the box"
    );
    assert!(banner.contains(dismiss_rect.center()));
    let below_before = targets["below"].y0;
    assert!(
        d.native_app_audit().is_certified(),
        "{:?}",
        d.native_app_audit()
    );

    d.perform_named("offline.retry", &crate::a11y::Action::Activate)
        .expect("the action is addressable");
    assert_eq!(retried.get(), 1);

    d.perform_named("offline.dismiss", &crate::a11y::Action::Activate)
        .expect("the dismiss control is addressable");
    d.relayout();
    assert_eq!(dismissed.get(), 1, "on_dismiss runs once");
    let targets = d.targets();
    assert!(
        !targets.contains_key("offline") || targets["offline"].is_zero_area(),
        "a dismissed banner collapses"
    );
    assert!(
        targets["below"].y0 < below_before - banner.height() + 0.5,
        "and gives back its height and its gap"
    );
}

#[test]
fn a_state_bound_banner_collapses_while_absent() {
    let state = crate::LocalState::new(None::<BannerNotice>);
    let root = Column::new()
        .push(Box::new(Banner::from_state(&state).with_id("sync")))
        .push(Box::new(crate::widget::Text::new("Below").with_id("below")));
    let mut d = driver(Box::new(root));
    let empty_y = d.targets()["below"].y0;
    state.set(Some(
        BannerNotice::error("Sync failed").message("We'll keep trying in the background."),
    ));
    d.relayout();
    let shown_y = d.targets()["below"].y0;
    assert!(
        shown_y > empty_y + 30.0,
        "the notice takes room: {empty_y} -> {shown_y}"
    );
    state.set(None);
    d.relayout();
    assert!((d.targets()["below"].y0 - empty_y).abs() < 0.5);
}

#[test]
fn the_banner_is_a_live_region_by_severity() {
    use crate::a11y::{Politeness, Role};
    let err = Banner::error("Boom").with_message("details");
    let node = Widget::accessibility(&err).expect("banner exposes a11y");
    assert_eq!(node.role, Role::Group);
    assert_eq!(node.name.as_deref(), Some("Boom"));
    assert_eq!(node.live, Some(Politeness::Assertive));
    let info = Banner::neutral("Private window");
    assert_eq!(
        Widget::accessibility(&info).unwrap().live,
        Some(Politeness::Polite)
    );
}

/// The banner (first) and what follows it in the harness's column.
fn band_and_shift(d: &FrameHarness, span: f64) -> (Option<crate::widget::Disclosing>, f64) {
    let children = d.root_widget().children();
    let band = children[0].disclosing();
    let shift = band.map_or(0.0, |band| band.follower_offset(span - band.extent));
    (band, shift)
}

fn sync_column(state: &crate::LocalState<Option<BannerNotice>>, dismissible: bool) -> Column {
    let banner = Banner::from_state(state).with_id("sync");
    let banner = if dismissible {
        banner.dismissible()
    } else {
        banner
    };
    Column::new()
        .push(Box::new(banner))
        .push(Box::new(crate::widget::Text::new("Below").with_id("below")))
}

fn sync_failed() -> BannerNotice {
    BannerNotice::error("Sync failed").message("We'll keep trying in the background.")
}

#[test]
fn an_arriving_banner_discloses_and_what_follows_moves_with_its_edge() {
    let state = crate::LocalState::new(None::<BannerNotice>);
    let mut d = driver(Box::new(sync_column(&state, false)));
    let empty_y = d.targets()["below"].y0;
    // Nobody did anything: the window went offline. The frame's instant is
    // held, so the test reads the motion where it starts.
    let _frame = crate::motion::pin_frame();
    state.set(Some(sync_failed()));
    d.relayout();
    let shown_y = d.targets()["below"].y0;
    assert!(
        shown_y > empty_y + 30.0,
        "the layout gives the banner its room at once"
    );
    let (band, shift) = band_and_shift(&d, shown_y - empty_y);
    let band = band.expect("an unprompted banner still discloses");
    assert!(band.laid && band.drawn.abs() < 0.5, "it opens from nothing");
    assert!(
        (shown_y + shift - empty_y).abs() < 0.5,
        "what follows is drawn where it stood and moves by transform"
    );
    assert!(d.root_widget().needs_animation_frame());

    // At rest it asks for nothing and nothing is shifted.
    let _settled = crate::motion::testing::settled();
    let (band, _) = band_and_shift(&d, shown_y - empty_y);
    assert!(band.is_none());
    assert!(!d.root_widget().needs_animation_frame());
}

#[test]
fn a_banner_dismissed_by_pointer_collapses_and_by_key_lands() {
    let state = crate::LocalState::new(Some(sync_failed()));
    let mut d = driver(Box::new(sync_column(&state, true)));
    let (band, _) = band_and_shift(&d, 0.0);
    assert!(band.is_none(), "first paint shows the banner at rest");
    let shown_y = d.targets()["below"].y0;
    let dismiss = d.targets()["sync.dismiss"];

    let _frame = crate::motion::pin_frame();
    d.click_at(dismiss.center().x, dismiss.center().y);
    d.relayout();
    let empty_y = d.targets()["below"].y0;
    assert!(empty_y < shown_y - 30.0, "the layout closes at once");
    let (band, shift) = band_and_shift(&d, shown_y - empty_y);
    let band = band.expect("a click collapses it");
    assert!(!band.laid);
    assert!(
        (empty_y + shift - shown_y).abs() < 0.5,
        "what follows starts where it stood"
    );
    drop(_frame);

    // Shown again and hidden by a key: it lands.
    let mut d = driver(Box::new(sync_column(&state, false)));
    state.set(Some(sync_failed()));
    d.relayout();
    crate::motion::note_source(crate::tokens::MotionSource::Keyboard);
    state.set(None);
    d.relayout();
    assert!(band_and_shift(&d, 0.0).0.is_none(), "a key's result lands");
}

#[test]
fn under_reduced_motion_a_banner_lands_and_its_contents_fade_in() {
    let state = crate::LocalState::new(None::<BannerNotice>);
    let mut d = driver(Box::new(sync_column(&state, false)));
    d.set_theme(Theme::dark().with_reduce_motion(true));
    let empty_y = d.targets()["below"].y0;
    let _frame = crate::motion::pin_frame();
    state.set(Some(sync_failed()));
    d.relayout();
    let shown_y = d.targets()["below"].y0;
    let (band, shift) = band_and_shift(&d, shown_y - empty_y);
    let band = band.expect("its contents fade in");
    assert!(
        (band.drawn - band.extent).abs() < 0.5,
        "the height lands at once"
    );
    assert!(shift.abs() < 0.5, "nothing below moves");
}
