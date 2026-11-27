//! The media transport in a NavShell's band.

use std::collections::HashMap;
use tilo_actions::ActionBinding;
use tilo_ui_app::app::driver::Driver;
use tilo_ui_app::state::State;
use tilo_ui_app::widget::internals::media_transport::{Metrics, layout_for_test};
use tilo_ui_app::widget::media_transport::*;
use tilo_ui_app::{widget::NavShell, widget::Text};
use tilo_ui_base::theme::Theme;
use tilo_ui_base::widget::ContentKind;
use tilo_ui_base::widget::Widget;
use vello::kurbo::Point;
use vello::kurbo::Rect;
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

const PLAYER: &str = "test.player:player";

fn player() -> tilo_actions::ResourceSpec {
    let mut player = tilo_actions::ResourceSpec::new(PLAYER, "test.player", "Player");
    for id in [
        "play-pause",
        "previous",
        "next",
        "toggle-shuffle",
        "cycle-repeat",
        "toggle-like",
        "toggle-mute",
        "seek",
        "set-volume",
    ] {
        player = player.with_action(
            tilo_actions::ActionSpec::operation(
                id,
                id,
                tilo_actions::OperationKind::Effect,
                "test.player.effect",
                &id,
            )
            .expect("a test action"),
        );
    }
    player
}

fn actions() -> TransportActions {
    let player = player();
    let unit = |id| Some(ActionBinding::unit_for(&player, id).expect("published"));
    let value = |id: &str| {
        let spec = player
            .actions
            .iter()
            .find(|spec| spec.id == id)
            .expect("published");
        Some(ActionBinding::new(&player, spec, |_: f64| serde_json::Map::new()).expect("bound"))
    };
    TransportActions {
        play_pause: unit("play-pause"),
        previous: unit("previous"),
        next: unit("next"),
        shuffle: unit("toggle-shuffle"),
        repeat: unit("cycle-repeat"),
        like: unit("toggle-like"),
        mute: unit("toggle-mute"),
        seek: value("seek"),
        volume: value("set-volume"),
    }
}

fn banded(state: TransportState, w: u32) -> (Driver, TransportFeed) {
    banded_with(state, w, actions())
}

fn banded_with(
    state: TransportState,
    w: u32,
    actions: TransportActions,
) -> (Driver, TransportFeed) {
    let feed = TransportFeed::new();
    feed.show(state);
    let shell = NavShell::new()
        .with_id("player")
        .content_kind(ContentKind::List)
        .content(Box::new(Text::new("Page")))
        .bottom_band(Box::new(MediaTransport::new(&feed, actions)));
    let mut h = Driver::for_root(State::new(), shell, (w, 700), Theme::dark());
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

fn control(h: &Driver, id: &str) -> Option<Rect> {
    find(h.root::<NavShell>(), id)
        .map(Widget::get_rect)
        .filter(|rect| rect.width() > 0.0)
}

fn press(h: &mut Driver, id: &str) {
    let rect = control(h, id).unwrap_or_else(|| panic!("{id} is laid out"));
    h.click_at(rect.center().x, rect.center().y);
}

fn bindings(h: &Driver) -> HashMap<String, tilo_ui_base::widget::ControlBindingKind> {
    tilo_ui_base::widget::controls::control_action_bindings(band_transport(h))
        .into_iter()
        .map(|binding| (binding.control_id, binding.kind))
        .collect()
}

#[test]
fn each_control_invokes_the_action_the_app_handed_it() {
    let (h, _) = banded(playing(), 1400);
    let bindings = bindings(&h);
    for (control, action) in [
        ("transport.play", "play-pause"),
        ("transport.previous", "previous"),
        ("transport.next", "next"),
        ("transport.shuffle", "toggle-shuffle"),
        ("transport.repeat", "cycle-repeat"),
        ("transport.like", "toggle-like"),
        ("transport.mute", "toggle-mute"),
        ("transport.position", "seek"),
        ("transport.volume", "set-volume"),
    ] {
        assert_eq!(
            bindings.get(control),
            Some(&tilo_ui_base::widget::ControlBindingKind::Action(
                tilo_actions::ActionAddress::new(PLAYER, action)
            )),
            "{control}"
        );
    }
    // The queue and the lyrics show the app's own panels: presses the band hands back as events,
    // bound to no action of the app's.
    for id in ["transport.queue", "transport.lyrics"] {
        assert!(control(&h, id).is_some(), "{id} is shown");
        assert_eq!(bindings.get(id), None, "{id}");
    }
}

#[test]
fn a_control_without_an_action_is_left_out() {
    let (h, _) = banded_with(
        playing(),
        1400,
        TransportActions {
            shuffle: None,
            seek: None,
            volume: None,
            ..actions()
        },
    );
    for id in [
        "transport.shuffle",
        "transport.position",
        "transport.volume",
    ] {
        assert_eq!(control(&h, id), None, "{id} is left out");
    }
    assert!(control(&h, "transport.play").is_some());
    assert!(control(&h, "transport.repeat").is_some());
}

#[test]
fn the_state_follows_its_declaration() {
    let (mut h, feed) = banded(playing(), 1400);
    press(&mut h, "transport.play");
    assert_eq!(feed.take_events(), Vec::new(), "play is the app's action");
    let named = |h: &Driver| {
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

fn pointer(h: &mut Driver, at: Point, state: Option<ElementState>) {
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

#[test]
fn a_finger_on_the_band_swipes() {
    let (h, _) = banded(playing(), 1400);
    let art = control(&h, "transport.art").expect("the art is there");
    let transport = band_transport(&h);
    assert_eq!(
        Widget::touch_claim(transport, art.center()),
        tilo_ui_base::interaction::TouchClaim::Drag
    );
    let play = control(&h, "transport.play").expect("play is there");
    assert_ne!(
        Widget::touch_claim(transport, play.center()),
        tilo_ui_base::interaction::TouchClaim::Drag,
        "a control is pressed, not swiped"
    );
}

fn band_transport(h: &Driver) -> &dyn Widget {
    fn walk(widget: &dyn Widget) -> Option<&dyn Widget> {
        if widget.name() == "MediaTransport" {
            return Some(widget);
        }
        widget.children().into_iter().find_map(walk)
    }
    walk(h.root::<NavShell>()).expect("the transport is in the band")
}

#[test]
fn the_volume_is_reachable_at_every_width() {
    for width in [480, 900] {
        let (mut h, feed) = banded(playing(), width);
        assert_eq!(control(&h, "transport.volume"), None, "no inline slider");
        press(&mut h, "transport.volume-menu");
        h.relayout_at((width, 700)).expect("layout settles");
        control(&h, "transport.volume-menu.slider")
            .unwrap_or_else(|| panic!("the popover holds the slider at {width}"));
        assert_eq!(
            bindings(&h).get("transport.volume-menu.slider"),
            Some(&tilo_ui_base::widget::ControlBindingKind::Action(
                tilo_actions::ActionAddress::new(PLAYER, "set-volume")
            )),
            "the slider sets the volume at {width}"
        );
        assert_eq!(feed.take_events(), Vec::new());
    }
}

/// The knob overhangs the slider's ends: resting at either end it keeps
/// the related gap to the elapsed and total times, never covering them.
#[test]
fn the_knob_at_either_end_clears_the_times() {
    let mut m = Metrics::of(&Theme::dark());
    m.time = 32.0;
    let reach = tilo_ui_app::widget::SliderFace::overhang();
    for width in [360.0, 900.0, 1400.0, 3440.0] {
        let (form, rects, _) = layout_for_test(&playing(), &handed_for_test(&actions()), &m, width);
        let scrub = rects[SCRUBBER];
        assert!(
            scrub.x0 - reach >= rects[ELAPSED].x1 + m.related - 0.5,
            "{form:?} at {width}: the knob at 0:00 ({scrub:?}) clears {:?}",
            rects[ELAPSED]
        );
        assert!(
            scrub.x1 + reach <= rects[TOTAL].x0 - m.related + 0.5,
            "{form:?} at {width}: the knob at the end ({scrub:?}) clears {:?}",
            rects[TOTAL]
        );
    }
}

/// At every width a window may be, each control the band shows stands inside it,
/// and the scrubber keeps a usable track between the times.
#[test]
fn the_band_fits_every_window_width() {
    let mut m = Metrics::of(&Theme::dark());
    m.time = 32.0;
    let names = [
        "art",
        "song",
        "like",
        "lyrics",
        "shuffle",
        "previous",
        "play",
        "next",
        "repeat",
        "elapsed",
        "scrubber",
        "total",
        "queue",
        "mute",
        "volume",
        "more",
        "volume-menu",
    ];
    for width in [320.0, 390.0, 480.0, 768.0, 1280.0, 1920.0, 3440.0] {
        let (form, rects, height) =
            layout_for_test(&playing(), &handed_for_test(&actions()), &m, width);
        for (index, rect) in rects.iter().enumerate() {
            if rect.area() <= 0.0 {
                continue;
            }
            assert!(
                rect.x0 >= -0.5 && rect.x1 <= width + 0.5,
                "{form:?} at {width}: {} at ({:.0}, {:.0})..({:.0}, {:.0}) leaves the band",
                names[index],
                rect.x0,
                rect.y0,
                rect.x1,
                rect.y1
            );
            assert!(
                rect.y0 >= -0.5 && rect.y1 <= height + 0.5,
                "{form:?} at {width}: {} at ({:.0}, {:.0})..({:.0}, {:.0}) leaves the band's {height:.0} height",
                names[index],
                rect.x0,
                rect.y0,
                rect.x1,
                rect.y1
            );
        }
    }
}

/// A band laid out for a finger gives the scrubber the square a finger reaches for, and the row of
/// controls above it the same: the controls themselves grow to their target, and a band that kept
/// the theme's smaller faces would give one less room than it takes.
#[test]
fn a_band_laid_out_for_a_finger_keeps_its_controls_their_target() {
    let mut bare = Metrics::of(&Theme::dark());
    bare.time = 32.0;
    let mut finger = bare;
    finger.target = 48.0;
    let mut checked = 0_usize;
    for width in [320.0, 390.0, 480.0, 768.0, 900.0, 1400.0, 3440.0] {
        let (form, rects, height) =
            layout_for_test(&playing(), &handed_for_test(&actions()), &finger, width);
        assert!(
            rects[SCRUBBER].height() >= finger.target - 0.5,
            "{form:?} at {width}: the scrubber is a target tall ({:?})",
            rects[SCRUBBER]
        );
        assert!(
            rects[SCRUBBER].y1 <= height + 0.5,
            "{form:?} at {width}: the scrubber stands in the band's {height:.0} height ({:?})",
            rects[SCRUBBER]
        );
        let (_, _, at_rest) =
            layout_for_test(&playing(), &handed_for_test(&actions()), &bare, width);
        assert!(
            height >= at_rest + finger.target - bare.scrub(),
            "{form:?} at {width}: the band grows for the target the controls take ({height:.0} \
             against {at_rest:.0})"
        );
        checked += 1;
    }
    assert!(
        checked >= 7,
        "the band was laid out at every width: {checked}"
    );
}

/// A band never lays one control over another, however narrow it is and however large the type:
/// where there is no room for the art beside the controls, the art is what gives way.
#[test]
fn a_narrow_band_lays_no_control_over_another() {
    let mut m = Metrics::of(&Theme::dark());
    m.time = 32.0;
    m.target = 48.0;
    let mut checked = 0_usize;
    for width in (180..=640).step_by(20) {
        let (form, rects, height) = layout_for_test(
            &playing(),
            &handed_for_test(&actions()),
            &m,
            f64::from(width),
        );
        for (first_index, first) in rects.iter().enumerate() {
            if first.area() <= 0.0 {
                continue;
            }
            for (second_index, second) in rects.iter().enumerate().skip(first_index + 1) {
                if second.area() <= 0.0 {
                    continue;
                }
                let both = first.intersect(*second);
                assert!(
                    both.area() <= 0.5,
                    "{form:?} at {width}: {first_index} ({first:?}) covers {second_index} ({second:?})"
                );
            }
        }
        assert!(height > 0.0, "{form:?} at {width}: the band has a height");
        checked += 1;
    }
    assert!(
        checked >= 20,
        "the band was laid out at every width: {checked}"
    );
}

/// A band told it is reached by a finger grows the scrubber's row to the square a finger reaches
/// for, and keeps the thin row the theme gives it for a mouse: the controls inside the band grow
/// there, and the band lays out what they take.
#[test]
fn a_band_reached_by_a_finger_grows_its_scrubber() {
    use tilo_ui_base::interaction::InteractionEnvironment;

    let (mut h, _) = banded(playing(), 390);
    h.set_interaction_environment(InteractionEnvironment::POINTER);
    h.relayout_at((390, 700)).expect("layout settles");
    let thin = control(&h, "transport.position")
        .expect("the scrubber is there")
        .height();

    h.set_interaction_environment(InteractionEnvironment::TOUCH);
    h.relayout_at((390, 700)).expect("layout settles");
    let finger = control(&h, "transport.position")
        .expect("the scrubber is there")
        .height();

    assert!(
        finger >= 48.0 - 0.5,
        "a finger's scrubber is a whole target tall: {finger:.0}"
    );
    assert!(
        finger > thin,
        "and taller than the mouse's {thin:.0}, which keeps the theme's row"
    );
}

/// The component a view builds for `ui.MediaTransport` hands the window the band's
/// manual region: its controls are placed by the band's own policy, never by a plain flow.
#[test]
fn a_declared_transport_hands_the_window_its_bands_layout() {
    use tilo_ui_app::widget::{DeclaredTransport, TransportTriggers};
    let triggers = TransportTriggers {
        play_pause: None,
        previous: None,
        next: None,
        shuffle: None,
        repeat: None,
        like: None,
        mute: None,
        seek: None,
        volume: None,
        lyrics: None,
        queue: None,
        open: None,
    };
    let transport = DeclaredTransport::new(playing(), triggers);
    assert!(
        tilo_ui_base::widget::Widget::manual_layout_policy(&transport).is_some(),
        "the band's region reaches the window through the component"
    );
    let mut visited = 0;
    let walked =
        tilo_ui_base::widget::Widget::for_each_layout_child(&transport, &mut |_| visited += 1);
    assert!(
        walked && visited == 1,
        "the band itself is the layout child"
    );

    let (width, height) = (900u32, 300u32);
    let mut h = Driver::new(State::new(), Box::new(transport), (width, height));
    for _ in 0..3 {
        h.relayout_at((width, height)).expect("layout settles");
    }
    let snapshot = h.inspection();
    let Some(band) = snapshot
        .root
        .nodes()
        .into_iter()
        .find(|node| node.widget == "MediaTransport")
    else {
        panic!("the band is laid out");
    };
    let [x, y, w, hgt] = band.rect;
    for node in band.nodes() {
        let [cx, cy, cw, ch] = node.rect;
        if cw <= 0.0 || ch <= 0.0 {
            continue;
        }
        assert!(
            cx >= x - 0.5 && cy >= y - 0.5 && cx + cw <= x + w + 0.5 && cy + ch <= y + hgt + 0.5,
            "{} at ({cx:.0}, {cy:.0}) {cw:.0}×{ch:.0} leaves the band at ({x:.0}, {y:.0}) {w:.0}×{hgt:.0}",
            node.widget
        );
    }
}

/// A band given a source plays it on the engine it is handed: what the view declares starts and
/// stops the sound, a moved place seeks it, and what the band shows is the sound's own place and
/// length — never a clock the view has to keep in step with.
#[test]
fn a_declared_transport_plays_its_source_and_shows_the_engines_place() {
    use tilo_ui_app::widget::{DeclaredTransport, MediaTransportProps};
    use tilo_ui_base::component::Described;

    let engine = media::MediaEngine::new_synthesized();
    let props = MediaTransportProps {
        title: "First Light".to_owned(),
        source: Some("file:///home/person/Music/first-light.wav".to_owned()),
        playing: true,
        volume: Some(0.5),
        engine: Some(engine),
        ..MediaTransportProps::default()
    };
    let mut transport = DeclaredTransport::from_props("root/0", props);
    tilo_ui_base::widget::Widget::sync(&mut transport, &(), &());
    let shown = transport.shown();
    assert!(shown.playing, "the engine plays what the view asked for");
    assert!(
        shown.duration_ms > 0.0,
        "the band shows the length the engine read, not the view's zero"
    );
    assert_eq!(shown.position_ms, 0.0, "it starts where it was opened");

    let paused = MediaTransportProps {
        title: "First Light".to_owned(),
        source: Some("file:///home/person/Music/first-light.wav".to_owned()),
        playing: false,
        volume: Some(0.5),
        ..MediaTransportProps::default()
    };
    assert!(Described::set_props(&mut transport, paused));
    tilo_ui_base::widget::Widget::sync(&mut transport, &(), &());
    assert!(!transport.shown().playing, "a paused view stops the sound");

    let sought = MediaTransportProps {
        title: "First Light".to_owned(),
        source: Some("file:///home/person/Music/first-light.wav".to_owned()),
        playing: false,
        volume: Some(0.5),
        position: Some(1.0),
        ..MediaTransportProps::default()
    };
    assert!(Described::set_props(&mut transport, sought));
    tilo_ui_base::widget::Widget::sync(&mut transport, &(), &());
    assert_eq!(
        transport.shown().position_ms,
        1_000.0,
        "the place the view declares is where the sound stands"
    );
}

/// A playing song's playhead ticks rather than animating: the window comes back at the next tick
/// for the elapsed second and the scrubber, and a window showing a song it declares settles. A
/// stopped playhead asks for neither.
#[test]
fn a_playing_song_ticks_its_playhead_rather_than_animating() {
    use tilo_ui_app::widget::{DeclaredTransport, MediaTransportProps};
    use tilo_ui_base::component::Described;

    let declared = |playing| MediaTransportProps {
        title: "First Light".to_owned(),
        source: Some("file:///home/person/Music/first-light.wav".to_owned()),
        playing,
        engine: Some(media::MediaEngine::new_synthesized()),
        ..MediaTransportProps::default()
    };
    let mut transport = DeclaredTransport::from_props("root/0", declared(true));
    Widget::sync(&mut transport, &(), &());
    assert!(
        transport.shown().playing,
        "the engine plays what the view declared"
    );
    assert!(
        !transport.needs_animation_frame(),
        "a playing playhead is no animation: a window showing it settles"
    );
    let now = tilo_ui_app::time::now();
    let tick = transport
        .next_wake_deadline()
        .expect("a playing playhead ticks");
    assert!(
        tick > now && tick <= now + tilo_ui_app::time::Duration::from_secs(1),
        "the playhead comes back within a tick, not before or long after: {tick:?} at {now:?}"
    );

    assert!(Described::set_props(&mut transport, declared(false)));
    Widget::sync(&mut transport, &(), &());
    assert!(!transport.shown().playing, "the view stopped the sound");
    assert_eq!(
        transport.next_wake_deadline(),
        None,
        "a stopped playhead asks for no frame"
    );
    assert!(
        !transport.needs_animation_frame(),
        "and no animation either"
    );
}
