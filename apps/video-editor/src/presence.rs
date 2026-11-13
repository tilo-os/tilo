//! Who else is in this edit: the vocabulary for naming a place in the editor —
//! a clip, the timeline, one inspector field — on the presence channel, and the
//! two halves that must agree about it.
//!
//! Presence is view state: it never reaches the actions or the record, so no
//! undo stack holds "someone moved their playhead". A participant has one point
//! (their text caret while typing in a field, else their playhead) and one range
//! (their selected clip), each published on the key of the place it is at, with
//! times in milliseconds.

use std::cell::{Cell, RefCell};

use uuid::Uuid;
use vello::Scene;
use vello::kurbo::{Affine, Rect};

use ui::PresencePayload;
use ui::paint::PaintCtx;
use ui::permissions::UserId;
use ui::presence::{PathSegment, PresenceCtx, PresenceKey};
use ui::text_render::TextRenderer;
use ui::theme::Theme;

use crate::model::ClipId;

pub const TIMELINE: &str = "ve_timeline";
pub const CLIP: &str = "ve_clip";
pub const FIELD: &str = "ve_field";

/// A title clip's words: the inspector's inline editor and the only field here
/// that reports a caret offset. Its subject is the title's media id.
pub const FIELD_TITLE_WORDS: &str = "title_words";

fn widget_key(parts: &[&str]) -> PresenceKey {
    PresenceKey::Widget {
        path: parts
            .iter()
            .map(|p| PathSegment::Field((*p).to_string()))
            .collect(),
    }
}

pub fn timeline_key() -> PresenceKey {
    widget_key(&[TIMELINE])
}

pub fn clip_key(id: ClipId) -> PresenceKey {
    widget_key(&[CLIP, &id.to_string()])
}

pub fn field_key(field: &str, subject: Uuid) -> PresenceKey {
    widget_key(&[FIELD, field, &subject.to_string()])
}

/// The tag and the uuid after it. `None` for the timeline key, another app's
/// keys, and a malformed uuid.
fn tagged_uuid(key: &PresenceKey, tag: &str) -> Option<Uuid> {
    let PresenceKey::Widget { path } = key else {
        return None;
    };
    let [PathSegment::Field(head), PathSegment::Field(id)] = path.as_slice() else {
        return None;
    };
    (head == tag).then(|| Uuid::parse_str(id).ok()).flatten()
}

pub fn clip_of(key: &PresenceKey) -> Option<ClipId> {
    tagged_uuid(key, CLIP)
}

/// Presence payload fields are `usize`, so a time is an integer: a millisecond is
/// finer than a frame at any rate here and survives a round trip exactly.
pub const TICKS_PER_SECOND: f64 = 1000.0;

/// Negative times clamp to zero here: `usize` cannot carry one, and this is the
/// one place that decides it.
pub fn ticks(seconds: f64) -> usize {
    if !seconds.is_finite() || seconds <= 0.0 {
        return 0;
    }
    (seconds * TICKS_PER_SECOND).round() as usize
}

pub fn seconds(ticks: usize) -> f64 {
    ticks as f64 / TICKS_PER_SECOND
}

const FACETS: usize = 2;

/// Publishes the local participant's place one frame at a time.
///
/// The host broadcasts a single key and payload per poll, only when it differs
/// from the last, while the editor has two things to say at once. So this keeps
/// what it last published per facet and hands over whichever has moved, one per
/// poll, scanning on from the last so a dragged playhead cannot starve the
/// selection. With nothing moved it returns the last frame unchanged, and the
/// host sends nothing.
#[derive(Debug, Default)]
pub struct LocalPresence {
    published: RefCell<[Option<(PresenceKey, PresencePayload)>; FACETS]>,
    last: Cell<usize>,
}

impl LocalPresence {
    pub fn next(
        &self,
        facets: [(PresenceKey, PresencePayload); FACETS],
    ) -> Option<(PresenceKey, PresencePayload)> {
        let mut published = self.published.borrow_mut();
        let mut facets = facets.map(Some);
        for step in 0..FACETS {
            // `% FACETS` keeps `i` inside both arrays.
            let i = (self.last.get() + 1 + step) % FACETS;
            let (Some(slot), Some(facet)) = (published.get_mut(i), facets.get_mut(i)) else {
                continue;
            };
            if slot.as_ref() != facet.as_ref() {
                *slot = facet.take();
                self.last.set(i);
                return slot.clone();
            }
        }
        published.get(self.last.get()).cloned().flatten()
    }
}

/// The point: a text caret in a field when one is being edited, else the
/// playhead.
pub fn point_frame(
    editing_field: Option<(&str, Uuid, usize)>,
    playhead: f64,
) -> (PresenceKey, PresencePayload) {
    match editing_field {
        Some((field, subject, offset)) => (
            field_key(field, subject),
            PresencePayload::Caret { pos: offset },
        ),
        None => (
            timeline_key(),
            PresencePayload::Caret {
                pos: ticks(playhead),
            },
        ),
    }
}

pub fn range_frame(selected: Option<(ClipId, f64, f64)>) -> (PresenceKey, PresencePayload) {
    match selected {
        Some((id, start, end)) => (
            clip_key(id),
            PresencePayload::Selection {
                start: ticks(start),
                end: ticks(end),
            },
        ),
        None => (
            timeline_key(),
            PresencePayload::Selection { start: 0, end: 0 },
        ),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PeerPlayhead {
    pub user: UserId,
    pub at: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PeerClip {
    pub user: UserId,
    pub clip: ClipId,
    /// The span they published, for when the clip is not present locally; prefer the
    /// local geometry when it is.
    pub span: (f64, f64),
}

/// Sorted by user id, so a render is stable frame to frame.
pub fn peer_playheads(presence: &PresenceCtx) -> Vec<PeerPlayhead> {
    let key = timeline_key();
    let mut out: Vec<PeerPlayhead> = presence
        .carets
        .get(&key)
        .map(|list| {
            list.iter()
                .map(|(user, pos)| PeerPlayhead {
                    user: user.clone(),
                    at: seconds(*pos),
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort_by(|a, b| a.user.0.cmp(&b.user.0));
    out
}

/// The empty range published for "nothing selected" lands on the timeline key,
/// which names no clip, so it is skipped.
pub fn peer_clips(presence: &PresenceCtx) -> Vec<PeerClip> {
    let mut out = Vec::new();
    for (key, list) in &presence.selections {
        let Some(clip) = clip_of(key) else { continue };
        for (user, range) in list {
            if range.start >= range.end {
                continue;
            }
            out.push(PeerClip {
                user: user.clone(),
                clip,
                span: (seconds(range.start), seconds(range.end)),
            });
        }
    }
    out.sort_by(|a, b| a.user.0.cmp(&b.user.0));
    out
}

pub fn peer_field_carets(
    presence: &PresenceCtx,
    field: &str,
    subject: Uuid,
) -> Vec<(UserId, usize)> {
    let key = field_key(field, subject);
    let mut out: Vec<(UserId, usize)> = presence.carets.get(&key).cloned().unwrap_or_default();
    out.sort_by(|a, b| a.0.0.cmp(&b.0.0));
    out
}

/// Height of a name chip, near enough, so playheads landing on the same
/// instant can be stepped apart instead of printing on top of one another.
const CHIP_STEP: f64 = 16.0;

/// What every peer overlay draws with, grouped so a new overlay does not grow
/// another long positional API.
pub struct PeerPaintCtx<'a> {
    paint: PaintCtx<'a>,
    presence: &'a PresenceCtx,
}

impl<'a> PeerPaintCtx<'a> {
    pub fn new(
        scene: &'a mut Scene,
        transform: Affine,
        text_renderer: &'a mut TextRenderer,
        theme: &'a Theme,
        presence: &'a PresenceCtx,
    ) -> Self {
        Self {
            paint: PaintCtx::new(scene, transform, text_renderer, theme),
            presence,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PeerTag {
    pub peer: usize,
    pub x: f64,
    pub row: u32,
    pub top: f64,
    /// Kept clear of the ruler's readings when the chip hangs in the ruler.
    pub span: (f64, f64),
}

/// A name hangs in the ruler, above the lanes, where it covers no clip. A name
/// that would land on the local playhead's time (`taken`) or on one already
/// placed steps down into the lanes until it finds room.
pub fn place_peer_tags(
    presence: &PresenceCtx,
    lanes: Rect,
    taken: &[(f64, f64)],
    time_to_x: impl Fn(f64) -> f64,
    mut name_width: impl FnMut(&str) -> f64,
) -> Vec<PeerTag> {
    let mut placed: Vec<PeerTag> = Vec::new();
    for (i, peer) in peer_playheads(presence).iter().enumerate() {
        let x = time_to_x(peer.at);
        if x < lanes.x0 - 0.5 || x > lanes.x1 + 0.5 {
            continue;
        }
        let w = name_width(&presence.display_name(&peer.user));
        let span = (x, x + w);
        let clashes = |a: (f64, f64), b: (f64, f64)| a.0 < b.1 && b.0 < a.1;
        let rows = u32::try_from(placed.len()).unwrap_or(u32::MAX);
        let row = (0..=rows)
            .find(|row| {
                let in_ruler_clash = *row == 0 && taken.iter().any(|t| clashes(*t, span));
                !in_ruler_clash
                    && !placed
                        .iter()
                        .any(|p| p.row == *row && clashes(p.span, span))
            })
            .unwrap_or(0);
        placed.push(PeerTag {
            peer: i,
            x,
            row,
            top: lanes.y0 + CHIP_STEP * f64::from(row),
            span,
        });
    }
    placed
}

pub fn draw_peer_playheads(ctx: &mut PeerPaintCtx<'_>, tags: &[PeerTag], bottom: f64) {
    let peers = peer_playheads(ctx.presence);
    for tag in tags {
        let Some(peer) = peers.get(tag.peer) else {
            continue;
        };
        let bar = Rect::new(tag.x - 1.0, tag.top, tag.x + 1.0, bottom);
        ctx.presence.draw_peer_caret_labeled(
            ctx.paint.scene,
            ctx.paint.theme,
            ctx.paint.text,
            ctx.paint.transform,
            &peer.user,
            &ctx.presence.display_name(&peer.user),
            bar,
        );
    }
}

/// Each in the peer's colour, with their name.
///
/// `visible` is what the timeline just drew, so an outline sits on the same
/// geometry as the clip body. `area` is where the lanes are clipped to: a chip
/// hangs above its clip, and without the ceiling a peer on the top track would be
/// labelled with the bottom two pixels of their name.
pub fn draw_peer_clips(ctx: &mut PeerPaintCtx<'_>, visible: &[(ClipId, Rect)], area: Rect) {
    for peer in peer_clips(ctx.presence) {
        let Some((_, rect)) = visible.iter().find(|(id, _)| *id == peer.clip) else {
            continue;
        };
        ctx.presence.draw_peer_outline_labeled_within(
            &mut ctx.paint,
            &peer.user,
            &ctx.presence.display_name(&peer.user),
            *rect,
            4.0,
            area,
        );
    }
}

/// A peer's caret is the local caret's twin, drawn from the field's own
/// byte-offset mapping, so it reaches the screen only while the field is open
/// here.
pub fn draw_peer_field_carets(
    ctx: &mut PeerPaintCtx<'_>,
    field: &str,
    subject: Uuid,
    mut caret_rect: impl FnMut(usize) -> Rect,
) {
    for (user, byte) in peer_field_carets(ctx.presence, field, subject) {
        ctx.presence.draw_peer_caret_labeled(
            ctx.paint.scene,
            ctx.paint.theme,
            ctx.paint.text,
            ctx.paint.transform,
            &user,
            &ctx.presence.display_name(&user),
            caret_rect(byte),
        );
    }
}

/// What another process sends to appear as a participant. Returned as text
/// because the thing that needs it is not this program; a doc test asserts every
/// key it names is the one the builders above produce.
pub fn external_driver_recipe() -> &'static str {
    concat!(
        "Connect to the collaboration broker socket and send:\n",
        "\n",
        "1. CollabRequest::Hello { app_id: \"so.tilo.video-editor\",\n",
        "                          cluster_id: <the workspace uuid>,\n",
        "                          record: <the open project, or None>,\n",
        "                          handle: Some(\"<the name to show>\") }\n",
        "   The handle is what the name chips read; without one the peer is\n",
        "   labelled with its raw id.\n",
        "\n",
        "2. CollabRequest::Presence { key: Workspace { id: <the workspace uuid> },\n",
        "                             payload: Arrived }\n",
        "   Announces arrival, and asks whoever holds the project to send it.\n",
        "\n",
        "Then, to be seen in the edit — all keys are PresenceKey::Widget:\n",
        "\n",
        "  playhead   path [\"ve_timeline\"]            Caret { pos: ms }\n",
        "  selection  path [\"ve_clip\", <clip uuid>]   Selection { start: ms,\n",
        "                                                        end: ms }\n",
        "  no selection\n",
        "             path [\"ve_timeline\"]            Selection { start: 0,\n",
        "                                                        end: 0 }\n",
        "  typing     path [\"ve_field\", \"title_words\", <media uuid>]\n",
        "                                             Caret { pos: byte }\n",
        "  pointer    the workspace scope               Pointer { x, y }\n",
        "\n",
        "Times are milliseconds. Uuids are hyphenated lower case. A caret and\n",
        "a selection are one each per participant, so publishing a new one\n",
        "moves it rather than adding a second.\n",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::presence::SelectionRange;

    fn uid(s: &str) -> UserId {
        UserId(s.to_string())
    }

    #[test]
    fn presence_keys_and_times_round_trip_exactly() {
        {
            let clip = Uuid::parse_str("6f1e2d3c-4b5a-4968-8778-695a4b3c2d1e").unwrap();

            let by_hand = PresenceKey::Widget {
                path: vec![
                    PathSegment::Field("ve_clip".to_string()),
                    PathSegment::Field("6f1e2d3c-4b5a-4968-8778-695a4b3c2d1e".to_string()),
                ],
            };
            assert_eq!(by_hand, clip_key(clip));

            let timeline = PresenceKey::Widget {
                path: vec![PathSegment::Field("ve_timeline".to_string())],
            };
            assert_eq!(timeline, timeline_key());

            let field = PresenceKey::Widget {
                path: vec![
                    PathSegment::Field("ve_field".to_string()),
                    PathSegment::Field("title_words".to_string()),
                    PathSegment::Field("6f1e2d3c-4b5a-4968-8778-695a4b3c2d1e".to_string()),
                ],
            };
            assert_eq!(field, field_key(FIELD_TITLE_WORDS, clip));
        }
        {
            let recipe = external_driver_recipe();
            for tag in [TIMELINE, CLIP, FIELD, FIELD_TITLE_WORDS] {
                assert!(recipe.contains(tag), "the recipe never mentions {tag:?}");
            }
            assert!(
                recipe.contains("so.tilo.video-editor"),
                "the recipe has to name the app id a driver announces as",
            );
            assert!(
                recipe.contains("milliseconds"),
                "a driver that guesses the time unit publishes a playhead in the \
             wrong place, so the recipe must state it",
            );
        }
        {
            let clip = Uuid::new_v4();
            assert_eq!(clip_of(&clip_key(clip)), Some(clip));
            // A key for another kind of object never reads back as a clip.
            assert_eq!(clip_of(&field_key(FIELD_TITLE_WORDS, clip)), None);
            // Nor does the timeline, which names no object at all.
            assert_eq!(clip_of(&timeline_key()), None);
            let foreign = PresenceKey::Widget {
                path: vec![
                    PathSegment::Field("wp_block".to_string()),
                    PathSegment::Field(clip.to_string()),
                ],
            };
            assert_eq!(clip_of(&foreign), None);
            // Nor a well-tagged key carrying something that is not a uuid.
            let malformed = PresenceKey::Widget {
                path: vec![
                    PathSegment::Field(CLIP.to_string()),
                    PathSegment::Field("not-a-uuid".to_string()),
                ],
            };
            assert_eq!(clip_of(&malformed), None);
        }
        {
            for t in [0.0, 0.001, 1.0, 12.5, 23.456, 3600.0] {
                assert!(
                    (seconds(ticks(t)) - t).abs() < 1e-9,
                    "{t}s came back as {}s",
                    seconds(ticks(t)),
                );
            }
            // A negative time clamps here rather than wrapping to an enormous one.
            assert_eq!(ticks(-5.0), 0);
            assert_eq!(ticks(f64::NAN), 0);
            assert_eq!(ticks(f64::NEG_INFINITY), 0);
        }
    }

    #[test]
    fn presence_publisher_emits_every_facet_fairly() {
        {
            let (key, payload) = point_frame(None, 12.5);
            assert_eq!(key, timeline_key());
            assert_eq!(payload, PresencePayload::Caret { pos: 12_500 });

            let clip = Uuid::new_v4();
            let (key, payload) = point_frame(Some((FIELD_TITLE_WORDS, clip, 4)), 12.5);
            assert_eq!(key, field_key(FIELD_TITLE_WORDS, clip));
            assert_eq!(
                payload,
                PresencePayload::Caret { pos: 4 },
                "while typing, the point is the text caret, not the playhead",
            );
        }
        {
            let clip = Uuid::new_v4();
            let (key, _) = range_frame(Some((clip, 2.0, 6.0)));
            assert_eq!(key, clip_key(clip));

            let (key, payload) = range_frame(None);
            assert_eq!(key, timeline_key(), "the empty range is not on any clip");
            assert_eq!(payload, PresencePayload::Selection { start: 0, end: 0 });

            // Both frames to one receiver, in order: the clip ends up with no peer.
            let mut ctx = PresenceCtx::empty();
            let (k, p) = range_frame(Some((clip, 2.0, 6.0)));
            ctx.apply_remote_presence("ada".into(), k, p);
            assert_eq!(peer_clips(&ctx).len(), 1, "selected");
            let (k, p) = range_frame(None);
            ctx.apply_remote_presence("ada".into(), k, p);
            assert!(peer_clips(&ctx).is_empty(), "and deselected again");
        }
        {
            let clip = Uuid::new_v4();
            let local = LocalPresence::default();
            let facets = || [point_frame(None, 12.5), range_frame(Some((clip, 2.0, 6.0)))];

            let first = local.next(facets()).expect("a first frame");
            let second = local.next(facets()).expect("a second frame");
            assert_ne!(first, second, "the second poll offers the other facet");

            let published = [first, second.clone()];
            assert!(
                published.iter().any(|(k, _)| *k == timeline_key()),
                "the playhead was never published: {published:?}",
            );
            assert!(
                published.iter().any(|(k, _)| *k == clip_key(clip)),
                "the selection was never published: {published:?}",
            );

            let third = local.next(facets());
            assert_eq!(third, Some(second.clone()));
            assert_eq!(local.next(facets()), Some(second));
        }
        {
            let clip = Uuid::new_v4();
            let local = LocalPresence::default();
            let mut saw_selection = false;
            for frame in 0..8 {
                // A playhead moving every frame, and a selection that never does.
                let t = 1.0 + f64::from(frame) * 0.25;
                let out = local.next([point_frame(None, t), range_frame(Some((clip, 2.0, 6.0)))]);
                if out.map(|(k, _)| k) == Some(clip_key(clip)) {
                    saw_selection = true;
                }
            }
            assert!(
                saw_selection,
                "the selection never got a turn while the playhead was moving",
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn peer_presence_is_read_back_per_peer_and_painted_in_place() {
        {
            let clip = Uuid::new_v4();
            let mut ctx = PresenceCtx::empty();

            let (k, p) = point_frame(None, 12.5);
            ctx.apply_remote_presence("ada".into(), k, p);
            let (k, p) = range_frame(Some((clip, 2.0, 6.0)));
            ctx.apply_remote_presence("ada".into(), k, p);

            let heads = peer_playheads(&ctx);
            assert_eq!(heads.len(), 1);
            assert_eq!(heads[0].user, uid("ada"));
            assert!((heads[0].at - 12.5).abs() < 1e-9, "at {}", heads[0].at);

            let clips = peer_clips(&ctx);
            assert_eq!(clips.len(), 1);
            assert_eq!(clips[0].clip, clip);
            assert_eq!(clips[0].span, (2.0, 6.0));
        }
        {
            let mut ctx = PresenceCtx::empty();
            for t in [1.0, 2.0, 3.0] {
                let (k, p) = point_frame(None, t);
                ctx.apply_remote_presence("ada".into(), k, p);
            }
            let heads = peer_playheads(&ctx);
            assert_eq!(heads.len(), 1, "one participant, one playhead: {heads:?}");
            assert!((heads[0].at - 3.0).abs() < 1e-9);
        }
        {
            let clip = Uuid::new_v4();
            let mut ctx = PresenceCtx::empty();
            let (k, p) = point_frame(None, 12.5);
            ctx.apply_remote_presence("ada".into(), k, p);
            assert_eq!(peer_playheads(&ctx).len(), 1);

            let (k, p) = point_frame(Some((FIELD_TITLE_WORDS, clip, 3)), 12.5);
            ctx.apply_remote_presence("ada".into(), k, p);
            assert!(
                peer_playheads(&ctx).is_empty(),
                "their ghost playhead steps aside while they type",
            );
            assert_eq!(
                peer_field_carets(&ctx, FIELD_TITLE_WORDS, clip),
                vec![(uid("ada"), 3)],
            );

            let (k, p) = point_frame(None, 12.5);
            ctx.apply_remote_presence("ada".into(), k, p);
            assert_eq!(peer_playheads(&ctx).len(), 1);
            assert!(peer_field_carets(&ctx, FIELD_TITLE_WORDS, clip).is_empty());
        }
        {
            let one = Uuid::new_v4();
            let two = Uuid::new_v4();
            let mut ctx = PresenceCtx::empty();
            for (who, t, clip) in [("ada", 4.0, one), ("bo", 9.0, two)] {
                let (k, p) = point_frame(None, t);
                ctx.apply_remote_presence(who.into(), k, p);
                let (k, p) = range_frame(Some((clip, 1.0, 2.0)));
                ctx.apply_remote_presence(who.into(), k, p);
            }
            let heads = peer_playheads(&ctx);
            assert_eq!(heads.len(), 2, "{heads:?}");
            assert_eq!(heads[0].user, uid("ada"));
            assert!((heads[0].at - 4.0).abs() < 1e-9);
            assert_eq!(heads[1].user, uid("bo"));
            assert!((heads[1].at - 9.0).abs() < 1e-9);

            let clips = peer_clips(&ctx);
            assert_eq!(clips.len(), 2, "{clips:?}");
            assert_eq!(clips[0].clip, one);
            assert_eq!(clips[1].clip, two);
            assert_ne!(
                ctx.color_for(&uid("ada")),
                ctx.color_for(&uid("bo")),
                "two people must not be drawn in one colour",
            );
        }
        {
            let clip = Uuid::new_v4();
            let mut ctx = PresenceCtx::empty();
            ctx.set_selection(
                &clip_key(clip),
                uid("ada"),
                SelectionRange { start: 5, end: 5 },
            );
            assert!(peer_clips(&ctx).is_empty());
        }
        {
            use ui::offscreen::OffscreenRenderer;
            use vello::peniko::Color;

            let subject = Uuid::new_v4();
            let mut ctx = PresenceCtx::empty();
            // "charlie" is the palette's emerald, so a green-dominant pixel is
            // that peer specifically and not some other ink.
            let (k, p) = point_frame(Some((FIELD_TITLE_WORDS, subject, 7)), 0.0);
            ctx.apply_remote_presence("charlie".into(), k, p);

            let mut scene = Scene::new();
            let mut tr = ui::text_render::TextRenderer::new();
            let theme = Theme::default();
            let mut asked = Vec::new();
            draw_peer_field_carets(
                &mut PeerPaintCtx::new(&mut scene, Affine::IDENTITY, &mut tr, &theme, &ctx),
                FIELD_TITLE_WORDS,
                subject,
                |byte| {
                    asked.push(byte);
                    Rect::new(40.0, 60.0, 42.0, 80.0)
                },
            );
            assert_eq!(
                asked,
                vec![7],
                "the field is asked for the byte the peer sent"
            );

            let mut r = OffscreenRenderer::new(120, 120);
            let rgba = r.render(&scene, Color::BLACK);
            let (cr, cg, cb, _) = OffscreenRenderer::sample(&rgba, 120, 40, 70);
            assert!(
                cg > cr && cg > cb && cg > 100,
                "expected the peer's emerald caret in the field, got ({cr},{cg},{cb})",
            );
        }
        {
            let ctx = PresenceCtx::empty();
            assert!(peer_playheads(&ctx).is_empty());
            assert!(peer_clips(&ctx).is_empty());
            assert!(peer_field_carets(&ctx, FIELD_TITLE_WORDS, Uuid::new_v4()).is_empty());
        }
    }
}
