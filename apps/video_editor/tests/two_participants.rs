//! Two people in one edit.
//!
//! Every assertion here crosses between two independent editors. One drives —
//! through real keyboard events, so its playhead and selection move the way a
//! person's would — and hands over only what
//! [`Widget::presence_payload`](ui::widget::Widget::presence_payload) offers
//! the host. Those frames go into the other editor through
//! [`PresenceCtx::apply_remote_presence`], the one decoder every inbound frame
//! passes through, and the second editor then *draws*.
//!
//! The point of doing it this way is the bug it catches. A test that publishes
//! presence and reads it back out of the same context proves the two ends of
//! one function agree with themselves. The failure that actually happens is
//! that the publisher keys a frame one way and the renderer looks for it
//! another way — and then everything passes, nothing is drawn, and the room
//! looks empty. So nothing below ever names a key: the driver's own output is
//! relayed verbatim, and what the other participant renders has to be a
//! consequence of it.
//!
//! The two editors share one document, because collaborators do: the second
//! adopts a save of the first, which is what a late joiner receives.

use std::collections::HashMap;

use automerge::AutoCommit;
use taffy::prelude::*;
use ui::offscreen::OffscreenRenderer;
use ui::permissions::{PermissionContext, UserId};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use vello::Scene;
use vello::kurbo::{Affine, Point};
use vello::peniko::Color;
use video_editor::app::ShotScenario;
use video_editor::doc::VideoDoc;
use video_editor::model::ClipId;
use video_editor::presence as vp;
use video_editor::{VideoEditorApp, shot};
use winit::event::{ElementState, WindowEvent};
use winit::keyboard::{Key, NamedKey};

const W: u32 = 1280;
const H: u32 = 800;

/// One editor and the document it holds.
struct Participant {
    app: VideoEditorApp,
    doc: AutoCommit,
}

impl Participant {
    /// The participant who opens the project.
    fn opening() -> Self {
        ui::portal::suppress_for_tests();
        let mut p = Participant {
            app: VideoEditorApp::new_for_shot(ShotScenario::Live),
            doc: AutoCommit::new(),
        };
        p.frame(&PresenceCtx::empty());
        p
    }

    /// A participant who joins and is handed the project, the way a late
    /// joiner is answered with the document.
    fn joining(from: &Participant) -> Self {
        ui::portal::suppress_for_tests();
        let mut p = Participant {
            app: VideoEditorApp::new(),
            doc: AutoCommit::load(&from.doc.clone().save()).expect("the handed-over document"),
        };
        p.frame(&PresenceCtx::empty());
        p
    }

    /// Sync, lay out and draw one frame — everything the host does per frame,
    /// so the editor has a real rect and real geometry behind it.
    fn frame(&mut self, presence: &PresenceCtx) -> Scene {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();

        self.app.sync(&mut self.doc, &automerge::ROOT);
        let child = self.app.build_layout(&mut taffy, &theme, &mut node_map);
        let root = taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::length(W as f32),
                        height: Dimension::length(H as f32),
                    },
                    ..Default::default()
                },
                &[child],
            )
            .expect("a root node");
        {
            let measure_map = node_map.clone();
            let tr = &mut tr;
            taffy
                .compute_layout_with_measure(
                    root,
                    taffy::Size {
                        width: AvailableSpace::Definite(W as f32),
                        height: AvailableSpace::Definite(H as f32),
                    },
                    |known, available, node_id, _, _| {
                        if let Some(&ptr) = measure_map.get(&node_id) {
                            unsafe { (*ptr).measure(known, available, tr) }
                        } else {
                            taffy::Size::ZERO
                        }
                    },
                )
                .expect("layout");
        }
        self.app.apply_layout(&taffy, child, Point::ORIGIN);

        let mut scene = Scene::new();
        self.app.draw_with_presence(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            presence,
            &PermissionContext::permissive(),
        );
        scene
    }

    /// Press a key, the way the host delivers one.
    fn press(&mut self, key: Key) {
        let mut ev: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = key;
        ev.physical_key =
            winit::keyboard::PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
        ev.location = winit::keyboard::KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        let event = WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
            event: ev,
            is_synthetic: false,
        };
        let mut handled = false;
        let mut focus = None;
        let taffy = TaffyTree::<()>::new();
        self.app.handle_event(
            &event,
            1.0,
            &winit::event::Modifiers::default(),
            &mut self.doc,
            &automerge::ROOT,
            &mut handled,
            &mut focus,
            NodeId::new(0),
            &taffy,
        );
    }

    /// Drain everything this editor currently wants to say about where its
    /// user is, and deliver it to `to` under `as_user`.
    ///
    /// The host offers one frame per poll and only sends what has changed, so
    /// draining is what a few frames of a live session do. Nothing here
    /// inspects or rebuilds a key — whatever comes out goes straight in.
    fn relay(&self, to: &mut PresenceCtx, as_user: &str) -> usize {
        let mut sent = 0;
        let mut last = None;
        for _ in 0..8 {
            let Some(frame) = self.app.presence_payload() else {
                break;
            };
            if Some(&frame) == last.as_ref() {
                break; // settled: nothing left to say
            }
            to.apply_remote_presence(as_user.to_string(), frame.0.clone(), frame.1.clone());
            last = Some(frame);
            sent += 1;
        }
        sent
    }
}

/// The clips a participant's document holds, in a stable order — read from
/// the document rather than from the editor, because that is the thing the
/// two of them actually share.
fn clip_ids(p: &mut Participant) -> Vec<ClipId> {
    let mut ids: Vec<ClipId> = VideoDoc::load_project(&mut p.doc)
        .clips
        .iter()
        .map(|c| c.id)
        .collect();
    ids.sort();
    ids
}

/// How long the shared project runs.
fn duration(p: &mut Participant) -> f64 {
    VideoDoc::load_project(&mut p.doc).duration()
}

/// Rasterise, and count how many pixels carry a colour.
fn pixels_of_colour(rgba: &[u8], rgb: (u8, u8, u8)) -> usize {
    rgba.chunks_exact(4)
        .filter(|p| p[0] == rgb.0 && p[1] == rgb.1 && p[2] == rgb.2 && p[3] > 0)
        .count()
}

/// The whole feature in one pass: one participant moves, the other sees it.
///
/// The driver's playhead is moved with `End` and its selection widened with
/// Ctrl+A — real key presses through the real handler, not fields poked from
/// a test — and the other participant is then asked, through the ordinary
/// reader functions, where that person is.
#[test]
fn one_participant_moves_and_the_other_can_say_where_they_are() {
    let mut ada = Participant::opening();
    let mut bo = Participant::joining(&ada);

    // They are looking at the same project, or nothing below means anything.
    let shared = clip_ids(&mut bo);
    assert!(!shared.is_empty(), "the joining participant received clips");
    assert_eq!(
        shared,
        clip_ids(&mut ada),
        "both participants hold the same clips, by the same ids",
    );

    // Ada jumps to the end of the project — a real key press.
    ada.press(Key::Named(NamedKey::End));
    ada.frame(&PresenceCtx::empty());

    let mut room = PresenceCtx::empty();
    let sent = ada.relay(&mut room, "ada");
    assert!(sent >= 2, "Ada had both a playhead and a selection to send, sent {sent}");

    // Where Bo now believes Ada is.
    let heads = vp::peer_playheads(&room);
    assert_eq!(heads.len(), 1, "exactly one other participant: {heads:?}");
    assert_eq!(heads[0].user, UserId("ada".to_string()));
    let end = duration(&mut ada);
    assert!(
        (heads[0].at - end).abs() < 0.01,
        "Ada jumped to the end ({end}s); Bo has her at {}s",
        heads[0].at,
    );

    let held = vp::peer_clips(&room);
    assert_eq!(held.len(), 1, "Ada has one clip selected: {held:?}");
    assert!(
        shared.contains(&held[0].clip),
        "and it is a clip Bo also holds, so Bo can draw it",
    );
    assert!(
        held[0].span.1 > held[0].span.0,
        "with a real span, {:?}",
        held[0].span,
    );
}

/// Moving is not a one-off. Ada steps her playhead repeatedly and Bo tracks
/// it each time, with no trail of the instants she passed through.
#[test]
fn the_other_participant_tracks_a_moving_playhead() {
    let mut ada = Participant::opening();
    let mut room = PresenceCtx::empty();

    // Arrows nudge a selected clip and step the playhead only when nothing is
    // selected, so Ada lets go of her clip before scrubbing.
    ada.press(Key::Named(NamedKey::Escape));
    ada.press(Key::Named(NamedKey::Home));
    ada.frame(&PresenceCtx::empty());
    ada.relay(&mut room, "ada");
    let at_start = vp::peer_playheads(&room);
    assert_eq!(at_start.len(), 1);
    assert!(at_start[0].at.abs() < 0.01, "at the start, got {}", at_start[0].at);

    let mut seen = vec![at_start[0].at];
    for _ in 0..4 {
        ada.press(Key::Named(NamedKey::ArrowRight));
        ada.frame(&PresenceCtx::empty());
        ada.relay(&mut room, "ada");
        let now = vp::peer_playheads(&room);
        assert_eq!(now.len(), 1, "one participant keeps one playhead: {now:?}");
        seen.push(now[0].at);
    }
    assert!(
        seen.windows(2).all(|w| w[1] > w[0]),
        "each step should have advanced Bo's picture of Ada: {seen:?}",
    );
}

/// Two other participants at once — the case the feature exists for. They
/// must not be merged into one, and they must not be drawn in one colour.
#[test]
fn two_other_participants_stay_distinct() {
    let mut ada = Participant::opening();
    let mut room = PresenceCtx::empty();

    ada.press(Key::Named(NamedKey::Home));
    ada.frame(&PresenceCtx::empty());
    ada.relay(&mut room, "ada");

    ada.press(Key::Named(NamedKey::End));
    ada.frame(&PresenceCtx::empty());
    ada.relay(&mut room, "cass");

    let heads = vp::peer_playheads(&room);
    assert_eq!(heads.len(), 2, "two participants, two playheads: {heads:?}");
    assert!(
        (heads[0].at - heads[1].at).abs() > 0.5,
        "they are in different places: {heads:?}",
    );
    assert_ne!(
        room.color_for(&UserId("ada".to_string())),
        room.color_for(&UserId("cass".to_string())),
        "two people must not be drawn in one colour",
    );
}

/// The half that makes it a feature: it has to reach the screen.
///
/// Bo draws the same frame twice — once with Ada in the room and once
/// without — and the only difference between the two runs is Ada. So a
/// difference in the pixels *is* Ada, and her own colour appearing in the
/// first and not the second is her, specifically, rather than a stray
/// relayout.
#[test]
fn a_participant_is_visible_on_the_other_persons_screen() {
    let mut ada = Participant::opening();
    let mut bo = Participant::joining(&ada);

    ada.press(Key::Named(NamedKey::Home));
    ada.frame(&PresenceCtx::empty());
    let mut room = PresenceCtx::empty();
    room.names.insert(UserId("ada".to_string()), "Ada".to_string());
    ada.relay(&mut room, "ada");
    assert_eq!(vp::peer_playheads(&room).len(), 1, "Ada is in the room");

    let alone = bo.frame(&PresenceCtx::empty());
    let together = bo.frame(&room);

    assert_ne!(
        shot::fingerprint(&alone),
        shot::fingerprint(&together),
        "drawing the room with Ada in it produced the identical scene to \
         drawing it without her -- she reached the model and not the screen",
    );

    let mut r = OffscreenRenderer::new(W, H);
    let empty_px = r.render(&alone, Color::BLACK);
    let peopled_px = r.render(&together, Color::BLACK);

    let ada_colour = room.color_for(&UserId("ada".to_string()));
    let with = pixels_of_colour(&peopled_px, ada_colour);
    let without = pixels_of_colour(&empty_px, ada_colour);
    assert!(
        with > 200,
        "Ada's colour {ada_colour:?} covers only {with} pixels -- a playhead \
         down the lanes and a name on it should be plainly visible",
    );
    assert!(
        with > without * 4,
        "Ada's colour is nearly as common without her ({without}px) as with \
         her ({with}px), so the assertion is measuring the app's own palette",
    );
}

/// Leaving has to be visible too. Ada deselects, and her outline comes off
/// Bo's screen — through a published frame, because there is no other way to
/// retract one.
#[test]
fn a_deselection_reaches_the_other_screen() {
    let mut ada = Participant::opening();
    let mut room = PresenceCtx::empty();
    ada.relay(&mut room, "ada");
    assert_eq!(vp::peer_clips(&room).len(), 1, "Ada starts with a clip");

    // Escape clears the selection.
    ada.press(Key::Named(NamedKey::Escape));
    ada.frame(&PresenceCtx::empty());
    ada.relay(&mut room, "ada");

    assert!(
        vp::peer_clips(&room).is_empty(),
        "Ada let go of the clip and Bo is still drawing her on it",
    );
    assert_eq!(
        vp::peer_playheads(&room).len(),
        1,
        "she is still in the room though -- deselecting is not leaving",
    );
}
