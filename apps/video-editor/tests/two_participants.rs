#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used,
    reason = "collaboration integration fixture asserts setup failures directly"
)]

//! Two people in one edit.
//!
//! Every assertion here crosses between two independent editors. One drives —
//! through real keyboard events, so its playhead and selection move the way a
//! person's would — and hands over only what
//! [`Widget::presence_payload`](ui::presence::capability::presence_payload) offers
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

use automerge::AutoCommit;
use taffy::{NodeId, TaffyTree};
use ui::headless::{Framed, viewport};
use ui::offscreen::OffscreenRenderer;
use ui::permissions::UserId;
use ui::presence::PresenceCtx;
use ui::theme::Theme;
use ui::widget::Widget;
use vello::Scene;
use vello::peniko::Color;
use video_editor::app::ShotScenario;
use video_editor::doc::VideoDoc;
use video_editor::model::{ClipId, MediaKind};
use video_editor::presence as vp;
use video_editor::{VideoEditorApp, shot};
use winit::event::{ElementState, WindowEvent};
use winit::keyboard::{Key, NamedKey};

const W: u32 = 1280;
const H: u32 = 800;
const W_F32: f32 = 1280.0;
const H_F32: f32 = 800.0;

/// One editor and the document it holds, mounted in the production frame host.
struct Participant {
    app: Framed<VideoEditorApp>,
}

impl Participant {
    /// The participant who opens the project.
    fn opening() -> Self {
        ui::host::install_default(ui::host::HostServices::recording().0);
        Self::mount(
            VideoEditorApp::new_for_shot(ShotScenario::Live),
            AutoCommit::new(),
        )
    }

    /// A participant who joins and is handed the project, the way a late
    /// joiner is answered with the document.
    fn joining(from: &Participant) -> Self {
        ui::host::install_default(ui::host::HostServices::recording().0);
        Self::mount(
            VideoEditorApp::new(),
            AutoCommit::load(&from.doc().clone().save()).expect("the handed-over document"),
        )
    }

    /// Mounting runs the first frame, drawn with nobody else in the room.
    fn mount(app: VideoEditorApp, doc: AutoCommit) -> Self {
        let mut state = ui::State::new();
        state.adopt_doc(doc);
        Participant {
            app: Framed::new(state, app, viewport((W_F32, H_F32)), Theme::dark()),
        }
    }

    fn doc(&self) -> &AutoCommit {
        &Framed::harness(&self.app).state().doc
    }

    /// Sync, lay out and draw one frame — everything the host does per frame,
    /// so the editor has a real rect and real geometry behind it.
    fn frame(&mut self, presence: &PresenceCtx) -> Scene {
        let harness = Framed::harness_mut(&mut self.app);
        harness.update_presence(|room| {
            *room = presence.clone();
            true
        });
        harness
            .relayout_themed(viewport((W_F32, H_F32)), Theme::dark())
            .expect("layout");
        harness.scene().clone()
    }

    /// Press a key, the way the host delivers one.
    fn press(&mut self, key: Key) {
        let mut ev: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = key;
        ev.physical_key = winit::keyboard::PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified,
        );
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
        let (app, state) = Framed::split(&mut self.app);
        app.handle_event(
            &event,
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &winit::event::Modifiers::default(),
                state: &mut state.doc,
                path: &automerge::ROOT,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: NodeId::new(0),
                taffy: &taffy,
            },
        );
    }

    /// Drain everything this editor currently wants to say about where its
    /// user is, and deliver it to `to` under `as_user`.
    ///
    /// The host offers one frame per poll and only sends what has changed, so
    /// draining is what a few frames of a live session do. Nothing here
    /// inspects or rebuilds a key — whatever comes out goes straight in.
    fn relay(&self, to: &mut PresenceCtx, as_user: &str) -> usize {
        let mut sent: usize = 0;
        let mut last = None;
        for _ in 0..8 {
            let Some(frame) = ui::presence::capability::presence_payload(&*self.app) else {
                break;
            };
            if Some(&frame) == last.as_ref() {
                break; // settled: nothing left to say
            }
            to.apply_remote_presence(as_user.to_string(), frame.0.clone(), frame.1.clone());
            last = Some(frame);
            sent = sent
                .checked_add(1)
                .expect("the bounded relay count fits usize");
        }
        sent
    }
}

/// The clips a participant's document holds, in a stable order — read from
/// the document rather than from the editor, because that is the thing the
/// two of them actually share.
fn clip_ids(p: &Participant) -> Vec<ClipId> {
    let mut ids: Vec<ClipId> = VideoDoc::load_project(p.doc())
        .clips
        .iter()
        .map(|c| c.id)
        .collect();
    ids.sort();
    ids
}

/// How long the shared project runs.
fn duration(p: &Participant) -> f64 {
    VideoDoc::load_project(p.doc()).duration()
}

/// The colour a participant is drawn in: their identity colour, on the
/// theme's presence palette (what every peer overlay paints with).
fn drawn_colour(room: &PresenceCtx, who: &str) -> (u8, u8, u8) {
    let colour = Theme::dark()
        .tokens
        .colors
        .presence
        .of_rgb(room.color_for(&UserId(who.to_string())))
        .to_rgba8();
    (colour.r, colour.g, colour.b)
}

/// Rasterise, and count how many pixels carry a colour.
fn pixels_of_colour(rgba: &[u8], rgb: (u8, u8, u8)) -> usize {
    rgba.chunks_exact(4)
        .filter(|p| p[0] == rgb.0 && p[1] == rgb.1 && p[2] == rgb.2 && p[3] > 0)
        .count()
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario: each former test is a labelled case block with its own fixture"
)]
fn participants_locate_and_track_each_other() {
    // Case `one_participant_moves_and_the_other_can_say_where_they_are`:
    // The whole feature in one pass: one participant moves, the other sees it.
    //
    // The driver's playhead is moved with `End` and its selection widened with
    // Ctrl+A — real key presses through the real handler, not fields poked from
    // a test — and the other participant is then asked, through the ordinary
    // reader functions, where that person is.
    {
        let mut ada = Participant::opening();
        let bo = Participant::joining(&ada);

        // They are looking at the same project, or nothing below means anything.
        let shared = clip_ids(&bo);
        assert!(!shared.is_empty(), "the joining participant received clips");
        assert_eq!(
            shared,
            clip_ids(&ada),
            "both participants hold the same clips, by the same ids",
        );

        // Ada jumps to the end of the project — a real key press.
        ada.press(Key::Named(NamedKey::End));
        ada.frame(&PresenceCtx::empty());

        let mut room = PresenceCtx::empty();
        let sent = ada.relay(&mut room, "ada");
        assert!(
            sent >= 2,
            "Ada had both a playhead and a selection to send, sent {sent}"
        );

        // Where Bo now believes Ada is.
        let heads = vp::peer_playheads(&room);
        assert_eq!(heads.len(), 1, "exactly one other participant: {heads:?}");
        assert_eq!(heads[0].user, UserId("ada".to_string()));
        let end = duration(&ada);
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
    // Case `the_other_participant_tracks_a_moving_playhead`:
    // Moving is not a one-off. Ada steps her playhead repeatedly and Bo tracks
    // it each time, with no trail of the instants she passed through.
    {
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
        assert!(
            at_start[0].at.abs() < 0.01,
            "at the start, got {}",
            at_start[0].at
        );

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
    // Case `two_other_participants_stay_distinct`:
    // Two other participants at once — the case the feature exists for. They
    // must not be merged into one, and they must not be drawn in one colour.
    {
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
    // Case `a_participant_typing_in_a_field_is_keyed_to_a_shared_object`:
    // Typing is the third thing a participant can be doing, and it moves their
    // point off the ruler and into the words — one person, one caret.
    //
    // What matters on the wire is that the caret is keyed to something the other
    // participant can resolve. A field caret keyed to a local-only handle would
    // publish perfectly and land nowhere, and the room would look as though
    // nobody was typing.
    {
        let mut ada = Participant::opening();

        let mut room = PresenceCtx::empty();
        ada.relay(&mut room, "ada");
        assert_eq!(
            vp::peer_playheads(&room).len(),
            1,
            "before typing, Ada's point is her playhead",
        );

        // "T" adds a title and opens its words for editing.
        ada.press(Key::Character("t".into()));
        ada.frame(&PresenceCtx::empty());
        ada.relay(&mut room, "ada");

        // The title she just made, as the *document* holds it — which is what a
        // second participant would receive.
        let title = VideoDoc::load_project(ada.doc())
            .media
            .iter()
            .find(|m| m.kind == MediaKind::Text)
            .map(|m| m.id)
            .expect("pressing T added a title to the document");

        let carets = vp::peer_field_carets(&room, vp::FIELD_TITLE_WORDS, title);
        assert_eq!(
            carets.len(),
            1,
            "Ada's caret should be in the title's words, keyed to the title the \
         document holds; found {carets:?}",
        );
        assert_eq!(carets[0].0, UserId("ada".to_string()));
        assert!(
            vp::peer_playheads(&room).is_empty(),
            "and her ghost playhead steps aside while she types -- a person has \
         one point, not two",
        );

        // A joiner receives the very object that key names, so it resolves.
        let bo = Participant::joining(&ada);
        assert!(
            VideoDoc::load_project(bo.doc())
                .media
                .iter()
                .any(|m| m.id == title),
            "the title Ada's caret is keyed to reached the other participant",
        );
    }
}

#[test]
fn participants_presence_reaches_the_other_screen() {
    // Case `a_participant_is_visible_on_the_other_persons_screen`:
    // The half that makes it a feature: it has to reach the screen.
    //
    // Bo draws the same frame twice — once with Ada in the room and once
    // without — and the only difference between the two runs is Ada. So a
    // difference in the pixels *is* Ada, and her own colour appearing in the
    // first and not the second is her, specifically, rather than a stray
    // relayout.
    {
        let mut ada = Participant::opening();
        let mut bo = Participant::joining(&ada);

        ada.press(Key::Named(NamedKey::Home));
        ada.frame(&PresenceCtx::empty());
        let mut room = PresenceCtx::empty();
        room.names
            .insert(UserId("ada".to_string()), "Ada".to_string());
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

        let ada_colour = drawn_colour(&room, "ada");
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
    // Case `a_participants_playhead_alone_reaches_the_screen`:
    // A participant with nothing selected is still somewhere, and that somewhere
    // has to be on screen.
    {
        let mut ada = Participant::opening();
        let mut bo = Participant::joining(&ada);

        // Nothing selected, so the only thing Ada has to show is her playhead.
        ada.press(Key::Named(NamedKey::Escape));
        ada.frame(&PresenceCtx::empty());
        let mut room = PresenceCtx::empty();
        room.names
            .insert(UserId("ada".to_string()), "Ada".to_string());
        ada.relay(&mut room, "ada");
        assert_eq!(vp::peer_playheads(&room).len(), 1, "she has a playhead");
        assert!(vp::peer_clips(&room).is_empty(), "and nothing selected");

        let (with, without) = colour_on_screen(&mut bo, &room, "ada");
        assert!(
            with > without + 100,
            "Ada's playhead put only {} more pixels of her colour on screen than \
         her absence did ({without}) -- a line down every lane with her name \
         on it should be unmistakable",
            with.saturating_sub(without),
        );
    }
    // Case `a_participants_selected_clip_alone_reaches_the_screen`:
    // And a participant holding a clip has to be on it, whatever their playhead
    // is doing.
    {
        let ada = Participant::opening();
        let mut bo = Participant::joining(&ada);

        // A clip near the start of the timeline, so it is on screen at the
        // default zoom -- an outline on a clip nobody can see proves nothing.
        let project = VideoDoc::load_project(bo.doc());
        let clip = project
            .clips
            .iter()
            .min_by(|a, b| a.start.partial_cmp(&b.start).unwrap())
            .expect("the shared project has clips");

        let mut room = PresenceCtx::empty();
        room.names
            .insert(UserId("ada".to_string()), "Ada".to_string());
        let (key, payload) = vp::range_frame(Some((clip.id, clip.start, clip.end())));
        room.apply_remote_presence("ada".to_string(), key, payload);
        assert_eq!(vp::peer_clips(&room).len(), 1, "she is holding a clip");
        assert!(vp::peer_playheads(&room).is_empty(), "and nothing else");

        let (with, without) = colour_on_screen(&mut bo, &room, "ada");
        assert!(
            with > without + 100,
            "Ada's held clip put only {} more pixels of her colour on screen than \
         her absence did ({without}) -- a ring round the clip and her name on \
         it should be unmistakable",
            with.saturating_sub(without),
        );
    }
    // Case `a_deselection_reaches_the_other_screen`:
    // Leaving has to be visible too. Ada deselects, and her outline comes off
    // Bo's screen — through a published frame, because there is no other way to
    // retract one.
    {
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
}

/// The two things a participant shows up as are drawn by two different calls,
/// and the test above is satisfied by either one of them: with a selection on
/// screen, a peer's colour is plentiful whether or not their playhead is drawn.
/// So each render is proved on its own, by a room in which the peer has only
/// that one thing.
///
/// Both rooms are built from the very functions the editor publishes itself
/// with, so a key that stopped matching would empty them.
fn colour_on_screen(bo: &mut Participant, room: &PresenceCtx, who: &str) -> (usize, usize) {
    let with = bo.frame(room);
    let without = bo.frame(&PresenceCtx::empty());
    let mut r = OffscreenRenderer::new(W, H);
    let a = r.render(&with, Color::BLACK);
    let b = r.render(&without, Color::BLACK);
    let colour = drawn_colour(room, who);
    (pixels_of_colour(&a, colour), pixels_of_colour(&b, colour))
}
