//! The banner: its parts and wrapping, its live region and touch targets, its
//! placement, and a banner arriving, leaving and built again.

use std::cell::*;
use std::collections::*;
use std::path::*;
use std::rc::*;
use std::sync::atomic::*;
use std::sync::*;
use std::time::*;
use tilo_ui_app::app::driver::Driver;
use tilo_ui_app::headless::*;
use tilo_ui_app::kurbo::*;
use tilo_ui_app::retained_present::*;
use tilo_ui_app::testing::*;
use tilo_ui_app::vello::*;
use tilo_ui_app::widget::*;
use tilo_ui_base::a11y::*;
use tilo_ui_base::component::*;
use tilo_ui_base::i18n::*;
use tilo_ui_base::icons::*;
use tilo_ui_base::interaction::*;
use tilo_ui_base::motion::*;
use tilo_ui_base::offscreen::*;
use tilo_ui_base::paint::*;
use tilo_ui_base::peniko::*;
use tilo_ui_base::presence::*;
use tilo_ui_base::synthetic_input::*;
use tilo_ui_base::text_render::*;
use tilo_ui_base::text_selection::*;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::*;
use tilo_ui_base::widget::*;
use winit::event::*;
use winit::keyboard::*;

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "in a test these are the assertion"
)]
fn driver(root: Box<dyn Widget>) -> Driver {
    let mut driver = Driver::new(tilo_ui_app::State::new(), root, (640, 400));
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
                .with_action(Button::new("offline.retry", "Try again").on_click(move || {
                    retry.set(retry.get() + 1);
                }))
                .on_dismiss(move || seen.set(seen.get() + 1)),
        ))
        .push(Box::new(
            tilo_ui_app::widget::Text::new("Below").with_id("below"),
        ));
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

    d.perform_named("offline.retry", &tilo_ui_base::a11y::Action::Activate)
        .expect("the action is addressable");
    assert_eq!(retried.get(), 1);

    d.perform_named("offline.dismiss", &tilo_ui_base::a11y::Action::Activate)
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
    let _settled = tilo_ui_base::motion::testing::settled();
    let state = tilo_ui_base::LocalState::new(None::<BannerNotice>);
    let root = Column::new()
        .push(Box::new(Banner::from_state(&state).with_id("sync")))
        .push(Box::new(
            tilo_ui_app::widget::Text::new("Below").with_id("below"),
        ));
    let mut d = driver(Box::new(root));
    let empty_y = d.targets()["below"].y0;
    state.set(Some(
        BannerNotice::error("Sync failed").message("We'll keep trying in the background."),
    ));
    d.relayout();
    let shown_y = d.targets()["below"].y0;
    assert!(
        shown_y > empty_y + 30.0,
        "the notice takes area: {empty_y} -> {shown_y}"
    );
    state.set(None);
    d.relayout();
    assert!((d.targets()["below"].y0 - empty_y).abs() < 0.5);
}

/// The first child of the driver's column, as it moves.
fn band(d: &Driver) -> Option<tilo_ui_base::widget::Disclosing> {
    d.root_widget().children()[0].disclosing()
}

/// A band's full and drawn height.
fn band_heights(band: Option<tilo_ui_base::widget::Disclosing>) -> (f64, f64) {
    match band {
        Some(tilo_ui_base::widget::Disclosing::Band { extent, drawn }) => (extent, drawn),
        other => panic!("a banner on its way is a band, not {other:?}"),
    }
}

fn sync_banner(
    state: &tilo_ui_base::LocalState<Option<BannerNotice>>,
    dismissible: bool,
) -> Banner {
    let banner = Banner::from_state(state).with_id("sync");
    if dismissible {
        banner.dismissible()
    } else {
        banner
    }
}

fn sync_column(
    state: &tilo_ui_base::LocalState<Option<BannerNotice>>,
    dismissible: bool,
) -> Column {
    Column::new()
        .push(Box::new(sync_banner(state, dismissible)))
        .push(Box::new(
            tilo_ui_app::widget::Text::new("Below").with_id("below"),
        ))
}

fn sync_failed() -> BannerNotice {
    BannerNotice::error("Sync failed").message("We'll keep trying in the background.")
}

#[test]
fn an_arriving_banner_discloses_over_what_follows_laid_out_where_it_stood() {
    let state = tilo_ui_base::LocalState::new(None::<BannerNotice>);
    let mut d = driver(Box::new(sync_column(&state, false)));
    let empty_y = d.targets()["below"].y0;
    // Nobody did anything: the window went offline. The frame's instant is
    // held, so the test reads the motion where it starts.
    let frame = tilo_ui_base::motion::pin_frame();
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
    let _settled = tilo_ui_base::motion::testing::settled();
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
    let state = tilo_ui_base::LocalState::new(Some(sync_failed()));
    let mut d = driver(Box::new(sync_column(&state, true)));
    assert!(band(&d).is_none(), "first paint shows the banner at rest");
    let shown_y = d.targets()["below"].y0;
    let dismiss = d.targets()["sync.dismiss"];

    let frame = tilo_ui_base::motion::pin_frame();
    d.click_at(dismiss.center().x, dismiss.center().y);
    d.relayout();
    let empty_y = d.targets()["below"].y0;
    assert!(
        empty_y < shown_y - 30.0,
        "what follows is laid out once where it rests, with more area"
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
    tilo_ui_base::motion::note_source(tilo_ui_base::tokens::MotionSource::Keyboard);
    state.set(None);
    d.relayout();
    assert!(band(&d).is_none(), "a key's result lands");
}

#[test]
fn a_banner_in_a_section_or_a_container_moves_what_follows_the_group() {
    for nest in ["section", "container"] {
        let state = tilo_ui_base::LocalState::new(None::<BannerNotice>);
        // Each nest its own banner: the one the last nest dropped is not
        // this one coming back.
        let banner: Box<dyn Widget> =
            Box::new(Banner::from_state(&state).with_id(format!("sync-{nest}")));
        let group: Box<dyn Widget> = if nest == "section" {
            Box::new(tilo_ui_app::widget::Section::new(vec![
                banner,
                Box::new(tilo_ui_app::widget::Text::new("Inside").with_id("inside")),
            ]))
        } else {
            Box::new(tilo_ui_app::widget::Container::new().with_child(banner))
        };
        let root = Column::new()
            .with_intrinsic_height()
            .push(group)
            .push(Box::new(
                tilo_ui_app::widget::Text::new("After").with_id("after"),
            ));
        let mut d = driver(Box::new(root));
        let empty_after = d.targets()["after"].y0;

        let frame = tilo_ui_base::motion::pin_frame();
        state.set(Some(sync_failed()));
        d.relayout();
        d.relayout();
        assert!(
            (d.targets()["after"].y0 - empty_after).abs() < 0.5,
            "{nest}: what follows the group keeps its place"
        );
        let group = d.root_widget().children()[0].disclosing();
        assert!(
            matches!(group, Some(tilo_ui_base::widget::Disclosing::Grows { by, .. }) if by.abs() < 0.5),
            "{nest}: the group grows with the band, from nothing: {group:?}"
        );
        assert!(
            matches!(
                d.root_widget().disclosing(),
                Some(tilo_ui_base::widget::Disclosing::Grows { .. })
            ),
            "{nest}: and so does a content-sized column around it"
        );
        drop(frame);

        let _settled = tilo_ui_base::motion::testing::settled();
        assert!(d.root_widget().children()[0].disclosing().is_none());
        d.relayout();
        assert!(
            d.targets()["after"].y0 > empty_after + 30.0,
            "{nest}: at rest the layout takes the banner in"
        );
        assert!(!d.root_widget().needs_animation_frame());
    }
}

fn rooted(column: Column) -> Driver {
    let mut d = Driver::for_root(tilo_ui_app::State::new(), column, (640, 400), Theme::dark());
    d.relayout();
    d
}

/// A view that builds its banner anew on every change, as most apps do.
fn rebuild(d: &mut Driver, banner: Option<Banner>) {
    let mut children: Vec<Box<dyn Widget>> = Vec::new();
    if let Some(banner) = banner {
        children.push(Box::new(banner));
    }
    children.push(Box::new(
        tilo_ui_app::widget::Text::new("Below").with_id("below"),
    ));
    d.root_mut::<Column>().set_children(children);
    d.relayout();
}

/// The column's offsets: how far its first child is drawn below its place.
fn first_offset(d: &Driver) -> f64 {
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
        .push(Box::new(
            tilo_ui_app::widget::Text::new("Below").with_id("below"),
        ))
}

#[test]
fn a_banner_its_view_stops_building_collapses_and_what_follows_moves_up() {
    use tilo_ui_app::time::testing::with_monotonic_time;
    let start = std::time::Instant::now();
    let mut d = with_monotonic_time(start, || rooted(offline_view()));
    let shown_y = d.targets()["below"].y0;
    let dismiss = d.targets()["offline.dismiss"];

    // Back online: the view builds no banner.
    with_monotonic_time(start, || {
        tilo_ui_base::motion::note_source(tilo_ui_base::tokens::MotionSource::Pointer);
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
        let at = start + std::time::Duration::from_millis(ms);
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
    let later = start + std::time::Duration::from_secs(2);
    with_monotonic_time(later, || {
        assert!(first_offset(&d).abs() < 0.01);
        d.relayout();
        assert!(d.root::<Column>().drawn_offsets().is_none());
        assert!(!d.root_widget().needs_animation_frame());
    });
}

#[test]
fn nothing_leaves_when_the_whole_view_is_replaced() {
    use tilo_ui_app::time::testing::with_monotonic_time;
    let start = std::time::Instant::now();
    let mut d = with_monotonic_time(start, || rooted(offline_view()));
    with_monotonic_time(start, || {
        tilo_ui_base::motion::note_source(tilo_ui_base::tokens::MotionSource::Pointer);
        d.root_mut::<Column>().set_children(vec![
            Box::new(tilo_ui_base::widget::Spacer::new()),
            Box::new(tilo_ui_app::widget::Text::new("Another page")),
            Box::new(tilo_ui_app::widget::Text::new("Below").with_id("below")),
        ]);
        d.relayout();
        assert!(d.root::<Column>().drawn_offsets().is_none());
        assert!(!d.root_widget().needs_animation_frame());
    });
}

/// A banner whose dismiss the view bound to an action runs it once, through the banner's own
/// control API, and goes away.
#[test]
fn a_banner_whose_dismiss_is_bound_runs_the_action_it_names_once() {
    let mut spec = tilo_actions::ResourceSpec::new("res-notice", "test.notice", "Notice");
    spec = spec.with_action(
        tilo_actions::ActionSpec::operation(
            "dismiss",
            "Dismiss",
            tilo_actions::OperationKind::Effect,
            "test.notice.effect",
            &(),
        )
        .expect("a test action"),
    );
    let binding = tilo_actions::ActionBinding::unit_for(&spec, "dismiss").expect("published");
    let address = binding.address().clone();
    let mut banner = Banner::new(BannerKind::Info, "Synced").with_dismiss_press(
        tilo_ui_base::widget::Press::of(tilo_ui_base::component::Event::Action(
            tilo_ui::Bind::bound(binding),
        )),
    );
    banner.dismiss_now_for_tests();
    assert!(
        banner.is_dismissed_for_tests(),
        "the press dismisses the banner"
    );
    let taken = tilo_ui_base::widget::controls::take_control_invocation(&mut banner);
    assert_eq!(
        taken.as_ref().map(|taken| taken.address.clone()),
        Some(address),
        "one press runs the dismiss once"
    );
    assert!(
        tilo_ui_base::widget::controls::take_control_invocation(&mut banner).is_none(),
        "the press is taken once"
    );
}

/// A banner painted at `width`: its box, and the tops of the lines it paints, each once.
fn lines_of(content: &BannerContent<'_>, width: f64) -> (Rect, Vec<f64>) {
    let theme = Theme::dark();
    let mut text = TextRenderer::new();
    let height = Banner::height(content, width, &mut text, &theme);
    let rect = Rect::new(0.0, 0.0, width, height);
    let mut scene = Scene::new();
    Banner::paint(
        &mut PaintCtx::new(&mut scene, Affine::IDENTITY, &mut text, &theme),
        rect,
        content,
        Point::new(-1.0, -1.0),
    );
    let mut tops: Vec<f64> = scene
        .encoding()
        .resources
        .glyph_runs
        .iter()
        .map(|run| f64::from(run.transform.translation[1]))
        .collect();
    tops.sort_by(f64::total_cmp);
    tops.dedup_by(|a, b| (*a - *b).abs() < 0.5);
    (rect, tops)
}

tilo_test_env::scenario! {
    /// The banner: kinds, wrapping, height, live region, touch targets, placement, and arriving, leaving and being built again.
    /// Each phase was a test of its own and builds its own fixture.
    fn a_banner_lays_out_its_parts_and_goes_on_from_the_one_it_replaces;




    phase the_banner_is_a_live_region_by_severity;
    {
        use tilo_ui_base::{a11y::Politeness, a11y::Role};
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

    phase a_banner_its_view_builds_again_goes_on_from_the_one_it_replaces;
    {
        let mut d = rooted(Column::new());
        rebuild(&mut d, None);
        let empty_y = d.targets()["below"].y0;
        let frame = tilo_ui_base::motion::pin_frame();
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
                .push(Box::new(tilo_ui_app::widget::Text::new("Below").with_id("below"))),
        );
        let shown_y = d.targets()["below"].y0;
        let _frame = tilo_ui_base::motion::pin_frame();
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




    phase a_banner_names_what_sent_it_above_the_title;
    // What sent a banner is a line of its own above the title, and a reader hears it with what the
    // banner says; a banner that names no sender has no such line.
    {
        let bare = BannerContent::new(BannerKind::Error, "Build failed")
            .with_message(Some("The link check found 4 broken links."));
        let sent = bare.clone().with_source(Some("Forgejo"));
        let (bare_box, lines) = lines_of(&bare, 600.0);
        let (sent_box, with_sender) = lines_of(&sent, 600.0);
        assert_eq!(lines.len(), 2, "the title and its message: {lines:?}");
        assert_eq!(
            with_sender.len(),
            3,
            "the sender above them: {with_sender:?}"
        );
        assert!(
            with_sender[0] < lines[0],
            "the sender is the first line: {with_sender:?} against {lines:?}"
        );
        let caption = f64::from(Theme::dark().tokens.typography.size(TextRole::Caption));
        assert!(
            sent_box.height() >= bare_box.height() + caption,
            "the sender takes a line of its own: {bare_box:?} -> {sent_box:?}"
        );
        assert!(
            with_sender
                .iter()
                .all(|top| *top >= sent_box.y0 && top + caption <= sent_box.y1),
            "every line stands inside the banner: {with_sender:?} in {sent_box:?}"
        );
        assert_eq!(
            lines_of(&bare.with_source(Some("")), 600.0),
            (bare_box, lines.clone()),
            "a sender that names nothing shows no line"
        );

        let announced = Banner::error("Build failed")
            .with_message("The link check found 4 broken links.")
            .with_source("Forgejo");
        let node = Widget::accessibility(&announced).expect("banner exposes a11y");
        assert_eq!(node.name.as_deref(), Some("Build failed"));
        let description = node.description.unwrap_or_default();
        assert!(
            description.contains("Forgejo")
                && description.contains("The link check found 4 broken links."),
            "the reader hears who sent it and what it says: {description}"
        );
        let unnamed =
            Banner::error("Build failed").with_message("The link check found 4 broken links.");
        assert_eq!(
            Widget::accessibility(&unnamed)
                .unwrap()
                .description
                .as_deref(),
            Some("The link check found 4 broken links."),
            "a banner with no sender says only what it says"
        );
    }
}

/// A banner's two actions keep the faces they were built with at every width: wrapped under the
/// message on a narrow banner they are the same controls they are beside it on a wide one.
#[test]
fn both_actions_keep_their_faces_whether_beside_or_under_the_message() {
    fn roles(d: &Driver) -> Vec<(String, ButtonRole)> {
        let mut found = Vec::new();
        let mut pending: Vec<&dyn Widget> = vec![d.root_widget()];
        while let Some(widget) = pending.pop() {
            if let Some(button) = widget.as_any().and_then(|any| any.downcast_ref::<Button>())
                && let Some(id) = widget.test_id()
            {
                found.push((id.to_owned(), button.role_of()));
            }
            pending.extend(widget.children().iter().map(|child| &**child));
        }
        found.sort_by(|a, b| a.0.cmp(&b.0));
        found
    }
    let column = Column::new().push(Box::new(
        Banner::warning("Bookmarks hasn't arrived yet")
            .with_message("Bookmarks from Browser hasn't reached this device yet.")
            .with_id("waiting")
            .with_action(Button::new("waiting.again", "Try again").role(ButtonRole::Secondary))
            .with_secondary_action(
                Button::new("waiting.without", "Continue without it").role(ButtonRole::Secondary),
            ),
    ));
    let mut d = driver(Box::new(column));
    d.relayout_at((900, 400)).expect("lays out wide");
    let wide = roles(&d);
    assert_eq!(
        wide,
        [
            ("waiting.again".to_owned(), ButtonRole::Secondary),
            ("waiting.without".to_owned(), ButtonRole::Secondary),
        ],
        "beside the message, each action is the control it was built as"
    );
    d.relayout_at((360, 400)).expect("lays out narrow");
    assert_eq!(roles(&d), wide, "under the message, the same faces");
}

#[test]
fn under_reduced_motion_a_banner_lands_and_its_contents_fade_in() {
    let state = tilo_ui_base::LocalState::new(None::<BannerNotice>);
    let mut d = driver(Box::new(sync_column(&state, false)));
    d.set_theme(Theme::dark().with_reduce_motion(true));
    let empty_y = d.targets()["below"].y0;
    let _frame = tilo_ui_base::motion::pin_frame();
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
fn a_banner_leaving_lands_by_key_and_fades_under_reduced_motion() {
    use tilo_ui_app::time::testing::with_monotonic_time;
    let start = std::time::Instant::now();
    let mut d = with_monotonic_time(start, || rooted(offline_view()));
    with_monotonic_time(start, || {
        tilo_ui_base::motion::note_source(tilo_ui_base::tokens::MotionSource::Keyboard);
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
        tilo_ui_base::motion::note_source(tilo_ui_base::tokens::MotionSource::Pointer);
        rebuild(&mut d, None);
        assert!(first_offset(&d).abs() < 0.01, "the height lands");
        assert!(
            d.root_widget().needs_animation_frame(),
            "and the paint fades"
        );
    });
}

/// How far the column's child `index` is drawn below its place.
fn offset_of(d: &Driver, index: usize) -> f64 {
    d.root::<Column>()
        .drawn_offsets()
        .and_then(|offsets| offsets.get(index).copied())
        .unwrap_or(0.0)
}

#[test]
fn a_banner_dropped_while_opening_leaves_from_where_it_is_drawn() {
    use tilo_ui_app::time::testing::with_monotonic_time;
    let start = std::time::Instant::now();
    let mut d = with_monotonic_time(start, || rooted(Column::new()));
    with_monotonic_time(start, || rebuild(&mut d, None));
    let empty_y = d.targets()["below"].y0;
    with_monotonic_time(start, || {
        tilo_ui_base::motion::note_source(tilo_ui_base::tokens::MotionSource::Pointer);
        rebuild(&mut d, Some(Banner::warning("Reconnecting")));
    });
    let midway = start + std::time::Duration::from_millis(90);
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
    let later = with_monotonic_time(midway + std::time::Duration::from_millis(60), || {
        first_offset(&d)
    });
    assert!(later < leaving, "and moves up");
    with_monotonic_time(start + std::time::Duration::from_secs(2), || {
        d.relayout();
        assert!(!d.root_widget().needs_animation_frame());
    });
}
