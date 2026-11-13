#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used,
    reason = "collaboration integration fixture asserts setup failures directly"
)]

//! Two people in one edit: every assertion crosses between two independent editors.
//!
//! One drives through real keyboard events and hands over only what
//! [`Widget::presence_payload`](ui::presence::capability::presence_payload) offers the host;
//! the frames go into the other through [`PresenceCtx::apply_remote_presence`], the decoder
//! every inbound frame passes through, and the second editor then draws.
//!
//! Nothing below names a key: a test that publishes and reads back through the same context
//! proves only that one function agrees with itself, while the failure that happens is a
//! publisher and a renderer keying a frame differently.
//!
//! The two editors share one record, as collaborators do: the second merges the first's
//! document, which is what a late joiner receives.

use ui::offscreen::OffscreenRenderer;
use ui::permissions::UserId;
use ui::prelude::TestApp;
use ui::presence::PresenceCtx;
use ui::theme::Theme;
use vello::Scene;
use vello::peniko::Color;
use video_editor::model::{ClipId, MediaKind, Project};
use video_editor::presence as vp;
use video_editor::{VideoEditor, shot};

const W: u32 = 1280;
const H: u32 = 800;

/// One editor and the record it holds, hosted as the app runs.
struct Participant {
    app: TestApp<VideoEditor>,
}

impl Participant {
    /// The participant who opens the project: the demo project, as the
    /// plain capture stages it.
    fn opening() -> Self {
        ui::host::install_default(ui::host::HostServices::recording().0);
        Self::focused(TestApp::<VideoEditor>::capture(None).expect("the project opens"))
    }

    /// A participant who joins and is handed the project, the way a late
    /// joiner is answered with the document.
    fn joining(from: &Participant) -> Self {
        ui::host::install_default(ui::host::HostServices::recording().0);
        let mut app = TestApp::<VideoEditor>::new().expect("the editor opens");
        app.merge_peer(&mut from.app.peer_document())
            .expect("the handed-over document");
        Self::focused(app)
    }

    /// The editor holds the keyboard, as a person working in it does: key
    /// presses reach it, and the host polls it for where its user is.
    fn focused(mut app: TestApp<VideoEditor>) -> Self {
        app.harness()
            .relayout_themed((W, H), Theme::dark())
            .expect("layout");
        app.harness()
            .click_named("video_editor_root")
            .expect("the editor is on screen");
        Participant { app }
    }

    /// The project this participant's record holds.
    fn project(&self) -> Project {
        self.app.state()
    }

    /// Sync, lay out and draw one frame — everything the host does per frame,
    /// so the editor has a real rect and real geometry behind it.
    fn frame(&mut self, presence: &PresenceCtx) -> Scene {
        let harness = self.app.harness();
        harness.update_presence(|ctx| {
            *ctx = presence.clone();
            true
        });
        harness
            .relayout_themed((W, H), Theme::dark())
            .expect("layout");
        harness.scene().clone()
    }

    /// Press a key, the way the host delivers one.
    fn press(&mut self, key: &str) {
        self.app.harness().key(key);
    }

    /// The host offers one frame per poll and only sends what has changed, so draining is what a
    /// few frames of a live session do. Nothing here inspects or rebuilds a key.
    fn relay(&mut self, to: &mut PresenceCtx, as_user: &str) -> usize {
        let mut sent: usize = 0;
        let mut last = None;
        for _ in 0..8 {
            let Some(frame) = self.app.harness().focus_presence_payload() else {
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

/// Read from the document rather than from the editor, because that is what the two actually
/// share.
fn clip_ids(p: &Participant) -> Vec<ClipId> {
    let mut ids: Vec<ClipId> = p.project().clips.iter().map(|c| c.id).collect();
    ids.sort();
    ids
}

/// How long the shared project runs.
fn duration(p: &Participant) -> f64 {
    p.project().duration()
}

/// The colour a participant is drawn in: their identity colour, on the
/// theme's presence palette (what every peer overlay paints with).
fn drawn_colour(ctx: &PresenceCtx, who: &str) -> (u8, u8, u8) {
    let colour = Theme::dark()
        .tokens
        .colors
        .presence
        .of_rgb(ctx.color_for(&UserId(who.to_string())))
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
    // The driver's playhead is moved with `End` and its selection widened with Ctrl+A, real keys
    // through the real handler, and the other participant is then asked where that person is.
    {
        let mut ada = Participant::opening();
        let bo = Participant::joining(&ada);

        let shared = clip_ids(&bo);
        assert!(!shared.is_empty(), "the joining participant received clips");
        assert_eq!(
            shared,
            clip_ids(&ada),
            "both participants hold the same clips, by the same ids",
        );

        // Ada jumps to the end of the project — a real key press.
        ada.press("End");
        ada.frame(&PresenceCtx::empty());

        let mut ctx = PresenceCtx::empty();
        let sent = ada.relay(&mut ctx, "ada");
        assert!(
            sent >= 2,
            "Ada had both a playhead and a selection to send, sent {sent}"
        );

        let heads = vp::peer_playheads(&ctx);
        assert_eq!(heads.len(), 1, "exactly one other participant: {heads:?}");
        assert_eq!(heads[0].user, UserId("ada".to_string()));
        let end = duration(&ada);
        assert!(
            (heads[0].at - end).abs() < 0.01,
            "Ada jumped to the end ({end}s); Bo has her at {}s",
            heads[0].at,
        );

        let held = vp::peer_clips(&ctx);
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
    // Ada steps her playhead repeatedly and Bo tracks it each time, with no trail of the instants
    // she passed through.
    {
        let mut ada = Participant::opening();
        let mut ctx = PresenceCtx::empty();

        // Arrows nudge a selected clip and step the playhead only when nothing is
        // selected, so Ada lets go of her clip before scrubbing.
        ada.press("Escape");
        ada.press("Home");
        ada.frame(&PresenceCtx::empty());
        ada.relay(&mut ctx, "ada");
        let at_start = vp::peer_playheads(&ctx);
        assert_eq!(at_start.len(), 1);
        assert!(
            at_start[0].at.abs() < 0.01,
            "at the start, got {}",
            at_start[0].at
        );

        let mut seen = vec![at_start[0].at];
        for _ in 0..4 {
            ada.press("ArrowRight");
            ada.frame(&PresenceCtx::empty());
            ada.relay(&mut ctx, "ada");
            let now = vp::peer_playheads(&ctx);
            assert_eq!(now.len(), 1, "one participant keeps one playhead: {now:?}");
            seen.push(now[0].at);
        }
        assert!(
            seen.windows(2).all(|w| w[1] > w[0]),
            "each step should have advanced Bo's picture of Ada: {seen:?}",
        );
    }
    // They must not be merged into one, and not drawn in one colour.
    {
        let mut ada = Participant::opening();
        let mut ctx = PresenceCtx::empty();

        ada.press("Home");
        ada.frame(&PresenceCtx::empty());
        ada.relay(&mut ctx, "ada");

        ada.press("End");
        ada.frame(&PresenceCtx::empty());
        ada.relay(&mut ctx, "cass");

        let heads = vp::peer_playheads(&ctx);
        assert_eq!(heads.len(), 2, "two participants, two playheads: {heads:?}");
        assert!(
            (heads[0].at - heads[1].at).abs() > 0.5,
            "they are in different places: {heads:?}",
        );
        assert_ne!(
            ctx.color_for(&UserId("ada".to_string())),
            ctx.color_for(&UserId("cass".to_string())),
            "two people must not be drawn in one colour",
        );
    }
    // A field caret keyed to a local-only handle would publish perfectly and land nowhere.
    {
        let mut ada = Participant::opening();

        let mut ctx = PresenceCtx::empty();
        ada.relay(&mut ctx, "ada");
        assert_eq!(
            vp::peer_playheads(&ctx).len(),
            1,
            "before typing, Ada's point is her playhead",
        );

        // "T" adds a title and opens its words for editing.
        ada.press("t");
        ada.frame(&PresenceCtx::empty());
        ada.relay(&mut ctx, "ada");

        // The title she just made, as the *document* holds it — which is what a
        // second participant would receive.
        let title = ada
            .project()
            .media
            .iter()
            .find(|m| m.kind == MediaKind::Text)
            .map(|m| m.id)
            .expect("pressing T added a title to the document");

        let carets = vp::peer_field_carets(&ctx, vp::FIELD_TITLE_WORDS, title);
        assert_eq!(
            carets.len(),
            1,
            "Ada's caret should be in the title's words, keyed to the title the \
         document holds; found {carets:?}",
        );
        assert_eq!(carets[0].0, UserId("ada".to_string()));
        assert!(
            vp::peer_playheads(&ctx).is_empty(),
            "and her ghost playhead steps aside while she types -- a person has \
         one point, not two",
        );

        // A joiner receives the very object that key names, so it resolves.
        let bo = Participant::joining(&ada);
        assert!(
            bo.project().media.iter().any(|m| m.id == title),
            "the title Ada's caret is keyed to reached the other participant",
        );
    }
}

#[test]
fn participants_presence_reaches_the_other_screen() {
    // Bo draws the same frame twice, once with Ada in the ctx, so a difference in the pixels is
    // Ada, and her colour in the first and not the second is her rather than a stray relayout.
    {
        let mut ada = Participant::opening();
        let mut bo = Participant::joining(&ada);

        ada.press("Home");
        ada.frame(&PresenceCtx::empty());
        let mut ctx = PresenceCtx::empty();
        ctx.names
            .insert(UserId("ada".to_string()), "Ada".to_string());
        ada.relay(&mut ctx, "ada");
        assert_eq!(vp::peer_playheads(&ctx).len(), 1, "Ada is in the workspace");

        let alone = bo.frame(&PresenceCtx::empty());
        let together = bo.frame(&ctx);

        assert_ne!(
            shot::fingerprint(&alone),
            shot::fingerprint(&together),
            "drawing the workspace with Ada in it produced the identical scene to \
         drawing it without her -- she reached the model and not the screen",
        );

        let mut r = OffscreenRenderer::new(W, H);
        let empty_px = r.render(&alone, Color::BLACK);
        let peopled_px = r.render(&together, Color::BLACK);

        let ada_colour = drawn_colour(&ctx, "ada");
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
    {
        let mut ada = Participant::opening();
        let mut bo = Participant::joining(&ada);

        // Nothing selected, so the only thing Ada has to show is her playhead.
        ada.press("Escape");
        ada.frame(&PresenceCtx::empty());
        let mut ctx = PresenceCtx::empty();
        ctx.names
            .insert(UserId("ada".to_string()), "Ada".to_string());
        ada.relay(&mut ctx, "ada");
        assert_eq!(vp::peer_playheads(&ctx).len(), 1, "she has a playhead");
        assert!(vp::peer_clips(&ctx).is_empty(), "and nothing selected");

        let (with, without) = colour_on_screen(&mut bo, &ctx, "ada");
        assert!(
            with > without + 100,
            "Ada's playhead put only {} more pixels of her colour on screen than \
         her absence did ({without}) -- a line down every lane with her name \
         on it should be unmistakable",
            with.saturating_sub(without),
        );
    }
    // A participant holding a clip has to be on it, whatever their playhead is doing.
    {
        let ada = Participant::opening();
        let mut bo = Participant::joining(&ada);

        // A clip near the start of the timeline, so it is on screen at the
        // default zoom -- an outline on a clip nobody can see proves nothing.
        let project = bo.project();
        let clip = project
            .clips
            .iter()
            .min_by(|a, b| a.start.partial_cmp(&b.start).unwrap())
            .expect("the shared project has clips");

        let mut ctx = PresenceCtx::empty();
        ctx.names
            .insert(UserId("ada".to_string()), "Ada".to_string());
        let (key, payload) = vp::range_frame(Some((clip.id, clip.start, clip.end())));
        ctx.apply_remote_presence("ada".to_string(), key, payload);
        assert_eq!(vp::peer_clips(&ctx).len(), 1, "she is holding a clip");
        assert!(vp::peer_playheads(&ctx).is_empty(), "and nothing else");

        let (with, without) = colour_on_screen(&mut bo, &ctx, "ada");
        assert!(
            with > without + 100,
            "Ada's held clip put only {} more pixels of her colour on screen than \
         her absence did ({without}) -- a ring round the clip and her name on \
         it should be unmistakable",
            with.saturating_sub(without),
        );
    }
    // Through a published frame, because there is no other way to retract an outline.
    {
        let mut ada = Participant::opening();
        let mut ctx = PresenceCtx::empty();
        ada.relay(&mut ctx, "ada");
        assert_eq!(vp::peer_clips(&ctx).len(), 1, "Ada starts with a clip");

        ada.press("Escape");
        ada.frame(&PresenceCtx::empty());
        ada.relay(&mut ctx, "ada");

        assert!(
            vp::peer_clips(&ctx).is_empty(),
            "Ada let go of the clip and Bo is still drawing her on it",
        );
        assert_eq!(
            vp::peer_playheads(&ctx).len(),
            1,
            "she is still in the workspace though -- deselecting is not leaving",
        );
    }
}

/// With a selection on screen a peer's colour is plentiful whether or not their playhead is
/// drawn, so each render is proved on its own, by a workspace in which the peer has only that
/// one thing.
fn colour_on_screen(bo: &mut Participant, ctx: &PresenceCtx, who: &str) -> (usize, usize) {
    let with = bo.frame(ctx);
    let without = bo.frame(&PresenceCtx::empty());
    let mut r = OffscreenRenderer::new(W, H);
    let a = r.render(&with, Color::BLACK);
    let b = r.render(&without, Color::BLACK);
    let colour = drawn_colour(ctx, who);
    (pixels_of_colour(&a, colour), pixels_of_colour(&b, colour))
}
