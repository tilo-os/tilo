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
fn fs_surface(s: Surface) -> vec4<f32> {
    return s.fill;
}
";

tilo_test_env::scenario! {
    fn the_shader_interpreter_draws_what_a_checked_shader_says;

    phase a_part_is_its_own_fill_where_the_shader_says_so;
    {
        let rect = vello::kurbo::Rect::new(0.0, 0.0, 4.0, 2.0);
        let probe = super::run(SOLID, &Inputs::of(&tokens(), rect, 0), None, 4, 2);
        assert!(probe.is_ok(), "{:?}", probe.err());
        let image = evaluate(&source(SOLID), rect, &tokens(), None, Color::BLACK);
        assert_eq!((image.width, image.height), (4, 2), "one texel per pixel");
        // The fill reaches the stage linear and leaves it encoded again: as the theme gave it.
        assert_eq!(image.data.data().get(..3), Some(&[10, 13, 15][..]));
        assert_eq!(image.data.data().get(3), Some(&255));
    }

    phase a_shader_reads_position_and_size_per_texel;
    // The pixel grid comes from `position` and `size`; a shader that steps across them paints blocks,
    // which is the point of a cell shader.
    {
        let blocky = r"
fn fs_surface(s: Surface) -> vec4<f32> {
    let cell = floor(s.position / 2.0);
    let parity = (cell.x + cell.y) % 2.0;
    return mix(s.fill, s.raised, vec4<f32>(parity));
}
";
        let rect = vello::kurbo::Rect::new(0.0, 0.0, 4.0, 4.0);
        let image = evaluate(&source(blocky), rect, &tokens(), None, Color::BLACK);
        let pixels = image.data.data();
        let at = |x: usize, y: usize| {
            let start = (y * 4 + x) * 4;
            pixels.get(start..start + 3).map(<[u8]>::to_vec)
        };
        let probe = super::run(blocky, &Inputs::of(&tokens(), rect, 0), None, 4, 4);
        assert!(probe.is_ok(), "{:?}", probe.err());
        // Cells (0,0) and (1,1) are one colour; (1,0) and (0,1) the other.
        assert_eq!(at(0, 0), at(3, 3));
        assert_eq!(at(3, 0), at(0, 3));
        assert_ne!(at(0, 0), at(3, 0));
    }

    phase a_shader_the_interpreter_cannot_run_is_its_fallback;
    {
        let loops = r"
fn fs_surface(s: Surface) -> vec4<f32> {
    var sum = 0.0;
    for (var i = 0; i < 4; i = i + 1) {
        sum = sum + 0.25;
    }
    return vec4<f32>(sum, 0.0, 0.0, 1.0);
}
";
        let rect = vello::kurbo::Rect::new(0.0, 0.0, 2.0, 2.0);
        let fallback = Color::from_rgb8(200, 10, 10);
        let image = evaluate(&source(loops), rect, &tokens(), None, fallback);
        assert_eq!(
            image.data.data().get(..4),
            Some(&fallback.to_rgba8().to_u8_array()[..]),
            "a shader the interpreter does not run draws its fallback"
        );
    }

    phase a_huge_part_is_bounded_and_a_tiny_one_has_a_texel;
    {
        assert_eq!(rail(0.0, 0.0), (1, 1));
        let (width, height) = rail(4000.0, 2000.0);
        assert!(u64::from(width) * u64::from(height) <= u64::from(MAX_TEXELS));
        assert!((f64::from(width) / f64::from(height) - 2.0).abs() < 0.01);
        assert_eq!(rail(40.0, 20.0), (40, 20));
    }

    phase the_documented_check_refuses_control_flow;
    // The documented check refuses what the interpreter would choke on, so a theme learns at load, not
    // from a fallback in production.
    {
        let looping = SOLID.replace("return s.fill;", "var x = 0.0;\n    loop {\n        x = x + 1.0;\n        if x > 2.0 { break; }\n    }\n    return s.fill;");
        let refused = Source {
            wgsl: looping,
            time: 0.0,
        }
        .validate()
        .expect_err("a loop is refused at load");
        assert!(refused.contains("straight through"), "{refused}");
        // What a theme may write is admitted.
        let admitted = Source {
            wgsl: SOLID.to_owned(),
            time: 0.0,
        };
        assert_eq!(admitted.validate(), Ok(()));
    }

    phase scalar_only_functions_run;
    // Functions of scalars alone are one lane: `smoothstep`, `clamp`, `mix`, `min` and `step` over `f32`
    // draw, they are not refused as a shape.
    {
        let scalar = SOLID.replace(
            "return s.fill;",
            "let ramp = smoothstep(0.0, 1.0, clamp(s.uv.y, 0.0, 1.0));\n    let a = mix(min(ramp, 1.0), step(0.5, s.uv.x), 0.5);\n    return vec4<f32>(a, a, a, 1.0);",
        );
        let rect = vello::kurbo::Rect::new(0.0, 0.0, 4.0, 4.0);
        let run = super::run(&scalar, &Inputs::of(&tokens(), rect, 0), None, 4, 4);
        assert!(run.is_ok(), "{:?}", run.err());
        assert_eq!(super::probe(&scalar), Ok(()));
    }

    phase the_lookup_reads_what_lies_behind_at_its_offset;
    // On the CPU, `backdrop(offset)` reads the backdrop a host provided, `offset` logical pixels from
    // the fragment: a stage that looks two pixels right of itself shows the image moved left.
    {
        let rect = vello::kurbo::Rect::new(0.0, 0.0, 4.0, 1.0);
        let behind = crate::raster::rgba8(
            [[0, 0, 0, 255], [80, 80, 80, 255], [160, 160, 160, 255], [240, 240, 240, 255]].concat(),
            4,
            1,
        );
        let shifted = "fn fs_surface(s: Surface) -> vec4<f32> {\n    return backdrop(vec2<f32>(2.0, 0.0));\n}";
        let image = evaluate(
            &source(shifted),
            rect,
            &tokens(),
            Some(&Behind { image: &behind }),
            Color::BLACK,
        );
        let reds: Vec<u8> = image.data.data().chunks(4).map(|texel| texel[0]).collect();
        assert_eq!(reds, [160, 240, 240, 240], "moved left by two, held at the edge");
        let grounded = evaluate(&source(shifted), rect, &tokens(), None, Color::BLACK);
        assert_eq!(
            grounded.data.data().get(..3),
            Some(&[15, 18, 23][..]),
            "with nothing behind, the lookup is the part's ground"
        );
    }

    phase the_liquid_glass_stage_lights_its_rim_and_leaves_its_middle_clear;
    // The shipped liquid-glass theme's stage on the CPU: the flat middle shows what is behind it
    // nearly as it is; the curved rim catches the light.
    {
        let document: crate::theme::ThemeDocument =
            serde_json::from_str(include_str!("../../../../themes/liquid-glass/theme.json"))
                .expect("the theme reads");
        let wgsl = document.material.floating.shader().wgsl.clone();
        let rect = vello::kurbo::Rect::new(0.0, 0.0, 64.0, 64.0);
        let mut glass = tokens();
        glass.radius = 16.0;
        glass.light = Color::WHITE;
        glass.params = vec![
            crate::part_paint::Param { name: "tint".into(), value: 0.0 },
            crate::part_paint::Param { name: "refraction".into(), value: 12.0 },
        ]
        .into();
        let behind = crate::raster::rgba8([60, 90, 140, 255].repeat(64 * 64), 64, 64);
        let image = evaluate(
            &source(&wgsl),
            rect,
            &glass,
            Some(&Behind { image: &behind }),
            Color::BLACK,
        );
        let at = |x: usize, y: usize| {
            let start = (y * 64 + x) * 4;
            image.data.data()[start..start + 3].to_vec()
        };
        let middle = at(32, 32);
        assert!(
            middle.iter().zip([60_u8, 90, 140]).all(|(seen, behind)| seen.abs_diff(behind) < 24),
            "the middle is clear: {middle:?}"
        );
        let rim = at(0, 32);
        let brightness = |rgb: &[u8]| rgb.iter().map(|&c| u32::from(c)).sum::<u32>();
        assert!(brightness(&rim) > brightness(&middle) + 60, "the rim catches the light: {rim:?} over {middle:?}");
    }
}
