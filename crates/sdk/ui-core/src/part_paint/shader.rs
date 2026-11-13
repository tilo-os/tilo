//! Evaluating a theme's surface shader (`crate::tokens::shader`). A surface shader is a straight-line
//! WGSL function over the documented inputs (`shader::check` refuses loops, branches and mutable
//! variables), so the toolkit evaluates it **once per settled part**: one call per texel of an image
//! sized to the part, cached with the part until its size, state, theme or what it samples changes.
//! What it cannot interpret is an error ([`EvalError`]) the caller draws as the part's fallback colour,
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

/// The code behind surface shaders: the grammar check a theme's shader passes when the theme is read,
/// and the interpreter that draws it.
#[derive(Clone, Copy)]
pub struct SurfaceShaderCode {
    /// Whether a theme may draw this program: [`shader::check`], then one texel through the
    /// interpreter.
    pub admit: fn(&str) -> Result<(), String>,
    /// The shader's image for a part ([`evaluate`]).
    pub evaluate:
        fn(&crate::part_paint::ShaderSource, Rect, &PartTokens, Color) -> vello::peniko::ImageData,
}

/// The surface shader's code where it is linked.
#[must_use]
pub fn builtin_surface_shader() -> SurfaceShaderCode {
    SurfaceShaderCode { admit, evaluate }
}

crate::lazy_code! {
    /// The surface shader's code ([`SurfaceShaderCode`]).
    pub static SURFACE_SHADER: SurfaceShaderCode = builtin_surface_shader, feature = "lazy-surface-shader";
}

/// `fallback` over the part: what a shader draws while its code is on the way, and what one that cannot
/// be evaluated draws.
#[must_use]
pub fn fallback_image(fallback: Color) -> vello::peniko::ImageData {
    crate::raster::rgba8(fallback_pixels(fallback, 1, 1), 1, 1)
}

fn admit(wgsl: &str) -> Result<(), String> {
    shader::check(wgsl)?;
    // Checked against the grammar is not yet drawable: what the interpreter
    // cannot run is refused here, at load, rather than drawn as a fallback on
    // every part.
    probe(wgsl).map_err(|reason| format!("surface shader cannot be drawn: {reason}"))
}

/// The shader's image for the part `rect` (logical pixels) in `theme`, drawn with `tokens`; `fallback`
/// is what a shader that cannot be evaluated is drawn as.
#[must_use]
fn evaluate(
    source: &crate::part_paint::ShaderSource,
    rect: Rect,
    tokens: &PartTokens,
    fallback: Color,
) -> vello::peniko::ImageData {
    #[cfg(test)]
    EVALUATED.with(|count| count.set(count.get() + 1));
    let (width, height) = rail(rect.width(), rect.height());
    let inputs = Inputs::of(tokens, rect, source.time_centis);
    match run(&source.wgsl, &inputs, width, height) {
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
    run(wgsl, &inputs, 1, 1)
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

struct Inputs {
    backdrop: [f32; 4],
    size: [f32; 2],
    fill: [f32; 4],
    ink: [f32; 4],
    ground: [f32; 4],
    raised: [f32; 4],
    accent: [f32; 4],
    light: [f32; 4],
    params: [f32; 4],
    time: f32,
}

impl Inputs {
    fn of(tokens: &PartTokens, rect: Rect, time_centis: u32) -> Self {
        let param =
            |index: usize, default: f32| tokens.param(shader::FINISH_PARAMS[index], default);
        Self {
            // A shader's backdrop input is the part's own ground where no
            // backdrop pass sampled what lies below it: the toolkit holds
            // that sample in the band, and the shader gets the band's
            // ground rather than a colour it might not have.
            backdrop: tokens.ground.components,
            size: [
                crate::num::f32_of_f64(rect.width()),
                crate::num::f32_of_f64(rect.height()),
            ],
            fill: tokens.fill.components,
            ink: tokens.ink.components,
            ground: tokens.ground.components,
            raised: tokens.raised.components,
            accent: tokens.accent.components,
            light: tokens.light.components,
            params: [param(0, 0.0), param(1, 1.0), param(2, 1.0), param(3, 0.0)],
            time: float(time_centis) / 100.0,
        }
    }

    fn argument(&self, index: usize) -> Option<Value> {
        Some(match index {
            0 => Value::Vec4(self.backdrop),
            // `uv` and `position` are filled per texel by the caller.
            1 | 3 => Value::Vec2(half(0.0)),
            2 => Value::Vec2(self.size),
            4 => Value::Vec4(self.fill),
            5 => Value::Vec4(self.ink),
            6 => Value::Vec4(self.ground),
            7 => Value::Vec4(self.raised),
            8 => Value::Vec4(self.accent),
            9 => Value::Vec4(self.light),
            10 => Value::Vec4(self.params),
            11 => Value::Scalar(self.time),
            _ => return None,
        })
    }
}

fn half(value: f32) -> [f32; 2] {
    [value, value]
}

#[derive(Clone, Copy)]
struct Texel {
    uv: [f32; 2],
    position: [f32; 2],
}

fn run(wgsl: &str, inputs: &Inputs, width: u32, height: u32) -> Result<Vec<u8>, EvalError> {
    let module = naga::front::wgsl::parse_str(wgsl)
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
            let texel = Texel {
                uv,
                position: [uv[0] * inputs.size[0], uv[1] * inputs.size[1]],
            };
            let rgba = program.shade(inputs, texel)?;
            for channel in rgba {
                pixels.push(encode(channel));
            }
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
}

impl Value {
    fn channels(&self) -> Option<&[f32]> {
        match self {
            Self::Scalar(_) | Self::Bool(_) => None,
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
            Self::Bool(_) => return None,
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
}

impl<'a> Program<'a> {
    fn new(module: &'a naga::Module) -> Result<Self, EvalError> {
        let Some((_, function)) = module.functions.iter().next() else {
            return Err(EvalError("declares no function".to_owned()));
        };
        Ok(Self {
            expressions: &function.expressions,
            body: &function.body,
        })
    }

    fn shade(&self, inputs: &Inputs, texel: Texel) -> Result<[f32; 4], EvalError> {
        let mut scope = Scope::new(self.expressions.len());
        let standing: Vec<(naga::Handle<naga::Expression>, naga::Expression)> = self
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
        for (handle, expression) in standing {
            let value = self.eval(&expression, inputs, texel, &scope)?;
            scope.put(handle, value);
        }
        self.block(self.body, inputs, texel, &mut scope)
            .map(|value| value.widened(4).unwrap_or([0.0, 0.0, 0.0, 1.0]))
    }

    fn block(
        &self,
        block: &naga::Block,
        inputs: &Inputs,
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
            naga::Expression::FunctionArgument(index) => match *index {
                1 => Value::Vec2(texel.uv),
                3 => Value::Vec2(texel.position),
                index => inputs
                    .argument(usize::try_from(index).unwrap_or(usize::MAX))
                    .ok_or_else(|| {
                        EvalError("reads an input the toolkit does not hand it".to_owned())
                    })?,
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
        M::Length => {
            let channels = args[0].channels();
            match channels {
                Some(values) => Some(Value::Scalar(
                    values.iter().map(|v| v * v).sum::<f32>().sqrt(),
                )),
                None => return Err(EvalError("measures a shape a vector is not".to_owned())),
            }
        }
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
        Value::Bool(_) => return None,
    })
}

/// A vector made of `values`, each a scalar or a smaller vector, summed to 2, 3 or 4 channels.
fn compose(values: &[Value]) -> Option<Value> {
    let mut out = Vec::with_capacity(4);
    for value in values {
        match value {
            Value::Scalar(v) => out.push(*v),
            Value::Bool(_) => return None,
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
