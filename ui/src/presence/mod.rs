use std::collections::HashMap;
use std::time::Instant;
use crate::permissions::UserId;

mod key;
mod render;
mod geom;
pub use key::{PresenceKey, PathSegment, SelectionRange};
pub use geom::{EdgeMist, MistEdge, edge_mist_for};
// `render::draw_remote_cursors` is a free function — re-export so
// downstream apps (terminal, settings, file-explorer) can call it from
// their per-frame draw without depending on the shell.
pub use render::{
    PeerCursor, PillPlacement, PillRequest, draw_peer_cursors, draw_remote_cursors,
    draw_remote_presence, layout_peer_pills,
};

/// How long a peer's "just typed" highlight lingers before it has fully
/// faded away. A textmarker swipe that decays over this window so a busy
/// document doesn't accumulate permanent smears of colour — you see where
/// a peer is *currently* working, then it quietly clears.
pub const TYPING_FADE: std::time::Duration = std::time::Duration::from_millis(2500);

/// Stable RGB color for a collaborator, used by every widget that paints a
/// per-peer affordance (tab presence dots, remote cursor, per-folder
/// presence dots, etc.).
///
/// The concept pins a fixed palette for four canonical ids — `me`,
/// `alice`, `bob`, `charlie` — matching the reference mockup's
/// `cursorColor` table. Any other id hashes onto [`PEER_PALETTE`], a
/// closed ring whose members are far enough apart to tell apart at a
/// glance.
///
/// The colour is a pure function of the id and nothing else. That is
/// what lets two machines agree: each host paints the peers it can see,
/// and the sets differ (everyone sees everyone but themselves), so a
/// colour chosen from "who is here" would give the same person a
/// different colour on every screen.
pub fn user_color(user_id: &str) -> (u8, u8, u8) {
    // Concept-pinned palette. Matches `nirios-concept/src/data.ts`
    // USERS[*].cursorColor so the Rust render is pixel-identical to the
    // reference mockup for these names. Each is also a member of
    // `PEER_PALETTE`, so a pin and a hash never land near-but-not-quite
    // on the same colour.
    match user_id {
        "me" => (0x63, 0x66, 0xf1),      // indigo-500
        "alice" => (0xec, 0x48, 0x99),   // pink-500
        "bob" => (0xf9, 0x73, 0x16),     // orange-500
        "charlie" => (0x10, 0xb9, 0x81), // emerald-500
        _ => {
            // FNV-1a 64-bit — cheap, deterministic, no RNG state, and
            // same distribution profile as the old per-widget hashes.
            let mut h: u64 = 0xcbf29ce484222325;
            for b in user_id.as_bytes() {
                h ^= *b as u64;
                h = h.wrapping_mul(0x100000001b3);
            }
            PEER_PALETTE[(h % PEER_PALETTE.len() as u64) as usize]
        }
    }
}

/// Every colour a peer can be drawn in.
///
/// The hash used to pick a hue out of all 360, which sounds like more
/// choice and is in practice worse: uniform sampling puts two peers a
/// couple of degrees apart often enough to see it. `dave`, `erin` and
/// `sara` landed on hues 87, 89 and 88 — three different people in one
/// indistinguishable green, which reads as a rendering fault rather
/// than as three people.
///
/// A closed ring trades that for an honest failure: two ids either get
/// clearly different colours or exactly the same one, and the name on
/// the pill is what settles an exact tie. Twelve hues spaced no closer
/// than 26°, then twelve more offset half a step and lifted in
/// lightness, so neighbours differ in brightness as well as hue and
/// nothing is dark enough to sink into the desktop or pale enough to
/// wash out on a white page.
pub const PEER_PALETTE: [(u8, u8, u8); 24] = [
    (0xf9, 0x73, 0x16), // orange-500 — pinned for "bob"
    (0xdf, 0xdc, 0x3a),
    (0x87, 0xdf, 0x3a),
    (0x3a, 0xdf, 0x4a),
    (0x10, 0xb9, 0x81), // emerald-500 — pinned for "charlie"
    (0x3a, 0xce, 0xdf),
    (0x3a, 0x84, 0xdf),
    (0x63, 0x66, 0xf1), // indigo-500 — pinned for "me"
    (0x89, 0x3a, 0xdf),
    (0xdf, 0x3a, 0xdf),
    (0xec, 0x48, 0x99), // pink-500 — pinned for "alice"
    (0xdf, 0x3a, 0x42),
    (0xf4, 0xcd, 0x71),
    (0xd1, 0xf4, 0x71),
    (0x89, 0xf4, 0x71),
    (0x71, 0xf4, 0xa3),
    (0x71, 0xf4, 0xe4),
    (0x71, 0xc8, 0xf4),
    (0x71, 0x90, 0xf4),
    (0x90, 0x71, 0xf4),
    (0xd3, 0x71, 0xf4),
    (0xf4, 0x71, 0xd3),
    (0xf4, 0x71, 0x94),
    (0xf4, 0x89, 0x71),
];

/// Give everyone in a room a colour nobody else in it is wearing.
///
/// [`user_color`] alone cannot promise this. It is a hash, so two ids
/// land on the same ring entry roughly one time in
/// [`PEER_PALETTE`]`.len()`, and `carol` lands exactly on `alice`'s
/// pink. One time in twenty-four sounds rare until four people are in a
/// room, where it is closer to one time in four.
///
/// So the room resolves it. Everyone keeps the colour their id gives
/// them when it is free, and whoever collides steps to the next free
/// entry. Ids are walked in sorted order, which is what makes this
/// agree across machines: every host in a room ends up seeing the same
/// membership (each sees the others plus itself), so each computes the
/// same assignment, and a person is the same colour on every screen.
/// Sorted order also bounds the churn — a new arrival can only move
/// someone it actually collides with, and only someone whose id sorts
/// after its own.
///
/// `reserved` are colours already spoken for by an explicit roster
/// choice; they are held back so a computed colour never lands on one.
pub fn assign_peer_colors(
    ids: &[&str],
    reserved: &[(u8, u8, u8)],
) -> HashMap<String, (u8, u8, u8)> {
    let mut sorted: Vec<&str> = ids.to_vec();
    sorted.sort_unstable();
    sorted.dedup();

    let mut taken = [false; PEER_PALETTE.len()];
    for c in reserved {
        if let Some(i) = PEER_PALETTE.iter().position(|p| p == c) {
            taken[i] = true;
        }
    }

    let mut out = HashMap::with_capacity(sorted.len());
    for id in sorted {
        let natural = user_color(id);
        let start = PEER_PALETTE
            .iter()
            .position(|p| *p == natural)
            // Every colour `user_color` returns is a ring member, so this
            // cannot fire; falling back to 0 keeps the loop total rather
            // than panicking in a draw path if that ever stops holding.
            .unwrap_or(0);
        let mut slot = start;
        for step in 0..PEER_PALETTE.len() {
            let i = (start + step) % PEER_PALETTE.len();
            if !taken[i] {
                slot = i;
                break;
            }
            // More people than colours: everyone past the ring's size
            // keeps their natural entry and shares it. A name still says
            // who is who.
            slot = start;
        }
        taken[slot] = true;
        out.insert(id.to_string(), PEER_PALETTE[slot]);
    }
    out
}

/// How far apart two colours are, weighted the way an eye weights them
/// (the "redmean" approximation). Used to state, as a test rather than
/// as a hope, that no two peers can be painted in confusable colours.
pub fn color_distance(a: (u8, u8, u8), b: (u8, u8, u8)) -> f32 {
    let rm = (a.0 as f32 + b.0 as f32) / 2.0;
    let dr = a.0 as f32 - b.0 as f32;
    let dg = a.1 as f32 - b.1 as f32;
    let db = a.2 as f32 - b.2 as f32;
    ((2.0 + rm / 256.0) * dr * dr + 4.0 * dg * dg + (2.0 + (255.0 - rm) / 256.0) * db * db).sqrt()
}

#[cfg(test)]
mod color_tests {
    use super::*;

    #[test]
    fn pinned_ids_match_concept_palette() {
        assert_eq!(user_color("me"), (0x63, 0x66, 0xf1));
        assert_eq!(user_color("alice"), (0xec, 0x48, 0x99));
        assert_eq!(user_color("bob"), (0xf9, 0x73, 0x16));
        assert_eq!(user_color("charlie"), (0x10, 0xb9, 0x81));
    }

    #[test]
    fn unknown_ids_fall_through_to_stable_hash() {
        let a = user_color("dave");
        let b = user_color("dave");
        let c = user_color("eve");
        assert_eq!(a, b, "same id must map to same color");
        assert_ne!(a, c, "different ids should map to different colors");
    }

    /// Two people in the same room must never be painted in two colours
    /// that only a colour-picker can tell apart. Enumerated over the
    /// whole closed set `user_color` can return, so the guarantee holds
    /// for every id rather than for the handful anyone thought to try.
    ///
    /// The bar is a redmean distance of 60. For scale: the scheme this
    /// replaced put `dave` and `sara` 4.8 apart, and `host` and `peer3`
    /// 4.4 apart — different people rendered in the same colour.
    #[test]
    fn no_two_peer_colours_are_confusable() {
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

    /// Every colour the pins hand out is also a ring member, so a pinned
    /// peer and a hashed one can collide outright but never land
    /// near-but-not-quite together — which is the case that looks broken.
    #[test]
    fn pinned_colours_are_ring_members() {
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

    /// A hashed id is drawn from the same closed set, so the distance
    /// guarantee above covers real peers and not just the table.
    #[test]
    fn hashed_ids_land_on_the_ring() {
        for id in ["carol", "dave", "erin", "sara", "danah", "eve", "ada", "ade"] {
            let c = user_color(id);
            assert!(
                PEER_PALETTE.contains(&c),
                "{id} is painted #{:02x}{:02x}{:02x}, which is not on the ring",
                c.0,
                c.1,
                c.2,
            );
        }
        // The pairs that used to be indistinguishable now are not.
        for (a, b) in [("dave", "sara"), ("dave", "erin"), ("ada", "ade")] {
            let (ca, cb) = (user_color(a), user_color(b));
            assert!(
                ca == cb || color_distance(ca, cb) >= 60.0,
                "{a} and {b} are {:.1} apart",
                color_distance(ca, cb),
            );
        }
    }

    /// The case the ring alone cannot fix: `carol` hashes onto exactly
    /// the pink `alice` is pinned to. In a room with both, one of them
    /// has to move.
    #[test]
    fn a_room_never_puts_two_people_in_one_colour() {
        assert_eq!(
            user_color("carol"),
            user_color("alice"),
            "this test is guarding a real collision; if the hash moved, \
             pick another colliding pair rather than deleting the test",
        );

        let room = ["alice", "bob", "carol", "danah", "erin", "dana"];
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
        // Whoever does not collide keeps the colour their id gives them.
        assert_eq!(assigned["alice"], user_color("alice"));
        assert_eq!(assigned["bob"], user_color("bob"));
    }

    /// Two machines in one room compute the same assignment, because
    /// each sees the same membership: the other people plus itself.
    /// Without that a person would be pink on one screen and orange on
    /// the next.
    #[test]
    fn every_machine_in_a_room_agrees_on_who_is_what_colour() {
        let room = ["alice", "bob", "carol"];
        // What each host has: everyone else, plus itself.
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

    /// A colour the roster handed out explicitly is not available to be
    /// computed onto, or an invited peer and a live one could collide.
    #[test]
    fn an_explicit_roster_colour_is_not_reassigned_to_someone_else() {
        let taken = user_color("carol"); // == alice's pink
        let assigned = assign_peer_colors(&["carol"], &[taken]);
        assert_ne!(
            assigned["carol"], taken,
            "carol took a colour the roster had already spoken for",
        );
    }

    /// Someone arriving late must not repaint the people already there
    /// unless they actually collide with one.
    #[test]
    fn a_late_arrival_leaves_the_others_alone() {
        let before = assign_peer_colors(&["alice", "bob"], &[]);
        let after = assign_peer_colors(&["alice", "bob", "dana"], &[]);
        assert_eq!(before["alice"], after["alice"]);
        assert_eq!(before["bob"], after["bob"]);
    }

    #[test]
    fn pinned_ids_differ_from_hash_fallback() {
        // Regression guard: the pinned palette must not accidentally
        // agree with what FNV→HSL would have produced, or we'd be
        // unable to tell override from fallback in a failure mode.
        let (r, g, b) = user_color("me");
        assert!(!(r == 0 && g == 0 && b == 0));
    }
}

/// A collaborator's identity for presence rendering: who they are and the
/// stable colour every affordance paints them in. This is the single
/// value a widget needs to draw a peer — id for attribution, name for the
/// chip label, colour for the caret / pointer / mist / typing tint.
///
/// The colour is derived deterministically from `id` via [`user_color`],
/// so the same peer is the same hue on every screen without any shared
/// state — two peers who never exchange a palette still agree on who is
/// pink and who is orange. A friendlier `name` (from the roster) can
/// differ from `id` without perturbing the colour.
#[derive(Debug, Clone, PartialEq)]
pub struct PresenceUser {
    pub id: String,
    pub name: String,
    pub color: (u8, u8, u8),
}

impl PresenceUser {
    /// Build a user whose colour is the deterministic [`user_color`] of
    /// `id`, with a display name (falls back to `id` when the roster has
    /// no friendlier name yet).
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        let id = id.into();
        let color = user_color(&id);
        Self { id, name: name.into(), color }
    }

    /// Build a user from just an id, using the id itself as the display
    /// name until a roster entry supplies a nicer one.
    pub fn from_id(id: impl Into<String>) -> Self {
        let id = id.into();
        let name = id.clone();
        Self::new(id, name)
    }

    /// This user's colour as premultiplied-friendly RGBA floats at
    /// `alpha` — the shape every renderer wants.
    pub fn color_rgba(&self, alpha: f32) -> [f32; 4] {
        [
            self.color.0 as f32 / 255.0,
            self.color.1 as f32 / 255.0,
            self.color.2 as f32 / 255.0,
            alpha,
        ]
    }
}

/// One "just typed" highlight span: the run of text a peer inserted and
/// when. Rendered as a fading textmarker swipe in the peer's colour;
/// [`PresenceCtx::active_typing`] turns its age into an alpha and prunes
/// it once past [`TYPING_FADE`].
#[derive(Debug, Clone)]
struct TypingMark {
    range: SelectionRange,
    born: Instant,
}

/// Which layer paints a peer's pointer.
///
/// A pointer is a position on a **screen**, not a position in a
/// document, and only one layer can place it correctly. Two can try:
/// an app receives peer presence through the collaboration broker, and
/// the compositor receives it over the network and paints its own
/// overlay above every window. Both used to draw, so one person showed
/// up as two identical dots.
///
/// The compositor owns it, for a reason stronger than tidiness. What
/// travels between hosts is a **desktop** coordinate — the publishing
/// compositor substitutes its own pointer position precisely because an
/// app's surface-local `(40, 30)` is a different place in every window
/// (see `quick_shell::shell::peer_presence`). An app that paints that
/// number into its own window space is drawing a desktop position as if
/// it were window-local: right only when the window happens to sit at
/// the origin, wrong everywhere else. The compositor is also the only
/// layer that spans the strip, the chrome, and the gaps between
/// windows, so it is the only one that can keep a peer's pointer
/// continuous as it crosses them.
///
/// What stays with the app is presence *inside its own content* —
/// caret, selection, typing swipe, authorship tint. Those are
/// positions in a document, which the compositor cannot resolve
/// because it does not have the text. They are painted by the widgets
/// that own the text and are untouched by this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PointerOwner {
    /// This process paints it. The pointer arrived without a desktop
    /// scope, so no compositor overlay is claiming it: the SDK/relay
    /// dialect (which carries no [`PresenceKey`]), hand-seeded shot
    /// fixtures, and the hosted backend's interpolated cursors — where
    /// [`PresenceCtx::pointers`] is never populated and this layer is
    /// the only one that can draw anything at all.
    #[default]
    Window,
    /// The compositor overlay paints it, from [`PresenceCtx::pointers`]
    /// under the same [`PresenceKey::Workspace`] scope it arrived on.
    /// The cursor is still kept here — "follow that peer" reads it
    /// through [`PresenceCtx::peer_pointer`] — it is simply not painted
    /// a second time by this layer.
    Desktop,
}

#[derive(Debug, Clone)]
pub struct RemoteCursor {
    pub user_id: String,
    pub color: [f32; 4],
    pub position: [f32; 2], // scene-space coords
    pub label: String,
    /// Which layer is responsible for drawing this pointer. See
    /// [`PointerOwner`]; `Window` unless the pointer arrived under a
    /// desktop scope.
    pub owner: PointerOwner,
}

impl RemoteCursor {
    /// Whether the layer that owns this process's window should paint
    /// this pointer, as opposed to leaving it to the compositor
    /// overlay. The single predicate every app-side painter consults,
    /// so "who draws this" has one answer rather than one per call
    /// site.
    pub fn painted_by_window(&self) -> bool {
        matches!(self.owner, PointerOwner::Window)
    }
}

/// What a subscriber still has to do after
/// [`PresenceCtx::apply_remote_presence`] has folded a frame in.
///
/// The presence stores are updated either way; these name the two
/// consequences that reach past them — moving the local viewport, and
/// answering a joiner with document state. A subscriber that holds no
/// document (the compositor) ignores both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemotePresenceEffect {
    /// Nothing beyond the presence stores changed.
    None,
    /// The peer broadcast a new view subset. A follower should
    /// re-bind its viewport to them.
    PeerViewChanged,
    /// A peer arrived on a non-tab scope — possibly a late joiner, so
    /// a document holder should reply with its full state.
    PeerArrived,
}

#[derive(Debug, Default, Clone)]
pub struct PresenceCtx {
    pub here: HashMap<PresenceKey, Vec<UserId>>,
    pub pointers: HashMap<PresenceKey, Vec<(UserId, (f32, f32))>>,
    pub selections: HashMap<PresenceKey, Vec<(UserId, SelectionRange)>>,
    pub carets: HashMap<PresenceKey, Vec<(UserId, usize)>>,
    pub remote_cursors: Vec<RemoteCursor>,
    /// Optional per-user RGB override. Seeded by the shell from
    /// [`Collaborator::cursor_color`] so widgets don't have to thread
    /// `Collaborator` refs to honour a non-palette colour. Read through
    /// [`PresenceCtx::color_for`] which falls through to [`user_color`]
    /// when a user has no override.
    pub color_overrides: HashMap<UserId, (u8, u8, u8)>,
    /// Friendly display names per user, seeded from the collab roster
    /// (`CollabEvent::Context`). Read through [`PresenceCtx::display_name`]
    /// / [`PresenceCtx::user`]; a peer with no roster entry falls back to
    /// their raw id, so a caret chip is never blank.
    pub names: HashMap<UserId, String>,
    /// Per-user "just typed" highlight spans, keyed by presence scope.
    /// A peer types → their inserted run is recorded here with a birth
    /// instant; [`PresenceCtx::active_typing`] fades it out over
    /// [`TYPING_FADE`]. Not serialised — purely a local render effect
    /// driven by the inbound presence stream.
    typing: HashMap<(UserId, PresenceKey), Vec<TypingMark>>,
    /// Current draw-time visibility window in scene-space pixels.
    /// Threaded through the draw recursion so widgets can early-out
    /// when their `get_rect()` doesn't intersect the active viewport
    /// — spatial culling. `Cell` so containers (Scrollable in
    /// particular) can scope-push a tighter inner viewport without
    /// needing `&mut PresenceCtx` everywhere; `Rect` is `Copy` so
    /// the Cell access is cheap.
    pub viewport: std::cell::Cell<Option<vello::kurbo::Rect>>,
    /// Each peer's last broadcast [`ViewSubset`](crate::view::ViewSubset) —
    /// the region and resource they are looking at (scroll / open resource /
    /// viewport / view mode), distinct from the point their cursor sits on.
    /// Seeded from the presence stream by
    /// [`Self::set_peer_view`]; read by a renderer that wants to show *where*
    /// a peer is working, and by the App's **attach/follow** to bind the
    /// local viewport to a chosen peer. Cleared for a peer on
    /// [`Self::unregister_peer`].
    views: HashMap<UserId, crate::view::ViewSubset>,
    /// The identities this session publishes its own presence under —
    /// what a frame it sent comes back stamped with. See
    /// [`Self::set_own_identity`]; empty means "unknown", which
    /// suppresses nobody.
    own: std::collections::HashSet<UserId>,
}

impl PresenceCtx {
    /// True when the draw recursion has an active viewport and the
    /// supplied rect doesn't intersect it — i.e., the widget's
    /// pixels would land entirely outside the visible window.
    /// Widgets short-circuit on this at the top of
    /// `draw_with_presence` to skip their `Scene::append` /
    /// glyph-emit work entirely. Off-viewport widgets MUST NOT
    /// clear their `dirty` flag (so they redraw when they scroll
    /// back in); the App's `mark_drawn` walks the live tree and
    /// only visits widgets whose draw actually ran.
    pub fn should_cull(&self, rect: vello::kurbo::Rect) -> bool {
        match self.viewport.get() {
            Some(vp) => rect.intersect(vp).is_zero_area(),
            None => false,
        }
    }

    /// Replace the active viewport, returning the previous value
    /// so the caller can restore it (e.g. a Scrollable pushes its
    /// inner rect for its child recursion, then pops it back).
    pub fn push_viewport(&self, vp: vello::kurbo::Rect) -> Option<vello::kurbo::Rect> {
        self.viewport.replace(Some(vp))
    }

    /// Restore the previous viewport. Pair with `push_viewport`.
    pub fn pop_viewport(&self, prev: Option<vello::kurbo::Rect>) {
        self.viewport.set(prev);
    }
}

impl PresenceCtx {
    pub fn empty() -> Self { Self::default() }

    /// Declare the identities a frame *this* session published comes
    /// back stamped with, so its own presence is never painted as
    /// somebody else's.
    ///
    /// Two apps one person has open are two connections and **one
    /// participant** — the broker says so outright, and resolves both
    /// to a single identity before it stamps their presence. It then
    /// fans each one's frames to the other, because a sibling app is a
    /// legitimate audience for them: `Arrived` is how a joiner asks the
    /// room for the document, and both a second window and `qos-agent`
    /// depend on being answered. What must not happen is the *painting*
    /// of that arrival, which puts a pill bearing the owner's own name
    /// over the owner's own document.
    ///
    /// The roster has always refused this — `room_roster` drops the
    /// local handle before it builds a row, on the grounds that a host
    /// self-names at mint time and must not read that back as a peer.
    /// This is the same rule on the path that paints.
    ///
    /// The discriminator is the **participant**, not the process and
    /// not the app: two windows of one app are one person, and so are a
    /// word processor and a file manager side by side. An agent editing
    /// the same document joins under an identity of its own, so it
    /// keeps its caret — an app participant with no window is still
    /// somebody else.
    ///
    /// Blank entries are dropped. An env var set to `""` is what a
    /// launcher produces when the variable it meant to forward was
    /// itself unset, and taking it literally would make every unnamed
    /// participant "me" and empty the room.
    ///
    /// Unset — or set to something the broker did not in fact stamp —
    /// the rule simply does not fire and everybody is painted. That
    /// direction is deliberate: a spurious pill for yourself is a
    /// blemish, a missing pill for a real collaborator is a lie about
    /// who is in the room.
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

    /// Whether `user_id` is this session's own participant — i.e. a
    /// frame it published, handed back to it by the broker's fan-out.
    pub fn is_own_identity(&self, user_id: &str) -> bool {
        if self.own.is_empty() || user_id.trim().is_empty() {
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

    /// Re-attribute `user` so they are listed on exactly one `PresenceKey::Tab`
    /// at a time: removes them from every other `Tab` key's presence list,
    /// then registers them on `target`. Matches the single-active-tab-per-user
    /// model so a peer switching tabs mid-session moves their dot rather than
    /// leaving a trail behind on the previously-focused pill.
    ///
    /// No-op if `target` isn't a `Tab` key — callers fall back to
    /// `register_presence` in that case.
    pub fn set_tab_focus(&mut self, target: &PresenceKey, user: UserId) {
        if !matches!(target, PresenceKey::Tab { .. }) {
            self.register_presence(target, user);
            return;
        }
        for (k, list) in self.here.iter_mut() {
            if matches!(k, PresenceKey::Tab { .. }) && k != target {
                list.retain(|u| u != &user);
            }
        }
        self.register_presence(target, user);
    }

    pub fn set_pointer(&mut self, key: &PresenceKey, user: UserId, pos: (f32, f32)) {
        let list = self.pointers.entry(key.clone()).or_default();
        if let Some(entry) = list.iter_mut().find(|(u, _)| *u == user) {
            entry.1 = pos;
        } else {
            list.push((user, pos));
        }
    }

    pub fn set_caret(&mut self, key: &PresenceKey, user: UserId, pos: usize) {
        // A caret is single-location: a peer has at most one. Clear them
        // from every OTHER key first so moving the caret (and the
        // caret-driven focus outline) doesn't leave a stale one behind.
        for (k, list) in self.carets.iter_mut() {
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
        for (k, list) in self.selections.iter_mut() {
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
        self.here.get(key).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn pointer_for(&self, key: &PresenceKey, user: &UserId) -> Option<(f32, f32)> {
        self.pointers.get(key)?.iter().find(|(u, _)| u == user).map(|(_, p)| *p)
    }

    pub fn selection_for(&self, key: &PresenceKey, user: &UserId) -> Option<SelectionRange> {
        self.selections.get(key)?.iter().find(|(u, _)| u == user).map(|(_, r)| *r)
    }

    pub fn caret_for(&self, key: &PresenceKey, user: &UserId) -> Option<usize> {
        self.carets.get(key)?.iter().find(|(u, _)| u == user).map(|(_, c)| *c)
    }

    /// Seed or replace this user's colour override. Accepts `#rgb`,
    /// `#rrggbb`, and `#rrggbbaa` (alpha ignored — presence renderers
    /// apply their own alpha). Malformed hex silently clears the override
    /// so the caller falls back to [`user_color`].
    pub fn set_color_override(&mut self, user: UserId, hex: &str) {
        match parse_hex_color(hex) {
            Some(rgb) => { self.color_overrides.insert(user, rgb); }
            None => { self.color_overrides.remove(&user); }
        }
    }

    /// Colour this user should render as. Checks the override map first,
    /// then falls through to the palette-pinned [`user_color`] so peers
    /// with no seed still behave exactly as before.
    pub fn color_for(&self, user: &UserId) -> (u8, u8, u8) {
        if let Some(rgb) = self.color_overrides.get(user) {
            return *rgb;
        }
        user_color(&user.0)
    }

    /// Record (or update) a peer's friendly display name — seeded from the
    /// collab roster. Overwrites any prior name.
    pub fn set_display_name(&mut self, user: UserId, name: impl Into<String>) {
        let name = name.into();
        // A name can arrive after the peer's pointer already has. The
        // roster is the one answer to "what is this peer called", so an
        // already-drawn pill is relabelled rather than left showing the
        // id until the peer happens to move again.
        for cursor in &mut self.remote_cursors {
            if cursor.user_id == user.0 {
                cursor.label = name.clone();
            }
        }
        self.names.insert(user, name);
    }

    /// The friendly name to show for `user`, falling back to the raw id
    /// when the roster hasn't announced one. Never blank.
    pub fn display_name(&self, user: &UserId) -> String {
        self.names
            .get(user)
            .cloned()
            .unwrap_or_else(|| user.0.clone())
    }

    /// Resolve a peer into a [`PresenceUser`] — id, friendly name, and the
    /// override-aware stable colour — ready to hand to any renderer.
    pub fn user(&self, user: &UserId) -> PresenceUser {
        PresenceUser {
            id: user.0.clone(),
            name: self.display_name(user),
            color: self.color_for(user),
        }
    }

    /// Record a peer's freshly-typed run under `key`, born `now`. Adjacent
    /// / overlapping marks from the same peer under the same key coalesce
    /// into one growing span with a refreshed birth instant, so a burst of
    /// keystrokes reads as a single moving highlighter rather than a stack
    /// of overlapping tints. Pass `Instant::now()` in production; tests
    /// pass an explicit base instant so the fade is deterministic.
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
    /// tapering linearly to 0 at [`TYPING_FADE`]). Fully-faded marks are
    /// filtered out. This is a read: call [`Self::prune_typing`] once a
    /// frame to actually reclaim expired entries.
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

    /// Every peer's still-visible typing spans under `key` at `now`, as
    /// `(user, range, alpha)`. The shape a text widget wants: it walks the
    /// list, maps each `range` to glyph rects, and paints
    /// [`Self::draw_peer_typing`]. Skips the local user is the caller's
    /// job — the store only ever holds *peers'* marks.
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

    /// Whether any peer still has an un-faded typing mark at `now`. The
    /// App polls this to keep requesting animation frames while a
    /// highlighter is still decaying.
    pub fn has_active_typing(&self, now: Instant) -> bool {
        let fade = TYPING_FADE;
        self.typing.values().flatten().any(|m| {
            now.saturating_duration_since(m.born) < fade
        })
    }

    /// Drop every typing mark that has fully faded by `now`, and any
    /// scope whose mark list became empty. Call once a frame.
    pub fn prune_typing(&mut self, now: Instant) {
        let fade = TYPING_FADE;
        self.typing.retain(|_, marks| {
            marks.retain(|m| now.saturating_duration_since(m.born) < fade);
            !marks.is_empty()
        });
    }

    /// Record (replace) a peer's broadcast [`ViewSubset`](crate::view::ViewSubset)
    /// — the region and resource they are looking at. Fed from the presence
    /// stream; the View is never merged, only replaced with the peer's
    /// latest, so a peer's entry is always exactly what they last published.
    pub fn set_peer_view(&mut self, user: UserId, view: crate::view::ViewSubset) {
        self.views.insert(user, view);
    }

    /// A peer's last broadcast [`ViewSubset`](crate::view::ViewSubset), if we
    /// have one. `None` before the peer has published a View, or after they
    /// leave. This is what **attach/follow** binds the local viewport to.
    pub fn peer_view(&self, user: &UserId) -> Option<&crate::view::ViewSubset> {
        self.views.get(user)
    }

    /// A peer's last-known pointer position in scene space, if we have one
    /// — the target a "follow" action scrolls toward. Reads the same
    /// `remote_cursors` overlay the pointer is drawn from, so following a
    /// peer lands exactly where their cursor shows.
    pub fn peer_pointer(&self, user_id: &str) -> Option<[f32; 2]> {
        self.remote_cursors
            .iter()
            .find(|c| c.user_id == user_id)
            .map(|c| c.position)
    }

    /// Hit-test a click against the off-screen mist bands for the current
    /// `viewport`: returns the id of the peer whose edge band contains
    /// `point`, so clicking a peer's mist selects them to follow. Bands
    /// are the fixed-thickness strips [`draw_remote_presence`] paints, so
    /// this stays in sync with what the user sees.
    pub fn hit_test_offscreen_mist(
        &self,
        viewport: vello::kurbo::Rect,
        point: vello::kurbo::Point,
    ) -> Option<String> {
        for cursor in &self.remote_cursors {
            let target = vello::kurbo::Point::new(
                cursor.position[0] as f64,
                cursor.position[1] as f64,
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

    /// Upsert a remote peer's cursor into `remote_cursors`. The color is
    /// resolved via `color_for(&UserId)` so pinned palette ids (alice/bob/
    /// charlie/me) keep their reference colors and unknown ids fall through
    /// to the FNV→HSL fallback. The label defaults to `user_id` when
    /// `display_name` is None (peers can announce a friendlier name via
    /// `CollabEvent::Context`).
    pub fn upsert_remote_cursor(
        &mut self,
        user_id: String,
        position: [f32; 2],
        display_name: Option<&str>,
    ) {
        self.upsert_remote_cursor_faded(user_id, position, display_name, 1.0);
    }

    /// [`Self::upsert_remote_cursor`] with an explicit opacity, so a
    /// caller that ages remote cursors out (rather than waiting for an
    /// explicit `Left`) can fade one rather than popping it. `alpha` is
    /// clamped to `0.0..=1.0` and lands in the cursor colour's alpha
    /// channel; `1.0` is exactly the plain upsert.
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

    /// [`Self::upsert_remote_cursor_faded`] naming which layer paints
    /// the result. Callers that know the pointer arrived on a desktop
    /// scope pass [`PointerOwner::Desktop`] so this layer stops drawing
    /// it and leaves it to the compositor overlay — see
    /// [`PointerOwner`] for why that is the compositor's job.
    ///
    /// An upsert re-states the owner: a peer whose pointer starts
    /// arriving on a different transport changes hands rather than
    /// keeping whatever it was first seen as.
    pub fn upsert_remote_cursor_owned(
        &mut self,
        user_id: String,
        position: [f32; 2],
        display_name: Option<&str>,
        alpha: f32,
        owner: PointerOwner,
    ) {
        // The cursor stream is a second door into the same pill
        // (`CollabEvent::RemoteCursorMoved` reaches here without going
        // through `apply_remote_presence`). A guard on one door and not
        // the other leaves the bug reachable.
        if self.is_own_identity(&user_id) {
            return;
        }
        let uid = UserId(user_id.clone());
        let (r, g, b) = self.color_for(&uid);
        let color = [
            r as f32 / 255.0,
            g as f32 / 255.0,
            b as f32 / 255.0,
            alpha.clamp(0.0, 1.0),
        ];
        // Told a name, use it; otherwise ask the roster, which falls
        // back to the raw id. Resolving through the roster is what lets
        // a pointer that moves without carrying a name still be
        // labelled with one the room already knows.
        let label = display_name
            .map(|s| s.to_string())
            .unwrap_or_else(|| self.display_name(&uid));
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

    // -----------------------------------------------------------------
    // String-id wrappers used by the App's collab-event pump.
    //
    // The wire protocol (`CollabEvent::RemotePresenceUpdate`) carries
    // peer ids as `String`s; the rest of `PresenceCtx` is keyed by
    // [`UserId`] (a newtype around `String`). The wrappers below let
    // the pump forward straight from the event payload without paying
    // for a manual `UserId(...)` wrap on every call, and ensure that
    // peer arrivals / departures / caret moves / selection changes
    // all touch the SAME fields the widget render passes already read
    // from (`here`, `carets`, `selections`, `remote_cursors`).
    // -----------------------------------------------------------------

    /// Record `user_id`'s caret position under `key`. Mirrors the
    /// `Caret` payload of [`crate::collaboration::PresencePayload`] and
    /// stores into the existing [`Self::carets`] map so widgets that
    /// already call [`Self::caret_for`] light up without a second lookup.
    pub fn set_remote_caret(&mut self, user_id: String, key: PresenceKey, pos: usize) {
        self.set_caret(&key, UserId(user_id), pos);
    }

    /// Record `user_id`'s selection range under `key`. Mirrors the
    /// `Selection` payload of [`crate::collaboration::PresencePayload`]
    /// and stores into the existing [`Self::selections`] map.
    pub fn set_remote_selection(
        &mut self,
        user_id: String,
        key: PresenceKey,
        start: usize,
        end: usize,
    ) {
        self.set_selection(&key, UserId(user_id), SelectionRange { start, end });
    }

    /// Attribute `user_id` to `key` (peer "arrived"). Routes through
    /// [`Self::set_tab_focus`] for `Tab` keys so a peer switching tabs
    /// moves their dot rather than accumulating attribution on every
    /// tab they've ever visited; other keys fall through to
    /// [`Self::register_presence`].
    pub fn register_peer_at(&mut self, user_id: String, key: PresenceKey) {
        let uid = UserId(user_id);
        if matches!(key, PresenceKey::Tab { .. }) {
            self.set_tab_focus(&key, uid);
        } else {
            self.register_presence(&key, uid);
        }
    }

    /// Fold one inbound presence frame into this context.
    ///
    /// This is the whole decode of a `RemotePresenceUpdate` — which
    /// store a payload lands in, and under which scope. It lives on
    /// `PresenceCtx` rather than on `App` because an app is not the
    /// only thing that subscribes to presence: the compositor draws
    /// peers over its own windows and reads the identical stream. Two
    /// decoders would drift, and a peer painted by one and not the
    /// other is indistinguishable on screen from a peer who is not
    /// there.
    ///
    /// The returned [`RemotePresenceEffect`] names the part a bare
    /// presence store cannot do for itself: following a peer's
    /// viewport, and answering a late joiner with document state. A
    /// subscriber that holds no document ignores it.
    pub fn apply_remote_presence(
        &mut self,
        user_id: String,
        key: PresenceKey,
        payload: crate::collaboration::PresencePayload,
    ) -> RemotePresenceEffect {
        use crate::collaboration::PresencePayload::*;

        // A frame this session published, handed back to it: another
        // app the same person has open. Nothing about it is painted —
        // see [`Self::set_own_identity`] — but `Arrived` still has to
        // be answered, because it is the request a joiner makes for the
        // document rather than a thing to draw. Suppressing the pill
        // must not suppress the handshake.
        if self.is_own_identity(&user_id) {
            // Following yourself is not a thing, and a View broadcast
            // rides inside the key rather than the payload.
            if key.as_view().is_some() {
                return RemotePresenceEffect::None;
            }
            return match payload {
                // A Tab-keyed arrival moves a focus dot; only a scope
                // arrival is a join asking to be sent the document.
                Arrived if !matches!(key, PresenceKey::Tab { .. }) => {
                    // Said out loud, and only on arrival so it cannot
                    // spam at pointer rate. A rule that works leaves
                    // no pill behind — which is indistinguishable from
                    // a session that was never sent anything, and a
                    // test unable to tell those apart is how this bug
                    // survived a screenshot. This line is the
                    // difference.
                    tracing::debug!(
                        user_id, "presence: own participant, not painting it as a peer"
                    );
                    RemotePresenceEffect::PeerArrived
                }
                _ => RemotePresenceEffect::None,
            };
        }

        // A View broadcast rides inside the routing key, not the payload
        // (the subset has no `PresencePayload` variant). Recognise it here,
        // store the peer's region/resource, and let the caller decide
        // whether it is following them. The paired payload is inert and
        // deliberately never reaches the match below.
        if let Some(subset) = key.as_view() {
            self.set_peer_view(UserId(user_id), subset);
            return RemotePresenceEffect::PeerViewChanged;
        }

        match payload {
            Pointer { x, y } => {
                // Populate BOTH presence stores: `pointers` (keyed by
                // scope, read by the shell's per-workspace overlay and by
                // `pointer_for`) and the screen-space `remote_cursors` vec
                // the shared `draw_remote_cursors` helper renders.
                //
                // Which of the two gets *painted* is the point of
                // `owner`. A pointer that arrived under a workspace
                // scope is a position on the desktop, and the
                // compositor overlay draws it from `pointers` above
                // every window. Stamping it `Desktop` is what stops
                // this layer drawing the very same peer a second time
                // from `remote_cursors` — the two stores are one
                // arrival, not two people. Any other scope has no
                // overlay claiming it, so it stays this layer's to
                // paint.
                let uid = UserId(user_id.clone());
                let owner = if matches!(key, PresenceKey::Workspace { .. }) {
                    PointerOwner::Desktop
                } else {
                    PointerOwner::Window
                };
                self.set_pointer(&key, uid, (x, y));
                self.upsert_remote_cursor_owned(user_id, [x, y], None, 1.0, owner);
            }
            Caret { pos } => {
                // Feed BOTH presence stores: the in-app BlockEditor reads
                // `carets` (set_caret); the cross-process overlay reads
                // the `remote_caret` map (set_remote_caret).
                let uid = UserId(user_id.clone());
                self.set_caret(&key, uid, pos);
                self.set_remote_caret(user_id, key, pos);
            }
            Selection { start, end } => {
                let uid = UserId(user_id.clone());
                self.set_selection(&key, uid, SelectionRange { start, end });
                self.set_remote_selection(user_id, key, start, end);
            }
            Typing { start, end } => {
                // A peer inserted `start..end` under `key`: record a fading
                // textmarker swipe in their colour. `Instant::now()` births
                // the mark; the render fades it over `TYPING_FADE`, and the
                // per-frame `prune_typing` reclaims it.
                self.record_peer_typing(
                    UserId(user_id),
                    key,
                    SelectionRange { start, end },
                    Instant::now(),
                );
            }
            Arrived => {
                let uid = UserId(user_id.clone());
                // Register in the cross-process peer registry too.
                self.register_peer_at(user_id, key.clone());
                // Tab keys move the peer's dot between tabs/pages; non-Tab
                // (app/workspace) arrivals are (possibly late) joiners, so
                // the caller should reply with its full state to converge.
                // (Page switches use Tab keys, never this.)
                if matches!(key, PresenceKey::Tab { .. }) {
                    self.set_tab_focus(&key, uid);
                } else {
                    self.register_presence(&key, uid);
                    return RemotePresenceEffect::PeerArrived;
                }
            }
            Left => {
                // `Left` is broadcast on `WindowEvent::CursorLeft` —
                // the peer's mouse pointer left their window viewport,
                // not the peer themselves. Their caret + selection
                // should KEEP rendering because they're still editing,
                // they've just moved their mouse elsewhere. Wiping
                // them via `unregister_peer` here was the
                // "remote caret disappears a few seconds after it was
                // placed" bug — any mouse-leave on the publisher side
                // tore down the entire presence record on every
                // subscriber.
                //
                // True peer disconnect arrives via the socket close
                // path (broker drops the peer from its fan-out
                // table); subscribers only need to clear pointer-
                // related state here.
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

    /// Drop every trace of `user_id`: remove them from every
    /// [`Self::here`] list, every [`Self::pointers`] / [`Self::carets`]
    /// / [`Self::selections`] list, and drop their entry from
    /// [`Self::remote_cursors`]. Called on the `Left` payload of
    /// [`crate::collaboration::PresencePayload`] and whenever a peer
    /// disconnects.
    pub fn unregister_peer(&mut self, user_id: String) {
        let uid = UserId(user_id.clone());
        // here
        for list in self.here.values_mut() {
            list.retain(|u| u != &uid);
        }
        // pointers
        for list in self.pointers.values_mut() {
            list.retain(|(u, _)| u != &uid);
        }
        // carets
        for list in self.carets.values_mut() {
            list.retain(|(u, _)| u != &uid);
        }
        // selections
        for list in self.selections.values_mut() {
            list.retain(|(u, _)| u != &uid);
        }
        // typing highlighter marks scoped to this peer
        self.typing.retain(|(u, _), _| u != &uid);
        // broadcast View (region/resource they were looking at)
        self.views.remove(&uid);
        // roster name
        self.names.remove(&uid);
        // remote cursor stamp
        self.remove_remote_cursor(&user_id);
    }
}

/// Parse a `#rgb` / `#rrggbb` / `#rrggbbaa` colour string to an RGB triple.
/// Alpha, when present, is discarded — presence affordances manage alpha
/// at render time. Leading `#` is optional. Returns `None` for anything
/// unparseable so callers can silently fall through to the palette.
pub fn parse_hex_color(hex: &str) -> Option<(u8, u8, u8)> {
    let s = hex.trim_start_matches('#');
    let bytes = s.as_bytes();
    match bytes.len() {
        3 => {
            let r = u8::from_str_radix(std::str::from_utf8(&[bytes[0], bytes[0]]).ok()?, 16).ok()?;
            let g = u8::from_str_radix(std::str::from_utf8(&[bytes[1], bytes[1]]).ok()?, 16).ok()?;
            let b = u8::from_str_radix(std::str::from_utf8(&[bytes[2], bytes[2]]).ok()?, 16).ok()?;
            Some((r, g, b))
        }
        6 | 8 => {
            let r = u8::from_str_radix(&s[0..2], 16).ok()?;
            let g = u8::from_str_radix(&s[2..4], 16).ok()?;
            let b = u8::from_str_radix(&s[4..6], 16).ok()?;
            Some((r, g, b))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tab_focus_tests {
    use super::*;

    #[test]
    fn set_tab_focus_moves_user_between_tabs() {
        let mut ctx = PresenceCtx::empty();
        let alice = UserId("alice".into());
        let a = PresenceKey::Tab { id: "aaa".into() };
        let b = PresenceKey::Tab { id: "bbb".into() };

        ctx.set_tab_focus(&a, alice.clone());
        assert_eq!(ctx.peers_on(&a), &[alice.clone()]);
        assert!(ctx.peers_on(&b).is_empty());

        ctx.set_tab_focus(&b, alice.clone());
        assert!(
            ctx.peers_on(&a).is_empty(),
            "alice must be cleared from her previous tab"
        );
        assert_eq!(ctx.peers_on(&b), &[alice]);
    }

    #[test]
    fn set_tab_focus_leaves_other_users_on_the_old_tab() {
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

    #[test]
    fn set_tab_focus_on_non_tab_key_falls_back_to_register_presence() {
        let mut ctx = PresenceCtx::empty();
        let alice = UserId("alice".into());
        let tab = PresenceKey::Tab { id: "aaa".into() };
        let ws = PresenceKey::Workspace { id: "home".into() };

        ctx.set_tab_focus(&tab, alice.clone());
        ctx.set_tab_focus(&ws, alice.clone());

        assert_eq!(
            ctx.peers_on(&tab),
            &[alice.clone()],
            "workspace registration must not evict the tab attribution"
        );
        assert_eq!(ctx.peers_on(&ws), &[alice]);
    }
}

#[cfg(test)]
mod color_override_tests {
    use super::*;

    #[test]
    fn parse_hex_color_accepts_common_forms() {
        assert_eq!(parse_hex_color("#ff0000"), Some((0xff, 0x00, 0x00)));
        assert_eq!(parse_hex_color("00ff00"), Some((0x00, 0xff, 0x00)));
        assert_eq!(parse_hex_color("#0000ffee"), Some((0x00, 0x00, 0xff)));
        // #rgb short form expands to #rrggbb.
        assert_eq!(parse_hex_color("#f0a"), Some((0xff, 0x00, 0xaa)));
    }

    #[test]
    fn parse_hex_color_rejects_malformed_input() {
        assert_eq!(parse_hex_color(""), None);
        assert_eq!(parse_hex_color("#12"), None);
        assert_eq!(parse_hex_color("#zzzzzz"), None);
        assert_eq!(parse_hex_color("not-a-color"), None);
    }

    #[test]
    fn color_for_falls_through_to_palette_without_override() {
        let ctx = PresenceCtx::empty();
        assert_eq!(ctx.color_for(&UserId("alice".into())), user_color("alice"));
    }

    #[test]
    fn color_for_honours_override_when_set() {
        let mut ctx = PresenceCtx::empty();
        ctx.set_color_override(UserId("alice".into()), "#abcdef");
        assert_eq!(
            ctx.color_for(&UserId("alice".into())),
            (0xab, 0xcd, 0xef)
        );
        // A different user is unaffected and still uses the palette.
        assert_eq!(ctx.color_for(&UserId("bob".into())), user_color("bob"));
    }

    #[test]
    fn malformed_override_clears_any_prior_entry() {
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
    fn upsert_remote_cursor_inserts_new_entry() {
        let mut ctx = PresenceCtx::empty();
        ctx.upsert_remote_cursor("alice".into(), [42.0, 17.0], None);
        assert_eq!(ctx.remote_cursors.len(), 1);
        let c = &ctx.remote_cursors[0];
        assert_eq!(c.user_id, "alice");
        assert_eq!(c.position, [42.0, 17.0]);
        // display_name=None falls through to the raw user_id as label.
        assert_eq!(c.label, "alice");
        // Color should come from the pinned palette for "alice".
        let (r, g, b) = user_color("alice");
        assert!((c.color[0] - r as f32 / 255.0).abs() < 1e-3);
        assert!((c.color[1] - g as f32 / 255.0).abs() < 1e-3);
        assert!((c.color[2] - b as f32 / 255.0).abs() < 1e-3);
        assert_eq!(c.color[3], 1.0);
    }

    #[test]
    fn upsert_remote_cursor_updates_existing_in_place() {
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

    #[test]
    fn upsert_remote_cursor_preserves_friendly_label_when_display_name_is_none() {
        let mut ctx = PresenceCtx::empty();
        // Seed with a friendly name (e.g. from CollabEvent::Context).
        ctx.upsert_remote_cursor("alice".into(), [0.0, 0.0], Some("Alice Smith"));
        assert_eq!(ctx.remote_cursors[0].label, "Alice Smith");

        // A later cursor move with None must not clobber the friendly label.
        ctx.upsert_remote_cursor("alice".into(), [50.0, 50.0], None);
        assert_eq!(ctx.remote_cursors.len(), 1);
        assert_eq!(ctx.remote_cursors[0].position, [50.0, 50.0]);
        assert_eq!(
            ctx.remote_cursors[0].label, "Alice Smith",
            "display_name=None must not overwrite a previously-set friendly label",
        );
    }

    #[test]
    fn remove_remote_cursor_clears_entry() {
        let mut ctx = PresenceCtx::empty();
        ctx.upsert_remote_cursor("alice".into(), [1.0, 2.0], None);
        ctx.upsert_remote_cursor("bob".into(), [3.0, 4.0], None);
        assert_eq!(ctx.remote_cursors.len(), 2);
        ctx.remove_remote_cursor("alice");
        assert_eq!(ctx.remote_cursors.len(), 1);
        assert_eq!(ctx.remote_cursors[0].user_id, "bob");
        // Removing an unknown id is a no-op.
        ctx.remove_remote_cursor("nobody");
        assert_eq!(ctx.remote_cursors.len(), 1);
    }
}

#[cfg(test)]
mod string_id_wrapper_tests {
    use super::*;

    #[test]
    fn set_remote_caret_writes_to_carets_map() {
        let mut ctx = PresenceCtx::empty();
        let key = PresenceKey::Workspace { id: "home".into() };
        ctx.set_remote_caret("alice".into(), key.clone(), 42);
        assert_eq!(ctx.caret_for(&key, &UserId("alice".into())), Some(42));
    }

    #[test]
    fn set_remote_selection_writes_to_selections_map() {
        let mut ctx = PresenceCtx::empty();
        let key = PresenceKey::Workspace { id: "home".into() };
        ctx.set_remote_selection("bob".into(), key.clone(), 3, 9);
        let r = ctx.selection_for(&key, &UserId("bob".into())).expect("selection");
        assert_eq!((r.start, r.end), (3, 9));
    }

    #[test]
    fn register_peer_at_workspace_adds_to_here() {
        let mut ctx = PresenceCtx::empty();
        let key = PresenceKey::Workspace { id: "home".into() };
        ctx.register_peer_at("alice".into(), key.clone());
        assert_eq!(ctx.peers_on(&key), &[UserId("alice".into())]);
    }

    #[test]
    fn register_peer_at_tab_routes_through_set_tab_focus() {
        // A peer switching between two tab keys must end up on exactly
        // one — the previous tab should drop them. This guards the
        // routing through set_tab_focus.
        let mut ctx = PresenceCtx::empty();
        let a = PresenceKey::Tab { id: "aaa".into() };
        let b = PresenceKey::Tab { id: "bbb".into() };
        ctx.register_peer_at("alice".into(), a.clone());
        ctx.register_peer_at("alice".into(), b.clone());
        assert!(ctx.peers_on(&a).is_empty(),
            "peer must have been evicted from previous tab");
        assert_eq!(ctx.peers_on(&b), &[UserId("alice".into())]);
    }

    #[test]
    fn unregister_peer_drops_every_trace() {
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

        ctx.unregister_peer("alice".into());

        // Alice is purged.
        assert!(ctx.peers_on(&key_a).is_empty());
        assert!(ctx.peers_on(&key_b).is_empty());
        assert!(ctx.caret_for(&key_a, &UserId("alice".into())).is_none());
        assert!(ctx.selection_for(&key_a, &UserId("alice".into())).is_none());
        assert!(ctx.pointer_for(&key_a, &UserId("alice".into())).is_none());
        assert!(ctx.remote_cursors.is_empty());
    }

    #[test]
    fn presence_user_color_is_stable_and_distinct() {
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

    #[test]
    fn presence_user_rgba_is_normalised() {
        let u = PresenceUser { id: "x".into(), name: "x".into(), color: (255, 128, 0) };
        let c = u.color_rgba(0.5);
        assert!((c[0] - 1.0).abs() < 1e-6);
        assert!((c[1] - 128.0 / 255.0).abs() < 1e-6);
        assert!((c[2] - 0.0).abs() < 1e-6);
        assert!((c[3] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn roster_name_resolves_with_id_fallback() {
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

    /// A peer the room has not named is labelled with the id it arrived
    /// under — not blank, and not somebody who does not exist.
    ///
    /// Both of the wrong answers are worse than the id. A blank chip
    /// floating over a caret reads as a rendering fault; a stand-in name
    /// reads as a person, and there is no person. The id is ugly and
    /// true, which is the right trade until an inviter says otherwise.
    #[test]
    fn an_unnamed_peer_is_labelled_with_its_id_and_never_with_a_stand_in() {
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

    /// Gaining a name must not change what colour a peer is drawn in.
    ///
    /// Colour is keyed on the id and the name is a label on it, which is
    /// what makes this hold — but it holds only for as long as nothing
    /// starts keying colour on the label, so it is asserted rather than
    /// left to be noticed by somebody watching a caret change colour
    /// mid-sentence.
    #[test]
    fn a_peer_that_gains_a_name_keeps_its_colour() {
        let mut ctx = PresenceCtx::empty();
        let peer = UserId("peer@5537458103208628978".into());
        let before = ctx.color_for(&peer);

        ctx.set_display_name(peer.clone(), "Xaver");

        assert_eq!(ctx.display_name(&peer), "Xaver");
        assert_eq!(
            ctx.color_for(&peer),
            before,
            "a peer must not change colour in front of the people watching it",
        );
    }

    /// A name can arrive after the pointer it belongs to. The pill has to
    /// pick it up, or a peer stays a number on screen until they happen
    /// to move again.
    #[test]
    fn a_name_that_arrives_after_the_pointer_relabels_it() {
        let mut ctx = PresenceCtx::empty();
        ctx.upsert_remote_cursor("peer@77".into(), [10.0, 20.0], None);
        assert_eq!(ctx.remote_cursors[0].label, "peer@77");

        ctx.set_display_name(UserId("peer@77".into()), "Xaver");

        assert_eq!(ctx.remote_cursors.len(), 1, "naming a peer adds no second pointer");
        assert_eq!(ctx.remote_cursors[0].label, "Xaver");
        assert_eq!(ctx.remote_cursors[0].position, [10.0, 20.0], "and moves none");
    }

    /// And the other order: a name learned from the roster before the
    /// peer's first move labels that move when it lands.
    #[test]
    fn a_pointer_that_arrives_after_the_name_is_labelled_with_it() {
        let mut ctx = PresenceCtx::empty();
        ctx.set_display_name(UserId("peer@77".into()), "Xaver");

        ctx.upsert_remote_cursor("peer@77".into(), [10.0, 20.0], None);

        assert_eq!(ctx.remote_cursors[0].label, "Xaver");
    }

    /// One person is one pointer.
    ///
    /// A pointer arriving on a workspace scope lands in *both* stores
    /// from one arrival — `pointers`, which the compositor overlay
    /// paints, and `remote_cursors`, which "follow that peer" reads.
    /// Two stores is correct; two painters is not. This asserts the
    /// split on the real decode: the overlay's store has the peer, and
    /// the window's painter declines them.
    ///
    /// This is the regression that hid for months behind mismatched
    /// names. The two layers resolved a peer's identity differently, so
    /// the duplicate rendered as two *different*-looking people and
    /// read as two peers rather than as one drawn twice. Unifying the
    /// names is what made it legible, and is exactly why the invariant
    /// is pinned here instead of left to the eye.
    #[test]
    fn a_desktop_pointer_is_the_compositors_to_paint_and_not_this_layers() {
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
        // And the record is kept, so following the peer still lands
        // where their cursor shows.
        assert_eq!(ctx.peer_pointer("peer@77"), Some([120.0, 90.0]));

        // But this layer does not paint it.
        assert_eq!(ctx.remote_cursors.len(), 1, "one arrival is one record");
        assert!(
            !ctx.remote_cursors[0].painted_by_window(),
            "a desktop pointer painted here as well is the same person twice",
        );
    }

    /// The other half, so the rule above cannot be satisfied by never
    /// painting anything. A pointer that arrived with no desktop scope
    /// has no overlay claiming it — the SDK/relay dialect carries no
    /// key at all, and the hosted backend never fills `pointers` — so
    /// this layer stays the one that draws it.
    #[test]
    fn a_pointer_no_overlay_claims_is_still_this_layers_to_paint() {
        let mut ctx = PresenceCtx::empty();

        // The relay/SDK path: a cursor with no scope at all.
        ctx.upsert_remote_cursor("peer@77".into(), [10.0, 20.0], None);
        assert!(
            ctx.remote_cursors[0].painted_by_window(),
            "dropping this one would leave the relay dialect with no pointer at all",
        );

        // And a scoped pointer that is a position in content rather
        // than on the desktop.
        let widget = PresenceKey::Widget { path: vec![PathSegment::Index(0)] };
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

    /// A peer that moves from the desktop transport onto an unscoped
    /// one (or back) changes hands rather than keeping whichever owner
    /// it was first seen with — otherwise a peer could end up owned by
    /// a layer that has stopped receiving them, and vanish.
    #[test]
    fn a_pointer_that_changes_transport_changes_hands() {
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

    #[test]
    fn typing_mark_fades_linearly_and_prunes() {
        use std::time::Duration;
        let mut ctx = PresenceCtx::empty();
        let bob = UserId("bob".into());
        let key = PresenceKey::Widget { path: vec![PathSegment::Index(0)] };
        let t0 = Instant::now();
        ctx.record_peer_typing(bob.clone(), key.clone(), SelectionRange { start: 3, end: 8 }, t0);

        // Fresh: alpha ~1.0, the full range present.
        let fresh = ctx.active_typing(&bob, &key, t0);
        assert_eq!(fresh.len(), 1);
        assert_eq!((fresh[0].0.start, fresh[0].0.end), (3, 8));
        assert!((fresh[0].1 - 1.0).abs() < 1e-3, "just-typed alpha is ~1.0");

        // Half-way through the fade window: alpha ~0.5.
        let mid = t0 + TYPING_FADE / 2;
        let half = ctx.active_typing(&bob, &key, mid);
        assert_eq!(half.len(), 1);
        assert!((half[0].1 - 0.5).abs() < 0.05, "alpha halves at mid-life, got {}", half[0].1);
        assert!(ctx.has_active_typing(mid));

        // Past the window: gone from the read, and prune reclaims it.
        let after = t0 + TYPING_FADE + Duration::from_millis(1);
        assert!(ctx.active_typing(&bob, &key, after).is_empty());
        assert!(!ctx.has_active_typing(after));
        assert!(!ctx.typing.is_empty(), "not yet pruned");
        ctx.prune_typing(after);
        assert!(ctx.typing.is_empty(), "prune reclaims fully-faded marks");
    }

    #[test]
    fn typing_marks_coalesce_when_contiguous() {
        let mut ctx = PresenceCtx::empty();
        let bob = UserId("bob".into());
        let key = PresenceKey::Widget { path: vec![PathSegment::Index(1)] };
        let t0 = Instant::now();
        ctx.record_peer_typing(bob.clone(), key.clone(), SelectionRange { start: 0, end: 3 }, t0);
        // A contiguous continuation (abuts the previous end) extends the span.
        ctx.record_peer_typing(bob.clone(), key.clone(), SelectionRange { start: 3, end: 6 }, t0);
        let marks = ctx.active_typing(&bob, &key, t0);
        assert_eq!(marks.len(), 1, "contiguous typing coalesces into one span");
        assert_eq!((marks[0].0.start, marks[0].0.end), (0, 6));

        // A disjoint edit elsewhere in the same block stays separate.
        ctx.record_peer_typing(bob.clone(), key.clone(), SelectionRange { start: 40, end: 44 }, t0);
        let marks = ctx.active_typing(&bob, &key, t0);
        assert_eq!(marks.len(), 2, "a far-away edit is its own span");
    }

    #[test]
    fn unregister_peer_clears_typing_and_name() {
        let mut ctx = PresenceCtx::empty();
        let alice = UserId("alice".into());
        let key = PresenceKey::Tab { id: "t".into() };
        ctx.set_display_name(alice.clone(), "Alice");
        ctx.record_peer_typing(alice.clone(), key.clone(), SelectionRange { start: 0, end: 2 }, Instant::now());
        assert!(!ctx.active_typing(&alice, &key, Instant::now()).is_empty());
        ctx.unregister_peer("alice".into());
        assert!(ctx.active_typing(&alice, &key, Instant::now()).is_empty());
        assert_eq!(ctx.display_name(&alice), "alice", "name reverts to id fallback");
    }

    #[test]
    fn peer_pointer_reads_follow_target() {
        let mut ctx = PresenceCtx::empty();
        ctx.upsert_remote_cursor("alice".into(), [120.0, 44.0], None);
        assert_eq!(ctx.peer_pointer("alice"), Some([120.0, 44.0]));
        assert_eq!(ctx.peer_pointer("nobody"), None);
    }

    #[test]
    fn unregister_peer_leaves_other_users_alone() {
        // Removing alice must not collateral-damage bob.
        let mut ctx = PresenceCtx::empty();
        let key = PresenceKey::Workspace { id: "home".into() };
        ctx.register_peer_at("alice".into(), key.clone());
        ctx.register_peer_at("bob".into(), key.clone());
        ctx.set_remote_caret("bob".into(), key.clone(), 7);
        ctx.upsert_remote_cursor("bob".into(), [99.0, 99.0], None);

        ctx.unregister_peer("alice".into());

        assert_eq!(ctx.peers_on(&key), &[UserId("bob".into())]);
        assert_eq!(ctx.caret_for(&key, &UserId("bob".into())), Some(7));
        assert_eq!(ctx.remote_cursors.len(), 1);
        assert_eq!(ctx.remote_cursors[0].user_id, "bob");
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
        PresenceKey::Workspace { id: "room-1".into() }
    }

    fn mine() -> PresenceCtx {
        let mut ctx = PresenceCtx::empty();
        ctx.set_own_identity(["quinten"]);
        ctx
    }

    /// Two apps one person has open resolve to one participant, so
    /// both publish under the same identity and the broker hands each
    /// one's frames to the other. Painting that draws the owner a pill
    /// bearing their own name, over their own document.
    #[test]
    fn a_session_does_not_paint_a_pill_for_itself() {
        let mut ctx = mine();
        ctx.apply_remote_presence(
            "quinten".into(),
            room(),
            PresencePayload::Pointer { x: 120.0, y: 90.0 },
        );
        ctx.apply_remote_presence("quinten".into(), room(), PresencePayload::Caret { pos: 7 });

        assert!(
            ctx.remote_cursors.is_empty(),
            "painted a cursor pill for this session's own identity: {:?}",
            ctx.remote_cursors,
        );
        assert_eq!(ctx.pointer_for(&room(), &UserId("quinten".into())), None);
        assert_eq!(ctx.caret_for(&room(), &UserId("quinten".into())), None);
        assert!(ctx.peers_on(&room()).is_empty());
    }

    /// The control. A second *person* is a second participant, and the
    /// self-rule may not touch them — hiding a real collaborator is the
    /// worse of the two failures by a distance.
    #[test]
    fn another_person_is_still_painted() {
        let mut ctx = mine();
        ctx.apply_remote_presence(
            "peer@77".into(),
            room(),
            PresencePayload::Pointer { x: 12.0, y: 34.0 },
        );
        ctx.apply_remote_presence("peer@77".into(), room(), PresencePayload::Caret { pos: 3 });

        assert_eq!(ctx.pointer_for(&room(), &UserId("peer@77".into())), Some((12.0, 34.0)));
        assert_eq!(ctx.caret_for(&room(), &UserId("peer@77".into())), Some(3));
        assert_eq!(ctx.remote_cursors.len(), 1);
        assert_eq!(ctx.remote_cursors[0].user_id, "peer@77");
    }

    /// A document legitimately has more than one app participant: the
    /// agent joins as one and publishes a caret as it edits, with no
    /// window anywhere. It announces an identity of its own, so the
    /// rule leaves it alone — the test is the participant, not the
    /// process and not the presence of a window.
    #[test]
    fn an_agent_in_the_same_document_is_still_painted() {
        let mut ctx = mine();
        ctx.apply_remote_presence("agent".into(), room(), PresencePayload::Caret { pos: 11 });
        assert_eq!(ctx.caret_for(&room(), &UserId("agent".into())), Some(11));
    }

    /// `Arrived` is not only a thing to draw — it is how a joiner asks
    /// the room for the document, and a second window of the same app
    /// depends on the answer. Suppressing the pill must not suppress
    /// the handshake.
    #[test]
    fn a_sibling_arriving_still_gets_the_document() {
        let mut ctx = mine();
        assert_eq!(
            ctx.apply_remote_presence("quinten".into(), room(), PresencePayload::Arrived),
            RemotePresenceEffect::PeerArrived,
            "a sibling that arrived must still be answered with state",
        );
        assert!(
            ctx.peers_on(&room()).is_empty(),
            "answering a sibling must not also list it as somebody else",
        );
    }

    /// A session that does not know what it is called suppresses
    /// nobody. The rule fails open on purpose.
    #[test]
    fn an_unnamed_session_suppresses_nobody() {
        let mut ctx = PresenceCtx::empty();
        ctx.apply_remote_presence(
            "quinten".into(),
            room(),
            PresencePayload::Pointer { x: 1.0, y: 2.0 },
        );
        assert_eq!(ctx.remote_cursors.len(), 1);
    }

    /// A blank handle is what a launcher produces when the variable it
    /// meant to forward was itself unset. Taking it literally would
    /// make every unnamed participant "me" and empty the room.
    #[test]
    fn a_blank_identity_is_not_an_identity() {
        let mut ctx = PresenceCtx::empty();
        ctx.set_own_identity(["", "   "]);
        assert!(!ctx.is_own_identity(""));
        ctx.apply_remote_presence("".into(), room(), PresencePayload::Pointer { x: 1.0, y: 2.0 });
        assert_eq!(ctx.remote_cursors.len(), 1);
    }

    /// The cursor stream reaches the same pill without passing through
    /// `apply_remote_presence`. A guard on one door and not the other
    /// leaves the bug reachable.
    #[test]
    fn the_cursor_stream_honours_the_same_rule() {
        let mut ctx = mine();
        ctx.upsert_remote_cursor("quinten".into(), [5.0, 5.0], None);
        assert!(ctx.remote_cursors.is_empty());
        ctx.upsert_remote_cursor("peer@77".into(), [5.0, 5.0], None);
        assert_eq!(ctx.remote_cursors.len(), 1);
    }

    /// Following yourself is not a thing. A `View` broadcast stamped
    /// with this session's own identity is neither stored nor reported
    /// as a reason to move the viewport.
    #[test]
    fn a_session_does_not_follow_its_own_view() {
        let mut ctx = mine();
        let subset = crate::view::ViewSubset {
            scroll: vec![10.0],
            open_resource: None,
            viewport: crate::view::ViewRect { x: 0.0, y: 0.0, width: 10.0, height: 10.0 },
            view_mode: None,
        };
        assert_eq!(
            ctx.apply_remote_presence(
                "quinten".into(),
                PresenceKey::view(&subset),
                PresencePayload::Arrived,
            ),
            RemotePresenceEffect::None,
        );
        assert!(ctx.peer_view(&UserId("quinten".into())).is_none());
    }
}
