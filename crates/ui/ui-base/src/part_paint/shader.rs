//! Evaluating a theme's material stage (`crate::tokens::shader`) on the CPU: the fallback where no
//! GPU runs the stage in the backdrop pass. A stage is a straight-line WGSL function over the
//! documented `Surface` (`shader::check` refuses loops, branches and mutable variables), so the
//! toolkit evaluates it **once per settled part**: one call per texel of an image sized to the part,
//! cached with the part until its size, state, theme or what it samples changes. `backdrop(offset)`
//! reads the backdrop a host provided, or the part's ground where there is none. What the
//! interpreter cannot run is an error ([`EvalError`]) the caller draws as the part's fallback colour,
//! never a blank part and never a panic.

use vello::kurbo::Rect;
use vello::peniko::Color;

use crate::part_paint::PartTokens;
use crate::tokens::shader;

/// The most texels one part evaluates: a 724 × 724 square, or any rectangle of the same area. A
/// pixelated shader over a huge window samples on the grid this caps to; it never costs more than this
/// per settled frame.
pub const MAX_TEXELS: u32 = 512 * 1024;

#[cfg(test)]
thread_local! {
    /// How many times this thread evaluated a shader: an idle part is evaluated once and replayed.
    pub(crate) static EVALUATED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Why a shader could not be evaluated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvalError(pub String);

/// Whether a theme may draw this program: [`shader::check`], then one texel through the
/// interpreter.
pub(crate) fn admit(wgsl: &str) -> Result<(), String> {
    shader::check(wgsl)?;
    // Checked against the grammar is not yet drawable: what the interpreter
    // cannot run is refused here, at load, rather than drawn as a fallback on
    // every part.
    probe(wgsl).map_err(|reason| format!("surface shader cannot be drawn: {reason}"))
}

/// What lies behind a part, for `backdrop(offset)`: an image covering the part's box.
pub(crate) struct Behind<'a> {
    pub image: &'a vello::peniko::ImageData,
}

impl Behind<'_> {
    /// The colour at `uv` across the part's box, linear, clamped to its edge.
    fn at(&self, uv: [f32; 2]) -> [f32; 4] {
        let image = self.image;
        let (width, height) = (image.width.max(1), image.height.max(1));
        let texel = |value: f32, edge: u32| {
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "clamped to the image's edge before the cast"
            )]
            let at = (value.clamp(0.0, 1.0) * float(edge))
                .floor()
                .min(float(edge - 1)) as usize;
            at
        };
        let (x, y) = (texel(uv[0], width), texel(uv[1], height));
        let start = (y * width as usize + x) * 4;
        let bytes = image.data.data();
        let Some(rgba) = bytes.get(start..start + 4) else {
            return [0.0, 0.0, 0.0, 1.0];
        };
        let channel = |byte: u8| linear(f32::from(byte) / 255.0);
        [
            channel(rgba[0]),
            channel(rgba[1]),
            channel(rgba[2]),
            f32::from(rgba[3]) / 255.0,
        ]
    }
}

/// The stage's image for the part `rect` (logical pixels) in `theme`, drawn with `tokens`, over
/// `behind` where a host provided what lies behind the part; `fallback` is what a stage that cannot be
/// evaluated is drawn as.
#[must_use]
pub(crate) fn evaluate(
    source: &crate::part_paint::ShaderSource,
    rect: Rect,
    tokens: &PartTokens,
    behind: Option<&Behind<'_>>,
    fallback: Color,
) -> vello::peniko::ImageData {
    #[cfg(test)]
    EVALUATED.with(|count| count.set(count.get() + 1));
    let (width, height) = rail(rect.width(), rect.height());
    // A settled part is replayed while the pointer moves, so the CPU path draws it with none.
    let inputs = Inputs::of(tokens, rect, source.time_centis);
    match run(&source.wgsl, &inputs, behind, width, height) {
        Ok(pixels) => crate::raster::rgba8(pixels, width, height),
        Err(error) => {
            tracing::warn!(
                shader = %error.0,
                "a theme's surface shader could not be evaluated; its fallback is drawn"
            );
            crate::raster::rgba8(fallback_pixels(fallback, width, height), width, height)
        }
    }
}

/// Whether the interpreter can draw `wgsl`: one texel over neutral inputs. # Errors What the
/// interpreter cannot run, as the theme's author reads it.
fn probe(wgsl: &str) -> Result<(), String> {
    let inputs = Inputs::of(&PartTokens::default(), Rect::new(0.0, 0.0, 1.0, 1.0), 0);
    run(wgsl, &inputs, None, 1, 1)
        .map(|_| ())
        .map_err(|error| error.0)
}

/// The image's size in texels: the part's own size, the same total area capped at [`MAX_TEXELS`] with
/// its aspect kept, at least 1 × 1.
fn rail(width: f64, height: f64) -> (u32, u32) {
    let area = width.max(1.0) * height.max(1.0);
    let scale = if area > f64::from(MAX_TEXELS) {
        (f64::from(MAX_TEXELS) / area).sqrt()
    } else {
        1.0
    };
    let edge = |value: f64| {
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "clamped to 1..=MAX_TEXELS before the cast"
        )]
        let texels = (value.max(0.0) * scale)
            .round()
            .clamp(1.0, f64::from(MAX_TEXELS)) as u32;
        texels
    };
    (edge(width), edge(height))
}

fn fallback_pixels(color: Color, width: u32, height: u32) -> Vec<u8> {
    color
        .to_rgba8()
        .to_u8_array()
        .repeat((width as usize).saturating_mul(height as usize))
}

/// What a stage reads that is the same for every texel of a part.
pub(crate) struct Inputs {
    pub size: [f32; 2],
    pub radius: f32,
    pub pointer: [f32; 3],
    pub light_dir: [f32; 3],
    pub light: [f32; 4],
    pub fill: [f32; 4],
    pub ink: [f32; 4],
    pub ground: [f32; 4],
    pub raised: [f32; 4],
    pub accent: [f32; 4],
    pub params: [f32; 4],
    pub time: f32,
    pub refraction: f32,
}

/// An sRGB-encoded channel, linear.
fn linear(value: f32) -> f32 {
    if value <= 0.040_45 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

/// A token colour as a stage reads it: linear, straight alpha.
pub(crate) fn linear_color(color: Color) -> [f32; 4] {
    let [r, g, b, a] = color.components;
    [linear(r), linear(g), linear(b), a]
}

/// The unit vector toward a light at `angle` degrees counter-clockwise from the right (90 from
/// above, as the Liquid set's `light_angle`), raised 45 degrees off the surface, in part coordinates
/// (`y` down).
pub(crate) fn light_dir(angle: f32) -> [f32; 3] {
    let (sin, cos) = angle.to_radians().sin_cos();
    let flat = std::f32::consts::FRAC_1_SQRT_2;
    [cos * flat, -sin * flat, flat]
}

impl Inputs {
    pub(crate) fn of(tokens: &PartTokens, rect: Rect, time_centis: u32) -> Self {
        let param =
            |index: usize, default: f32| tokens.param(shader::FINISH_PARAMS[index], default);
        Self {
            size: [
                crate::num::f32_of_f64(rect.width()),
                crate::num::f32_of_f64(rect.height()),
            ],
            radius: tokens.radius,
            pointer: [0.0; 3],
            light_dir: light_dir(tokens.param("light_angle", 135.0)),
            light: linear_color(tokens.light),
            fill: linear_color(tokens.fill),
            ink: linear_color(tokens.ink),
            ground: linear_color(tokens.ground),
            raised: linear_color(tokens.raised),
            accent: linear_color(tokens.accent),
            params: [param(0, 0.0), param(1, 1.0), param(2, 1.0), param(3, 0.0)],
            time: float(time_centis) / 100.0,
            refraction: tokens.param("refraction", 0.0),
        }
    }
}

/// What a stage reads that changes from texel to texel: where it is, and the outline and slab there.
/// The same arithmetic as the GPU pipeline's (`shader::wrap`).
#[derive(Clone, Copy)]
struct Texel {
    uv: [f32; 2],
    position: [f32; 2],
    distance: f32,
    radius: f32,
    edge: [f32; 2],
    normal: [f32; 3],
}

impl Texel {
    fn at(uv: [f32; 2], inputs: &Inputs) -> Self {
        let size = inputs.size;
        let half = [size[0] * 0.5, size[1] * 0.5];
        let shorter = half[0].min(half[1]);
        let radius = inputs.radius.clamp(0.0, shorter.max(0.0));
        let position = [uv[0] * size[0], uv[1] * size[1]];
        let p = [position[0] - half[0], position[1] - half[1]];
        let q = [
            p[0].abs() - (half[0] - radius),
            p[1].abs() - (half[1] - radius),
        ];
        let outside = [q[0].max(0.0), q[1].max(0.0)];
        let reach = outside[0].hypot(outside[1]);
        let distance = reach + q[0].max(q[1]).min(0.0) - radius;
        let away = |v: f32| if v < 0.0 { -1.0 } else { 1.0 };
        let edge = if q[0] > 0.0 && q[1] > 0.0 {
            let length = reach.max(1e-6);
            [
                outside[0] / length * away(p[0]),
                outside[1] / length * away(p[1]),
            ]
        } else if q[0] > q[1] {
            [away(p[0]), 0.0]
        } else {
            [0.0, away(p[1])]
        };
        let rim = radius.max(6.0).clamp(1.0, shorter.max(1.0));
        let k = (1.0 + distance / rim).clamp(0.0, 1.0);
        Self {
            uv,
            position,
            distance,
            radius,
            edge,
            normal: [edge[0] * k, edge[1] * k, (1.0 - k * k).max(0.0).sqrt()],
        }
    }

    /// The `Surface` field at `index` (`shader::FIELDS`).
    fn field(&self, index: u32, inputs: &Inputs) -> Option<Value> {
        Some(match index {
            0 => Value::Vec2(self.uv),
            1 => Value::Vec2(inputs.size),
            2 => Value::Vec2(self.position),
            3 => Value::Scalar(self.distance),
            4 => Value::Scalar(self.radius),
            5 => Value::Vec2(self.edge),
            6 => Value::Vec3(self.normal),
            7 => Value::Vec3(inputs.pointer),
            8 => Value::Vec3(inputs.light_dir),
            9 => Value::Vec4(inputs.light),
            10 => Value::Vec4(inputs.fill),
            11 => Value::Vec4(inputs.ink),
            12 => Value::Vec4(inputs.ground),
            13 => Value::Vec4(inputs.raised),
            14 => Value::Vec4(inputs.accent),
            15 => Value::Vec4(inputs.params),
            16 => Value::Scalar(inputs.time),
            17 => Value::Scalar(inputs.refraction),
            _ => return None,
        })
    }
}

fn run(
    wgsl: &str,
    inputs: &Inputs,
    behind: Option<&Behind<'_>>,
    width: u32,
    height: u32,
) -> Result<Vec<u8>, EvalError> {
    let module = naga::front::wgsl::parse_str(&shader::checked_module(wgsl))
        .map_err(|error| EvalError(format!("does not parse: {}", error.message())))?;
    let program = Program::new(&module)?;
    let mut pixels = Vec::with_capacity(
        (width as usize)
            .saturating_mul(height as usize)
            .saturating_mul(4),
    );
    for y in 0..height {
        for x in 0..width {
            let uv = [
                (float(x) + 0.5) / float(width),
                (float(y) + 0.5) / float(height),
            ];
            let texel = Texel::at(uv, inputs);
            let rgba = program.shade(inputs, behind, texel)?;
            for channel in &rgba[..3] {
                pixels.push(encode(*channel));
            }
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "clamped to 0..=1 before the cast"
            )]
            let alpha = (if rgba[3].is_finite() {
                rgba[3].clamp(0.0, 1.0)
            } else {
                0.0
            } * 255.0)
                .round() as u8;
            pixels.push(alpha);
        }
    }
    Ok(pixels)
}

fn float(value: u32) -> f32 {
    #[allow(clippy::cast_precision_loss, reason = "an image edge, far below 2^24")]
    let value = value as f32;
    value
}

fn encode(value: f32) -> u8 {
    let value = if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let srgb = if value <= 0.003_130_8 {
        12.92 * value
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    };
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to 0..=1 before the cast"
    )]
    let byte = (srgb * 255.0).round() as u8;
    byte
}

/// A value the interpreter carries: a scalar, a vector of 2, 3 or 4 channels, or a bool.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Value {
    Scalar(f32),
    Vec2([f32; 2]),
    Vec3([f32; 3]),
    Vec4([f32; 4]),
    Bool(bool),
    /// The stage's one input, whose fields are read per texel.
    Surface,
}

impl Value {
    fn channels(&self) -> Option<&[f32]> {
        match self {
            Self::Scalar(_) | Self::Bool(_) | Self::Surface => None,
            Self::Vec2(v) => Some(v),
            Self::Vec3(v) => Some(v),
            Self::Vec4(v) => Some(v),
        }
    }

    fn splat(&self) -> Option<f32> {
        match self {
            Self::Scalar(v) => Some(*v),
            _ => None,
        }
    }

    fn widened(&self, n: usize) -> Option<[f32; 4]> {
        let mut out = [0.0; 4];
        match self {
            Self::Scalar(v) => out = [*v; 4],
            Self::Vec2(v) => out[..2].copy_from_slice(v),
            Self::Vec3(v) => out[..3].copy_from_slice(v),
            Self::Vec4(v) => out = *v,
            Self::Bool(_) | Self::Surface => return None,
        }
        (self.channels().map_or(usize::MAX, <[f32]>::len) <= n || self.splat().is_some())
            .then_some(out)
    }
}

/// A shader in a form the interpreter runs: its expression list and its statement list, resolved
/// against the module.
struct Program<'a> {
    expressions: &'a naga::Arena<naga::Expression>,
    body: &'a naga::Block,
    /// The lookup's function, which a `Call` names.
    lookup: Option<naga::Handle<naga::Function>>,
}

impl<'a> Program<'a> {
    fn new(module: &'a naga::Module) -> Result<Self, EvalError> {
        let Some((_, function)) = module
            .functions
            .iter()
            .find(|(_, function)| function.name.as_deref() == Some(shader::ENTRY))
        else {
            return Err(EvalError(format!("declares no `{}`", shader::ENTRY)));
        };
        Ok(Self {
            expressions: &function.expressions,
            body: &function.body,
            lookup: module.functions.iter().find_map(|(handle, function)| {
                (function.name.as_deref() == Some(shader::LOOKUP)).then_some(handle)
            }),
        })
    }

    fn shade(
        &self,
        inputs: &Inputs,
        behind: Option<&Behind<'_>>,
        texel: Texel,
    ) -> Result<[f32; 4], EvalError> {
        let mut scope = Scope::new(self.expressions.len());
        let kept: Vec<(naga::Handle<naga::Expression>, naga::Expression)> = self
            .expressions
            .iter()
            .filter(|(_, expression)| {
                matches!(
                    expression,
                    naga::Expression::FunctionArgument(_) | naga::Expression::Literal(_)
                )
            })
            .map(|(handle, expression)| (handle, expression.clone()))
            .collect();
        for (handle, expression) in kept {
            let value = self.eval(&expression, inputs, texel, &scope)?;
            scope.put(handle, value);
        }
        self.block(self.body, (inputs, behind), texel, &mut scope)
            .map(|value| value.widened(4).unwrap_or([0.0, 0.0, 0.0, 1.0]))
    }

    fn block(
        &self,
        block: &naga::Block,
        (inputs, behind): (&Inputs, Option<&Behind<'_>>),
        texel: Texel,
        scope: &mut Scope,
    ) -> Result<Value, EvalError> {
        for statement in block {
            match statement {
                naga::Statement::Emit(range) => {
                    let inner = range.index_range();
                    let take = inner.end.saturating_sub(inner.start);
                    let emitted: Vec<(naga::Handle<naga::Expression>, naga::Expression)> = self
                        .expressions
                        .iter()
                        .skip(inner.start as usize)
                        .take(take as usize)
                        .map(|(handle, expression)| (handle, expression.clone()))
                        .collect();
                    for (handle, expression) in emitted {
                        let value = self.eval(&expression, inputs, texel, scope)?;
                        scope.put(handle, value);
                    }
                }
                naga::Statement::Call {
                    function,
                    arguments,
                    result: Some(result),
                } if Some(*function) == self.lookup => {
                    let offset = arguments
                        .first()
                        .map(|handle| self.value(*handle, scope))
                        .transpose()?
                        .and_then(|value| value.widened(2))
                        .ok_or_else(|| {
                            EvalError(format!("calls `{}` without an offset", shader::LOOKUP))
                        })?;
                    let seen = match behind {
                        Some(behind) => behind.at([
                            texel.uv[0] + offset[0] / inputs.size[0].max(1.0),
                            texel.uv[1] + offset[1] / inputs.size[1].max(1.0),
                        ]),
                        None => inputs.ground,
                    };
                    scope.put(*result, Value::Vec4(seen));
                }
                naga::Statement::Return { value } => {
                    return match value {
                        Some(handle) => self.value(*handle, scope),
                        None => Ok(Value::Vec4([0.0; 4])),
                    };
                }
                other => {
                    return Err(EvalError(format!(
                        "uses a statement the toolkit does not run in a surface shader: {other:?}"
                    )));
                }
            }
        }
        Ok(Value::Vec4([0.0, 0.0, 0.0, 1.0]))
    }

    fn value(
        &self,
        handle: naga::Handle<naga::Expression>,
        scope: &Scope,
    ) -> Result<Value, EvalError> {
        scope
            .get(handle)
            .ok_or_else(|| EvalError("refers to a value it never computed".to_owned()))
    }

    /// Evaluate one expression.
    fn eval(
        &self,
        expression: &naga::Expression,
        inputs: &Inputs,
        texel: Texel,
        scope: &Scope,
    ) -> Result<Value, EvalError> {
        Ok(match expression {
            naga::Expression::Literal(literal) => Value::Scalar(literal_value(literal)?),
            naga::Expression::FunctionArgument(0) => Value::Surface,
            naga::Expression::FunctionArgument(_) => {
                return Err(EvalError(
                    "reads an input the toolkit does not hand it".to_owned(),
                ));
            }
            naga::Expression::Select {
                condition,
                accept,
                reject,
            } => match self.value(*condition, scope)? {
                Value::Bool(true) => self.value(*accept, scope)?,
                Value::Bool(false) => self.value(*reject, scope)?,
                _ => {
                    return Err(EvalError(
                        "selects on a value a single bool is not".to_owned(),
                    ));
                }
            },
            naga::Expression::ZeroValue(_) => Value::Scalar(0.0),
            naga::Expression::Compose { components, .. } => {
                let mut values = Vec::with_capacity(components.len());
                for component in components {
                    values.push(self.value(*component, scope)?);
                }
                compose(&values)
                    .ok_or_else(|| EvalError("composes a shape a vector is not".to_owned()))?
            }
            naga::Expression::Splat { size, value } => {
                let value = self
                    .value(*value, scope)?
                    .splat()
                    .ok_or_else(|| EvalError("splats a value a scalar is not".to_owned()))?;
                let size = vector_len(*size);
                let out = [value; 4];
                match size {
                    1 => Value::Scalar(out[0]),
                    2 => Value::Vec2([out[0], out[1]]),
                    3 => Value::Vec3([out[0], out[1], out[2]]),
                    _ => Value::Vec4(out),
                }
            }
            naga::Expression::Swizzle {
                size,
                vector,
                pattern,
            } => {
                let source = self.value(*vector, scope)?;
                let channels = source
                    .channels()
                    .ok_or_else(|| EvalError("swizzles a value a vector is not".to_owned()))?;
                let mut out = [0.0; 4];
                let count = vector_len(*size);
                for (at, component) in pattern.iter().take(count).enumerate() {
                    out[at] = *channels.get(pattern_index(*component)).ok_or_else(|| {
                        EvalError("swizzles a channel the vector does not have".to_owned())
                    })?;
                }
                match count {
                    1 => Value::Scalar(out[0]),
                    2 => Value::Vec2([out[0], out[1]]),
                    3 => Value::Vec3([out[0], out[1], out[2]]),
                    _ => Value::Vec4(out),
                }
            }
            naga::Expression::AccessIndex { base, index } => {
                let base = self.value(*base, scope)?;
                if base == Value::Surface {
                    return texel.field(*index, inputs).ok_or_else(|| {
                        EvalError("reads a field `Surface` does not have".to_owned())
                    });
                }
                let channels = base
                    .channels()
                    .ok_or_else(|| EvalError("indexes a value a vector is not".to_owned()))?;
                let channel = channels.get(*index as usize).ok_or_else(|| {
                    EvalError("indexes a channel a vector does not have".to_owned())
                })?;
                Value::Scalar(*channel)
            }
            naga::Expression::Unary { op, expr } => {
                let value = self.value(*expr, scope)?;
                match op {
                    naga::UnaryOperator::Negate => map(&value, &|v: f32| -v)
                        .ok_or_else(|| EvalError("negates a value a number is not".to_owned()))?,
                    naga::UnaryOperator::LogicalNot => match value {
                        Value::Bool(flag) => Value::Bool(!flag),
                        _ => return Err(EvalError("negates a value a bool is not".to_owned())),
                    },
                    naga::UnaryOperator::BitwiseNot => {
                        return Err(EvalError("uses a bitwise operator".to_owned()));
                    }
                }
            }
            naga::Expression::Binary { op, left, right } => {
                let left = self.value(*left, scope)?;
                let right = self.value(*right, scope)?;
                self.binary(*op, left, right)?
            }
            naga::Expression::Math {
                fun,
                arg,
                arg1,
                arg2,
                arg3,
                ..
            } => {
                let args = [Some(*arg), *arg1, *arg2, *arg3];
                let mut values = Vec::with_capacity(4);
                for handle in args.into_iter().flatten() {
                    values.push(self.value(handle, scope)?);
                }
                math(*fun, &values)?
            }
            other => {
                return Err(EvalError(format!(
                    "uses an expression the toolkit does not run in a surface shader: {other:?}"
                )));
            }
        })
    }

    fn binary(
        &self,
        op: naga::BinaryOperator,
        left: Value,
        right: Value,
    ) -> Result<Value, EvalError> {
        use naga::BinaryOperator as Op;
        let pair = |f: &dyn Fn(f32, f32) -> f32| -> Option<Value> {
            match (left.splat(), right.splat()) {
                (Some(a), Some(b)) => Some(Value::Scalar(f(a, b))),
                _ => None,
            }
        };
        let lanes = |f: &dyn Fn(f32, f32) -> f32| -> Option<Value> {
            let n = left
                .channels()
                .map(<[f32]>::len)
                .max(right.channels().map(<[f32]>::len))?;
            let a = left.widened(n)?;
            let b = right.widened(n)?;
            let mut out = [0.0; 4];
            for at in 0..n {
                out[at] = f(a[at], b[at]);
            }
            Some(match n {
                1 => Value::Scalar(out[0]),
                2 => Value::Vec2([out[0], out[1]]),
                3 => Value::Vec3([out[0], out[1], out[2]]),
                _ => Value::Vec4(out),
            })
        };
        let both = |f: &dyn Fn(f32, f32) -> f32| pair(f).or_else(|| lanes(f));
        let value = match op {
            Op::Add => both(&|a, b| a + b),
            Op::Subtract => both(&|a, b| a - b),
            Op::Multiply => both(&|a, b| a * b),
            Op::Divide => both(&|a, b| a / b),
            Op::Modulo => both(&|a, b| a - b * (a / b).floor()),
            Op::Equal
            | Op::NotEqual
            | Op::Less
            | Op::LessEqual
            | Op::Greater
            | Op::GreaterEqual => {
                let (Some(a), Some(b)) = (left.splat(), right.splat()) else {
                    return Err(EvalError("compares a shape a scalar is not".to_owned()));
                };
                Some(Value::Bool(match op {
                    Op::Equal => (a - b).abs() <= f32::EPSILON,
                    Op::NotEqual => (a - b).abs() > f32::EPSILON,
                    Op::Less => a < b,
                    Op::LessEqual => a <= b,
                    Op::Greater => a > b,
                    _ => a >= b,
                }))
            }
            other => {
                return Err(EvalError(format!(
                    "uses an operator a surface shader does not run: {other:?}"
                )));
            }
        };
        value.ok_or_else(|| EvalError("mixes shapes that do not match".to_owned()))
    }
}

/// Values the shader computed so far in the current call, by the index of the expression that computed
/// each (`naga`'s `Emit` ranges name exactly which expressions run before the statement that reads
/// them).
struct Scope {
    computed: Vec<Option<Value>>,
}

impl Scope {
    fn new(expressions: usize) -> Self {
        Self {
            computed: vec![None; expressions],
        }
    }

    fn put(&mut self, handle: naga::Handle<naga::Expression>, value: Value) {
        if let Some(slot) = self.computed.get_mut(handle.index()) {
            *slot = Some(value);
        }
    }

    fn get(&self, handle: naga::Handle<naga::Expression>) -> Option<Value> {
        self.computed.get(handle.index()).copied().flatten()
    }
}

fn math(fun: naga::MathFunction, args: &[Value]) -> Result<Value, EvalError> {
    use naga::MathFunction as M;
    let one = |f: &dyn Fn(f32) -> f32| -> Option<Value> { map(&args[0], f) };
    // `n` arguments combined channel by channel, each a scalar or a vector
    // of the same width (WGSL's `mix`, `clamp`, `smoothstep` and `step` take
    // a scalar where a vector is expected, broadcast).
    let lanes = |f: &dyn Fn(&[f32; 4]) -> f32| -> Option<Value> {
        // All scalars is one lane: `clamp(uv.y, 0.0, 1.0)` is a scalar.
        let width = args
            .iter()
            .filter_map(Value::channels)
            .map(<[f32]>::len)
            .max()
            .unwrap_or(1);
        let mut widened = Vec::with_capacity(args.len());
        for argument in args {
            widened.push(argument.widened(width)?);
        }
        let mut out = [0.0; 4];
        for at in 0..width {
            let mut lane = [0.0; 4];
            for (slot, value) in lane.iter_mut().zip(&widened) {
                *slot = value[at];
            }
            out[at] = f(&lane);
        }
        Some(match width {
            1 => Value::Scalar(out[0]),
            2 => Value::Vec2([out[0], out[1]]),
            3 => Value::Vec3([out[0], out[1], out[2]]),
            _ => Value::Vec4(out),
        })
    };
    let both = |f: &dyn Fn(f32, f32) -> f32| -> Option<Value> { lanes(&|v| f(v[0], v[1])) };
    let value = match fun {
        M::Abs => one(&f32::abs),
        M::Sign => one(&f32::signum),
        M::Floor => one(&f32::floor),
        M::Ceil => one(&f32::ceil),
        M::Round => one(&f32::round),
        M::Trunc => one(&f32::trunc),
        M::Fract => one(&|v| v - v.floor()),
        M::Sqrt => one(&f32::sqrt),
        M::InverseSqrt => one(&|v| 1.0 / v.sqrt()),
        M::Exp => one(&f32::exp),
        M::Exp2 => one(&f32::exp2),
        M::Log => one(&f32::ln),
        M::Log2 => one(&f32::log2),
        M::Sin => one(&f32::sin),
        M::Cos => one(&f32::cos),
        M::Tan => one(&f32::tan),
        M::Saturate => one(&|v| v.clamp(0.0, 1.0)),
        M::Radians => one(&f32::to_radians),
        M::Degrees => one(&f32::to_degrees),
        M::Min => both(&f32::min),
        M::Max => both(&f32::max),
        M::Pow => both(&f32::powf),
        M::Step => both(&|edge, x| if x < edge { 0.0 } else { 1.0 }),
        M::Clamp => lanes(&|v| v[0].max(v[1]).min(v[2])),
        M::Mix => lanes(&|v| v[0] + (v[1] - v[0]) * v[2]),
        M::SmoothStep => lanes(&|v| {
            let t = ((v[2] - v[0]) / (v[1] - v[0])).clamp(0.0, 1.0);
            t * t * (3.0 - 2.0 * t)
        }),
        M::Fma => lanes(&|v| v[0].mul_add(v[1], v[2])),
        M::Dot => {
            let (a, b) = (args[0].channels(), args[1].channels());
            match (a, b) {
                (Some(a), Some(b)) if a.len() == b.len() => {
                    Some(Value::Scalar(a.iter().zip(b).map(|(x, y)| x * y).sum()))
                }
                _ => return Err(EvalError("dots shapes that do not match".to_owned())),
            }
        }
        M::Length => match (&args[0], args[0].channels()) {
            (Value::Scalar(v), _) => Some(Value::Scalar(v.abs())),
            (_, Some(values)) => Some(Value::Scalar(
                values.iter().map(|v| v * v).sum::<f32>().sqrt(),
            )),
            _ => return Err(EvalError("measures a shape a vector is not".to_owned())),
        },
        M::Normalize => {
            let length = args[0]
                .channels()
                .map(|values| values.iter().map(|v| v * v).sum::<f32>().sqrt())
                .ok_or_else(|| EvalError("normalizes a shape a vector is not".to_owned()))?;
            map(&args[0], &|v| if length > 0.0 { v / length } else { 0.0 })
        }
        M::Reflect => match (args[0].channels(), args[1].channels()) {
            (Some(i), Some(n)) if i.len() == n.len() => {
                let d: f32 = i.iter().zip(n).map(|(a, b)| a * b).sum();
                let mut out = [0.0; 4];
                for (at, (a, b)) in i.iter().zip(n).enumerate() {
                    out[at] = a - 2.0 * d * b;
                }
                Some(match i.len() {
                    2 => Value::Vec2([out[0], out[1]]),
                    3 => Value::Vec3([out[0], out[1], out[2]]),
                    _ => Value::Vec4(out),
                })
            }
            _ => return Err(EvalError("reflects shapes that do not match".to_owned())),
        },
        M::Cross => match (args[0], args[1]) {
            (Value::Vec3(a), Value::Vec3(b)) => Some(Value::Vec3([
                a[1] * b[2] - a[2] * b[1],
                a[2] * b[0] - a[0] * b[2],
                a[0] * b[1] - a[1] * b[0],
            ])),
            _ => return Err(EvalError("crosses shapes a vec3 is not".to_owned())),
        },
        M::Asin => one(&f32::asin),
        M::Acos => one(&f32::acos),
        M::Atan => one(&f32::atan),
        M::Atan2 => both(&f32::atan2),
        M::Distance => match (args[0].channels(), args[1].channels()) {
            (Some(a), Some(b)) if a.len() == b.len() => Some(Value::Scalar(
                a.iter()
                    .zip(b)
                    .map(|(x, y)| (x - y) * (x - y))
                    .sum::<f32>()
                    .sqrt(),
            )),
            _ => return Err(EvalError("measures shapes that do not match".to_owned())),
        },
        other => {
            return Err(EvalError(format!(
                "uses a function a surface shader does not run: {other:?}"
            )));
        }
    };
    value.ok_or_else(|| EvalError(format!("calls {fun:?} on a shape it does not take")))
}

/// `value` with `f` applied to each channel.
fn map(value: &Value, f: &dyn Fn(f32) -> f32) -> Option<Value> {
    Some(match value {
        Value::Scalar(v) => Value::Scalar(f(*v)),
        Value::Vec2(v) => Value::Vec2([f(v[0]), f(v[1])]),
        Value::Vec3(v) => Value::Vec3([f(v[0]), f(v[1]), f(v[2])]),
        Value::Vec4(v) => Value::Vec4([f(v[0]), f(v[1]), f(v[2]), f(v[3])]),
        Value::Bool(_) | Value::Surface => return None,
    })
}

/// A vector made of `values`, each a scalar or a smaller vector, summed to 2, 3 or 4 channels.
fn compose(values: &[Value]) -> Option<Value> {
    let mut out = Vec::with_capacity(4);
    for value in values {
        match value {
            Value::Scalar(v) => out.push(*v),
            Value::Bool(_) | Value::Surface => return None,
            other => out.extend_from_slice(other.channels()?),
        }
    }
    Some(match out.as_slice() {
        [a, b] => Value::Vec2([*a, *b]),
        [a, b, c] => Value::Vec3([*a, *b, *c]),
        [a, b, c, d] => Value::Vec4([*a, *b, *c, *d]),
        _ => return None,
    })
}

fn literal_value(literal: &naga::Literal) -> Result<f32, EvalError> {
    Ok(match *literal {
        naga::Literal::F32(value) => value,
        naga::Literal::F64(value) => {
            #[allow(
                clippy::cast_possible_truncation,
                reason = "a shader literal; f64 to f32 is what the pipeline does"
            )]
            let value = value as f32;
            value
        }
        naga::Literal::AbstractInt(value) => {
            #[allow(
                clippy::cast_precision_loss,
                reason = "a shader literal; an abstract number is one the compiler narrows"
            )]
            let value = value as f32;
            value
        }
        naga::Literal::AbstractFloat(value) => {
            #[allow(
                clippy::cast_possible_truncation,
                reason = "a shader literal; f64 to f32 is what the compiler narrows"
            )]
            let value = value as f32;
            value
        }
        naga::Literal::I32(value) => {
            #[allow(
                clippy::cast_precision_loss,
                reason = "a shader literal, narrowed to the shader's own f32 inputs"
            )]
            let value = value as f32;
            value
        }
        naga::Literal::U32(value) => float(value),
        naga::Literal::Bool(value) => {
            return Err(EvalError(format!(
                "a bool literal ({value}) is not a colour"
            )));
        }
        other => {
            return Err(EvalError(format!(
                "uses a literal a float shader does not: {other:?}"
            )));
        }
    })
}

fn vector_len(size: naga::VectorSize) -> usize {
    match size {
        naga::VectorSize::Bi => 2,
        naga::VectorSize::Tri => 3,
        naga::VectorSize::Quad => 4,
    }
}

fn pattern_index(component: naga::SwizzleComponent) -> usize {
    use naga::SwizzleComponent as C;
    match component {
        C::X => 0,
        C::Y => 1,
        C::Z => 2,
        C::W => 3,
    }
}

#[cfg(test)]
mod tests;
