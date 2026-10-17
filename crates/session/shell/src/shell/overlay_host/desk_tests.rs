//! The desk and the Super surface through the overlay host: the gestures,
//! typing, the pointer, the caret, interruptions and the widgets' controls,
//! on the production paths the compositor drives.
#![allow(
    clippy::arithmetic_side_effects,
    reason = "test timelines add milliseconds to one monotonic instant, and geometry is finite"
)]

use std::rc::Rc;
use std::time::{Duration, Instant};

use automerge::AutoCommit;
use tilo_desktop::notifications::{Addressed, Notification};
use tilo_desktop_model::Collaborator;
use ui::widget::Container;
use vello::kurbo::{Point, Rect};

use super::*;
use crate::shell::chrome_state::{Chrome, ChromeState};
use crate::shell::desk_widgets::GLANCE_AFTER_PAUSE;
use crate::shell::now_playing::tests::{FakeSource, player};
use crate::shell::now_playing::{PlaybackStatus, PlayerCommand};
use crate::shell::quick_settings::now_playing::{CardTarget, Transport};
use crate::shell::state::WorkspaceTree;

fn ms(t0: Instant, n: u64) -> Instant {
    t0 + Duration::from_millis(n)
}

/// A 1600 × 900 desk with a player and one other person in the workspace.
fn desk() -> (ShellOverlayHost, Chrome, Rc<FakeSource>) {
    let chrome = ChromeState::new(WorkspaceTree::bootstrap_demo());
    let mut host = ShellOverlayHost::with_fixture_content(Box::new(Container::new()))
        .with_chrome(chrome.clone());
    host.rect = Rect::new(0.0, 0.0, 1600.0, 900.0);
    let source = Rc::new(FakeSource {
        players: vec![player("spotify", "Spotify", PlaybackStatus::Playing)],
        ..FakeSource::default()
    });
    host.now_playing_source = source.clone();
    {
        let state = chrome.borrow();
        let mut tree = state.tree_mut();
        tree.desktop
            .collaborators
            .push(Collaborator::new("mara", "Mara"));
    }
    (host, chrome, source)
}

fn forms(host: &ShellOverlayHost) -> (Form, Form) {
    (
        host.desk_form(WidgetId::Music),
        host.desk_form(WidgetId::Team),
    )
}

#[test]
fn super_tap_opens_the_surface_and_it_stays_until_closed() {
    let (mut host, _chrome, _) = desk();
    let doc = AutoCommit::new();
    let now = Instant::now();
    host.super_gesture(SuperGesture::Tap, &doc, &automerge::ROOT);
    assert_eq!(host.super_surface(), Some(SuperSurface::Pinned));
    assert!(host.palette_visible, "search is the surface's field");
    assert!(
        host.palette.shows_field_only(),
        "empty, nothing listed under it"
    );
    host.update_desk(now);
    assert_eq!(
        forms(&host),
        (Form::Open, Form::Open),
        "every widget open around it"
    );
    host.update_desk(ms(now, 60_000));
    assert_eq!(
        host.super_surface(),
        Some(SuperSurface::Pinned),
        "no timer closes it"
    );

    // Typing searches as it always has: the field grows into results.
    host.palette.set_query("spo");
    assert!(!host.palette.shows_field_only());

    // Another tap closes it.
    host.super_gesture(SuperGesture::Tap, &doc, &automerge::ROOT);
    assert_eq!(host.super_surface(), None);
    assert!(!host.palette_visible);
    host.update_desk(ms(now, 60_100));
    assert_eq!(forms(&host), (Form::Glance, Form::Glance));

    // Escape (or a click outside) closes the palette; the surface follows.
    host.super_gesture(SuperGesture::Tap, &doc, &automerge::ROOT);
    host.palette_visible = false;
    host.update_desk(ms(now, 60_200));
    assert_eq!(host.super_surface(), None);
    assert!(
        !host.palette.shows_field_only(),
        "plain search opens with its list again"
    );
}

#[test]
fn holding_super_peeks_and_release_closes_unless_it_was_used() {
    let (mut host, _chrome, _) = desk();
    let doc = AutoCommit::new();
    host.super_gesture(SuperGesture::Peek, &doc, &automerge::ROOT);
    assert_eq!(host.super_surface(), Some(SuperSurface::Peek));
    assert!(host.palette_visible);
    host.super_gesture(SuperGesture::PeekReleased, &doc, &automerge::ROOT);
    assert_eq!(
        host.super_surface(),
        None,
        "nothing typed: release closes it"
    );
    assert!(!host.palette_visible);

    host.super_gesture(SuperGesture::Peek, &doc, &automerge::ROOT);
    host.palette.set_query("not");
    host.super_gesture(SuperGesture::PeekReleased, &doc, &automerge::ROOT);
    assert_eq!(
        host.super_surface(),
        Some(SuperSurface::Pinned),
        "typed: it stays"
    );
    host.close_super_surface();

    host.super_gesture(SuperGesture::Peek, &doc, &automerge::ROOT);
    host.super_gesture(SuperGesture::PeekTaken, &doc, &automerge::ROOT);
    host.super_gesture(SuperGesture::PeekReleased, &doc, &automerge::ROOT);
    assert_eq!(
        host.super_surface(),
        Some(SuperSurface::Pinned),
        "clicked into: it stays"
    );
    host.close_super_surface();

    host.super_gesture(SuperGesture::Peek, &doc, &automerge::ROOT);
    host.super_gesture(SuperGesture::PeekCancelled, &doc, &automerge::ROOT);
    assert_eq!(host.super_surface(), None, "a chord after all");
    assert!(!host.palette_visible);
}

#[test]
fn typing_sends_widgets_away_and_a_pause_brings_them_back() {
    let (mut host, chrome, _) = desk();
    let t0 = Instant::now();
    host.update_desk(t0);
    assert_eq!(forms(&host), (Form::Glance, Form::Glance));
    host.note_client_typing(Some(t0), t0);
    assert_eq!(forms(&host), (Form::Away, Form::Away));
    assert!(chrome.borrow().typing_at(t0));
    assert_eq!(
        host.desk_wake_deadline(ms(t0, 1000)),
        Some(t0 + GLANCE_AFTER_PAUSE),
        "one wake at the pause, no frames before it"
    );
    host.update_desk(ms(t0, 2900));
    assert_eq!(forms(&host), (Form::Away, Form::Away));
    host.update_desk(t0 + GLANCE_AFTER_PAUSE);
    assert_eq!(forms(&host), (Form::Glance, Form::Glance));
    assert!(!chrome.borrow().typing_at(t0 + GLANCE_AFTER_PAUSE));
}

#[test]
fn only_what_is_addressed_to_the_person_surfaces_while_they_type() {
    let (mut host, chrome, _) = desk();
    let t0 = Instant::now();
    host.note_client_typing(Some(t0), t0);
    let news = chrome
        .borrow_mut()
        .push_notification_full(Notification::new("Build finished", "main is green"));
    let mention = chrome.borrow_mut().push_notification_full(
        Notification::new("Mara", "Can you look at the layout?").with_from("Mara", "mara"),
    );
    let call = chrome.borrow_mut().push_notification_full(
        Notification::new("Jonas is calling", "").with_addressed(Addressed::IncomingCall),
    );
    let shown = |chrome: &Chrome| {
        chrome
            .borrow()
            .notifications()
            .iter()
            .map(|n| n.id)
            .collect::<Vec<_>>()
    };
    assert!(
        shown(&chrome).contains(&mention),
        "a mention surfaces during typing"
    );
    assert!(
        shown(&chrome).contains(&call),
        "a call surfaces during typing"
    );
    assert!(!shown(&chrome).contains(&news), "news waits");
    assert_eq!(chrome.borrow().waiting_banner_count(), 1);
    host.update_desk(ms(t0, 1000));
    assert!(!shown(&chrome).contains(&news), "still typing");
    host.update_desk(t0 + GLANCE_AFTER_PAUSE);
    assert!(
        shown(&chrome).contains(&news),
        "it surfaces once they pause"
    );
    assert_eq!(chrome.borrow().waiting_banner_count(), 0);
}

#[test]
fn each_widget_honours_its_own_setting() {
    let (mut host, chrome, _) = desk();
    let t0 = Instant::now();
    let strip = chrome
        .borrow()
        .active_strip_id()
        .map(|strip| strip.0)
        .expect("the demo has a strip");
    let mut prefs = host.desk_widget_prefs().clone();
    prefs.music.show = ShowMode::Ask;
    host.set_desk_widget_prefs(prefs);
    host.update_desk(t0);
    assert_eq!(forms(&host), (Form::Away, Form::Glance), "only when I ask");
    // "Always show on this strip", from the Team card's pin.
    host.toggle_widget_pin(WidgetId::Team);
    assert_eq!(host.desk_widget_prefs().team.show, ShowMode::Always);
    assert_eq!(host.desk_widget_prefs().team.always_on_strips, vec![strip]);
    host.note_client_typing(Some(ms(t0, 10)), ms(t0, 10));
    assert_eq!(
        forms(&host),
        (Form::Away, Form::Glance),
        "pinned stays through typing"
    );
    host.toggle_widget_pin(WidgetId::Team);
    assert_eq!(
        host.desk_widget_prefs().team.show,
        ShowMode::Pause,
        "unpinned"
    );
    host.update_desk(ms(t0, 20));
    assert_eq!(forms(&host).1, Form::Away);
}

#[test]
fn passing_an_edge_is_not_a_glance_but_reaching_for_it_is() {
    let (mut host, _chrome, _) = desk();
    let t0 = Instant::now();
    host.note_client_typing(Some(t0), t0);
    let team = host.desk_slot(WidgetId::Team).expect("team is laid out");
    // Down the right edge at speed, across Team's spot.
    for k in 0..40u32 {
        let y = 100.0 + 18.0 * f64::from(k);
        host.desk_pointer(Point::new(1592.0, y), ms(t0, 20 + u64::from(k) * 8));
        assert_eq!(
            host.desk_form(WidgetId::Team),
            Form::Away,
            "transit at y {y}"
        );
    }
    // Now toward Team's spot from the middle, slowing to a stop there.
    let target = team.glance.center();
    let from = Point::new(900.0, target.y + 40.0);
    for (k, f) in [0.0, 0.5, 0.8, 0.95, 1.0, 1.0].into_iter().enumerate() {
        let p = from + (target - from) * f;
        host.desk_pointer(p, ms(t0, 600 + u64::try_from(k).unwrap() * 8));
    }
    assert!(host.desk.board.typing(ms(t0, 700)));
    assert_eq!(
        host.desk_form(WidgetId::Team),
        Form::Glance,
        "aim brings it while typing"
    );
    assert_eq!(host.desk_form(WidgetId::Music), Form::Away, "and only it");
}

#[test]
fn nothing_covers_the_caret() {
    let (mut host, _chrome, _) = desk();
    let t0 = Instant::now();
    host.update_desk(t0);
    let music = host.desk_slot(WidgetId::Music).expect("music is laid out");
    host.set_text_caret(Some(Rect::from_center_size(
        music.glance.center(),
        (2.0, 18.0),
    )));
    host.update_desk(ms(t0, 10));
    assert_eq!(host.desk_form(WidgetId::Music), Form::Away);
    assert_eq!(
        host.desk_form(WidgetId::Team),
        Form::Glance,
        "a widget elsewhere stays"
    );
    host.set_text_caret(Some(Rect::new(300.0, 200.0, 302.0, 218.0)));
    host.update_desk(ms(t0, 20));
    assert_eq!(host.desk_form(WidgetId::Music), Form::Glance);
}

#[test]
fn music_controls_reach_the_player_without_switching_workspaces() {
    let (mut host, chrome, source) = desk();
    let doc = AutoCommit::new();
    // The person's own player runs in another workspace than this one.
    assert!(chrome.borrow_mut().switch_workspace(1));
    let here = chrome
        .borrow()
        .tree()
        .desktop
        .active_workspace()
        .map(|workspace| workspace.id);
    host.super_gesture(SuperGesture::Tap, &doc, &automerge::ROOT);
    let now = Instant::now();
    host.update_desk(now);
    assert_eq!(host.desk_form(WidgetId::Music), Form::Open);
    let slot = host.desk_slot(WidgetId::Music).expect("music is laid out");
    let view = host.music_view(now).expect("a player");
    let targets = host
        .desk
        .music_card
        .laid_out(card_rect(slot.open))
        .targets(&view);
    let at = |which: Transport| {
        targets
            .iter()
            .find(|(target, _)| *target == CardTarget::Transport(which))
            .map(|(_, rect)| rect.center())
            .expect("the card has the control")
    };
    for (which, command) in [
        (Transport::PlayPause, PlayerCommand::PlayPause),
        (Transport::Next, PlayerCommand::Next),
    ] {
        host.last_cursor = Some(at(which));
        assert!(host.route_desk_event(&ui::synthetic_input::mouse_press()));
        // The control took the press; its release is nobody's.
        host.route_desk_event(&ui::synthetic_input::mouse_release());
        assert_eq!(
            source.sent.borrow().last(),
            Some(&("org.mpris.MediaPlayer2.spotify".to_string(), command))
        );
    }
    assert_eq!(
        chrome
            .borrow()
            .tree()
            .desktop
            .active_workspace()
            .map(|workspace| workspace.id),
        here,
        "the controls never switch workspaces"
    );
    assert_eq!(
        host.super_surface(),
        Some(SuperSurface::Pinned),
        "and keep the surface"
    );
}

#[test]
fn a_click_opens_a_glance_and_a_drag_moves_it_to_another_edge() {
    let (mut host, _chrome, _) = desk();
    let t0 = Instant::now();
    host.update_desk(t0);
    let music = host.desk_slot(WidgetId::Music).expect("music is laid out");
    host.last_cursor = Some(music.glance.center());
    assert!(host.route_desk_event(&ui::synthetic_input::mouse_press()));
    assert!(host.route_desk_event(&ui::synthetic_input::mouse_release()));
    assert_eq!(host.desk_form(WidgetId::Music), Form::Open);
    host.desk.board.close();
    host.update_desk(ms(t0, 10));

    host.last_cursor = Some(music.glance.center());
    assert!(host.route_desk_event(&ui::synthetic_input::mouse_press()));
    let drop_at = Point::new(60.0, 860.0);
    host.desk_pointer(drop_at, ms(t0, 40));
    host.last_cursor = Some(drop_at);
    assert!(host.route_desk_event(&ui::synthetic_input::mouse_release()));
    assert_eq!(
        host.desk_widget_prefs().music.edge,
        DockEdge::BottomLeft,
        "it snaps there"
    );
    host.update_desk(ms(t0, 60));
    let moved = host.desk_slot(WidgetId::Music).expect("still laid out");
    assert!(
        moved.glance.x0 < 100.0 && moved.glance.y1 > 800.0,
        "{moved:?}"
    );
}

#[test]
fn widgets_stay_away_under_other_surfaces_and_beside_a_live_call() {
    let (mut host, chrome, _) = desk();
    let t0 = Instant::now();
    chrome.borrow_mut().overlays.quick_settings = true;
    host.update_desk(t0);
    assert_eq!(
        forms(&host),
        (Form::Away, Form::Away),
        "Quick Settings owns the screen"
    );
    chrome.borrow_mut().overlays.quick_settings = false;
    let call = Rect::new(1300.0, 640.0, 1590.0, 890.0);
    host.set_system_activity_rects(vec![call]);
    host.update_desk(ms(t0, 10));
    let music = host.desk_slot(WidgetId::Music).expect("music is laid out");
    assert!(
        !music.glance.overlaps(call),
        "the call keeps its place: {music:?}"
    );
    assert_eq!(host.desk_form(WidgetId::Music), Form::Glance);
}
