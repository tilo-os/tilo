use crate::permissions::MemberId;
use crate::time::{Duration, Instant, SystemTime};
#[cfg(feature = "model")]
use collab_types::Member;
use std::collections::HashMap;
use tilo_sort::CompactSort as _;

pub mod capability;
mod caret_tag;
mod chat;
#[cfg(feature = "model")]
pub mod draft;
mod faces;
pub mod frame;
mod geom;
mod interaction;
mod written;
#[cfg(feature = "model")]
pub use tilo_doc::written::written_by;
pub mod key;
mod labels;
mod layer;
mod mark;
#[cfg(feature = "model")]
mod members;
mod overlay;
mod placed;
pub mod render;
pub use caret_tag::{
    CaretTag, LabeledCaret, TAG_WIDGET, hide_names_beside_caret_tags, overlaps, tag_gap, tags_crowd,
};
pub use chat::{
    CURSOR_CHAT_LINGER, CURSOR_CHAT_MAX_CHARS, ChatBubble, CursorChats, POINTER_NAME_LINGER,
    ReactionFloat, single_emoji,
};
#[cfg(feature = "model")]
pub use draft::DraftSaid;
pub(crate) use faces::paint_active_ring;
pub use faces::{Face, FaceStack};
pub use geom::{EdgeMist, MistEdge, edge_mist_for};
pub use interaction::{
    ACTIVE_INTERACTION_EXPIRY, AgentActionMarker, MarkerPlace, TERMINAL_INTERACTION_DWELL,
};
pub use key::{DIGEST_KEY_FIELD, PathSegment, PointerSpace, PresenceKey, SelectionRange};
pub use labels::{
    contains_opaque_id, human_label, is_unnamed_person, person_label, shown_name, unnamed_label,
    unnamed_person_label, unnamed_person_short_label,
};
pub use layer::LayerMarks;
pub use layer::{PresenceLayer, WidgetPresence};
pub use mark::PresenceMark;
pub use placed::PlacedPointer;
pub use render::{
    FOLLOWED_POINTER_REACH, PeerCursor, PillPlacement, PillRequest, contrast_ink,
    draw_peer_cursors, draw_peer_presence, draw_remote_cursors, draw_remote_presence,
    layout_peer_pills, paint_follow_frame, paint_followed_pointer, paint_own_chat_bubble,
    paint_reaction, paint_speaking_ring,
};
pub use written::written_under;

/// The scope this process should publish its presence under — the workspace
/// it collaborates in, or `None` when it has no workspace. Read from
/// [`local_cluster_id`](crate::collaboration::local_cluster_id), the
/// same value the routing address is built from, so the two cannot drift apart.
///
/// `None` when the workspace is the nil uuid: that value addresses nobody
/// (`CrdtRoute::is_addressable` rejects it — an unminted workspace would
/// otherwise make every unconfigured machine share one workspace). Fail closed.
#[cfg(feature = "model")]
pub fn local_workspace_key() -> Option<PresenceKey> {
    let cluster = crate::collaboration::local_cluster_id();
    if cluster.is_nil() {
        return None;
    }
    Some(PresenceKey::Workspace {
        id: cluster.to_string(),
    })
}

/// [`PresenceKey::surface_pointer`] for this process's own surface. The
/// coordinates a winit `CursorMoved` carries are surface-local, so this
/// is the only representation an app can provide.
pub fn local_surface_pointer() -> PresenceKey {
    PresenceKey::surface_pointer(crate::platform::local_surface_id())
}

/// How long a person stays shown as composing after their last word: a draft under way is
/// said again as they type ([`DraftSaid`]), so one who stops drops out shortly after, with no
/// message.
pub const COMPOSING_LINGER: crate::time::Duration = crate::time::Duration::from_secs(6);

/// Longest forward caret jump still read as "this peer just wrote that" (the `Caret` arm of
/// [`PresenceCtx::apply_remote_presence`]). Generous on purpose: a caret key is per *block*, so a
/// stray mark is bounded to one paragraph, while too tight a cap would drop the marker on the edit
/// most worth seeing.
const TYPING_DERIVE_MAX_RUN: usize = 512;

/// How long after a remote text edit an advancing caret may still be read as
/// that peer having written what it crossed — one network hop, no more, or a
/// real edit would license markers for every caret that moves afterward.
const EDIT_PULSE_WINDOW: crate::time::Duration = crate::time::Duration::from_millis(1500);

pub use tilo_action_policy::identity_color;
pub use tilo_presence::{
    PEER_PALETTE, agent_client, agent_identity, identity_rgb, member_color, user_color,
};

/// A presence colour as the surfaces paint it: the `(r, g, b)` a person's
/// colour travels as ([`user_color`], a colour they chose), in the colour a
/// face or a row is drawn with.
#[must_use]
pub fn presence_colour((red, green, blue): (u8, u8, u8)) -> vello::peniko::Color {
    vello::peniko::Color::from_rgb8(red, green, blue)
}

/// [`user_color`] as a colour: what a face of `user_id` wears when the workspace
/// has not given them one ([`Face::colour`]).
#[must_use]
pub fn identity_colour(user_id: &str) -> vello::peniko::Color {
    presence_colour(user_color(user_id))
}

/// [`identity_colour`] for a member known by its id and whether it is software: an agent
/// wears its client's hue ([`PresenceHue::of_agent`], the same for every run of it), as the
/// bar, a chip and history draw it.
///
/// [`PresenceHue::of_agent`]: crate::tokens::PresenceHue::of_agent
#[must_use]
pub fn identity_colour_of(id: &str, is_agent: bool) -> vello::peniko::Color {
    if is_agent {
        presence_colour(identity_rgb(crate::tokens::PresenceHue::of_agent(id)))
    } else {
        identity_colour(id)
    }
}

/// The face of someone here whose person the workspace does not know yet:
/// grey, which the presence palette paints neutral
/// ([`crate::tokens::colors::PresencePalette::of`]), never a ring colour
/// they would then change from.
pub const UNSETTLED_RGB: (u8, u8, u8) = (0x9a, 0x9a, 0x9a);

/// How far apart two colours are, weighted the way an eye weights them (the
/// "redmean" approximation).
pub fn color_distance(a: (u8, u8, u8), b: (u8, u8, u8)) -> f32 {
    let rm = f32::midpoint(f32::from(a.0), f32::from(b.0));
    let dr = f32::from(a.0) - f32::from(b.0);
    let dg = f32::from(a.1) - f32::from(b.1);
    let db = f32::from(a.2) - f32::from(b.2);
    ((2.0 + rm / 256.0) * dr * dr + 4.0 * dg * dg + (2.0 + (255.0 - rm) / 256.0) * db * db).sqrt()
}

#[cfg(test)]
mod color_tests {
    use super::*;

    tilo_test_env::scenario! {
        fn peer_colours_are_distinct_stable_and_agreed;

        phase user_colours_are_pinned_or_hashed_onto_a_distinct_ring;
        {
            // phase: pinned_ids_take_their_slots
            {
                assert_eq!(user_color("me"), PEER_PALETTE[8]);
                assert_eq!(user_color("alice"), PEER_PALETTE[11]);
                assert_eq!(user_color("bob"), PEER_PALETTE[1]);
                assert_eq!(user_color("charlie"), PEER_PALETTE[4]);
            }
            // phase: the_ring_is_the_dark_presence_palette
            {
                let palette = crate::tokens::Colors::dark().presence;
                for (slot, rgb) in PEER_PALETTE.iter().enumerate() {
                    let painted = palette.get(slot);
                    for (a, b) in [
                        (rgb.0, painted.to_rgba8().r),
                        (rgb.1, painted.to_rgba8().g),
                        (rgb.2, painted.to_rgba8().b),
                    ] {
                        assert!(a.abs_diff(b) <= 1, "slot {slot}: {rgb:?} vs {painted:?}");
                    }
                }
            }
            // phase: unknown_ids_fall_through_to_stable_hash
            {
                let a = user_color("dave");
                let b = user_color("dave");
                let c = user_color("eve");
                assert_eq!(a, b, "same id must map to same color");
                assert_ne!(a, c, "different ids should map to different colors");
            }
            // phase: no_two_peer_colours_are_confusable
            // Enumerated over the whole closed set `user_color` can return, so the
            // guarantee holds for every id. Bar is a redmean distance of 60; the old
            // hash-to-hue scheme put `dave`/`sara` 4.8 apart and `host`/`peer3` 4.4
            // apart — different people in the same colour.
            {
                for (i, a) in PEER_PALETTE.iter().enumerate() {
                    for b in &PEER_PALETTE[i + 1..] {
                        let d = color_distance(*a, *b);
                        assert!(
                            d >= 60.0,
                            "#{:02x}{:02x}{:02x} and #{:02x}{:02x}{:02x} are only {d:.1} apart — \
                     two peers wearing those read as one",
                            a.0,
                            a.1,
                            a.2,
                            b.0,
                            b.1,
                            b.2,
                        );
                    }
                }
            }
            // phase: pinned_colours_are_ring_members
            // Pins are also ring members, so a pinned peer and a hashed one can
            // only collide outright, never land near-but-not-quite together.
            {
                for id in ["me", "alice", "bob", "charlie"] {
                    let c = user_color(id);
                    assert!(
                        PEER_PALETTE.contains(&c),
                        "{id} is painted #{:02x}{:02x}{:02x}, which is not on the ring",
                        c.0,
                        c.1,
                        c.2,
                    );
                }
            }
            // phase: hashed_ids_land_on_the_ring
            {
                for id in [
                    "carol", "dave", "erin", "sara", "danah", "eve", "ada", "ade",
                ] {
                    let c = user_color(id);
                    assert!(
                        PEER_PALETTE.contains(&c),
                        "{id} is painted #{:02x}{:02x}{:02x}, which is not on the ring",
                        c.0,
                        c.1,
                        c.2,
                    );
                }
                for (a, b) in [("dave", "sara"), ("dave", "erin"), ("ada", "ade")] {
                    let (ca, cb) = (user_color(a), user_color(b));
                    assert!(
                        ca == cb || color_distance(ca, cb) >= 60.0,
                        "{a} and {b} are {:.1} apart",
                        color_distance(ca, cb),
                    );
                }
            }
            // phase: pinned_ids_differ_from_hash_fallback
            {
                let (r, g, b) = user_color("me");
                assert!(!(r == 0 && g == 0 && b == 0));
            }
        }

        phase an_agent_keeps_one_colour_across_its_runs;
        // An agent keeps one colour however often it comes: the key is the agent it is, never the
        // run it works in.
        {
            for (first, second) in [
                // The same agent admitted again, with a run of its own each time.
                ("agent:claude-code@box#1", "agent:claude-code@box#4"),
                ("agent:helper@box#2", "agent:helper@box#12"),
                // The name a shell shows it under, numbered per connection.
                ("Claude Code #1", "Claude Code #3"),
                // Its identity, as the workspace grants name it.
                ("agent:helper@box", "helper@box"),
            ] {
                assert_eq!(
                    member_color(&MemberId::agent(first, "alice")),
                    member_color(&MemberId::agent(second, "ada")),
                    "{first} and {second} are one agent, so one colour"
                );
            }
        }

        phase software_wears_the_hue_of_what_it_is;
        {
            assert_eq!(
                member_color(&MemberId::agent("claude-code@box", "ada")),
                identity_rgb(crate::tokens::PresenceHue::Orange)
            );
            assert_eq!(member_color(&MemberId::person("ada")), user_color("ada"));
        }
    }
}

/// One "just typed" highlight span: the run a peer inserted and when.
/// [`PresenceCtx::active_typing`] turns its age into an alpha and prunes it
/// once past [`crate::motion::Trace::LIFE`].
#[derive(Debug, Clone, PartialEq)]
struct TypingMark {
    range: SelectionRange,
    born: Instant,
}

/// Which layer paints a peer's pointer — a **screen** position, so only one
/// layer may place it correctly (an app painting a shell coordinate into
/// its own window space is right only at the origin). The compositor owns
/// desktop-scoped pointers, since it alone spans the strip, chrome and gaps
/// between windows continuously; an app owns presence *inside* its own
/// content (caret, selection, typing swipe), since the compositor has no
/// access to the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PointerOwner {
    /// No desktop scope, so no compositor overlay claims it: the SDK/relay
    /// dialect, shot fixtures, and the hosted backend's interpolated cursors.
    #[default]
    Window,
    /// The compositor overlay paints it, from [`PresenceCtx::pointers`]. Still
    /// kept here so "follow that peer" can read it via [`PresenceCtx::peer_pointer`].
    Shell,
    /// In some other surface's content coordinates — the frame arrived (the
    /// broker fans presence to every app in the workspace) but is not this
    /// process's to place. Makes the fan-out safe: exactly one of N apps
    /// answers "mine", the rest land here without consulting each other.
    OtherSurface,
}

#[derive(Debug, Clone)]
pub struct RemoteCursor {
    pub member: MemberId,
    pub color: [f32; 4],
    pub position: [f32; 2], // scene-space coords
    pub label: String,
    /// See [`PointerOwner`]; `Window` unless the pointer arrived under a desktop scope.
    pub owner: PointerOwner,
}

impl RemoteCursor {
    /// The single predicate every app-side painter consults, so "who draws this" has one answer.
    pub fn painted_by_window(&self) -> bool {
        matches!(self.owner, PointerOwner::Window)
    }
}

/// What a subscriber still has to do after
/// [`PresenceCtx::apply_remote_presence`] folds a frame in, beyond the
/// presence stores. A subscriber with no document (the compositor) ignores it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemotePresenceEffect {
    None,
    /// The peer broadcast a new view subset; a follower should re-bind its viewport to them.
    PeerViewChanged,
    /// A peer arrived on a non-tab scope — possibly a late joiner needing full state.
    PeerArrived,
    /// A valid, in-scope semantic interaction advanced to a newer phase.
    InteractionAdvanced,
}

/// How long a peer's pointer stays painted after it was last heard about.
/// Comfortably past the ~1s restate interval a live peer keeps, so this
/// notices a peer gone quiet rather than blinking one whose keepalive was
/// briefly delayed. See [`PresenceCtx::prune_pointers`].
pub const POINTER_IDLE: Duration = Duration::from_secs(5);

/// How long a quiet peer's pointer takes to fade out, ending
/// at [`POINTER_IDLE`]: the standard fade of a stream gone quiet
/// ([`crate::motion::Follow::FADE`]). See [`PresenceCtx::pointer_opacity`].
pub const POINTER_FADE: Duration = crate::motion::Follow::FADE;

/// Longest a peer's pointer takes to glide to a newly heard position. Peers publish at ~30 Hz, so a
/// moving pointer glides over the gap between its last two samples and arrives as the next is due;
/// this caps the glide after a pause ([`crate::motion::Follow::GLIDE_MAX`]).
pub const POINTER_EASE_MAX: Duration = crate::motion::Follow::GLIDE_MAX;

/// A pointer's last-known `(x, y)` per user, at each [`PresenceKey`].
pub type PointerPositions = HashMap<PresenceKey, Vec<(MemberId, (f32, f32))>>;

/// How a peer's pointer glides from where it was shown to where it was last
/// heard. `offset` is relative to the stored position, so a pointer carried
/// along with what it is anchored to (a scrolling tile) glides with it.
#[derive(Debug, Clone, Copy)]
struct PointerGlide {
    /// Where the glide started, minus the stored position.
    offset: (f32, f32),
    /// When the glide started: the instant the new position was heard.
    since: Instant,
    /// How long it lasts — the gap between the last two samples, capped at
    /// [`POINTER_EASE_MAX`]. Never zero.
    span: Duration,
    /// When the pointer was last heard at a *different* position, which
    /// times the next glide.
    moved: Instant,
}

impl PointerGlide {
    /// Where the pointer stored at `target` shows at `now`.
    fn at(&self, target: (f32, f32), now: Instant) -> (f32, f32) {
        let rest = 1.0 - crate::motion::Follow::glide(self.since, self.span, now);
        (
            target.0 + self.offset.0 * rest,
            target.1 + self.offset.1 * rest,
        )
    }

    fn easing(&self, now: Instant) -> bool {
        self.offset != (0.0, 0.0) && now.saturating_duration_since(self.since) < self.span
    }
}

#[derive(Debug, Default, Clone)]
pub struct PresenceCtx {
    pub here: HashMap<PresenceKey, Vec<MemberId>>,
    pub pointers: PointerPositions,
    /// When each pointer in [`Self::pointers`] was last heard about, so
    /// [`Self::prune_pointers`] can retire one gone quiet — without it a
    /// crashed peer's cursor stays painted for the rest of the session.
    pointer_seen: HashMap<(PresenceKey, MemberId), Instant>,
    /// How each pointer in [`Self::pointers`] glides between samples; see
    /// [`Self::pointer_position_at`]. Absent for a pointer seen at only one
    /// position, which is shown where it is.
    pointer_glides: HashMap<(PresenceKey, MemberId), PointerGlide>,
    /// When each pointer was last heard somewhere new (its first sight
    /// included): how long it has stood still, which decides whether its
    /// name is still shown ([`Self::pointer_name_opacity`]).
    pointer_moved: HashMap<(PresenceKey, MemberId), Instant>,
    /// What people are saying beside their pointers, and their reactions.
    pub chats: CursorChats,
    /// How available each person says they are in each workspace, and the
    /// voice channel they are in there, keyed by the workspace and the
    /// person: one person says it to each workspace on their own terms.
    availability: HashMap<(String, MemberId), (crate::Availability, Option<String>)>,
    /// Who is composing a message in which resource, and since when: the resource's key, the
    /// person, and the instant they last said so. A composer that stops, or a window that
    /// closes, says `composing: false`; one gone quiet without a word drops out after
    /// [`COMPOSING_LINGER`].
    composing: HashMap<(PresenceKey, MemberId), Instant>,
    /// The members of the workspace's last member list ([`Self::apply_members`]): a list that
    /// replaces it retires whoever is no longer on it.
    members: std::collections::HashSet<MemberId>,
    /// Faces that bounce because their person waved at this one.
    bounces: HashMap<MemberId, crate::motion::Attention>,
    pub selections: HashMap<PresenceKey, Vec<(MemberId, SelectionRange)>>,
    pub carets: HashMap<PresenceKey, Vec<(MemberId, usize)>>,
    pub remote_cursors: Vec<RemoteCursor>,
    /// Seeded by the shell from `Member::cursor_color`; falls
    /// through to [`user_color`] via [`PresenceCtx::color_for`] when unset.
    pub color_overrides: HashMap<MemberId, (u8, u8, u8)>,
    /// Friendly display names per user, seeded from the workspace's
    /// `workspace/members` resource ([`Self::apply_members`]). A peer with no
    /// entry falls back to its raw id via [`PresenceCtx::display_name`], so a
    /// caret chip is never blank.
    pub names: HashMap<MemberId, String>,
    /// The picture each member set, as the image's own bytes, seeded from the workspace's
    /// `workspace/members` resource: what an author's face draws where a view shows them, and in
    /// no record of its own.
    #[cfg(feature = "model")]
    pictures: HashMap<MemberId, collab_types::AvatarBytes>,
    /// The person each agent or bot acts for, as the broker admitted it: a fact a history
    /// hover names, never part of the agent's name.
    acting_for: HashMap<MemberId, MemberId>,
    /// Short workspace-level intentions, separate from pointer/caret state. The
    /// origin remains visible so inferred prose is never presented as a quote.
    statuses: HashMap<MemberId, tilo_presence::LiveStatus>,
    /// Broker-authenticated semantic activity, keyed by invocation. Unlike
    /// pointer/caret presence this never pretends an action caller has an
    /// input device: applications may project its stable resource anchor to
    /// native geometry, while the generic painter shows a compact fallback.
    interactions: HashMap<tilo_actions::InvocationId, interaction::InteractionPresenceEntry>,
    /// Last marker tip actually painted for each agent. This is local visual
    /// continuity only: it never enters pointer presence or action state.
    interaction_markers: HashMap<tilo_actions::Actor, interaction::AgentMarkerContinuity>,
    /// What each write put into a text, by writer and place, drawn where it stands
    /// ([`Self::draw_writes_in`]): an agent's last write in a place stays marked there.
    written: std::cell::RefCell<HashMap<written::WrittenPlace, written::Written>>,
    /// Counts changes to `written` ([`Self::written_generation`]).
    written_generation: std::cell::Cell<u64>,
    /// Per-user "just typed" highlight spans, keyed by presence scope and
    /// faded out by [`PresenceCtx::active_typing`] over [`crate::motion::Trace::LIFE`].
    /// Not serialised — purely a local render effect.
    typing: HashMap<(MemberId, PresenceKey), Vec<TypingMark>>,
    /// When a remote *document* change (not just a caret move) was last
    /// applied, fed by [`Self::note_remote_text_edit`]; the `Caret` arm of
    /// [`Self::apply_remote_presence`] only paints a typing mark when this
    /// is also recent.
    last_remote_text_edit: Option<Instant>,
    /// Draw-time visibility window in scene-space pixels, for spatial culling.
    /// `Cell` so containers (Scrollable) can scope-push a tighter inner
    /// viewport without needing `&mut PresenceCtx` everywhere.
    pub viewport: std::cell::Cell<Option<vello::kurbo::Rect>>,
    /// Where this frame painted each peer's caret name tag, in scene
    /// coordinates, so the pointer layer drawn after the windows can leave
    /// out a second tag with the same name right beside it.
    caret_tags: std::cell::RefCell<Vec<CaretTag>>,
    /// Where this frame drew each peer inside a text widget — the caret
    /// painted in their name, or the band across their selection — in scene
    /// coordinates, and what the words drawn with it say of them. What a
    /// reader is told of a peer writing in text ([`Self::mark_words`]).
    presence_marks: std::cell::RefCell<Vec<PresenceMark>>,
    /// The authority the subtree being drawn holds, scoped by whoever hands
    /// it one (a host, a bound component) through [`Self::with_permissions`]
    /// and read by guarded widgets through [`Self::permits`]. `None` checks
    /// nothing.
    #[cfg(feature = "model")]
    permissions: std::cell::RefCell<Option<crate::permissions::PermissionContext>>,
    /// [`crate::permissions::shared_revision`] as the drawing in progress
    /// sees it. A retained drawing made under another revision is stale:
    /// what a guarded widget paints follows permissions that moved with
    /// nothing in the tree marking it.
    paint_permissions: std::cell::Cell<u64>,
    /// Each peer's last broadcast [`ViewSubset`](tilo_presence::ViewSubset) —
    /// where they're looking, distinct from their cursor. Read by
    /// **attach/follow** to bind the local viewport to a chosen peer.
    views: HashMap<MemberId, tilo_presence::ViewSubset>,
    /// Identities this session publishes its own presence under — what a
    /// frame it sent comes back stamped with. Empty means "unknown", which
    /// suppresses nobody. See [`Self::set_own_identity`].
    own: std::collections::HashSet<MemberId>,
    /// Which member in the workspace this session *is* — the row of the
    /// workspace's `workspace/members` resource that names it. Deliberately not in [`Self::names`], [`Self::here`] or the pointer
    /// and caret stores, which describe *other* people. `None` until the workspace has said.
    self_member: Option<(MemberId, String)>,
    /// When the workspace admitted each member, this session's own
    /// included, Unix milliseconds, as the workspace's `workspace/members`
    /// resource records it.
    joined: HashMap<MemberId, u64>,
    /// The surface this process draws, spelled as [`crate::local_surface_id`] spells it, compared
    /// against a pointer's claimed surface to decide whether a content-space position is this
    /// process's to place. `None` fails closed: a pointer in the wrong window stays invisible
    /// rather than being placed wrong.
    surface: Option<String>,
}

impl PresenceCtx {
    /// True when the supplied rect falls entirely outside the active
    /// viewport, so a widget can skip its draw. Off-viewport widgets MUST
    /// NOT clear their `dirty` flag, or `mark_drawn` (which only visits
    /// widgets whose draw ran) leaves them stuck dirty when scrolled back in.
    pub fn should_cull(&self, rect: vello::kurbo::Rect) -> bool {
        match self.viewport.get() {
            Some(vp) => rect.intersect(vp).is_zero_area(),
            None => false,
        }
    }

    /// The active viewport, when a scrolling ancestor set one.
    pub fn viewport(&self) -> Option<vello::kurbo::Rect> {
        self.viewport.get()
    }

    /// Replace the active viewport, returning the previous value so the caller can restore it.
    pub fn push_viewport(&self, vp: vello::kurbo::Rect) -> Option<vello::kurbo::Rect> {
        self.viewport.replace(Some(vp))
    }

    pub fn pop_viewport(&self, prev: Option<vello::kurbo::Rect>) {
        self.viewport.set(prev);
    }

    /// Draw with `permissions` as the authority guarded widgets check, then
    /// restore the outer one.
    #[cfg(feature = "model")]
    pub fn with_permissions<R>(
        &self,
        permissions: crate::permissions::PermissionContext,
        draw: impl FnOnce() -> R,
    ) -> R {
        let outer = self.permissions.replace(Some(permissions));
        let outer_revision = self
            .paint_permissions
            .replace(crate::permissions::shared_revision());
        let out = draw();
        self.paint_permissions.set(outer_revision);
        self.permissions.replace(outer);
        out
    }

    /// What [`crate::permissions::shared_revision`] was when the drawing in
    /// progress was lent its authority: a retained drawing from another
    /// generation paints what the permissions no longer allow, or not what
    /// they now do.
    pub fn paint_permission_generation(&self) -> u64 {
        self.paint_permissions.get()
    }

    /// Whether the authority of the subtree being drawn covers `scope`.
    /// Outside any [`Self::with_permissions`] everything is allowed.
    #[cfg(feature = "model")]
    pub fn permits(&self, scope: &crate::permissions::PermissionScope) -> bool {
        self.permissions
            .borrow()
            .as_ref()
            .is_none_or(|permissions| permissions.check(scope))
    }

    /// The authority of the subtree being drawn, if anyone scoped one.
    #[cfg(feature = "model")]
    pub fn permission_context(&self) -> Option<crate::permissions::PermissionContext> {
        self.permissions.borrow().clone()
    }

    /// Whether the person the drawing in progress is for may act on what `widget` guards: what
    /// the authority lent through `with_permissions` answers, and yes outside any. Without
    /// Tilo's model no authority is ever lent, so everything is allowed.
    pub fn permits_widget(&self, widget: &dyn crate::widget::Widget) -> bool {
        #[cfg(feature = "model")]
        {
            use crate::permissions::WidgetAuthority as _;
            self.permissions
                .borrow()
                .as_ref()
                .is_none_or(|permissions| permissions.permits_widget(widget))
        }
        #[cfg(not(feature = "model"))]
        {
            let _ = widget;
            true
        }
    }

    /// Forget the caret name tags painted so far: a host calls it before a
    /// frame that draws every window again.
    pub fn clear_caret_tags(&self) {
        self.caret_tags.borrow_mut().clear();
    }

    /// The caret name tags painted since [`Self::clear_caret_tags`], in
    /// scene coordinates.
    pub fn caret_tags(&self) -> Vec<CaretTag> {
        self.caret_tags.borrow().clone()
    }

    pub(crate) fn note_caret_tag(&self, tag: CaretTag) {
        self.caret_tags.borrow_mut().push(tag);
    }

    /// Forget the peers' carets and selections drawn in text so far: a host
    /// calls it before a frame that draws every window again.
    pub fn clear_presence_marks(&self) {
        self.presence_marks.borrow_mut().clear();
    }

    /// The peers' carets and selections a text widget drew since
    /// [`Self::clear_presence_marks`], in scene coordinates.
    pub fn presence_marks(&self) -> Vec<PresenceMark> {
        self.presence_marks.borrow().clone()
    }

    pub(crate) fn note_presence_mark(&self, mark: PresenceMark) {
        self.presence_marks.borrow_mut().push(mark);
    }

    /// The caret just noted says, in the words drawn beside it, what its peer
    /// is doing here (an action's own label): a reader is told that rather
    /// than that the peer is writing.
    pub(crate) fn note_caret_saying(&self, words: &str) {
        if let Some(mark) = self.presence_marks.borrow_mut().last_mut() {
            mark.saying = Some(words.to_owned());
        }
    }

    /// What a reader is told of one peer's caret or selection drawn in text:
    /// the words the widget drew with it when they say what the peer is doing
    /// there, else that the peer is writing — or selecting — there.
    pub fn mark_words(&self, mark: &PresenceMark) -> String {
        if let Some(saying) = &mark.saying {
            return saying.clone();
        }
        crate::i18n::widget_format(
            if mark.selection {
                "presence-selecting-here"
            } else {
                "presence-writing-here"
            },
            &[("name", self.display_name(&mark.user).into())],
        )
    }
}

impl PresenceCtx {
    pub fn empty() -> Self {
        Self::default()
    }

    /// Declare the identities a frame *this* session published comes back
    /// stamped with, so its own presence is never painted as somebody else's.
    ///
    /// Discriminator is the member, not the process or app: two windows
    /// of one app are one person, but an agent editing the same document
    /// joins under its own identity and stays a peer.
    pub fn set_own_identity(&mut self, ids: impl IntoIterator<Item = MemberId>) {
        self.own = ids.into_iter().collect();
    }

    /// Add one more identity of this session without disturbing the others —
    /// [`Self::set_own_identity`] replaces its whole set instead. Returns
    /// whether anything was new.
    pub fn add_own_identity(&mut self, id: MemberId) -> bool {
        self.own.insert(id)
    }

    /// Set from [`crate::local_surface_id`] — the same value the publishing
    /// side scopes its pointer with, so the two spellings cannot drift.
    pub fn set_surface_id(&mut self, surface: impl Into<String>) {
        let surface = surface.into();
        self.surface = (!surface.trim().is_empty()).then_some(surface);
    }

    /// A pointer is in exactly one space, so a frame fanned to every app in
    /// a workspace still has at most one willing to draw it.
    pub fn pointer_owner_for(&self, key: &PresenceKey) -> PointerOwner {
        match key.pointer_space() {
            None => PointerOwner::Window,
            Some(PointerSpace::Shell { .. } | PointerSpace::Tile { .. }) => PointerOwner::Shell,
            Some(PointerSpace::Surface { surface }) => match self.surface.as_deref() {
                Some(mine) if mine == surface => PointerOwner::Window,
                _ => PointerOwner::OtherSurface,
            },
        }
    }

    /// Fed only from the row of the workspace's `workspace/members` resource
    /// that names this session — a machine cannot mint this for itself.
    pub fn set_self_member(&mut self, id: MemberId, name: impl Into<String>) {
        self.self_member = Some((id, name.into()));
    }

    /// Clear the workspace-certified spelling of this session after a complete
    /// roster replacement no longer contains an exact self row. Local
    /// process identities remain in `own`; this only withdraws the workspace's
    /// authenticated answer.
    pub(crate) fn clear_self_member(&mut self) {
        self.self_member = None;
    }

    /// Which member in the workspace this session is, if the workspace has said.
    pub fn self_id(&self) -> Option<&MemberId> {
        self.self_member.as_ref().map(|(id, _)| id)
    }

    /// What the workspace calls this session's own member. `None` for a
    /// member the workspace knows only as an address.
    pub fn self_name(&self) -> Option<&str> {
        self.self_member
            .as_ref()
            .map(|(_, n)| n.trim())
            .filter(|n| !n.is_empty())
    }

    /// Whether `member` is this session's own member: the workspace's self row, or one of the
    /// identities [`Self::set_own_identity`] declared.
    pub fn is_own_identity(&self, member: &MemberId) -> bool {
        self.self_member
            .as_ref()
            .is_some_and(|(id, _)| id == member)
            || self.own.contains(member)
    }

    pub fn register_presence(&mut self, key: &PresenceKey, user: MemberId) {
        let list = self.here.entry(key.clone()).or_default();
        if !list.iter().any(|u| u == &user) {
            list.push(user);
        }
    }

    /// Re-attribute `user` to exactly one `PresenceKey::Tab` at a time, so a
    /// peer switching tabs moves their dot rather than leaving a trail. No-op
    /// (falls back to `register_presence`) if `target` isn't a `Tab` key.
    pub fn set_tab_focus(&mut self, target: &PresenceKey, user: MemberId) {
        if !matches!(target, PresenceKey::Tab { .. }) {
            self.register_presence(target, user);
            return;
        }
        for (k, list) in &mut self.here {
            if matches!(k, PresenceKey::Tab { .. }) && k != target {
                list.retain(|u| u != &user);
            }
        }
        self.register_presence(target, user);
    }

    pub fn set_pointer(&mut self, key: &PresenceKey, user: MemberId, pos: (f32, f32)) {
        self.set_pointer_at(key, user, pos, crate::time::now());
    }

    /// [`Self::set_pointer`] with the clock supplied, so retirement can be
    /// exercised without sleeping. Records the peer as *present* as well as
    /// pointing — deliberately a separate record, since the pointer expires
    /// but being here does not. Single-location like `set_caret`/
    /// `set_selection`: a person has one pointer, so this moves it rather
    /// than adding a new entry under a different scope.
    ///
    /// A move heard at a later instant than the last is a new sample, and
    /// the pointer glides to it ([`Self::pointer_position_at`]). A move
    /// stamped with the *same* instant is that sample re-placed — the tile
    /// it is anchored to moved — so it is carried there with no glide of its
    /// own, any glide already in flight riding along.
    pub fn set_pointer_at(
        &mut self,
        key: &PresenceKey,
        user: MemberId,
        pos: (f32, f32),
        now: Instant,
    ) {
        // Off every other scope first, so a pointer that changed scope leaves
        // nothing painted where it used to be.
        for (k, list) in &mut self.pointers {
            if k != key {
                list.retain(|(u, _)| u != &user);
            }
        }
        self.pointers.retain(|_, list| !list.is_empty());
        self.pointer_seen.retain(|(k, u), _| k == key || u != &user);
        self.pointer_glides
            .retain(|(k, u), _| k == key || u != &user);
        self.pointer_moved
            .retain(|(k, u), _| k == key || u != &user);

        let stored = self.pointer_for(key, &user);
        if stored != Some(pos) {
            self.pointer_moved.insert((key.clone(), user.clone()), now);
        }
        let heard = self.pointer_seen.get(&(key.clone(), user.clone())).copied();
        self.glide_pointer(key, &user, stored.zip(heard), pos, now);
        let list = self.pointers.entry(key.clone()).or_default();
        if let Some(entry) = list.iter_mut().find(|(u, _)| *u == user) {
            entry.1 = pos;
        } else {
            list.push((user.clone(), pos));
        }
        self.pointer_seen.insert((key.clone(), user.clone()), now);
        // Shell scopes only: a pointer inside a surface's content is a
        // position in a document, not a presence dot on the shell. (Not
        // `scopes_a_shell_pointer`, which also answers yes for a routing
        // artefact that is nobody's presence.)
        if matches!(
            key,
            PresenceKey::Workspace { .. }
                | PresenceKey::Pointer {
                    space: PointerSpace::Shell { .. }
                }
        ) {
            self.register_presence(key, user);
        }
    }

    /// Start, keep or drop `user`'s glide at `key` for a move to `pos` heard
    /// at `now`. `stored` is the position held until now and when it was
    /// heard; `None` for a pointer not seen here before.
    fn glide_pointer(
        &mut self,
        key: &PresenceKey,
        user: &MemberId,
        stored: Option<((f32, f32), Instant)>,
        pos: (f32, f32),
        now: Instant,
    ) {
        let id = (key.clone(), user.clone());
        // First sight is placed where it is: there is nowhere to glide from.
        let Some((stored, heard)) = stored else {
            self.pointer_glides.remove(&id);
            return;
        };
        // A restatement, or the same sample carried with its anchor.
        if stored == pos || heard == now {
            return;
        }
        let glide = self.pointer_glides.get(&id).copied();
        let shown = glide.map_or(stored, |glide| glide.at(stored, now));
        let moved = glide.map_or(heard, |glide| glide.moved);
        let span = now.saturating_duration_since(moved).min(POINTER_EASE_MAX);
        if span.is_zero() {
            self.pointer_glides.remove(&id);
            return;
        }
        self.pointer_glides.insert(
            id,
            PointerGlide {
                offset: (shown.0 - pos.0, shown.1 - pos.1),
                since: now,
                span,
                moved: now,
            },
        );
    }

    /// Where to paint `user`'s pointer at `key` at `now`: on its way from where it was shown to
    /// where it was last heard, eased over the gap between its last two samples (at most
    /// [`POINTER_EASE_MAX`]). `None` when there is no such pointer; painters read this rather than
    /// [`Self::pointer_for`]'s raw position.
    pub fn pointer_position_at(
        &self,
        key: &PresenceKey,
        user: &MemberId,
        now: Instant,
    ) -> Option<(f32, f32)> {
        let target = self.pointer_for(key, user)?;
        Some(
            self.pointer_glides
                .get(&(key.clone(), user.clone()))
                .map_or(target, |glide| glide.at(target, now)),
        )
    }

    /// Whether any pointer is still gliding at `now` (see
    /// [`Self::pointer_position_at`]) — a reason to paint the next frame too.
    pub fn pointers_easing(&self, now: Instant) -> bool {
        self.pointer_glides.values().any(|glide| glide.easing(now))
    }

    /// Retire every pointer nobody has restated within `horizon`. Returns
    /// whether anything was retired — a reason to repaint. `horizon` must
    /// comfortably exceed the ~1s restate interval, so this notices a peer
    /// gone quiet rather than blinking one whose keepalive was briefly
    /// delayed. Only the pointer goes — the peer stays in [`Self::here`],
    /// since going quiet about pointing is not leaving — and with it the
    /// peer's [`Self::remote_cursors`] record of that pointer, as
    /// a pointer's removal does, so a peer that vanished leaves nothing
    /// for a window to keep repainting.
    pub fn prune_pointers(&mut self, now: Instant, horizon: Duration) -> bool {
        let seen = &mut self.pointer_seen;
        let mut retired = Vec::new();
        self.pointers.retain(|key, list| {
            list.retain(|(user, _)| {
                let fresh = seen
                    .get(&(key.clone(), user.clone()))
                    .is_some_and(|t| now.saturating_duration_since(*t) < horizon);
                if !fresh {
                    retired.push(user.clone());
                }
                fresh
            });
            !list.is_empty()
        });
        if retired.is_empty() {
            return false;
        }
        seen.retain(|_, t| now.saturating_duration_since(*t) < horizon);
        self.pointer_glides.retain(|id, _| seen.contains_key(id));
        self.pointer_moved.retain(|id, _| seen.contains_key(id));
        for user in retired {
            // A person has one pointer; once it is gone from every scope the
            // cursor record of it goes too.
            if !self
                .pointers
                .values()
                .any(|list| list.iter().any(|(u, _)| *u == user))
            {
                self.remove_remote_cursor(&user);
            }
        }
        true
    }

    /// How opaque to paint `user`'s pointer at `key`: fully until the last
    /// `fade` before it goes quiet for `horizon`, then falling to zero as
    /// [`Self::prune_pointers`] is about to retire it. A zero `fade` (reduced
    /// motion) keeps it opaque until it is retired.
    pub fn pointer_opacity(
        &self,
        key: &PresenceKey,
        user: &MemberId,
        now: Instant,
        horizon: Duration,
        fade: Duration,
    ) -> f32 {
        let Some(seen) = self.pointer_seen.get(&(key.clone(), user.clone())) else {
            return 1.0;
        };
        let left = horizon.saturating_sub(now.saturating_duration_since(*seen));
        crate::motion::Follow::fading(left, fade)
    }

    /// How opaque to paint the name beside `user`'s pointer at `key`: whole while it moves and
    /// while they are talking ([`CursorChats::talking`]), then fading over the last `fade` of
    /// [`POINTER_NAME_LINGER`] once it stands still. A zero `fade` (reduced motion) keeps it whole
    /// until the linger is up.
    pub fn pointer_name_opacity(
        &self,
        key: &PresenceKey,
        user: &MemberId,
        now: Instant,
        fade: Duration,
    ) -> f32 {
        if self.chats.talking(user, now) {
            return 1.0;
        }
        let Some(moved) = self.pointer_moved.get(&(key.clone(), user.clone())) else {
            return 1.0;
        };
        let left = POINTER_NAME_LINGER.saturating_sub(now.saturating_duration_since(*moved));
        if fade.is_zero() {
            return if left.is_zero() { 0.0 } else { 1.0 };
        }
        crate::motion::Follow::fading(left, fade)
    }

    /// What `user` said in `workspace`, or, with no workspace named, in the one workspace this
    /// context hears.
    fn said(
        &self,
        workspace: Option<&str>,
        user: &MemberId,
    ) -> Option<&(crate::Availability, Option<String>)> {
        match workspace {
            Some(workspace) => self.availability.get(&(workspace.to_owned(), user.clone())),
            None => self
                .availability
                .iter()
                .find_map(|((_, who), said)| (who == user).then_some(said)),
        }
    }

    /// The voice channel `user` says they are in, in `workspace`.
    pub fn call_in(&self, workspace: &str, user: &MemberId) -> Option<&str> {
        self.said(Some(workspace), user)
            .and_then(|(_, call)| call.as_deref())
    }

    /// Whether `user` has said how available they are in `workspace`.
    #[must_use]
    pub fn has_availability_in(&self, workspace: &str, user: &MemberId) -> bool {
        self.said(Some(workspace), user).is_some()
    }

    /// How available `user` says they are in the workspace in front.
    pub fn availability_of(&self, user: &MemberId) -> crate::Availability {
        self.said(None, user)
            .map_or(crate::Availability::Available, |(state, _)| *state)
    }

    /// Whether `user` has said how available they are in the workspace in
    /// front.
    #[must_use]
    pub fn has_availability(&self, user: &MemberId) -> bool {
        self.said(None, user).is_some()
    }

    /// The voice channel `user` is in, in the workspace in front.
    pub fn call_of(&self, user: &MemberId) -> Option<&str> {
        self.said(None, user).and_then(|(_, call)| call.as_deref())
    }

    /// Bounce `user`'s face once: they just waved at this person.
    pub fn bounce(&mut self, user: MemberId, motion: crate::tokens::EffectiveMotion, now: Instant) {
        let bounce = self.bounces.entry(user).or_default();
        bounce.pulse(motion, now);
        if let Some(settles) = bounce.settles_at() {
            crate::motion::want_frames_until(settles);
        }
    }

    /// The scale of `user`'s face at `now` (1 at rest).
    pub fn face_scale(&self, user: &MemberId, now: Instant) -> f64 {
        self.bounces
            .get(user)
            .map_or(1.0, |bounce| bounce.scale(now))
    }

    /// Whether any pointer is fading out at `now` (see
    /// [`Self::pointer_opacity`]) — a reason to paint the next frame too.
    pub fn pointers_fading(&self, now: Instant, horizon: Duration, fade: Duration) -> bool {
        self.pointer_seen.values().any(|seen| {
            let age = now.saturating_duration_since(*seen);
            age < horizon && horizon.saturating_sub(age) < fade
        })
    }

    pub fn set_caret(&mut self, key: &PresenceKey, user: MemberId, pos: usize) {
        // A caret is single-location: a peer has at most one. Clear them
        // from every OTHER key first so moving the caret (and the
        // caret-driven focus outline) doesn't leave a stale one behind.
        for (k, list) in &mut self.carets {
            if k != key {
                list.retain(|(u, _)| u != &user);
            }
        }
        let list = self.carets.entry(key.clone()).or_default();
        if let Some(entry) = list.iter_mut().find(|(u, _)| *u == user) {
            entry.1 = pos;
        } else {
            list.push((user, pos));
        }
    }

    pub fn set_selection(&mut self, key: &PresenceKey, user: MemberId, range: SelectionRange) {
        // Single-location, same as the caret: drop this peer's selection
        // from every other key so a moved selection doesn't persist.
        for (k, list) in &mut self.selections {
            if k != key {
                list.retain(|(u, _)| u != &user);
            }
        }
        let list = self.selections.entry(key.clone()).or_default();
        if let Some(entry) = list.iter_mut().find(|(u, _)| *u == user) {
            entry.1 = range;
        } else {
            list.push((user, range));
        }
    }

    pub fn peers_on(&self, key: &PresenceKey) -> &[MemberId] {
        self.here.get(key).map_or(&[], Vec::as_slice)
    }

    pub fn pointer_for(&self, key: &PresenceKey, user: &MemberId) -> Option<(f32, f32)> {
        self.pointers
            .get(key)?
            .iter()
            .find(|(u, _)| u == user)
            .map(|(_, p)| *p)
    }

    pub fn selection_for(&self, key: &PresenceKey, user: &MemberId) -> Option<SelectionRange> {
        self.selections
            .get(key)?
            .iter()
            .find(|(u, _)| u == user)
            .map(|(_, r)| *r)
    }

    pub fn caret_for(&self, key: &PresenceKey, user: &MemberId) -> Option<usize> {
        self.carets
            .get(key)?
            .iter()
            .find(|(u, _)| u == user)
            .map(|(_, c)| *c)
    }

    /// Seed or replace this user's colour override (see [`parse_hex_color`]
    /// for the accepted formats). Malformed input silently clears the
    /// override, falling back to [`user_color`].
    pub fn set_color_override(&mut self, user: MemberId, hex: &str) {
        match parse_hex_color(hex) {
            Some(rgb) => {
                self.color_overrides.insert(user, rgb);
            }
            None => {
                self.color_overrides.remove(&user);
            }
        }
    }

    /// Paint `user` in the presence colour they wear — their choice, or the
    /// one their stable identity derives, as their own machine said it — in
    /// place of the colour their id derives here. Recolours a cursor already
    /// on screen.
    pub fn set_presence_hue(&mut self, user: MemberId, hue: crate::tokens::PresenceHue) {
        let rgb = identity_rgb(hue);
        if self.color_overrides.get(&user) == Some(&rgb) {
            return;
        }
        for cursor in &mut self.remote_cursors {
            if cursor.member == user {
                let alpha = cursor.color[3];
                cursor.color = [
                    f32::from(rgb.0) / 255.0,
                    f32::from(rgb.1) / 255.0,
                    f32::from(rgb.2) / 255.0,
                    alpha,
                ];
            }
        }
        self.color_overrides.insert(user, rgb);
    }

    pub fn color_for(&self, user: &MemberId) -> (u8, u8, u8) {
        self.color_overrides
            .get(user)
            .copied()
            .unwrap_or_else(|| identity_rgb(Self::identity_hue(user)))
    }

    /// The hue `user`'s identity derives where the workspace announced none: a person's
    /// own, and for software the one what it is wears rather than the one its run's id
    /// would — [`crate::tokens::PresenceHue::of_member`], the rule every surface asks.
    fn identity_hue(user: &MemberId) -> crate::tokens::PresenceHue {
        crate::tokens::PresenceHue::of_member(user)
    }

    /// The hue `user` wears: the one the workspace announced for them (their
    /// choice, as the member list said it), else the one their identity
    /// derives. [`Self::color_for`] is this hue's colour, and every surface
    /// paints a person through one of the two, so their face, their caret and
    /// their byline are one colour.
    pub fn hue_for(&self, user: &MemberId) -> crate::tokens::PresenceHue {
        self.color_overrides
            .get(user)
            .and_then(|(red, green, blue)| {
                crate::tokens::PresenceHue::nearest_to_srgb(*red, *green, *blue)
            })
            .unwrap_or_else(|| Self::identity_hue(user))
    }

    /// When the workspace admitted `user`, as its roster records it; `None` when
    /// it has no time for them.
    pub fn set_joined_at(&mut self, user: MemberId, joined: Option<u64>) {
        match joined {
            Some(at) => self.joined.insert(user, at),
            None => self.joined.remove(&user),
        };
    }

    /// Seeded from the collab roster. Overwrites any prior name.
    pub fn set_display_name(&mut self, user: MemberId, name: impl Into<String>) {
        self.names.insert(user.clone(), name.into());
        // Relabel any already-drawn pill too, in case the pointer arrived first.
        self.relabel_cursor(&user);
    }

    fn relabel_cursor(&mut self, user: &MemberId) {
        let shown = self.display_name(user);
        for cursor in &mut self.remote_cursors {
            if cursor.member == *user {
                cursor.label.clone_from(&shown);
            }
        }
    }

    /// The friendly name to show for `user`, this session's own member included, falling back
    /// to the raw id when the roster has not announced one. Never blank. Two members who share a
    /// name both go by it: their faces and colours tell them apart, never a number in the name.
    pub fn display_name(&self, user: &MemberId) -> String {
        self.name_beside_face(user)
    }

    /// The name `user` goes by in a chip, beside their face: theirs, and never joined with what
    /// they are. A face says a person, an agent's own
    /// mark says an agent, and a bot's tile says a bot, so a chip reads "Ada" and "Quill", never
    /// "Quill · Agent". A line of text with no face beside it says [`Self::display_name`].
    pub fn chip_name(&self, user: &MemberId) -> String {
        self.name_beside_face(user)
    }

    /// The name the roster holds for `user`, before what they are is said.
    fn name_beside_face(&self, user: &MemberId) -> String {
        let name = self
            .names
            .get(user)
            .and_then(|name| shown_name(name))
            .or_else(|| {
                self.self_member
                    .as_ref()
                    .filter(|(id, _)| id == user)
                    .and_then(|(_, name)| shown_name(name))
            })
            .map(str::to_owned);
        name.unwrap_or_else(|| unnamed_label(user))
    }

    /// One row of the roster: who `user` is here, in the colour they wear.
    #[cfg(feature = "model")]
    pub fn user(&self, user: &MemberId) -> Member {
        Member {
            id: user.clone(),
            role: None,
            name: self.display_name(user),
            avatar: self.pictures.get(user).cloned(),
            colour: self.hue_for(user),
            joined_at: self.joined.get(user).copied(),
            is_self: self.is_own_identity(user),
            removed: false,
            administrator: false,
            grants: 0,
        }
    }

    /// Called by the `App` after it diffs the document, not on every delta
    /// — a metadata-only delta isn't an edit.
    pub fn note_remote_text_edit(&mut self, now: Instant) {
        self.last_remote_text_edit = Some(now);
    }

    fn edit_landed_recently(&self, now: Instant) -> bool {
        self.last_remote_text_edit
            .is_some_and(|t| now.saturating_duration_since(t) <= EDIT_PULSE_WINDOW)
    }

    /// Adjacent/overlapping marks from the same peer under the same key
    /// coalesce into one growing span with a refreshed birth instant, so a
    /// keystroke burst reads as one moving highlighter, not a stack of tints.
    pub fn record_peer_typing(
        &mut self,
        user: MemberId,
        key: PresenceKey,
        range: SelectionRange,
        now: Instant,
    ) {
        let marks = self.typing.entry((user, key)).or_default();
        // Coalesce with any mark this touches or abuts (ranges within one
        // char count as contiguous typing).
        if let Some(m) = marks.iter_mut().find(|m| {
            range.start <= m.range.end.saturating_add(1)
                && m.range.start <= range.end.saturating_add(1)
        }) {
            m.range.start = m.range.start.min(range.start);
            m.range.end = m.range.end.max(range.end);
            m.born = now;
        } else {
            marks.push(TypingMark { range, born: now });
        }
    }

    /// The peer's still-visible typing spans under `key` at time `now`,
    /// each paired with a fade alpha in `0.0..=1.0` (1.0 = just typed,
    /// tapering linearly to 0 at [`crate::motion::Trace::LIFE`]). This is a read only:
    /// call [`Self::prune_typing`] separately to reclaim expired entries.
    pub fn active_typing(
        &self,
        user: &MemberId,
        key: &PresenceKey,
        now: Instant,
    ) -> Vec<(SelectionRange, f32)> {
        self.typing_marks_for_key(key, now)
            .into_iter()
            .filter(|(typist, _, _)| typist == user)
            .map(|(_, range, alpha)| (range, alpha))
            .collect()
    }

    /// Every peer's typing spans under `key` at `now`, as `(user, range, alpha)`.
    pub fn typing_marks_for_key(
        &self,
        key: &PresenceKey,
        now: Instant,
    ) -> Vec<(MemberId, SelectionRange, f32)> {
        let mut out = Vec::new();
        for ((user, k), marks) in &self.typing {
            if k != key {
                continue;
            }
            for m in marks {
                if let Some(alpha) =
                    crate::motion::Trace::alpha(now.saturating_duration_since(m.born))
                {
                    out.push((user.clone(), m.range, alpha));
                }
            }
        }
        out
    }

    /// Whether anyone else is shown at `key` now: their caret, their
    /// selection, or what they are typing. A field that keeps the drawing it
    /// made of itself asks before keeping it, since what others show there
    /// comes and goes without the field's own content changing.
    #[must_use]
    pub fn shows_anyone_at(&self, key: &PresenceKey) -> bool {
        self.carets.get(key).is_some_and(|list| !list.is_empty())
            || self
                .selections
                .get(key)
                .is_some_and(|list| !list.is_empty())
            || self.typing.keys().any(|(_, typed)| typed == key)
    }

    /// Whether the workspace says anyone is typing anywhere: the clock is only
    /// read to place a fading mark where it says so.
    #[must_use]
    pub fn has_typing_marks(&self) -> bool {
        !self.typing.is_empty()
    }

    /// Every visible typing mark's fade step at `now`, in order: what the
    /// painted highlights look like, so a frame that changes none of them
    /// need not repaint.
    pub(crate) fn typing_steps(&self, now: Instant) -> Vec<u32> {
        let mut steps: Vec<u32> = self
            .typing
            .values()
            .flatten()
            .filter_map(|m| crate::motion::Trace::step(now.saturating_duration_since(m.born)))
            .collect();
        steps.compact_sort();
        steps
    }

    /// Whether a typing highlight is still on screen at `now`.
    pub fn has_active_typing(&self, now: Instant) -> bool {
        self.typing
            .values()
            .flatten()
            .any(|m| now.saturating_duration_since(m.born) < crate::motion::Trace::LIFE)
    }

    /// Call once a frame.
    pub fn prune_typing(&mut self, now: Instant) {
        self.typing.retain(|_, marks| {
            marks.retain(|m| now.saturating_duration_since(m.born) < crate::motion::Trace::LIFE);
            !marks.is_empty()
        });
    }

    /// Never merged, only replaced, so a peer's entry is always exactly what they last published.
    pub fn set_peer_view(
        &mut self,
        user: MemberId,
        view: tilo_presence::ViewSubset,
    ) {
        self.views.insert(user, view);
    }

    /// What **attach/follow** binds the local viewport to.
    pub fn peer_view(&self, user: &MemberId) -> Option<&tilo_presence::ViewSubset> {
        self.views.get(user)
    }

    /// The target a "follow" action scrolls toward — reads the same
    /// `remote_cursors` overlay the pointer is drawn from, so following a
    /// peer lands exactly where their cursor shows.
    pub fn peer_pointer(&self, member: &MemberId) -> Option<[f32; 2]> {
        self.remote_cursors
            .iter()
            .find(|c| c.member == *member)
            .map(|c| c.position)
    }

    /// Returns the id of the peer whose off-screen mist band (the strips
    /// [`draw_remote_presence`] paints) contains `point`, so clicking a
    /// peer's mist selects them to follow.
    pub fn hit_test_offscreen_mist(
        &self,
        viewport: vello::kurbo::Rect,
        point: vello::kurbo::Point,
    ) -> Option<MemberId> {
        for cursor in &self.remote_cursors {
            // Exactly the peers draw_remote_presence paints a band for: never itself.
            if self.is_own_identity(&cursor.member) || !cursor.painted_by_window() {
                continue;
            }
            let target = vello::kurbo::Point::new(
                f64::from(cursor.position[0]),
                f64::from(cursor.position[1]),
            );
            let Some(mist) = edge_mist_for(viewport, target) else {
                continue;
            };
            let band = render::mist_band_rect(viewport, &mist);
            if band.contains(point) {
                return Some(cursor.member.clone());
            }
        }
        None
    }

    /// Upsert a remote peer's cursor into `remote_cursors`. Label defaults to
    /// `member` when `display_name` is `None`.
    pub fn upsert_remote_cursor(
        &mut self,
        member: MemberId,
        position: [f32; 2],
        display_name: Option<&str>,
    ) {
        self.upsert_remote_cursor_faded(member, position, display_name, 1.0);
    }

    /// [`Self::upsert_remote_cursor`] with an explicit opacity, so a caller
    /// that ages cursors out can fade one rather than popping it. Clamped to `0.0..=1.0`.
    pub fn upsert_remote_cursor_faded(
        &mut self,
        member: MemberId,
        position: [f32; 2],
        display_name: Option<&str>,
        alpha: f32,
    ) {
        self.upsert_remote_cursor_owned(
            member,
            position,
            display_name,
            alpha,
            PointerOwner::Window,
        );
    }

    /// [`Self::upsert_remote_cursor_faded`] naming which layer paints the
    /// result — see [`PointerOwner`]. An upsert re-states the owner, so a
    /// peer whose pointer changes transport changes hands too.
    pub fn upsert_remote_cursor_owned(
        &mut self,
        member: MemberId,
        position: [f32; 2],
        display_name: Option<&str>,
        alpha: f32,
        owner: PointerOwner,
    ) {
        // Second API into the same pill (`RemoteCursorMoved` bypasses
        // `apply_remote_presence`); guarding only one API leaves the bug reachable.
        if self.is_own_identity(&member) {
            return;
        }
        let (r, g, b) = self.color_for(&member);
        let color = [
            f32::from(r) / 255.0,
            f32::from(g) / 255.0,
            f32::from(b) / 255.0,
            alpha.clamp(0.0, 1.0),
        ];
        let label = display_name.map_or_else(|| self.display_name(&member), str::to_owned);
        if let Some(existing) = self.remote_cursors.iter_mut().find(|c| c.member == member) {
            existing.position = position;
            existing.color = color;
            existing.owner = owner;
            // Don't overwrite an existing friendly label with the raw id.
            if display_name.is_some() {
                existing.label = label;
            }
        } else {
            self.remote_cursors.push(RemoteCursor {
                member,
                color,
                position,
                label,
                owner,
            });
        }
    }

    pub fn remove_remote_cursor(&mut self, member: &MemberId) {
        self.remote_cursors.retain(|c| c.member != *member);
    }

    /// Note `member` at `key`: their dot on a tab, or their presence in any other scope.
    pub fn register_peer_at(&mut self, member: MemberId, key: &PresenceKey) {
        if matches!(key, PresenceKey::Tab { .. }) {
            self.set_tab_focus(key, member);
        } else {
            self.register_presence(key, member);
        }
    }

    /// When the first composer gone quiet drops out of [`Self::composing_at`]: what a frame that
    /// shows who is typing wakes for, so a writer whose window vanished stops reading as typing
    /// with nothing else happening. `None` while nobody composes.
    #[must_use]
    pub fn composing_ends(&self) -> Option<Instant> {
        self.composing
            .values()
            .min()
            .and_then(|since| since.checked_add(COMPOSING_LINGER))
    }

    /// Who is composing a message in `key` now, the person looking excluded: a channel's
    /// "Mia is typing". A composer gone quiet for [`COMPOSING_LINGER`] without saying they
    /// stopped drops out, so a crashed window leaves no one typing forever.
    #[must_use]
    pub fn composing_at(&self, key: &PresenceKey, now: Instant) -> Vec<MemberId> {
        let mut found: Vec<MemberId> = self
            .composing
            .iter()
            .filter(|((at, _), since)| {
                at == key && now.saturating_duration_since(**since) < COMPOSING_LINGER
            })
            .map(|((_, who), _)| who.clone())
            .collect();
        found.sort();
        found
    }

    /// Drop composers gone quiet for [`COMPOSING_LINGER`]: what a frame does before it asks
    /// [`Self::composing_at`], so the answer does not depend on when it was last asked.
    pub fn prune_composing(&mut self, now: Instant) {
        self.composing
            .retain(|_, since| now.saturating_duration_since(*since) < COMPOSING_LINGER);
    }

    /// Fold one inbound presence frame into this context — the whole decode of a
    /// `RemotePresenceUpdate`. Lives on `PresenceCtx` rather than `App` because the compositor
    /// subscribes to the identical stream, and two decoders would drift. The returned
    /// [`RemotePresenceEffect`] names what a bare presence store can't do for itself.
    #[cfg(feature = "model")]
    pub fn apply_remote_presence(
        &mut self,
        member: MemberId,
        key: PresenceKey,
        payload: crate::collaboration::PresencePayload,
    ) -> RemotePresenceEffect {
        self.apply_remote_presence_at(member, key, payload, crate::time::now())
    }

    /// [`Self::apply_remote_presence`] with the instant the frame was heard supplied. A compositor
    /// re-placing a pointer it heard earlier (its tile moved) passes the original instant, so the
    /// move is carried rather than read as the peer moving again ([`Self::set_pointer_at`]).
    #[cfg(feature = "model")]
    #[allow(
        clippy::too_many_lines,
        reason = "public wire-decode API called from crates outside ui; narrowing payload to &PresencePayload would require updating call sites out of scope for this pass"
    )]
    pub fn apply_remote_presence_at(
        &mut self,
        member: MemberId,
        key: PresenceKey,
        payload: crate::collaboration::PresencePayload,
        now: Instant,
    ) -> RemotePresenceEffect {
        use crate::collaboration::PresencePayload::{
            Arrived, Availability, Caret, Composing, Interaction, Left, Pointer, Selection, Status,
            Typing,
        };

        // A frame this session published, handed back: another app the same
        // person has open. Nothing about it is painted, but `Arrived` still
        // needs answering — it's a joiner's request for the document.
        if self.is_own_identity(&member) {
            // Following yourself is not a thing.
            if key.as_view().is_some() {
                return RemotePresenceEffect::None;
            }
            return match payload {
                // A Tab-keyed arrival moves a focus dot; only a scope
                // arrival affects member-level presence.
                Arrived if !matches!(key, PresenceKey::Tab { .. }) => {
                    tracing::debug!(
                        target: "tilo.state",
                        event = "presence_self_filtered",
                        %member,
                        "ignored own member in peer rendering"
                    );
                    RemotePresenceEffect::PeerArrived
                }
                _ => RemotePresenceEffect::None,
            };
        }

        // A View broadcast rides inside the routing key, not the payload
        // (the subset has no `PresencePayload` variant); the paired payload
        // is inert and never reaches the match below.
        if let Some(subset) = key.as_view() {
            self.set_peer_view(member, subset);
            return RemotePresenceEffect::PeerViewChanged;
        }

        match payload {
            Pointer { x, y } => {
                // Populate BOTH stores: `pointers` (scope-keyed, read by the
                // shell overlay and `pointer_for`) and screen-space
                // `remote_cursors`. Which gets *painted* is `owner`, decided
                // by the frame's coordinate space — see `pointer_owner_for`.
                let uid = member.clone();
                let owner = self.pointer_owner_for(&key);
                self.set_pointer_at(&key, uid, (x, y), now);
                self.upsert_remote_cursor_owned(member, [x, y], None, 1.0, owner);
            }
            Caret { pos } => {
                // Feed BOTH stores: in-app BlockEditor reads `carets`
                // (set_caret); the cross-process overlay reads `remote_caret`.
                let uid = member.clone();
                // Derive a typing mark from a caret that advanced under the
                // same scope, not only from an explicit `Typing` payload —
                // an agent/SDK peer publishing bare caret moves has no
                // other way to show what it wrote. Depends on a document
                // change having actually landed, so a caret merely reading
                // paints nothing.
                if let Some(prev) = self.caret_for(&key, &uid)
                    && pos > prev
                    && pos.saturating_sub(prev) <= TYPING_DERIVE_MAX_RUN
                    && self.edit_landed_recently(now)
                {
                    self.record_peer_typing(
                        uid.clone(),
                        key.clone(),
                        SelectionRange {
                            start: prev,
                            end: pos,
                        },
                        now,
                    );
                }
                self.set_caret(&key, uid, pos);
            }
            Selection { start, end } => {
                let uid = member.clone();
                self.set_selection(&key, uid, SelectionRange { start, end });
            }
            Typing { start, end } => {
                self.record_peer_typing(member, key, SelectionRange { start, end }, now);
            }
            Composing { composing } => {
                // A draft under way in the resource the key names: nothing of what it says,
                // only that someone is writing it. A stop clears it at once.
                let uid = member;
                if composing {
                    self.composing.insert((key, uid), now);
                } else {
                    self.composing.remove(&(key, uid));
                }
            }
            Interaction(update) => {
                // Semantic action presence has its own resource/anchor model.
                // Applying it must never synthesize a pointer, caret, or text
                // selection for an actor that used no such input device. The
                // resource key also selects exactly one app surface: presence
                // is fanned across a workspace, but an action in Writer must not
                // paint an activity card in Files and every other window.
                let belongs_here = match &key {
                    PresenceKey::Resource { app, id, kind } => {
                        self.surface.as_deref() == Some(app.as_str())
                            && interaction::same_or_made_in((id, kind), &update.resource)
                            && update
                                .action
                                .app
                                .as_ref()
                                .is_none_or(|owner| owner.id == *app)
                    }
                    PresenceKey::Widget { .. }
                    | PresenceKey::Tab { .. }
                    | PresenceKey::Workspace { .. }
                    | PresenceKey::App { .. }
                    | PresenceKey::View { .. }
                    | PresenceKey::Pointer { .. } => false,
                };
                if belongs_here && self.apply_interaction(*update).unwrap_or(false) {
                    return RemotePresenceEffect::InteractionAdvanced;
                }
            }
            Status { text, automatic } => {
                // Status is workspace presence, never document/widget presence.
                if matches!(key, PresenceKey::Workspace { .. }) {
                    let uid = member;
                    if text.trim().is_empty() {
                        self.statuses.remove(&uid);
                    } else if text.chars().count() <= crate::MAX_STATUS_CHARS {
                        self.statuses.insert(
                            uid,
                            tilo_presence::LiveStatus {
                                text,
                                origin: if automatic {
                                    tilo_presence::StatusOrigin::Inferred {
                                        generator: "remote-local-model".into(),
                                    }
                                } else {
                                    tilo_presence::StatusOrigin::Manual
                                },
                                updated_at: crate::time::wall(),
                            },
                        );
                    }
                }
            }
            Availability { state, call } => {
                // Workspace presence, like a status, said to that workspace.
                if let PresenceKey::Workspace { id: workspace } = key {
                    let call = call.filter(|call| !call.trim().is_empty());
                    self.availability.insert((workspace, member), (state, call));
                }
            }
            Arrived => {
                // Tab keys move the peer's dot; non-Tab arrivals are (possibly
                // late) joiners, so the caller should reply with full state.
                let joined = !matches!(key, PresenceKey::Tab { .. });
                self.register_peer_at(member, &key);
                if joined {
                    return RemotePresenceEffect::PeerArrived;
                }
            }
            Left => {
                // `Left` fires on mouse leaving the window viewport, not the
                // peer leaving — caret/selection must keep rendering. Do not
                // call `unregister_peer` here: that tears down the whole
                // presence record on every mouse-leave. True disconnect
                // arrives via the socket close path instead.
                let uid = member.clone();
                for list in self.pointers.values_mut() {
                    list.retain(|(u, _)| u != &uid);
                }
                self.pointer_glides.retain(|(_, u), _| u != &uid);
                self.remove_remote_cursor(&member);
                let _ = key;
            }
        }
        RemotePresenceEffect::None
    }

    /// Drop every trace of `member`, on an actual disconnect (not `Left`,
    /// which is just the pointer leaving the viewport — see the arm above).
    pub fn unregister_peer(&mut self, member: &MemberId) {
        let uid = member.clone();
        self.availability.retain(|(_, who), _| *who != uid);
        self.acting_for.remove(&uid);
        for list in self.here.values_mut() {
            list.retain(|u| u != &uid);
        }
        for list in self.pointers.values_mut() {
            list.retain(|(u, _)| u != &uid);
        }
        self.pointer_seen.retain(|(_, u), _| u != &uid);
        self.pointer_glides.retain(|(_, u), _| u != &uid);
        self.pointer_moved.retain(|(_, u), _| u != &uid);
        self.chats.forget(&uid);
        #[cfg(feature = "model")]
        self.pictures.remove(&uid);
        for list in self.carets.values_mut() {
            list.retain(|(u, _)| u != &uid);
        }
        for list in self.selections.values_mut() {
            list.retain(|(u, _)| u != &uid);
        }
        self.typing.retain(|(u, _), _| u != &uid);
        self.views.remove(&uid);
        self.names.remove(&uid);
        self.joined.remove(&uid);
        self.statuses.remove(&uid);
        self.remove_interactions_for_actor(member);
        self.remove_remote_cursor(member);
    }

    /// Current status for a workspace member. Inferred statuses apply their
    /// normal expiry even if a peer vanished without a clean disconnect.
    pub fn status_for(
        &self,
        user: &MemberId,
        now: SystemTime,
    ) -> Option<&tilo_presence::LiveStatus> {
        let status = self.statuses.get(user)?;
        match status.origin {
            tilo_presence::StatusOrigin::Manual => Some(status),
            tilo_presence::StatusOrigin::Inferred { .. }
                if now
                    .duration_since(status.updated_at)
                    .is_ok_and(|age| age <= crate::INFERRED_STATUS_TTL) =>
            {
                Some(status)
            }
            tilo_presence::StatusOrigin::Inferred { .. } => None,
        }
    }
}

impl crate::permissions::WidgetAuthority for PresenceCtx {
    fn permits_widget(&self, widget: &dyn crate::widget::Widget) -> bool {
        PresenceCtx::permits_widget(self, widget)
    }
}

/// Parse a `#rgb` / `#rrggbb` / `#rrggbbaa` colour string to an RGB triple
/// (see [`crate::color::parse_hex`]). Alpha is discarded; `None` for anything
/// unparseable.
pub fn parse_hex_color(hex: &str) -> Option<(u8, u8, u8)> {
    crate::color::parse_hex(hex).map(|c| {
        let c = c.to_rgba8();
        (c.r, c.g, c.b)
    })
}

#[cfg(test)]
mod tab_focus_tests {
    use super::*;

    tilo_test_env::scenario! {
        fn set_tab_focus_moves_only_that_user_and_only_between_tabs;

        phase set_tab_focus_moves_user_between_tabs;
        {
            let mut ctx = PresenceCtx::empty();
            let alice = MemberId::person("alice");
            let a = PresenceKey::Tab { id: "aaa".into() };
            let b = PresenceKey::Tab { id: "bbb".into() };

            ctx.set_tab_focus(&a, alice.clone());
            assert_eq!(ctx.peers_on(&a), std::slice::from_ref(&alice));
            assert!(ctx.peers_on(&b).is_empty());

            ctx.set_tab_focus(&b, alice.clone());
            assert!(
                ctx.peers_on(&a).is_empty(),
                "alice must be cleared from her previous tab"
            );
            assert_eq!(ctx.peers_on(&b), &[alice]);
        }

        phase set_tab_focus_leaves_other_users_on_the_old_tab;
        {
            let mut ctx = PresenceCtx::empty();
            let alice = MemberId::person("alice");
            let bob = MemberId::person("bob");
            let a = PresenceKey::Tab { id: "aaa".into() };
            let b = PresenceKey::Tab { id: "bbb".into() };

            ctx.set_tab_focus(&a, alice.clone());
            ctx.set_tab_focus(&a, bob.clone());
            ctx.set_tab_focus(&b, alice.clone());

            assert_eq!(ctx.peers_on(&a), &[bob]);
            assert_eq!(ctx.peers_on(&b), &[alice]);
        }

        phase set_tab_focus_on_non_tab_key_falls_back_to_register_presence;
        {
            let mut ctx = PresenceCtx::empty();
            let alice = MemberId::person("alice");
            let tab = PresenceKey::Tab { id: "aaa".into() };
            let ws = PresenceKey::Workspace { id: "home".into() };

            ctx.set_tab_focus(&tab, alice.clone());
            ctx.set_tab_focus(&ws, alice.clone());

            assert_eq!(
                ctx.peers_on(&tab),
                std::slice::from_ref(&alice),
                "workspace registration must not evict the tab attribution"
            );
            assert_eq!(ctx.peers_on(&ws), &[alice]);
        }
    }
}

#[cfg(test)]
mod color_override_tests {
    use super::*;

    tilo_test_env::scenario! {
        fn a_colour_override_is_parsed_and_worn_everywhere;

        phase colour_overrides_parse_hex_and_otherwise_fall_back_to_the_palette;
        {
            // parse_hex_color accepts the common forms and rejects malformed input.
            for (input, want) in [
                ("#ff0000", Some((0xff, 0x00, 0x00))),
                ("00ff00", Some((0x00, 0xff, 0x00))),
                ("#0000ffee", Some((0x00, 0x00, 0xff))),
                ("#f0a", Some((0xff, 0x00, 0xaa))),
                ("", None),
                ("#12", None),
                ("#zzzzzz", None),
                ("not-a-color", None),
            ] {
                assert_eq!(parse_hex_color(input), want, "case {input:?}");
            }

            // color_for falls through to the palette without an override.
            let ctx = PresenceCtx::empty();
            assert_eq!(ctx.color_for(&MemberId::person("alice")), user_color("alice"));

            // color_for honours an override when set.
            let mut ctx = PresenceCtx::empty();
            ctx.set_color_override(MemberId::person("alice"), "#abcdef");
            assert_eq!(ctx.color_for(&MemberId::person("alice")), (0xab, 0xcd, 0xef));
            assert_eq!(ctx.color_for(&MemberId::person("bob")), user_color("bob"));

            // A malformed override clears any prior entry.
            let mut ctx = PresenceCtx::empty();
            ctx.set_color_override(MemberId::person("alice"), "#abcdef");
            ctx.set_color_override(MemberId::person("alice"), "nonsense");
            assert_eq!(ctx.color_for(&MemberId::person("alice")), user_color("alice"));
        }

        phase the_announced_hue_is_the_one_a_person_wears_everywhere;
        // One person is one colour: the hue the workspace announced for them is
        // the one their face, their caret and their byline wear, and only someone
        // who announced none is painted in what their id derives.
        {
            let mut ctx = PresenceCtx::empty();
            let ada = MemberId::person("ada");
            assert_eq!(
                ctx.hue_for(&crate::permissions::MemberId::person("ada")),
                crate::tokens::PresenceHue::of_identity("ada"),
                "nobody announced a colour for her yet"
            );
            ctx.set_presence_hue(ada.clone(), crate::tokens::PresenceHue::Violet);
            assert_eq!(ctx.hue_for(&crate::permissions::MemberId::person("ada")), crate::tokens::PresenceHue::Violet);
            assert_eq!(
                ctx.color_for(&ada),
                identity_rgb(crate::tokens::PresenceHue::Violet),
                "the colour they are painted in is the hue they wear"
            );
            assert_eq!(
                ctx.hue_for(&crate::permissions::MemberId::person("bo")),
                crate::tokens::PresenceHue::of_identity("bo"),
                "someone else keeps their own"
            );
        }
    }
}

#[cfg(test)]
#[cfg(feature = "model")]
mod remote_cursor_tests {
    use super::*;

    tilo_test_env::scenario! {
        fn a_remote_cursor_wears_its_colour_and_upserts_in_place;

        phase a_peer_wears_the_colour_their_machine_says;
        {
            let mut ctx = PresenceCtx::empty();
            let peer = MemberId::person("peer@42");
            ctx.upsert_remote_cursor(peer.clone(), [1.0, 2.0], None);
            assert_eq!(ctx.color_for(&peer), user_color("peer@42"));
            ctx.set_presence_hue(peer.clone(), crate::tokens::PresenceHue::Violet);
            let violet = identity_rgb(crate::tokens::PresenceHue::Violet);
            assert_eq!(ctx.color_for(&peer), violet);
            let painted = ctx.remote_cursors[0].color;
            assert!((painted[0] - f32::from(violet.0) / 255.0).abs() < 1e-3);
            assert!((painted[3] - 1.0).abs() < f32::EPSILON, "alpha is kept");
            let packed = collab_types::Member::of_person(
                &collab_types::Person::new(peer.id(), "Ada")
                    .wearing(crate::tokens::PresenceHue::Violet),
                None,
            )
            .packed_colour();
            assert_eq!(
                crate::collaboration::colour_from_packed(packed),
                Some(crate::tokens::PresenceHue::Violet),
                "every presence colour survives the roster's wire"
            );
            for hue in crate::tokens::PresenceHue::ALL {
                let (r, g, b) = identity_rgb(hue);
                let packed = (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b);
                assert_eq!(crate::collaboration::colour_from_packed(packed), Some(hue));
            }
            assert_eq!(crate::collaboration::colour_from_packed(0), None);
        }

        phase remote_cursors_upsert_in_place_and_remove;
        {
            // phase: upsert_remote_cursor_inserts_new_entry
            {
                let mut ctx = PresenceCtx::empty();
                ctx.upsert_remote_cursor(MemberId::person("alice"), [42.0, 17.0], None);
                assert_eq!(ctx.remote_cursors.len(), 1);
                let c = &ctx.remote_cursors[0];
                assert_eq!(c.member, MemberId::person("alice"));
                assert_eq!(c.position, [42.0, 17.0]);
                assert_eq!(c.label, unnamed_person_label(), "an unnamed person is someone without a name yet");
                let (r, g, b) = user_color("alice");
                assert!((c.color[0] - f32::from(r) / 255.0).abs() < 1e-3);
                assert!((c.color[1] - f32::from(g) / 255.0).abs() < 1e-3);
                assert!((c.color[2] - f32::from(b) / 255.0).abs() < 1e-3);
                assert_eq!(c.color[3], 1.0);
            }
            // phase: upsert_remote_cursor_updates_existing_in_place
            {
                let mut ctx = PresenceCtx::empty();
                ctx.upsert_remote_cursor(MemberId::person("bob"), [10.0, 10.0], None);
                ctx.upsert_remote_cursor(MemberId::person("bob"), [200.0, 300.0], None);
                assert_eq!(
                    ctx.remote_cursors.len(),
                    1,
                    "second upsert for same user_id must not push a duplicate",
                );
                assert_eq!(ctx.remote_cursors[0].position, [200.0, 300.0]);
            }
            // phase: upsert_remote_cursor_preserves_friendly_label_when_display_name_is_none
            {
                let mut ctx = PresenceCtx::empty();
                ctx.upsert_remote_cursor(MemberId::person("alice"), [0.0, 0.0], Some("Alice Smith"));
                assert_eq!(ctx.remote_cursors[0].label, "Alice Smith");

                ctx.upsert_remote_cursor(MemberId::person("alice"), [50.0, 50.0], None);
                assert_eq!(ctx.remote_cursors.len(), 1);
                assert_eq!(ctx.remote_cursors[0].position, [50.0, 50.0]);
                assert_eq!(
                    ctx.remote_cursors[0].label, "Alice Smith",
                    "display_name=None must not overwrite a previously-set friendly label",
                );
            }
            // phase: remove_remote_cursor_clears_entry
            {
                let mut ctx = PresenceCtx::empty();
                ctx.upsert_remote_cursor(MemberId::person("alice"), [1.0, 2.0], None);
                ctx.upsert_remote_cursor(MemberId::person("bob"), [3.0, 4.0], None);
                assert_eq!(ctx.remote_cursors.len(), 2);
                ctx.remove_remote_cursor(&MemberId::person("alice"));
                assert_eq!(ctx.remote_cursors.len(), 1);
                assert_eq!(ctx.remote_cursors[0].member, MemberId::person("bob"));
                ctx.remove_remote_cursor(&MemberId::person("nobody"));
                assert_eq!(ctx.remote_cursors.len(), 1);
            }
        }
    }
}

#[cfg(test)]
#[cfg(feature = "model")]
mod string_id_wrapper_tests {
    use super::*;

    tilo_test_env::scenario! {
        fn each_pointer_is_painted_by_exactly_the_layer_that_owns_its_space;

        phase a_desktop_pointer_is_the_compositors_to_paint_and_not_this_layers;
        // One person is one pointer: a workspace-scoped arrival lands in *both*
        // stores — `pointers` (compositor paints) and `remote_cursors` ("follow"
        // reads) — but only one of them may paint it.
        {
            let mut ctx = PresenceCtx::empty();
            let workspace = "40fc5b85-22d7-4294-a9df-ecc8a391cfce";
            let key = PresenceKey::Workspace {
                id: workspace.into(),
            };

            ctx.apply_remote_presence(
                MemberId::person("peer@77"),
                key.clone(),
                crate::collaboration::PresencePayload::Pointer { x: 120.0, y: 90.0 },
            );

            // The compositor's store has them: this is not "the peer never
            // arrived" passing for "the peer is not drawn twice".
            assert_eq!(
                ctx.pointer_for(&key, &MemberId::person("peer@77")),
                Some((120.0, 90.0)),
                "the overlay must still have a desktop pointer to paint",
            );
            assert_eq!(ctx.peer_pointer(&MemberId::person("peer@77")), Some([120.0, 90.0]));

            assert_eq!(ctx.remote_cursors.len(), 1, "one arrival is one record");
            assert!(
                !ctx.remote_cursors[0].painted_by_window(),
                "a desktop pointer painted here as well is the same person twice",
            );
        }

        phase a_pointer_no_overlay_claims_is_still_this_layers_to_paint;
        // The other half: a pointer with no desktop scope has no overlay
        // claiming it, so this layer stays the one that draws it.
        {
            let mut ctx = PresenceCtx::empty();

            // The relay/SDK path: a cursor with no scope at all.
            ctx.upsert_remote_cursor(MemberId::person("peer@77"), [10.0, 20.0], None);
            assert!(
                ctx.remote_cursors[0].painted_by_window(),
                "dropping this one would leave the relay dialect with no pointer at all",
            );

            let widget = PresenceKey::Widget {
                path: vec![PathSegment::Index(0)],
            };
            ctx.apply_remote_presence(
                MemberId::person("peer@88"),
                widget,
                crate::collaboration::PresencePayload::Pointer { x: 5.0, y: 6.0 },
            );
            let scoped = ctx
                .remote_cursors
                .iter()
                .find(|c| c.member == MemberId::person("peer@88"))
                .expect("the pointer landed");
            assert!(
                scoped.painted_by_window(),
                "only a desktop scope hands a pointer to the compositor",
            );
        }

        phase one_content_pointer_reaches_two_surfaces_and_one_places_it;
        // The same frame, handed to two surfaces, is placeable by one of them —
        // each asks the same question of the same carried fact, without
        // consulting the other.
        {
            let frame = PresenceKey::surface_pointer("so.tilo.word-processor");

            let mut word_processor = PresenceCtx::empty();
            word_processor.set_surface_id("so.tilo.word-processor");
            let mut file_manager = PresenceCtx::empty();
            file_manager.set_surface_id("so.tilo.files");

            for ctx in [&mut word_processor, &mut file_manager] {
                ctx.apply_remote_presence(
                    MemberId::person("peer@77"),
                    frame.clone(),
                    crate::collaboration::PresencePayload::Pointer { x: 214.0, y: 34.0 },
                );
            }

            let drawn = |ctx: &PresenceCtx| {
                ctx.remote_cursors
                    .iter()
                    .filter(|c| c.painted_by_window())
                    .count()
            };
            assert_eq!(
                drawn(&word_processor),
                1,
                "the surface those pixels were measured in draws them",
            );
            assert_eq!(
                drawn(&file_manager),
                0,
                "the same offset into a file list is not the place a word \
             processor's (214, 34) describes",
            );
            assert_eq!(
                file_manager.pointer_owner_for(&frame),
                PointerOwner::OtherSurface,
                "and it can say so from the frame alone",
            );
        }

        phase a_session_that_has_not_named_its_surface_places_no_content_pointer;
        // A session that has not said which surface it draws places no content
        // pointer at all.
        //
        // Fail closed. The alternative is to place it anyway and be right only
        // when the guess happens to hold — and a pointer in the wrong window
        // looks exactly like a pointer in the right one, so nothing would ever
        // report the guess as wrong.
        {
            let mut ctx = PresenceCtx::empty();
            let key = PresenceKey::surface_pointer("so.tilo.word-processor");
            assert_eq!(ctx.pointer_owner_for(&key), PointerOwner::OtherSurface);
            ctx.apply_remote_presence(
                MemberId::person("peer@77"),
                key,
                crate::collaboration::PresencePayload::Pointer { x: 1.0, y: 2.0 },
            );
            assert!(
                ctx.remote_cursors.iter().all(|c| !c.painted_by_window()),
                "an unnamed surface must not claim somebody else's coordinates",
            );
        }

        phase a_pointer_that_changes_transport_changes_hands;
        // A peer moving transport changes hands rather than keeping its first owner.
        {
            let mut ctx = PresenceCtx::empty();
            let key = PresenceKey::Workspace {
                id: "workspace".into(),
            };
            ctx.apply_remote_presence(
                MemberId::person("peer@77"),
                key,
                crate::collaboration::PresencePayload::Pointer { x: 1.0, y: 2.0 },
            );
            assert!(!ctx.remote_cursors[0].painted_by_window());

            ctx.upsert_remote_cursor(MemberId::person("peer@77"), [3.0, 4.0], None);

            assert_eq!(ctx.remote_cursors.len(), 1, "still one person");
            assert!(
                ctx.remote_cursors[0].painted_by_window(),
                "a peer nobody else is painting must come back to this layer",
            );
        }
    }

    tilo_test_env::scenario! {
        fn peer_state_is_written_dropped_labelled_and_faded_by_id;

        phase peer_state_writes_land_in_their_maps_and_relationships;
        {
            // phase: a_caret_is_kept_in_the_carets_map
            {
                let mut ctx = PresenceCtx::empty();
                let key = PresenceKey::Workspace { id: "home".into() };
                ctx.set_caret(&key.clone(), MemberId::person("alice"), 42);
                assert_eq!(ctx.caret_for(&key, &MemberId::person("alice")), Some(42));
            }
            // phase: a_selection_is_kept_in_the_selections_map
            {
                let mut ctx = PresenceCtx::empty();
                let key = PresenceKey::Workspace { id: "home".into() };
                ctx.set_selection(&key.clone(), MemberId::person("bob"), crate::presence::SelectionRange { start: 3, end: 9 });
                let r = ctx
                    .selection_for(&key, &MemberId::person("bob"))
                    .expect("selection");
                assert_eq!((r.start, r.end), (3, 9));
            }
            // phase: register_peer_at_workspace_adds_to_here
            {
                let mut ctx = PresenceCtx::empty();
                let key = PresenceKey::Workspace { id: "home".into() };
                ctx.register_peer_at(MemberId::person("alice"), &key.clone());
                assert_eq!(ctx.peers_on(&key), &[MemberId::person("alice")]);
            }
            // phase: register_peer_at_tab_routes_through_set_tab_focus
            {
                // A peer switching between two tab keys must end up on exactly
                // one — the previous tab should drop them. This guards the
                // routing through set_tab_focus.
                let mut ctx = PresenceCtx::empty();
                let a = PresenceKey::Tab { id: "aaa".into() };
                let b = PresenceKey::Tab { id: "bbb".into() };
                ctx.register_peer_at(MemberId::person("alice"), &a.clone());
                ctx.register_peer_at(MemberId::person("alice"), &b.clone());
                assert!(
                    ctx.peers_on(&a).is_empty(),
                    "peer must have been evicted from previous tab"
                );
                assert_eq!(ctx.peers_on(&b), &[MemberId::person("alice")]);
            }
            // phase: peer_pointer_reads_follow_target
            {
                let mut ctx = PresenceCtx::empty();
                ctx.upsert_remote_cursor(MemberId::person("alice"), [120.0, 44.0], None);
                assert_eq!(ctx.peer_pointer(&MemberId::person("alice")), Some([120.0, 44.0]));
                assert_eq!(ctx.peer_pointer(&MemberId::person("nobody")), None);
            }
        }

        phase unregistering_a_peer_drops_only_its_traces;
        {
            // phase: unregister_peer_drops_every_trace
            {
                let mut ctx = PresenceCtx::empty();
                let key_a = PresenceKey::Workspace { id: "home".into() };
                let key_b = PresenceKey::Tab { id: "tab1".into() };

                ctx.register_peer_at(MemberId::person("alice"), &key_a.clone());
                ctx.register_peer_at(MemberId::person("alice"), &key_b.clone());
                ctx.set_caret(&key_a.clone(), MemberId::person("alice"), 5);
                ctx.set_selection(&key_a.clone(), MemberId::person("alice"), crate::presence::SelectionRange { start: 1, end: 4 });
                ctx.set_pointer(&key_a, MemberId::person("alice"), (10.0, 20.0));
                ctx.upsert_remote_cursor(MemberId::person("alice"), [10.0, 20.0], None);

                // Sanity: alice is everywhere.
                assert!(!ctx.peers_on(&key_a).is_empty());
                assert!(!ctx.peers_on(&key_b).is_empty());
                assert!(ctx.caret_for(&key_a, &MemberId::person("alice")).is_some());
                assert!(ctx.selection_for(&key_a, &MemberId::person("alice")).is_some());
                assert!(ctx.pointer_for(&key_a, &MemberId::person("alice")).is_some());
                assert_eq!(ctx.remote_cursors.len(), 1);

                ctx.unregister_peer(&MemberId::person("alice"));

                // Alice is purged.
                assert!(ctx.peers_on(&key_a).is_empty());
                assert!(ctx.peers_on(&key_b).is_empty());
                assert!(ctx.caret_for(&key_a, &MemberId::person("alice")).is_none());
                assert!(ctx.selection_for(&key_a, &MemberId::person("alice")).is_none());
                assert!(ctx.pointer_for(&key_a, &MemberId::person("alice")).is_none());
                assert!(ctx.remote_cursors.is_empty());
            }
            // phase: unregister_peer_clears_typing_and_name
            {
                let mut ctx = PresenceCtx::empty();
                let alice = MemberId::person("alice");
                let key = PresenceKey::Tab { id: "t".into() };
                ctx.set_display_name(alice.clone(), "Alice");
                ctx.record_peer_typing(
                    alice.clone(),
                    key.clone(),
                    SelectionRange { start: 0, end: 2 },
                    crate::time::now(),
                );
                assert!(
                    !ctx.active_typing(&alice, &key, crate::time::now())
                        .is_empty()
                );
                ctx.unregister_peer(&MemberId::person("alice"));
                assert!(
                    ctx.active_typing(&alice, &key, crate::time::now())
                        .is_empty()
                );
                assert_eq!(
                    ctx.display_name(&alice),
                    unnamed_person_label(),
                    "a person whose name went is a guest again"
                );
            }
            // phase: unregister_peer_leaves_other_users_alone
            {
                // Removing alice must not collateral-damage bob.
                let mut ctx = PresenceCtx::empty();
                let key = PresenceKey::Workspace { id: "home".into() };
                ctx.register_peer_at(MemberId::person("alice"), &key.clone());
                ctx.register_peer_at(MemberId::person("bob"), &key.clone());
                ctx.set_caret(&key.clone(), MemberId::person("bob"), 7);
                ctx.upsert_remote_cursor(MemberId::person("bob"), [99.0, 99.0], None);

                ctx.unregister_peer(&MemberId::person("alice"));

                assert_eq!(ctx.peers_on(&key), &[MemberId::person("bob")]);
                assert_eq!(ctx.caret_for(&key, &MemberId::person("bob")), Some(7));
                assert_eq!(ctx.remote_cursors.len(), 1);
                assert_eq!(ctx.remote_cursors[0].member, MemberId::person("bob"));
            }
        }

        phase peers_keep_stable_colours_and_honest_labels_in_any_arrival_order;
        {
            // phase: presence_user_color_is_stable_and_distinct
            {
                // Same id → same colour every time (deterministic, no shared state).
                let mut ctx = PresenceCtx::empty();
                ctx.set_display_name(MemberId::person("alice"), "Alice Smith");
                let a1 = ctx.user(&MemberId::person("alice"));
                assert_eq!(a1.colour, crate::tokens::PresenceHue::of_identity("alice"));
                assert_eq!(identity_rgb(a1.colour), user_color("alice"));
                // Distinct ids → distinct colours (the whole point of the palette).
                let bob = ctx.user(&MemberId::person("bob"));
                assert_ne!(a1.colour, bob.colour);
                assert_eq!(a1.name, "Alice Smith");
            }
            // phase: roster_name_resolves_with_id_fallback
            {
                let mut ctx = PresenceCtx::empty();
                let alice = MemberId::person("alice");
                // No roster entry yet: a guest, never their id.
                assert_eq!(ctx.display_name(&alice), unnamed_person_label());
                ctx.set_display_name(alice.clone(), "Alice Smith");
                assert_eq!(ctx.display_name(&alice), "Alice Smith");
                // `user()` bundles id + name + colour.
                let pu = ctx.user(&alice);
                assert_eq!(pu.name, "Alice Smith");
                assert_eq!(identity_rgb(pu.colour), user_color("alice"));
            }
            // phase: an_agent_stays_one_when_renamed
            {
                let mut ctx = PresenceCtx::empty();
                let scribe = MemberId::agent("scribe", "ab".repeat(32).clone());
                ctx.set_display_name(scribe.clone(), "Scribe");
                assert_eq!(ctx.display_name(&scribe), "Scribe");
                ctx.set_display_name(scribe.clone(), "Editorial helper");
                assert!(!ctx.user(&scribe).id.is_person());
                assert_eq!(ctx.display_name(&scribe), "Editorial helper");
            }
            // phase: an_unnamed_peer_is_a_guest_and_never_its_id
            // A person nobody named is a guest on every surface: never blank, and never their key,
            // which reads as a fault on a caret chip.
            {
                let ctx = PresenceCtx::empty();
                let peer = MemberId::person("ab".repeat(32));
                let label = ctx.display_name(&peer);
                assert_eq!(label, unnamed_person_label());
                assert_eq!(
                    ctx.user(&peer).name,
                    unnamed_person_label(),
                    "every renderer resolves the same way",
                );
            }
            // phase: a_peer_that_gains_a_name_keeps_its_colour
            // Colour is keyed on the id; gaining a name must not perturb it.
            {
                let mut ctx = PresenceCtx::empty();
                let peer = MemberId::person("peer@5537458103208628978");
                let before = ctx.color_for(&peer);

                ctx.set_display_name(peer.clone(), "Bob");

                assert_eq!(ctx.display_name(&peer), "Bob");
                assert_eq!(
                    ctx.color_for(&peer),
                    before,
                    "a peer must not change colour in front of the people watching it",
                );
            }
            // phase: a_name_that_arrives_after_the_pointer_relabels_it
            // A name can arrive after the pointer it belongs to. The pill has to
            // pick it up, or a peer stays a guest on screen until they happen
            // to move again.
            {
                let mut ctx = PresenceCtx::empty();
                ctx.upsert_remote_cursor(MemberId::person("peer@77"), [10.0, 20.0], None);
                assert_eq!(ctx.remote_cursors[0].label, unnamed_person_label());

                ctx.set_display_name(MemberId::person("peer@77"), "Bob");

                assert_eq!(
                    ctx.remote_cursors.len(),
                    1,
                    "naming a peer adds no second pointer"
                );
                assert_eq!(ctx.remote_cursors[0].label, "Bob");
                assert_eq!(
                    ctx.remote_cursors[0].position,
                    [10.0, 20.0],
                    "and moves none"
                );
            }
            // phase: a_pointer_that_arrives_after_the_name_is_labelled_with_it
            // And the other order: a name learned from the roster before the
            // peer's first move labels that move when it lands.
            {
                let mut ctx = PresenceCtx::empty();
                ctx.set_display_name(MemberId::person("peer@77"), "Bob");

                ctx.upsert_remote_cursor(MemberId::person("peer@77"), [10.0, 20.0], None);

                assert_eq!(ctx.remote_cursors[0].label, "Bob");
            }
        }

        phase people_sharing_a_name_both_go_by_it_for_everyone_including_themselves;
        {
            let ada = MemberId::person("key-a");
            let other_ada = MemberId::person("key-b");
            let helper = MemberId::agent("helper", "key-b");
            let other_helper = MemberId::agent("helper-bea", "key-b");
            let mut mine = PresenceCtx::empty();
            mine.upsert_remote_cursor(other_ada.clone(), [1.0, 1.0], Some("ada"));
            mine.set_self_member(ada.clone(), "Ada");
            mine.set_display_name(other_ada.clone(), "ada");
            mine.set_display_name(helper.clone(), "Helper");
            mine.set_display_name(other_helper.clone(), "Helper");
            assert_eq!(mine.display_name(&ada), "Ada");
            assert_eq!(mine.display_name(&other_ada), "ada");
            assert_eq!(mine.self_label().as_deref(), Some("Ada"));
            assert_eq!(mine.remote_cursors[0].label, "ada", "a name is never numbered");
            assert_eq!(mine.display_name(&helper), "Helper");
            assert_eq!(mine.display_name(&other_helper), "Helper");
            mine.set_display_name(other_ada.clone(), "Bea");
            assert_eq!(mine.remote_cursors[0].label, "Bea", "a pill follows a new name");
        }

        phase typing_marks_fade_prune_and_coalesce;
        {
            // phase: typing_mark_fades_linearly_and_prunes
            {
                use crate::time::Duration;
                let mut ctx = PresenceCtx::empty();
                let bob = MemberId::person("bob");
                let key = PresenceKey::Widget {
                    path: vec![PathSegment::Index(0)],
                };
                let t0 = crate::time::now();
                ctx.record_peer_typing(
                    bob.clone(),
                    key.clone(),
                    SelectionRange { start: 3, end: 8 },
                    t0,
                );

                // Fresh: alpha ~1.0, the full range present.
                let fresh = ctx.active_typing(&bob, &key, t0);
                assert_eq!(fresh.len(), 1);
                assert_eq!((fresh[0].0.start, fresh[0].0.end), (3, 8));
                assert!((fresh[0].1 - 1.0).abs() < 1e-3, "just-typed alpha is ~1.0");

                // Half-way through the fade window: alpha ~0.5.
                let mid = t0 + crate::motion::Trace::LIFE / 2;
                let half = ctx.active_typing(&bob, &key, mid);
                assert_eq!(half.len(), 1);
                assert!(
                    (half[0].1 - 0.5).abs() < 0.05,
                    "alpha halves at mid-life, got {}",
                    half[0].1
                );
                assert!(ctx.has_active_typing(mid));
                assert_eq!(
                    ctx.widgets_change_at(mid),
                    Some(t0 + crate::motion::Trace::LIFE / 2 + crate::motion::Trace::LIFE / crate::motion::Trace::STEPS),
                    "the fade wakes the host at its next step, not every frame"
                );
                assert_eq!(
                    ctx.active_typing(&bob, &key, mid + Duration::from_millis(10))[0].1,
                    half[0].1,
                    "within a step the highlight does not change"
                );

                // Past the window: gone from the read, and prune reclaims it.
                let after = t0 + crate::motion::Trace::LIFE + Duration::from_millis(1);
                assert!(ctx.active_typing(&bob, &key, after).is_empty());
                assert!(!ctx.has_active_typing(after));
                assert!(!ctx.typing.is_empty(), "not yet pruned");
                ctx.prune_typing(after);
                assert!(ctx.typing.is_empty(), "prune reclaims fully-faded marks");
            }
            // phase: typing_marks_coalesce_when_contiguous
            {
                let mut ctx = PresenceCtx::empty();
                let bob = MemberId::person("bob");
                let key = PresenceKey::Widget {
                    path: vec![PathSegment::Index(1)],
                };
                let t0 = crate::time::now();
                ctx.record_peer_typing(
                    bob.clone(),
                    key.clone(),
                    SelectionRange { start: 0, end: 3 },
                    t0,
                );
                // A contiguous continuation (abuts the previous end) extends the span.
                ctx.record_peer_typing(
                    bob.clone(),
                    key.clone(),
                    SelectionRange { start: 3, end: 6 },
                    t0,
                );
                let marks = ctx.active_typing(&bob, &key, t0);
                assert_eq!(marks.len(), 1, "contiguous typing coalesces into one span");
                assert_eq!((marks[0].0.start, marks[0].0.end), (0, 6));

                // A disjoint edit elsewhere in the same block stays separate.
                ctx.record_peer_typing(
                    bob.clone(),
                    key.clone(),
                    SelectionRange { start: 40, end: 44 },
                    t0,
                );
                let marks = ctx.active_typing(&bob, &key, t0);
                assert_eq!(marks.len(), 2, "a far-away edit is its own span");
            }
        }
    }
}

/// A person has one pointer, and the screen has to agree.
///
/// Two halves: which layer is entitled to paint a given arrival, and how many
/// rows one person may occupy in the store behind it.
#[cfg(test)]
#[cfg(feature = "model")]
mod one_person_one_pointer_tests {
    use super::*;
    use crate::collaboration::PresencePayload;

    tilo_test_env::scenario! {
        fn one_person_has_one_pointer_that_moves_between_scopes;

        phase a_pointer_published_without_a_scope_belongs_to_the_desktop;
        // A pointer arriving with no scope of its own is on the *shell*.
        // Reading it as a *widget* scope told every window the pointer was
        // theirs, so one person came out once per open window.
        {
            let mut ctx = PresenceCtx::default();
            ctx.apply_remote_presence(
                MemberId::person("peer@7"),
                PresenceKey::pointer_default(),
                PresencePayload::Pointer { x: 319.0, y: 484.0 },
            );

            assert!(
                !ctx.remote_cursors[0].painted_by_window(),
                "an unscoped pointer is the compositor's to paint; a window \
             painting it as well is the same person once per window",
            );
        }

        phase a_workspace_scoped_pointer_still_belongs_to_the_desktop;
        // The workspace scope keeps the answer it always had, so the rule above cannot
        // be satisfied by making *nothing* the window's to paint.
        {
            let mut ctx = PresenceCtx::default();
            ctx.apply_remote_presence(
                MemberId::person("peer@7"),
                PresenceKey::Workspace {
                    id: "workspace".into(),
                },
                PresencePayload::Pointer { x: 10.0, y: 20.0 },
            );
            assert!(!ctx.remote_cursors[0].painted_by_window());
        }

        phase a_pointer_inside_a_window_is_still_the_windows_to_paint;
        // A pointer inside a window's content is still that window's
        // to paint — the relay dialect and hosted backend have no overlay above
        // them, so swallowing every scope would leave those peers invisible.
        {
            let mut ctx = PresenceCtx::default();
            ctx.apply_remote_presence(
                MemberId::person("peer@7"),
                PresenceKey::Widget {
                    path: vec![PathSegment::Index(3)],
                },
                PresencePayload::Pointer { x: 10.0, y: 20.0 },
            );
            assert!(
                ctx.remote_cursors[0].painted_by_window(),
                "nothing else paints this one",
            );
        }

        phase a_persons_pointer_moves_between_scopes_rather_than_multiplying;
        // A person's pointer *moves*; it does not accumulate — a peer heard
        // from under two scopes must produce one row, not two, or anything
        // reading across scopes sees them twice.
        {
            let mut ctx = PresenceCtx::default();
            let who = MemberId::person("peer@7");
            let first = PresenceKey::Workspace {
                id: "workspace".into(),
            };
            let second = PresenceKey::pointer_default();

            ctx.set_pointer(&first, who.clone(), (100.0, 200.0));
            ctx.set_pointer(&second, who.clone(), (640.0, 480.0));

            let rows: Vec<_> = ctx
                .pointers
                .iter()
                .flat_map(|(k, v)| v.iter().map(move |(u, p)| (k.clone(), u.clone(), *p)))
                .collect();
            assert_eq!(rows.len(), 1, "one person is one pointer, got {rows:?}");
            assert_eq!(rows[0].1, who);
            assert_eq!(rows[0].2, (640.0, 480.0), "and it is where they are now");
            assert_eq!(
                ctx.pointer_for(&first, &who),
                None,
                "the scope they left must not keep a copy",
            );
        }

        phase moving_one_persons_pointer_leaves_everybody_elses_alone;
        // Moving one person's pointer must not take anybody else's with it.
        {
            let mut ctx = PresenceCtx::default();
            let workspace = PresenceKey::Workspace {
                id: "workspace".into(),
            };
            let alice = MemberId::person("peer@1");
            let bob = MemberId::person("peer@2");

            ctx.set_pointer(&workspace, alice.clone(), (10.0, 10.0));
            ctx.set_pointer(&workspace, bob.clone(), (20.0, 20.0));
            ctx.set_pointer(&PresenceKey::pointer_default(), alice.clone(), (30.0, 30.0));

            assert_eq!(ctx.pointer_for(&workspace, &bob), Some((20.0, 20.0)));
            assert_eq!(
                ctx.pointer_for(&PresenceKey::pointer_default(), &alice),
                Some((30.0, 30.0))
            );
            assert_eq!(ctx.pointer_for(&workspace, &alice), None);
        }
    }
}

#[cfg(test)]
#[path = "render_label_tests.rs"]
mod render_label_tests;
