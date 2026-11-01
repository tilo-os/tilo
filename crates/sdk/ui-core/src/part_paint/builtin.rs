//! The painter sets the toolkit ships: Default, Glass and Block
//! (`docs/architecture/theming.md`, "Part painters"). They implement the
//! same [`PartPainter`] a theme's own set does and draw through the same
//! host; a theme document names them as it would any other set.
//!
//! * **Default** is the design language's look: opaque grounds and faces,
//!   or, given a `blur` or a `tint`, a body that shows the blurred backdrop
//!   under its ground at that tint, and a rest control face that lets the
//!   window ground through at it.
//! * **Glass** is Default with light on it: the backdrop bent at the rim
//!   (`refraction`), a sheen across the upper body (`specular`) and a lit
//!   rim (`edge_highlight`).
//! * **Block** is flat colour, square to the border: no translucency, no
//!   light, no hairline of its own.
//! * **Liquid** is a lens: what lies below bent along the rounded edge as
//!   a convex lens bends it (`refraction` across a `band`), straight in the
//!   flat middle, optionally softened (`blur`) and fringed (`dispersion`),
//!   with a thin specular rim lit from `light_angle` (`edge_highlight`).
//!
//! Each draws the four surface parts (window ground, floating panel, bar
//! body and control face) and leaves the rest to the toolkit.

use std::sync::{Arc, LazyLock};

use vello::kurbo::{Affine, BezPath, Point, Rect, RoundedRect, Shape, Size, Vec2};
use vello::peniko::Color;

use super::{
    Brush, Lens, Op, PaintedPart, PainterSet, ParamSpec, Part, PartPainter, PartScene, PartState,
    PartTokens, Sampling, States, Stop,
};

/// The name a document gives the Default set.
pub const DEFAULT: &str = "default";
/// The name a document gives the Glass set.
pub const GLASS: &str = "glass";
/// The name a document gives the Block set.
pub const BLOCK: &str = "block";
/// The name a document gives the Pixelated set.
pub const PIXELATED: &str = "pixelated";
/// The name a document gives the Shader set.
pub const SHADER: &str = "shader";
/// The name a document gives the Liquid set.
pub const LIQUID: &str = "liquid";

/// Cell band a document may state for the Pixelated set, logical pixels.
pub const CELL_ADMITTED: std::ops::RangeInclusive<f32> = 1.0..=64.0;
/// The cell band the Pixelated set is built at.
pub const CELL_USABLE: std::ops::RangeInclusive<f32> = 2.0..=32.0;
/// Tint band for the Pixelated and Shader sets: the ground's opacity over
/// what they sample. Any, as for Default; legibility is the band's.
pub const COVER_ADMITTED: std::ops::RangeInclusive<f32> = 0.0..=1.0;

/// The Shader set's `scale` band: a cell or pattern size, logical pixels.
pub const SHADER_SCALE_ADMITTED: std::ops::RangeInclusive<f32> = 1.0..=64.0;
/// The Shader set's `phase` band: an offset a pattern starts from.
pub const SHADER_PHASE_ADMITTED: std::ops::RangeInclusive<f32> = 0.0..=1000.0;

/// Blur band a document may state, logical pixels.
pub const BLUR_ADMITTED: std::ops::RangeInclusive<f32> = 0.0..=64.0;
/// Blur band a theme is built at.
pub const BLUR_USABLE: std::ops::RangeInclusive<f32> = 0.0..=48.0;
/// Tint band: any. Legibility is the backdrop band's, not the tint's.
pub const TINT_ADMITTED: std::ops::RangeInclusive<f32> = 0.0..=1.0;
/// Saturation band: `0` grey, `1` as it is, `2` twice as vivid.
pub const SATURATION_ADMITTED: std::ops::RangeInclusive<f32> = 0.0..=2.0;
/// Refraction band, logical pixels of displacement at the rim.
pub const REFRACTION_ADMITTED: std::ops::RangeInclusive<f32> = 0.0..=32.0;
/// Edge highlight and specular band: past half they read as a second
/// surface, not light on this one.
pub const LIGHT_ADMITTED: std::ops::RangeInclusive<f32> = 0.0..=0.5;
/// The Liquid set's `band`: how far in from the rim the bend reaches,
/// logical pixels.
pub const BAND_ADMITTED: std::ops::RangeInclusive<f32> = 0.0..=64.0;
/// The band a theme is built at: past 48 a bar or a button is all rim.
pub const BAND_USABLE: std::ops::RangeInclusive<f32> = 0.0..=48.0;
/// The Liquid set's `light_angle`: the direction the light comes from,
/// degrees counter-clockwise from the right (90 from above).
pub const LIGHT_ANGLE_ADMITTED: std::ops::RangeInclusive<f32> = 0.0..=360.0;
/// The Liquid set's `dispersion`: red and blue set apart by up to this
/// fraction of the bend. Past half the fringe is a rainbow, not glass.
pub const DISPERSION_ADMITTED: std::ops::RangeInclusive<f32> = 0.0..=0.5;

static DEFAULT_SET: LazyLock<PainterSet> =
    LazyLock::new(|| PainterSet::new(DEFAULT, Arc::new(Stock { light: false })));
static GLASS_SET: LazyLock<PainterSet> =
    LazyLock::new(|| PainterSet::new(GLASS, Arc::new(Stock { light: true })));
static BLOCK_SET: LazyLock<PainterSet> = LazyLock::new(|| PainterSet::new(BLOCK, Arc::new(Block)));
static PIXELATED_SET: LazyLock<PainterSet> =
    LazyLock::new(|| PainterSet::new(PIXELATED, Arc::new(Pixelated)));
static SHADER_SET: LazyLock<PainterSet> =
    LazyLock::new(|| PainterSet::new(SHADER, Arc::new(Shader)));
static LIQUID_SET: LazyLock<PainterSet> =
    LazyLock::new(|| PainterSet::new(LIQUID, Arc::new(Liquid)));

/// The Default set.
#[must_use]
pub fn default_set() -> &'static PainterSet {
    &DEFAULT_SET
}

/// The Glass set.
#[must_use]
pub fn glass_set() -> &'static PainterSet {
    &GLASS_SET
}

/// The Block set.
#[must_use]
pub fn block_set() -> &'static PainterSet {
    &BLOCK_SET
}

/// The Pixelated set.
#[must_use]
pub fn pixelated_set() -> &'static PainterSet {
    &PIXELATED_SET
}

/// The Shader set.
#[must_use]
pub fn shader_set() -> &'static PainterSet {
    &SHADER_SET
}

/// The Liquid set.
#[must_use]
pub fn liquid_set() -> &'static PainterSet {
    &LIQUID_SET
}

/// The built-in set `name` a document may name without installing anything.
#[must_use]
pub fn builtin(name: &str) -> Option<&'static PainterSet> {
    match name {
        DEFAULT => Some(default_set()),
        GLASS => Some(glass_set()),
        BLOCK => Some(block_set()),
        PIXELATED => Some(pixelated_set()),
        SHADER => Some(shader_set()),
        LIQUID => Some(liquid_set()),
        _ => None,
    }
}

/// Default and Glass: one look, with or without light on it.
struct Stock {
    light: bool,
}

/// How a stock body or face is lit; all zero for Default.
#[derive(Clone, Copy)]
struct Light {
    refraction: f32,
    edge_highlight: f32,
    specular: f32,
    /// The light the surface catches, from the theme's tokens.
    catch: Color,
}

impl Stock {
    fn light(&self, tokens: &PartTokens) -> Light {
        if !self.light {
            return Light {
                refraction: 0.0,
                edge_highlight: 0.0,
                specular: 0.0,
                catch: tokens.light,
            };
        }
        Light {
            refraction: tokens.param("refraction", 0.0),
            edge_highlight: tokens.param("edge_highlight", 0.0),
            specular: tokens.param("specular", 0.0),
            catch: tokens.light,
        }
    }
}

/// Whether the document gave the role a blur, a tint or a saturation: a
/// body that lets what is below it through
/// (`crate::tokens::Finish::translucent`).
fn translucent(tokens: &PartTokens) -> bool {
    tokens
        .params
        .iter()
        .any(|param| matches!(param.name.as_str(), "blur" | "tint" | "saturation"))
}

impl PartPainter for Stock {
    fn paint(
        &self,
        part: Part,
        size: Size,
        state: PartState,
        _progress: f32,
        tokens: &PartTokens,
    ) -> Option<PaintedPart> {
        let body = body(size, tokens);
        let path = body.to_path(0.1);
        let light = self.light(tokens);
        let tint = tokens.param("tint", 1.0);
        let mut scene = PartScene::new();
        match part {
            Part::WindowGround | Part::FloatingPanel | Part::BarBody => {
                let blur = tokens.param("blur", 0.0);
                // A window's ground stays its frosted fill: only floating
                // chrome and bars blur what lies below.
                let sampled = tokens.backdrop && part != Part::WindowGround;
                if sampled && translucent(tokens) && blur > 0.0 {
                    // The backdrop, the sheen on it, held in the band by the
                    // toolkit; the ground at its tint; the rim over all.
                    scene.fill(
                        path.clone(),
                        Brush::Backdrop {
                            blur,
                            lens: Lens {
                                refraction: light.refraction,
                                ..Lens::default()
                            },
                            fallback: tokens.fill,
                            glaze: sheen(&body, light).into_iter().collect(),
                        },
                    );
                    scene.fill(path, Brush::Solid(tokens.fill.with_alpha(tint)));
                } else {
                    scene.fill(path, Brush::Solid(tokens.fill));
                    scene.ops.extend(sheen(&body, light));
                }
                scene.ops.extend(rim(&body, light));
            }
            Part::ControlFace => {
                // The rest face lets the window ground through at the tint
                // and catches the light; a face carrying state stays opaque,
                // its ink measured on it.
                let rest = state.now == States::REST;
                let fill = if rest && translucent(tokens) {
                    tokens.fill.with_alpha(tokens.fill.components[3] * tint)
                } else {
                    tokens.fill
                };
                if fill.components[3] > 0.0 {
                    scene.fill(path, Brush::Solid(fill));
                    if rest {
                        scene.ops.extend(sheen(&body, light));
                        scene.ops.extend(rim(&body, light));
                    }
                }
            }
            _ => return None,
        }
        Some(PaintedPart::new(scene))
    }

    fn params(&self) -> Vec<ParamSpec> {
        // Default is opaque until a document gives it a blur or a tint, and
        // the rest then take their neutral values; Glass states all of them.
        let unless_glass = |value: f32| (!self.light).then_some(value);
        let mut params = vec![
            ParamSpec::new("blur", unless_glass(0.0), BLUR_ADMITTED).usable_within(BLUR_USABLE),
            ParamSpec::new("tint", unless_glass(1.0), TINT_ADMITTED),
            ParamSpec::new("saturation", unless_glass(1.0), SATURATION_ADMITTED),
        ];
        if self.light {
            params.extend([
                ParamSpec::new("refraction", None, REFRACTION_ADMITTED),
                ParamSpec::new("edge_highlight", None, LIGHT_ADMITTED),
                ParamSpec::new("specular", None, LIGHT_ADMITTED),
            ]);
        }
        params
    }
}

/// Block: flat colour, nothing else.
struct Block;

impl PartPainter for Block {
    fn paint(
        &self,
        part: Part,
        size: Size,
        _state: PartState,
        _progress: f32,
        tokens: &PartTokens,
    ) -> Option<PaintedPart> {
        let mut scene = PartScene::new();
        match part {
            Part::WindowGround | Part::FloatingPanel | Part::BarBody | Part::ControlFace => {
                if part != Part::ControlFace || tokens.fill.components[3] > 0.0 {
                    scene.fill(body(size, tokens).to_path(0.1), Brush::Solid(tokens.fill));
                }
            }
            _ => return None,
        }
        Some(PaintedPart::new(scene))
    }
}

/// Pixelated: what lies below the part, sampled at a coarse cell so the
/// surface shows it as pixels. A window, a panel and a bar show the
/// backdrop through the cell grid at `cover`; where there is no backdrop
/// pass (a window's own ground, a panel outside a recording) the part is
/// its fill, the ground ink is measured on, and a control face is its
/// fill.
struct Pixelated;

impl PartPainter for Pixelated {
    fn paint(
        &self,
        part: Part,
        size: Size,
        _state: PartState,
        _progress: f32,
        tokens: &PartTokens,
    ) -> Option<PaintedPart> {
        let path = body(size, tokens).to_path(0.1);
        let cell = tokens.param("cell", 8.0).clamp(1.0, 64.0);
        let cover = tokens.param("cover", 0.85).clamp(0.0, 1.0);
        // Softening before the cells: none by default, so each cell is the
        // plain average of what lies below it.
        let softening = tokens.param("blur", 0.0);
        let mut scene = PartScene::new();
        match part {
            Part::WindowGround | Part::FloatingPanel | Part::BarBody => {
                // A window's ground stays its fill: only floating chrome and
                // bars show what lies below as cells.
                if tokens.backdrop && part != Part::WindowGround {
                    scene.fill(
                        path.clone(),
                        Brush::Backdrop {
                            blur: softening,
                            lens: Lens {
                                sampling: Sampling::Blocky,
                                cell,
                                ..Lens::default()
                            },
                            fallback: tokens.fill,
                            glaze: Vec::new(),
                        },
                    );
                    scene.fill(path, Brush::Solid(tokens.fill.with_alpha(cover)));
                } else {
                    scene.fill(path, Brush::Solid(tokens.fill));
                }
            }
            Part::ControlFace => {
                if tokens.fill.components[3] > 0.0 {
                    scene.fill(path, Brush::Solid(tokens.fill));
                }
            }
            _ => return None,
        }
        Some(PaintedPart::new(scene))
    }

    fn params(&self) -> Vec<ParamSpec> {
        vec![
            ParamSpec::new("cell", Some(8.0), CELL_ADMITTED).usable_within(CELL_USABLE),
            ParamSpec::new("cover", Some(0.85), COVER_ADMITTED),
            ParamSpec::new("blur", Some(0.0), BLUR_ADMITTED).usable_within(BLUR_USABLE),
        ]
    }
}

/// Shader: the theme's own WGSL fragment over the fixed input set
/// (`crate::tokens::shader`), evaluated once per settled frame.
///
/// The set draws every part it is given as one [`Brush::Shader`] over the
/// part's body. The toolkit evaluates it into an image the first time it is
/// drawn and replays that image until the part's size, state or tokens
/// change, so an idle surface draws no frame at all.
struct Shader;

impl PartPainter for Shader {
    fn paint(
        &self,
        part: Part,
        size: Size,
        _state: PartState,
        _progress: f32,
        tokens: &PartTokens,
    ) -> Option<PaintedPart> {
        match part {
            Part::WindowGround | Part::FloatingPanel | Part::BarBody | Part::ControlFace => {}
            _ => return None,
        }
        let path = body(size, tokens).to_path(0.1);
        let mut scene = PartScene::new();
        scene.ops.push(Op::Fill {
            path,
            brush: Brush::Shader {
                source: tokens.shader.clone(),
                fallback: tokens.fill,
            },
            brush_transform: Affine::IDENTITY,
        });
        Some(PaintedPart::new(scene))
    }

    /// The four numbers a shader reads as `params.xyzw`
    /// (`crate::tokens::shader::FINISH_PARAMS`), each with the value it
    /// has when a theme states none.
    fn params(&self) -> Vec<ParamSpec> {
        let [tint, saturation, scale, phase] = crate::tokens::shader::FINISH_PARAMS;
        vec![
            ParamSpec::new(tint, Some(0.0), COVER_ADMITTED),
            ParamSpec::new(saturation, Some(1.0), SATURATION_ADMITTED),
            ParamSpec::new(scale, Some(1.0), SHADER_SCALE_ADMITTED),
            ParamSpec::new(phase, Some(0.0), SHADER_PHASE_ADMITTED),
        ]
    }
}

/// Liquid: a lens of glass over what lies below. A window, a panel and a
/// bar that show the backdrop show it bent along their rounded edge as a
/// convex lens bends it: within `band` of the rim each pixel shows what
/// lies up to `refraction` further in along the outline's normal (all of it
/// at the rim, easing to none at the band's inner side), red and blue set
/// apart by `dispersion`; the flat middle shows what lies below straight,
/// softened by `blur` (none by default). A window's ground is glass too:
/// what lies below a window shows through it the same way. The bend is the
/// backdrop pass's
/// (`crate::backdrop`), evaluated with the blur, so a settled surface draws
/// nothing new. The ground lies over it at `tint`, and every body and rest
/// face carries a thin specular rim lit from `light_angle`: at
/// `edge_highlight` where it faces the light, faintly on the far side where
/// light leaves the glass. Where there is no backdrop pass the part is its
/// fill under the rim.
struct Liquid;

impl PartPainter for Liquid {
    fn paint(
        &self,
        part: Part,
        size: Size,
        state: PartState,
        _progress: f32,
        tokens: &PartTokens,
    ) -> Option<PaintedPart> {
        let body = body(size, tokens);
        let path = body.to_path(0.1);
        let tint = tokens.param("tint", 0.1).clamp(0.0, 1.0);
        let rim = SpecularRim {
            strength: tokens.param("edge_highlight", 0.45),
            light_angle: tokens.param("light_angle", 135.0),
            catch: tokens.light,
        };
        let mut scene = PartScene::new();
        match part {
            Part::WindowGround | Part::FloatingPanel | Part::BarBody => {
                if tokens.backdrop {
                    scene.fill(
                        path.clone(),
                        Brush::Backdrop {
                            blur: tokens.param("blur", 0.0),
                            lens: liquid_lens(tokens),
                            fallback: tokens.fill,
                            glaze: Vec::new(),
                        },
                    );
                    scene.fill(path, Brush::Solid(tokens.fill.with_alpha(tint)));
                } else {
                    scene.fill(path, Brush::Solid(tokens.fill));
                }
                scene.ops.extend(rim.ops(&body));
            }
            Part::ControlFace => {
                // As Glass's: the rest face lets the window ground through
                // at the tint and catches the light; a face carrying state
                // stays opaque, its ink measured on it.
                let rest = state.now == States::REST;
                let fill = if rest {
                    tokens.fill.with_alpha(tokens.fill.components[3] * tint)
                } else {
                    tokens.fill
                };
                if fill.components[3] > 0.0 {
                    scene.fill(path, Brush::Solid(fill));
                    if rest {
                        scene.ops.extend(rim.ops(&body));
                    }
                }
            }
            _ => return None,
        }
        Some(PaintedPart::new(scene))
    }

    /// `tint` is required, so every Liquid role states it and reads as
    /// translucent to the legibility audit; the rest have stock values.
    fn params(&self) -> Vec<ParamSpec> {
        vec![
            ParamSpec::new("tint", None, TINT_ADMITTED),
            ParamSpec::new("refraction", Some(12.0), REFRACTION_ADMITTED),
            ParamSpec::new("band", Some(18.0), BAND_ADMITTED).usable_within(BAND_USABLE),
            ParamSpec::new("blur", Some(0.0), BLUR_ADMITTED).usable_within(BLUR_USABLE),
            ParamSpec::new("dispersion", Some(0.0), DISPERSION_ADMITTED),
            ParamSpec::new("edge_highlight", Some(0.45), LIGHT_ADMITTED),
            ParamSpec::new("light_angle", Some(135.0), LIGHT_ANGLE_ADMITTED),
        ]
    }
}

/// The lens a Liquid body asks the backdrop pass for. A stated band of `0`
/// is a band too thin to bend anything, not the lens's own default.
fn liquid_lens(tokens: &PartTokens) -> Lens {
    Lens {
        refraction: tokens.param("refraction", 12.0),
        band: tokens.param("band", 18.0).max(f32::MIN_POSITIVE),
        dispersion: tokens.param("dispersion", 0.0),
        ..Lens::default()
    }
}

/// A thin bright edge just inside a body's outline, lit from one side:
/// each stretch of the rim is as bright as its outward normal faces the
/// light ([`rim_light`]).
#[derive(Clone, Copy)]
struct SpecularRim {
    /// Opacity of the light where the rim faces it squarely.
    strength: f32,
    /// The direction the light comes from, degrees counter-clockwise from
    /// the right (90 from above).
    light_angle: f32,
    /// The light the surface catches, from the theme's tokens.
    catch: Color,
}

/// The rim's width, logical pixels: one and a half, so it reads as an edge
/// catching light at any scale and never as a border.
const RIM_WIDTH: f64 = 1.5;

/// Arcs each rounded corner of the rim is drawn in: enough that the light
/// turns smoothly round the corner.
const RIM_CORNER_STEPS: u32 = 4;

/// How bright the rim is where its outward `normal` points, lit from
/// `toward_light` (unit vectors in part coordinates, y down): fully where
/// it faces the light, nothing side on, and a third as much where it faces
/// away, where light that entered the glass leaves it.
#[must_use]
pub(crate) fn rim_light(normal: Vec2, toward_light: Vec2) -> f32 {
    let facing = normal.dot(toward_light);
    let lit = facing.max(0.0).powi(2);
    let through = (-facing).max(0.0).powi(2) / 3.0;
    crate::num::f32_of_f64((lit + through).clamp(0.0, 1.0))
}

impl SpecularRim {
    /// The unit vector toward the light, in part coordinates (y down).
    fn toward_light(self) -> Vec2 {
        let angle = f64::from(self.light_angle).to_radians();
        Vec2::new(angle.cos(), -angle.sin())
    }

    /// The light's colour where the rim's outward normal is `normal`.
    fn at(self, normal: Vec2) -> Color {
        self.catch
            .with_alpha(self.strength.clamp(0.0, 1.0) * rim_light(normal, self.toward_light()))
    }

    /// The rim of `body` as fills, one per straight edge and a few per
    /// corner, each lit for its own normal: fills that meet edge to edge,
    /// never strokes, whose caps would overlap and light the joins twice.
    /// Nothing at `0` strength or on a body too small to have a rim.
    fn ops(self, body: &RoundedRect) -> Vec<Op> {
        use std::f64::consts::{FRAC_PI_2, FRAC_PI_4, PI};
        let rect = body.rect();
        if self.strength <= 0.0
            || rect.width() <= 2.0 * RIM_WIDTH
            || rect.height() <= 2.0 * RIM_WIDTH
        {
            return Vec::new();
        }
        let radius = body
            .radii()
            .top_left
            .min(rect.width() / 2.0)
            .min(rect.height() / 2.0)
            .max(0.0);
        let width = RIM_WIDTH;
        let inner_radius = (radius - width).max(0.0);
        let mut ops = Vec::new();
        let solid = |path: BezPath, normal: Vec2| Op::Fill {
            path,
            brush: Brush::Solid(self.at(normal)),
            brush_transform: Affine::IDENTITY,
        };
        // The straight edges, between the corners.
        let (x0, y0, x1, y1) = (rect.x0, rect.y0, rect.x1, rect.y1);
        let edges = [
            (
                Rect::new(x0 + radius, y0, x1 - radius, y0 + width),
                Vec2::new(0.0, -1.0),
            ),
            (
                Rect::new(x1 - width, y0 + radius, x1, y1 - radius),
                Vec2::new(1.0, 0.0),
            ),
            (
                Rect::new(x0 + radius, y1 - width, x1 - radius, y1),
                Vec2::new(0.0, 1.0),
            ),
            (
                Rect::new(x0, y0 + radius, x0 + width, y1 - radius),
                Vec2::new(-1.0, 0.0),
            ),
        ];
        for (strip, normal) in edges {
            if strip.width() > 0.0 && strip.height() > 0.0 {
                ops.push(solid(strip.to_path(0.1), normal));
            }
        }
        // The corners: the centre of each corner's arc and the angle (y
        // down, from +x) it starts at; each sweeps a quarter turn.
        let corners = [
            (Point::new(x1 - radius, y0 + radius), -FRAC_PI_2),
            (Point::new(x1 - radius, y1 - radius), 0.0),
            (Point::new(x0 + radius, y1 - radius), FRAC_PI_2),
            (Point::new(x0 + radius, y0 + radius), PI),
        ];
        let step = FRAC_PI_2 / f64::from(RIM_CORNER_STEPS);
        for (centre, start) in corners {
            if radius <= 0.0 {
                // A square corner: the rim's corner square, lit for the
                // diagonal it faces.
                let outward = Vec2::from_angle(start + FRAC_PI_4);
                let square = Rect::from_center_size(
                    centre + Vec2::new(-outward.x.signum(), -outward.y.signum()) * (width / 2.0),
                    (width, width),
                );
                ops.push(solid(square.to_path(0.1), outward));
                continue;
            }
            for index in 0..RIM_CORNER_STEPS {
                let from = start + step * f64::from(index);
                let to = from + step;
                let mut piece = BezPath::new();
                piece.move_to(centre + Vec2::from_angle(from) * radius);
                vello::kurbo::Arc::new(centre, (radius, radius), from, step, 0.0)
                    .to_cubic_beziers(0.1, |p1, p2, p| piece.curve_to(p1, p2, p));
                if inner_radius > 0.0 {
                    piece.line_to(centre + Vec2::from_angle(to) * inner_radius);
                    vello::kurbo::Arc::new(centre, (inner_radius, inner_radius), to, -step, 0.0)
                        .to_cubic_beziers(0.1, |p1, p2, p| piece.curve_to(p1, p2, p));
                } else {
                    piece.line_to(centre);
                }
                piece.close_path();
                // The light turns with the normal across the arc: a
                // gradient from one end's light to the other's.
                ops.push(Op::Fill {
                    path: piece,
                    brush: Brush::Linear {
                        start: centre + Vec2::from_angle(from) * radius,
                        end: centre + Vec2::from_angle(to) * radius,
                        stops: vec![
                            Stop {
                                offset: 0.0,
                                color: self.at(Vec2::from_angle(from)),
                            },
                            Stop {
                                offset: 1.0,
                                color: self.at(Vec2::from_angle(to)),
                            },
                        ],
                    },
                    brush_transform: Affine::IDENTITY,
                });
            }
        }
        ops
    }
}

/// The part's body: its rect at the radius the toolkit gave it.
fn body(size: Size, tokens: &PartTokens) -> RoundedRect {
    RoundedRect::from_rect(
        Rect::from_origin_size(Point::ORIGIN, size),
        f64::from(tokens.radius),
    )
}

/// The sheen a glass body catches across its upper body: the theme's light
/// at `specular` opacity at the top-left, gone past the body's middle.
/// Nothing at `0`.
fn sheen(body: &RoundedRect, light: Light) -> Option<Op> {
    let rect = body.rect();
    let specular = light.specular;
    if specular <= 0.0 || rect.width() <= 2.0 || rect.height() <= 2.0 {
        return None;
    }
    Some(Op::Fill {
        path: body.to_path(0.1),
        brush: Brush::Linear {
            start: Point::new(rect.x0, rect.y0),
            end: Point::new(rect.x0 + rect.width() * 0.3, rect.y0 + rect.height() * 0.6),
            stops: vec![
                Stop {
                    offset: 0.0,
                    color: light.catch.with_alpha(specular),
                },
                Stop {
                    offset: 0.45,
                    color: light.catch.with_alpha(specular * 0.35),
                },
                Stop {
                    offset: 1.0,
                    color: light.catch.with_alpha(0.0),
                },
            ],
        },
        brush_transform: Affine::IDENTITY,
    })
}

/// The lit rim of a glass body: its outermost pixel row, brightest along
/// the top edge at `edge_highlight` opacity, fading down the sides and
/// catching a little bounced light at the foot. Nothing at `0`.
fn rim(body: &RoundedRect, light: Light) -> Option<Op> {
    let rect = body.rect();
    let edge_highlight = light.edge_highlight;
    if edge_highlight <= 0.0 || rect.width() <= 2.0 || rect.height() <= 2.0 {
        return None;
    }
    let outline: BezPath = crate::surface::edge_outline(rect, body.radii().top_left).to_path(0.1);
    Some(Op::Stroke {
        path: outline,
        width: 1.0,
        brush: Brush::Linear {
            start: Point::new(rect.x0, rect.y0),
            end: Point::new(rect.x0, rect.y1),
            stops: vec![
                Stop {
                    offset: 0.0,
                    color: light.catch.with_alpha(edge_highlight),
                },
                Stop {
                    offset: 0.5,
                    color: light.catch.with_alpha(edge_highlight * 0.12),
                },
                Stop {
                    offset: 1.0,
                    color: light.catch.with_alpha(edge_highlight * 0.35),
                },
            ],
        },
    })
}
