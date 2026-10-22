//! The media transport in a NavShell's band.

use super::*;
use crate::headless::FrameHarness;
use crate::state::State;
use crate::widget::{ContentKind, NavShell};
use vello::kurbo::Point;
use winit::event::{ElementState, MouseButton, WindowEvent};

fn playing() -> TransportState {
    TransportState {
        title: "Song".into(),
        subtitle: "Artist".into(),
        error: false,
        art: TransportArt::Placeholder,
        art_name: "Album".into(),
        opens: Some("Open album".into()),
        playing: false,
        idle: false,
        position_ms: 30_000.0,
        duration_ms: 120_000.0,
        volume: 0.5,
        shuffle: Some(false),
        repeat: Some(TransportRepeat::Off),
        liked: Some(false),
        lyrics: Some(false),
        queue: Some(false),
        more: true,
    }
}

/// A shell whose band is a transport showing `state`, laid out at `w`.
fn banded(state: TransportState, w: u32) -> (FrameHarness, TransportFeed) {
    let feed = TransportFeed::new();
    feed.show(state);
    let shell = NavShell::new()
        .with_id("player")
        .content_kind(ContentKind::List)
        .content(Box::new(Text::new("Page")))
        .bottom_band(Box::new(MediaTransport::new(&feed)));
    let mut h = FrameHarness::for_root(State::new(), shell, (w, 700), Theme::dark());
    for _ in 0..3 {
        h.relayout_at((w, 700)).expect("layout settles");
    }
    (h, feed)
}

fn find<'a>(widget: &'a dyn Widget, id: &str) -> Option<&'a dyn Widget> {
    if widget.test_id() == Some(id) {
        return Some(widget);
    }
    widget
        .children()
        .into_iter()
        .find_map(|child| find(child, id))
}

/// The rect of the control `id`, when it is there and laid out.
fn control(h: &FrameHarness, id: &str) -> Option<Rect> {
    find(h.root::<NavShell>(), id)
        .map(Widget::get_rect)
        .filter(|rect| rect.width() > 0.0)
}

fn press(h: &mut FrameHarness, id: &str) {
    let rect = control(h, id).unwrap_or_else(|| panic!("{id} is laid out"));
    h.click_at(rect.center().x, rect.center().y);
}

/// A wide window shows every control the app declares inline, in the
/// band across the window's foot, and no More button.
#[test]
fn a_wide_transport_shows_its_controls_inline_in_the_band() {
    let (h, _) = banded(playing(), 1400);
    let band = h.root::<NavShell>().geometry().band;
    assert!(band.height() > 0.0);
    for id in [
        "transport.like",
        "transport.lyrics",
        "transport.shuffle",
        "transport.previous",
        "transport.play",
        "transport.next",
        "transport.repeat",
        "transport.position",
        "transport.queue",
        "transport.mute",
        "transport.volume",
    ] {
        let rect = control(&h, id).unwrap_or_else(|| panic!("{id} is there"));
        assert!(band.contains(rect.center()), "{id} at {rect:?} in {band:?}");
    }
    assert_eq!(control(&h, "transport.more"), None, "no More while wide");
    let play = control(&h, "transport.play").unwrap();
    let scrub = control(&h, "transport.position").unwrap();
    assert!(
        play.y1 <= scrub.y0 + 0.5,
        "the transport stands over the scrubber"
    );
    assert!(
        (play.center().x - scrub.center().x).abs() < 1.0,
        "and is centred on it"
    );
}

/// A press reaches the app as what it asks for; the app's player stays
/// the truth, so nothing changes until the app shows the new state.
#[test]
fn presses_reach_the_app_as_events_and_the_state_follows_its_declaration() {
    let (mut h, feed) = banded(playing(), 1400);
    press(&mut h, "transport.play");
    press(&mut h, "transport.shuffle");
    assert_eq!(
        feed.take_events(),
        vec![TransportEvent::TogglePlay, TransportEvent::ToggleShuffle]
    );
    let named = |h: &FrameHarness| {
        find(h.root::<NavShell>(), "transport.play")
            .and_then(Widget::accessibility)
            .and_then(|node| node.name)
    };
    assert_eq!(
        named(&h).as_deref(),
        Some("Play"),
        "not before the app says so"
    );
    feed.show(TransportState {
        playing: true,
        ..playing()
    });
    h.relayout_at((1400, 700)).expect("layout settles");
    assert_eq!(named(&h).as_deref(), Some("Pause"));
}

/// With nothing to play the transport rests: its controls are there,
/// disabled, and a press asks for nothing.
#[test]
fn an_idle_transport_rests() {
    let (mut h, feed) = banded(
        TransportState {
            title: "Nothing playing".into(),
            idle: true,
            ..TransportState::default()
        },
        1400,
    );
    press(&mut h, "transport.play");
    assert_eq!(feed.take_events(), Vec::new());
    assert_eq!(control(&h, "transport.like"), None, "left out, not shown");
}

/// A narrow window folds the transport: the song beside previous, play,
/// next and mute, with a More button the app answers with its menu under
/// it, over the scrubber.
#[test]
fn a_narrow_transport_folds_the_rest_behind_more() {
    let (mut h, feed) = banded(playing(), 480);
    for id in ["transport.shuffle", "transport.repeat", "transport.volume"] {
        assert_eq!(control(&h, id), None, "{id} folds behind More");
    }
    let more = control(&h, "transport.more").expect("More is there");
    let play = control(&h, "transport.play").expect("play stays");
    assert!((more.center().y - play.center().y).abs() < 1.0, "one row");
    press(&mut h, "transport.more");
    let events = feed.take_events();
    let [TransportEvent::More(at)] = events.as_slice() else {
        panic!("More asks for the menu: {events:?}");
    };
    assert!(
        (at.center().x - more.center().x).abs() < 1.0
            && (at.center().y - more.center().y).abs() < 1.0,
        "under the button: {at:?} vs {more:?}"
    );
}

fn pointer(h: &mut FrameHarness, at: Point, state: Option<ElementState>) {
    let device_id = winit::event::DeviceId::dummy();
    h.dispatch_to_root(&WindowEvent::CursorMoved {
        device_id,
        position: winit::dpi::PhysicalPosition::new(at.x, at.y),
    });
    if let Some(state) = state {
        h.dispatch_to_root(&WindowEvent::MouseInput {
            device_id,
            state,
            button: MouseButton::Left,
        });
    }
}

/// The art opens what plays when the app says what that is, and is only
/// a picture when it doesn't.
#[test]
fn the_art_opens_what_plays() {
    let (mut h, feed) = banded(playing(), 1400);
    press(&mut h, "transport.art");
    assert_eq!(feed.take_events(), vec![TransportEvent::OpenPlaying]);
    feed.show(TransportState {
        opens: None,
        ..playing()
    });
    h.relayout_at((1400, 700)).expect("layout settles");
    press(&mut h, "transport.art");
    assert_eq!(feed.take_events(), Vec::new(), "a picture opens nothing");
}

/// A drag across the band skips: leftwards to the next song, rightwards
/// to the previous one; a short one is a press on what is under it.
#[test]
fn a_swipe_across_the_band_skips() {
    let (mut h, feed) = banded(playing(), 1400);
    let art = control(&h, "transport.art").expect("the art is there");
    let reach = 2.0 * f64::from(Theme::dark().tokens.metrics.min_pointer_target) + 4.0;
    let start = art.center();
    pointer(&mut h, start, Some(ElementState::Pressed));
    pointer(&mut h, Point::new(start.x + reach, start.y), None);
    pointer(
        &mut h,
        Point::new(start.x + reach, start.y),
        Some(ElementState::Released),
    );
    assert_eq!(feed.take_events(), vec![TransportEvent::Previous]);
    let from = Point::new(start.x + reach, start.y);
    pointer(&mut h, from, Some(ElementState::Pressed));
    pointer(&mut h, start, Some(ElementState::Released));
    assert_eq!(feed.take_events(), vec![TransportEvent::Next]);
}

/// Touch parity: a finger dragged across the band claims the drag, so it
/// swipes as the pointer does instead of scrolling.
#[test]
fn a_finger_on_the_band_swipes() {
    let (h, _) = banded(playing(), 1400);
    let art = control(&h, "transport.art").expect("the art is there");
    let transport = band_transport(&h);
    assert_eq!(
        Widget::touch_claim(transport, art.center()),
        crate::interaction::TouchClaim::Drag
    );
    let play = control(&h, "transport.play").expect("play is there");
    assert_ne!(
        Widget::touch_claim(transport, play.center()),
        crate::interaction::TouchClaim::Drag,
        "a control is pressed, not swiped"
    );
}

/// The transport in the band.
fn band_transport(h: &FrameHarness) -> &dyn Widget {
    fn walk(widget: &dyn Widget) -> Option<&dyn Widget> {
        if widget.name() == "MediaTransport" {
            return Some(widget);
        }
        widget.children().into_iter().find_map(walk)
    }
    walk(h.root::<NavShell>()).expect("the transport is in the band")
}

/// The volume is reachable at every width: where the slider doesn't fit
/// beside the mute toggle, the speaker's popover holds it.
#[test]
fn the_volume_is_reachable_at_every_width() {
    for width in [480, 900] {
        let (mut h, feed) = banded(playing(), width);
        assert_eq!(control(&h, "transport.volume"), None, "no inline slider");
        press(&mut h, "transport.volume-menu");
        h.relayout_at((width, 700)).expect("layout settles");
        let slider = control(&h, "transport.volume-menu.slider")
            .unwrap_or_else(|| panic!("the popover holds the slider at {width}"));
        h.click_at(slider.x0 + slider.width() * 0.9, slider.center().y);
        let events = feed.take_events();
        assert!(
            matches!(events.as_slice(), [.., TransportEvent::Volume(v)] if *v > 0.6),
            "the slider sets the volume at {width}: {events:?}"
        );
    }
}
