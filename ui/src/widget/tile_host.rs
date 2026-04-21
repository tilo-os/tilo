use automerge::{AutoCommit, ObjId};
use std::sync::{Arc, OnceLock};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::{Blob, Format, Image};
use winit::event::WindowEvent;
use taffy::{TaffyTree, NodeId, Style, Size, Dimension};
use crate::widget::Widget;
use crate::widget::app_widget::AppWidget;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::presence::PresenceCtx;

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
    Image::new(blob, Format::Rgba8, size, size)
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
        vello::peniko::BlendMode::default(),
        alpha,
        transform,
        &target,
    );
    scene.fill(
        vello::peniko::Fill::NonZero,
        brush_transform,
        sprite,
        None,
        &Rect::new(0.0, 0.0, sprite.width as f64, sprite.height as f64),
    );
    scene.pop_layer();
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

pub struct TileHost {
    content: TileContent,
    rect: Rect,
    /// Set by the parent `WindowGroup` each frame to indicate whether
    /// this tile is the "focused" tile — the concept's
    /// `shadow-[0_0_60px_rgba(99,102,241,0.6)]` bloom
    /// (`WindowFrame.tsx:359`) is painted only when this is true.
    pub focused: bool,
}

impl TileHost {
    pub fn new(content: TileContent) -> Self {
        Self { content, rect: Rect::ZERO, focused: false }
    }
    pub fn kind_string(&self) -> String {
        match &self.content {
            TileContent::App(_) => "App".into(),
            TileContent::Stub { kind, .. } => kind.as_str().into(),
        }
    }
    pub fn content_mut(&mut self) -> &mut TileContent { &mut self.content }
    /// Toggle the indigo-bloom shadow. Called by `WindowGroup::draw`
    /// before dispatching to the active host.
    pub fn set_focused(&mut self, focused: bool) { self.focused = focused; }
}

impl Widget for TileHost {
    fn sync(&mut self, s: &mut AutoCommit, p: &ObjId) { if let TileContent::App(w) = &mut self.content { w.sync(s, p); } }
    fn update(&mut self, s: &mut AutoCommit, p: &ObjId) -> bool { if let TileContent::App(w) = &mut self.content { w.update(s, p) } else { false } }
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
            let child_node = taffy.children(node).unwrap()[0];
            w.apply_layout(taffy, child_node, Point::new(self.rect.x0, self.rect.y0));
        }
    }
    fn draw(&mut self, scene: &mut Scene, transform: Affine, tr: &mut TextRenderer, theme: &Theme, presence: &PresenceCtx) {
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
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            theme.tokens.colors.surfaces.window_card_border,
            None,
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
    }
    fn handle_event(&mut self, event: &WindowEvent, scale_factor: f64, m: &winit::event::Modifiers, s: &mut AutoCommit, p: &ObjId, handled: &mut bool, focus_target: &mut Option<NodeId>, node_id: NodeId, taffy: &TaffyTree<()>) {
        if let TileContent::App(w) = &mut self.content {
            let child_node = taffy.children(node_id).unwrap()[0];
            w.handle_event(event, scale_factor, m, s, p, handled, focus_target, child_node, taffy);
        }
    }
    fn name(&self) -> &str { "TileHost" }
    fn get_rect(&self) -> Rect { self.rect }
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
