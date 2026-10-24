//! Who else is in this edit.
//!
//! A video editor is a spatial document, and the things a collaborator can be
//! *at* are not runs of text: they are a clip, a track, an instant on the
//! timeline, a field in the inspector. This module is the vocabulary for
//! naming those places on the presence channel, plus the two halves that have
//! to agree about it — what this editor publishes about itself, and how it
//! resolves what arrives from everyone else.
//!
//! # Why presence and not the document
//!
//! A playhead, a selection and a caret are **view state**. Nobody's undo stack
//! should contain "Ada moved her playhead", and nobody's project file should
//! record which clip Ada had highlighted when she closed the window. The
//! project's standing rule is that view state is never an action; the presence
//! channel exists precisely so that view state can still be *shared*. So none
//! of this goes through [`crate::actions`] and none of it reaches the CRDT —
//! it is broadcast, rendered, and forgotten.
//!
//! # The keys
//!
//! Every key is a [`PresenceKey::Widget`] whose path starts with a constant
//! tag, mirroring how the word processor keys a paragraph as `wp_block`:
//!
//! | Place | Path |
//! |---|---|
//! | the timeline itself | `["ve_timeline"]` |
//! | one clip | `["ve_clip", <clip uuid>]` |
//! | one editable field | `["ve_field", <field name>, <subject uuid>]` |
//!
//! The uuid is written out with `Uuid::to_string` — its plain hyphenated
//! form. Ids are the *document's* ids, identical on every peer, which is what
//! makes a key mean the same object on both screens; a positional index would
//! drift the moment someone else inserted a clip. (The accessibility tree
//! hashes the same ids because its keys are `u64`. Presence keys are strings,
//! so they carry the id itself and stay legible in a log.)
//!
//! # The payloads
//!
//! There are only two slots per participant, and the toolkit enforces that:
//! [`PresenceCtx::set_caret`] and [`PresenceCtx::set_selection`] each clear
//! that peer from every *other* key first, so a person has at most one caret
//! and at most one selection anywhere. That is a good model of a person, and
//! this module maps the editor onto it deliberately:
//!
//! * **Caret** — the participant's *point*. Normally their playhead, keyed to
//!   the timeline. While they are typing in a field it is their text caret,
//!   keyed to that field, and their playhead ghost steps aside for as long as
//!   that lasts. One person, one point.
//! * **Selection** — the participant's *range*: the clip they have selected,
//!   keyed to that clip, carrying the clip's span so a receiver can place it
//!   even before the clip itself has replicated. Selecting nothing publishes
//!   an empty range on the timeline key, which — because the store is
//!   exclusive — is what retracts a previously published outline.
//! * **Pointer** — their mouse. This one is free: [`ui::App`] broadcasts it
//!   under the room scope on every move, and the toolkit's shared cursor
//!   painter draws it. Nothing here has to do anything for it to work.
//!
//! Times cross the wire as **milliseconds** ([`ticks`]), because the payload
//! fields are `usize`. A millisecond is finer than a frame at any sane rate
//! and coarse enough to be exact after a round trip.
//!
//! # Appearing as a collaborator from outside
//!
//! Nothing here is private to this process. A separate program — the sort of
//! thing [`apps/agent`](../../agent/index.html) is for the word processor —
//! becomes a visible participant in a running editor by connecting to the
//! collaboration broker and sending presence frames under these exact keys.
//! It does not need to link this crate: the keys are four constants and a
//! uuid, and [`external_driver_recipe`] spells out the whole exchange.

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

// ==========================================================================
// The vocabulary
// ==========================================================================

/// Path tag for the timeline as a whole — where a playhead lives.
pub const TIMELINE: &str = "ve_timeline";
/// Path tag for one clip, followed by the clip's uuid.
pub const CLIP: &str = "ve_clip";
/// Path tag for one editable field, followed by the field's name and the
/// uuid of whatever it edits.
pub const FIELD: &str = "ve_field";

/// The field name for a title clip's words — the inspector's inline
/// multi-line editor, and the one editable field in this app that reports
/// a caret offset. Its subject is the title's media id.
pub const FIELD_TITLE_WORDS: &str = "title_words";

fn widget_key(parts: &[&str]) -> PresenceKey {
    PresenceKey::Widget {
        path: parts
            .iter()
            .map(|p| PathSegment::Field((*p).to_string()))
            .collect(),
    }
}

/// The timeline itself: the scope a playhead is published under, and the
/// scope an empty selection is published under to say "nothing selected".
pub fn timeline_key() -> PresenceKey {
    widget_key(&[TIMELINE])
}

pub fn clip_key(id: ClipId) -> PresenceKey {
    widget_key(&[CLIP, &id.to_string()])
}

/// One editable field — `field` names the field, `subject` the object it
/// edits (the clip being renamed, say).
pub fn field_key(field: &str, subject: Uuid) -> PresenceKey {
    widget_key(&[FIELD, field, &subject.to_string()])
}

/// Read a key back: the tag and the uuid that follows it, when the key is one
/// of ours and names an object. `None` for the timeline key (which names no
/// object), for another app's keys, and for a malformed uuid.
fn tagged_uuid(key: &PresenceKey, tag: &str) -> Option<Uuid> {
    let PresenceKey::Widget { path } = key else {
        return None;
    };
    let [PathSegment::Field(head), PathSegment::Field(id)] = path.as_slice() else {
        return None;
    };
    (head == tag).then(|| Uuid::parse_str(id).ok()).flatten()
}

/// The clip a key names, if it names one.
pub fn clip_of(key: &PresenceKey) -> Option<ClipId> {
    tagged_uuid(key, CLIP)
}

// ==========================================================================
// Time on the wire
// ==========================================================================

/// Wire units per second. Presence payload fields are `usize`, so a time has
/// to be an integer; a millisecond is finer than a frame at any rate this
/// editor supports and coarse enough to survive a round trip exactly.
pub const TICKS_PER_SECOND: f64 = 1000.0;

/// Seconds → wire units. Negative times clamp to zero: the timeline starts at
/// zero and `usize` cannot carry a negative anyway, so this is the one place
/// that decides it rather than wrapping to a huge positive somewhere later.
pub fn ticks(seconds: f64) -> usize {
    if !seconds.is_finite() || seconds <= 0.0 {
        return 0;
    }
    (seconds * TICKS_PER_SECOND).round() as usize
}

/// Wire units → seconds.
pub fn seconds(ticks: usize) -> f64 {
    ticks as f64 / TICKS_PER_SECOND
}

// ==========================================================================
// What this editor says about itself
// ==========================================================================

/// How many independent things this editor publishes about where its user is.
/// One point (a caret) and one range (a selection) — the two the presence
/// stores hold per participant.
const FACETS: usize = 2;

/// Publishes the local participant's place in the edit, one frame at a time.
///
/// [`PublishesPresence::presence_payload`](ui::presence::capability::PublishesPresence::presence_payload) hands the host a *single*
/// key-and-payload per poll, and the host broadcasts it only when it differs
/// from the last one — which is the right contract for a text caret and an
/// awkward one here, because this editor has two things to say at once: where
/// the playhead is and which clip is selected.
///
/// They can both be true on the receiving side, because the caret store and
/// the selection store are separate and each is keyed. So this keeps a record
/// of what it last published *for each facet* and hands over whichever one has
/// since moved, one per poll. A change is therefore published within a frame
/// or two of happening, and once published it stays true on the peer until it
/// changes again — nothing has to be re-sent to keep it alive.
///
/// The scan starts after the facet published last, so a playhead being dragged
/// — which changes every single frame — cannot starve the selection of its
/// turn.
///
/// When nothing has moved it returns the last frame it published, unchanged,
/// so the host's own diff sees no change and sends nothing.
#[derive(Debug, Default)]
pub struct LocalPresence {
    published: RefCell<[Option<(PresenceKey, PresencePayload)>; FACETS]>,
    last: Cell<usize>,
}

impl LocalPresence {
    /// Offer this poll's frames — `[point, range]` — and take back the one to
    /// publish.
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

/// The **point** frame: where this participant is, as a single spot.
///
/// A text caret in a field when one is being edited, otherwise the playhead.
/// The two share the one caret store on purpose — see the module docs.
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

/// The **range** frame: what this participant has selected.
///
/// `selected` is the anchor clip and its span in seconds. With nothing
/// selected this publishes an empty range on the timeline key — which,
/// because the selection store keeps one selection per participant, is what
/// takes a previously published outline back off the peer's screen.
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

// ==========================================================================
// What everyone else says about themselves
// ==========================================================================

/// A participant's playhead, as read back off the presence channel.
#[derive(Debug, Clone, PartialEq)]
pub struct PeerPlayhead {
    pub user: UserId,
    /// Timeline position in seconds.
    pub at: f64,
}

/// A participant's selected clip.
#[derive(Debug, Clone, PartialEq)]
pub struct PeerClip {
    pub user: UserId,
    pub clip: ClipId,
    /// The span they published for it, in seconds. Their view of the clip;
    /// prefer the local document's geometry when the clip is present locally,
    /// and fall back to this when it is not.
    pub span: (f64, f64),
}

/// Every participant's playhead, sorted by user id so a render is stable
/// frame to frame rather than reordering with a hash map's whim.
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

/// Every participant's selected clip. The empty range published for "nothing
/// selected" lands on the timeline key, which names no clip, so it is skipped
/// here and the peer simply has no entry.
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

/// Every participant's caret in one named field, as a byte offset into that
/// field's text.
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

// ==========================================================================
// Drawing them
// ==========================================================================

/// Height of a name chip, near enough, so playheads landing on the same
/// instant can be stepped apart instead of printing on top of one another.
const CHIP_STEP: f64 = 16.0;

/// Shared drawing resources for collaborative overlays.
///
/// Every peer overlay uses the same scene, transform, text renderer, theme,
/// and presence snapshot. Grouping them makes that invariant explicit and
/// keeps new overlay geometry from growing another long positional API.
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

/// Where one participant's playhead name sits: the playhead's x, and the top
/// of the line the chip hangs above.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PeerTag {
    /// Index into [`peer_playheads`].
    pub peer: usize,
    pub x: f64,
    /// Which chip row the name hangs in: 0 is the ruler, every further row
    /// one chip down into the lanes.
    pub row: u32,
    /// The line starts here; the name chip hangs just above it.
    pub top: f64,
    /// The horizontal span the name takes, kept clear of the ruler's readings
    /// when the chip hangs in the ruler.
    pub span: (f64, f64),
}

/// Place every participant's playhead name.
///
/// A name hangs in the ruler band, above the lanes, where it covers no clip.
/// The ruler only has one row, so a name that would land on the local
/// playhead's time (`taken`) or on a name already placed steps down into the
/// lanes, one chip at a time, until it finds room.
///
/// `name_width` measures a name as its chip will show it.
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
        // One more row than there are names placed always has room.
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

/// Draw every participant's playhead as a ghost line down the lanes with
/// their name above it, where [`place_peer_tags`] put it.
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

/// Outline the clips other participants have selected, each in their colour
/// and carrying their name.
///
/// `visible` is what the timeline just drew — clip id and the rect it was
/// drawn into — so an outline is placed from the same geometry as the clip
/// body under it. A peer holding a clip that is not on screen (scrolled out,
/// or not yet replicated here) simply is not drawn.
///
/// `area` is the region the lanes are clipped to. A name chip hangs above the
/// clip it names, and there is nothing above the top lane to hang it in, so
/// the chip has to know where the ceiling is or a peer on the top track is
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

/// Draw everyone else's caret in one editable field.
///
/// `caret_rect` is the field's own byte-offset-to-rectangle mapping — the
/// very one its focused local caret is drawn from — so a peer's caret is the
/// local caret's twin rather than a second guess at where a character sits.
///
/// This only reaches the screen while the field is open here too, because a
/// closed field has no geometry to place a caret in. That is the case it
/// exists for: two people in the same words at the same time, which is when
/// knowing where the other one is stops being a nicety.
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

// ==========================================================================
// The contract, for something that is not this program
// ==========================================================================

/// What a separate process has to send to appear in a running editor as a
/// participant — cursor, playhead, selection and all.
///
/// Returned as text rather than performed, because the thing that needs it is
/// by definition not this program. It is kept next to the key builders above
/// so the two cannot drift: the doc test at the bottom of this module asserts
/// that every key named here is the key those builders actually produce.
///
/// The frames are ordinary `CollabRequest::Presence` messages on the
/// collaboration broker socket, exactly as [`ui::App`] sends its own.
pub fn external_driver_recipe() -> &'static str {
    concat!(
        "Connect to the collaboration broker socket and send:\n",
        "\n",
        "1. CollabRequest::Hello { app_id: \"so.tilo.video-editor\",\n",
        "                          cluster_id: <the room uuid>,\n",
        "                          record: <the open project, or None>,\n",
        "                          handle: Some(\"<the name to show>\") }\n",
        "   The handle is what the name chips read; without one the peer is\n",
        "   labelled with its raw id.\n",
        "\n",
        "2. CollabRequest::Presence { key: Workspace { id: <the room uuid> },\n",
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
        "  pointer    the room scope                  Pointer { x, y }\n",
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
        // Case `a_hand_built_key_is_the_key_this_crate_publishes_under`:
        // The exact bytes an external driver builds by hand have to be the exact
        // bytes this crate builds through its helpers, or a driver that follows
        // the documented recipe publishes into a scope nothing renders.
        //
        // Written out from primitives deliberately — calling `clip_key` on both
        // sides would assert nothing at all.
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
        // Case `the_published_recipe_names_the_keys_this_crate_actually_builds`:
        // The recipe is the only thing an outside program reads before writing a
        // driver, so every key it names has to be a key this crate really uses.
        // Prose drifts silently; this makes it fail loudly instead.
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
        // Case `a_time_survives_the_round_trip_to_the_millisecond`:
        // A time has to come back off the wire as the time that went on to it,
        // or a peer's playhead is drawn somewhere its owner never put it.
        {
            for t in [0.0, 0.001, 1.0, 12.5, 23.456, 3600.0] {
                assert!(
                    (seconds(ticks(t)) - t).abs() < 1e-9,
                    "{t}s came back as {}s",
                    seconds(ticks(t)),
                );
            }
            // A negative time clamps here rather than wrapping to an enormous
            // positive somewhere downstream.
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
        // Case `deselecting_publishes_a_frame_that_retracts_the_outline`:
        // Deselecting has to *say* something. The selection store keeps one
        // selection per participant keyed to where it is, so the only way to take
        // an outline off a peer's screen is to publish a different, empty one.
        {
            let clip = Uuid::new_v4();
            let (key, _) = range_frame(Some((clip, 2.0, 6.0)));
            assert_eq!(key, clip_key(clip));

            let (key, payload) = range_frame(None);
            assert_eq!(key, timeline_key(), "the empty range is not on any clip");
            assert_eq!(payload, PresencePayload::Selection { start: 0, end: 0 });

            // And prove it actually clears: feed both frames to a receiver in
            // order and the clip must end up with no peer on it.
            let mut ctx = PresenceCtx::empty();
            let (k, p) = range_frame(Some((clip, 2.0, 6.0)));
            ctx.apply_remote_presence("ada".into(), k, p);
            assert_eq!(peer_clips(&ctx).len(), 1, "selected");
            let (k, p) = range_frame(None);
            ctx.apply_remote_presence("ada".into(), k, p);
            assert!(peer_clips(&ctx).is_empty(), "and deselected again");
        }
        // Case `both_facets_get_published_across_successive_polls`:
        // Both facets have to reach the peer. The host only broadcasts a frame
        // that differs from the previous one, so a publisher that returned the
        // playhead every poll would broadcast the selection exactly never.
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

            // With nothing moving, it keeps returning the same frame, so the
            // host's diff sees no change and the socket stays quiet.
            let third = local.next(facets());
            assert_eq!(third, Some(second.clone()));
            assert_eq!(local.next(facets()), Some(second));
        }
        // Case `a_dragged_playhead_does_not_starve_the_selection`:
        // A playhead being dragged changes on every single frame. If the
        // publisher always offered the facet that moved, the selection would
        // never get a turn for as long as the drag lasted.
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
        // Case `a_published_frame_reads_back_as_the_peer_that_sent_it`:
        // The peer readers resolve what the publishers produced — the two halves
        // under test together, which is the only way to catch them keying
        // differently.
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
        // Case `a_peers_playhead_moves_rather_than_accumulating`:
        // A participant has one playhead. Publishing a new one has to move it,
        // not leave a trail of every instant they ever visited.
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
        // Case `typing_in_a_field_moves_the_peers_point_off_the_ruler`:
        // Typing in a field takes the participant's caret with it — that is what
        // the exclusive caret store means, and it is the honest reading: their
        // point is in the field, not on the ruler.
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
        // Case `several_peers_each_hold_their_own_place`:
        // Several participants at once, which is the case the whole feature is
        // for. Each keeps their own place and their own colour.
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
        // Case `an_empty_range_on_a_clip_is_not_read_as_a_selection`:
        // An empty range on a clip key is not a selection. Rendering it would
        // draw a zero-width outline on a clip nobody has selected.
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
        // Case `a_peers_caret_in_a_field_is_painted_where_the_field_puts_it`:
        // A caret in a field has to reach the screen, in the peer's colour, at
        // the offset the field itself puts that byte at.
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
                    // Stands in for the field's own mapping: a 2px bar at x=40.
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
        // Case `an_empty_room_resolves_to_nothing`:
        // Nobody in the room means nothing to draw — the single-user path must
        // not paint stray chrome.
        {
            let ctx = PresenceCtx::empty();
            assert!(peer_playheads(&ctx).is_empty());
            assert!(peer_clips(&ctx).is_empty());
            assert!(peer_field_carets(&ctx, FIELD_TITLE_WORDS, Uuid::new_v4()).is_empty());
        }
    }
}
