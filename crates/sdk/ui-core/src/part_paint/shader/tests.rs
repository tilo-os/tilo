//! The interpreter over the documented input set: a checked shader draws what its arithmetic says, per
//! texel; one the interpreter cannot run is its fallback, never a blank part or a panic.

use super::*;
use crate::part_paint::ShaderSource;
use crate::tokens::shader::Source;

fn tokens() -> PartTokens {
    PartTokens {
        fill: Color::new([0.04, 0.05, 0.06, 1.0]),
        ground: Color::new([0.06, 0.07, 0.09, 1.0]),
        ..PartTokens::default()
    }
}

fn source(wgsl: &str) -> ShaderSource {
    crate::tokens::shader_source(&Source {
        wgsl: wgsl.to_owned(),
        time: 0.0,
    })
}

const SOLID: &str = r"
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
    return fill;
}
";

#[test]
fn a_part_is_its_own_fill_where_the_shader_says_so() {
    let rect = vello::kurbo::Rect::new(0.0, 0.0, 4.0, 2.0);
    let probe = super::run(SOLID, &Inputs::of(&tokens(), rect, 0), 4, 2);
    assert!(probe.is_ok(), "{:?}", probe.err());
    let image = evaluate(&source(SOLID), rect, &tokens(), Color::BLACK);
    assert_eq!((image.width, image.height), (4, 2), "one texel per pixel");
    assert_eq!(image.data.data().get(..3), Some(&[56, 63, 69][..]));
    assert_eq!(image.data.data().get(3), Some(&255));
}

/// The pixel grid comes from `position` and `size`; a shader that steps across them paints blocks,
/// which is the point of a cell shader.
#[test]
fn a_shader_reads_position_and_size_per_texel() {
    let blocky = r"
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
    let cell = floor(position / 2.0);
    let parity = (cell.x + cell.y) % 2.0;
    return mix(fill, raised, vec4<f32>(parity));
}
";
    let rect = vello::kurbo::Rect::new(0.0, 0.0, 4.0, 4.0);
    let image = evaluate(&source(blocky), rect, &tokens(), Color::BLACK);
    let pixels = image.data.data();
    let at = |x: usize, y: usize| {
        let start = (y * 4 + x) * 4;
        pixels.get(start..start + 3).map(<[u8]>::to_vec)
    };
    let probe = super::run(blocky, &Inputs::of(&tokens(), rect, 0), 4, 4);
    assert!(probe.is_ok(), "{:?}", probe.err());
    // Cells (0,0) and (1,1) are one colour; (1,0) and (0,1) the other.
    assert_eq!(at(0, 0), at(3, 3));
    assert_eq!(at(3, 0), at(0, 3));
    assert_ne!(at(0, 0), at(3, 0));
}

#[test]
fn a_shader_the_interpreter_cannot_run_is_its_fallback() {
    let loops = r"
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
    var sum = 0.0;
    for (var i = 0; i < 4; i = i + 1) {
        sum = sum + 0.25;
    }
    return vec4<f32>(sum, 0.0, 0.0, 1.0);
}
";
    let rect = vello::kurbo::Rect::new(0.0, 0.0, 2.0, 2.0);
    let fallback = Color::from_rgb8(200, 10, 10);
    let image = evaluate(&source(loops), rect, &tokens(), fallback);
    assert_eq!(
        image.data.data().get(..4),
        Some(&fallback.to_rgba8().to_u8_array()[..]),
        "a shader the interpreter does not run draws its fallback"
    );
}

#[test]
fn a_huge_part_is_bounded_and_a_tiny_one_has_a_texel() {
    assert_eq!(rail(0.0, 0.0), (1, 1));
    let (width, height) = rail(4000.0, 2000.0);
    assert!(u64::from(width) * u64::from(height) <= u64::from(MAX_TEXELS));
    assert!((f64::from(width) / f64::from(height) - 2.0).abs() < 0.01);
    assert_eq!(rail(40.0, 20.0), (40, 20));
}

/// The documented check refuses what the interpreter would choke on, so a theme learns at load, not
/// from a fallback in production.
#[test]
fn the_documented_check_refuses_control_flow() {
    let looping = SOLID.replace("return fill;", "var x = 0.0;\n    loop {\n        x = x + 1.0;\n        if x > 2.0 { break; }\n    }\n    return fill;");
    let refused = Source {
        wgsl: looping,
        time: 0.0,
    }
    .validate()
    .expect_err("a loop is refused at load");
    assert!(refused.contains("cannot be drawn"), "{refused}");
    // What a theme may write is admitted.
    let admitted = Source {
        wgsl: SOLID.to_owned(),
        time: 0.0,
    };
    assert_eq!(admitted.validate(), Ok(()));
}

/// A settled part in a shader finish is evaluated once and replayed: an idle surface runs no shader,
/// and another program is another picture.
#[test]
fn a_settled_shader_part_is_evaluated_once_and_replayed() {
    use crate::part_paint::{Moment, Part, States, paint_part, part_tokens};
    use crate::theme::{Preset, Theme};
    use crate::tokens::{Finish, SurfaceRole, SurfaceShader};
    let theme_with = |wgsl: &str| {
        let mut document = Preset::Dark.document();
        document.material.window =
            Finish::named("shader", std::collections::BTreeMap::<String, f32>::new())
                .expect("the shader set")
                .with_shader(SurfaceShader {
                    wgsl: wgsl.to_owned(),
                    time: 0.0,
                });
        Theme::from_document(&document).unwrap_or_else(|| panic!("{:?}", Theme::refusal(&document)))
    };
    let paint = |theme: &Theme| {
        let tokens = part_tokens(
            theme,
            SurfaceRole::Window,
            (
                theme.tokens.colors.surfaces.content,
                Color::BLACK,
                Color::TRANSPARENT,
            ),
            theme.tokens.colors.surfaces.base,
        );
        paint_part(
            &mut vello::Scene::new(),
            vello::kurbo::Affine::IDENTITY,
            vello::kurbo::Rect::new(0.0, 0.0, 40.0, 30.0),
            (
                theme.painter(SurfaceRole::Window),
                Part::WindowGround,
                SurfaceRole::Window,
            ),
            Moment::settled(States::REST),
            &tokens,
            theme,
        );
    };
    let theme = theme_with(SOLID);
    let before = EVALUATED.with(std::cell::Cell::get);
    for _ in 0..3 {
        paint(&theme);
    }
    assert_eq!(
        EVALUATED.with(std::cell::Cell::get) - before,
        1,
        "replayed while idle"
    );
    let other = theme_with(&SOLID.replace("return fill;", "return ground;"));
    paint(&other);
    assert_eq!(
        EVALUATED.with(std::cell::Cell::get) - before,
        2,
        "a new program draws anew"
    );
}

/// Functions of scalars alone are one lane: `smoothstep`, `clamp`, `mix`, `min` and `step` over `f32`
/// draw, they are not refused as a shape.
#[test]
fn scalar_only_functions_run() {
    let scalar = SOLID.replace(
        "return fill;",
        "let ramp = smoothstep(0.0, 1.0, clamp(uv.y, 0.0, 1.0));\n    let a = mix(min(ramp, 1.0), step(0.5, uv.x), 0.5);\n    return vec4<f32>(a, a, a, 1.0);",
    );
    let rect = vello::kurbo::Rect::new(0.0, 0.0, 4.0, 4.0);
    let run = super::run(&scalar, &Inputs::of(&tokens(), rect, 0), 4, 4);
    assert!(run.is_ok(), "{:?}", run.err());
    assert_eq!(super::probe(&scalar), Ok(()));
}
