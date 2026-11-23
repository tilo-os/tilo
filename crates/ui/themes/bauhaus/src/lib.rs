//! Bauhaus: a painter set written only against `tilo_theme_sdk`, built to
//! wasm and loaded by the toolkit like any theme's own set
//! (`docs/architecture/theming.md`, "Part painters").
//!
//! Surfaces are flat colour with hard edges. Floating chrome carries a rule
//! in the accent down its leading edge, and each surface shades towards its
//! ink in ordered-dither bands along its foot. Buttons stand on a hard
//! offset shadow and sink onto it when pressed. All the shading is a 4 by 4
//! Bayer threshold tile drawn texel for texel, so it stays crisp at any
//! scale and costs one pattern fill per band.
//!
//! Every colour comes from the part's tokens, so ink keeps the contrast the
//! toolkit measured. The shading reaches the ink by `depth` at most, which
//! the set keeps small.

use tilo_theme_sdk::kurbo::{Affine, Rect, Shape, Size};
use tilo_theme_sdk::peniko::Color;
use tilo_theme_sdk::{
    Brush, PaintedPart, ParamSpec, Part, PartImage, PartPainter, PartScene, PartState, PartTokens,
    States, Transition,
};

/// The set's parameters, as a theme document states them.
pub mod param {
    /// Tone steps a shaded band takes from the fill to the deepest shade.
    pub const LEVELS: &str = "levels";
    /// How far towards the ink the deepest shade goes, 0 to 1.
    pub const DEPTH: &str = "depth";
    /// Edge of a dither texel, device pixels.
    pub const GRAIN: &str = "grain";
    /// How far a button's shadow is offset, logical pixels.
    pub const SHADOW: &str = "shadow";
}

const LEVELS_DEFAULT: f32 = 4.0;
const DEPTH_DEFAULT: f32 = 0.14;
const GRAIN_DEFAULT: f32 = 2.0;
const SHADOW_DEFAULT: f32 = 3.0;

/// The 4 by 4 Bayer matrix: the order in which texels of a tile turn over
/// as the tone deepens.
const BAYER: [u8; 16] = [0, 8, 2, 10, 12, 4, 14, 6, 3, 11, 1, 9, 15, 7, 13, 5];

/// Bytes of a tile: 16 RGBA texels.
const TILE_BYTES: usize = 64;

/// Width of the accent rule down floating chrome, logical pixels.
const RULE: f64 = 4.0;

/// Height of the shaded foot of a bar, logical pixels.
const BAR_FOOT: f64 = 6.0;

/// Height of the shaded foot of floating chrome, logical pixels: inside the
/// padding below its last row, so no text stands on the shade.
const PANEL_FOOT: f64 = 12.0;

/// The Bauhaus painter set.
#[derive(Clone, Copy, Debug, Default)]
pub struct Bauhaus;

impl PartPainter for Bauhaus {
    fn paint(
        &self,
        part: Part,
        size: Size,
        state: PartState,
        progress: f32,
        tokens: &PartTokens,
    ) -> Option<PaintedPart> {
        let look = Look::of(tokens);
        let rect = size.to_rect();
        let mut scene = PartScene::new();
        match part {
            Part::WindowGround => {
                scene.fill(rect.to_path(0.1), Brush::Solid(tokens.fill));
            }
            Part::BarBody => {
                scene.fill(rect.to_path(0.1), Brush::Solid(tokens.fill));
                let foot = Rect::new(
                    0.0,
                    (size.height - BAR_FOOT).max(0.0),
                    size.width,
                    size.height,
                );
                look.shade(&mut scene, foot, tokens.fill, tokens.ink);
            }
            Part::FloatingPanel => {
                scene.fill(rect.to_path(0.1), Brush::Solid(tokens.fill));
                let foot = Rect::new(
                    0.0,
                    size.height - PANEL_FOOT.min(size.height / 4.0),
                    size.width,
                    size.height,
                );
                look.shade(&mut scene, foot, tokens.fill, tokens.ink);
                let rule = Rect::new(0.0, 0.0, RULE.min(size.width), size.height);
                scene.fill(rule.to_path(0.1), Brush::Solid(tokens.accent));
            }
            Part::ControlFace => {
                if tokens.fill.components[3] > 0.0 {
                    scene.fill(rect.to_path(0.1), Brush::Solid(tokens.fill));
                }
            }
            Part::ButtonContainer => look.button(&mut scene, size, state, progress, tokens),
            _ => return None,
        }
        // The whole rect takes the pointer: the shadow is part of the
        // button, so a press on it lands.
        Some(PaintedPart::new(scene))
    }

    fn params(&self) -> Vec<ParamSpec> {
        vec![
            ParamSpec::new(param::LEVELS, Some(LEVELS_DEFAULT), 2.0..=16.0),
            ParamSpec::new(param::DEPTH, Some(DEPTH_DEFAULT), 0.0..=0.4).usable_within(0.0..=0.25),
            ParamSpec::new(param::GRAIN, Some(GRAIN_DEFAULT), 1.0..=4.0),
            ParamSpec::new(param::SHADOW, Some(SHADOW_DEFAULT), 0.0..=8.0),
        ]
    }

    fn transition(&self, part: Part, _from: States, _to: States) -> Transition {
        match part {
            // A press drops the face onto its shadow in visible steps.
            Part::ButtonContainer => Transition::Stepped {
                steps: 3,
                duration: 0.06,
            },
            _ => Transition::None,
        }
    }
}

/// The parameters a part is drawn with, read from its tokens.
#[derive(Clone, Copy, Debug)]
struct Look {
    levels: f32,
    depth: f32,
    /// Edge of a dither texel, logical pixels.
    texel: f32,
    shadow: f64,
}

impl Look {
    fn of(tokens: &PartTokens) -> Self {
        let scale = if tokens.scale > 0.0 {
            tokens.scale
        } else {
            1.0
        };
        Self {
            levels: tokens
                .param(param::LEVELS, LEVELS_DEFAULT)
                .clamp(2.0, 16.0)
                .round(),
            depth: tokens.param(param::DEPTH, DEPTH_DEFAULT).clamp(0.0, 0.4),
            texel: tokens
                .param(param::GRAIN, GRAIN_DEFAULT)
                .clamp(1.0, 4.0)
                .round()
                / scale,
            shadow: f64::from(tokens.param(param::SHADOW, SHADOW_DEFAULT).clamp(0.0, 8.0)),
        }
    }

    /// `area` shaded from `fill` at its top to `depth` of the way towards
    /// `ink` at its foot, in `levels` dithered bands.
    fn shade(self, scene: &mut PartScene, area: Rect, fill: Color, ink: Color) {
        if self.depth <= 0.0 || area.height() <= 0.0 {
            return;
        }
        let deepest = fill.lerp_rect(ink, self.depth);
        for band in 1..16_u8 {
            let band = f32::from(band);
            if band >= self.levels {
                break;
            }
            let top = area.y0 + area.height() * f64::from(band / self.levels);
            let bottom = area.y0 + area.height() * f64::from((band + 1.0) / self.levels);
            let rows = Rect::new(area.x0, top, area.x1, bottom);
            self.tone(scene, rows, fill, deepest, band / (self.levels - 1.0));
        }
    }

    /// `area` dithered between `from` and `to`, `amount` of the way (0 to
    /// 1, in sixteenths of the tile).
    fn tone(self, scene: &mut PartScene, area: Rect, from: Color, to: Color, amount: f32) {
        scene.fill_placed(
            area.to_path(0.1),
            Brush::Pattern {
                image: dither(from, to, amount),
                scale: self.texel,
            },
            Affine::IDENTITY,
        );
    }

    /// A button: a hard shadow `shadow` down and to the right, the face
    /// above it, and the face sunk onto the shadow while pressed.
    fn button(
        self,
        scene: &mut PartScene,
        size: Size,
        state: PartState,
        progress: f32,
        tokens: &PartTokens,
    ) {
        if tokens.fill.components[3] <= 0.0 {
            // A plain button shows no face at rest; the toolkit's ink is
            // its label.
            return;
        }
        let offset = self.shadow.min(size.width / 4.0).min(size.height / 4.0);
        let pressed_in = |states: States| {
            if states.contains(States::PRESSED) {
                1.0
            } else {
                0.0
            }
        };
        let before = pressed_in(state.was);
        let after = pressed_in(state.now);
        let sink = (before + (after - before) * f64::from(progress.clamp(0.0, 1.0))) * offset;
        let shadow = Rect::new(offset, offset, size.width, size.height);
        if offset > 0.0 {
            let ink = if tokens.border_width > 0.0 {
                tokens.border
            } else {
                tokens.ink
            };
            scene.fill(
                shadow.to_path(0.1),
                Brush::Pattern {
                    image: dither(Color::TRANSPARENT, ink, 0.5),
                    scale: self.texel,
                },
            );
        }
        let face = Rect::new(
            sink,
            sink,
            size.width - offset + sink,
            size.height - offset + sink,
        );
        // `fill` is already the face its state resolved to (hovered,
        // pressed, selected), measured against its ink.
        scene.fill(face.to_path(0.1), Brush::Solid(tokens.fill));
        let edge = if tokens.border_width > 0.0 {
            Some((tokens.border_width, tokens.border))
        } else if tokens.edge.components[3] > 0.0 {
            Some((1.0, tokens.edge))
        } else {
            None
        };
        if let Some((width, ink)) = edge {
            let inset = f64::from(width) / 2.0;
            scene.stroke(face.inset(-inset).to_path(0.1), width, Brush::Solid(ink));
        }
    }
}

/// A 4 by 4 tile, `amount` (0 to 1) of whose texels are `to` and the rest
/// `from`, turned over in Bayer order so the tone is even.
fn dither(from: Color, to: Color, amount: f32) -> PartImage {
    let from = from.to_rgba8();
    let to = to.to_rgba8();
    let mut rgba = Vec::with_capacity(TILE_BYTES);
    for rank in BAYER {
        let texel = if (f32::from(rank) + 0.5) / 16.0 < amount {
            to
        } else {
            from
        };
        rgba.extend([texel.r, texel.g, texel.b, texel.a]);
    }
    PartImage {
        width: 4,
        height: 4,
        rgba,
    }
}

#[cfg(target_arch = "wasm32")]
tilo_theme_sdk::export_painter!(Bauhaus);

#[cfg(test)]
mod tests;
