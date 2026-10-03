use crate::permissions::UserId;
use crate::time::{Duration, Instant, SystemTime};
use std::collections::HashMap;

pub mod capability;
mod geom;
mod interaction;
mod key;
mod render;
pub use geom::{EdgeMist, MistEdge, edge_mist_for};
pub use interaction::{
    ACTIVE_INTERACTION_EXPIRY, InteractionCardPlacement, TERMINAL_INTERACTION_DWELL,
};
pub use key::{DIGEST_KEY_FIELD, PathSegment, PointerSpace, PresenceKey, SelectionRange};
/// Shared with the other widgets that frame a label in fixed-pixel insets
/// (the capture studio's docked recorder), so one rule decides how much
/// larger than the default a label paints.
pub(crate) use render::label_magnification;
pub use render::{
    PeerCursor, PillPlacement, PillRequest, contrast_ink, draw_peer_cursors, draw_peer_presence,
    draw_remote_cursors, draw_remote_presence, layout_peer_pills,
};

/// How long a "just typed" highlight lingers before fading out.
pub const TYPING_FADE: crate::time::Duration = crate::time::Duration::from_millis(2500);

/// Longest forward caret jump still read as "this peer just wrote that"
/// (see the `Caret` arm of [`PresenceCtx::apply_remote_presence`]).
/// Generous on purpose: a caret key is per *block*, so a stray mark is
/// bounded to one paragraph, while too tight a cap would drop the marker
/// on the edit most worth seeing (an agent replacing a line with a sentence).
const TYPING_DERIVE_MAX_RUN: usize = 512;

/// How long after a remote text edit an advancing caret may still be read as
/// that peer having written what it crossed — one network hop, no more, or a
/// real edit would license markers for every caret that moves afterward.
const EDIT_PULSE_WINDOW: crate::time::Duration = crate::time::Duration::from_millis(1500);

/// Stable identity colour for a collaborator who has not chosen one: the
/// presence hue [`crate::tokens::PresenceHue::of_identity`] derives from
/// their id, as the sRGB colour in [`PEER_PALETTE`]. A pure function of the
/// id alone: two hosts painting the same room see different peer sets
/// (everyone but themselves), so a colour derived from who's present
/// couldn't agree across machines.
///
/// This is the identity as it travels; paint it through
/// `theme.tokens.colors.presence.of_rgb(..)`, which carries the hue at the
/// active theme's presence lightness and chroma.
pub fn user_color(user_id: &str) -> (u8, u8, u8) {
    identity_rgb(crate::tokens::PresenceHue::of_identity(user_id))
}

/// A presence hue as the identity colour it travels as.
pub fn identity_rgb(hue: crate::tokens::PresenceHue) -> (u8, u8, u8) {
    PEER_PALETTE.get(hue.slot()).copied().unwrap_or_default()
}

/// Every colour a peer can be drawn in: the design language's twelve
/// presence hues (`tokens::colors::PRESENCE_HUES`, 30° apart) at the dark
/// material's presence lightness and chroma (OKLCH L 0.74, C 0.14, gamut
/// mapped), as sRGB. Equal lightness and chroma, so no participant looks
/// more important than another; a closed ring rather than a hash over all
/// 360 hues, so two people never land a few degrees apart.
pub const PEER_PALETTE: [(u8, u8, u8); 12] = [
    (0xf7, 0x85, 0x7d), // 25°  red
    (0xed, 0x91, 0x4c), // 55°  orange — pinned for "bob"
    (0xd3, 0xa3, 0x29), // 85°  amber
    (0xa8, 0xb5, 0x40), // 115° olive
    (0x6e, 0xc2, 0x72), // 145° green — pinned for "charlie"
    (0x00, 0xc7, 0xa6), // 175° teal
    (0x00, 0xc1, 0xd1), // 205° cyan
    (0x38, 0xb8, 0xf6), // 235° sky
    (0x81, 0xa8, 0xff), // 265° blue — pinned for "me"
    (0xb2, 0x98, 0xf9), // 295° violet
    (0xd7, 0x8a, 0xdb), // 325° magenta
    (0xee, 0x83, 0xaf), // 355° pink — pinned for "alice"
];

/// Give everyone in a room a colour nobody else in it is wearing.
///
/// [`user_color`] alone can't promise this — it's a hash, so `carol` lands
/// on `alice`'s pink. Whoever collides steps to the next free ring entry,
/// walking ids in sorted order so every host agrees despite seeing a
/// different membership (everyone but itself). `reserved` colours are
/// already spoken for by an explicit roster choice.
pub fn assign_peer_colors(
    ids: &[&str],
    reserved: &[(u8, u8, u8)],
) -> HashMap<String, (u8, u8, u8)> {
    let mut sorted: Vec<&str> = ids.to_vec();
    sorted.sort_unstable();
    sorted.dedup();

    let mut taken = [false; PEER_PALETTE.len()];
    for c in reserved {
        if let Some(i) = PEER_PALETTE.iter().position(|p| p == c)
            && let Some(slot) = taken.get_mut(i)
        {
            *slot = true;
        }
    }

    let mut out = HashMap::with_capacity(sorted.len());
    for id in sorted {
        let natural = user_color(id);
        let start = PEER_PALETTE
            .iter()
            .position(|p| *p == natural)
            // Always a ring member; 0 just keeps a draw path total rather than panicking.
            .unwrap_or(0);
        let mut slot = start;
        for step in 0..PEER_PALETTE.len() {
            // `PEER_PALETTE.len()` (24) is a nonzero compile-time constant,
            // so the remainder can never divide by zero; `start`/`step` are
            // both < 24, so the sum can't realistically overflow either.
            let i = start
                .saturating_add(step)
                .checked_rem(PEER_PALETTE.len())
                .unwrap_or(0);
            if !taken.get(i).copied().unwrap_or(false) {
                slot = i;
                break;
            }
            // More people than colours: share the natural entry; the name still says who is who.
            slot = start;
        }
        if let Some(s) = taken.get_mut(slot) {
            *s = true;
        }
        out.insert(
            id.to_string(),
            PEER_PALETTE.get(slot).copied().unwrap_or_default(),
        );
    }
    out
}

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

    #[test]
    fn user_colours_are_pinned_or_hashed_onto_a_distinct_ring() {
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

    #[test]
    fn a_room_assigns_distinct_agreed_stable_colours() {
        // phase: a_room_never_puts_two_people_in_one_colour
        // The case the ring alone can't fix: `judy` hashes onto exactly the
        // pink `alice` is pinned to, so one of them has to move.
        {
            assert_eq!(
                user_color("judy"),
                user_color("alice"),
                "this test is guarding a real collision; if the hash moved, \
             pick another colliding pair rather than deleting the test",
            );

            let room = ["alice", "bob", "judy", "danah", "erin", "dana"];
            let assigned = assign_peer_colors(&room, &[]);
            assert_eq!(assigned.len(), room.len());
            for (i, a) in room.iter().enumerate() {
                for b in &room[i + 1..] {
                    assert_ne!(
                        assigned[*a], assigned[*b],
                        "{a} and {b} were given the same colour",
                    );
                    assert!(
                        color_distance(assigned[*a], assigned[*b]) >= 60.0,
                        "{a} and {b} are too close to tell apart",
                    );
                }
            }
            assert_eq!(assigned["alice"], user_color("alice"));
            assert_eq!(assigned["bob"], user_color("bob"));
        }
        // phase: every_machine_in_a_room_agrees_on_who_is_what_colour
        {
            let room = ["alice", "bob", "judy"];
            for me in room {
                let mut mine: Vec<&str> = room.iter().copied().filter(|u| *u != me).collect();
                mine.push(me);
                assert_eq!(
                    assign_peer_colors(&mine, &[]),
                    assign_peer_colors(&room, &[]),
                    "the host running as {me} disagrees with the others",
                );
            }
        }
        // phase: an_explicit_roster_colour_is_not_reassigned_to_someone_else
        // A colour the roster handed out explicitly is not available to be
        // computed onto, or an invited peer and a live one could collide.
        {
            let taken = user_color("judy"); // == alice's pink
            let assigned = assign_peer_colors(&["judy"], &[taken]);
            assert_ne!(
                assigned["judy"], taken,
                "judy took a colour the roster had already spoken for",
            );
        }
        // phase: a_late_arrival_leaves_the_others_alone
        // Someone arriving late must not repaint the people already there
        // unless they actually collide with one.
        {
            let before = assign_peer_colors(&["alice", "bob"], &[]);
            let after = assign_peer_colors(&["alice", "bob", "dana"], &[]);
            assert_eq!(before["alice"], after["alice"]);
            assert_eq!(before["bob"], after["bob"]);
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PresenceUser {
    pub id: String,
    pub name: String,
    pub color: (u8, u8, u8),
    /// Principal kind supplied by the collaboration context, never guessed
    /// from the display name.
    pub is_agent: bool,
}

/// The kind of participant behind a live presence stream. This comes from
/// the authenticated room context, never from a display-name convention.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ParticipantKind {
    #[default]
    Person,
    Agent,
}

impl From<bool> for ParticipantKind {
    fn from(is_agent: bool) -> Self {
        if is_agent { Self::Agent } else { Self::Person }
    }
}

impl AsRef<UserId> for UserId {
    fn as_ref(&self) -> &UserId {
        self
    }
}

impl PresenceUser {
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        let id = id.into();
        let color = user_color(&id);
        Self {
            id,
            name: name.into(),
            color,
            is_agent: false,
        }
    }

    /// Uses the id itself as the display name until a roster entry supplies a nicer one.
    pub fn from_id(id: impl Into<String>) -> Self {
        let id = id.into();
        let name = id.clone();
        Self::new(id, name)
    }

    pub fn color_rgba(&self, alpha: f32) -> [f32; 4] {
        [
            f32::from(self.color.0) / 255.0,
            f32::from(self.color.1) / 255.0,
            f32::from(self.color.2) / 255.0,
            alpha,
        ]
    }
}

/// One "just typed" highlight span: the run a peer inserted and when.
/// [`PresenceCtx::active_typing`] turns its age into an alpha and prunes it
/// once past [`TYPING_FADE`].
#[derive(Debug, Clone)]
struct TypingMark {
    range: SelectionRange,
    born: Instant,
}

/// Which layer paints a peer's pointer — a **screen** position, so only one
/// layer may place it correctly (an app painting a desktop coordinate into
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
    Desktop,
    /// In some other surface's content coordinates — the frame arrived (the
    /// broker fans presence to every app in the room) but is not this
    /// process's to place. Makes the fan-out safe: exactly one of N apps
    /// answers "mine", the rest land here without consulting each other.
    OtherSurface,
}

#[derive(Debug, Clone)]
pub struct RemoteCursor {
    pub user_id: String,
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

/// How long a quiet peer's pointer takes to fade out, ending at
/// [`POINTER_IDLE`]. See [`PresenceCtx::pointer_opacity`].
pub const POINTER_FADE: Duration = Duration::from_millis(400);

/// A pointer's last-known `(x, y)` per user, at each [`PresenceKey`].
pub type PointerPositions = HashMap<PresenceKey, Vec<(UserId, (f32, f32))>>;

#[derive(Debug, Default, Clone)]
pub struct PresenceCtx {
    pub here: HashMap<PresenceKey, Vec<UserId>>,
    pub pointers: PointerPositions,
    /// When each pointer in [`Self::pointers`] was last heard about, so
    /// [`Self::prune_pointers`] can retire one gone quiet — without it a
    /// crashed peer's cursor stays painted for the rest of the session.
    pointer_seen: HashMap<(PresenceKey, UserId), Instant>,
    pub selections: HashMap<PresenceKey, Vec<(UserId, SelectionRange)>>,
    pub carets: HashMap<PresenceKey, Vec<(UserId, usize)>>,
    /// Ephemeral visual relationships, keyed by `(room, follower)`. They
    /// never affect document state or anyone's local viewport.
    following: HashMap<(PresenceKey, UserId), UserId>,
    pub remote_cursors: Vec<RemoteCursor>,
    /// Seeded by the shell from `Collaborator::cursor_color`; falls
    /// through to [`user_color`] via [`PresenceCtx::color_for`] when unset.
    pub color_overrides: HashMap<UserId, (u8, u8, u8)>,
    /// Friendly display names per user, seeded from the collab roster
    /// (`CollabEvent::Context`). A peer with no entry falls back to its raw
    /// id via [`PresenceCtx::display_name`], so a caret chip is never blank.
    pub names: HashMap<UserId, String>,
    /// Authenticated participant kind from `CollabEvent::Context`. Renderers
    /// use this in addition to colour so an agent cannot be mistaken for a
    /// person when names are hidden, elided, or inaccessible.
    participant_kinds: HashMap<UserId, ParticipantKind>,
    /// Short room-level intentions, separate from pointer/caret state. The
    /// origin remains visible so inferred prose is never presented as a quote.
    statuses: HashMap<UserId, crate::live_collaboration::LiveStatus>,
    /// Broker-authenticated semantic activity, keyed by invocation. Unlike
    /// pointer/caret presence this never pretends an action caller has an
    /// input device: applications may project its stable resource anchor to
    /// native geometry, while the generic painter shows a compact fallback.
    interactions: HashMap<tilo_actions::InvocationId, interaction::InteractionPresenceEntry>,
    /// Per-user "just typed" highlight spans, keyed by presence scope and
    /// faded out by [`PresenceCtx::active_typing`] over [`TYPING_FADE`].
    /// Not serialised — purely a local render effect.
    typing: HashMap<(UserId, PresenceKey), Vec<TypingMark>>,
    /// When a remote *document* change (not just a caret move) was last
    /// applied, fed by [`Self::note_remote_text_edit`]; the `Caret` arm of
    /// [`Self::apply_remote_presence`] only paints a typing mark when this
    /// is also recent.
    last_remote_text_edit: Option<Instant>,
    /// Draw-time visibility window in scene-space pixels, for spatial culling.
    /// `Cell` so containers (Scrollable) can scope-push a tighter inner
    /// viewport without needing `&mut PresenceCtx` everywhere.
    pub viewport: std::cell::Cell<Option<vello::kurbo::Rect>>,
    /// Each peer's last broadcast [`ViewSubset`](crate::view::ViewSubset) —
    /// where they're looking, distinct from their cursor. Read by
    /// **attach/follow** to bind the local viewport to a chosen peer.
    views: HashMap<UserId, crate::view::ViewSubset>,
    /// Identities this session publishes its own presence under — what a
    /// frame it sent comes back stamped with. Empty means "unknown", which
    /// suppresses nobody. See [`Self::set_own_identity`].
    own: std::collections::HashSet<UserId>,
    /// Which participant in the room this session *is* — the `is_self` row
    /// of `CollabEvent::Context`. Deliberately not in [`Self::names`]/
    /// [`Self::here`]/the pointer-caret stores, which describe *other*
    /// people (a row for yourself there is a pill over your own document).
    /// `None` until the room has said.
    self_participant: Option<(UserId, String)>,
    /// The surface this process draws, spelled as [`crate::local_surface_id`]
    /// spells it, compared against a pointer's claimed surface to decide
    /// whether a content-space position is this process's to place. `None`
    /// fails closed — a pointer in the wrong window should stay invisible,
    /// not get placed wrong.
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

    /// Replace the active viewport, returning the previous value so the caller can restore it.
    pub fn push_viewport(&self, vp: vello::kurbo::Rect) -> Option<vello::kurbo::Rect> {
        self.viewport.replace(Some(vp))
    }

    pub fn pop_viewport(&self, prev: Option<vello::kurbo::Rect>) {
        self.viewport.set(prev);
    }
}

impl PresenceCtx {
    pub fn empty() -> Self {
        Self::default()
    }

    /// Declare the identities a frame *this* session published comes back
    /// stamped with, so its own presence is never painted as somebody else's.
    ///
    /// Discriminator is the participant, not the process or app: two windows
    /// of one app are one person, but an agent editing the same document
    /// joins under its own identity and stays a peer. Blank entries are
    /// dropped — an unset forwarded env var produces `""`, and taking it
    /// literally would make every unnamed participant "me".
    pub fn set_own_identity<I, S>(&mut self, ids: I)
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.own = ids
            .into_iter()
            .filter(|s| !s.as_ref().trim().is_empty())
            .map(|s| UserId(s.as_ref().to_string()))
            .collect();
    }

    /// Add one more spelling of "me" without disturbing the others —
    /// [`Self::set_own_identity`] replaces its whole set instead. Returns
    /// whether anything was new. Blank is ignored, as in `set_own_identity`.
    pub fn add_own_identity(&mut self, id: impl AsRef<str>) -> bool {
        let id = id.as_ref().trim();
        if id.is_empty() {
            return false;
        }
        self.own.insert(UserId(id.to_string()))
    }

    /// Set from [`crate::local_surface_id`] — the same value the publishing
    /// side scopes its pointer with, so the two spellings cannot drift.
    pub fn set_surface_id(&mut self, surface: impl Into<String>) {
        let surface = surface.into();
        self.surface = (!surface.trim().is_empty()).then_some(surface);
    }

    /// A pointer is in exactly one space, so a frame fanned to every app in
    /// a room still has at most one willing to draw it.
    pub fn pointer_owner_for(&self, key: &PresenceKey) -> PointerOwner {
        match key.pointer_space() {
            None => PointerOwner::Window,
            Some(PointerSpace::Desktop { .. } | PointerSpace::Tile { .. }) => PointerOwner::Desktop,
            Some(PointerSpace::Surface { surface }) => match self.surface.as_deref() {
                Some(mine) if mine == surface => PointerOwner::Window,
                _ => PointerOwner::OtherSurface,
            },
        }
    }

    /// Fed only from the `is_self` row of `CollabEvent::Context` — a
    /// machine cannot mint this for itself. Blank id is dropped, as in
    /// `set_own_identity`.
    pub fn set_self_participant(&mut self, id: UserId, name: impl Into<String>) {
        if id.0.trim().is_empty() {
            return;
        }
        self.self_participant = Some((id, name.into()));
    }

    /// Which participant in the room this session is, if the room has said.
    pub fn self_id(&self) -> Option<&UserId> {
        self.self_participant.as_ref().map(|(id, _)| id)
    }

    /// What the room calls this session's own participant. `None` for a
    /// participant the room knows only as an address.
    pub fn self_name(&self) -> Option<&str> {
        self.self_participant
            .as_ref()
            .map(|(_, n)| n.trim())
            .filter(|n| !n.is_empty())
    }

    /// Whether `user_id` is this session's own participant. Checks
    /// `self_participant` before the declared list (which
    /// [`Self::set_own_identity`] replaces wholesale), and also matches the
    /// room's *name* for this session — a locally-sent frame is stamped
    /// `bob`, the announced row is `ada`; safe because a remote id is
    /// always `peer@<handle>`, never a bare name.
    pub fn is_own_identity(&self, user_id: &str) -> bool {
        if user_id.trim().is_empty() {
            return false;
        }
        if self
            .self_participant
            .as_ref()
            .is_some_and(|(id, name)| id.0 == user_id || name.trim() == user_id.trim())
        {
            return true;
        }
        if self.own.is_empty() {
            return false;
        }
        self.own.contains(&UserId(user_id.to_string()))
    }

    pub fn register_presence(&mut self, key: &PresenceKey, user: UserId) {
        let list = self.here.entry(key.clone()).or_default();
        if !list.iter().any(|u| u == &user) {
            list.push(user);
        }
    }

    pub fn remove_presence(&mut self, key: &PresenceKey, user: &UserId) {
        if let Some(list) = self.here.get_mut(key) {
            list.retain(|u| u != user);
        }
    }

    /// Re-attribute `user` to exactly one `PresenceKey::Tab` at a time, so a
    /// peer switching tabs moves their dot rather than leaving a trail. No-op
    /// (falls back to `register_presence`) if `target` isn't a `Tab` key.
    pub fn set_tab_focus(&mut self, target: &PresenceKey, user: UserId) {
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

    pub fn set_pointer(&mut self, key: &PresenceKey, user: UserId, pos: (f32, f32)) {
        self.set_pointer_at(key, user, pos, crate::time::now());
    }

    /// [`Self::set_pointer`] with the clock supplied, so retirement can be
    /// exercised without sleeping. Records the peer as *present* as well as
    /// pointing — deliberately a separate record, since the pointer expires
    /// but being here does not. Single-location like `set_caret`/
    /// `set_selection`: a person has one pointer, so this moves it rather
    /// than adding a new entry under a different scope.
    pub fn set_pointer_at(
        &mut self,
        key: &PresenceKey,
        user: UserId,
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

        let list = self.pointers.entry(key.clone()).or_default();
        if let Some(entry) = list.iter_mut().find(|(u, _)| *u == user) {
            entry.1 = pos;
        } else {
            list.push((user.clone(), pos));
        }
        self.pointer_seen.insert((key.clone(), user.clone()), now);
        // Desktop scopes only: a pointer inside a surface's content is a
        // position in a document, not a presence dot on the desktop. (Not
        // `scopes_a_desktop_pointer`, which also answers yes for a routing
        // artefact that is nobody's presence.)
        if matches!(
            key,
            PresenceKey::Workspace { .. }
                | PresenceKey::Pointer {
                    space: PointerSpace::Desktop { .. }
                }
        ) {
            self.register_presence(key, user);
        }
    }

    /// Retire every pointer nobody has restated within `horizon`. Returns
    /// whether anything was retired — a reason to repaint. `horizon` must
    /// comfortably exceed the ~1s restate interval, so this notices a peer
    /// gone quiet rather than blinking one whose keepalive was briefly
    /// delayed. Only the pointer goes — the peer stays in [`Self::here`],
    /// since going quiet about pointing is not leaving.
    pub fn prune_pointers(&mut self, now: Instant, horizon: Duration) -> bool {
        let seen = &mut self.pointer_seen;
        let mut retired = false;
        self.pointers.retain(|key, list| {
            list.retain(|(user, _)| {
                let fresh = seen
                    .get(&(key.clone(), user.clone()))
                    .is_some_and(|t| now.saturating_duration_since(*t) < horizon);
                if !fresh {
                    retired = true;
                }
                fresh
            });
            !list.is_empty()
        });
        if retired {
            seen.retain(|_, t| now.saturating_duration_since(*t) < horizon);
        }
        retired
    }

    /// How opaque to paint `user`'s pointer at `key`: fully until the last
    /// `fade` before it goes quiet for `horizon`, then falling to zero as
    /// [`Self::prune_pointers`] is about to retire it. A zero `fade` (reduced
    /// motion) keeps it opaque until it is retired.
    pub fn pointer_opacity(
        &self,
        key: &PresenceKey,
        user: &UserId,
        now: Instant,
        horizon: Duration,
        fade: Duration,
    ) -> f32 {
        let Some(seen) = self.pointer_seen.get(&(key.clone(), user.clone())) else {
            return 1.0;
        };
        let left = horizon.saturating_sub(now.saturating_duration_since(*seen));
        if fade.is_zero() || left >= fade {
            return 1.0;
        }
        (left.as_secs_f32() / fade.as_secs_f32()).clamp(0.0, 1.0)
    }

    /// Whether any pointer is fading out at `now` (see
    /// [`Self::pointer_opacity`]) — a reason to paint the next frame too.
    pub fn pointers_fading(&self, now: Instant, horizon: Duration, fade: Duration) -> bool {
        self.pointer_seen.values().any(|seen| {
            let age = now.saturating_duration_since(*seen);
            age < horizon && horizon.saturating_sub(age) < fade
        })
    }

    pub fn set_caret(&mut self, key: &PresenceKey, user: UserId, pos: usize) {
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

    pub fn set_selection(&mut self, key: &PresenceKey, user: UserId, range: SelectionRange) {
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

    pub fn peers_on(&self, key: &PresenceKey) -> &[UserId] {
        self.here.get(key).map_or(&[], Vec::as_slice)
    }

    pub fn pointer_for(&self, key: &PresenceKey, user: &UserId) -> Option<(f32, f32)> {
        self.pointers
            .get(key)?
            .iter()
            .find(|(u, _)| u == user)
            .map(|(_, p)| *p)
    }

    pub fn selection_for(&self, key: &PresenceKey, user: &UserId) -> Option<SelectionRange> {
        self.selections
            .get(key)?
            .iter()
            .find(|(u, _)| u == user)
            .map(|(_, r)| *r)
    }

    pub fn caret_for(&self, key: &PresenceKey, user: &UserId) -> Option<usize> {
        self.carets
            .get(key)?
            .iter()
            .find(|(u, _)| u == user)
            .map(|(_, c)| *c)
    }

    /// Seed or replace this user's colour override (see [`parse_hex_color`]
    /// for the accepted formats). Malformed input silently clears the
    /// override, falling back to [`user_color`].
    pub fn set_color_override(&mut self, user: UserId, hex: &str) {
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
    pub fn set_presence_hue(&mut self, user: UserId, hue: crate::tokens::PresenceHue) {
        let rgb = identity_rgb(hue);
        if self.color_overrides.get(&user) == Some(&rgb) {
            return;
        }
        for cursor in &mut self.remote_cursors {
            if cursor.user_id == user.0 {
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

    pub fn color_for(&self, user: &UserId) -> (u8, u8, u8) {
        if let Some(rgb) = self.color_overrides.get(user) {
            return *rgb;
        }
        user_color(&user.0)
    }

    /// Seeded from the collab roster. Overwrites any prior name.
    pub fn set_display_name(&mut self, user: UserId, name: impl Into<String>) {
        let name = name.into();
        let shown = if self.is_agent(&user) {
            crate::history_source::agent_display_label(&name)
        } else {
            name.clone()
        };
        // Relabel any already-drawn pill too, in case the pointer arrived first.
        for cursor in &mut self.remote_cursors {
            if cursor.user_id == user.0 {
                cursor.label.clone_from(&shown);
            }
        }
        self.names.insert(user, name);
    }

    /// Seed the authenticated participant kind announced by the room. Both
    /// the legacy boolean and the typed enum map to the same authenticated
    /// kind, so changing names never changes a participant's principal kind.
    pub fn set_participant_kind(
        &mut self,
        user: impl AsRef<UserId>,
        kind: impl Into<ParticipantKind>,
    ) {
        let user = user.as_ref().clone();
        self.participant_kinds.insert(user.clone(), kind.into());
        let shown = self.display_name(&user);
        for cursor in &mut self.remote_cursors {
            if cursor.user_id == user.0 {
                cursor.label.clone_from(&shown);
            }
        }
    }

    pub fn participant_kind(&self, user: &UserId) -> ParticipantKind {
        self.participant_kinds
            .get(user)
            .copied()
            .unwrap_or_default()
    }

    pub fn is_agent(&self, user: &UserId) -> bool {
        self.participant_kind(user) == ParticipantKind::Agent
    }

    /// The friendly name to show for `user`, falling back to the raw id
    /// when the roster hasn't announced one. Never blank.
    pub fn display_name(&self, user: &UserId) -> String {
        let name = self
            .names
            .get(user)
            .cloned()
            .unwrap_or_else(|| user.0.clone());
        if self.is_agent(user) {
            crate::history_source::agent_display_label(&name)
        } else {
            name
        }
    }

    pub fn user(&self, user: &UserId) -> PresenceUser {
        PresenceUser {
            id: user.0.clone(),
            name: self.display_name(user),
            color: self.color_for(user),
            is_agent: self.is_agent(user),
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
        user: UserId,
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
    /// tapering linearly to 0 at [`TYPING_FADE`]). This is a read only:
    /// call [`Self::prune_typing`] separately to reclaim expired entries.
    pub fn active_typing(
        &self,
        user: &UserId,
        key: &PresenceKey,
        now: Instant,
    ) -> Vec<(SelectionRange, f32)> {
        let fade = TYPING_FADE.as_secs_f32();
        let Some(marks) = self.typing.get(&(user.clone(), key.clone())) else {
            return Vec::new();
        };
        marks
            .iter()
            .filter_map(|m| {
                let age = now.saturating_duration_since(m.born).as_secs_f32();
                if age >= fade {
                    return None;
                }
                let alpha = (1.0 - age / fade).clamp(0.0, 1.0);
                Some((m.range, alpha))
            })
            .collect()
    }

    /// Every peer's typing spans under `key` at `now`, as `(user, range, alpha)`.
    pub fn typing_marks_for_key(
        &self,
        key: &PresenceKey,
        now: Instant,
    ) -> Vec<(UserId, SelectionRange, f32)> {
        let fade = TYPING_FADE.as_secs_f32();
        let mut out = Vec::new();
        for ((user, k), marks) in &self.typing {
            if k != key {
                continue;
            }
            for m in marks {
                let age = now.saturating_duration_since(m.born).as_secs_f32();
                if age >= fade {
                    continue;
                }
                let alpha = (1.0 - age / fade).clamp(0.0, 1.0);
                out.push((user.clone(), m.range, alpha));
            }
        }
        out
    }

    /// The App polls this to keep requesting animation frames while a highlighter is still decaying.
    pub fn has_active_typing(&self, now: Instant) -> bool {
        let fade = TYPING_FADE;
        self.typing
            .values()
            .flatten()
            .any(|m| now.saturating_duration_since(m.born) < fade)
    }

    /// Call once a frame.
    pub fn prune_typing(&mut self, now: Instant) {
        let fade = TYPING_FADE;
        self.typing.retain(|_, marks| {
            marks.retain(|m| now.saturating_duration_since(m.born) < fade);
            !marks.is_empty()
        });
    }

    /// Never merged, only replaced, so a peer's entry is always exactly what they last published.
    pub fn set_peer_view(&mut self, user: UserId, view: crate::view::ViewSubset) {
        self.views.insert(user, view);
    }

    /// What **attach/follow** binds the local viewport to.
    pub fn peer_view(&self, user: &UserId) -> Option<&crate::view::ViewSubset> {
        self.views.get(user)
    }

    /// The target a "follow" action scrolls toward — reads the same
    /// `remote_cursors` overlay the pointer is drawn from, so following a
    /// peer lands exactly where their cursor shows.
    pub fn peer_pointer(&self, user_id: &str) -> Option<[f32; 2]> {
        self.remote_cursors
            .iter()
            .find(|c| c.user_id == user_id)
            .map(|c| c.position)
    }

    /// Every live `(follower, target)` relationship announced in `key`.
    /// This is deliberately a render-only projection of transient presence.
    pub fn following_in(&self, key: &PresenceKey) -> Vec<(&UserId, &UserId)> {
        self.following
            .iter()
            .filter_map(|((scope, follower), target)| (scope == key).then_some((follower, target)))
            .collect()
    }

    /// Returns the id of the peer whose off-screen mist band (the strips
    /// [`draw_remote_presence`] paints) contains `point`, so clicking a
    /// peer's mist selects them to follow.
    pub fn hit_test_offscreen_mist(
        &self,
        viewport: vello::kurbo::Rect,
        point: vello::kurbo::Point,
    ) -> Option<String> {
        for cursor in &self.remote_cursors {
            // Exactly the peers draw_remote_presence paints a band for: never itself.
            if self.is_own_identity(&cursor.user_id) || !cursor.painted_by_window() {
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
                return Some(cursor.user_id.clone());
            }
        }
        None
    }

    /// Upsert a remote peer's cursor into `remote_cursors`. Label defaults to
    /// `user_id` when `display_name` is `None`.
    pub fn upsert_remote_cursor(
        &mut self,
        user_id: String,
        position: [f32; 2],
        display_name: Option<&str>,
    ) {
        self.upsert_remote_cursor_faded(user_id, position, display_name, 1.0);
    }

    /// [`Self::upsert_remote_cursor`] with an explicit opacity, so a caller
    /// that ages cursors out can fade one rather than popping it. Clamped to `0.0..=1.0`.
    pub fn upsert_remote_cursor_faded(
        &mut self,
        user_id: String,
        position: [f32; 2],
        display_name: Option<&str>,
        alpha: f32,
    ) {
        self.upsert_remote_cursor_owned(
            user_id,
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
        user_id: String,
        position: [f32; 2],
        display_name: Option<&str>,
        alpha: f32,
        owner: PointerOwner,
    ) {
        // Second door into the same pill (`RemoteCursorMoved` bypasses
        // `apply_remote_presence`); guarding only one door leaves the bug reachable.
        if self.is_own_identity(&user_id) {
            return;
        }
        let uid = UserId(user_id.clone());
        let (r, g, b) = self.color_for(&uid);
        let color = [
            f32::from(r) / 255.0,
            f32::from(g) / 255.0,
            f32::from(b) / 255.0,
            alpha.clamp(0.0, 1.0),
        ];
        let label =
            display_name.map_or_else(|| self.display_name(&uid), std::string::ToString::to_string);
        if let Some(existing) = self
            .remote_cursors
            .iter_mut()
            .find(|c| c.user_id == user_id)
        {
            existing.position = position;
            existing.color = color;
            existing.owner = owner;
            // Don't overwrite an existing friendly label with the raw id.
            if display_name.is_some() {
                existing.label = label;
            }
        } else {
            self.remote_cursors.push(RemoteCursor {
                user_id,
                color,
                position,
                label,
                owner,
            });
        }
    }

    pub fn remove_remote_cursor(&mut self, user_id: &str) {
        self.remote_cursors.retain(|c| c.user_id != user_id);
    }

    // String-id wrappers for the App's collab-event pump: the wire protocol
    // carries peer ids as `String`, the rest of `PresenceCtx` is keyed by
    // [`UserId`]. Let the pump forward straight from the event payload.

    // `key` is only ever borrowed inside these three wrappers, so clippy
    // would rather see `&PresenceKey` — but they're the public wire-decode
    // entrypoints called from `shell::peer_presence` and app crates
    // (`apps/notes`, `apps/word-processor`) outside this file's ownership,
    // each handing over an owned `PresenceKey` it just decoded; narrowing
    // the parameter would ripple into call sites this pass can't touch.
    #[allow(
        clippy::needless_pass_by_value,
        reason = "public wire-decode API called from crates outside ui; narrowing to &PresenceKey would require updating call sites out of scope for this pass"
    )]
    pub fn set_remote_caret(&mut self, user_id: String, key: PresenceKey, pos: usize) {
        self.set_caret(&key, UserId(user_id), pos);
    }

    #[allow(
        clippy::needless_pass_by_value,
        reason = "public wire-decode API called from crates outside ui; narrowing to &PresenceKey would require updating call sites out of scope for this pass"
    )]
    pub fn set_remote_selection(
        &mut self,
        user_id: String,
        key: PresenceKey,
        start: usize,
        end: usize,
    ) {
        self.set_selection(&key, UserId(user_id), SelectionRange { start, end });
    }

    #[allow(
        clippy::needless_pass_by_value,
        reason = "public wire-decode API called from crates outside ui; narrowing to &PresenceKey would require updating call sites out of scope for this pass"
    )]
    pub fn register_peer_at(&mut self, user_id: String, key: PresenceKey) {
        let uid = UserId(user_id);
        if matches!(key, PresenceKey::Tab { .. }) {
            self.set_tab_focus(&key, uid);
        } else {
            self.register_presence(&key, uid);
        }
    }

    /// Fold one inbound presence frame into this context — the whole decode
    /// of a `RemotePresenceUpdate`. Lives on `PresenceCtx` rather than `App`
    /// because the compositor also subscribes to the identical stream, and
    /// two decoders would drift. The returned [`RemotePresenceEffect`] names
    /// what a bare presence store can't do for itself.
    // `payload`'s variants are all Copy fields, but `PresencePayload` itself
    // isn't, so clippy would rather see `&PresencePayload` here — this is
    // the same public wire-decode entrypoint called from
    // `shell::peer_presence` and app crates outside this file's ownership,
    // so narrowing it would ripple into call sites out of scope for this
    // pass.
    #[allow(
        clippy::needless_pass_by_value,
        clippy::too_many_lines,
        reason = "public wire-decode API called from crates outside ui; narrowing payload to &PresencePayload would require updating call sites out of scope for this pass"
    )]
    pub fn apply_remote_presence(
        &mut self,
        user_id: String,
        key: PresenceKey,
        payload: crate::collaboration::PresencePayload,
    ) -> RemotePresenceEffect {
        use crate::collaboration::PresencePayload::{
            Arrived, Caret, Following, Interaction, Left, Pointer, Selection, Status, Typing,
        };

        // A frame this session published, handed back: another app the same
        // person has open. Nothing about it is painted, but `Arrived` still
        // needs answering — it's a joiner's request for the document.
        if self.is_own_identity(&user_id) {
            // Following yourself is not a thing.
            if key.as_view().is_some() {
                return RemotePresenceEffect::None;
            }
            return match payload {
                // A Tab-keyed arrival moves a focus dot; only a scope
                // arrival affects participant-level presence.
                Arrived if !matches!(key, PresenceKey::Tab { .. }) => {
                    tracing::debug!(
                        target: "tilo.state",
                        event = "presence_self_filtered",
                        %user_id,
                        "ignored own participant in peer rendering"
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
            self.set_peer_view(UserId(user_id), subset);
            return RemotePresenceEffect::PeerViewChanged;
        }

        match payload {
            Pointer { x, y } => {
                // Populate BOTH stores: `pointers` (scope-keyed, read by the
                // shell overlay and `pointer_for`) and screen-space
                // `remote_cursors`. Which gets *painted* is `owner`, decided
                // by the frame's coordinate space — see `pointer_owner_for`.
                let uid = UserId(user_id.clone());
                let owner = self.pointer_owner_for(&key);
                self.set_pointer(&key, uid, (x, y));
                self.upsert_remote_cursor_owned(user_id, [x, y], None, 1.0, owner);
            }
            Caret { pos } => {
                // Feed BOTH stores: in-app BlockEditor reads `carets`
                // (set_caret); the cross-process overlay reads `remote_caret`.
                let uid = UserId(user_id.clone());
                let now = crate::time::now();
                // Derive a typing mark from a caret that advanced under the
                // same scope, not only from an explicit `Typing` payload —
                // an agent/SDK peer publishing bare caret moves has no
                // other way to show what it wrote. Gated on a document
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
                self.set_remote_caret(user_id, key, pos);
            }
            Selection { start, end } => {
                let uid = UserId(user_id.clone());
                self.set_selection(&key, uid, SelectionRange { start, end });
                self.set_remote_selection(user_id, key, start, end);
            }
            Typing { start, end } => {
                self.record_peer_typing(
                    UserId(user_id),
                    key,
                    SelectionRange { start, end },
                    crate::time::now(),
                );
            }
            Interaction(update) => {
                // Semantic action presence has its own resource/anchor model.
                // Applying it must never synthesize a pointer, caret, or text
                // selection for an actor that used no such input device. The
                // resource key also selects exactly one app surface: presence
                // is fanned across a room, but an action in Writer must not
                // paint an activity card in Files and every other window.
                let belongs_here = match &key {
                    PresenceKey::Resource { app, id, kind } => {
                        self.surface.as_deref() == Some(app.as_str())
                            && update.resource.id == *id
                            && update.resource.kind == *kind
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
                // Status is room presence, never document/widget presence.
                if matches!(key, PresenceKey::Workspace { .. }) {
                    let uid = UserId(user_id);
                    if text.trim().is_empty() {
                        self.statuses.remove(&uid);
                    } else if text.chars().count() <= crate::MAX_STATUS_CHARS {
                        self.statuses.insert(
                            uid,
                            crate::live_collaboration::LiveStatus {
                                text,
                                origin: if automatic {
                                    crate::live_collaboration::StatusOrigin::Inferred {
                                        generator: "remote-local-model".into(),
                                    }
                                } else {
                                    crate::live_collaboration::StatusOrigin::Manual
                                },
                                updated_at: crate::time::wall(),
                            },
                        );
                    }
                }
            }
            Following { target } => {
                // A follow relationship is only meaningful within a room.
                // Reject narrower scopes rather than letting a document or
                // widget frame imply control of that content.
                if matches!(key, PresenceKey::Workspace { .. }) {
                    let follower = UserId(user_id);
                    if let Some(target) = target.filter(|target| !target.trim().is_empty()) {
                        self.following.insert((key, follower), UserId(target));
                    } else {
                        self.following.remove(&(key, follower));
                    }
                }
            }
            Arrived => {
                let uid = UserId(user_id.clone());
                self.register_peer_at(user_id, key.clone());
                // Tab keys move the peer's dot; non-Tab arrivals are (possibly
                // late) joiners, so the caller should reply with full state.
                if matches!(key, PresenceKey::Tab { .. }) {
                    self.set_tab_focus(&key, uid);
                } else {
                    self.register_presence(&key, uid);
                    return RemotePresenceEffect::PeerArrived;
                }
            }
            Left => {
                // `Left` fires on mouse leaving the window viewport, not the
                // peer leaving — caret/selection must keep rendering. Do not
                // call `unregister_peer` here: that tears down the whole
                // presence record on every mouse-leave. True disconnect
                // arrives via the socket close path instead.
                let uid = UserId(user_id.clone());
                for list in self.pointers.values_mut() {
                    list.retain(|(u, _)| u != &uid);
                }
                self.remove_remote_cursor(&user_id);
                let _ = key;
            }
        }
        RemotePresenceEffect::None
    }

    /// Drop every trace of `user_id`, on an actual disconnect (not `Left`,
    /// which is just the pointer leaving the viewport — see the arm above).
    /// Stop drawing `user_id`'s pointer, wherever it was, without touching
    /// anything else about them: they are still here, just not pointing at
    /// anything this device can show. Returns whether a pointer went.
    pub fn clear_pointer(&mut self, user_id: &str) -> bool {
        let uid = UserId(user_id.to_owned());
        let before = self.pointers.values().map(Vec::len).sum::<usize>();
        for list in self.pointers.values_mut() {
            list.retain(|(u, _)| u != &uid);
        }
        self.pointers.retain(|_, list| !list.is_empty());
        self.pointer_seen.retain(|(_, u), _| u != &uid);
        let cursors = self.remote_cursors.len();
        self.remove_remote_cursor(user_id);
        before != self.pointers.values().map(Vec::len).sum::<usize>()
            || cursors != self.remote_cursors.len()
    }

    pub fn unregister_peer(&mut self, user_id: &str) {
        let uid = UserId(user_id.to_owned());
        self.participant_kinds.remove(&uid);
        for list in self.here.values_mut() {
            list.retain(|u| u != &uid);
        }
        for list in self.pointers.values_mut() {
            list.retain(|(u, _)| u != &uid);
        }
        self.pointer_seen.retain(|(_, u), _| u != &uid);
        for list in self.carets.values_mut() {
            list.retain(|(u, _)| u != &uid);
        }
        for list in self.selections.values_mut() {
            list.retain(|(u, _)| u != &uid);
        }
        self.typing.retain(|(u, _), _| u != &uid);
        self.following
            .retain(|(_, follower), target| follower != &uid && target != &uid);
        self.views.remove(&uid);
        self.names.remove(&uid);
        self.statuses.remove(&uid);
        self.remove_interactions_for_actor(user_id);
        self.remove_remote_cursor(user_id);
    }

    /// Current status for a room participant. Inferred statuses apply their
    /// normal expiry even if a peer vanished without a clean disconnect.
    pub fn status_for(
        &self,
        user: &UserId,
        now: SystemTime,
    ) -> Option<&crate::live_collaboration::LiveStatus> {
        let status = self.statuses.get(user)?;
        match status.origin {
            crate::live_collaboration::StatusOrigin::Manual => Some(status),
            crate::live_collaboration::StatusOrigin::Inferred { .. }
                if now
                    .duration_since(status.updated_at)
                    .is_ok_and(|age| age <= crate::INFERRED_STATUS_TTL) =>
            {
                Some(status)
            }
            crate::live_collaboration::StatusOrigin::Inferred { .. } => None,
        }
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

    #[test]
    fn set_tab_focus_moves_only_that_user_and_only_between_tabs() {
        // phase: set_tab_focus_moves_user_between_tabs
        {
            let mut ctx = PresenceCtx::empty();
            let alice = UserId("alice".into());
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
        // phase: set_tab_focus_leaves_other_users_on_the_old_tab
        {
            let mut ctx = PresenceCtx::empty();
            let alice = UserId("alice".into());
            let bob = UserId("bob".into());
            let a = PresenceKey::Tab { id: "aaa".into() };
            let b = PresenceKey::Tab { id: "bbb".into() };

            ctx.set_tab_focus(&a, alice.clone());
            ctx.set_tab_focus(&a, bob.clone());
            ctx.set_tab_focus(&b, alice.clone());

            assert_eq!(ctx.peers_on(&a), &[bob]);
            assert_eq!(ctx.peers_on(&b), &[alice]);
        }
        // phase: set_tab_focus_on_non_tab_key_falls_back_to_register_presence
        {
            let mut ctx = PresenceCtx::empty();
            let alice = UserId("alice".into());
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

    #[test]
    fn colour_overrides_parse_hex_and_otherwise_fall_back_to_the_palette() {
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
        assert_eq!(ctx.color_for(&UserId("alice".into())), user_color("alice"));

        // color_for honours an override when set.
        let mut ctx = PresenceCtx::empty();
        ctx.set_color_override(UserId("alice".into()), "#abcdef");
        assert_eq!(ctx.color_for(&UserId("alice".into())), (0xab, 0xcd, 0xef));
        assert_eq!(ctx.color_for(&UserId("bob".into())), user_color("bob"));

        // A malformed override clears any prior entry.
        let mut ctx = PresenceCtx::empty();
        ctx.set_color_override(UserId("alice".into()), "#abcdef");
        ctx.set_color_override(UserId("alice".into()), "nonsense");
        assert_eq!(ctx.color_for(&UserId("alice".into())), user_color("alice"));
    }
}

#[cfg(test)]
mod remote_cursor_tests {
    use super::*;

    #[test]
    fn a_peer_wears_the_colour_their_machine_says() {
        let mut ctx = PresenceCtx::empty();
        let peer = UserId("peer@42".into());
        ctx.upsert_remote_cursor(peer.0.clone(), [1.0, 2.0], None);
        assert_eq!(ctx.color_for(&peer), user_color("peer@42"));
        ctx.set_presence_hue(peer.clone(), crate::tokens::PresenceHue::Violet);
        let violet = identity_rgb(crate::tokens::PresenceHue::Violet);
        assert_eq!(ctx.color_for(&peer), violet);
        let painted = ctx.remote_cursors[0].color;
        assert!((painted[0] - f32::from(violet.0) / 255.0).abs() < 1e-3);
        assert!((painted[3] - 1.0).abs() < f32::EPSILON, "alpha is kept");
        let packed = crate::collaboration::Collaborator {
            id: peer.0.clone(),
            name: "Ada".into(),
            colour: Some(crate::tokens::PresenceHue::Violet),
            is_agent: false,
            is_self: false,
        }
        .packed_colour();
        assert_eq!(
            crate::collaboration::Collaborator::colour_from_packed(packed),
            Some(crate::tokens::PresenceHue::Violet),
            "every presence colour survives the roster's wire"
        );
        for hue in crate::tokens::PresenceHue::ALL {
            let (r, g, b) = identity_rgb(hue);
            let packed = (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b);
            assert_eq!(
                crate::collaboration::Collaborator::colour_from_packed(packed),
                Some(hue)
            );
        }
        assert_eq!(
            crate::collaboration::Collaborator::colour_from_packed(0),
            None
        );
    }

    #[test]
    fn remote_cursors_upsert_in_place_and_remove() {
        // phase: upsert_remote_cursor_inserts_new_entry
        {
            let mut ctx = PresenceCtx::empty();
            ctx.upsert_remote_cursor("alice".into(), [42.0, 17.0], None);
            assert_eq!(ctx.remote_cursors.len(), 1);
            let c = &ctx.remote_cursors[0];
            assert_eq!(c.user_id, "alice");
            assert_eq!(c.position, [42.0, 17.0]);
            assert_eq!(c.label, "alice");
            let (r, g, b) = user_color("alice");
            assert!((c.color[0] - f32::from(r) / 255.0).abs() < 1e-3);
            assert!((c.color[1] - f32::from(g) / 255.0).abs() < 1e-3);
            assert!((c.color[2] - f32::from(b) / 255.0).abs() < 1e-3);
            assert_eq!(c.color[3], 1.0);
        }
        // phase: upsert_remote_cursor_updates_existing_in_place
        {
            let mut ctx = PresenceCtx::empty();
            ctx.upsert_remote_cursor("bob".into(), [10.0, 10.0], None);
            ctx.upsert_remote_cursor("bob".into(), [200.0, 300.0], None);
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
            ctx.upsert_remote_cursor("alice".into(), [0.0, 0.0], Some("Alice Smith"));
            assert_eq!(ctx.remote_cursors[0].label, "Alice Smith");

            ctx.upsert_remote_cursor("alice".into(), [50.0, 50.0], None);
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
            ctx.upsert_remote_cursor("alice".into(), [1.0, 2.0], None);
            ctx.upsert_remote_cursor("bob".into(), [3.0, 4.0], None);
            assert_eq!(ctx.remote_cursors.len(), 2);
            ctx.remove_remote_cursor("alice");
            assert_eq!(ctx.remote_cursors.len(), 1);
            assert_eq!(ctx.remote_cursors[0].user_id, "bob");
            ctx.remove_remote_cursor("nobody");
            assert_eq!(ctx.remote_cursors.len(), 1);
        }
    }
}

#[cfg(test)]
mod string_id_wrapper_tests {
    use super::*;
    use crate::collaboration::PresencePayload;

    #[test]
    fn peer_state_writes_land_in_their_maps_and_relationships() {
        // phase: set_remote_caret_writes_to_carets_map
        {
            let mut ctx = PresenceCtx::empty();
            let key = PresenceKey::Workspace { id: "home".into() };
            ctx.set_remote_caret("alice".into(), key.clone(), 42);
            assert_eq!(ctx.caret_for(&key, &UserId("alice".into())), Some(42));
        }
        // phase: set_remote_selection_writes_to_selections_map
        {
            let mut ctx = PresenceCtx::empty();
            let key = PresenceKey::Workspace { id: "home".into() };
            ctx.set_remote_selection("bob".into(), key.clone(), 3, 9);
            let r = ctx
                .selection_for(&key, &UserId("bob".into()))
                .expect("selection");
            assert_eq!((r.start, r.end), (3, 9));
        }
        // phase: register_peer_at_workspace_adds_to_here
        {
            let mut ctx = PresenceCtx::empty();
            let key = PresenceKey::Workspace { id: "home".into() };
            ctx.register_peer_at("alice".into(), key.clone());
            assert_eq!(ctx.peers_on(&key), &[UserId("alice".into())]);
        }
        // phase: register_peer_at_tab_routes_through_set_tab_focus
        {
            // A peer switching between two tab keys must end up on exactly
            // one — the previous tab should drop them. This guards the
            // routing through set_tab_focus.
            let mut ctx = PresenceCtx::empty();
            let a = PresenceKey::Tab { id: "aaa".into() };
            let b = PresenceKey::Tab { id: "bbb".into() };
            ctx.register_peer_at("alice".into(), a.clone());
            ctx.register_peer_at("alice".into(), b.clone());
            assert!(
                ctx.peers_on(&a).is_empty(),
                "peer must have been evicted from previous tab"
            );
            assert_eq!(ctx.peers_on(&b), &[UserId("alice".into())]);
        }
        // phase: peer_pointer_reads_follow_target
        {
            let mut ctx = PresenceCtx::empty();
            ctx.upsert_remote_cursor("alice".into(), [120.0, 44.0], None);
            assert_eq!(ctx.peer_pointer("alice"), Some([120.0, 44.0]));
            assert_eq!(ctx.peer_pointer("nobody"), None);
        }
        // phase: follow_relationship_is_room_scoped_ephemeral_and_releasable
        {
            let mut ctx = PresenceCtx::empty();
            let room = PresenceKey::Workspace {
                id: "studio".into(),
            };
            let other_room = PresenceKey::Workspace {
                id: "elsewhere".into(),
            };
            ctx.apply_remote_presence(
                "alice".into(),
                room.clone(),
                PresencePayload::Following {
                    target: Some("bob".into()),
                },
            );
            let relationships: Vec<_> = ctx
                .following_in(&room)
                .into_iter()
                .map(|(follower, target)| (follower.clone(), target.clone()))
                .collect();
            assert_eq!(
                relationships,
                vec![(UserId("alice".into()), UserId("bob".into()))],
                "observers learn only the follower-target relation in this room"
            );
            assert!(ctx.following_in(&other_room).is_empty());

            ctx.apply_remote_presence(
                "alice".into(),
                room.clone(),
                PresencePayload::Following { target: None },
            );
            assert!(
                ctx.following_in(&room).is_empty(),
                "stop clears the visual cue"
            );

            ctx.apply_remote_presence(
                "alice".into(),
                room.clone(),
                PresencePayload::Following {
                    target: Some("bob".into()),
                },
            );
            ctx.unregister_peer("bob");
            assert!(
                ctx.following_in(&room).is_empty(),
                "disconnecting either endpoint removes the ephemeral relationship"
            );
        }
    }

    #[test]
    fn unregistering_a_peer_drops_only_its_traces() {
        // phase: unregister_peer_drops_every_trace
        {
            let mut ctx = PresenceCtx::empty();
            let key_a = PresenceKey::Workspace { id: "home".into() };
            let key_b = PresenceKey::Tab { id: "tab1".into() };

            ctx.register_peer_at("alice".into(), key_a.clone());
            ctx.register_peer_at("alice".into(), key_b.clone());
            ctx.set_remote_caret("alice".into(), key_a.clone(), 5);
            ctx.set_remote_selection("alice".into(), key_a.clone(), 1, 4);
            ctx.set_pointer(&key_a, UserId("alice".into()), (10.0, 20.0));
            ctx.upsert_remote_cursor("alice".into(), [10.0, 20.0], None);

            // Sanity: alice is everywhere.
            assert!(!ctx.peers_on(&key_a).is_empty());
            assert!(!ctx.peers_on(&key_b).is_empty());
            assert!(ctx.caret_for(&key_a, &UserId("alice".into())).is_some());
            assert!(ctx.selection_for(&key_a, &UserId("alice".into())).is_some());
            assert!(ctx.pointer_for(&key_a, &UserId("alice".into())).is_some());
            assert_eq!(ctx.remote_cursors.len(), 1);

            ctx.unregister_peer("alice");

            // Alice is purged.
            assert!(ctx.peers_on(&key_a).is_empty());
            assert!(ctx.peers_on(&key_b).is_empty());
            assert!(ctx.caret_for(&key_a, &UserId("alice".into())).is_none());
            assert!(ctx.selection_for(&key_a, &UserId("alice".into())).is_none());
            assert!(ctx.pointer_for(&key_a, &UserId("alice".into())).is_none());
            assert!(ctx.remote_cursors.is_empty());
        }
        // phase: unregister_peer_clears_typing_and_name
        {
            let mut ctx = PresenceCtx::empty();
            let alice = UserId("alice".into());
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
            ctx.unregister_peer("alice");
            assert!(
                ctx.active_typing(&alice, &key, crate::time::now())
                    .is_empty()
            );
            assert_eq!(
                ctx.display_name(&alice),
                "alice",
                "name reverts to id fallback"
            );
        }
        // phase: unregister_peer_leaves_other_users_alone
        {
            // Removing alice must not collateral-damage bob.
            let mut ctx = PresenceCtx::empty();
            let key = PresenceKey::Workspace { id: "home".into() };
            ctx.register_peer_at("alice".into(), key.clone());
            ctx.register_peer_at("bob".into(), key.clone());
            ctx.set_remote_caret("bob".into(), key.clone(), 7);
            ctx.upsert_remote_cursor("bob".into(), [99.0, 99.0], None);

            ctx.unregister_peer("alice");

            assert_eq!(ctx.peers_on(&key), &[UserId("bob".into())]);
            assert_eq!(ctx.caret_for(&key, &UserId("bob".into())), Some(7));
            assert_eq!(ctx.remote_cursors.len(), 1);
            assert_eq!(ctx.remote_cursors[0].user_id, "bob");
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn peers_keep_stable_colours_and_honest_labels_in_any_arrival_order() {
        // phase: presence_user_color_is_stable_and_distinct
        {
            // Same id → same colour every time (deterministic, no shared state).
            let a1 = PresenceUser::from_id("alice");
            let a2 = PresenceUser::new("alice", "Alice Smith");
            assert_eq!(a1.color, a2.color, "colour depends only on id");
            assert_eq!(a1.color, user_color("alice"));
            // Distinct ids → distinct colours (the whole point of the palette).
            let bob = PresenceUser::from_id("bob");
            assert_ne!(a1.color, bob.color);
            // A friendlier name doesn't perturb the colour.
            assert_eq!(a2.name, "Alice Smith");
            assert_eq!(a1.name, "alice", "from_id uses the id as the fallback name");
        }
        // phase: presence_user_rgba_is_normalised
        {
            let u = PresenceUser {
                id: "x".into(),
                name: "x".into(),
                color: (255, 128, 0),
                is_agent: false,
            };
            let c = u.color_rgba(0.5);
            assert!((c[0] - 1.0).abs() < 1e-6);
            assert!((c[1] - 128.0 / 255.0).abs() < 1e-6);
            assert!((c[2] - 0.0).abs() < 1e-6);
            assert!((c[3] - 0.5).abs() < 1e-6);
        }
        // phase: roster_name_resolves_with_id_fallback
        {
            let mut ctx = PresenceCtx::empty();
            let alice = UserId("alice".into());
            // No roster entry yet → falls back to the id.
            assert_eq!(ctx.display_name(&alice), "alice");
            ctx.set_display_name(alice.clone(), "Alice Smith");
            assert_eq!(ctx.display_name(&alice), "Alice Smith");
            // `user()` bundles id + name + colour.
            let pu = ctx.user(&alice);
            assert_eq!(pu.name, "Alice Smith");
            assert_eq!(pu.color, user_color("alice"));
        }
        // phase: participant_kind_survives_name_resolution
        {
            let mut ctx = PresenceCtx::empty();
            let scribe = UserId("agent-scribe".into());
            ctx.set_display_name(scribe.clone(), "Scribe");
            ctx.set_participant_kind(&scribe, true);
            assert!(ctx.is_agent(&scribe));
            assert!(ctx.user(&scribe).is_agent);
            assert_eq!(ctx.display_name(&scribe), "Scribe · Agent");
            ctx.set_display_name(scribe.clone(), "Editorial helper");
            assert!(
                ctx.user(&scribe).is_agent,
                "renaming cannot turn software into a person"
            );
            assert_eq!(ctx.display_name(&scribe), "Editorial helper · Agent");
            ctx.set_participant_kind(&scribe, false);
            assert!(!ctx.user(&scribe).is_agent);
            assert_eq!(ctx.display_name(&scribe), "Editorial helper");
        }
        // phase: an_unnamed_peer_is_labelled_with_its_id_and_never_with_a_stand_in
        // Labelled with the raw id, never blank and never a stand-in name — a
        // blank chip reads as a rendering fault, a stand-in reads as a real person.
        {
            let ctx = PresenceCtx::empty();
            let peer = UserId("peer@5537458103208628978".into());

            let label = ctx.display_name(&peer);
            assert_eq!(label, "peer@5537458103208628978");
            assert!(!label.trim().is_empty(), "a caret chip must never be blank");
            assert_eq!(
                ctx.user(&peer).name,
                "peer@5537458103208628978",
                "every renderer resolves the same way, so none of them invents a name",
            );
        }
        // phase: a_peer_that_gains_a_name_keeps_its_colour
        // Colour is keyed on the id; gaining a name must not perturb it.
        {
            let mut ctx = PresenceCtx::empty();
            let peer = UserId("peer@5537458103208628978".into());
            let before = ctx.color_for(&peer);

            ctx.set_display_name(peer.clone(), "bob");

            assert_eq!(ctx.display_name(&peer), "bob");
            assert_eq!(
                ctx.color_for(&peer),
                before,
                "a peer must not change colour in front of the people watching it",
            );
        }
        // phase: a_name_that_arrives_after_the_pointer_relabels_it
        // A name can arrive after the pointer it belongs to. The pill has to
        // pick it up, or a peer stays a number on screen until they happen
        // to move again.
        {
            let mut ctx = PresenceCtx::empty();
            ctx.upsert_remote_cursor("peer@77".into(), [10.0, 20.0], None);
            assert_eq!(ctx.remote_cursors[0].label, "peer@77");

            ctx.set_display_name(UserId("peer@77".into()), "bob");

            assert_eq!(
                ctx.remote_cursors.len(),
                1,
                "naming a peer adds no second pointer"
            );
            assert_eq!(ctx.remote_cursors[0].label, "bob");
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
            ctx.set_display_name(UserId("peer@77".into()), "bob");

            ctx.upsert_remote_cursor("peer@77".into(), [10.0, 20.0], None);

            assert_eq!(ctx.remote_cursors[0].label, "bob");
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn each_pointer_is_painted_by_exactly_the_layer_that_owns_its_space() {
        // phase: a_desktop_pointer_is_the_compositors_to_paint_and_not_this_layers
        // One person is one pointer: a workspace-scoped arrival lands in *both*
        // stores — `pointers` (compositor paints) and `remote_cursors` ("follow"
        // reads) — but only one of them may paint it.
        {
            let mut ctx = PresenceCtx::empty();
            let room = "40fc5b85-22d7-4294-a9df-ecc8a391cfce";
            let key = PresenceKey::Workspace { id: room.into() };

            ctx.apply_remote_presence(
                "peer@77".into(),
                key.clone(),
                crate::collaboration::PresencePayload::Pointer { x: 120.0, y: 90.0 },
            );

            // The compositor's store has them: this is not "the peer never
            // arrived" passing for "the peer is not drawn twice".
            assert_eq!(
                ctx.pointer_for(&key, &UserId("peer@77".into())),
                Some((120.0, 90.0)),
                "the overlay must still have a desktop pointer to paint",
            );
            assert_eq!(ctx.peer_pointer("peer@77"), Some([120.0, 90.0]));

            assert_eq!(ctx.remote_cursors.len(), 1, "one arrival is one record");
            assert!(
                !ctx.remote_cursors[0].painted_by_window(),
                "a desktop pointer painted here as well is the same person twice",
            );
        }
        // phase: a_pointer_no_overlay_claims_is_still_this_layers_to_paint
        // The other half: a pointer with no desktop scope has no overlay
        // claiming it, so this layer stays the one that draws it.
        {
            let mut ctx = PresenceCtx::empty();

            // The relay/SDK path: a cursor with no scope at all.
            ctx.upsert_remote_cursor("peer@77".into(), [10.0, 20.0], None);
            assert!(
                ctx.remote_cursors[0].painted_by_window(),
                "dropping this one would leave the relay dialect with no pointer at all",
            );

            let widget = PresenceKey::Widget {
                path: vec![PathSegment::Index(0)],
            };
            ctx.apply_remote_presence(
                "peer@88".into(),
                widget,
                crate::collaboration::PresencePayload::Pointer { x: 5.0, y: 6.0 },
            );
            let scoped = ctx
                .remote_cursors
                .iter()
                .find(|c| c.user_id == "peer@88")
                .expect("the pointer landed");
            assert!(
                scoped.painted_by_window(),
                "only a desktop scope hands a pointer to the compositor",
            );
        }
        // phase: one_content_pointer_reaches_two_surfaces_and_one_places_it
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
                    "peer@77".into(),
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
        // phase: a_session_that_has_not_named_its_surface_places_no_content_pointer
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
                "peer@77".into(),
                key,
                crate::collaboration::PresencePayload::Pointer { x: 1.0, y: 2.0 },
            );
            assert!(
                ctx.remote_cursors.iter().all(|c| !c.painted_by_window()),
                "an unnamed surface must not claim somebody else's coordinates",
            );
        }
        // phase: a_pointer_that_changes_transport_changes_hands
        // A peer moving transport changes hands rather than keeping its first owner.
        {
            let mut ctx = PresenceCtx::empty();
            let key = PresenceKey::Workspace { id: "room".into() };
            ctx.apply_remote_presence(
                "peer@77".into(),
                key,
                crate::collaboration::PresencePayload::Pointer { x: 1.0, y: 2.0 },
            );
            assert!(!ctx.remote_cursors[0].painted_by_window());

            ctx.upsert_remote_cursor("peer@77".into(), [3.0, 4.0], None);

            assert_eq!(ctx.remote_cursors.len(), 1, "still one person");
            assert!(
                ctx.remote_cursors[0].painted_by_window(),
                "a peer nobody else is painting must come back to this layer",
            );
        }
    }

    #[test]
    fn typing_marks_fade_prune_and_coalesce() {
        // phase: typing_mark_fades_linearly_and_prunes
        {
            use crate::time::Duration;
            let mut ctx = PresenceCtx::empty();
            let bob = UserId("bob".into());
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
            let mid = t0 + TYPING_FADE / 2;
            let half = ctx.active_typing(&bob, &key, mid);
            assert_eq!(half.len(), 1);
            assert!(
                (half[0].1 - 0.5).abs() < 0.05,
                "alpha halves at mid-life, got {}",
                half[0].1
            );
            assert!(ctx.has_active_typing(mid));

            // Past the window: gone from the read, and prune reclaims it.
            let after = t0 + TYPING_FADE + Duration::from_millis(1);
            assert!(ctx.active_typing(&bob, &key, after).is_empty());
            assert!(!ctx.has_active_typing(after));
            assert!(!ctx.typing.is_empty(), "not yet pruned");
            ctx.prune_typing(after);
            assert!(ctx.typing.is_empty(), "prune reclaims fully-faded marks");
        }
        // phase: typing_marks_coalesce_when_contiguous
        {
            let mut ctx = PresenceCtx::empty();
            let bob = UserId("bob".into());
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

/// A session must not paint its own participant, and must go on
/// painting everybody else. Both halves are asserted here: the second
/// is the one that keeps a fix in this area from hiding the room.
#[cfg(test)]
mod own_identity_tests {
    use super::*;
    use crate::collaboration::PresencePayload;

    fn room() -> PresenceKey {
        PresenceKey::Workspace {
            id: "room-1".into(),
        }
    }

    fn mine() -> PresenceCtx {
        let mut ctx = PresenceCtx::empty();
        ctx.set_own_identity(["ada"]);
        ctx
    }

    #[test]
    fn a_session_suppresses_only_its_own_identity() {
        // phase: a_session_does_not_paint_a_pill_for_itself
        {
            let mut ctx = mine();
            ctx.apply_remote_presence(
                "ada".into(),
                room(),
                PresencePayload::Pointer { x: 120.0, y: 90.0 },
            );
            ctx.apply_remote_presence("ada".into(), room(), PresencePayload::Caret { pos: 7 });

            assert!(
                ctx.remote_cursors.is_empty(),
                "painted a cursor pill for this session's own identity: {:?}",
                ctx.remote_cursors,
            );
            assert_eq!(ctx.pointer_for(&room(), &UserId("ada".into())), None);
            assert_eq!(ctx.caret_for(&room(), &UserId("ada".into())), None);
            assert!(ctx.peers_on(&room()).is_empty());
        }
        // phase: another_person_is_still_painted
        // The control. A second *person* is a second participant, and the
        // self-rule may not touch them — hiding a real collaborator is the
        // worse of the two failures by a distance.
        {
            let mut ctx = mine();
            ctx.apply_remote_presence(
                "peer@77".into(),
                room(),
                PresencePayload::Pointer { x: 12.0, y: 34.0 },
            );
            ctx.apply_remote_presence("peer@77".into(), room(), PresencePayload::Caret { pos: 3 });

            assert_eq!(
                ctx.pointer_for(&room(), &UserId("peer@77".into())),
                Some((12.0, 34.0))
            );
            assert_eq!(ctx.caret_for(&room(), &UserId("peer@77".into())), Some(3));
            assert_eq!(ctx.remote_cursors.len(), 1);
            assert_eq!(ctx.remote_cursors[0].user_id, "peer@77");
        }
        // phase: an_agent_in_the_same_document_is_still_painted
        // A document legitimately has more than one app participant: the
        // agent joins as one and publishes a caret as it edits, with no
        // window anywhere. It announces an identity of its own, so the
        // rule leaves it alone — the test is the participant, not the
        // process and not the presence of a window.
        {
            let mut ctx = mine();
            ctx.apply_remote_presence("agent".into(), room(), PresencePayload::Caret { pos: 11 });
            assert_eq!(ctx.caret_for(&room(), &UserId("agent".into())), Some(11));
        }
        // phase: a_sibling_arriving_still_gets_the_document
        // `Arrived` is not only a thing to draw — it is how a joiner asks
        // the room for the document, and a second window of the same app
        // depends on the answer. Suppressing the pill must not suppress
        // the handshake.
        {
            let mut ctx = mine();
            assert_eq!(
                ctx.apply_remote_presence("ada".into(), room(), PresencePayload::Arrived),
                RemotePresenceEffect::PeerArrived,
                "a sibling that arrived must still be answered with state",
            );
            assert!(
                ctx.peers_on(&room()).is_empty(),
                "answering a sibling must not also list it as somebody else",
            );
        }
        // phase: an_unnamed_session_suppresses_nobody
        // A session that does not know what it is called suppresses
        // nobody. The rule fails open on purpose.
        {
            let mut ctx = PresenceCtx::empty();
            ctx.apply_remote_presence(
                "ada".into(),
                room(),
                PresencePayload::Pointer { x: 1.0, y: 2.0 },
            );
            assert_eq!(ctx.remote_cursors.len(), 1);
        }
        // phase: a_blank_identity_is_not_an_identity
        {
            let mut ctx = PresenceCtx::empty();
            ctx.set_own_identity(["", "   "]);
            assert!(!ctx.is_own_identity(""));
            ctx.apply_remote_presence(
                String::new(),
                room(),
                PresencePayload::Pointer { x: 1.0, y: 2.0 },
            );
            assert_eq!(ctx.remote_cursors.len(), 1);
        }
        // phase: the_cursor_stream_honours_the_same_rule
        // The cursor stream reaches the same pill without passing through
        // `apply_remote_presence`. A guard on one door and not the other
        // leaves the bug reachable.
        {
            let mut ctx = mine();
            ctx.upsert_remote_cursor("ada".into(), [5.0, 5.0], None);
            assert!(ctx.remote_cursors.is_empty());
            ctx.upsert_remote_cursor("peer@77".into(), [5.0, 5.0], None);
            assert_eq!(ctx.remote_cursors.len(), 1);
        }
        // phase: a_frame_stamped_with_the_rooms_name_for_this_session_is_still_its_own
        {
            let mut ctx = PresenceCtx::empty();
            ctx.set_own_identity(["ada"]);
            ctx.set_self_participant(UserId("peer@42".into()), "bob");
            assert!(ctx.is_own_identity("bob"));
            assert!(
                !ctx.is_own_identity("Claude"),
                "a peer the room happens to have named must stay a peer",
            );
        }
    }

    #[test]
    fn the_own_participant_is_known_without_becoming_a_peer() {
        // phase: a_session_does_not_follow_its_own_view
        // Following yourself is not a thing. A `View` broadcast stamped
        // with this session's own identity is neither stored nor reported
        // as a reason to move the viewport.
        {
            let mut ctx = mine();
            let subset = crate::view::ViewSubset {
                scroll: vec![10.0],
                open_resource: None,
                viewport: crate::view::ViewRect {
                    x: 0.0,
                    y: 0.0,
                    width: 10.0,
                    height: 10.0,
                },
                view_mode: None,
            };
            assert_eq!(
                ctx.apply_remote_presence(
                    "ada".into(),
                    PresenceKey::view(&subset),
                    PresencePayload::Arrived,
                ),
                RemotePresenceEffect::None,
            );
            assert!(ctx.peer_view(&UserId("ada".into())).is_none());
        }
        // phase: knowing_which_participant_you_are_does_not_make_you_a_peer
        // The room's answer to "which participant am I" arrives without making
        // this session a participant of its own presence store — a `names` entry
        // for it would paint a pill over its own document.
        {
            let mut ctx = PresenceCtx::empty();
            let me = UserId("peer@9007495404254172154".into());
            ctx.set_self_participant(me.clone(), "bob");

            assert_eq!(ctx.self_id(), Some(&me));
            assert_eq!(ctx.self_name(), Some("bob"));
            assert!(
                !ctx.names.contains_key(&me),
                "the row became a display name, which is what the roster, the \
             caret chips and the mention colours all read"
            );

            // And a frame stamped with that address is this session's own, so the
            // pill and the cursor stay suppressed.
            assert!(ctx.is_own_identity(&me.0));
            assert_eq!(
                ctx.apply_remote_presence(me.0, room(), PresencePayload::Caret { pos: 4 }),
                RemotePresenceEffect::None,
            );
            assert!(
                ctx.peers_on(&room()).is_empty(),
                "this session was listed as somebody else in its own room"
            );
        }
        // phase: an_unnamed_own_participant_reports_no_name
        // A participant the room has admitted but not named has an address and no
        // name. Handing the address back as a name is how a routing hash ends up
        // rendered where a name belongs.
        {
            let mut ctx = PresenceCtx::empty();
            let me = UserId("peer@42".into());
            ctx.set_self_participant(me.clone(), "   ");
            assert_eq!(ctx.self_id(), Some(&me));
            assert_eq!(ctx.self_name(), None);
        }
        // phase: a_blank_own_participant_is_refused
        {
            let mut ctx = PresenceCtx::empty();
            ctx.set_self_participant(UserId("  ".into()), "bob");
            assert_eq!(ctx.self_id(), None);
            assert!(!ctx.is_own_identity(""));
            assert!(!ctx.is_own_identity("  "));
        }
        // phase: restating_the_published_identities_keeps_the_room_address
        {
            let mut ctx = PresenceCtx::empty();
            let me = UserId("peer@42".into());
            ctx.set_self_participant(me.clone(), "bob");
            ctx.set_own_identity(["so.tilo.chat"]);
            assert!(ctx.is_own_identity(&me.0));
            assert!(ctx.is_own_identity("so.tilo.chat"));
        }
    }

    #[test]
    fn a_quiet_pointer_fades_out_just_before_it_is_retired() {
        let t0 = crate::time::now();
        let room = PresenceKey::Workspace { id: "r".into() };
        let peer = UserId("peer@7".into());
        let mut ctx = PresenceCtx::default();
        ctx.set_pointer_at(&room, peer.clone(), (10.0, 20.0), t0);
        let opacity = |ctx: &PresenceCtx, after: Duration, fade: Duration| {
            ctx.pointer_opacity(&room, &peer, t0 + after, POINTER_IDLE, fade)
        };

        let fade_starts = POINTER_IDLE.saturating_sub(POINTER_FADE);
        assert!((opacity(&ctx, fade_starts, POINTER_FADE) - 1.0).abs() < f32::EPSILON);
        assert!(!ctx.pointers_fading(t0 + fade_starts, POINTER_IDLE, POINTER_FADE));
        let midway = fade_starts + POINTER_FADE / 2;
        let half = opacity(&ctx, midway, POINTER_FADE);
        assert!((half - 0.5).abs() < 0.01, "half-faded midway: {half}");
        assert!(ctx.pointers_fading(t0 + midway, POINTER_IDLE, POINTER_FADE));
        assert!(
            (opacity(&ctx, midway, Duration::ZERO) - 1.0).abs() < f32::EPSILON,
            "reduced motion: opaque until retired",
        );
        assert!(ctx.prune_pointers(t0 + POINTER_IDLE, POINTER_IDLE));
        assert_eq!(ctx.pointer_for(&room, &peer), None);
        assert!(!ctx.pointers_fading(t0 + POINTER_IDLE, POINTER_IDLE, POINTER_FADE));

        // Heard from again: opaque once more.
        let t1 = t0 + POINTER_IDLE + Duration::from_secs(1);
        ctx.set_pointer_at(&room, peer.clone(), (10.0, 20.0), t1);
        assert!(
            (ctx.pointer_opacity(&room, &peer, t1, POINTER_IDLE, POINTER_FADE) - 1.0).abs()
                < f32::EPSILON
        );
    }

    #[test]
    fn pointer_registrations_retire_unrestated_pointers_and_keep_kinds() {
        // phase: a_pointer_nobody_restates_is_retired_and_nothing_else_is
        // A pointer nobody restates is retired; everything else about that peer
        // stays — retiring a cursor is not evicting a peer, or a participant
        // would vanish from the roster whenever it stops moving.
        {
            let t0 = crate::time::now();
            let horizon = Duration::from_secs(4);
            let room = PresenceKey::Workspace { id: "r".into() };
            let block = PresenceKey::Widget {
                path: vec![PathSegment::Index(0)],
            };
            let claude = UserId("peer@7".into());
            let bob = UserId("peer@8".into());

            let mut ctx = PresenceCtx::default();
            ctx.set_display_name(claude.clone(), "Claude");
            ctx.set_pointer_at(&room, claude.clone(), (1178.31, 582.0), t0);
            ctx.set_caret(&block, claude.clone(), 12);

            assert_eq!(ctx.pointer_for(&room, &claude), Some((1178.31, 582.0)));
            assert!(
                !ctx.prune_pointers(t0 + Duration::from_secs(1), horizon),
                "a peer heard from a moment ago is not stale",
            );
            assert_eq!(
                ctx.pointer_for(&room, &claude),
                Some((1178.31, 582.0)),
                "and is still painted",
            );

            // Somebody else keeps talking; the first one stops.
            let t1 = t0 + horizon + Duration::from_secs(1);
            ctx.set_pointer_at(&room, bob.clone(), (300.0, 200.0), t1);
            assert!(
                ctx.prune_pointers(t1, horizon),
                "the one that went quiet is retired, which is a reason to repaint",
            );
            assert_eq!(
                ctx.pointer_for(&room, &claude),
                None,
                "no cursor for a peer that has stopped saying where it is pointing",
            );
            assert_eq!(
                ctx.pointer_for(&room, &bob),
                Some((300.0, 200.0)),
                "and the peer still talking is untouched - this retires one \
             pointer, not the scope",
            );

            // The peer did not leave.
            assert!(
                ctx.peers_on(&room).contains(&claude),
                "a peer that has gone quiet about pointing is still in the room",
            );
            assert_eq!(
                ctx.caret_for(&block, &claude),
                Some(12),
                "and its caret in the document is untouched",
            );
            assert_eq!(
                ctx.display_name(&claude),
                "Claude",
                "and the room still knows what to call it",
            );

            // And it comes back the instant it says something again.
            ctx.set_pointer_at(&room, claude.clone(), (10.0, 20.0), t1);
            assert_eq!(ctx.pointer_for(&room, &claude), Some((10.0, 20.0)));
        }
        // phase: authenticated_participant_kind_survives_presence_and_clears_on_disconnect
        {
            let mut ctx = PresenceCtx::empty();
            let agent = UserId("peer@agent-7".into());

            assert_eq!(ctx.participant_kind(&agent), ParticipantKind::Person);
            ctx.set_participant_kind(agent.clone(), ParticipantKind::Agent);
            ctx.set_display_name(agent.clone(), "Release assistant");
            ctx.set_caret(&PresenceKey::Widget { path: vec![] }, agent.clone(), 4);

            assert!(ctx.is_agent(&agent));
            assert_eq!(ctx.display_name(&agent), "Release assistant · Agent");

            ctx.unregister_peer(&agent.0);
            assert_eq!(ctx.participant_kind(&agent), ParticipantKind::Person);
        }
        // phase: a_desktop_pointer_registers_its_peer_and_an_in_app_one_does_not
        // A workspace-scoped pointer registers its peer as present; a
        // widget-scoped one (a position in a document, not on the desktop) does not.
        {
            let t0 = crate::time::now();
            let room = PresenceKey::Workspace { id: "r".into() };
            let block = PresenceKey::Widget {
                path: vec![PathSegment::Index(0)],
            };
            let alice = UserId("peer@1".into());
            let bob = UserId("peer@2".into());

            let mut ctx = PresenceCtx::default();
            ctx.set_pointer_at(&room, alice.clone(), (5.0, 6.0), t0);
            ctx.set_pointer_at(&block, bob.clone(), (7.0, 8.0), t0);

            assert!(ctx.peers_on(&room).contains(&alice));
            assert!(
                !ctx.peers_on(&block).contains(&bob),
                "a pointer over a widget must not put a presence dot on it",
            );
        }
    }
}

/// A person has one pointer, and the screen has to agree.
///
/// Two halves: which layer is entitled to paint a given arrival, and how many
/// rows one person may occupy in the store behind it.
#[cfg(test)]
mod one_person_one_pointer_tests {
    use super::*;
    use crate::collaboration::PresencePayload;

    #[test]
    fn one_person_has_one_pointer_that_moves_between_scopes() {
        // phase: a_pointer_published_without_a_scope_belongs_to_the_desktop
        // A pointer arriving with no scope of its own is on the *desktop* —
        // the capnp presence surface has no keyed cursor call, so it goes out as
        // `publishCursor(x, y)` and loses the room scope on the wire. Reading
        // that back as a *widget* scope told every window the pointer was
        // theirs, so one person came out once per open window.
        {
            let mut ctx = PresenceCtx::default();
            ctx.apply_remote_presence(
                "peer@7".into(),
                PresenceKey::pointer_default(),
                PresencePayload::Pointer { x: 319.0, y: 484.0 },
            );

            assert!(
                !ctx.remote_cursors[0].painted_by_window(),
                "an unscoped pointer is the compositor's to paint; a window \
             painting it as well is the same person once per window",
            );
        }
        // phase: a_room_scoped_pointer_still_belongs_to_the_desktop
        // The room scope keeps the answer it always had, so the rule above cannot
        // be satisfied by making *nothing* the window's to paint.
        {
            let mut ctx = PresenceCtx::default();
            ctx.apply_remote_presence(
                "peer@7".into(),
                PresenceKey::Workspace { id: "room".into() },
                PresencePayload::Pointer { x: 10.0, y: 20.0 },
            );
            assert!(!ctx.remote_cursors[0].painted_by_window());
        }
        // phase: a_pointer_inside_a_window_is_still_the_windows_to_paint
        // A pointer inside a window's content is still that window's
        // to paint — the relay dialect and hosted backend have no overlay above
        // them, so swallowing every scope would leave those peers invisible.
        {
            let mut ctx = PresenceCtx::default();
            ctx.apply_remote_presence(
                "peer@7".into(),
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
        // phase: a_persons_pointer_moves_between_scopes_rather_than_multiplying
        // A person's pointer *moves*; it does not accumulate — a peer heard
        // from under two scopes must produce one row, not two, or anything
        // reading across scopes sees them twice.
        {
            let mut ctx = PresenceCtx::default();
            let who = UserId("peer@7".into());
            let first = PresenceKey::Workspace { id: "room".into() };
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
        // phase: moving_one_persons_pointer_leaves_everybody_elses_alone
        // Moving one person's pointer must not take anybody else's with it.
        {
            let mut ctx = PresenceCtx::default();
            let room = PresenceKey::Workspace { id: "room".into() };
            let alice = UserId("peer@1".into());
            let bob = UserId("peer@2".into());

            ctx.set_pointer(&room, alice.clone(), (10.0, 10.0));
            ctx.set_pointer(&room, bob.clone(), (20.0, 20.0));
            ctx.set_pointer(&PresenceKey::pointer_default(), alice.clone(), (30.0, 30.0));

            assert_eq!(ctx.pointer_for(&room, &bob), Some((20.0, 20.0)));
            assert_eq!(
                ctx.pointer_for(&PresenceKey::pointer_default(), &alice),
                Some((30.0, 30.0))
            );
            assert_eq!(ctx.pointer_for(&room, &alice), None);
        }
    }
}
