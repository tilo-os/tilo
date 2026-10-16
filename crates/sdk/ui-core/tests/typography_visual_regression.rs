//! Golden-image coverage for the gallery's canonical multilingual typography
//! card. The matrix exercises palette, fractional display scaling, and
//! independent text scaling through the real widget/layout/render pipeline.
//!
//! The baselines are Mesa llvmpipe renders; other adapters rasterise
//! antialiasing differently, so the comparison refuses to run on them.
//! Regenerate intentional changes with the "Render visual baseline
//! candidates" workflow (`dev-visual-baselines.yml`), which pins the
//! builder image and llvmpipe. Review candidates, publish their immutable
//! artifacts, and update `artifacts.json`; generated PNGs stay out of Git.
//! Baselines resolve through `tools/test-assets.py`, which verifies their exact
//! length and SHA-256 on every cache read. Prewarm for offline test runs with
//! that tool and set `TILO_TEST_ASSETS_OFFLINE=1`.
//! Locally, inside that image:
//! `UPDATE_TYPOGRAPHY_BASELINES=1 VK_DRIVER_FILES=/usr/share/vulkan/icd.d/lvp_icd.x86_64.json
//! WGPU_ADAPTER_NAME=llvmpipe cargo test -p tilo-ui --test it typography_visual_regression`
//!
//! `typography_baseline_geometry` checks the same layouts against the
//! baselines' dimensions without a GPU, so a layout change that needs new
//! baselines fails in every run rather than only where llvmpipe renders.

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use taffy::{AvailableSpace, NodeId, Size, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point};

use tilo_ui::State;
use tilo_ui::offscreen::OffscreenRenderer;
use tilo_ui::presence::PresenceCtx;
use tilo_ui::text_render::TextRenderer;
use tilo_ui::theme::Theme;
use tilo_ui::widget::Widget;

#[path = "../../../dev/ui-examples/examples/common/gallery_scene.rs"]
#[expect(
    dead_code,
    reason = "this regression renders only the typography showcase from the shared gallery scene"
)]
mod gallery_scene;

const LOGICAL_WIDTH: u32 = 1160;

#[derive(Clone, Copy)]
pub(crate) struct Case {
    pub(crate) name: &'static str,
    light: bool,
    display_scale: f64,
    text_scale: f32,
}

pub(crate) const CASES: &[Case] = &[
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
    adapter: String,
}

/// A laid-out typography card, ready to paint.
struct Laid {
    theme: Theme,
    widget: Box<dyn Widget>,
    text_renderer: TextRenderer,
    width: u32,
    height: u32,
}

#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "LOGICAL_WIDTH/logical_height/display_scale describe a bounded gallery-card canvas \
              (at most a few thousand px, scale <= 1.5), always positive and well within f32's \
              exact-integer range and u32's range"
)]
fn lay_out(case: Case) -> Laid {
    let theme = if case.light {
        Theme::light()
    } else {
        Theme::dark()
    }
    .with_text_scale(case.text_scale);
    let mut widget = gallery_scene::build_typography_showcase(&theme);
    let state = State::new();
    widget.sync(&state.doc, &state.root);

    let catalog = tilo_ui::font_catalog::FontCatalog::controlled_fixture(
        tilo_ui::font_catalog::checkout_root(),
        "en-US",
    );
    let mut text_renderer =
        TextRenderer::with_font_catalog(&catalog).expect("checked-in typography font catalog");
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
    Laid {
        theme,
        widget,
        text_renderer,
        width: (f64::from(LOGICAL_WIDTH) * case.display_scale).round() as u32,
        height: (f64::from(logical_height) * case.display_scale).round() as u32,
    }
}

/// Pixel size of a case's frame, from layout alone.
pub(crate) fn frame_size(case: Case) -> (u32, u32) {
    let laid = lay_out(case);
    (laid.width, laid.height)
}

pub(crate) fn baseline_path(case: Case) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let fixtures = root.join("tests/fixtures/typography");
    let filename = format!("{}.png", case.name);
    let result = std::process::Command::new("python3")
        .arg(root.join("../../../tools/test-assets.py"))
        .arg(fixtures.join("artifacts.json"))
        .args(["--file", &filename])
        .output()
        .expect("run pinned typography asset resolver");
    assert!(
        result.status.success(),
        "resolve immutable typography baseline {filename}: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    PathBuf::from(
        String::from_utf8(result.stdout)
            .expect("UTF-8 asset path")
            .trim(),
    )
}

fn render(case: Case) -> Frame {
    let Laid {
        theme,
        mut widget,
        mut text_renderer,
        width,
        height,
    } = lay_out(case);
    let mut scene = Scene::new();
    widget.draw(
        &mut scene,
        Affine::scale(case.display_scale),
        &mut text_renderer,
        &theme,
        &PresenceCtx::default(),
    );
    let mut renderer = OffscreenRenderer::try_new(width, height)
        .expect("typography golden qualification requires an actual offscreen adapter");
    let info = renderer.adapter_info();
    assert!(
        info.as_ref()
            .is_some_and(|info| info.name.contains("llvmpipe")),
        "typography baselines are Mesa llvmpipe renders, but this run got adapter {info:?}; \
         run with VK_DRIVER_FILES=/usr/share/vulkan/icd.d/lvp_icd.x86_64.json \
         WGPU_ADAPTER_NAME=llvmpipe (see this file's header)"
    );
    let adapter = format!("{info:?}");
    Frame {
        width,
        height,
        // Golden generation and comparison must use the renderer's canonical
        // one-shot path. It retries glyph-atlas/bump warm-up internally;
        // `render` can return a partial first frame even without overflow.
        rgba: renderer.render_resilient(&scene, theme.tokens.colors.surfaces.content),
        adapter,
    }
}

fn write_png(path: &Path, frame: &Frame) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create typography snapshot directory");
    }
    tilo_ui::png::write_rgba8(path, frame.width, frame.height, &frame.rgba)
        .expect("write typography snapshot");
}

fn read_png(path: &Path) -> Frame {
    let bytes = std::fs::read(path).unwrap_or_else(|error| {
        panic!(
            "missing verified typography baseline {}: {error}",
            path.display()
        )
    });
    let decoded = tilo_ui::png::decode_rgba8(&bytes).expect("decode typography PNG");
    Frame {
        width: decoded.width,
        height: decoded.height,
        rgba: decoded.rgba,
        adapter: String::new(),
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
        adapter: actual.adapter.clone(),
    }
}

fn record_evidence(case: Case, frame: &Frame) {
    let Some(directory) = std::env::var_os("TILO_TYPOGRAPHY_EVIDENCE_DIR") else {
        return;
    };
    let directory = PathBuf::from(directory);
    std::fs::create_dir_all(&directory).expect("create typography evidence directory");
    write_png(&directory.join(format!("{}-actual.png", case.name)), frame);
    let receipt = serde_json::json!({
        "case": case.name,
        "width": frame.width,
        "height": frame.height,
        "logical_width": LOGICAL_WIDTH,
        "display_scale": case.display_scale,
        "text_scale": case.text_scale,
        "light": case.light,
        "font_manifest_sha256": tilo_ui::font_catalog::catalog_manifest_sha256(),
        "font_scope": "controlled-product-and-fixtures",
        "font_locale": "en-US",
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "adapter": frame.adapter,
        "wgpu_backend_request": std::env::var("WGPU_BACKEND").ok(),
        "vulkan_driver_files_request": std::env::var("VK_DRIVER_FILES").ok(),
        "vulkan_legacy_icd_request": std::env::var("VK_ICD_FILENAMES").ok(),
    });
    std::fs::write(
        directory.join(format!("{}-receipt.json", case.name)),
        serde_json::to_vec_pretty(&receipt).expect("serialize typography receipt"),
    )
    .expect("write typography receipt");
}

#[test]
fn multilingual_gallery_matches_scale_matrix() {
    tilo_ui::i18n::I18nManager::init("en-US");
    let diagnostics =
        PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("typography-visual-regression");
    let update = std::env::var_os("UPDATE_TYPOGRAPHY_BASELINES").is_some();
    let mut failures = Vec::new();

    for &case in CASES {
        let actual = render(case);
        record_evidence(case, &actual);
        if update {
            let candidate = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/typography")
                .join(format!("{}.png", case.name));
            write_png(&candidate, &actual);
            eprintln!("updated {}", candidate.display());
            continue;
        }

        let baseline = baseline_path(case);
        let expected = read_png(&baseline);
        if (actual.width, actual.height) != (expected.width, expected.height) {
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
