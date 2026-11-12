//! A theme's own surface shader: a short WGSL function a finish names, validated when the theme is
//! read (`docs/architecture/theming.md`, "A surface shader").
//!
//! A theme may paint a surface role with its own colour arithmetic instead of choosing from the
//! built-in painter sets: ordered dithering, pixelation, a hatch, a gradient the palette roles
//! cannot express. The shader is **one pure function** over a fixed, documented set of inputs
//! ([`INPUTS`]), declaring no entry point and no bindings of its own — the toolkit writes the
//! pipeline around it — so it cannot sample anything but what the toolkit samples for it, cannot
//! read the person's data or the window's content, and has no clock: `time` is the static value the
//! theme states.
//!
//! It runs **once per settled frame**: the toolkit evaluates it into the surface's image when the
//! surface, its theme or what the surface samples changes, and replays that image until then, so an
//! idle surface draws no frame.
//!
//! The source is checked when the theme is read ([`Source::validate`]): it must parse as WGSL,
//! declare exactly one function `fs_surface` whose parameters and result are exactly [`INPUTS`],
//! and declare no entry point and no global. A theme that fails is refused, with the reason, and
//! the theme it sits on does not apply.

use serde::{Deserialize, Serialize};

/// The most source a theme may carry, bytes. A surface shader is a few
/// dozen lines; anything longer is a program, not a look.
pub const MAX_SOURCE: usize = 16 * 1024;

/// The function a surface shader must declare.
pub const ENTRY: &str = "fs_surface";

/// What a surface shader may read, in the order it must take them: the
/// parameter's name and type in WGSL, and what it holds. Every colour is
/// linear sRGB with alpha last.
pub const INPUTS: &[(&str, &str, &str)] = &[
    (
        "backdrop",
        "vec4<f32>",
        "what lies below the surface, sampled where this fragment is (the part's own ground where nothing lies below it)",
    ),
    (
        "uv",
        "vec2<f32>",
        "the fragment's position across the part, `0..1` from its top left",
    ),
    ("size", "vec2<f32>", "the part's size in logical pixels"),
    (
        "position",
        "vec2<f32>",
        "the fragment's position in the part, logical pixels from its top left",
    ),
    (
        "fill",
        "vec4<f32>",
        "the part's own fill, as its role resolved it",
    ),
    ("ink", "vec4<f32>", "the ink the part would draw on it"),
    ("ground", "vec4<f32>", "the surface the part stands on"),
    ("raised", "vec4<f32>", "one surface step above the ground"),
    ("accent", "vec4<f32>", "the person's accent"),
    ("light", "vec4<f32>", "the light the theme's surfaces catch"),
    (
        "params",
        "vec4<f32>",
        "the finish's own four numbers, in the order `[`FINISH_PARAMS`]` names them",
    ),
    (
        "time",
        "f32",
        "seconds: the static value the theme states, never a running clock, so a shader cannot animate while nothing moves",
    ),
];

/// The finish parameters a shader may read, as `params.xyzw`.
pub const FINISH_PARAMS: [&str; 4] = ["tint", "saturation", "scale", "phase"];

/// The result every surface shader returns: an opaque or translucent colour
/// for the fragment (alpha last). The toolkit holds what it paints inside
/// the material's legibility band, so a shader cannot break ink's contrast.
pub const RESULT: &str = "vec4<f32>";

/// A surface shader as a theme document states it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    /// The WGSL, declaring `fs_surface` and nothing the toolkit does not
    /// hand it.
    pub wgsl: String,
    /// The `time` input, seconds. `0` for a still surface; a value the theme
    /// states, never the wall clock.
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

    /// Whether a document may apply this source: a non-empty, bounded,
    /// finite-time WGSL program of the shape [`check`] describes.
    ///
    /// # Errors
    /// What is wrong with it, as the theme's author reads it.
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
        // Its program is checked once the code is in; the theme is read
        // again then (`part_paint::SURFACE_SHADER`).
        crate::part_paint::SURFACE_SHADER
            .get()
            .map_or(Ok(()), |code| (code.admit)(&self.wgsl))
    }
}

/// Check that `wgsl` is a surface shader of the documented shape, and name
/// what is wrong when it is not.
///
/// It must parse; declare exactly one function, [`ENTRY`]; that function
/// must take [`INPUTS`] in order and return [`RESULT`]; and the module must
/// declare no entry point and no global variable (no binding, no texture,
/// no sampler, no override): the toolkit owns every input.
///
/// # Errors
/// The first thing wrong with it, with the line and column where the
/// grammar refused it.
pub fn check(wgsl: &str) -> Result<(), String> {
    let module =
        naga::front::wgsl::parse_str(wgsl).map_err(|error| match error.location(wgsl) {
            Some(at) => format!(
                "surface shader does not parse: {} (line {}, column {})",
                error.message(),
                at.line_number,
                at.line_position
            ),
            None => format!("surface shader does not parse: {}", error.message()),
        })?;
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
    if module.functions.len() != 1 {
        return Err(format!(
            "surface shader declares {} functions; it declares one, `{ENTRY}`",
            module.functions.len()
        ));
    }
    let Some((_, function)) = module.functions.iter().next() else {
        return Err(format!(
            "surface shader declares no function; it declares `{ENTRY}`"
        ));
    };
    if function.name.as_deref() != Some(ENTRY) {
        return Err(format!(
            "surface shader's function is `{}`, not `{ENTRY}`",
            function.name.as_deref().unwrap_or("(unnamed)")
        ));
    }
    if function.arguments.len() != INPUTS.len() {
        return Err(format!(
            "`{ENTRY}` takes {} inputs; the toolkit hands it {} ({})",
            function.arguments.len(),
            INPUTS.len(),
            INPUTS
                .iter()
                .map(|(name, ..)| *name)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    for (argument, (name, ty, _)) in function.arguments.iter().zip(INPUTS) {
        let actual = scalar_or_vector(&module, argument.ty);
        if actual.as_deref() != Some(*ty) {
            return Err(format!(
                "`{ENTRY}`'s input `{}` is {}; the toolkit hands it {ty}",
                name,
                actual.unwrap_or_else(|| "not a plain value".to_owned())
            ));
        }
    }
    let Some(result) = &function.result else {
        return Err(format!("`{ENTRY}` returns nothing; it returns {RESULT}"));
    };
    match scalar_or_vector(&module, result.ty).as_deref() {
        Some(RESULT) => Ok(()),
        Some(other) => Err(format!("`{ENTRY}` returns {other}; it returns {RESULT}")),
        None => Err(format!(
            "`{ENTRY}` returns a type a shader cannot; it returns {RESULT}"
        )),
    }
}

/// `ty` as the WGSL a document reads (`vec4<f32>`, `f32`), or `None` for a
/// type a plain input cannot be.
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

/// The WGSL name of `scalar`'s type, where it is an `f32`.
fn scalar_word(scalar: naga::Scalar) -> Option<&'static str> {
    matches!((scalar.kind, scalar.width), (naga::ScalarKind::Float, 4)).then_some("f32")
}

/// The complete WGSL module the toolkit compiles: the pipeline around the
/// theme's `fs_surface`.
///
/// The documented inputs are function arguments here rather than a struct,
/// so a shader declares no types of its own and the toolkit binds
/// everything: the sampled backdrop, the theme's colours for the part, the
/// uniforms and the static time.
#[must_use]
pub fn wrap(shader: &Source) -> String {
    format!(
        r"
struct Uniforms {{
    fill: vec4<f32>,
    ink: vec4<f32>,
    ground: vec4<f32>,
    raised: vec4<f32>,
    accent: vec4<f32>,
    light: vec4<f32>,
    params: vec4<f32>,
    size: vec2<f32>,
    time: f32,
    pad: f32,
}}

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var<uniform> u: Uniforms;

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

@fragment
fn shade_main(in: VOut) -> @location(0) vec4<f32> {{
    let backdrop = textureSampleLevel(src, samp, in.uv, 0.0);
    return fs_surface(
        backdrop,
        in.uv,
        u.size,
        in.uv * u.size,
        u.fill,
        u.ink,
        u.ground,
        u.raised,
        u.accent,
        u.light,
        u.params,
        u.time,
    );
}}

{source}
",
        source = shader.wgsl
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r"
fn fs_surface(
    backdrop: vec4<f32>,
    uv: vec2<f32>,
    size: vec2<f32>,
    position: vec2<f32>,
    fill: vec4<f32>,
    ink: vec4<f32>,
    ground: vec4<f32>,
    raised: vec4<f32>,
    accent: vec4<f32>,
    light: vec4<f32>,
    params: vec4<f32>,
    time: f32,
) -> vec4<f32> {
    let cell = max(params.z, 1.0);
    let pixel = floor(position / cell) * cell + cell * 0.5;
    let grid = pixel / size;
    let amount = vec4<f32>(grid.x * params.x);
    return mix(fill, backdrop, amount);
}
";

    /// A shader over the documented set is admitted, and the module the
    /// toolkit compiles around it is itself valid WGSL.
    #[test]
    fn a_shader_of_the_documented_shape_is_admitted_and_wraps() {
        check(GOOD).expect("the documented shape is admitted");
        let source = Source {
            wgsl: GOOD.to_owned(),
            time: 3.0,
        };
        assert_eq!(source.validate(), Ok(()));
        let module = wrap(&source);
        naga::front::wgsl::parse_str(&module).expect("the wrapper is WGSL");
        assert!(module.contains("fn shade_main"));
        assert!(module.contains(ENTRY));
    }

    /// What a theme author writes wrong is named, not swallowed.
    #[test]
    fn a_shader_of_another_shape_is_refused_with_the_reason() {
        let cases: [(&str, &str); 6] = [
            ("", "0 functions"),
            ("fn other(a: f32) -> f32 { return a; }", "not `fs_surface`"),
            (
                "@fragment fn fs_surface() -> @location(0) vec4<f32> { return vec4<f32>(1.0); }",
                "entry point",
            ),
            (
                "@group(0) @binding(0) var t: texture_2d<f32>;\nfn fs_surface(a: vec4<f32>, b: vec2<f32>, c: vec2<f32>, d: vec2<f32>, e: vec4<f32>, f: vec4<f32>, g: vec4<f32>, h: vec4<f32>, i: vec4<f32>, j: vec4<f32>, k: vec4<f32>, l: f32) -> vec4<f32> { return a; }",
                "global variable",
            ),
            (
                "fn fs_surface(a: f32) -> f32 { return a; }",
                "takes 1 inputs",
            ),
            (
                "fn fs_surface(a: vec4<f32>, b: vec2<f32>, c: vec2<f32>, d: vec2<f32>, e: vec4<f32>, f: vec4<f32>, g: vec4<f32>, h: vec4<f32>, i: vec4<f32>, j: vec4<f32>, k: vec4<f32>, l: f32) -> f32 { return l; }",
                "returns f32",
            ),
        ];
        for (wgsl, reason) in cases {
            let refused = check(wgsl).expect_err(wgsl);
            assert!(refused.contains(reason), "{wgsl}: {refused}");
        }
        let bad_type = GOOD.replace("ink: vec4<f32>", "ink: vec3<f32>");
        let refused = check(&bad_type).expect_err("wrong input type");
        assert!(refused.contains("`ink` is vec3<f32>"), "{refused}");
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
}
