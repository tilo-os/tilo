use crate::presence::PresenceCtx;
use crate::record::{RecordCheckout, RecordId, RecordStore};
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::Spinner;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use crate::widget::app_widget::AppWidget;
use automerge::{AutoCommit, ObjId, ROOT};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::{
    Blob, Color, ImageAlphaType, ImageBrush, ImageData as Image, ImageFormat as Format,
};
use winit::event::WindowEvent;

const SPAWN_FADE_MS: f32 = 180.0;
const SPAWN_SCALE_START: f64 = 0.95;

/// One logical pixel: heavier reads as a drawn outline around the app,
/// not the boundary of a surface.
const EDGE_STROKE_WIDTH: f64 = 1.0;

/// Half a pixel heavier than `EDGE_STROKE_WIDTH`, in the focus colour, so
/// the focused surface reads as lit rather than fenced.
const FOCUS_EDGE_STROKE_WIDTH: f64 = 1.6;

/// Ambient shadow sits at alpha 0.25; this adds on top in the focus hue —
/// enough to pick the window out of a strip without becoming the
/// loudest thing on screen.
const FOCUS_BLOOM_ALPHA: f32 = 0.30;

/// 256 px keeps the one-shot RGBA8 allocation tiny (256 KiB) while
/// staying large enough that stretching under a full-height card doesn't
/// show sampling artefacts.
const SHADOW_SPRITE_SIZE: u32 = 256;

/// Gaussian falloff constant `k` (fraction of the sprite's half-extent):
/// `alpha = exp(-((d - d_peak) / k)^2)`. 0.35 lands most of the shadow's
/// energy inside ~60% of the radius — tuned by eye to match Tailwind
/// `shadow-2xl`'s visual density.
const SHADOW_GAUSSIAN_K: f64 = 0.35;

/// Normalised radius where the Gaussian bump peaks. 0.30 keeps the
/// centre transparent (the card paints over nothing, not a darkened
/// underlay) and the sprite edge transparent (no hard halo when
/// stretched).
const SHADOW_PEAK_RADIUS: f64 = 0.30;

/// Rounds a channel-times-alpha product into a colour byte. Every caller
/// passes either `alpha_frac * 255.0` or `channel * alpha_frac` where
/// `alpha_frac` is already clamped to `[0.0, 1.0]` and `channel` is a
/// `u8` widened to `f64`, so `value` is always in `[0.0, 255.0]` — the
/// lint can't see that through the clamp above, but the `.clamp` here
/// makes the saturating float-to-int cast provably safe regardless.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "value is clamped into [0.0, 255.0] on the line above, so the \
              cast can never truncate or flip sign"
)]
fn channel_byte(value: f64) -> u8 {
    value.round().clamp(0.0, 255.0) as u8
}

fn build_shadow_sprite(red: u8, green: u8, blue: u8) -> Image {
    let size = SHADOW_SPRITE_SIZE;
    let size_f = f64::from(size);
    let cx = (size_f - 1.0) * 0.5;
    let cy = (size_f - 1.0) * 0.5;
    let max_radius = cx.max(cy);
    let falloff = SHADOW_GAUSSIAN_K;
    let peak_radius = SHADOW_PEAK_RADIUS;
    let side_len = usize::try_from(size).unwrap_or(usize::MAX);
    let mut data = Vec::<u8>::with_capacity(side_len.saturating_mul(side_len).saturating_mul(4));
    for py in 0..size {
        for px in 0..size {
            let dx = (f64::from(px) - cx) / max_radius;
            let dy = (f64::from(py) - cy) / max_radius;
            let dist = (dx * dx + dy * dy).sqrt();
            let bump = (-((dist - peak_radius) / falloff).powi(2)).exp();
            let inner = (dist / peak_radius).min(1.0);
            let outer = (1.0 - dist).max(0.0);
            let alpha_frac = (bump * inner * outer).clamp(0.0, 1.0);
            data.push(channel_byte(f64::from(red) * alpha_frac));
            data.push(channel_byte(f64::from(green) * alpha_frac));
            data.push(channel_byte(f64::from(blue) * alpha_frac));
            data.push(channel_byte(alpha_frac * 255.0));
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

fn ambient_shadow_sprite() -> &'static Image {
    static SPRITE: OnceLock<Image> = OnceLock::new();
    SPRITE.get_or_init(|| build_shadow_sprite(0, 0, 0))
}

/// Tinted with the theme's own focus colour (`ui/HIG.md` §9.4) — never
/// hard-code it, high-contrast's focus colour is nothing like a violet
/// glow. Cached per RGB triple rather than rebuilt each frame: baking one
/// is a 256x256 pass over 65k pixels, and a session visits few themes.
fn focus_bloom_sprite(tint: Color) -> Image {
    static SPRITES: OnceLock<Mutex<HashMap<[u8; 3], Image>>> = OnceLock::new();
    let rgba = tint.to_rgba8();
    let key = [rgba.r, rgba.g, rgba.b];
    let cache = SPRITES.get_or_init(|| Mutex::new(HashMap::new()));
    let mut cache = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    cache
        .entry(key)
        .or_insert_with(|| build_shadow_sprite(key[0], key[1], key[2]))
        .clone()
}

/// RGB in `sprite` is already tinted; `alpha` only scales opacity.
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
    let sx = tw / f64::from(sprite.width);
    let sy = th / f64::from(sprite.height);
    let brush_transform = translated_and_scaled(transform, target.x0, target.y0, sx, sy);
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
        &Rect::new(0.0, 0.0, f64::from(sprite.width), f64::from(sprite.height)),
    );
    scene.pop_layer();
}

/// `kurbo::Affine` composition is plain f64 matrix math with no panic
/// path; `arithmetic_side_effects` flags the `*` operator overload anyway.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "kurbo::Affine composition is f64 matrix math; it never panics \
              or overflows, unlike integer +/-/*"
)]
fn translated(transform: Affine, x: f64, y: f64) -> Affine {
    transform * Affine::translate((x, y))
}

/// See [`translated`]: translate-then-scale is the same plain f64 math.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "kurbo::Affine composition is f64 matrix math; it never panics \
              or overflows, unlike integer +/-/*"
)]
fn translated_and_scaled(transform: Affine, x: f64, y: f64, sx: f64, sy: f64) -> Affine {
    transform * Affine::translate((x, y)) * Affine::scale_non_uniform(sx, sy)
}

/// See [`translated`]: scaling about a pivot point is translate, scale,
/// translate back — still the same plain f64 matrix math.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "kurbo::Affine composition is f64 matrix math; it never panics \
              or overflows, unlike integer +/-/*"
)]
fn scaled_about(transform: Affine, centre: Point, scale: f64) -> Affine {
    transform
        * Affine::translate((centre.x, centre.y))
        * Affine::scale(scale)
        * Affine::translate((-centre.x, -centre.y))
}

/// `Duration::as_secs_f32` converts precisely with no lossy integer
/// step, unlike `as_millis() as f32` (`u128` -> `f32`), which is what the
/// truncation/precision-loss lints below were actually catching.
fn elapsed_ms_f32(t: Instant) -> f32 {
    t.elapsed().as_secs_f32() * 1000.0
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum UndoStep {
    Undo,
    Redo,
}

fn undo_step(event: &WindowEvent, m: winit::event::Modifiers) -> Option<UndoStep> {
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
pub enum AppKind {
    Terminal,
    Files,
    Editor,
    Browser,
    Settings,
}

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

pub enum TileContent {
    App(Box<dyn AppWidget>),
    Stub {
        kind: AppKind,
        label: String,
    },
    /// Distinct from `Stub`'s "coming soon": `title` names the thing,
    /// `detail` says what's actually the case (e.g. a peer's app not
    /// installed here is absent, not coming soon).
    Notice {
        title: String,
        detail: String,
    },
}

/// A tab is an open thing, not a view onto one shared thing: the document
/// behind it is the tab's own, otherwise two tabs of the same app would
/// share one document, and undo/presence/search with it.
struct Binding {
    store: RecordStore,
    /// Where the tab's content lives while no file names it. Never
    /// changes for the life of the tab.
    tab: RecordCheckout,
    /// `tab.id` until the hosted app declares a file through
    /// [`Widget::record_intent`](crate::widget::Widget::record_intent) —
    /// opening a document *is* moving the tab onto that document's record;
    /// the scratch record it leaves behind is still its own document.
    live: RecordId,
    /// Whether the app has taken this record's document through a first
    /// `sync` yet. What that pass writes (the object a body lives in, the
    /// seed a fresh note carries) is the document's *shape*, not something
    /// the user did, so the history's baseline is taken after it —
    /// otherwise the first Ctrl+Z deletes the body wholesale instead of
    /// reverting what was typed into it.
    established: bool,
}

impl Binding {
    fn follow(&mut self, want: Option<RecordCheckout>) -> RecordId {
        if let Some(want) = want
            && want.id != self.live
        {
            self.store.open_record(&want);
            self.live = want.id;
            self.established = false;
        }
        self.live.clone()
    }
}

pub struct TileHost {
    content: TileContent,
    /// `None` when nothing is bound — a bare `ui` host, a stub card, a
    /// test exercising only paint. Unbound tiles edit whatever document
    /// the parent pass carries.
    binding: Option<Binding>,
    announced: Option<RecordId>,
    /// Set by [`TileHost::replace_content`]; drained by
    /// [`Widget::take_structure_dirty`]. Tells the layout pass this
    /// tile's node shape changed even though no hosted widget reported
    /// anything.
    structure_dirty: bool,
    /// Latched: once this tile has shown something, its window stays on
    /// screen for good. Must survive [`TileHost::replace_content`] — a
    /// peer's already-open window arrives as a card, then gets replaced
    /// by this machine's own not-yet-drawn app instance, and without the
    /// latch the window would blink out and back.
    was_presented: std::cell::Cell<bool>,
    rect: Rect,
    pub focused: bool,
    /// `None` by default, matching the "animations only fire for runtime
    /// insertions" rule `TabBar` uses.
    spawned_at: Option<Instant>,
    /// Pushed down by the parent [`crate::widget::WindowGroup`] every
    /// frame: the pill is where a wayland app-id becomes a name a person
    /// recognises, and resolving it a second time down here would only be
    /// a second chance for the two to disagree.
    opening_name: String,
    opening_spinner: Spinner,
    /// Set on the first frame the opening card is actually painted: a
    /// card nobody saw has nothing to dissolve, and fading one out over
    /// an app that opened in a blink would invent a transition out of a
    /// placeholder that was never on screen.
    opening_shown: bool,
    /// Drives the dissolve; only ever set once `opening_shown` is.
    handed_over_at: Option<Instant>,
}

/// Same length as the tab-close fade — one motion becoming another, short
/// enough nobody is waiting on it.
const HANDOVER_MS: u64 = 180;

/// Mirrors `HANDOVER_MS` as an `f32` for the dissolve's progress ratio —
/// kept as its own literal rather than `HANDOVER_MS as f32` so the
/// conversion is a compile-time constant, not a cast the lints must trust.
const HANDOVER_MS_F32: f32 = 180.0;

/// Larger than the toolkit default so it holds its own under a title on a
/// full-window card, small enough to stay an indicator rather than the
/// subject of the screen.
const OPENING_SPINNER_PX: f64 = 36.0;

/// Mirrors `OPENING_SPINNER_PX` as an `f32`: `Spinner::with_size` takes an
/// `f32`, while every rect computation below stays in `f64` alongside the
/// rest of this widget's geometry.
const OPENING_SPINNER_PX_F32: f32 = 36.0;

const OPENING_SPINNER_GAP: f64 = 24.0;

impl TileHost {
    pub fn new(content: TileContent) -> Self {
        Self {
            content,
            binding: None,
            announced: None,
            structure_dirty: false,
            was_presented: std::cell::Cell::new(false),
            rect: Rect::ZERO,
            focused: false,
            spawned_at: None,
            opening_name: String::new(),
            opening_spinner: Spinner::with_size(OPENING_SPINNER_PX_F32),
            opening_shown: false,
            handed_over_at: None,
        }
    }

    pub fn set_opening_name(&mut self, name: &str) {
        if self.opening_name != name {
            self.opening_name.clear();
            self.opening_name.push_str(name);
        }
    }

    /// Test seam for the parent group's push — a card naming the wrong
    /// app, or an app-id, is the defect this exists to catch.
    #[cfg(test)]
    pub(crate) fn opening_name_for_test(&self) -> &str {
        &self.opening_name
    }

    fn handover_in_flight(&self) -> bool {
        self.handed_over_at
            .is_some_and(|t| t.elapsed() < Duration::from_millis(HANDOVER_MS))
    }

    /// Idempotent on the same record — rebinding would hand the tab a
    /// fresh empty document, and the tree re-binds every tile on rebuild.
    pub fn bind_record(&mut self, store: RecordStore, tab: RecordCheckout) {
        if let Some(b) = &self.binding
            && b.tab.id == tab.id
        {
            return;
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

    fn resolve_record(&mut self) -> Option<RecordId> {
        let want = match &self.content {
            TileContent::App(w) => w.record_intent(),
            TileContent::Stub { .. } | TileContent::Notice { .. } => None,
        };
        let live = self.binding.as_mut()?.follow(want);
        self.announce_scope();
        Some(live)
    }

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
            TileContent::Notice { title, .. } => title.clone(),
        }
    }
    pub fn content_mut(&mut self) -> &mut TileContent {
        &mut self.content
    }
    /// Used when a window announced before it existed (an opening card)
    /// becomes the app; replacing the tab instead would hand focus
    /// elsewhere. Keeps the record binding but resets `announced` so the
    /// new occupant is told its scope, and flags a structural rebuild
    /// since `App` and the card variants have different taffy shapes.
    pub fn replace_content(&mut self, content: TileContent) {
        self.content = content;
        self.announced = None;
        self.structure_dirty = true;
    }
    pub fn content(&self) -> &TileContent {
        &self.content
    }
    pub fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }

    /// A call mid-animation restarts the curve.
    pub fn start_spawn_animation(&mut self) {
        self.spawned_at = Some(Instant::now());
    }

    pub fn spawn_in_flight(&self) -> bool {
        match self.spawned_at {
            Some(t) => elapsed_ms_f32(t) < SPAWN_FADE_MS,
            None => false,
        }
    }

    /// Desktop-space card rect for an app-provided complete semantic canvas.
    /// The provider reports tile-content size; the shell card grows from the
    /// live tile's top-left and never shrinks below the live window surface.
    pub fn complete_content_rect(&self) -> Option<Rect> {
        let TileContent::App(app) = &self.content else {
            return None;
        };
        let size = app.complete_content_size()?;
        if !size.width.is_finite()
            || !size.height.is_finite()
            || size.width <= 0.0
            || size.height <= 0.0
        {
            return None;
        }
        Some(Rect::new(
            self.rect.x0,
            self.rect.y0,
            self.rect.x0 + size.width.max(self.rect.width()),
            self.rect.y0 + size.height.max(self.rect.height()),
        ))
    }

    /// Paint a complete app-provided canvas in the ordinary window card. The
    /// app owns only its semantic pixels; shared clipping, corner radius and
    /// focus edge remain toolkit code, exactly as on screen.
    pub fn draw_complete_content_capture(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) -> Option<Rect> {
        let rect = self.complete_content_rect()?;
        let radius = f64::from(theme.tokens.radii.xl);
        let shape = RoundedRect::from_rect(rect, radius);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.window_card,
            None,
            &shape,
        );
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &shape,
        );
        let painted = match &mut self.content {
            TileContent::App(app) => {
                app.draw_complete_content(scene, rect, transform, tr, theme, presence)
            }
            TileContent::Stub { .. } | TileContent::Notice { .. } => false,
        };
        scene.pop_layer();
        if !painted {
            return None;
        }
        // Export crops to the app bounds, so the live drop shadow is neither
        // visible nor useful. Omitting it also avoids stretching a
        // window-height raster effect across a multi-page document.
        self.draw_edge_at(scene, transform, theme, radius, rect);
        Some(rect)
    }

    /// Exposed for tests to pin the curve shape without peeking into
    /// Vello's scene buffer.
    #[cfg(test)]
    pub fn spawn_alpha(&self) -> Option<f32> {
        self.spawned_at.and_then(|t| {
            let elapsed = elapsed_ms_f32(t);
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
                    b.store
                        .with_history(&id, |doc, history| history.rebase(doc));
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
        let changed = b
            .store
            .with(&id, |doc| w.update(doc, &ROOT))
            .unwrap_or(false);
        // Close the burst against *this* record's history: the host's own
        // commit pass no longer touches the document the tab is editing,
        // and would otherwise leave it open forever.
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
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = match &mut self.content {
            TileContent::App(w) => {
                let child = w.build_layout(taffy, theme, node_map);
                taffy.parent_node(
                    Style {
                        size: Size {
                            width: Dimension::percent(1.0),
                            height: Dimension::percent(1.0),
                        },
                        ..Default::default()
                    },
                    &[child],
                )
            }
            TileContent::Stub { .. } | TileContent::Notice { .. } => taffy.leaf_node(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                ..Default::default()
            }),
        };
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
        if let TileContent::App(w) = &mut self.content {
            // Guards a structural mismatch: content is `App` but the taffy
            // node has no child yet (build_layout ran while content was
            // still `Stub`, or structure_dirty hasn't fired).
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
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        // Clear an expired spawn timestamp before branching, or it stays
        // `Some` forever after the curve ends.
        if let Some(t) = self.spawned_at
            && elapsed_ms_f32(t) >= SPAWN_FADE_MS
        {
            self.spawned_at = None;
        }
        if theme.reduce_motion {
            self.spawned_at = None;
        }
        // Wrap the steady-state paint in one alpha layer + centre-pivot
        // scale so the whole tile fades/scales as one unit.
        if self.spawn_in_flight()
            && let Some(started) = self.spawned_at
        {
            let elapsed = elapsed_ms_f32(started);
            let t = (elapsed / SPAWN_FADE_MS).clamp(0.0, 1.0);
            let eased = 1.0 - (1.0 - t).powi(3);
            let alpha = eased;
            let scale = SPAWN_SCALE_START + (1.0 - SPAWN_SCALE_START) * f64::from(eased);
            // 160 px clip so the shadow sprites (ambient_pad=50,
            // bloom_pad=120) aren't chopped mid-fade.
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
            let anim_transform = scaled_about(transform, centre, scale);
            self.draw_steady_state(scene, anim_transform, tr, theme, presence);
            scene.pop_layer();
            return;
        }
        self.draw_steady_state(scene, transform, tr, theme, presence);
    }
    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        mods: &winit::event::Modifiers,
        s: &mut AutoCommit,
        p: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        let live = self.resolve_record();
        let TileContent::App(app) = &mut self.content else {
            return;
        };
        // Same structural-mismatch guard as apply_layout above.
        let children = taffy.children(node_id).unwrap_or_default();
        let Some(&child_node) = children.first() else {
            return;
        };
        let (Some(id), Some(bind)) = (live, &self.binding) else {
            app.handle_event(
                event,
                scale_factor,
                mods,
                s,
                p,
                handled,
                focus_target,
                child_node,
                taffy,
            );
            return;
        };
        // An `UndoEntry` is a pair of change hashes inside *this* record's
        // change graph, so running it against another tab's document
        // either does nothing or reverts the wrong thing.
        if app.is_focused()
            && let Some(step) = undo_step(event, *mods)
        {
            bind.store.with_history(&id, |doc, history| {
                let author = crate::collaboration::local_user_id();
                // Close any open burst first, so Ctrl+Z right after typing
                // reverts what was just typed, not the action before it —
                // free when the burst is already closed.
                if matches!(step, UndoStep::Undo) {
                    history.commit_action_authored(
                        doc,
                        None,
                        app.get_state(),
                        Some(author.clone()),
                    );
                }
                match step {
                    UndoStep::Undo => history.undo_as(doc, Some(&author), None, app.get_state()),
                    UndoStep::Redo => history.redo_as(doc, Some(&author), None, app.get_state()),
                }
                .map(|entry| {
                    if let Some(meta) = entry.widget_metadata.as_deref() {
                        app.set_state(meta);
                    }
                })
            });
            app.on_commit();
            *handled = true;
            return;
        }
        bind.store.with(&id, |doc| {
            app.handle_event(
                event,
                scale_factor,
                mods,
                doc,
                &ROOT,
                handled,
                focus_target,
                child_node,
                taffy,
            );
        });
    }
    fn name(&self) -> &'static str {
        "TileHost"
    }
    /// Only an app can answer anything but `true` — a stub/notice is
    /// painted by this process and is on screen the moment the tile is.
    fn presented(&self) -> bool {
        if self.was_presented.get() {
            return true;
        }
        let now = match &self.content {
            TileContent::App(w) => w.presented(),
            TileContent::Stub { .. } | TileContent::Notice { .. } => true,
        };
        self.was_presented.set(now);
        now
    }
    /// A tile that has ever presented is never opening again: the latch
    /// stops a window whose buffer briefly stops fitting its tab from
    /// falling back to a placeholder the person already watched go away.
    fn opening(&self) -> bool {
        if self.was_presented.get() {
            return false;
        }
        match &self.content {
            TileContent::App(w) => w.opening(),
            TileContent::Stub { .. } | TileContent::Notice { .. } => false,
        }
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
    fn take_floating_surface_request(
        &mut self,
    ) -> Option<crate::floating_surface::FloatingSurfaceRequest> {
        match &mut self.content {
            TileContent::App(widget) => widget.take_floating_surface_request(),
            TileContent::Stub { .. } | TileContent::Notice { .. } => None,
        }
    }
    fn take_capture_studio_request(&mut self) -> Option<super::CaptureMode> {
        match &mut self.content {
            TileContent::App(widget) => widget.take_capture_studio_request(),
            TileContent::Stub { .. } | TileContent::Notice { .. } => None,
        }
    }
    fn take_floating_surface_source_updates(&mut self) -> Vec<(String, Rect)> {
        match &mut self.content {
            TileContent::App(widget) => widget.take_floating_surface_source_updates(),
            TileContent::Stub { .. } | TileContent::Notice { .. } => Vec::new(),
        }
    }
    fn draw_floating_surfaces(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if let TileContent::App(widget) = &mut self.content {
            widget.draw_floating_surfaces(scene, transform, text_renderer, theme, presence);
        }
    }
    fn set_floating_surface_bounds(&mut self, bounds: Rect) {
        if let TileContent::App(widget) = &mut self.content {
            widget.set_floating_surface_bounds(bounds);
        }
    }
    fn floating_surface_presentations(
        &self,
    ) -> Vec<crate::floating_surface::FloatingSurfacePresentation> {
        match &self.content {
            TileContent::App(widget) => widget.floating_surface_presentations(),
            TileContent::Stub { .. } | TileContent::Notice { .. } => Vec::new(),
        }
    }
    fn active_floating_surface_ids(&self) -> Vec<String> {
        match &self.content {
            TileContent::App(widget) => widget.active_floating_surface_ids(),
            TileContent::Stub { .. } | TileContent::Notice { .. } => Vec::new(),
        }
    }
    fn on_floating_surface_host_message(
        &mut self,
        message: &crate::floating_surface::FloatingSurfaceHostMessage,
    ) {
        if let TileContent::App(widget) = &mut self.content {
            widget.on_floating_surface_host_message(message);
        }
    }
    fn wants_pointer_capture(&self) -> bool {
        match &self.content {
            TileContent::App(widget) => widget.wants_pointer_capture(),
            TileContent::Stub { .. } | TileContent::Notice { .. } => false,
        }
    }
    /// Presence identity is the record shown, not the tile — the only
    /// thing that tells two tabs of one app apart, since a peer's caret is
    /// a position in *a document*. Unbound tiles report nothing.
    fn presence_key(&self) -> Option<crate::presence::PresenceKey> {
        self.binding
            .as_ref()
            .map(|b| crate::presence::PresenceKey::Tab {
                id: b.live.as_str().to_string(),
            })
    }
    /// A bound tile has already committed the app's burst against its own
    /// record in [`Widget::update`]; saying "yes" here would commit the
    /// host's own, unrelated document too.
    fn needs_commit(&self) -> bool {
        if self.binding.is_some() {
            return false;
        }
        match &self.content {
            TileContent::App(w) => w.needs_commit(),
            TileContent::Stub { .. } | TileContent::Notice { .. } => false,
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
        // Both flags drained, not short-circuited, so neither can be left
        // set behind the other.
        let swapped = std::mem::take(&mut self.structure_dirty);
        let inner = match &mut self.content {
            TileContent::App(w) => w.take_structure_dirty(),
            _ => false,
        };
        swapped || inner
    }
    fn needs_animation_frame(&self) -> bool {
        // `TileHost` has no `children()`, so the default recursive
        // implementation would always say false; override so the spawn
        // fade and a hosted app's own signal both bubble up. The opening
        // card's indicator and its dissolve are motion too, and neither is
        // visible through `children()` either.
        let inner = match &self.content {
            TileContent::App(w) => w.needs_animation_frame(),
            TileContent::Stub { .. } | TileContent::Notice { .. } => false,
        };
        self.spawn_in_flight() || Widget::opening(self) || self.handover_in_flight() || inner
    }
}

impl TileHost {
    /// Ambient drop shadow, and — only when focused — the coloured bloom
    /// that reads as a window being lit rather than fenced. Split out of
    /// `draw_steady_state` to keep that function's line count sane; paint
    /// order (ambient, then bloom, then the card on top) is unchanged.
    ///
    /// Deliberately a deeper shadow rather than a border: a strip of
    /// non-overlapping windows has no z-order to read focus from, and
    /// fencing off the content area would draw a shape belonging to
    /// nothing else on screen. Lighting the window's own footprint keeps
    /// the signal attached to the object it describes.
    fn draw_shadows_at(&self, scene: &mut Scene, transform: Affine, theme: &Theme, rect: Rect) {
        // Ambient `shadow-2xl` (`0 25px 50px -12px rgba(0,0,0,0.25)`): 25 px
        // downward offset, ~50 px bleed so the falloff clears the corners.
        let ambient_pad = 50.0_f64;
        let ambient_y = 25.0_f64;
        let ambient_target = Rect::new(
            rect.x0 - ambient_pad,
            rect.y0 - ambient_pad + ambient_y,
            rect.x1 + ambient_pad,
            rect.y1 + ambient_pad + ambient_y,
        );
        draw_shadow_sprite(
            scene,
            transform,
            ambient_shadow_sprite(),
            ambient_target,
            0.25,
        );
        if self.focused {
            let bloom_pad = 120.0_f64;
            let bloom_target = Rect::new(
                rect.x0 - bloom_pad,
                rect.y0 - bloom_pad,
                rect.x1 + bloom_pad,
                rect.y1 + bloom_pad,
            );
            draw_shadow_sprite(
                scene,
                transform,
                &focus_bloom_sprite(theme.tokens.colors.border_focus),
                bloom_target,
                FOCUS_BLOOM_ALPHA,
            );
        }
    }

    fn draw_shadows(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        self.draw_shadows_at(scene, transform, theme, self.rect);
    }

    /// The window's outline. Painted *outside* the card's silhouette, safe
    /// from app content by the clip in `draw_steady_state` rather than a
    /// paint-order race: a stroke over unknown app content can't pick one
    /// alpha that reads right on every ground, but the desktop outside is
    /// known, so `Theme::window_edge` can derive one that does. Inflated by
    /// half the stroke width so the band sits flush with zero overlap; the
    /// radius grows by the same half so it stays concentric at the
    /// corners. Focus reuses this same edge (colour + weight) rather than a
    /// second ring, keeping the signal part of the window's own silhouette.
    fn draw_edge_at(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        radius: f64,
        rect: Rect,
    ) {
        let (edge_width, edge_color) = if self.focused {
            (FOCUS_EDGE_STROKE_WIDTH, theme.tokens.colors.border_focus)
        } else {
            (EDGE_STROKE_WIDTH, theme.window_edge())
        };
        let half = edge_width / 2.0;
        let edge = RoundedRect::from_rect(rect.inflate(half, half), radius + half);
        scene.stroke(
            &vello::kurbo::Stroke::new(edge_width),
            transform,
            edge_color,
            None,
            &edge,
        );
    }

    fn draw_edge(&self, scene: &mut Scene, transform: Affine, theme: &Theme, radius: f64) {
        self.draw_edge_at(scene, transform, theme, radius, self.rect);
    }

    /// Ambient shadow → focus bloom → white card + border → hosted app or
    /// stub contents. Extracted from `draw` so the spawn-fade branch can
    /// wrap the same paint under one scaled layer.
    fn draw_steady_state(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let radius = f64::from(theme.tokens.radii.xl);
        self.draw_shadows(scene, transform, theme);
        let shape = RoundedRect::from_rect(self.rect, radius);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.window_card,
            None,
            &shape,
        );
        // Hosted apps fill their own background as a plain rectangle (an
        // app can't know it's being framed); without this clip the first
        // fill squares off the window's rounded corners.
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &shape,
        );
        // An app asked for but not arrived gets the card in place of the
        // app, not behind it: its early buffer is the client's own size,
        // not the tab's, and painting it would show the window visibly
        // resizing a moment after opening.
        if Widget::opening(self) {
            self.opening_shown = true;
            draw_opening_card(
                scene,
                transform,
                tr,
                theme,
                presence,
                self.rect,
                &self.opening_name,
                &mut self.opening_spinner,
            );
        } else {
            // Dissolve the card over the arrived window rather than cut
            // it away, so the tile is never briefly empty.
            if self.opening_shown && self.handed_over_at.is_none() {
                self.handed_over_at = Some(Instant::now());
            }
            match &mut self.content {
                TileContent::App(w) => w.draw(scene, transform, tr, theme, presence),
                TileContent::Stub { kind, label } => {
                    let detail = format!("{} coming soon", kind.as_str());
                    draw_card_text(scene, transform, tr, theme, self.rect, label, &detail);
                }
                TileContent::Notice { title, detail } => {
                    draw_card_text(scene, transform, tr, theme, self.rect, title, detail);
                }
            }
            // Reduced motion takes the cut — the window just being there
            // is the honest end state.
            if theme.reduce_motion {
                self.opening_shown = false;
                self.handed_over_at = None;
            } else if let Some(started) = self.handed_over_at {
                let t = elapsed_ms_f32(started) / HANDOVER_MS_F32;
                if t >= 1.0 {
                    self.opening_shown = false;
                    self.handed_over_at = None;
                } else {
                    scene.push_layer(
                        vello::peniko::Fill::NonZero,
                        vello::peniko::BlendMode::default(),
                        1.0 - t,
                        transform,
                        &shape,
                    );
                    draw_opening_card(
                        scene,
                        transform,
                        tr,
                        theme,
                        presence,
                        self.rect,
                        &self.opening_name,
                        &mut self.opening_spinner,
                    );
                    scene.pop_layer();
                }
            }
        }
        scene.pop_layer();
        self.draw_edge(scene, transform, theme, radius);
    }
}

/// No sentence, deliberately: the app's name over a working indicator is
/// the only two things someone waiting for a window needs. Paints its own
/// ground because it is also the top half of the dissolve that retires
/// it, covering the arrived window underneath while still fading.
// Standard immediate-mode paint-helper signature for this vello renderer
// (scene + transform + text_renderer + theme, plus this draw's own
// content); every parameter is independently load-bearing.
#[allow(clippy::too_many_arguments)]
fn draw_opening_card(
    scene: &mut Scene,
    transform: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    presence: &PresenceCtx,
    rect: Rect,
    name: &str,
    spinner: &mut Spinner,
) {
    scene.fill(
        vello::peniko::Fill::NonZero,
        transform,
        theme.tokens.colors.surfaces.window_card,
        None,
        &rect,
    );
    // Measured against the card, not a fixed ramp slot: `neutral[1]` is
    // the *dark* end of a dark theme's ramp, and on that theme's own dark
    // card the name would land at ~1.05:1 contrast — invisible on screen.
    let on_card = theme.on_card().ink;
    let size = theme.tokens.typography.scale.xl;
    let cx = rect.center().x;
    // Above centre, so name + indicator reads as one centred pair rather
    // than the name alone with the indicator hanging below.
    let y = rect.center().y - f64::from(size);
    if !name.is_empty() {
        let (tw, _) = tr.measure(name, size, None);
        tr.draw_text(
            scene,
            name,
            size,
            on_card,
            translated(transform, cx - f64::from(tw) / 2.0, y),
            None,
        );
    }
    let top = y + f64::from(size) + OPENING_SPINNER_GAP;
    spinner.set_rect(Rect::new(
        cx - OPENING_SPINNER_PX / 2.0,
        top,
        cx + OPENING_SPINNER_PX / 2.0,
        top + OPENING_SPINNER_PX,
    ));
    Widget::draw(spinner, scene, transform, tr, theme, presence);
}

/// One function so the two callers (`Stub`, `Notice`) can't drift apart —
/// only the sentence they supply is meant to read as meaningful.
fn draw_card_text(
    scene: &mut Scene,
    transform: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    title: &str,
    detail: &str,
) {
    // Derived from the card's own luminance: it's a black surface under
    // high-contrast and white elsewhere, so a fixed ramp slot is only
    // legible on one of them.
    let card = theme.tokens.colors.surfaces.window_card;
    let on_card = theme.on_card().ink;
    // The floor here is measured for an ideal glyph but a real one
    // anti-aliases: derived at 4.5:1 it measured 3.8:1 on screen against
    // the light card, a tier under the floor it was picked to clear.
    let on_card_dim = theme
        .on_surface_at(card, crate::tokens::colors::ON_CARD_SMALL_TEXT_CONTRAST)
        .ink_dim;
    let text_size = theme.tokens.typography.scale.xl;
    let (tw, _) = tr.measure(title, text_size, None);
    let x = rect.center().x - f64::from(tw) / 2.0;
    let y = rect.center().y - f64::from(text_size) / 2.0;
    tr.draw_text(
        scene,
        title,
        text_size,
        on_card,
        translated(transform, x, y),
        None,
    );
    if detail.is_empty() {
        return;
    }
    let sm = theme.tokens.typography.scale.sm;
    let (sw, _) = tr.measure(detail, sm, None);
    let sx = rect.center().x - f64::from(sw) / 2.0;
    tr.draw_text(
        scene,
        detail,
        sm,
        on_card_dim,
        translated(transform, sx, y + f64::from(text_size) + 8.0),
        None,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn sprite_alpha_at(sprite: &Image, x: u32, y: u32) -> u8 {
        let bytes: &[u8] = sprite.data.data();
        let idx = y
            .saturating_mul(sprite.width)
            .saturating_add(x)
            .saturating_mul(4)
            .saturating_add(3);
        let idx = usize::try_from(idx).unwrap_or(usize::MAX);
        bytes.get(idx).copied().unwrap_or(0)
    }

    /// Test fixtures below deal only in small, non-negative pixel
    /// coordinates (well under `u32::MAX`), so these narrowing casts
    /// can't truncate or flip sign in practice — the lint can't see that
    /// through the float math or fixture constants.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "test fixture coordinates are small non-negative constants"
    )]
    fn px(v: f64) -> u32 {
        v as u32
    }

    /// See [`px`]: the millisecond durations used in these tests are
    /// small positive compile-time constants.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "duration constants used in these tests are small positive values"
    )]
    fn millis(v: f32) -> u64 {
        v as u64
    }

    #[test]
    fn shadow_sprite_peaks_in_interior_not_centre_or_edge() {
        let sprite = ambient_shadow_sprite();
        let size = sprite.width;
        let cy = size / 2;
        // Centre and outer edge are guaranteed zero by the `inner`/
        // `outer` shoulder terms.
        let a_centre = sprite_alpha_at(sprite, size / 2, cy);
        let a_edge = sprite_alpha_at(sprite, size - 1, cy);
        // 0.3 × diameter puts the sample on the Gaussian peak ring;
        // 0.9 × diameter sits in the decayed tail.
        let r_peak = px(f64::from(size) * 0.3);
        let r_tail = px(f64::from(size) * 0.9);
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

    /// Writes one string into whatever document it is handed, and
    /// optionally declares a file it has open.
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
            let n = taffy.leaf_node(Style::default());
            node_map.insert(n, std::ptr::from_mut::<dyn Widget>(self));
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
        fn name(&self) -> &'static str {
            "Scribe"
        }
        fn get_rect(&self) -> Rect {
            Rect::ZERO
        }
        fn record_intent(&self) -> Option<RecordCheckout> {
            self.opens.as_ref().map(|p| {
                RecordCheckout::new(
                    RecordId::for_file(p),
                    crate::record::RecordKind::new("file"),
                )
            })
        }
    }

    impl crate::widget::app_widget::AppWidget for Scribe {
        fn app_kind(&self) -> AppKind {
            AppKind::Editor
        }
        fn app_title(&self) -> &'static str {
            "Scribe"
        }
        fn app_icon(&self) -> crate::widget::app_widget::IconDescriptor {
            crate::widget::app_widget::IconDescriptor::Glyph("S")
        }
        fn permission_hint(&self) -> Option<AppKind> {
            None
        }
    }

    struct FloatingProbe {
        acknowledged: std::rc::Rc<std::cell::Cell<bool>>,
        update: Option<Rect>,
    }

    impl Widget for FloatingProbe {
        fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &Theme,
            node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            let node = taffy.leaf_node(Style::default());
            node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
            node
        }
        fn apply_layout(&mut self, _taffy: &TaffyTree<()>, _node: NodeId, _origin: Point) {}
        fn draw(
            &mut self,
            _scene: &mut Scene,
            _transform: Affine,
            _tr: &mut TextRenderer,
            _theme: &Theme,
            _presence: &PresenceCtx,
        ) {
        }
        fn handle_event(
            &mut self,
            _event: &WindowEvent,
            _scale_factor: f64,
            _modifiers: &winit::event::Modifiers,
            _state: &mut AutoCommit,
            _path: &ObjId,
            _handled: &mut bool,
            _focus_target: &mut Option<NodeId>,
            _node_id: NodeId,
            _taffy: &TaffyTree<()>,
        ) {
        }
        fn name(&self) -> &'static str {
            "FloatingProbe"
        }
        fn get_rect(&self) -> Rect {
            Rect::ZERO
        }
        fn take_floating_surface_source_updates(&mut self) -> Vec<(String, Rect)> {
            self.update
                .take()
                .map(|rect| vec![("probe-pip".into(), rect)])
                .unwrap_or_default()
        }
        fn active_floating_surface_ids(&self) -> Vec<String> {
            vec!["probe-pip".into()]
        }
        fn on_floating_surface_host_message(
            &mut self,
            message: &crate::floating_surface::FloatingSurfaceHostMessage,
        ) {
            if matches!(
                message,
                crate::floating_surface::FloatingSurfaceHostMessage::Adopted { id, .. }
                    if id == "probe-pip"
            ) {
                self.acknowledged.set(true);
            }
        }
    }

    impl crate::widget::app_widget::AppWidget for FloatingProbe {
        fn app_kind(&self) -> AppKind {
            AppKind::Browser
        }
        fn app_title(&self) -> &'static str {
            "Floating probe"
        }
        fn app_icon(&self) -> crate::widget::app_widget::IconDescriptor {
            crate::widget::app_widget::IconDescriptor::Glyph("P")
        }
        fn permission_hint(&self) -> Option<AppKind> {
            None
        }
    }

    #[test]
    fn tile_host_forwards_the_complete_floating_surface_lifecycle() {
        let acknowledged = std::rc::Rc::new(std::cell::Cell::new(false));
        let source = Rect::new(20.0, 30.0, 420.0, 255.0);
        let mut host = TileHost::new(TileContent::App(Box::new(FloatingProbe {
            acknowledged: acknowledged.clone(),
            update: Some(source),
        })));

        assert_eq!(host.active_floating_surface_ids(), vec!["probe-pip"]);
        assert_eq!(
            host.take_floating_surface_source_updates(),
            vec![("probe-pip".into(), source)]
        );
        assert!(host.take_floating_surface_source_updates().is_empty());

        host.on_floating_surface_host_message(
            &crate::floating_surface::FloatingSurfaceHostMessage::Adopted {
                id: "probe-pip".into(),
                rect: [100.0, 80.0, 500.0, 305.0],
            },
        );
        assert!(acknowledged.get());
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
        std::thread::sleep(Duration::from_millis(millis(SPAWN_FADE_MS) + 20));
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
        // Indirect: no direct hook to check the draw path skips the
        // wrapper layer, so this pins spawn_in_flight/spawn_alpha instead
        // — guards against the wrapper becoming always-on and costing an
        // extra layer + affine compose every frame.
        let h = stub_host();
        assert!(!h.spawn_in_flight(), "fresh host must not be in flight");
        assert!(
            h.spawn_alpha().is_none(),
            "fresh host must report no spawn alpha",
        );
    }

    /// Fills its entire rect with one flat opaque colour, exactly as
    /// `FileExplorerWidget::paint_background` and every other hosted
    /// app's background pass does. Module-scoped (rather than nested in
    /// its one test) purely to keep that test's line count sane.
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
            let n = taffy.leaf_node(Style::default());
            node_map.insert(n, std::ptr::from_mut::<dyn Widget>(self));
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
        fn name(&self) -> &'static str {
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
        fn app_title(&self) -> &'static str {
            "FloodFill"
        }
        fn app_icon(&self) -> crate::widget::app_widget::IconDescriptor {
            crate::widget::app_widget::IconDescriptor::Glyph("F")
        }
        fn permission_hint(&self) -> Option<AppKind> {
            None
        }
    }

    /// An app that fills its whole rect (as every hosted app does, since
    /// it can't know it's being framed) must not square off the window's
    /// rounded corners or paint over its border — both are the frame's.
    #[test]
    fn a_hosted_app_cannot_square_off_the_window_or_erase_its_border() {
        use crate::offscreen::OffscreenRenderer;

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

        let radius = f64::from(theme.tokens.radii.xl);
        // A point inside the tile's bounding box but outside its rounded
        // silhouette: one pixel in from the box corner, well inside the
        // corner arc's cut.
        let (cx, cy) = (px(rect.x0) + 2, px(rect.y0) + 2);
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

        // One pixel out from the boundary is the frame's edge: it must be
        // neither the app (flood escaped its clip) nor bare desktop (no
        // edge there at all).
        let edge_x = px(rect.x0) - 1;
        let (er, eg, eb, _) = OffscreenRenderer::sample(&rgba, w, edge_x, 100);
        assert!(
            (er, eg, eb) != (0xFF, 0x00, 0x00),
            "the hosted app's fill leaked onto the window's edge at \
             ({edge_x},100) — got ({er},{eg},{eb}), the app's flood colour",
        );
        assert!(
            (er, eg, eb) != (0x00, 0x00, 0xFF),
            "the window has no edge at ({edge_x},100) — the pixel is bare \
             desktop, so nothing separates the window from it",
        );

        // ...and the first pixel *inside* the boundary is the app's, whole.
        // The frame stops at the silhouette.
        let inside_x = px(rect.x0);
        let (nr, ng, nb, _) = OffscreenRenderer::sample(&rgba, w, inside_x, 100);
        assert_eq!(
            (nr, ng, nb),
            (0xFF, 0x00, 0x00),
            "the frame tinted a pixel the app owns at ({inside_x},100) — got \
             ({nr},{ng},{nb}), expected the app's flood colour",
        );
    }

    /// Module-scoped alongside [`FloodFill`] purely to keep its one test's
    /// line count sane.
    struct Flood {
        rect: Rect,
        color: vello::peniko::Color,
    }
    impl Widget for Flood {
        fn sync(&mut self, _s: &mut AutoCommit, _p: &ObjId) {}
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &Theme,
            node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            let n = taffy.leaf_node(Style::default());
            node_map.insert(n, std::ptr::from_mut::<dyn Widget>(self));
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
                self.color,
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
        fn name(&self) -> &'static str {
            "Flood"
        }
        fn get_rect(&self) -> Rect {
            self.rect
        }
    }
    impl crate::widget::app_widget::AppWidget for Flood {
        fn app_kind(&self) -> AppKind {
            AppKind::Editor
        }
        fn app_title(&self) -> &'static str {
            "Flood"
        }
        fn app_icon(&self) -> crate::widget::app_widget::IconDescriptor {
            crate::widget::app_widget::IconDescriptor::Glyph("F")
        }
        fn permission_hint(&self) -> Option<AppKind> {
            None
        }
    }

    /// The window's edge must never tint a pixel the hosted app owns —
    /// stroking a constant over unknown app content reads right on one
    /// ground and wrong on the rest of the ramp.
    #[test]
    fn the_window_edge_never_tints_a_pixel_the_app_owns() {
        use crate::offscreen::OffscreenRenderer;

        // Spanning the ramp: a dark app, two mid grounds, and the light
        // chrome a word processor paints.
        let grounds: [(u8, u8, u8); 4] = [
            (0x10, 0x10, 0x12),
            (0x40, 0x40, 0x44),
            (0x80, 0x80, 0x84),
            (0xC0, 0xC0, 0xC4),
        ];
        let (w, h) = (200u32, 200u32);
        let rect = Rect::new(50.0, 50.0, 150.0, 150.0);
        let theme = Theme::default();

        for (r8, g8, b8) in grounds {
            let color = vello::peniko::Color::from_rgb8(r8, g8, b8);
            let mut host = TileHost::new(TileContent::App(Box::new(Flood { rect, color })));
            host.rect = rect;
            let mut tr = TextRenderer::new();
            let mut scene = Scene::new();
            host.draw(
                &mut scene,
                Affine::IDENTITY,
                &mut tr,
                &theme,
                &PresenceCtx::empty(),
            );
            let mut renderer = OffscreenRenderer::new(w, h);
            let rgba = renderer.render(&scene, vello::peniko::Color::from_rgb8(0, 0, 0xFF));

            // Both vertical edges at mid-height, and the horizontal edges at
            // mid-width: the first pixel inside the silhouette on every side.
            let probes = [
                ("left", px(rect.x0), 100u32),
                ("right", px(rect.x1) - 1, 100u32),
                ("top", 100u32, px(rect.y0)),
                ("bottom", 100u32, px(rect.y1) - 1),
            ];
            for (side, px, py) in probes {
                let (sr, sg, sb, _) = OffscreenRenderer::sample(&rgba, w, px, py);
                assert_eq!(
                    (sr, sg, sb),
                    (r8, g8, b8),
                    "the frame tinted the app's own pixel on the {side} edge \
                     at ({px},{py}): app painted ({r8},{g8},{b8}), the render \
                     shows ({sr},{sg},{sb}). The window's edge belongs outside \
                     the card, on ground the shell knows — over app content it \
                     is a constant on an unknown surface",
                );
            }
        }
    }

    /// Focus is a change to the window's own edge, staying inside the
    /// card's silhouette — not a square fence around the bounding box,
    /// which would double-line the rounded corner and paint on desktop
    /// diagonally outside it. Checked at the edge's flat and diagonally
    /// outside the corner, across every theme preset.
    #[test]
    fn focus_lights_the_card_s_edge_and_never_squares_off_its_corner() {
        use crate::offscreen::OffscreenRenderer;

        let (width, height) = (200u32, 200u32);
        let rect = Rect::new(50.0, 50.0, 150.0, 150.0);
        // Desktop stand-in. Deliberately a colour no palette contains, so
        // "still desktop" cannot be confused with "some theme token".
        let ground = vello::peniko::Color::from_rgb8(0x00, 0xFF, 0x00);

        for theme in crate::theme::ALL_PRESETS_FOR_TEST.iter().map(|f| f()) {
            let name = theme.name.clone();
            let focus = theme.tokens.colors.border_focus;
            let (fr, fg, fb) = {
                let rgba = focus.to_rgba8();
                (rgba.r, rgba.g, rgba.b)
            };

            let shot = |focused: bool| {
                let mut host = TileHost::new(TileContent::Stub {
                    kind: AppKind::Editor,
                    label: String::new(),
                });
                host.rect = rect;
                host.set_focused(focused);
                let mut tr = TextRenderer::new();
                let mut scene = Scene::new();
                host.draw(
                    &mut scene,
                    Affine::IDENTITY,
                    &mut tr,
                    &theme,
                    &PresenceCtx::empty(),
                );
                let mut renderer = OffscreenRenderer::new(width, height);
                renderer.render(&scene, ground)
            };

            let lit = shot(true);
            let dark = shot(false);

            // Mid-span of the top edge: the band the stroke occupies sits
            // just outside the card boundary.
            let (sample_x, sample_y) = (100u32, px(rect.y0) - 1);
            let (lr, lg, lb, _) = OffscreenRenderer::sample(&lit, width, sample_x, sample_y);
            assert_eq!(
                (lr, lg, lb),
                (fr, fg, fb),
                "{name}: the focused window's edge is not the theme's focus                  colour at ({sample_x},{sample_y}) — got ({lr},{lg},{lb}), expected                  ({fr},{fg},{fb}). Focus has to come from the theme: a baked                  constant is right on one palette and meaningless on the rest",
            );
            let (ur, ug, ub, _) = OffscreenRenderer::sample(&dark, width, sample_x, sample_y);
            assert_ne!(
                (ur, ug, ub),
                (fr, fg, fb),
                "{name}: an unfocused window paints the focus colour on its                  edge at ({sample_x},{sample_y}), so focus distinguishes nothing",
            );

            // Well outside the rounded arc, where a square fence would
            // have painted. The ambient shadow legitimately darkens this
            // pixel, so the claim is only that focus colour isn't there,
            // not that it's untouched.
            let (corner_x, corner_y) = (px(rect.x0) - 2, px(rect.y0) - 2);
            for (label, buf) in [("focused", &lit), ("unfocused", &dark)] {
                let (sr, sg, sb, _) = OffscreenRenderer::sample(buf, width, corner_x, corner_y);
                assert_ne!(
                    (sr, sg, sb),
                    (fr, fg, fb),
                    "{name}: the {label} window painted the focus colour at ({corner_x},{corner_y}), outside its own rounded corner on bare desktop. Only a sharp-cornered rectangle drawn around the window reaches that pixel, which is the artefact this replaced",
                );
                assert!(
                    sg > sr && sg > sb,
                    "{name}: the {label} window covered the desktop at ({corner_x},{corner_y}) with ({sr},{sg},{sb}). Outside its rounded corner the desktop must still read as the desktop, shadowed at most, never stroked over",
                );
            }
        }
    }

    /// An app that has been asked for and has not drawn: the two things
    /// its widget is allowed to say about itself.
    struct Starting {
        rect: Rect,
        drawn: std::rc::Rc<std::cell::Cell<bool>>,
        presented: bool,
        opening: bool,
    }

    impl Widget for Starting {
        fn sync(&mut self, _s: &mut AutoCommit, _p: &ObjId) {}
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _t: &Theme,
            _m: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            taffy.leaf_node(Style::default())
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
            self.drawn.set(true);
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                STARTING_FLOOD,
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
        fn name(&self) -> &'static str {
            "Starting"
        }
        fn get_rect(&self) -> Rect {
            self.rect
        }
        fn presented(&self) -> bool {
            self.presented
        }
        fn opening(&self) -> bool {
            self.opening
        }
    }

    impl crate::widget::app_widget::AppWidget for Starting {
        fn app_kind(&self) -> AppKind {
            AppKind::Files
        }
        fn app_title(&self) -> &'static str {
            "Starting"
        }
        fn app_icon(&self) -> crate::widget::app_widget::IconDescriptor {
            crate::widget::app_widget::IconDescriptor::Glyph("S")
        }
        fn permission_hint(&self) -> Option<AppKind> {
            None
        }
    }

    /// Loud and unmistakable, so a single sampled pixel says whether the
    /// app painted.
    const STARTING_FLOOD: vello::peniko::Color = vello::peniko::Color::from_rgb8(0xFF, 0x00, 0x00);

    fn starting_host(
        presented: bool,
        opening: bool,
    ) -> (TileHost, std::rc::Rc<std::cell::Cell<bool>>) {
        let rect = Rect::new(50.0, 50.0, 150.0, 150.0);
        let drawn = std::rc::Rc::new(std::cell::Cell::new(false));
        let mut host = TileHost::new(TileContent::App(Box::new(Starting {
            rect,
            drawn: std::rc::Rc::clone(&drawn),
            presented,
            opening,
        })));
        host.rect = rect;
        (host, drawn)
    }

    fn paint(host: &mut TileHost, theme: &Theme) -> Scene {
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        host.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            theme,
            &PresenceCtx::empty(),
        );
        scene
    }

    /// An unpresented app has only its own early buffer size, so painting
    /// it is the window appearing magnified then snapping down. The frame
    /// holds the place instead, since it's the one thing that knows the
    /// tab's real size.
    #[test]
    fn a_tile_holding_a_place_does_not_paint_the_app_behind_it() {
        let theme = Theme::default();
        let (mut host, drawn) = starting_host(false, true);
        let _ = paint(&mut host, &theme);
        assert!(
            !drawn.get(),
            "the app painted into a tile that is still holding its place; \
             its buffer is not the size of the tab yet, and putting it on \
             screen is the resize-after-open this gate exists to prevent",
        );
    }

    /// And the moment it has something to show, it is what is shown.
    #[test]
    fn a_tile_paints_the_app_as_soon_as_it_has_something_to_show() {
        let theme = Theme::default();
        let (mut host, drawn) = starting_host(true, false);
        let _ = paint(&mut host, &theme);
        assert!(
            drawn.get(),
            "an app with a frame that fits its tab must be what the tile \
             paints; anything else is a placeholder standing in front of a \
             window that is already there",
        );
    }

    /// Its indicator is the only thing distinguishing a wait from a hang,
    /// and nothing else in the tile is animating to trigger a frame
    /// otherwise.
    #[test]
    fn a_tile_holding_a_place_keeps_asking_for_frames() {
        let (host, _) = starting_host(false, true);
        assert!(
            host.needs_animation_frame(),
            "the indicator on the opening card would sit frozen, which \
             reads as an app that has stopped rather than one starting",
        );
    }

    /// A tile that has ever had a window in it never goes back to holding
    /// a place for one — without the latch, a drag on a group edge would
    /// flash the opening card over a window the person is using.
    #[test]
    fn a_tile_that_has_shown_a_window_never_holds_a_place_again() {
        let (host, _) = starting_host(true, false);
        assert!(Widget::presented(&host), "precondition: it presented");
        // The app now claims both, as a client whose buffer stopped
        // matching its tab mid-resize would.
        let (host_again, _) = starting_host(true, true);
        assert!(Widget::presented(&host_again));
        assert!(
            !Widget::opening(&host_again),
            "a tile that has presented must never answer `opening` again — \
             a window the person is looking at would be replaced by a card \
             saying it is on its way",
        );
    }

    /// What is in the tile while the place is held is the frame's own
    /// card, not the desktop showing through.
    #[test]
    fn the_card_that_holds_a_place_is_opaque() {
        use crate::offscreen::OffscreenRenderer;
        let theme = Theme::default();
        let (mut host, _) = starting_host(false, true);
        let scene = paint(&mut host, &theme);
        let mut r = OffscreenRenderer::new(200, 200);
        // A ground colour that appears nowhere else, so a pixel of it in
        // the middle of the tile means the card did not cover it.
        let rgba = r.render(&scene, vello::peniko::Color::from_rgb8(0, 0, 0xFF));
        let (cr, cg, cb, _) = OffscreenRenderer::sample(&rgba, 200, 100, 100);
        assert!(
            !(cr < 40 && cg < 40 && cb > 200),
            "the desktop showed through the middle of a window's own place \
             at (100,100) — got ({cr},{cg},{cb}), the backdrop colour. The \
             card has to hold the space, or the window is a hole until it \
             arrives",
        );
        assert!(
            !(cr > 200 && cg < 80 && cb < 80),
            "the app painted after all; got ({cr},{cg},{cb})",
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
        std::thread::sleep(Duration::from_millis(millis(SPAWN_FADE_MS) + 20));
        assert!(
            !h.needs_animation_frame(),
            "after the spawn window elapses, no more frames are needed",
        );
    }
}
