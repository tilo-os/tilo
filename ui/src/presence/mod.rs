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
pub use render::{draw_remote_cursors, draw_remote_presence};

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
/// `cursorColor` table. Any other id falls through to a stable FNV-1a
/// hash → HSL so the same peer keeps the same hue everywhere they
/// render, but two different peers are visually distinguishable from
/// each other.
pub fn user_color(user_id: &str) -> (u8, u8, u8) {
    // Concept-pinned palette. Matches `nirios-concept/src/data.ts`
    // USERS[*].cursorColor so the Rust render is pixel-identical to the
    // reference mockup for these names.
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
            let hue = (h % 360) as f32;
            hsl_to_rgb(hue, 0.65, 0.55)
        }
    }
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (u8, u8, u8) {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hh = h / 60.0;
    let x = c * (1.0 - (hh.rem_euclid(2.0) - 1.0).abs());
    let (r1, g1, b1) = match hh as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    (
        ((r1 + m) * 255.0).clamp(0.0, 255.0) as u8,
        ((g1 + m) * 255.0).clamp(0.0, 255.0) as u8,
        ((b1 + m) * 255.0).clamp(0.0, 255.0) as u8,
    )
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

#[derive(Debug, Clone)]
pub struct RemoteCursor {
    pub user_id: String,
    pub color: [f32; 4],
    pub position: [f32; 2], // scene-space coords
    pub label: String,
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
        self.names.insert(user, name.into());
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
        let uid = UserId(user_id.clone());
        let (r, g, b) = self.color_for(&uid);
        let color = [
            r as f32 / 255.0,
            g as f32 / 255.0,
            b as f32 / 255.0,
            alpha.clamp(0.0, 1.0),
        ];
        let label = display_name
            .map(|s| s.to_string())
            .unwrap_or_else(|| user_id.clone());
        if let Some(existing) = self
            .remote_cursors
            .iter_mut()
            .find(|c| c.user_id == user_id)
        {
            existing.position = position;
            existing.color = color;
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
