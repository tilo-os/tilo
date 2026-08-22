use automerge::{AutoCommit, ObjId, ROOT};
use std::sync::{Arc, OnceLock};
use std::time::Instant;
use crate::record::{RecordCheckout, RecordId, RecordStore};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::{Blob, ImageAlphaType, ImageBrush, ImageData as Image, ImageFormat as Format};
use winit::event::WindowEvent;
use taffy::{TaffyTree, NodeId, Style, Size, Dimension};
use crate::widget::Widget;
use crate::widget::app_widget::AppWidget;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::presence::PresenceCtx;

/// Duration of the concept-parity spawn fade-in applied to a newly
/// created `TileHost`. Mirrors the nirios-concept `WindowTile`
/// `motion.initial={{ scale: 0.95, opacity: 0 }}` →
/// `motion.animate={{ scale: 1, opacity: 1 }}` pattern — kept short
/// (~180 ms) so tiles read as "arriving" without delaying the user's
/// next interaction with the tile contents.
const SPAWN_FADE_MS: f32 = 180.0;
/// Initial uniform scale of the spawn curve. Matches the concept's
/// `scale: 0.95` starting keyframe; the eased progress closes the gap
/// back to 1.0 at the end of `SPAWN_FADE_MS`.
const SPAWN_SCALE_START: f64 = 0.95;

/// Side length of the pre-baked radial shadow sprite. 256 px keeps the
/// one-shot RGBA8 allocation tiny (256 KiB) while being large enough
/// that stretching the sprite under a full-height content card doesn't
/// reveal sampling artefacts.
const SHADOW_SPRITE_SIZE: u32 = 256;

/// Gaussian falloff constant `k`, expressed as a fraction of the
/// sprite's half-extent. Alpha follows
/// `alpha = exp(-((d - d_peak) / k)^2)` with `d` the distance from the
/// centre normalised to the half-extent. 0.35 lands most of the
/// shadow's energy inside ~60 % of the radius — tuned by eye to match
/// Tailwind `shadow-2xl`'s visual density.
const SHADOW_GAUSSIAN_K: f64 = 0.35;

/// Normalised radius at which the Gaussian bump peaks. 0.30 puts the
/// peak ring at roughly a third of the way to the edge, matching the
/// task spec: centre transparent (so the white card paints over pure
/// nothing rather than a darkened underlay), peak in the interior,
/// sprite edge transparent (so the stretched image has no hard halo).
const SHADOW_PEAK_RADIUS: f64 = 0.30;

/// Build a 256×256 RGBA8 premultiplied sprite with a radial Gaussian
/// falloff baked into the alpha channel and `(r, g, b)` baked into the
/// RGB channels. Called once per tint via `OnceLock`.
///
/// The alpha envelope is `bump * inner_shoulder * outer_shoulder`:
/// - `bump = exp(-((d - SHADOW_PEAK_RADIUS) / SHADOW_GAUSSIAN_K)^2)` —
///   the classic Gaussian bump centred at the peak radius.
/// - `inner_shoulder = clamp(d / SHADOW_PEAK_RADIUS, 0, 1)` forces the
///   centre to zero so the card's own fill paints over transparent
///   sprite, not a pre-darkened spot.
/// - `outer_shoulder = max(1 - d, 0)` forces the sprite boundary to
///   zero so the stretched image can't leak a square-edged halo.
///
/// Vello expects premultiplied RGBA8: RGB channels are `src * alpha`.
fn build_shadow_sprite(r: u8, g: u8, b: u8) -> Image {
    let size = SHADOW_SPRITE_SIZE;
    let s = size as f64;
    let cx = (s - 1.0) * 0.5;
    let cy = (s - 1.0) * 0.5;
    let max_r = cx.max(cy);
    let k = SHADOW_GAUSSIAN_K;
    let d_peak = SHADOW_PEAK_RADIUS;
    let mut data = Vec::<u8>::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let dx = (x as f64 - cx) / max_r;
            let dy = (y as f64 - cy) / max_r;
            let d = (dx * dx + dy * dy).sqrt();
            let bump = (-((d - d_peak) / k).powi(2)).exp();
            let inner = (d / d_peak).min(1.0);
            let outer = (1.0 - d).max(0.0);
            let a = (bump * inner * outer).clamp(0.0, 1.0);
            let a8 = (a * 255.0).round() as u8;
            data.push((r as f64 * a).round() as u8);
            data.push((g as f64 * a).round() as u8);
            data.push((b as f64 * a).round() as u8);
            data.push(a8);
        }
    }
    let blob = Blob::new(Arc::new(data));
    Image {
        data: blob,
        format: Format::Rgba8,
        alpha_type: ImageAlphaType::Alpha,
        width: size,
        height: size,
    }
}

/// Cached ambient (black) drop-shadow sprite used for `shadow-2xl`.
fn ambient_shadow_sprite() -> &'static Image {
    static SPRITE: OnceLock<Image> = OnceLock::new();
    SPRITE.get_or_init(|| build_shadow_sprite(0, 0, 0))
}

/// Cached indigo bloom sprite used for the focused-tile
/// `shadow-[0_0_60px_rgba(99,102,241,0.6)]`. RGB = `#6366f1`
/// (indigo-500).
fn indigo_bloom_sprite() -> &'static Image {
    static SPRITE: OnceLock<Image> = OnceLock::new();
    SPRITE.get_or_init(|| build_shadow_sprite(0x63, 0x66, 0xf1))
}

/// Stretch a cached shadow sprite across `target`, modulated by `alpha`
/// via a `push_layer`/`pop_layer` pair so callers can scale the overall
/// shadow intensity without re-baking the sprite. The sprite's baked
/// RGB already encodes the tint; `alpha` is just a global multiplier.
fn draw_shadow_sprite(
    scene: &mut Scene,
    transform: Affine,
    sprite: &Image,
    target: Rect,
    alpha: f32,
) {
    let tw = target.width();
    let th = target.height();
    if tw <= 0.0 || th <= 0.0 || alpha <= 0.0 {
        return;
    }
    let sx = tw / sprite.width as f64;
    let sy = th / sprite.height as f64;
    let brush_transform = transform
        * Affine::translate((target.x0, target.y0))
        * Affine::scale_non_uniform(sx, sy);
    scene.push_layer(
        vello::peniko::Fill::NonZero,
        vello::peniko::BlendMode::default(),
        alpha,
        transform,
        &target,
    );
    let sprite_brush = ImageBrush::new(sprite.clone());
    scene.fill(
        vello::peniko::Fill::NonZero,
        brush_transform,
        &sprite_brush,
        None,
        &Rect::new(0.0, 0.0, sprite.width as f64, sprite.height as f64),
    );
    scene.pop_layer();
}

/// Which direction through a document's history a keystroke asks for.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum UndoStep {
    Undo,
    Redo,
}

/// The undo / redo chord an event carries, if any.
///
/// The same three bindings the host answers globally (`Ctrl+Z`,
/// `Ctrl+Shift+Z`, `Ctrl+Y`) — read here first, and only while a tile's
/// app holds focus, so the keystroke lands on the document the user is
/// looking at.
fn undo_step(event: &WindowEvent, m: &winit::event::Modifiers) -> Option<UndoStep> {
    if !(m.state().control_key() || m.state().super_key()) {
        return None;
    }
    let WindowEvent::KeyboardInput { event: key, .. } = event else {
        return None;
    };
    if key.state != winit::event::ElementState::Pressed {
        return None;
    }
    let winit::keyboard::Key::Character(c) = &key.logical_key else {
        return None;
    };
    match c.as_str() {
        "z" | "Z" if m.state().shift_key() => Some(UndoStep::Redo),
        "z" | "Z" => Some(UndoStep::Undo),
        "y" | "Y" => Some(UndoStep::Redo),
        _ => None,
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum AppKind { Terminal, Files, Editor, Browser, Settings }

impl AppKind {
    pub fn as_str(&self) -> &str {
        match self {
            AppKind::Terminal => "Terminal",
            AppKind::Files => "Files",
            AppKind::Editor => "Editor",
            AppKind::Browser => "Browser",
            AppKind::Settings => "Settings",
        }
    }
}

/// Content for a single tile (single tab) hosted by a `TileHost`.
///
/// As of Plan 3 Task 3, `App` carries a `Box<dyn AppWidget>` — `AppWidget: Widget`
/// so layout/draw/event calls still auto-upcast to the base `Widget` contract.
/// The `Wayland(SurfaceId)` variant is reserved for future compositor integration.
pub enum TileContent {
    App(Box<dyn AppWidget>),
    Stub { kind: AppKind, label: String },
    // reserved: Wayland(u64),
}

/// Which record a tile is showing, and where that record's document lives.
///
/// A tab is an open thing, not a view onto one shared thing, so the
/// document behind it is the tab's own. Without this the host handed every
/// tile the same document root and the same key inside it: two Notes tabs
/// were one note wearing two pills, and everything downstream — undo,
/// presence, in-app search — inherited that.
struct Binding {
    store: RecordStore,
    /// The record the *tab* is: where its content lives while no file
    /// names it. Never changes for the life of the tab.
    tab: RecordCheckout,
    /// The record actually live in this tile. `tab.id` until the hosted
    /// app declares a file through
    /// [`Widget::record_intent`](crate::widget::Widget::record_intent) —
    /// opening a document *is* moving the tab onto that document's record,
    /// and the scratch record it leaves behind is still its own document.
    live: RecordId,
    /// Whether the app has taken this record's document through a first
    /// `sync` yet. What that pass writes — the object a body lives in, the
    /// seed a fresh note carries — is the document's *shape*, not
    /// something the user did, so the history's baseline is taken after
    /// it. Otherwise the first Ctrl+Z deletes the body wholesale instead
    /// of reverting what was typed into it.
    established: bool,
}

impl Binding {
    /// Follow what the app says it is working on. Returns the record whose
    /// document the app must be handed this pass.
    fn follow(&mut self, want: Option<RecordCheckout>) -> RecordId {
        if let Some(want) = want {
            if want.id != self.live {
                self.store.open_record(&want);
                self.live = want.id;
                self.established = false;
            }
        }
        self.live.clone()
    }
}

pub struct TileHost {
    content: TileContent,
    /// The tab's own document, or `None` for a tile nothing bound — a
    /// bare `ui` host, a stub card, a test that only exercises paint.
    /// Unbound tiles keep the old behaviour of editing whatever document
    /// the parent pass carries.
    binding: Option<Binding>,
    /// The record scope the hosted app was last told about, so a scope is
    /// announced once per change rather than every frame.
    announced: Option<RecordId>,
    rect: Rect,
    /// Set by the parent `WindowGroup` each frame to indicate whether
    /// this tile is the "focused" tile — the concept's
    /// `shadow-[0_0_60px_rgba(99,102,241,0.6)]` bloom
    /// (`WindowFrame.tsx:359`) is painted only when this is true.
    pub focused: bool,
    /// Timestamp at which this TileHost was marked "newly spawned".
    /// When set, `draw` applies an ease-out-cubic fade + uniform scale
    /// curve over `SPAWN_FADE_MS`, mirroring the nirios-concept
    /// `WindowTile` framer-motion
    /// `initial={{ scale: 0.95, opacity: 0 }}` →
    /// `animate={{ scale: 1, opacity: 1 }}` pattern. Cleared back to
    /// `None` at the top of `draw` once the curve has elapsed so
    /// subsequent frames render at steady state and don't pay the
    /// layer push/pop cost. Tests that construct a `TileHost` via
    /// `TileHost::new` get `None` here by default, matching the
    /// "animations only fire for runtime insertions" invariant used
    /// by `TabBar`'s insert fade.
    spawned_at: Option<Instant>,
}

impl TileHost {
    pub fn new(content: TileContent) -> Self {
        Self {
            content,
            binding: None,
            announced: None,
            rect: Rect::ZERO,
            focused: false,
            spawned_at: None,
        }
    }

    /// Give this tile its own document: the record `tab` names, kept in
    /// `store`.
    ///
    /// Called once per tab as the window tree is built. From here on the
    /// hosted app reads and writes that record's document rather than
    /// whatever the parent pass carries, which is what makes a second tab
    /// of an app a second thing rather than another window onto the first.
    ///
    /// Idempotent on the same record — rebuilding the tree re-binds every
    /// tile, and a bind that opened the record again would hand the tab a
    /// fresh empty document.
    pub fn bind_record(&mut self, store: RecordStore, tab: RecordCheckout) {
        if let Some(b) = &self.binding {
            if b.tab.id == tab.id {
                return;
            }
        }
        store.open_record(&tab);
        let live = tab.id.clone();
        self.binding = Some(Binding {
            store,
            tab,
            live,
            established: false,
        });
        self.announce_scope();
    }

    /// Bring the binding up to date with what the app says it is working
    /// on, and hand back the record whose document it must be given.
    fn resolve_record(&mut self) -> Option<RecordId> {
        let want = match &self.content {
            TileContent::App(w) => w.record_intent(),
            TileContent::Stub { .. } => None,
        };
        let live = self.binding.as_mut()?.follow(want);
        self.announce_scope();
        Some(live)
    }

    /// Tell the hosted app which record it is on, once per change.
    fn announce_scope(&mut self) {
        let Some(live) = self.binding.as_ref().map(|b| b.live.clone()) else {
            return;
        };
        if self.announced.as_ref() == Some(&live) {
            return;
        }
        if let TileContent::App(w) = &mut self.content {
            w.set_record_scope(&live);
        }
        self.announced = Some(live);
    }
    pub fn kind_string(&self) -> String {
        match &self.content {
            TileContent::App(_) => "App".into(),
            TileContent::Stub { kind, .. } => kind.as_str().into(),
        }
    }
    pub fn content_mut(&mut self) -> &mut TileContent { &mut self.content }
    /// What this tile is showing, read-only.
    pub fn content(&self) -> &TileContent { &self.content }
    /// Toggle the indigo-bloom shadow. Called by `WindowGroup::draw`
    /// before dispatching to the active host.
    pub fn set_focused(&mut self, focused: bool) { self.focused = focused; }

    /// Latch the concept-parity spawn fade-in (opacity 0→1, scale
    /// 0.95→1.0 with an ease-out-cubic curve over `SPAWN_FADE_MS`).
    /// Callers that create a brand-new `TileHost` — e.g. a future
    /// `WindowGroup::add_tab` path or the detach/insert flow in
    /// `GroupRow` — invoke this immediately after construction so the
    /// tile reads as "arriving" rather than popping in opaque. A call
    /// inside the animation window simply restarts the curve.
    pub fn start_spawn_animation(&mut self) {
        self.spawned_at = Some(Instant::now());
    }

    /// True while the spawn fade is still within its `SPAWN_FADE_MS`
    /// window. Used by the `draw` path to decide whether to compose
    /// the alpha/scale layer and by `needs_animation_frame` to keep
    /// pumping redraws.
    pub fn spawn_in_flight(&self) -> bool {
        match self.spawned_at {
            Some(t) => t.elapsed().as_millis() < SPAWN_FADE_MS as u128,
            None => false,
        }
    }

    /// Ease-out-cubic progress of the spawn curve in `[0.0, 1.0]`, or
    /// `None` when no spawn animation is in flight. Exposed for tests
    /// so the curve shape can be pinned without peeking into Vello's
    /// scene buffer.
    #[cfg(test)]
    pub fn spawn_alpha(&self) -> Option<f32> {
        self.spawned_at.and_then(|t| {
            let elapsed = t.elapsed().as_millis() as f32;
            if elapsed >= SPAWN_FADE_MS {
                None
            } else {
                let raw = (elapsed / SPAWN_FADE_MS).clamp(0.0, 1.0);
                Some(1.0 - (1.0 - raw).powi(3))
            }
        })
    }
}

impl Widget for TileHost {
    fn sync(&mut self, s: &mut AutoCommit, p: &ObjId) {
        let live = self.resolve_record();
        let TileContent::App(w) = &mut self.content else {
            return;
        };
        match (live, &mut self.binding) {
            (Some(id), Some(b)) => {
                b.store.with(&id, |doc| w.sync(doc, &ROOT));
                if !b.established {
                    // The document's shape is now established; the user's
                    // history starts here. See `Binding::established`.
                    b.store.with_history(&id, |doc, history| history.rebase(doc));
                    b.established = true;
                }
            }
            _ => w.sync(s, p),
        }
    }

    fn update(&mut self, s: &mut AutoCommit, p: &ObjId) -> bool {
        let live = self.resolve_record();
        let TileContent::App(w) = &mut self.content else {
            return false;
        };
        let (Some(id), Some(b)) = (live, &self.binding) else {
            return w.update(s, p);
        };
        let changed = b.store.with(&id, |doc| w.update(doc, &ROOT)).unwrap_or(false);
        // Close the app's typing burst against *this* record's history.
        // The host's own commit pass works on the document it holds, which
        // is no longer the one the tab is editing — leaving the burst open
        // forever, and the tab with nothing to undo.
        if w.needs_commit() {
            b.store.with_history(&id, |doc, history| {
                history.commit_action_authored(
                    doc,
                    None,
                    w.get_state(),
                    Some(crate::collaboration::local_user_id()),
                );
            });
            w.on_commit();
        }
        changed
    }
    fn build_layout(&mut self, taffy: &mut TaffyTree<()>, theme: &Theme, node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>) -> NodeId {
        let node = match &mut self.content {
            TileContent::App(w) => {
                let child = w.build_layout(taffy, theme, node_map);
                taffy.new_with_children(Style { size: Size { width: Dimension::percent(1.0), height: Dimension::percent(1.0) }, ..Default::default() }, &[child]).unwrap()
            }
            TileContent::Stub { .. } => {
                taffy.new_leaf(Style { size: Size { width: Dimension::percent(1.0), height: Dimension::percent(1.0) }, ..Default::default() }).unwrap()
            }
        };
        node_map.insert(node, self as *mut dyn Widget);
        node
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(origin.x + l.location.x as f64, origin.y + l.location.y as f64, origin.x + l.location.x as f64 + l.size.width as f64, origin.y + l.location.y as f64 + l.size.height as f64);
        if let TileContent::App(w) = &mut self.content {
            // Defensive guard for a structural mismatch where `self.content`
            // is `App` but the taffy node has no child (the build_layout
            // pass that registered the node ran while content was still
            // `Stub`, or a structure_dirty rebuild hasn't fired yet).
            // Previously panicked with "index out of bounds: the len is
            // 0 but the index is 0" when opening Files for the first
            // time on a freshly-booted shell.
            let children = taffy.children(node).unwrap_or_default();
            if let Some(&child_node) = children.first() {
                w.apply_layout(taffy, child_node, Point::new(self.rect.x0, self.rect.y0));
            } else {
                tracing::warn!(
                    "TileHost::apply_layout: App content with no taffy child; \
                     skipping (next frame's structure rebuild should resolve)"
                );
            }
        }
    }
    fn draw(&mut self, scene: &mut Scene, transform: Affine, tr: &mut TextRenderer, theme: &Theme, presence: &PresenceCtx) {
        // Clear an expired spawn timestamp before branching so
        // `spawn_in_flight()` can't stay `Some` forever once the curve
        // has ended — otherwise subsequent steady-state frames would
        // still carry the Option around for no reason.
        if let Some(t) = self.spawned_at {
            if t.elapsed().as_millis() >= SPAWN_FADE_MS as u128 {
                self.spawned_at = None;
            }
        }
        // Reduced motion: a new tile appears at its resting opacity and
        // scale with no fade-in. Drop the timestamp so the steady-state
        // branch below runs and we stop requesting animation frames.
        if theme.reduce_motion {
            self.spawned_at = None;
        }
        // Spawn-fade branch: compose the fade + uniform-scale curve by
        // wrapping the steady-state paint in a single alpha layer +
        // centre-pivot scale. The whole tile (shadow sprites + card +
        // content) is scaled and faded together so the motion reads as
        // one arriving unit.
        if self.spawn_in_flight() {
            let started = self.spawned_at.expect("spawn_in_flight guards Some");
            let elapsed = started.elapsed().as_millis() as f32;
            let t = (elapsed / SPAWN_FADE_MS).clamp(0.0, 1.0);
            let eased = 1.0 - (1.0 - t).powi(3);
            let alpha = eased;
            let scale = SPAWN_SCALE_START + (1.0 - SPAWN_SCALE_START) * eased as f64;
            // Wide clip so the shadow sprites (`ambient_pad = 50`,
            // `bloom_pad = 120`) aren't chopped mid-fade. 160 px is a
            // touch wider than the bloom's outer extent to leave
            // headroom if future tweaks widen the glow.
            let clip = Rect::new(
                self.rect.x0 - 160.0,
                self.rect.y0 - 160.0,
                self.rect.x1 + 160.0,
                self.rect.y1 + 160.0,
            );
            scene.push_layer(
                vello::peniko::Fill::NonZero,
                vello::peniko::BlendMode::default(),
                alpha,
                transform,
                &clip,
            );
            let centre = self.rect.center();
            let anim_transform = transform
                * Affine::translate((centre.x, centre.y))
                * Affine::scale(scale)
                * Affine::translate((-centre.x, -centre.y));
            self.draw_steady_state(scene, anim_transform, tr, theme, presence);
            scene.pop_layer();
            return;
        }
        self.draw_steady_state(scene, transform, tr, theme, presence);
    }
    fn handle_event(&mut self, event: &WindowEvent, scale_factor: f64, m: &winit::event::Modifiers, s: &mut AutoCommit, p: &ObjId, handled: &mut bool, focus_target: &mut Option<NodeId>, node_id: NodeId, taffy: &TaffyTree<()>) {
        let live = self.resolve_record();
        let TileContent::App(w) = &mut self.content else {
            return;
        };
        // Same structural-mismatch guard as apply_layout above.
        let children = taffy.children(node_id).unwrap_or_default();
        let Some(&child_node) = children.first() else {
            return;
        };
        let (Some(id), Some(b)) = (live, &self.binding) else {
            w.handle_event(event, scale_factor, m, s, p, handled, focus_target, child_node, taffy);
            return;
        };
        // Undo belongs to the document, so it is answered here rather than
        // by the host's one global stack. An `UndoEntry` is a pair of
        // change hashes inside *this* record's change graph; run against
        // another tab's document it names changes that document has never
        // seen, and Ctrl+Z either does nothing or reverts the wrong thing.
        if w.is_focused()
            && let Some(step) = undo_step(event, m)
        {
            b.store.with_history(&id, |doc, history| {
                let author = crate::collaboration::local_user_id();
                // Close whatever burst is still open first, so Ctrl+Z
                // immediately after typing reverts what was just typed
                // rather than the action before it. A commit with nothing
                // in the transaction records nothing, so this is free when
                // the burst has already closed.
                if matches!(step, UndoStep::Undo) {
                    history.commit_action_authored(
                        doc,
                        None,
                        w.get_state(),
                        Some(author.clone()),
                    );
                }
                match step {
                    UndoStep::Undo => history.undo_as(doc, Some(&author), None, w.get_state()),
                    UndoStep::Redo => history.redo_as(doc, Some(&author), None, w.get_state()),
                }
                .map(|entry| {
                    if let Some(meta) = entry.widget_metadata.as_deref() {
                        w.set_state(meta);
                    }
                })
            });
            w.on_commit();
            *handled = true;
            return;
        }
        b.store.with(&id, |doc| {
            w.handle_event(
                event,
                scale_factor,
                m,
                doc,
                &ROOT,
                handled,
                focus_target,
                child_node,
                taffy,
            )
        });
    }
    fn name(&self) -> &str { "TileHost" }
    fn get_rect(&self) -> Rect { self.rect }
    /// The tile's presence identity is the record it is showing.
    ///
    /// Which is the only thing that tells two tabs of one app apart: the
    /// app is the same app, the widget path inside it is the same path,
    /// and a peer's caret is a position in *a document*. Unbound tiles
    /// report nothing, as they did before there was a record to name.
    fn presence_key(&self) -> Option<crate::presence::PresenceKey> {
        self.binding.as_ref().map(|b| crate::presence::PresenceKey::Tab {
            id: b.live.as_str().to_string(),
        })
    }
    /// A bound tile has already committed the app's burst against its own
    /// record in [`Widget::update`]; saying "yes" here would ask the host
    /// to commit its own, unrelated document too.
    fn needs_commit(&self) -> bool {
        if self.binding.is_some() {
            return false;
        }
        match &self.content {
            TileContent::App(w) => w.needs_commit(),
            TileContent::Stub { .. } => false,
        }
    }
    fn on_commit(&mut self) {
        if self.binding.is_some() {
            return;
        }
        if let TileContent::App(w) = &mut self.content {
            w.on_commit();
        }
    }
    fn take_structure_dirty(&mut self) -> bool {
        // A hosted app widget could itself mutate its subtree (e.g. a file
        // explorer that spawns a new pane). Forward through so App can
        // rebuild the taffy tree when the inner widget reports dirty.
        if let TileContent::App(w) = &mut self.content {
            w.take_structure_dirty()
        } else {
            false
        }
    }
    fn needs_animation_frame(&self) -> bool {
        // `TileHost` doesn't expose children via `children()`, so the
        // default `Widget::needs_animation_frame` (which recurses into
        // `children()`) would return `false` for every host. Override
        // explicitly so (a) the spawn fade keeps pumping frames until
        // it settles, and (b) any hosted `AppWidget`'s own animation
        // signal still bubbles up through the tile to `App::run`.
        let inner = match &self.content {
            TileContent::App(w) => w.needs_animation_frame(),
            TileContent::Stub { .. } => false,
        };
        self.spawn_in_flight() || inner
    }
}

impl TileHost {
    /// Steady-state paint: ambient shadow → focused-only indigo bloom
    /// → white card + border → hosted `AppWidget` or stub contents.
    /// Extracted from `draw` so the spawn-fade branch can wrap the
    /// exact same paint under a single `push_layer` + scaled
    /// `transform` without duplicating the underlying paint code.
    fn draw_steady_state(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let radius = theme.tokens.radii.xl as f64;
        // Ambient `shadow-2xl` (`0 25px 50px -12px rgba(0,0,0,0.25)`):
        // stretch the cached radial-Gaussian sprite beneath the card
        // with a 25 px downward offset and ~50 px bleed on each side so
        // the soft falloff extends past the card's rounded corners.
        // Replaces the prior 24-ring concentric rounded-rect stack,
        // which visibly stair-stepped because Vello 0.2 has no blur
        // primitive to smooth the ring union.
        let ambient_pad = 50.0_f64;
        let ambient_y = 25.0_f64;
        let ambient_target = Rect::new(
            self.rect.x0 - ambient_pad,
            self.rect.y0 - ambient_pad + ambient_y,
            self.rect.x1 + ambient_pad,
            self.rect.y1 + ambient_pad + ambient_y,
        );
        draw_shadow_sprite(
            scene,
            transform,
            ambient_shadow_sprite(),
            ambient_target,
            0.25,
        );
        // Focused-only indigo bloom
        // (`shadow-[0_0_60px_rgba(99,102,241,0.6)]`): wider bleed,
        // centred (no y-offset), tinted indigo-500. Only painted when
        // the parent `WindowGroup` marks this tile focused — matches
        // the concept's `isSearching` predicate in WindowFrame.tsx:359.
        if self.focused {
            let bloom_pad = 120.0_f64;
            let bloom_target = Rect::new(
                self.rect.x0 - bloom_pad,
                self.rect.y0 - bloom_pad,
                self.rect.x1 + bloom_pad,
                self.rect.y1 + bloom_pad,
            );
            draw_shadow_sprite(
                scene,
                transform,
                indigo_bloom_sprite(),
                bloom_target,
                0.6,
            );
        }
        // White rounded-3xl card (concept: `bg-white` `rounded-3xl`
        // `border-white/20` `shadow-2xl`).
        let shape = RoundedRect::from_rect(self.rect, radius);
        scene.fill(vello::peniko::Fill::NonZero, transform, theme.tokens.colors.surfaces.window_card, None, &shape);
        // Everything the tile hosts paints inside the card's silhouette.
        // Hosted apps fill their own background as a plain rectangle —
        // they have to, since an app cannot know it is being framed — so
        // without this clip the first `scene.fill` an app makes squares
        // off the window's corners and paints over the border below.
        // Clipping here keeps the rounded window shape a property of the
        // frame (which the OS owns) rather than something every app has
        // to reproduce.
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &shape,
        );
        match &mut self.content {
            TileContent::App(w) => w.draw(scene, transform, tr, theme, presence),
            TileContent::Stub { kind, label } => {
                // Dark text on the white card.
                let on_card = theme.tokens.colors.neutral[1]; // slate-900 in dark theme
                let on_card_dim = theme.tokens.colors.neutral[4]; // slate-600
                let text_size = theme.tokens.typography.scale.xl;
                let (tw, _) = tr.measure(label, text_size, None);
                let x = self.rect.center().x - tw as f64 / 2.0;
                let y = self.rect.center().y - text_size as f64 / 2.0;
                tr.draw_text(scene, label, text_size, on_card, transform * Affine::translate((x, y)), None);
                let sm = theme.tokens.typography.scale.sm;
                let s = format!("{} coming soon", kind.as_str());
                let (sw, _) = tr.measure(&s, sm, None);
                let sx = self.rect.center().x - sw as f64 / 2.0;
                tr.draw_text(scene, &s, sm, on_card_dim, transform * Affine::translate((sx, y + text_size as f64 + 8.0)), None);
            }
        }
        scene.pop_layer();
        // Border last. Stroked over the hosted content so the window
        // always keeps a visible edge against the desktop — an app that
        // fills its own dark background can no longer erase the one line
        // that separates the window from the wallpaper behind it.
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            theme.tokens.colors.surfaces.window_card_border,
            None,
            &shape,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn sprite_alpha_at(sprite: &Image, x: u32, y: u32) -> u8 {
        let bytes: &[u8] = sprite.data.data().as_ref();
        let idx = ((y * sprite.width + x) * 4 + 3) as usize;
        bytes[idx]
    }

    /// Per the task spec: the radial sprite must be transparent at the
    /// centre, transparent at the edge, and non-trivially opaque in the
    /// interior band so the shadow reads as a soft ring rather than a
    /// hard rectangle or a flat fill. Samples are taken along the
    /// horizontal row through the sprite centre at fractions
    /// 0.0 / 0.3 / 0.9 of the diameter (task spec's recommendation).
    #[test]
    fn shadow_sprite_peaks_in_interior_not_centre_or_edge() {
        let sprite = ambient_shadow_sprite();
        let size = sprite.width;
        let cy = size / 2;
        // Centre and outer edge are guaranteed zero by the `inner`/
        // `outer` shoulder terms; a pixel offset of ±1 from centre and
        // +/-0 from edge still evaluates near zero within 2/255.
        let a_centre = sprite_alpha_at(sprite, size / 2, cy);
        let a_edge = sprite_alpha_at(sprite, size - 1, cy);
        // 0.3 × diameter puts the sample on the Gaussian peak ring;
        // 0.9 × diameter sits in the decayed tail.
        let r_peak = (size as f64 * 0.3) as u32;
        let r_tail = (size as f64 * 0.9) as u32;
        let a_peak = sprite_alpha_at(sprite, r_peak, cy);
        let a_tail = sprite_alpha_at(sprite, r_tail, cy);
        assert!(
            a_centre <= 2 && a_edge <= 2,
            "centre and edge must be (near-)transparent; got centre={a_centre} edge={a_edge}",
        );
        assert!(
            a_peak > a_tail,
            "peak (0.3d={a_peak}) must exceed tail (0.9d={a_tail})",
        );
    }

    /// A hosted app that writes one string into whatever document it is
    /// handed, and optionally declares a file it has open.
    struct Scribe {
        text: String,
        opens: Option<std::path::PathBuf>,
    }

    impl Widget for Scribe {
        fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
            use automerge::transaction::Transactable;
            let _ = state.put(path, "body", self.text.as_str());
        }
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &Theme,
            node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            let n = taffy.new_leaf(Style::default()).unwrap();
            node_map.insert(n, self as *mut dyn Widget);
            n
        }
        fn apply_layout(&mut self, _t: &TaffyTree<()>, _n: NodeId, _o: Point) {}
        fn draw(
            &mut self,
            _s: &mut Scene,
            _t: Affine,
            _tr: &mut TextRenderer,
            _th: &Theme,
            _p: &PresenceCtx,
        ) {
        }
        fn handle_event(
            &mut self,
            _event: &WindowEvent,
            _scale: f64,
            _m: &winit::event::Modifiers,
            _state: &mut AutoCommit,
            _path: &ObjId,
            _handled: &mut bool,
            _focus: &mut Option<NodeId>,
            _node: NodeId,
            _taffy: &TaffyTree<()>,
        ) {
        }
        fn name(&self) -> &str {
            "Scribe"
        }
        fn get_rect(&self) -> Rect {
            Rect::ZERO
        }
        /// The file-backed half of a tab's identity: an app that has a
        /// file open says so, and the tile moves onto that file's record.
        fn record_intent(&self) -> Option<RecordCheckout> {
            self.opens.as_ref().map(|p| {
                RecordCheckout::new(RecordId::for_file(p), crate::record::RecordKind::new("file"))
            })
        }
    }

    impl crate::widget::app_widget::AppWidget for Scribe {
        fn app_kind(&self) -> AppKind {
            AppKind::Editor
        }
        fn app_title(&self) -> &str {
            "Scribe"
        }
        fn app_icon(&self) -> crate::widget::app_widget::IconDescriptor {
            crate::widget::app_widget::IconDescriptor::Glyph("S")
        }
        fn permission_hint(&self) -> Option<AppKind> {
            None
        }
    }

    fn body_of(store: &RecordStore, id: &RecordId) -> Option<String> {
        use automerge::ReadDoc;
        store.read(id, |doc| match doc.get(&ROOT, "body") {
            Ok(Some((automerge::Value::Scalar(v), _))) => match v.as_ref() {
                automerge::ScalarValue::Str(s) => Some(s.to_string()),
                _ => None,
            },
            _ => None,
        })?
    }

    fn tab_checkout(id: u64) -> RecordCheckout {
        RecordCheckout::new(RecordId::for_tab(id), crate::record::RecordKind::new("tab"))
    }

    /// A tab with no file is its own record; a tab whose app has a file
    /// open is that file's.
    #[test]
    fn a_tile_follows_the_record_its_app_names() {
        let store = RecordStore::in_memory();
        let mut parent = AutoCommit::new();

        let mut scratch = TileHost::new(TileContent::App(Box::new(Scribe {
            text: "unsaved".into(),
            opens: None,
        })));
        scratch.bind_record(store.clone(), tab_checkout(1));
        scratch.sync(&mut parent, &ROOT);
        assert_eq!(
            body_of(&store, &RecordId::for_tab(1)).as_deref(),
            Some("unsaved"),
            "a tab with no file writes into the record the tab is",
        );

        let file = std::path::PathBuf::from("/work/report.odt");
        let mut opened = TileHost::new(TileContent::App(Box::new(Scribe {
            text: "the report".into(),
            opens: Some(file.clone()),
        })));
        opened.bind_record(store.clone(), tab_checkout(2));
        opened.sync(&mut parent, &ROOT);
        assert_eq!(
            body_of(&store, &RecordId::for_file(&file)).as_deref(),
            Some("the report"),
            "an app that has a file open edits that file's record",
        );
        assert_eq!(
            body_of(&store, &RecordId::for_tab(2)),
            None,
            "and not the scratch record its tab started on",
        );
        assert_eq!(
            body_of(&store, &RecordId::for_tab(1)).as_deref(),
            Some("unsaved"),
            "the other tab is untouched by any of it",
        );
    }

    /// Two tabs opened on one file are one document — a copy would
    /// diverge, and no amount of merging makes two copies one file again.
    #[test]
    fn two_tabs_on_one_file_are_one_document() {
        let store = RecordStore::in_memory();
        let mut parent = AutoCommit::new();
        let file = std::path::PathBuf::from("/work/shared.odt");
        for (tab, text) in [(3u64, "first"), (4u64, "second")] {
            let mut host = TileHost::new(TileContent::App(Box::new(Scribe {
                text: text.into(),
                opens: Some(file.clone()),
            })));
            host.bind_record(store.clone(), tab_checkout(tab));
            host.sync(&mut parent, &ROOT);
        }
        assert_eq!(
            store.open_records().len(),
            3,
            "two scratch records and the one file both tabs opened",
        );
        assert_eq!(
            body_of(&store, &RecordId::for_file(&file)).as_deref(),
            Some("second"),
            "the second tab edited the same document as the first",
        );
    }

    fn stub_host() -> TileHost {
        TileHost::new(TileContent::Stub {
            kind: AppKind::Terminal,
            label: "t".into(),
        })
    }

    #[test]
    fn start_spawn_animation_records_timestamp() {
        let mut h = stub_host();
        assert!(h.spawned_at.is_none(), "fresh host has no spawn timestamp");
        h.start_spawn_animation();
        assert!(
            h.spawned_at.is_some(),
            "start_spawn_animation must record an Instant",
        );
    }

    #[test]
    fn spawn_in_flight_returns_true_within_duration() {
        let mut h = stub_host();
        h.start_spawn_animation();
        // Sleep well inside the fade window so the elapsed check
        // can't race a slow CI box past `SPAWN_FADE_MS`.
        std::thread::sleep(Duration::from_millis(20));
        assert!(
            h.spawn_in_flight(),
            "spawn_in_flight must be true within SPAWN_FADE_MS",
        );
        let alpha = h.spawn_alpha().expect("alpha in flight");
        assert!(
            (0.0..1.0).contains(&alpha),
            "eased alpha mid-flight must be in [0, 1); got {alpha}",
        );
    }

    #[test]
    fn spawn_in_flight_returns_false_after_duration() {
        let mut h = stub_host();
        h.start_spawn_animation();
        // Sleep past `SPAWN_FADE_MS + 20` so the elapsed check
        // definitively exceeds the window even with timer jitter.
        std::thread::sleep(Duration::from_millis(SPAWN_FADE_MS as u64 + 20));
        assert!(
            !h.spawn_in_flight(),
            "spawn_in_flight must be false after SPAWN_FADE_MS elapses",
        );
        assert!(
            h.spawn_alpha().is_none(),
            "expired spawn must report no alpha",
        );
    }

    #[test]
    fn draw_without_spawn_animation_does_not_push_extra_layer() {
        // Indirect assertion: when no spawn animation has been
        // started, `spawn_in_flight` is false, `spawn_alpha` is None,
        // and the `draw` path skips the wrapper `push_layer`/centre-
        // pivot-scale block entirely. Pinning that pair keeps future
        // edits from accidentally making the wrapper always-on — which
        // would regress steady-state cost by one layer + one affine
        // compose per frame per tile.
        let h = stub_host();
        assert!(
            !h.spawn_in_flight(),
            "fresh host must not be in flight",
        );
        assert!(
            h.spawn_alpha().is_none(),
            "fresh host must report no spawn alpha",
        );
    }

    /// An app that fills its whole rect — which is what every hosted app
    /// does, since an app cannot know it is being framed — must not be
    /// able to square off the window's rounded corners or paint over its
    /// border. Both are properties of the frame the OS draws, and both
    /// were being destroyed by the first `scene.fill` the app made.
    ///
    /// Rendered through the real `Widget::draw`, then sampled: the outer
    /// corner has to stay the desktop behind the tile, and the card's
    /// edge has to still be distinguishable from the app's own fill.
    #[test]
    fn a_hosted_app_cannot_square_off_the_window_or_erase_its_border() {
        use crate::offscreen::OffscreenRenderer;

        /// Fills its entire rect with one flat opaque colour, exactly as
        /// `FileExplorerWidget::paint_background` and every other hosted
        /// app's background pass does.
        struct FloodFill {
            rect: Rect,
        }
        const FLOOD: vello::peniko::Color = vello::peniko::Color::from_rgb8(0xFF, 0x00, 0x00);
        impl Widget for FloodFill {
            fn sync(&mut self, _s: &mut AutoCommit, _p: &ObjId) {}
            fn build_layout(
                &mut self,
                taffy: &mut TaffyTree<()>,
                _theme: &Theme,
                node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
            ) -> NodeId {
                let n = taffy.new_leaf(Style::default()).unwrap();
                node_map.insert(n, self as *mut dyn Widget);
                n
            }
            fn apply_layout(&mut self, _t: &TaffyTree<()>, _n: NodeId, _o: Point) {}
            fn draw(
                &mut self,
                scene: &mut Scene,
                transform: Affine,
                _tr: &mut TextRenderer,
                _th: &Theme,
                _p: &PresenceCtx,
            ) {
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    FLOOD,
                    None,
                    &self.rect,
                );
            }
            fn handle_event(
                &mut self,
                _event: &WindowEvent,
                _scale: f64,
                _m: &winit::event::Modifiers,
                _state: &mut AutoCommit,
                _path: &ObjId,
                _handled: &mut bool,
                _focus: &mut Option<NodeId>,
                _node: NodeId,
                _taffy: &TaffyTree<()>,
            ) {
            }
            fn name(&self) -> &str {
                "FloodFill"
            }
            fn get_rect(&self) -> Rect {
                self.rect
            }
        }
        impl crate::widget::app_widget::AppWidget for FloodFill {
            fn app_kind(&self) -> AppKind {
                AppKind::Files
            }
            fn app_title(&self) -> &str {
                "FloodFill"
            }
            fn app_icon(&self) -> crate::widget::app_widget::IconDescriptor {
                crate::widget::app_widget::IconDescriptor::Glyph("F")
            }
            fn permission_hint(&self) -> Option<AppKind> {
                None
            }
        }

        // Tile inset far enough from the surface edge that the ambient
        // shadow sprite has somewhere to land without wrapping.
        let (w, h) = (200u32, 200u32);
        let rect = Rect::new(50.0, 50.0, 150.0, 150.0);
        let mut host = TileHost::new(TileContent::App(Box::new(FloodFill { rect })));
        host.rect = rect;

        let theme = Theme::default();
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        host.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            &PresenceCtx::empty(),
        );

        let mut r = OffscreenRenderer::new(w, h);
        // Distinct from both the flood colour and the card so a leak from
        // any of the three is identifiable in the failure message.
        let rgba = r.render(&scene, vello::peniko::Color::from_rgb8(0, 0, 0xFF));

        let radius = theme.tokens.radii.xl as f64;
        // A point inside the tile's bounding box but outside its rounded
        // silhouette: one pixel in from the box corner, well inside the
        // corner arc's cut.
        let (cx, cy) = (rect.x0 as u32 + 2, rect.y0 as u32 + 2);
        assert!(
            radius > 8.0,
            "test assumes a visibly rounded card; radii.xl is {radius}",
        );
        let (cr, cg, cb, _) = OffscreenRenderer::sample(&rgba, w, cx, cy);
        assert!(
            cr < 128,
            "the hosted app's fill leaked outside the card's rounded \
             corner at ({cx},{cy}) — got ({cr},{cg},{cb}), which is the \
             app's flood colour. The corner belongs to the frame.",
        );

        // Dead centre is the app: the clip must not have swallowed it.
        let (mr, mg, mb, _) = OffscreenRenderer::sample(&rgba, w, 100, 100);
        assert!(
            mr > 200 && mg < 80 && mb < 80,
            "the hosted app must still paint inside the card; centre is \
             ({mr},{mg},{mb}), expected the app's flood colour",
        );

        // Along the tile's left edge, mid-height, the border is stroked
        // over the app. It must not be pure flood colour.
        let edge_x = rect.x0 as u32;
        let (er, eg, eb, _) = OffscreenRenderer::sample(&rgba, w, edge_x, 100);
        assert!(
            (er, eg, eb) != (0xFF, 0x00, 0x00),
            "the window's border was erased by the hosted app's fill at \
             ({edge_x},100) — the edge that separates a window from the \
             desktop is the frame's to draw, not the app's",
        );
    }

    #[test]
    fn needs_animation_frame_reflects_spawn_anim_state() {
        let mut h = stub_host();
        assert!(
            !h.needs_animation_frame(),
            "static host must not demand animation frames",
        );
        h.start_spawn_animation();
        assert!(
            h.needs_animation_frame(),
            "in-flight spawn must pump the App run loop",
        );
        std::thread::sleep(Duration::from_millis(SPAWN_FADE_MS as u64 + 20));
        assert!(
            !h.needs_animation_frame(),
            "after the spawn window elapses, no more frames are needed",
        );
    }
}
