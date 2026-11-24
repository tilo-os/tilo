//! A theme's own material stage: a short WGSL function a finish names, validated when the theme is
//! read (`docs/architecture/theming.md`, "A surface shader"; `docs/designs/theme-resource.md`). A
//! theme paints a surface role with its own arithmetic instead of choosing from the built-in
//! painter sets: glass that bends what lies behind it by the surface's normal and catches the light,
//! ordered dithering, a hatch, a gradient the palette roles cannot express.
//!
//! The stage is **one function**, `fs_surface(s: Surface) -> vec4<f32>`, over a fixed, documented
//! [`Surface`](PRELUDE) the toolkit fills per fragment, plus one lookup, `backdrop(offset)`: what
//! lies behind the surface, blurred by the finish's blur, `offset` logical pixels from the fragment.
//! It declares no entry point, binding or global of its own — the toolkit writes the pipeline
//! around it ([`wrap`]) — so it reads nothing but what the toolkit hands it: not the person's data,
//! not the window's content, no clock (`time` is the static value the theme states). It has no
//! loops and at most [`MAX_LOOKUPS`] lookups, so its cost per pixel is bounded.
//!
//! It runs on the GPU as the last pass of the backdrop pass (`crate::backdrop`), and on the CPU
//! raster path through the interpreter (`crate::part_paint::shader`), where `backdrop` reads the
//! backdrop a host provided or the part's ground.

use serde::{Deserialize, Serialize};

/// The most source a theme may carry, bytes.
pub const MAX_SOURCE: usize = 16 * 1024;

/// The function a material stage must declare.
pub const ENTRY: &str = "fs_surface";

/// The lookup a stage calls for what lies behind the surface.
pub const LOOKUP: &str = "backdrop";

/// The most `backdrop` lookups one stage may make per fragment: a refraction, its colour fringe and
/// a few taps of a frost.
pub const MAX_LOOKUPS: usize = 8;

/// What a stage reads, as the fields of `Surface` in order: the field's name and type in WGSL, and
/// what it holds. Colours are linear sRGB with straight alpha; lengths are logical pixels.
pub const FIELDS: &[(&str, &str, &str)] = &[
    (
        "uv",
        "vec2<f32>",
        "the fragment's position across the part, `0..1` from its top left",
    ),
    ("size", "vec2<f32>", "the part's size"),
    (
        "position",
        "vec2<f32>",
        "the fragment's position in the part, from its top left",
    ),
    (
        "distance",
        "f32",
        "signed distance to the part's rounded outline: negative inside, 0 on it",
    ),
    ("radius", "f32", "the outline's corner radius"),
    (
        "edge",
        "vec2<f32>",
        "the outline's outward normal nearest the fragment, unit length",
    ),
    (
        "normal",
        "vec3<f32>",
        "the normal of a slab whose rim curves down over `radius` (at least 6) pixels to the outline: `(0, 0, 1)` on the flat top, tipping toward `edge` at the rim",
    ),
    (
        "pointer",
        "vec3<f32>",
        "the pointer in part coordinates in `xy`; `z` is 1 while the window has a pointer, 0 when it has none",
    ),
    (
        "light_dir",
        "vec3<f32>",
        "unit vector toward the light the theme's surfaces catch (`light_angle`, from above the screen)",
    ),
    ("light", "vec4<f32>", "that light's colour"),
    (
        "fill",
        "vec4<f32>",
        "the part's own fill, as its role resolved it",
    ),
    ("ink", "vec4<f32>", "the ink the part would draw on it"),
    ("ground", "vec4<f32>", "the surface the part stands on"),
    ("raised", "vec4<f32>", "one surface step above the ground"),
    ("accent", "vec4<f32>", "the person's accent"),
    (
        "params",
        "vec4<f32>",
        "the finish's own four numbers, in the order [`FINISH_PARAMS`] names them",
    ),
    (
        "time",
        "f32",
        "seconds: the static value the theme states, never a running clock",
    ),
    (
        "refraction",
        "f32",
        "how far the theme bends what lies behind the surface at its rim (the finish's `refraction`, the slider people set)",
    ),
];

/// The finish parameters a stage may read, as `params.xyzw`.
pub const FINISH_PARAMS: [&str; 4] = ["tint", "saturation", "scale", "phase"];

/// The result every stage returns: the fragment's colour, linear sRGB, alpha last. The toolkit
/// holds what it paints inside the material's legibility band, so a stage cannot break ink's
/// contrast.
pub const RESULT: &str = "vec4<f32>";

/// The declarations the toolkit puts before a theme's stage: `Surface` ([`FIELDS`]). Kept on one
/// line, so a position the compiler names in the theme's source is off by exactly one line.
#[must_use]
pub fn prelude() -> String {
    let fields = FIELDS
        .iter()
        .map(|(name, ty, _)| format!("{name}: {ty}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("struct Surface {{ {fields} }}\n")
}

/// The lookup as the checker and the interpreter see it: a function of the right signature whose
/// body the toolkit supplies ([`wrap`] on the GPU, the interpreter on the CPU). One line.
const LOOKUP_STUB: &str =
    "fn backdrop(offset: vec2<f32>) -> vec4<f32> { return vec4<f32>(offset, 0.0, 0.0); }\n";

/// The source the checker and the interpreter parse: the prelude, the lookup and the theme's stage.
/// The theme's source starts on line [`PRELUDE_LINES`] + 1.
#[must_use]
pub fn checked_module(wgsl: &str) -> String {
    format!("{}{LOOKUP_STUB}{wgsl}", prelude())
}

/// How many lines [`checked_module`] puts before the theme's source.
pub const PRELUDE_LINES: u32 = 2;

/// A surface shader as a theme document states it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub wgsl: String,
    /// The `time` input, seconds. `0` for a still surface; a value the theme states,
    /// never the wall clock.
    #[serde(default)]
    pub time: f32,
}

impl Source {
    /// The shader of a finish that states no source: the toolkit's own.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Whether this is a theme's shader at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.wgsl.trim().is_empty()
    }

    /// Whether a document may apply this source: a non-empty, bounded, finite-time WGSL
    /// program of the shape [`check`] describes. What is wrong with it, as the theme's
    /// author reads it.
    pub fn validate(&self) -> Result<(), String> {
        if self.is_empty() {
            return Err("surface shader is empty".to_owned());
        }
        if self.wgsl.len() > MAX_SOURCE {
            return Err(format!("surface shader is longer than {MAX_SOURCE} bytes"));
        }
        if !self.time.is_finite() {
            return Err("surface shader time is not a number".to_owned());
        }
        crate::part_paint::shader::admit(&self.wgsl)
    }
}

/// Check that `wgsl` is a material stage of the documented shape, and name what is wrong when it is
/// not. With the prelude before it ([`checked_module`]) it must parse; declare exactly one function,
/// [`ENTRY`], taking one `Surface` and returning [`RESULT`]; declare no entry point and no global
/// (no binding, no texture, no sampler, no override): the toolkit owns every input. Its body runs
/// straight through — no loop, no branch, no mutable variable — and calls nothing but [`LOOKUP`],
/// at most [`MAX_LOOKUPS`] times. The pipeline the toolkit wraps around it ([`wrap`]) must then
/// validate as the GPU will compile it. The first thing wrong, with the line and column in the
/// theme's own source where the compiler refused it.
pub fn check(wgsl: &str) -> Result<(), String> {
    let module = parse(&checked_module(wgsl))?;
    if let Some(entry) = module.entry_points.first() {
        return Err(format!(
            "surface shader declares the entry point `{}`; it declares only `{ENTRY}`, a plain function",
            entry.name
        ));
    }
    if !module.global_variables.is_empty() {
        return Err(
            "surface shader declares a global variable; the toolkit hands it every input"
                .to_owned(),
        );
    }
    let own: Vec<_> = module
        .functions
        .iter()
        .filter(|(_, function)| function.name.as_deref() != Some(LOOKUP))
        .collect();
    let [(_, function)] = own.as_slice() else {
        return Err(format!(
            "surface shader declares {} functions; it declares one, `{ENTRY}`",
            own.len()
        ));
    };
    if function.name.as_deref() != Some(ENTRY) {
        return Err(format!(
            "surface shader's function is `{}`, not `{ENTRY}`",
            function.name.as_deref().unwrap_or("(unnamed)")
        ));
    }
    let takes_surface = function.arguments.len() == 1
        && function
            .arguments
            .first()
            .is_some_and(|argument| module.types[argument.ty].name.as_deref() == Some("Surface"));
    if !takes_surface {
        return Err(format!(
            "`{ENTRY}` takes {} inputs; it takes one, `s: Surface`, whose fields are {}",
            function.arguments.len(),
            FIELDS
                .iter()
                .map(|(name, ..)| *name)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !function.local_variables.is_empty() {
        return Err(
            "surface shader declares a `var`; it computes with `let`, straight through".to_owned(),
        );
    }
    let lookup = module.functions.iter().find_map(|(handle, function)| {
        (function.name.as_deref() == Some(LOOKUP)).then_some(handle)
    });
    let mut lookups = 0_usize;
    for statement in &function.body {
        match statement {
            naga::Statement::Emit(_) | naga::Statement::Return { .. } => {}
            naga::Statement::Call {
                function: called, ..
            } if Some(*called) == lookup => {
                lookups = lookups.saturating_add(1);
            }
            naga::Statement::Loop { .. } => {
                return Err("surface shader loops; it runs straight through".to_owned());
            }
            naga::Statement::Call { .. } => {
                return Err(format!(
                    "surface shader calls a function; it may call only `{LOOKUP}`"
                ));
            }
            _ => {
                return Err(
                    "surface shader branches or stores; it runs straight through (use `select`)"
                        .to_owned(),
                );
            }
        }
    }
    if lookups > MAX_LOOKUPS {
        return Err(format!(
            "surface shader looks behind the surface {lookups} times; at most {MAX_LOOKUPS}"
        ));
    }
    let Some(result) = &function.result else {
        return Err(format!("`{ENTRY}` returns nothing; it returns {RESULT}"));
    };
    match scalar_or_vector(&module, result.ty).as_deref() {
        Some(RESULT) => {}
        Some(other) => return Err(format!("`{ENTRY}` returns {other}; it returns {RESULT}")),
        None => {
            return Err(format!(
                "`{ENTRY}` returns a type a shader cannot; it returns {RESULT}"
            ));
        }
    }
    let wrapped = parse(&wrap(&Source {
        wgsl: wgsl.to_owned(),
        time: 0.0,
    }))?;
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::empty(),
    )
    .validate(&wrapped)
    .map(|_| ())
    .map_err(|error| format!("surface shader does not compile: {}", error.as_inner()))
}

/// `wgsl` parsed, or the compiler's reason at its line and column in the theme's own source.
fn parse(wgsl: &str) -> Result<naga::Module, String> {
    naga::front::wgsl::parse_str(wgsl).map_err(|error| match error.location(wgsl) {
        Some(at) => format!(
            "surface shader does not parse: {} (line {}, column {})",
            error.message(),
            at.line_number.saturating_sub(PRELUDE_LINES).max(1),
            at.line_position
        ),
        None => format!("surface shader does not parse: {}", error.message()),
    })
}

fn scalar_or_vector(module: &naga::Module, ty: naga::Handle<naga::Type>) -> Option<String> {
    Some(match module.types[ty].inner {
        naga::TypeInner::Scalar(scalar) => scalar_word(scalar)?.to_owned(),
        naga::TypeInner::Vector { size, scalar } => {
            let size = match size {
                naga::VectorSize::Bi => "vec2",
                naga::VectorSize::Tri => "vec3",
                naga::VectorSize::Quad => "vec4",
            };
            format!("{size}<{}>", scalar_word(scalar)?)
        }
        _ => return None,
    })
}

fn scalar_word(scalar: naga::Scalar) -> Option<&'static str> {
    matches!((scalar.kind, scalar.width), (naga::ScalarKind::Float, 4)).then_some("f32")
}

/// The uniforms the GPU pipeline around a stage reads, in the order [`wrap`] declares them: what
/// [`FIELDS`] does not compute per fragment. Lengths are target pixels; `scale` is target pixels per
/// logical pixel.
pub const UNIFORM_BYTES: u64 = 16 * 10;

/// The complete WGSL module the toolkit compiles: the pipeline around the theme's `fs_surface`.
/// Binding 0 is what lies behind the surface (blurred, in the band), at the region's size; the
/// fragment computes the outline, its normals and the slab before calling the stage.
#[must_use]
pub fn wrap(shader: &Source) -> String {
    format!(
        r"{prelude}
struct Stage {{
    fill: vec4<f32>,
    ink: vec4<f32>,
    ground: vec4<f32>,
    raised: vec4<f32>,
    accent: vec4<f32>,
    light: vec4<f32>,
    params: vec4<f32>,
    // Logical size, corner radius, time.
    size_radius_time: vec4<f32>,
    // Pointer in part coordinates, whether there is one, and the refraction; then the light's
    // direction.
    pointer: vec4<f32>,
    light_dir: vec4<f32>,
}}

@group(0) @binding(0) var behind: texture_2d<f32>;
@group(0) @binding(1) var smooth_: sampler;
@group(0) @binding(2) var<uniform> stage: Stage;

var<private> here: vec2<f32>;

struct VOut {{
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}}

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VOut {{
    let uv = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    var out: VOut;
    out.position = vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0);
    out.uv = uv;
    return out;
}}

fn to_linear(c: vec4<f32>) -> vec4<f32> {{
    let rgb = select(pow((c.rgb + 0.055) / 1.055, vec3<f32>(2.4)), c.rgb / 12.92, c.rgb <= vec3<f32>(0.04045));
    return vec4<f32>(rgb, c.a);
}}

fn to_encoded(c: vec4<f32>) -> vec4<f32> {{
    let v = clamp(c.rgb, vec3<f32>(0.0), vec3<f32>(1.0));
    let rgb = select(1.055 * pow(v, vec3<f32>(1.0 / 2.4)) - 0.055, v * 12.92, v <= vec3<f32>(0.0031308));
    return vec4<f32>(rgb, clamp(c.a, 0.0, 1.0));
}}

fn backdrop(offset: vec2<f32>) -> vec4<f32> {{
    let size = stage.size_radius_time.xy;
    return to_linear(textureSampleLevel(behind, smooth_, here + offset / size, 0.0));
}}

fn sign_of(v: f32) -> f32 {{
    return select(1.0, -1.0, v < 0.0);
}}

@fragment
fn stage_main(in: VOut) -> @location(0) vec4<f32> {{
    here = in.uv;
    let size = stage.size_radius_time.xy;
    let half = size * 0.5;
    let shorter = min(half.x, half.y);
    let radius = clamp(stage.size_radius_time.z, 0.0, shorter);
    let position = in.uv * size;
    let p = position - half;
    let q = abs(p) - (half - vec2<f32>(radius));
    let outside = max(q, vec2<f32>(0.0));
    let distance = length(outside) + min(max(q.x, q.y), 0.0) - radius;
    var edge: vec2<f32>;
    if (q.x > 0.0 && q.y > 0.0) {{
        edge = outside / max(length(outside), 1e-6) * vec2<f32>(sign_of(p.x), sign_of(p.y));
    }} else if (q.x > q.y) {{
        edge = vec2<f32>(sign_of(p.x), 0.0);
    }} else {{
        edge = vec2<f32>(0.0, sign_of(p.y));
    }}
    let rim = clamp(max(radius, 6.0), 1.0, shorter);
    let k = clamp(1.0 + distance / rim, 0.0, 1.0);
    let normal = vec3<f32>(edge * k, sqrt(max(1.0 - k * k, 0.0)));
    var s: Surface;
    s.uv = in.uv;
    s.size = size;
    s.position = position;
    s.distance = distance;
    s.radius = radius;
    s.edge = edge;
    s.normal = normal;
    s.pointer = stage.pointer.xyz;
    s.light_dir = stage.light_dir.xyz;
    s.light = stage.light;
    s.fill = stage.fill;
    s.ink = stage.ink;
    s.ground = stage.ground;
    s.raised = stage.raised;
    s.accent = stage.accent;
    s.params = stage.params;
    s.time = stage.size_radius_time.w;
    s.refraction = stage.pointer.w;
    // The scene clips the image to the part's outline, antialiased; the stage paints its box.
    return to_encoded(fs_surface(s));
}}

{source}
",
        prelude = prelude(),
        source = shader.wgsl
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r"
fn fs_surface(s: Surface) -> vec4<f32> {
    let cell = max(s.params.z, 1.0);
    let pixel = floor(s.position / cell) * cell + cell * 0.5;
    let grid = pixel / s.size;
    let behind = backdrop(-s.normal.xy * 4.0);
    return mix(s.fill, behind, vec4<f32>(grid.x * s.params.x));
}
";

    tilo_test_env::scenario! {
        /// The check a theme's material stage passes before it is wrapped.
        fn a_material_stage_is_admitted_only_in_the_documented_shape;

        phase a_stage_of_the_documented_shape_is_admitted_and_wraps;
        {
            check(GOOD).expect("the documented shape is admitted");
            let source = Source {
                wgsl: GOOD.to_owned(),
                time: 3.0,
            };
            assert_eq!(source.validate(), Ok(()));
            let module = naga::front::wgsl::parse_str(&wrap(&source)).expect("the wrapper is WGSL");
            naga::valid::Validator::new(
                naga::valid::ValidationFlags::all(),
                naga::valid::Capabilities::empty(),
            )
            .validate(&module)
            .expect("the wrapped pipeline validates as the GPU compiles it");
        }

        phase a_stage_of_another_shape_is_refused_with_the_reason;
        {
            let lookups = (0..=MAX_LOOKUPS)
                .map(|at| format!("let b{at} = backdrop(vec2<f32>({at}.0));"))
                .collect::<Vec<_>>()
                .join("\n");
            let too_many = format!("fn fs_surface(s: Surface) -> vec4<f32> {{\n{lookups}\nreturn b0;\n}}");
            let cases: [(&str, &str); 9] = [
                ("", "0 functions"),
                ("fn other(a: f32) -> f32 { return a; }", "not `fs_surface`"),
                (
                    "@fragment fn fs_surface() -> @location(0) vec4<f32> { return vec4<f32>(1.0); }",
                    "entry point",
                ),
                (
                    "@group(0) @binding(0) var t: texture_2d<f32>;\nfn fs_surface(s: Surface) -> vec4<f32> { return s.fill; }",
                    "global variable",
                ),
                ("fn fs_surface(a: f32) -> f32 { return a; }", "it takes one, `s: Surface`"),
                ("fn fs_surface(s: Surface) -> f32 { return s.time; }", "returns f32"),
                (
                    "fn fs_surface(s: Surface) -> vec4<f32> { var c = s.fill; loop { break; } return c; }",
                    "`var`",
                ),
                (
                    "fn fs_surface(s: Surface) -> vec4<f32> { if (s.time > 1.0) { return s.ink; } return s.fill; }",
                    "branches",
                ),
                (&too_many, "at most 8"),
            ];
            for (wgsl, reason) in cases {
                let refused = check(wgsl).expect_err(wgsl);
                assert!(refused.contains(reason), "{wgsl}: {refused}");
            }
            let refused = check("fn fs_surface(s: Surface) -> vec4<f32> {\n    return s.nothing;\n}")
                .expect_err("an unknown field");
            assert!(refused.contains("line 2"), "the position is the theme's own: {refused}");
            let too_long = Source {
                wgsl: " ".repeat(MAX_SOURCE + 1),
                time: 0.0,
            };
            assert!(too_long.validate().is_err());
            let no_time = Source {
                wgsl: GOOD.to_owned(),
                time: f32::NAN,
            };
            assert!(no_time.validate().is_err());
            assert!(Source::empty().validate().is_err());
        }

        phase the_shipped_liquid_glass_theme_is_admitted;
        {
            for document in [
                include_str!("../../../themes/liquid-glass/theme.json"),
                include_str!("../../../themes/liquid-glass-light/theme.json"),
            ] {
                let document: crate::theme::ThemeDocument =
                    serde_json::from_str(document).expect("the theme reads");
                assert!(
                    crate::theme::Theme::from_document(&document).is_some(),
                    "{:?}",
                    crate::theme::Theme::refusal(&document)
                );
            }
        }
    }
}
