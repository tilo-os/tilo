//! The person's answer stays in view (`docs/designs/app-language/canvas.md` §7, "Everything stays
//! in view"): the place answering where they pointed is panned into view whole as it grows, never
//! zoomed, never past the viewport's height; a place an agent puts off-screen after it is not
//! chased at the cost of the answer; and the rectangle they point at is outlined in a soft theme
//! role.

use super::*;
use crate::Theme;
use crate::widget::primitives::SurfaceLevel;

fn theme() -> Theme {
    Theme::dark()
}

/// The viewport every test lays its map out in.
const VIEWPORT: Rect = Rect::new(0.0, 0.0, 800.0, 600.0);

/// A pin at `(x, y)`, `width` wide and at most `height` tall, that hugs what it holds and grows
/// from where the person pointed, showing `body`. Its view is bare, so it keeps no place's band.
fn answer(key: &str, (x, y): (f64, f64), (width, height): (f64, f64), body: &str) -> Map {
    Map::new().pin(key, grown(x, y, width, height), words(body))
}

fn grown(x: f64, y: f64, width: f64, height: f64) -> PinPlace {
    PinPlace {
        x,
        y,
        width,
        height: Some(height),
        band: 0.0,
        hug: true,
        grows: true,
    }
}

fn words(body: &str) -> Element {
    Box::new(crate::widget::Text::new(body))
}

fn lay_out(map: &mut Map) {
    let mut text = crate::font_catalog::fixture_renderer();
    crate::widget::layout::lay_out_detached(map, VIEWPORT, &theme(), &mut text)
        .expect("the map lays out");
}

fn draw(map: &mut Map) {
    let mut scene = Scene::new();
    let mut text = crate::font_catalog::fixture_renderer();
    map.draw(&mut crate::draw::DrawCtx::new(&mut scene, Affine::IDENTITY, &mut text, &theme(), PresenceCtx::default().draw_env()));
}

/// An empty canvas the person pointed at `at` on, laid out and drawn: the map the next one is
/// built in place of.
fn pointed(at: (f64, f64)) -> Map {
    let mut map = Map::new();
    lay_out(&mut map);
    draw(&mut map);
    map.point_at(Point::new(at.0, at.1));
    map
}

/// Whether `area` (canvas pixels) stands whole inside what `map`'s camera, where it is going,
/// shows.
fn in_view(map: &Map, area: Rect) -> bool {
    let seen = map.seen_rect().expect("laid out").inflate(0.5, 0.5);
    area.x0 >= seen.x0 && area.y0 >= seen.y0 && area.x1 <= seen.x1 && area.y1 <= seen.y1
}

/// The person asked near the bottom right corner: the turn at the caret runs past the
/// viewport's edge as its answer grows. The camera pans the least that shows all of it, caption
/// and words, and never zooms; and the card never grows past what the viewport shows.
#[test]
fn an_answer_asked_at_the_edge_is_panned_into_view_whole() {
    let _frame = crate::motion::pin_frame();
    let old = pointed((560.0, 420.0));
    let reply = "Nine tasks are due this week, and each is listed here. ".repeat(30);
    let mut map = answer("turn", (560.0, 420.0), (520.0, 2000.0), &reply);
    drop(old);
    lay_out(&mut map);
    lay_out(&mut map);
    assert_eq!(map.answer.as_deref(), Some("turn"), "the turn answers");
    let area = map.area_of("turn").expect("the turn stands");
    assert!(
        area.height() * map.camera().zoom <= VIEWPORT.height() - 2.0 * ARRIVAL_MARGIN + 0.5,
        "the card is never taller than what the person sees: {area:?}"
    );
    assert!(
        in_view(&map, area),
        "the whole turn stands in view: {area:?} in {:?}",
        map.seen_rect()
    );
    assert!(
        (map.camera().zoom - 1.0).abs() < f64::EPSILON,
        "the camera panned, never zoomed: {:?}",
        map.camera()
    );
}

/// An answer that grows after it stood (the agent's words arriving) is followed: the camera keeps
/// all of it in view, until the person moves the lens themselves.
#[test]
fn a_growing_answer_is_kept_in_view_until_the_person_pans() {
    let _frame = crate::motion::pin_frame();
    let old = pointed((100.0, 380.0));
    let mut map = answer("turn", (100.0, 380.0), (400.0, 480.0), "Thinking…");
    drop(old);
    lay_out(&mut map);
    draw(&mut map);
    let before = map.camera();
    // The reply arrives: the same pin, its words now many lines.
    let long = "The importer handles tables now, and merged cells too. ".repeat(30);
    map.put_pin_view(0, words(&long));
    map.invalidation.invalidate();
    lay_out(&mut map);
    lay_out(&mut map);
    let area = map.area_of("turn").expect("the turn stands");
    assert!(
        area.y1 > 600.0,
        "the answer grew past the viewport: {area:?}"
    );
    assert_eq!(map.answer.as_deref(), Some("turn"), "still the answer");
    assert!(
        map.camera().origin.y > before.origin.y,
        "the camera followed it down: {:?} from {before:?}, rect {:?}, laid {:?}",
        map.camera(),
        map.rect,
        map.laid.borrow().get("turn")
    );
    assert!(in_view(&map, area), "all of it in view: {area:?}");
    // The person pans away: the canvas is theirs, and the answer is not chased any more.
    map.set_camera(MapCamera {
        origin: Point::new(-500.0, -500.0),
        zoom: 1.0,
    });
    lay_out(&mut map);
    assert!(map.answer.is_none(), "their hand lets the answer go");
    assert_eq!(map.camera().origin, Point::new(-500.0, -500.0));
}

/// A turn whose view has said no height yet (its words not arrived) stands at its most for now:
/// the camera shows its top, and does not go to the foot of a room it will not fill.
#[test]
fn an_answer_that_said_no_height_yet_is_not_followed_to_its_foot() {
    let _frame = crate::motion::pin_frame();
    let old = pointed((100.0, 500.0));
    let mut map = Map::new().pin(
        "turn",
        grown(100.0, 500.0, 400.0, 480.0),
        Box::new(crate::widget::Spacer::new()),
    );
    drop(old);
    lay_out(&mut map);
    lay_out(&mut map);
    assert!(
        map.camera().origin.y < 100.0,
        "the camera showed its top, not its most: {:?}",
        map.camera()
    );
}

/// After the person's turn stands in view, the agent puts a place far off-screen: the camera does
/// not chase it and push the turn out. One it can show with the turn still in view, it shows.
#[test]
fn the_turn_wins_over_a_place_the_agent_puts_off_screen() {
    let _frame = crate::motion::pin_frame();
    let old = pointed((100.0, 100.0));
    let mut turn = answer("turn", (100.0, 100.0), (400.0, 480.0), "Two tasks are due.");
    drop(old);
    lay_out(&mut turn);
    draw(&mut turn);
    let resting = turn.camera();
    assert!(in_view(&turn, turn.area_of("turn").expect("the turn")));
    // Far off: showing it would push the turn out of view, so the camera stays.
    let far = Map::new()
        .pin(
            "turn",
            grown(100.0, 100.0, 400.0, 480.0),
            words("Two tasks are due."),
        )
        .pin("tasks", grown(1600.0, 1200.0, 720.0, 520.0), words("Tasks"));
    drop(turn);
    let mut far = far;
    lay_out(&mut far);
    lay_out(&mut far);
    assert_eq!(
        far.answer.as_deref(),
        Some("turn"),
        "the turn is still the answer"
    );
    assert!(
        far.camera().same(resting),
        "the camera stayed with the turn: {:?}",
        far.camera()
    );
    assert!(in_view(&far, far.area_of("turn").expect("the turn")));
    draw(&mut far);
    // Near: it fits beside the turn once the camera pans a little, so it is shown, the turn too.
    let near = Map::new()
        .pin(
            "turn",
            grown(100.0, 100.0, 400.0, 480.0),
            words("Two tasks are due."),
        )
        .pin("tasks", grown(1600.0, 1200.0, 720.0, 520.0), words("Tasks"))
        .pin("notes", grown(620.0, 100.0, 200.0, 200.0), words("Notes"));
    drop(far);
    let mut near = near;
    lay_out(&mut near);
    lay_out(&mut near);
    assert!(
        in_view(&near, near.area_of("notes").expect("the notes")),
        "what fits beside the turn is shown"
    );
    assert!(
        in_view(&near, near.area_of("turn").expect("the turn")),
        "and the turn stays in view"
    );
}

/// The rectangle the person points at is outlined in the theme's selection wash, a role lighter
/// than the focus accent, rounded as a place is: never a hard accent stroke.
#[test]
fn the_pointed_rectangle_is_outlined_softly_in_a_theme_role() {
    for theme in [Theme::dark(), Theme::light()] {
        let look = OutlineLook::of(&theme);
        assert_eq!(look.colour, theme.tokens.colors.intent.selection_tint);
        assert_ne!(look.colour, theme.border_focus, "not the focus accent");
        assert!(
            look.colour.components[3] < theme.border_focus.components[3],
            "lighter than the focus accent"
        );
        assert!(
            (look.radius - SurfaceLevel::Window.radius(&theme)).abs() < f64::EPSILON
                && look.radius > 0.0,
            "rounded as a place is"
        );
        assert!(look.width <= 1.0, "a soft line");
    }
}

/// A list of `rows` rows of 40 pixels that fills its parent, as a found inbox or notes list is.
fn list(rows: u64) -> Element {
    let model =
        crate::widget::VecListModel::new((0..rows).collect(), |row| *row).expect("unique rows");
    Box::new(
        crate::widget::VirtualList::new(model, 40.0, |_, index, _| {
            Box::new(
                crate::widget::Container::new()
                    .with_height(40.0)
                    .with_child(Box::new(crate::widget::Text::new(&format!(
                        "Conversation {index}"
                    )))),
            )
        })
        .filling(),
    )
}

/// A list that scrolls says what its rows come to where it is asked what it holds: a place found
/// at a click hugs its six rows rather than at a window's height with an empty foot,
/// and a list longer than the room stands at the room and scrolls. Given a height, the list
/// takes it, however many rows it holds.
#[test]
fn a_found_list_hugs_its_rows_up_to_its_room() {
    let _frame = crate::motion::pin_frame();
    let mut short = Map::new().pin("inbox", grown(0.0, 100.0, 400.0, 480.0), list(6));
    lay_out(&mut short);
    let inbox = short.laid.borrow()["inbox"];
    let band = 0.0;
    assert!(
        (inbox.height() - 240.0).abs() <= 2.0,
        "six rows of 40: {inbox:?}"
    );
    let mut long = Map::new().pin("inbox", grown(0.0, 100.0, 400.0, 300.0), list(400));
    lay_out(&mut long);
    let inbox = long.laid.borrow()["inbox"];
    assert!(
        (inbox.height() - band - 300.0).abs() <= 0.5,
        "four hundred rows stand at the room and scroll: {inbox:?}"
    );
    // Given a height (a place sized by hand), the list fills it, short or long.
    let mut sized = Map::new().pin(
        "inbox",
        PinPlace {
            hug: false,
            ..grown(0.0, 100.0, 400.0, 300.0)
        },
        list(6),
    );
    lay_out(&mut sized);
    let inbox = sized.laid.borrow()["inbox"];
    assert!(
        (inbox.height() - band - 300.0).abs() <= 0.5,
        "a place with a height of its own stands at it: {inbox:?}"
    );
}

/// The same holds through a place's own surface (`ui.Place`), as the canvas stands a found list.
#[test]
fn a_found_list_in_its_place_hugs_its_rows() {
    let _frame = crate::motion::pin_frame();
    let place: Element = Box::new(
        <crate::widget::canvas_place::CanvasPlace as crate::component::Described>::from_props(
            "inbox",
            crate::widget::canvas_place::PlaceProps {
                name: Some("All inboxes".into()),
                body: Some(list(6)),
                ..crate::widget::canvas_place::PlaceProps::default()
            },
        ),
    );
    let mut map = Map::new().pin(
        "inbox",
        PinPlace {
            band: crate::widget::canvas_place::PLACE_BAND,
            ..grown(0.0, 100.0, 400.0, 480.0)
        },
        place,
    );
    lay_out(&mut map);
    let inbox = map.laid.borrow()["inbox"];
    let band = crate::widget::canvas_place::PLACE_BAND;
    assert!(
        inbox.height() - band < 330.0 && inbox.height() - band >= 240.0,
        "the place hugs its six rows: {inbox:?}"
    );
}

/// The person asked near the bottom of the canvas, and the agent's turn opened a long inbox
/// beside it: the camera pans the least that brings the two into view together, never the inbox
/// running past the edge. It only pans.
#[test]
fn a_turn_and_the_place_it_opened_are_framed_together() {
    let _frame = crate::motion::pin_frame();
    let old = pointed((40.0, 480.0));
    let mut turn = answer(
        "turn",
        (40.0, 480.0),
        (300.0, 2000.0),
        "Here are your mails.",
    );
    drop(old);
    lay_out(&mut turn);
    draw(&mut turn);
    let zoom = turn.camera().zoom;
    // The inbox arrives beside the turn: a list longer than the room, so it stands at its most.
    let both = Map::new()
        .pin(
            "turn",
            grown(40.0, 480.0, 300.0, 2000.0),
            words("Here are your mails."),
        )
        .pin("inbox", grown(380.0, 480.0, 400.0, 2000.0), list(400));
    drop(turn);
    let mut both = both;
    lay_out(&mut both);
    draw(&mut both);
    lay_out(&mut both);
    assert_eq!(both.answer.as_deref(), Some("turn"), "the turn answers");
    let turn = both.area_of("turn").expect("the turn");
    let inbox = both.area_of("inbox").expect("the inbox");
    assert!(
        in_view(&both, turn),
        "the turn in view: {turn:?} in {:?}",
        both.seen_rect()
    );
    assert!(
        in_view(&both, inbox),
        "the inbox beside it in view: {inbox:?} in {:?}",
        both.seen_rect()
    );
    let seen = both.seen_rect().expect("laid out");
    let group = turn.union(inbox);
    assert!(
        (seen.y1 - group.y1 - ARRIVAL_MARGIN).abs() <= 1.0,
        "the least pan: the pair's foot a margin over the viewport's, {group:?} in {seen:?}"
    );
    assert!(
        (both.camera().zoom - zoom).abs() < f64::EPSILON,
        "panned, never zoomed"
    );
}

/// A short turn and a short place it opened, asked near the foot of the canvas, running past it:
/// the camera pans the least that brings both in, no further, so what the person was looking at
/// moves as little as it can.
#[test]
fn a_short_pair_asked_at_the_foot_is_panned_the_least() {
    let _frame = crate::motion::pin_frame();
    let old = pointed((40.0, 520.0));
    let both = Map::new()
        .pin(
            "turn",
            grown(40.0, 520.0, 300.0, 2000.0),
            words("Two tasks are due."),
        )
        .pin("tasks", grown(380.0, 520.0, 400.0, 2000.0), list(6));
    drop(old);
    let mut both = both;
    lay_out(&mut both);
    draw(&mut both);
    lay_out(&mut both);
    let group = both
        .area_of("turn")
        .expect("the turn")
        .union(both.area_of("tasks").expect("the tasks"));
    let seen = both.seen_rect().expect("laid out");
    assert!(in_view(&both, group), "both in view: {group:?} in {seen:?}");
    // The pan went no further than the pair needed as it stood (its list measured at its most
    // first, then at its rows): the pair stays near the foot it was asked at, never centred.
    assert!(
        seen.y1 - group.y1 < group.y0 - seen.y0,
        "the least pan, not a centring: turn {:?}, tasks {:?}, seen {seen:?}",
        both.area_of("turn"),
        both.area_of("tasks"),
    );
    assert!(
        both.camera().origin.x.abs() < 0.5,
        "nothing across needed a pan: {:?}",
        both.camera()
    );
}

/// A turn that stands whole in view moves nothing, however close it comes to the viewport's
/// edge: the camera is the person's until something new is out of sight (`canvas.md` §7).
#[test]
fn an_answer_in_view_moves_nothing() {
    let _frame = crate::motion::pin_frame();
    let old = pointed((300.0, 100.0));
    let resting = old.camera();
    // Its trailing edge 8 pixels inside the viewport's, nearer than the margin a pan keeps.
    let mut map = answer("turn", (300.0, 100.0), (492.0, 480.0), "Hi! What can I do?");
    drop(old);
    lay_out(&mut map);
    draw(&mut map);
    lay_out(&mut map);
    assert_eq!(map.answer.as_deref(), Some("turn"), "the turn answers");
    assert!(in_view(&map, map.area_of("turn").expect("the turn")));
    assert!(
        map.camera().same(resting),
        "the camera stayed where the person had it: {:?} from {resting:?}",
        map.camera()
    );
}

/// The caret the person's click puts at the viewport's trailing edge stands there, its field
/// running past the edge: the pointing itself never moves the camera, only the answer it brings.
#[test]
fn the_caret_never_moves_the_camera() {
    let _frame = crate::motion::pin_frame();
    let old = pointed((700.0, 560.0));
    let resting = old.camera();
    let caret = PinPlace {
        x: 700.0,
        y: 560.0,
        width: 420.0,
        height: Some(40.0),
        band: 0.0,
        hug: false,
        grows: false,
    };
    let mut map = Map::new().pin("caret", caret, words("remind me"));
    drop(old);
    lay_out(&mut map);
    draw(&mut map);
    lay_out(&mut map);
    assert!(
        map.camera().same(resting),
        "the click moved nothing: {:?} from {resting:?}",
        map.camera()
    );
}

/// A turn's steps unfold where they are: pressing the quiet line under an answer that hugs its
/// words grows the place by the steps it lists. The canvas measures the place in a copy of the
/// window's tree before it stands it, and the row's body must open in the window's own tree too,
/// not only in the copy, or the line turns open over nothing.
#[test]
fn unfolding_a_hugging_answers_steps_grows_it_by_them() {
    let _frame = crate::motion::pin_frame();
    let _settled = crate::motion::testing::settled();
    // The row as a view describes it (Chat's turn): its steps are its body's views.
    let row = <crate::widget::DisclosureRow as crate::component::Described>::from_props(
        "steps",
        crate::widget::DisclosureRowProps {
            title: "Looked up Notes and 5 more steps".into(),
            body: (0..6)
                .map(|index| -> Element {
                    Box::new(crate::widget::Text::new(&format!(
                        "Looked up Notes {index}"
                    )))
                })
                .collect(),
            dense: true,
            ..crate::widget::DisclosureRowProps::default()
        },
    );
    let card = crate::widget::arrange::Stack::stack()
        .gap(crate::tokens::Rhythm::Related)
        .push(Box::new(crate::widget::Text::new("Two notes changed.")))
        .push(Box::new(row));
    let body: Element =
        Box::new(crate::widget::Scrollable::new_vertical().with_child(Box::new(card)));
    let band = crate::widget::canvas_place::PLACE_BAND;
    let place: Element = Box::new(
        <crate::widget::canvas_place::CanvasPlace as crate::component::Described>::from_props(
            "turn",
            crate::widget::canvas_place::PlaceProps {
                name: Some("what changed?".into()),
                body: Some(body),
                ..crate::widget::canvas_place::PlaceProps::default()
            },
        ),
    );
    let map = Map::new().pin(
        "turn",
        PinPlace {
            band,
            ..grown(100.0, 80.0, 420.0, 400.0)
        },
        place,
    );
    let mut host =
        crate::headless::FrameHarness::for_root(crate::State::new(), map, (800, 600), theme());
    host.relayout();
    host.recompose().expect("a frame settles");
    let laid = |host: &mut crate::headless::FrameHarness| {
        host.root_widget_mut()
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<Map>())
            .expect("the map")
            .laid
            .borrow()["turn"]
    };
    let folded = laid(&mut host);
    // The person presses the steps' line, the card's last, and the frames the press asks for run
    // as the live window runs them: laid out where something moved, never built again whole.
    host.send(&crate::synthetic_input::cursor_moved(
        folded.x0 + 30.0,
        folded.y1 - 10.0,
    ));
    for state in [ElementState::Pressed, ElementState::Released] {
        host.send(&crate::synthetic_input::mouse_input(
            state,
            winit::event::MouseButton::Left,
        ));
    }
    for _ in 0..4 {
        host.idle();
    }
    let open = laid(&mut host);
    assert_eq!(
        host.with_widget_mut(|row: &mut crate::widget::DisclosureRow| row.is_open()),
        Some(true),
        "the press opened the steps' line"
    );
    assert!(
        open.height() > folded.height() + 6.0 * 14.0,
        "the place grows by the six steps it unfolds: {folded:?} -> {open:?}"
    );
}

/// A turn that fails grows its "Request refused" row where it stands, with no pointer over it:
/// the banner a view adds to a hugging answer arrives, and the place stands it once it has, in
/// the frames the window draws by itself. Before, it stood only once the pointer moved there.
#[test]
fn a_banner_added_to_a_hugging_answer_stands_without_the_pointer() {
    let card = crate::widget::arrange::Stack::stack()
        .gap(crate::tokens::Rhythm::Related)
        .push(Box::new(crate::widget::Text::new(
            "The model's answer could not be used.",
        )));
    let body: Element =
        Box::new(crate::widget::Scrollable::new_vertical().with_child(Box::new(card)));
    let map = Map::new().pin("turn", grown(100.0, 80.0, 420.0, 400.0), body);
    let mut harness =
        crate::headless::FrameHarness::for_root(crate::State::new(), map, (800, 600), theme());
    harness.relayout();
    harness.recompose().expect("a frame settles");
    for _ in 0..4 {
        harness.idle();
    }
    let laid = |harness: &mut crate::headless::FrameHarness| {
        harness
            .root_widget_mut()
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<Map>())
            .expect("the map")
            .laid
            .borrow()["turn"]
    };
    let before = laid(&mut harness);
    // The view's next tree holds the failure's row: the reconciler inserts it in place.
    harness
        .with_widget_mut(|scroll: &mut crate::widget::Scrollable| {
            let stack = scroll
                .child_mut()
                .and_then(|child| child.as_any_mut())
                .and_then(|any| any.downcast_mut::<crate::widget::arrange::Stack>())
                .expect("the card");
            let banner =
                crate::widget::Banner::new(crate::widget::BannerKind::Error, "Request refused");
            stack.insert(1, crate::widget::arrange::Part::new(Box::new(banner)));
        })
        .expect("the scroll");
    // The window draws what it asks for, as time passes, and nothing moves the pointer.
    let banner_height = |harness: &mut crate::headless::FrameHarness| {
        harness
            .with_widget_mut(|banner: &mut crate::widget::Banner| banner.get_rect())
            .map(|rect| rect.height())
    };
    let deadline = crate::time::now() + std::time::Duration::from_millis(1500);
    loop {
        harness.idle();
        if banner_height(&mut harness).is_some_and(|height| height > 20.0) {
            break;
        }
        assert!(
            crate::time::now() < deadline,
            "the banner did not lay itself out without a pointer"
        );
        std::thread::sleep(std::time::Duration::from_millis(8));
    }
    let banner = harness
        .with_widget_mut(|banner: &mut crate::widget::Banner| banner.get_rect())
        .expect("the banner");
    let after = laid(&mut harness);
    assert!(
        banner.height() > 20.0,
        "the banner stands in the card at its height: {banner:?}"
    );
    assert!(
        after.height() > before.height() + 20.0,
        "the place grows by the row it holds: {before:?} -> {after:?}"
    );
}
