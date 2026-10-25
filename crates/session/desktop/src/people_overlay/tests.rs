use super::*;

use crate::collaborators::RosterStatus;

const HOST: Rect = Rect::new(0.0, 40.0, 1440.0, 900.0);

fn row(id: &str, label: &str) -> RosterRow {
    RosterRow {
        id: id.into(),
        label: label.into(),
        initial: label.chars().next().unwrap_or('?'),
        color: vello::peniko::Color::from_rgb8(0x25, 0x63, 0xeb),
        is_agent: false,
        is_bot: false,
        role: None,
        invited: false,
        activity: Some(format!("In {label}'s notes")),
        status: None,
        recent: Vec::new(),
        followable: true,
        availability: ui::Availability::Available,
        call: None,
        join: None,
        own_device: false,
    }
}

fn screen(id: &str) -> PeerScreen {
    PeerScreen {
        id: id.into(),
        aspect: 16.0 / 10.0,
        windows: vec![
            PeerWindow {
                rect: Rect::new(0.0, 0.0, 0.5, 1.0),
                app: "so.tilo.chat".into(),
                app_name: "Chat".into(),
                title: Some("#general".into()),
                picture: Some(7),
                focused: true,
            },
            PeerWindow {
                rect: Rect::new(0.5, 0.0, 1.0, 1.0),
                app: "so.tilo.notes".into(),
                app_name: "Notes".into(),
                title: None,
                picture: None,
                focused: false,
            },
        ],
    }
}

/// Records which windows were asked for their live picture.
#[derive(Default)]
struct Pictures(Vec<u64>);

impl PeekPictures for Pictures {
    fn paint(&mut self, window: u64, _: &mut Scene, _: Affine, _: Rect) -> bool {
        self.0.push(window);
        true
    }
}

struct Frame<'a> {
    rows: &'a [RosterRow],
    screens: &'a [PeerScreen],
    messages: &'a [PeerMessages],
    followed: Option<&'a str>,
    unavailable: Option<&'a str>,
}

fn draw(overlay: &mut PeopleOverlay, frame: &Frame<'_>, pictures: &mut dyn PeekPictures) {
    draw_own(overlay, frame, None, pictures);
}

/// [`draw`], with this person's own row leading the list.
fn draw_own(
    overlay: &mut PeopleOverlay,
    frame: &Frame<'_>,
    own: Option<&RosterRow>,
    pictures: &mut dyn PeekPictures,
) {
    overlay.draw(
        &mut Scene::new(),
        Affine::IDENTITY,
        &mut TextRenderer::new(),
        &Theme::dark(),
        &PeopleOverlayModel {
            host: HOST,
            occupied: &[],
            rows: frame.rows,
            screens: frame.screens,
            messages: frame.messages,
            followed_id: frame.followed,
            messaging_unavailable: frame.unavailable,
            own,
        },
        pictures,
    );
}

fn moved(point: Point) -> WindowEvent {
    ui::synthetic_input::cursor_moved(point.x, point.y)
}

fn button(state: ElementState) -> WindowEvent {
    ui::synthetic_input::mouse_input(state, MouseButton::Left)
}

fn at<T>(start: Instant, after_ms: u64, run: impl FnOnce() -> T) -> T {
    let when = start
        .checked_add(Duration::from_millis(after_ms))
        .unwrap_or(start);
    tilo_clock::testing::with_monotonic_time(when, run)
}

fn shown_overlay() -> PeopleOverlay {
    PeopleOverlay {
        shown: ui::motion::Presence::opened(),
        ..PeopleOverlay::default()
    }
}

/// A person's own words lead their row; a model's description comes only
/// when they wrote none, and is marked as a model's; where they are is
/// the last resort.
#[test]
fn their_own_words_come_first_and_a_models_are_marked() {
    let mut ada = row("ada", "Ada");
    assert_eq!(
        doing(&ada),
        Some(("In Ada's notes".into(), LineSource::Observed))
    );
    ada.status = Some(RosterStatus {
        text: "Drafting the launch post".into(),
        source: LineSource::Generated,
    });
    assert_eq!(
        doing(&ada),
        Some(("Drafting the launch post".into(), LineSource::Generated))
    );
    ada.status = Some(RosterStatus {
        text: "Fixing the onboarding copy".into(),
        source: LineSource::Written,
    });
    assert_eq!(
        doing(&ada),
        Some(("Fixing the onboarding copy".into(), LineSource::Written))
    );
}

/// A row says what someone is doing with where they are under it: their
/// own words plainly, an automatic description (a model's, or the host's
/// reading of what they did) marked; without either, only where they are.
#[test]
fn a_row_says_what_they_do_then_where_and_marks_what_software_wrote() {
    let mut ada = row("ada", "Ada");
    let line = |doing: &str, place: Option<&str>, automatic| {
        Some(RowLine {
            doing: doing.into(),
            place: place.map(Into::into),
            automatic,
        })
    };
    assert_eq!(row_line(&ada), line("In Ada's notes", None, false));
    for (source, automatic) in [
        (LineSource::Observed, true),
        (LineSource::Generated, true),
        (LineSource::Written, false),
    ] {
        ada.status = Some(RosterStatus {
            text: "Working in Chat".into(),
            source,
        });
        assert_eq!(
            row_line(&ada),
            line("Working in Chat", Some("In Ada's notes"), automatic),
            "{source:?}"
        );
        assert_eq!(
            crate::collaborators::says_automatically(&ada),
            automatic,
            "the dropdown marks it alike"
        );
    }
    ada.activity = None;
    assert_eq!(row_line(&ada), line("Working in Chat", None, false));
}

/// This person's own row leads the list: a field with what they said,
/// which takes the keyboard only once pressed. Enter says the words,
/// Escape puts back what was there, and the clear button takes them away.
#[test]
fn their_own_row_edits_what_they_are_doing_in_place() {
    use winit::keyboard::{ModifiersState, NamedKey, SmolStr};
    let rows = vec![row("ada", "Ada")];
    let frame = Frame {
        rows: &rows,
        screens: &[],
        messages: &[],
        followed: None,
        unavailable: None,
    };
    let mut me = row("me", "Kim");
    me.followable = false;
    me.activity = None;
    let mut overlay = shown_overlay();
    let _guard = ui::motion::testing::settled();
    draw_own(&mut overlay, &frame, Some(&me), &mut NoPictures);
    assert!(overlay.own_row.area() > 0.0);
    assert!(
        overlay.rows[0].rect.y0 >= overlay.own_row.y1 - 0.5,
        "their own row comes first"
    );
    let typed = Key::Character(SmolStr::new("h"));
    assert_eq!(
        overlay.key(&typed, ModifiersState::empty()),
        None,
        "untouched, the field leaves the keyboard alone"
    );
    let field = overlay.own_status.field.center();
    let mut handled = false;
    let press = |overlay: &mut PeopleOverlay, at: Point, handled: &mut bool| {
        overlay.handle_event(&moved(at), 1.0, handled);
        overlay.handle_event(&button(ElementState::Pressed), 1.0, handled);
        overlay.handle_event(&button(ElementState::Released), 1.0, handled)
    };
    assert_eq!(press(&mut overlay, field, &mut handled), None);
    assert!(handled, "the press is the overlay's");
    assert!(overlay.composing(), "the field holds the keyboard");
    assert!(overlay.commit_text("Reviewing the launch plan"));
    assert_eq!(
        overlay.key(&Key::Named(NamedKey::Enter), ModifiersState::empty()),
        Some(Some(PeopleHit::SetStatus(
            "Reviewing the launch plan".into()
        )))
    );
    assert!(!overlay.composing(), "said, the field lets go");
    // The host says them back: their row shows their words.
    me.status = Some(RosterStatus {
        text: "Reviewing the launch plan".into(),
        source: LineSource::Written,
    });
    draw_own(&mut overlay, &frame, Some(&me), &mut NoPictures);
    press(&mut overlay, field, &mut handled);
    assert!(overlay.commit_text(" and the deck"));
    assert_eq!(
        overlay.key(&Key::Named(NamedKey::Escape), ModifiersState::empty()),
        Some(None)
    );
    assert!(!overlay.composing());
    draw_own(&mut overlay, &frame, Some(&me), &mut NoPictures);
    let clear = overlay
        .own_status
        .clear_rect()
        .expect("their words can be cleared");
    assert_eq!(
        press(&mut overlay, clear.center(), &mut handled),
        Some(PeopleHit::SetStatus(String::new()))
    );
    assert!(!overlay.composing(), "clearing takes no keyboard");
}

/// Agents are listed after the people, under their own heading; invited
/// people and this person's other devices are not in the overlay.
#[test]
fn agents_follow_the_people_under_their_own_heading() {
    let mut agent = row("bot", "Scribe");
    agent.is_agent = true;
    let mut invited = row("eve", "Eve");
    invited.invited = true;
    let rows = vec![agent, row("ada", "Ada"), invited, row("ben", "Ben")];
    let mut overlay = shown_overlay();
    draw(
        &mut overlay,
        &Frame {
            rows: &rows,
            screens: &[],
            messages: &[],
            followed: None,
            unavailable: None,
        },
        &mut NoPictures,
    );
    let order: Vec<&str> = overlay.rows.iter().map(|place| place.id.as_str()).collect();
    assert_eq!(order, ["ada", "ben", "bot"]);
    assert_eq!(overlay.headings.len(), 1, "one heading, over the agents");
    assert!(overlay.headings[0].1.y0 >= overlay.rows[1].rect.y1);
}

/// The panel is dragged by its body as a desk widget is: drawn under the
/// pointer while held and, let go, snapped to the edge nearest the pointer;
/// the host hears which, to keep it where the widgets keep theirs.
#[test]
fn dragging_the_panel_moves_it_and_reports_its_new_edge() {
    let rows = vec![row("ada", "Ada")];
    let frame = Frame {
        rows: &rows,
        screens: &[],
        messages: &[],
        followed: None,
        unavailable: None,
    };
    let mut overlay = shown_overlay();
    let start = ui::time::now();
    at(start, 0, || draw(&mut overlay, &frame, &mut NoPictures));
    assert_eq!(overlay.edge(), DockEdge::Right);
    let grab = Point::new(overlay.header.center().x, overlay.header.center().y);
    let mut handled = false;
    let mut hits = Vec::new();
    for (ms, event) in [
        (0, moved(grab)),
        (5, button(ElementState::Pressed)),
        (20, moved(Point::new(500.0, 400.0))),
        (40, moved(Point::new(120.0, 420.0))),
        (60, moved(Point::new(60.0, 430.0))),
        (200, button(ElementState::Released)),
    ] {
        hits.extend(at(start, ms, || {
            overlay.handle_event(&event, 1.0, &mut handled)
        }));
    }
    assert!(handled, "the drag is the overlay's");
    let Some(PeopleHit::Moved(edge)) = hits.last().cloned() else {
        panic!("a drag reports where it came to rest: {hits:?}");
    };
    assert_eq!(edge, DockEdge::Left);
    assert_eq!(overlay.edge(), edge);
    let _guard = ui::motion::testing::settled();
    at(start, 2000, || draw(&mut overlay, &frame, &mut NoPictures));
    assert!(
        (overlay.panel.x0 - (HOST.x0 + crate::desk_edges::MARGIN)).abs() < 1.0,
        "it rests on the left edge: {:?}",
        overlay.panel
    );
}

/// Passing over a row opens nothing; resting on it a quarter second opens
/// its peek, and the pointer can travel into the peek without closing it.
#[test]
fn a_peek_opens_on_a_rest_not_a_pass() {
    let rows = vec![row("ada", "Ada"), row("ben", "Ben")];
    let screens = [screen("ada")];
    let frame = Frame {
        rows: &rows,
        screens: &screens,
        messages: &[],
        followed: None,
        unavailable: None,
    };
    let mut overlay = shown_overlay();
    let start = ui::time::now();
    at(start, 0, || draw(&mut overlay, &frame, &mut NoPictures));
    let ada = overlay.rows[0].rect.center();
    let ben = overlay.rows[1].rect.center();
    let mut handled = false;
    at(start, 10, || {
        overlay.handle_event(&moved(ada), 1.0, &mut handled)
    });
    at(start, 100, || draw(&mut overlay, &frame, &mut NoPictures));
    assert_eq!(overlay.peeked(), None, "a pass opens nothing");
    at(start, 110, || {
        overlay.handle_event(&moved(ben), 1.0, &mut handled)
    });
    at(start, 300, || draw(&mut overlay, &frame, &mut NoPictures));
    assert_eq!(
        overlay.peeked(),
        None,
        "Ben's intent started over at 110 ms"
    );
    at(start, 400, || draw(&mut overlay, &frame, &mut NoPictures));
    assert_eq!(overlay.peeked(), Some("ben"));
    // Into the peek: it stays.
    let _guard = ui::motion::testing::settled();
    at(start, 900, || draw(&mut overlay, &frame, &mut NoPictures));
    let inside = overlay.peek.drawn.center();
    at(start, 950, || {
        overlay.handle_event(&moved(inside), 1.0, &mut handled)
    });
    at(start, 1600, || draw(&mut overlay, &frame, &mut NoPictures));
    assert_eq!(
        overlay.peeked(),
        Some("ben"),
        "the pointer on the peek keeps it"
    );
    // Away from both: it closes after the grace.
    at(start, 1700, || {
        overlay.handle_event(&moved(Point::new(700.0, 600.0)), 1.0, &mut handled)
    });
    at(start, 2200, || draw(&mut overlay, &frame, &mut NoPictures));
    assert_eq!(overlay.peeked(), None);
}

/// Their screen is built from their presence: a window on something shared
/// with this person shows this device's own picture of it; any other
/// asks for no picture and shows only its app.
#[test]
fn only_shared_windows_are_pictured() {
    let rows = vec![row("ada", "Ada")];
    let screens = [screen("ada")];
    let frame = Frame {
        rows: &rows,
        screens: &screens,
        messages: &[],
        followed: None,
        unavailable: None,
    };
    let mut overlay = shown_overlay();
    let _guard = ui::motion::testing::settled();
    draw(&mut overlay, &frame, &mut NoPictures);
    let id = overlay.rows[0].id.clone();
    overlay.open_peek(&id);
    let mut pictures = Pictures::default();
    draw(&mut overlay, &frame, &mut pictures);
    assert_eq!(pictures.0, [7], "only the shared window is pictured");
    assert!(screens[0].windows[0].shared());
    assert!(!screens[0].windows[1].shared());
}

/// Follow is on the row and in the peek, and both say the same thing: the
/// host toggles the one follow it keeps.
#[test]
fn follow_is_one_press_from_the_row_and_the_peek() {
    let rows = vec![row("ada", "Ada")];
    let screens = [screen("ada")];
    let frame = Frame {
        rows: &rows,
        screens: &screens,
        messages: &[],
        followed: None,
        unavailable: None,
    };
    let mut overlay = shown_overlay();
    let _guard = ui::motion::testing::settled();
    draw(&mut overlay, &frame, &mut NoPictures);
    let follow = overlay.rows[0].follow.center();
    let mut handled = false;
    overlay.handle_event(&moved(follow), 1.0, &mut handled);
    draw(&mut overlay, &frame, &mut NoPictures);
    overlay.handle_event(&button(ElementState::Pressed), 1.0, &mut handled);
    let hit = overlay.handle_event(&button(ElementState::Released), 1.0, &mut handled);
    assert_eq!(hit, Some(PeopleHit::ToggleFollow("ada".into())));
    overlay.open_peek("ada");
    draw(&mut overlay, &frame, &mut NoPictures);
    let peek_follow = overlay.peek.targets()[0].1.center();
    overlay.handle_event(&moved(peek_follow), 1.0, &mut handled);
    draw(&mut overlay, &frame, &mut NoPictures);
    overlay.handle_event(&button(ElementState::Pressed), 1.0, &mut handled);
    let hit = overlay.handle_event(&button(ElementState::Released), 1.0, &mut handled);
    assert_eq!(hit, Some(PeopleHit::ToggleFollow("ada".into())));
}

/// The message box takes the keyboard only once pressed; Enter hands the
/// message to the host for Chat to send, Escape lets go of the box and
/// the peek.
#[test]
fn the_message_box_sends_on_enter_and_escape_closes_it() {
    use winit::keyboard::{ModifiersState, NamedKey, SmolStr};
    let rows = vec![row("ada", "Ada")];
    let messages = [PeerMessages {
        id: "ada".into(),
        lines: vec![MessageLine {
            mine: false,
            text: "see you at standup".into(),
        }],
    }];
    let frame = Frame {
        rows: &rows,
        screens: &[],
        messages: &messages,
        followed: None,
        unavailable: None,
    };
    let mut overlay = shown_overlay();
    let _guard = ui::motion::testing::settled();
    draw(&mut overlay, &frame, &mut NoPictures);
    overlay.open_peek("ada");
    draw(&mut overlay, &frame, &mut NoPictures);
    let typed = Key::Character(SmolStr::new("h"));
    assert_eq!(
        overlay.key(&typed, ModifiersState::empty()),
        None,
        "untouched, the box leaves the keyboard alone"
    );
    let field = overlay.composer.field.center();
    let mut handled = false;
    overlay.handle_event(&moved(field), 1.0, &mut handled);
    overlay.handle_event(&button(ElementState::Pressed), 1.0, &mut handled);
    overlay.handle_event(&button(ElementState::Released), 1.0, &mut handled);
    assert!(overlay.composing());
    assert!(overlay.commit_text("hi Ada"));
    let sent = overlay.key(&Key::Named(NamedKey::Enter), ModifiersState::empty());
    assert_eq!(
        sent,
        Some(Some(PeopleHit::Send {
            to: "ada".into(),
            text: "hi Ada".into()
        }))
    );
    overlay.sent();
    let escaped = overlay.key(&Key::Named(NamedKey::Escape), ModifiersState::empty());
    assert_eq!(escaped, Some(None));
    assert!(!overlay.composing());
    draw(&mut overlay, &frame, &mut NoPictures);
    assert_eq!(overlay.peeked(), None, "Escape closed the peek");
}

/// Where direct messages cannot be sent from this device, the box says so
/// and offers no field to type into.
#[test]
fn without_direct_messages_the_box_says_why() {
    let rows = vec![row("ada", "Ada")];
    let reason = "Direct messages need the Tilo app on this device";
    let frame = Frame {
        rows: &rows,
        screens: &[],
        messages: &[],
        followed: None,
        unavailable: Some(reason),
    };
    let mut overlay = shown_overlay();
    let _guard = ui::motion::testing::settled();
    draw(&mut overlay, &frame, &mut NoPictures);
    overlay.open_peek("ada");
    draw(&mut overlay, &frame, &mut NoPictures);
    assert_eq!(overlay.composer.field, Rect::ZERO);
    assert!(overlay.composer.send_rect().is_none());
}

/// At its edge the panel stands where a desk widget would: past a widget
/// or a call card already waiting there, never on top of it.
#[test]
fn the_panel_steps_past_what_already_waits_at_its_edge() {
    let rows = vec![row("ada", "Ada")];
    let mut overlay = shown_overlay();
    let _guard = ui::motion::testing::settled();
    let draw_with = |overlay: &mut PeopleOverlay, occupied: &[Rect]| {
        overlay.draw(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &Theme::dark(),
            &PeopleOverlayModel {
                host: HOST,
                occupied,
                rows: &rows,
                screens: &[],
                messages: &[],
                followed_id: None,
                messaging_unavailable: None,
                own: None,
            },
            &mut NoPictures,
        );
        overlay.panel
    };
    let alone = draw_with(&mut overlay, &[]);
    let widget = alone.inflate(4.0, 4.0);
    let beside = draw_with(&mut overlay, &[widget]);
    assert!(!beside.overlaps(widget), "{beside:?} on {widget:?}");
    assert!(
        (beside.x1 - alone.x1).abs() < 1.0,
        "still at the right edge"
    );
}
