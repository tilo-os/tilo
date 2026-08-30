//! Golden-image coverage for the gallery's canonical multilingual typography
//! card. The matrix exercises palette, fractional display scaling, and
//! independent text scaling through the real widget/layout/render pipeline.
//!
//! Update intentional changes with:
//! `UPDATE_TYPOGRAPHY_BASELINES=1 cargo test -p ui --test it typography_visual_regression`

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::path::{Path, PathBuf};

use taffy::{AvailableSpace, NodeId, Size, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point};

use crate::support::offscreen::OffscreenRenderer;
use ui::State;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;

#[path = "../examples/common/gallery_scene.rs"]
mod gallery_scene;

const LOGICAL_WIDTH: u32 = 1160;

#[derive(Clone, Copy)]
struct Case {
    name: &'static str,
    light: bool,
    display_scale: f64,
    text_scale: f32,
}

const CASES: &[Case] = &[
    Case {
        name: "dark-display-1-text-1",
        light: false,
        display_scale: 1.0,
        text_scale: 1.0,
    },
    Case {
        name: "light-display-1-text-1",
        light: true,
        display_scale: 1.0,
        text_scale: 1.0,
    },
    Case {
        name: "dark-display-1_25-text-1",
        light: false,
        display_scale: 1.25,
        text_scale: 1.0,
    },
    Case {
        name: "light-display-1_25-text-1",
        light: true,
        display_scale: 1.25,
        text_scale: 1.0,
    },
    Case {
        name: "dark-display-1_5-text-1_25",
        light: false,
        display_scale: 1.5,
        text_scale: 1.25,
    },
    Case {
        name: "light-display-1_5-text-1_25",
        light: true,
        display_scale: 1.5,
        text_scale: 1.25,
    },
];

struct Frame {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

fn render(case: Case) -> Option<Frame> {
    let theme = if case.light {
        Theme::light()
    } else {
        Theme::dark()
    }
    .with_text_scale(case.text_scale);
    let mut widget = gallery_scene::build_typography_showcase(&theme);
    let mut state = State::new();
    widget.sync(&mut state.doc, &state.root);

    let mut text_renderer = TextRenderer::new();
    let mut taffy = TaffyTree::<()>::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let node = widget.build_layout(&mut taffy, &theme, &mut node_map);
    taffy
        .compute_layout_with_measure(
            node,
            Size {
                width: AvailableSpace::Definite(LOGICAL_WIDTH as f32),
                height: AvailableSpace::MaxContent,
            },
            |known, available, node_id, _, _| {
                node_map.get(&node_id).map_or(Size::ZERO, |&ptr| unsafe {
                    (*ptr).measure(known, available, &mut text_renderer)
                })
            },
        )
        .expect("typography gallery layout");
    let logical_height = taffy
        .layout(node)
        .expect("typography gallery root layout")
        .size
        .height
        .ceil()
        .max(1.0) as u32;
    widget.apply_layout(&taffy, node, Point::ZERO);

    let mut scene = Scene::new();
    widget.draw(
        &mut scene,
        Affine::scale(case.display_scale),
        &mut text_renderer,
        &theme,
        &PresenceCtx::default(),
    );
    let width = (f64::from(LOGICAL_WIDTH) * case.display_scale).round() as u32;
    let height = (f64::from(logical_height) * case.display_scale).round() as u32;
    let mut renderer = match OffscreenRenderer::try_new(width, height) {
        Ok(renderer) => renderer,
        Err(error) => {
            eprintln!("offscreen: {error} — skipping typography visual regression");
            return None;
        }
    };
    Some(Frame {
        width,
        height,
        rgba: renderer.render(&scene, theme.bg_app),
    })
}

fn write_png(path: &Path, frame: &Frame) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create typography snapshot directory");
    }
    let file = File::create(path).expect("create typography snapshot");
    let mut encoder = png::Encoder::new(BufWriter::new(file), frame.width, frame.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .expect("write typography PNG header")
        .write_image_data(&frame.rgba)
        .expect("write typography PNG pixels");
}

fn read_png(path: &Path) -> Frame {
    let decoder = png::Decoder::new(BufReader::new(File::open(path).unwrap_or_else(|error| {
        panic!(
            "missing typography baseline {}: {error}; regenerate with UPDATE_TYPOGRAPHY_BASELINES=1",
            path.display()
        )
    })));
    let mut reader = decoder.read_info().expect("read typography PNG header");
    let mut bytes = vec![0; reader.output_buffer_size().expect("PNG buffer size")];
    let info = reader
        .next_frame(&mut bytes)
        .expect("decode typography PNG");
    assert_eq!(info.color_type, png::ColorType::Rgba);
    assert_eq!(info.bit_depth, png::BitDepth::Eight);
    bytes.truncate(info.buffer_size());
    Frame {
        width: info.width,
        height: info.height,
        rgba: bytes,
    }
}

fn diagnostic_diff(actual: &Frame, expected: &Frame) -> Frame {
    let rgba = actual
        .rgba
        .chunks_exact(4)
        .zip(expected.rgba.chunks_exact(4))
        .flat_map(|(a, b)| {
            if a.iter().zip(b).all(|(x, y)| x.abs_diff(*y) <= 1) {
                [0, 0, 0, 255]
            } else {
                [255, a[1].abs_diff(b[1]).saturating_mul(8), 255, 255]
            }
        })
        .collect();
    Frame {
        width: actual.width,
        height: actual.height,
        rgba,
    }
}

#[test]
fn multilingual_gallery_matches_scale_matrix() {
    ui::i18n::I18nManager::init("en-US");
    let baseline_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/typography");
    let update = std::env::var_os("UPDATE_TYPOGRAPHY_BASELINES").is_some();
    let mut failures = Vec::new();

    for &case in CASES {
        let Some(actual) = render(case) else {
            return;
        };
        let baseline = baseline_dir.join(format!("{}.png", case.name));
        if update {
            write_png(&baseline, &actual);
            eprintln!("updated {}", baseline.display());
            continue;
        }

        let expected = read_png(&baseline);
        if (actual.width, actual.height) != (expected.width, expected.height) {
            let diagnostics = std::env::temp_dir().join("typography-visual-regression");
            let actual_path = diagnostics.join(format!("{}-actual.png", case.name));
            write_png(&actual_path, &actual);
            failures.push(format!(
                "{} changed dimensions: actual {}x{}, baseline {}x{}; actual: {}",
                case.name,
                actual.width,
                actual.height,
                expected.width,
                expected.height,
                actual_path.display()
            ));
            continue;
        }
        let differing = OffscreenRenderer::pixels_differing_beyond_aa(&actual.rgba, &expected.rgba);
        if differing > 0 {
            let diagnostics = std::env::temp_dir().join("typography-visual-regression");
            let actual_path = diagnostics.join(format!("{}-actual.png", case.name));
            let diff_path = diagnostics.join(format!("{}-diff.png", case.name));
            write_png(&actual_path, &actual);
            write_png(&diff_path, &diagnostic_diff(&actual, &expected));
            failures.push(format!(
                "{} differs from its typography baseline in {differing} pixels; actual: {}; diff: {}",
                case.name,
                actual_path.display(),
                diff_path.display()
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
