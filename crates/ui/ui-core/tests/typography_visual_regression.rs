//! Golden-image coverage for the canonical multilingual typography
//! card. The matrix exercises palette, fractional display scaling, and
//! independent text scaling through the real widget/layout/render pipeline.
//!
//! The baselines are Mesa llvmpipe renders; other adapters rasterise
//! antialiasing differently, so the comparison refuses to run on them.
//! Regenerate intentional changes with tools/ci/visual-baselines.sh
//! in the builder image, which pins llvmpipe. Review candidates, publish their immutable
//! artifacts, and update `artifacts.json`; generated PNGs stay out of Git.
//! Baselines resolve through `tools/test-assets.py`, which verifies their exact
//! length and SHA-256 on every cache read. Prewarm for offline test runs with
//! that tool and set `TILO_TEST_ASSETS_OFFLINE=1`.
//! Locally, inside that image:
//! `UPDATE_TYPOGRAPHY_BASELINES=1 VK_DRIVER_FILES=/usr/share/vulkan/icd.d/lvp_icd.x86_64.json
//! WGPU_ADAPTER_NAME=llvmpipe cargo test -p tilo-ui --test it typography_visual_regression`
//!
//! `typography_golden_geometry` checks the same layouts against the
//! baselines' dimensions without a GPU, so a layout change that needs new
//! baselines fails in every run rather than only where llvmpipe renders.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tilo_test_env::SpawnUnderEnvLock as _;

use taffy::{AvailableSpace, NodeId, Size, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point};

use tilo_ui::State;
use tilo_ui_base::offscreen::OffscreenRenderer;
use tilo_ui_base::presence::PresenceCtx;
use tilo_ui_base::text_render::TextRenderer;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::widget::Widget;

use tilo_ui_base::tokens::TextRole;
use tilo_ui::widget::{Column, Container, Divider, Row, Text};
use tilo_ui_base::{FontChoice, HangingPunctuation, NumericStyle, OpenTypeTag, OpticalSizing, ParagraphAlign};

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
    let mut widget = build_typography_showcase(&theme);
    let state = State::new();
    widget.sync(&state.doc, &state.root);

    let mut text_renderer = tilo_ui_base::font_catalog::fixture_renderer();
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

pub(crate) fn baseline_path(case: Case) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let fixtures = root.join("tests/fixtures/typography");
    let filename = format!("{}.png", case.name);
    let result = std::process::Command::new("python3")
        .arg(root.join("../../../tools/test-assets.py"))
        .arg(fixtures.join("artifacts.json"))
        .args(["--file", &filename])
        .output_locked()
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
    widget.draw(&mut tilo_ui_base::draw::DrawCtx::new(
        &mut scene,
        Affine::scale(case.display_scale),
        &mut text_renderer,
        &theme,
        PresenceCtx::default().draw_env(),
    ));
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
        "font_manifest_sha256": tilo_ui_base::font_catalog::catalog_manifest_sha256(),
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
    tilo_ui_base::i18n::I18nManager::init("en-US");
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

// The multilingual typography card the baselines were rendered from.

fn dim(t: &str, theme: &Theme) -> Box<dyn Widget> {
    Box::new(
        Text::new(t)
            .with_role(TextRole::Caption)
            .with_color(theme.text_dim),
    )
}

/// A titled card: heading + divider + body, on a raised background.
fn card(heading: &str, body: Box<dyn Widget>) -> Box<dyn Widget> {
    let inner = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_gap(12.0)
        .push(Box::new(Text::new(heading).with_role(TextRole::Heading)))
        .push(Box::new(Divider::horizontal()))
        .push(body);
    Box::new(
        Container::new()
            .with_background()
            .with_child(Box::new(inner)),
    )
}

fn col(gap: f32) -> Column {
    Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_gap(gap)
}

fn specimen_cell(caption: &str, specimen: Text, theme: &Theme, width: f32) -> Box<dyn Widget> {
    Box::new(
        Container::new()
            .with_no_padding()
            .with_width(width)
            .with_child(Box::new(
                col(4.0).push(dim(caption, theme)).push(Box::new(specimen)),
            )),
    )
}

fn specimen_pair(
    theme: &Theme,
    before_caption: &str,
    before: Text,
    after_caption: &str,
    after: Text,
) -> Box<dyn Widget> {
    Box::new(
        Row::new()
            .push(specimen_cell(before_caption, before, theme, 520.0))
            .push(specimen_cell(after_caption, after, theme, 520.0)),
    )
}

fn typography_intro_and_serif(column: Column, _theme: &Theme) -> Column {
    column
        .push(Box::new(
            Text::new("A clear hierarchy makes dense interfaces feel calm.")
                .with_role(TextRole::LargeTitle),
        ))
        .push(Box::new(
            Text::new("Every role derives from one body size").with_role(TextRole::Title),
        ))
        .push(Box::new(
            Text::new("Section heading").with_role(TextRole::Heading),
        ))
        .push(Box::new(
            Text::new("Group label inside a section").with_role(TextRole::Subheading),
        ))
        .push(Box::new(
            Text::new("Body text is the default: what most of the interface says.")
                .with_role(TextRole::Body),
        ))
        .push(Box::new(
            Text::new("Control label, or a row title in a dense list").with_role(TextRole::Label),
        ))
        .push(Box::new(
            Text::new("Caption: a secondary line, a timestamp, metadata.")
                .with_role(TextRole::Caption),
        ))
        .push(Box::new(
            Text::new(
                "Serif reading — AVATAR · WAVE · To · Ta · Yo · office · affinity · ffi · naïve café.",
            )
            .with_role(TextRole::Reading),
        ))
        .push(Box::new(
            Text::new("Italic serif — Typography should feel composed, never merely typeset.")
                .with_role(TextRole::Reading)
                .with_italic(true),
        ))
}

fn typography_feature_pairs(column: Column, theme: &Theme) -> Column {
    let reading = || TextRole::Reading;
    column
        .push(specimen_pair(
            theme,
            "Before: kerning disabled",
            Text::new("AVATAR  WAVE  To Ta Yo")
                .with_role(reading())
                .with_kerning(false),
            "After: kerning enabled",
            Text::new("AVATAR  WAVE  To Ta Yo")
                .with_role(reading())
                .with_kerning(true),
        ))
        .push(specimen_pair(
            theme,
            "Before: standard forms",
            Text::new("office · affinity · historic 1847").with_role(reading()),
            "After: discretionary ligatures",
            Text::new("office · affinity · historic 1847")
                .with_role(reading())
                .with_discretionary_ligatures(true),
        ))
        .push(specimen_pair(
            theme,
            "Before: proportional lining figures",
            Text::new("1847  2026  €12,408.50")
                .with_role(reading())
                .with_numeric_style(NumericStyle::ProportionalLining),
            "After: tabular old-style figures",
            Text::new("1847  2026  €12,408.50")
                .with_role(reading())
                .with_numeric_style(NumericStyle::TabularOldstyle),
        ))
        .push(specimen_pair(
            theme,
            "Before: Adwaita Sans wght 300 · opsz 14",
            Text::new("Variable fonts adapt continuously")
                .with_role(TextRole::Title)
                .with_font_family(FontChoice::Named("Adwaita Sans".into()))
                .with_font_variation(OpenTypeTag::WEIGHT, 300.0)
                .with_optical_sizing(OpticalSizing::Value(14.0)),
            "After: Adwaita Sans wght 720 · optical sizing auto",
            Text::new("Variable fonts adapt continuously")
                .with_role(TextRole::Title)
                .with_font_family(FontChoice::Named("Adwaita Sans".into()))
                .with_font_variation(OpenTypeTag::WEIGHT, 720.0)
                .with_optical_sizing(OpticalSizing::Auto),
        ))
}

fn typography_wrapping_and_punctuation(column: Column, theme: &Theme) -> Column {
    column
        .push(specimen_pair(
            theme,
            "Before: ordinary wrapping",
            Text::new("A balanced heading wraps its thought into lines of comparable visual weight.")
                .with_role(TextRole::Title),
            "After: balanced wrapping",
            Text::new("A balanced heading wraps its thought into lines of comparable visual weight.")
                .with_role(TextRole::Title)
                .with_balanced_wrap(),
        ))
        .push(specimen_pair(
            theme,
            "Before: ragged start",
            Text::new(
                "Good typography makes a paragraph feel even before it is read. Spacing should support the argument without calling attention to itself.",
            )
            .with_role(TextRole::Reading),
            "After: full justification",
            Text::new(
                "Good typography makes a paragraph feel even before it is read. Spacing should support the argument without calling attention to itself.",
            )
            .with_role(TextRole::Reading)
            .with_align(ParagraphAlign::Justified),
        ))
        .push(specimen_pair(
            theme,
            "Before: punctuation inside the measure",
            Text::new("“A composed edge,” she wrote, “lets the words lead.”")
                .with_role(TextRole::Reading),
            "After: opening and closing punctuation hangs",
            Text::new("“A composed edge,” she wrote, “lets the words lead.”")
                .with_role(TextRole::Reading)
                .with_hanging_punctuation(HangingPunctuation::BOTH),
        ))
        .push(dim(
            "Pagination: RichTextLayout enforces configurable two-line widow and orphan minima at page and column breaks.",
            theme,
        ))
}

fn typography_scripts_and_bidi(column: Column, _theme: &Theme) -> Column {
    column
                .push(Box::new(
                    Text::new("const greeting = \"Hello, 世界\";")
                        .with_role(TextRole::Code),
                ))
                .push(Box::new(
                    Text::new("العربية — مرحبًا بالعالم").with_role(TextRole::Body),
                ))
                .push(Box::new(
                    Text::new("עברית — שלום עולם").with_role(TextRole::Body),
                ))
                .push(Box::new(
                    Text::new("हिन्दी — नमस्ते दुनिया  ·  বাংলা — নমস্কার বিশ্ব")
                        .with_role(TextRole::Body),
                ))
                .push(Box::new(
                    Text::new("ไทย — สวัสดีชาวโลก  ·  日本語 東京  ·  简体中文 上海")
                        .with_role(TextRole::Body),
                ))
                .push(Box::new(
                    Text::new(
                        "Thai dictionary wrap (no spaces): ทุกสองสัปดาห์ทุกสองสัปดาห์ทุกสองสัปดาห์ทุกสองสัปดาห์ทุกสองสัปดาห์ทุกสองสัปดาห์ทุกสองสัปดาห์ทุกสองสัปดาห์ทุกสองสัปดาห์ทุกสองสัปดาห์ทุกสองสัปดาห์ทุกสองสัปดาห์ทุกสองสัปดาห์ทุกสองสัปดาห์ทุกสองสัปดาห์ทุกสองสัปดาห์ทุกสองสัปดาห์ทุกสองสัปดาห์ทุกสองสัปดาห์ทุกสองสัปดาห์",
                    )
                    .with_role(TextRole::Body),
                ))
                .push(Box::new(
                    col(4.0)
                        .push(Box::new(
                            Text::new("English auto hyphenation:")
                                .with_role(TextRole::Body),
                        ))
                        .push(Box::new(
                            Container::new().with_no_padding().with_width(190.0).with_child(
                                Box::new(
                                    Text::new("Internationalization demonstrates typography.")
                                        .with_role(TextRole::Reading)
                                        .with_auto_hyphenation("en-US"),
                                ),
                            ),
                        )),
                ))
                .push(Box::new(
                    Text::new("Mixed BiDi: Invoice 123 — חשבון ٤٥٦ — total €78")
                        .with_role(TextRole::Body),
                ))
                .push(Box::new(
                    Text::new("Emoji clusters: 👩🏽‍💻  👨‍👩‍👧‍👦  🏳️‍🌈  🇪🇺")
                        .with_role(TextRole::Body),
                ))
}

fn build_typography_showcase(theme: &Theme) -> Box<dyn Widget> {
    let column = typography_intro_and_serif(col(9.0), theme);
    let column = typography_feature_pairs(column, theme);
    let column = typography_wrapping_and_punctuation(column, theme);
    let column = typography_scripts_and_bidi(column, theme);
    card("Typography & Scripts", Box::new(column))
}
