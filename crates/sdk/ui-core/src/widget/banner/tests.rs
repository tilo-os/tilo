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
    // Where it rests: the motion on the way is its own test.
    let _settled = crate::motion::testing::settled();
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

/// The first child of the harness's column, as it moves.
fn band(d: &FrameHarness) -> Option<crate::widget::Disclosing> {
    d.root_widget().children()[0].disclosing()
}

/// A band's full and drawn height.
fn band_heights(band: Option<crate::widget::Disclosing>) -> (f64, f64) {
    match band {
        Some(crate::widget::Disclosing::Band { extent, drawn }) => (extent, drawn),
        other => panic!("a banner on its way is a band, not {other:?}"),
    }
}

fn sync_banner(state: &crate::LocalState<Option<BannerNotice>>, dismissible: bool) -> Banner {
    let banner = Banner::from_state(state).with_id("sync");
    if dismissible {
        banner.dismissible()
    } else {
        banner
    }
}

fn sync_column(state: &crate::LocalState<Option<BannerNotice>>, dismissible: bool) -> Column {
    Column::new()
        .push(Box::new(sync_banner(state, dismissible)))
        .push(Box::new(crate::widget::Text::new("Below").with_id("below")))
}

fn sync_failed() -> BannerNotice {
    BannerNotice::error("Sync failed").message("We'll keep trying in the background.")
}

#[test]
fn an_arriving_banner_discloses_over_what_follows_laid_out_where_it_stood() {
    let state = crate::LocalState::new(None::<BannerNotice>);
    let mut d = driver(Box::new(sync_column(&state, false)));
    let empty_y = d.targets()["below"].y0;
    // Nobody did anything: the window went offline. The frame's instant is
    // held, so the test reads the motion where it starts.
    let frame = crate::motion::pin_frame();
    state.set(Some(sync_failed()));
    d.relayout();
    d.relayout();
    assert!(
        (d.targets()["below"].y0 - empty_y).abs() < 0.5,
        "what follows keeps its place, so nothing under it is left bare"
    );
    let (extent, drawn) = band_heights(band(&d));
    assert!(extent > 30.0, "the banner is drawn at its full height");
    assert!(drawn.abs() < 0.5, "and opens from nothing");
    assert!(d.root_widget().needs_animation_frame());
    assert!(
        !d.root_widget().is_layout_dirty(),
        "a frame of the motion lays nothing out"
    );
    drop(frame);

    // At rest the layout takes the banner in, once; then nothing is asked.
    let _settled = crate::motion::testing::settled();
    assert!(band(&d).is_none());
    assert!(d.root_widget().is_layout_dirty());
    d.relayout();
    let shown_y = d.targets()["below"].y0;
    assert!(shown_y >= empty_y + extent - 0.5);
    assert!(!d.root_widget().needs_animation_frame());
    assert!(!d.root_widget().is_layout_dirty());
}

#[test]
fn a_banner_dismissed_by_pointer_collapses_and_by_key_lands() {
    let state = crate::LocalState::new(Some(sync_failed()));
    let mut d = driver(Box::new(sync_column(&state, true)));
    assert!(band(&d).is_none(), "first paint shows the banner at rest");
    let shown_y = d.targets()["below"].y0;
    let dismiss = d.targets()["sync.dismiss"];

    let frame = crate::motion::pin_frame();
    d.click_at(dismiss.center().x, dismiss.center().y);
    d.relayout();
    let empty_y = d.targets()["below"].y0;
    assert!(
        empty_y < shown_y - 30.0,
        "what follows is laid out once where it rests, with more room"
    );
    let moving = band(&d).expect("a click collapses it");
    let (extent, drawn) = band_heights(Some(moving));
    assert!(
        (drawn - extent).abs() < 0.5,
        "it closes from its full height"
    );
    let gap = shown_y - empty_y - extent;
    assert!(
        (empty_y + moving.follower_offset(gap) - shown_y).abs() < 0.5,
        "what follows starts where it stood"
    );
    drop(frame);

    // Shown again and hidden by a key: it lands.
    let mut d = driver(Box::new(sync_column(&state, false)));
    state.set(Some(sync_failed()));
    d.relayout();
    crate::motion::note_source(crate::tokens::MotionSource::Keyboard);
    state.set(None);
    d.relayout();
    assert!(band(&d).is_none(), "a key's result lands");
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
    assert!(
        d.targets()["below"].y0 > empty_y + 30.0,
        "the height lands at once"
    );
    assert!(band(&d).is_none(), "nothing below moves");
    assert!(
        d.root_widget().needs_animation_frame(),
        "its contents fade in"
    );
}

#[test]
fn a_banner_in_a_section_or_a_container_moves_what_follows_the_group() {
    for nest in ["section", "container"] {
        let state = crate::LocalState::new(None::<BannerNotice>);
        // Each nest its own banner: the one the last nest dropped is not
        // this one coming back.
        let banner: Box<dyn Widget> =
            Box::new(Banner::from_state(&state).with_id(format!("sync-{nest}")));
        let group: Box<dyn Widget> = if nest == "section" {
            Box::new(crate::widget::Section::new(vec![
                banner,
                Box::new(crate::widget::Text::new("Inside").with_id("inside")),
            ]))
        } else {
            Box::new(crate::widget::Container::new().with_child(banner))
        };
        let root = Column::new()
            .with_intrinsic_height()
            .push(group)
            .push(Box::new(crate::widget::Text::new("After").with_id("after")));
        let mut d = driver(Box::new(root));
        let empty_after = d.targets()["after"].y0;

        let frame = crate::motion::pin_frame();
        state.set(Some(sync_failed()));
        d.relayout();
        d.relayout();
        assert!(
            (d.targets()["after"].y0 - empty_after).abs() < 0.5,
            "{nest}: what follows the group keeps its place"
        );
        let group = d.root_widget().children()[0].disclosing();
        assert!(
            matches!(group, Some(crate::widget::Disclosing::Grows(by)) if by.abs() < 0.5),
            "{nest}: the group grows with the band, from nothing: {group:?}"
        );
        assert!(
            matches!(
                d.root_widget().disclosing(),
                Some(crate::widget::Disclosing::Grows(_))
            ),
            "{nest}: and so does a content-sized column around it"
        );
        drop(frame);

        let _settled = crate::motion::testing::settled();
        assert!(d.root_widget().children()[0].disclosing().is_none());
        d.relayout();
        assert!(
            d.targets()["after"].y0 > empty_after + 30.0,
            "{nest}: at rest the layout takes the banner in"
        );
        assert!(!d.root_widget().needs_animation_frame());
    }
}

fn rooted(column: Column) -> FrameHarness {
    let mut d = FrameHarness::for_root(State::new(), column, (640, 400), Theme::dark());
    d.relayout();
    d
}

/// A view that builds its banner anew on every change, as most apps do.
fn rebuild(d: &mut FrameHarness, banner: Option<Banner>) {
    let mut children: Vec<Box<dyn Widget>> = Vec::new();
    if let Some(banner) = banner {
        children.push(Box::new(banner));
    }
    children.push(Box::new(crate::widget::Text::new("Below").with_id("below")));
    d.root_mut::<Column>().set_children(children);
    d.relayout();
}

#[test]
fn a_banner_its_view_builds_again_goes_on_from_the_one_it_replaces() {
    let mut d = rooted(Column::new());
    rebuild(&mut d, None);
    let empty_y = d.targets()["below"].y0;
    let frame = crate::motion::pin_frame();
    rebuild(&mut d, Some(Banner::error("Sync failed")));
    assert!(band(&d).is_some(), "a banner new to the view discloses");
    assert!((d.targets()["below"].y0 - empty_y).abs() < 0.5);
    rebuild(&mut d, Some(Banner::error("Sync failed")));
    assert!(
        band(&d).is_some(),
        "built again mid-motion, it goes on opening"
    );
    assert!((d.targets()["below"].y0 - empty_y).abs() < 0.5);
    drop(frame);

    // A banner there from the first frame, built again at rest.
    let mut d = rooted(
        Column::new()
            .push(Box::new(Banner::warning("Offline")))
            .push(Box::new(crate::widget::Text::new("Below").with_id("below"))),
    );
    let shown_y = d.targets()["below"].y0;
    let _frame = crate::motion::pin_frame();
    rebuild(&mut d, Some(Banner::warning("Offline")));
    assert!(band(&d).is_none(), "built again at rest, it stays at rest");
    assert!(!d.root_widget().needs_animation_frame());
    assert!((d.targets()["below"].y0 - shown_y).abs() < 0.5);
    rebuild(&mut d, Some(Banner::warning("Offline, retrying")));
    assert!(
        band(&d).is_none() && !d.root_widget().needs_animation_frame(),
        "a banner that says something new in the same place does not arrive again"
    );
}

/// The column's offsets: how far its first child is drawn below its place.
fn first_offset(d: &FrameHarness) -> f64 {
    d.root::<Column>()
        .drawn_offsets()
        .and_then(|offsets| offsets.first().copied())
        .unwrap_or(0.0)
}

fn offline_view() -> Column {
    let dismissed = Rc::new(Cell::new(0));
    let seen = dismissed.clone();
    Column::new()
        .push(Box::new(
            Banner::warning("Offline")
                .with_id("offline")
                .dismissible()
                .on_dismiss(move || seen.set(seen.get() + 1)),
        ))
        .push(Box::new(crate::widget::Text::new("Below").with_id("below")))
}

#[test]
fn a_banner_its_view_stops_building_collapses_and_what_follows_moves_up() {
    use crate::time::testing::with_monotonic_time;
    let start = crate::time::now();
    let mut d = with_monotonic_time(start, || rooted(offline_view()));
    let shown_y = d.targets()["below"].y0;
    let dismiss = d.targets()["offline.dismiss"];

    // Back online: the view builds no banner.
    with_monotonic_time(start, || {
        crate::motion::note_source(crate::tokens::MotionSource::Pointer);
        rebuild(&mut d, None);
    });
    let empty_y = d.targets()["below"].y0;
    assert!(
        empty_y < shown_y - 30.0,
        "what follows is laid out once where it rests"
    );
    assert!(
        (empty_y + with_monotonic_time(start, || first_offset(&d)) - shown_y).abs() < 0.5,
        "and is drawn where it stood"
    );
    // It moves up continuously, by transform, with the ghost's edge.
    let mut last = shown_y - empty_y;
    for ms in [40, 80, 120, 160] {
        let at = start + crate::time::Duration::from_millis(ms);
        let offset = with_monotonic_time(at, || first_offset(&d));
        assert!(offset < last + 0.01, "at {ms} ms it only moves up");
        last = offset;
        assert!(with_monotonic_time(at, || d
            .root_widget()
            .needs_animation_frame()));
    }
    assert!(last < shown_y - empty_y - 1.0, "it has moved");

    // The ghost takes no input: its close button is gone with it.
    with_monotonic_time(start, || {
        d.click_at(dismiss.center().x, dismiss.center().y);
    });
    assert!(!d.targets().contains_key("offline.dismiss"));

    // Gone, it asks for one frame to draw it gone, then nothing.
    let later = start + crate::time::Duration::from_secs(2);
    with_monotonic_time(later, || {
        assert!(first_offset(&d).abs() < 0.01);
        d.relayout();
        assert!(d.root::<Column>().drawn_offsets().is_none());
        assert!(!d.root_widget().needs_animation_frame());
    });
}

#[test]
fn a_banner_leaving_lands_by_key_and_fades_under_reduced_motion() {
    use crate::time::testing::with_monotonic_time;
    let start = crate::time::now();
    let mut d = with_monotonic_time(start, || rooted(offline_view()));
    with_monotonic_time(start, || {
        crate::motion::note_source(crate::tokens::MotionSource::Keyboard);
        rebuild(&mut d, None);
        assert!(
            d.root::<Column>().drawn_offsets().is_none(),
            "a key's result lands"
        );
    });

    let mut d = with_monotonic_time(start, || rooted(offline_view()));
    d.set_theme(Theme::dark().with_reduce_motion(true));
    with_monotonic_time(start, || {
        d.relayout();
        crate::motion::note_source(crate::tokens::MotionSource::Pointer);
        rebuild(&mut d, None);
        assert!(first_offset(&d).abs() < 0.01, "the height lands");
        assert!(
            d.root_widget().needs_animation_frame(),
            "and the paint fades"
        );
    });
}

#[test]
fn nothing_leaves_when_the_whole_view_is_replaced() {
    use crate::time::testing::with_monotonic_time;
    let start = crate::time::now();
    let mut d = with_monotonic_time(start, || rooted(offline_view()));
    with_monotonic_time(start, || {
        crate::motion::note_source(crate::tokens::MotionSource::Pointer);
        d.root_mut::<Column>().set_children(vec![
            Box::new(crate::widget::Spacer::new()),
            Box::new(crate::widget::Text::new("Another page")),
            Box::new(crate::widget::Text::new("Below").with_id("below")),
        ]);
        d.relayout();
        assert!(d.root::<Column>().drawn_offsets().is_none());
        assert!(!d.root_widget().needs_animation_frame());
    });
}

/// How far the column's child `index` is drawn below its place.
fn offset_of(d: &FrameHarness, index: usize) -> f64 {
    d.root::<Column>()
        .drawn_offsets()
        .and_then(|offsets| offsets.get(index).copied())
        .unwrap_or(0.0)
}

fn grouped(nest: &str, banner: Option<Banner>) -> Vec<Box<dyn Widget>> {
    let group: Box<dyn Widget> = if nest == "section" {
        let mut items: Vec<Box<dyn Widget>> = Vec::new();
        if let Some(banner) = banner {
            items.push(Box::new(banner));
        }
        items.push(Box::new(
            crate::widget::Text::new("Inside").with_id("inside"),
        ));
        Box::new(crate::widget::Section::new(items))
    } else {
        let container = crate::widget::Container::new();
        Box::new(match banner {
            Some(banner) => container.with_child(Box::new(banner)),
            None => container,
        })
    };
    vec![
        group,
        Box::new(crate::widget::Text::new("After").with_id("after")),
    ]
}

#[test]
fn a_banner_its_view_stops_building_in_a_section_or_a_container_leaves_too() {
    use crate::time::testing::with_monotonic_time;
    for nest in ["section", "container"] {
        let start = crate::time::now();
        let mut column = Column::new().with_intrinsic_height();
        column.set_children(grouped(
            nest,
            Some(Banner::warning(format!("Offline {nest}"))),
        ));
        let mut d = with_monotonic_time(start, || rooted(column));
        let shown_after = d.targets()["after"].y0;

        with_monotonic_time(start, || {
            crate::motion::note_source(crate::tokens::MotionSource::Pointer);
            d.root_mut::<Column>().set_children(grouped(nest, None));
            d.relayout();
        });
        let empty_after = d.targets()["after"].y0;
        assert!(
            empty_after < shown_after - 30.0,
            "{nest}: laid out once where it rests"
        );
        let at_start = with_monotonic_time(start, || offset_of(&d, 1));
        assert!(
            (empty_after + at_start - shown_after).abs() < 0.5,
            "{nest}: what follows the group is drawn where it stood"
        );
        let midway = with_monotonic_time(start + crate::time::Duration::from_millis(50), || {
            offset_of(&d, 1)
        });
        assert!(
            midway > 0.5 && midway < at_start - 0.5,
            "{nest}: and moves up with the banner's edge: {midway} of {at_start}"
        );
        if nest == "section" {
            let inside = d.targets()["inside"].y0;
            assert!(inside < d.targets()["after"].y0);
        }

        let later = start + crate::time::Duration::from_secs(2);
        with_monotonic_time(later, || {
            d.relayout();
            assert!(d.root::<Column>().drawn_offsets().is_none());
            assert!(
                !d.root_widget().needs_animation_frame(),
                "{nest}: it settles"
            );
        });
    }
}

#[test]
fn a_banner_dropped_while_opening_leaves_from_where_it_is_drawn() {
    use crate::time::testing::with_monotonic_time;
    let start = crate::time::now();
    let mut d = with_monotonic_time(start, || rooted(Column::new()));
    with_monotonic_time(start, || rebuild(&mut d, None));
    let empty_y = d.targets()["below"].y0;
    with_monotonic_time(start, || {
        crate::motion::note_source(crate::tokens::MotionSource::Pointer);
        rebuild(&mut d, Some(Banner::warning("Reconnecting")));
    });
    let midway = start + crate::time::Duration::from_millis(90);
    let opened = with_monotonic_time(midway, || {
        d.relayout();
        offset_of(&d, 1)
    });
    assert!(opened > 1.0, "it is on its way in: {opened}");

    // Its view drops it mid-way: it leaves from there, with its paint.
    with_monotonic_time(midway, || rebuild(&mut d, None));
    assert!((d.targets()["below"].y0 - empty_y).abs() < 0.5);
    let leaving = with_monotonic_time(midway, || first_offset(&d));
    assert!(
        (leaving - opened).abs() < 1.0,
        "what follows goes on from where it was drawn: {leaving} against {opened}"
    );
    let later = with_monotonic_time(midway + crate::time::Duration::from_millis(60), || {
        first_offset(&d)
    });
    assert!(later < leaving, "and moves up");
    with_monotonic_time(start + crate::time::Duration::from_secs(2), || {
        d.relayout();
        assert!(!d.root_widget().needs_animation_frame());
    });
}
