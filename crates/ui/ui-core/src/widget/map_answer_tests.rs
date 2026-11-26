//! The person's answer stays in view (`docs/designs/app-language/canvas.md` §7, "Everything stays
//! in view"): the place answering where they pointed is panned into view whole as it grows, never
//! zoomed, never past the viewport's height; a place an agent puts off-screen after it is not
//! chased at the cost of the answer; and the rectangle they point at is outlined in a soft theme
//! role.
use super::test_support::*;

use super::*;

/// The person asked near the bottom right corner: the turn at the caret runs past the
/// viewport's edge as its answer grows. The camera pans the least that shows all of it, caption
/// and words, and never zooms; and the card never grows past what the viewport shows.
#[test]
fn an_answer_asked_at_the_edge_is_panned_into_view_whole() {
    let _frame = tilo_ui_base::motion::pin_frame();
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
    let _frame = tilo_ui_base::motion::pin_frame();
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
    let _frame = tilo_ui_base::motion::pin_frame();
    let old = pointed((100.0, 500.0));
    let mut map = Map::new().pin(
        "turn",
        grown(100.0, 500.0, 400.0, 480.0),
        Box::new(tilo_ui_base::widget::Spacer::new()),
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
    let _frame = tilo_ui_base::motion::pin_frame();
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

/// A list that scrolls says what its rows come to where it is asked what it holds: a place found
/// at a click hugs its six rows rather than at a window's height with an empty foot,
/// and a list longer than the room stands at the room and scrolls. Given a height, the list
/// takes it, however many rows it holds.
#[test]
fn a_found_list_hugs_its_rows_up_to_its_room() {
    let _frame = tilo_ui_base::motion::pin_frame();
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

/// The person asked near the bottom of the canvas, and the agent's turn opened a long inbox
/// beside it: the camera pans the least that brings the two into view together, never the inbox
/// running past the edge. It only pans.
#[test]
fn a_turn_and_the_place_it_opened_are_framed_together() {
    let _frame = tilo_ui_base::motion::pin_frame();
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
    let _frame = tilo_ui_base::motion::pin_frame();
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
    let _frame = tilo_ui_base::motion::pin_frame();
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
    let _frame = tilo_ui_base::motion::pin_frame();
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
