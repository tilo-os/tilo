//! Typography ratchet: the font sizes, size constants and pixel-size text
//! APIs that a `TextRole` should own.
//!
//! Apps say what text *is* (`Title`, `Body`, `Caption`, ...) and the theme
//! decides its size, weight and leading, so one text scale changes every
//! screen at once. Every literal size, every size constant and every call
//! into the pixel-size measuring and drawing API is a place where that chain
//! breaks. They are counted per file in widget, chrome, shell and app
//! production code (`tests/baselines/typography/<unit>.txt`, see
//! `support::ratchet`) and may only fall.

#![allow(
    clippy::expect_used,
    clippy::string_slice,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::path::Path;

use regex::Captures;

use crate::support::ratchet::{Pattern, Ratchet, no_exemptions};

/// The type system's own definitions and the text engine behind it.
fn is_type_engine(path: &Path) -> bool {
    let s = path.to_string_lossy();
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    s.ends_with("/tokens/typography.rs")
        || s.contains("/text_render/")
        || name.starts_with("text_render")
        || name == "text_metrics.rs"
}

/// A size constant whose name says it is a content size in pixels
/// (`READING_CONTENT_PX`) belongs to a document engine, not to chrome.
fn not_a_content_size(c: &Captures<'_>) -> bool {
    !c[1].contains("CONTENT_PX")
}

/// `TextRenderer::measure` has three arguments. Widget layout measurement
/// accepts four (width, height, theme, renderer) and a manual layout's child
/// measurement two (child, constraints); neither carries a pixel text size.
/// A call closed on its line is the text API only with three arguments; an
/// incomplete/multiline call stays counted unless four arguments are visible.
fn is_pixel_text_call(line: &str, c: &Captures<'_>) -> bool {
    if &c[1] != "measure" {
        return true;
    }
    let mut depth = 0_u32;
    let mut quoted = false;
    let mut escaped = false;
    let mut commas = 0_u32;
    let mut last = None;
    let mut closed = false;
    let end = c.get(0).expect("matched call").end();
    for ch in line[end..].chars() {
        if quoted {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                quoted = false;
            }
            continue;
        }
        if ch == ')' && depth == 0 {
            closed = true;
            break;
        }
        if !ch.is_whitespace() {
            last = Some(ch);
        }
        match ch {
            '"' => quoted = true,
            '(' | '[' | '{' => depth = depth.saturating_add(1),
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => commas = commas.saturating_add(1),
            _ => {}
        }
    }
    let separators = commas.saturating_sub(u32::from(last == Some(',')));
    if closed && last.is_some() {
        separators == 2
    } else {
        separators < 3
    }
}

pub fn typography_ratchet() -> Ratchet {
    Ratchet {
        name: "typography",
        roots: &[
            "crates/sdk/ui-core/src/widget",
            "crates/sdk/ui-core/src/chrome",
            "crates/session/shell/src",
            "apps/*/src",
        ],
        patterns: vec![
            Pattern::new(
                "literal_font_size",
                r"(?:TextStyle::new|\.with_(?:scaled_|fixed_)?size|\.text_size|\.with_font_size|\.set_font_size)\(\s*\d+(?:\.\d+)?(?:_f32)?\s*\)",
                "literal font size — give the text a TextRole",
            ),
            Pattern::new(
                "legacy_size_api",
                r"\.(draw_text|draw_text_weighted|measure|measure_weighted|measure_styled|elide_to_width|elide_to_width_weighted)\(",
                "pixel-size text API — measure, draw and elide through the role-based text \
                 API, or compose a Text widget",
            ).keep_line(is_pixel_text_call),
            Pattern::new(
                "font_size_const",
                r"^\s*(?:pub(?:\([a-z]+\))?\s+)?const\s+([A-Z0-9_]*(?:FONT|TEXT|SIZE|TITLE|LABEL|CAPTION|HEADING)[A-Z0-9_]*)\s*:\s*f32\s*=\s*\d",
                "named font size — delete the constant and give the text a TextRole",
            )
            .keep(not_a_content_size),
            Pattern::new(
                "type_step_arithmetic",
                r"typography\.scale\.\w+\s*[-+*/]",
                "arithmetic on a type step invents a size between roles — pick the role that fits",
            ),
            Pattern::new(
                "raw_type_step",
                r"typography\.scale\.|TypeScale::default\(\)",
                "raw type-scale step — name the TextRole instead of a size",
            ),
            Pattern::new(
                "renderer_scale_helper",
                r"renderer_input_style|renderer_type_size",
                "renderer-scale helper — a role is scaled once by the theme; do not scale it again",
            ),
        ],
        exempt: is_type_engine,
        advice: "Give text a TextRole (Display, LargeTitle, Title, Heading, Subheading, Body, \
                 Label, Caption, Reading, Code) with .strong() / .numeric() / .italic() \
                 modifiers; the theme owns size, weight and leading.",
    }
}

/// Applications never pick OpenType features: they say what text is (a
/// `TextKind`, or a widget that knows) and the toolkit resolves the features
/// against the face. Every raw feature setting in an app is counted here and
/// may only fall.
pub fn opentype_ratchet() -> Ratchet {
    Ratchet {
        name: "opentype",
        roots: &["apps/*/src"],
        patterns: vec![Pattern::new(
            "raw_opentype_feature",
            r#"\.with_(?:font_feature|kerning|discretionary_ligatures|small_caps|numeric_style)\(|OpenTypeTag::new\(|FeatureTag::new\(|b"(?:tnum|pnum|lnum|onum|zero|smcp|c2sc|liga|dlig|kern|case|frac|(?:ss|cv)\d\d)""#,
            "raw OpenType feature — say what the text is with a TextKind \
             (Numeric, Secret, Identifier, Code) and let the toolkit pick the features",
        )],
        exempt: no_exemptions,
        advice: "Give text a TextKind (`Text::with_kind`, `TextStyle::with_kind`, \
                 `ColumnDef::with_kind`, a `TextInputKind`) or use a widget that knows \
                 its content; the toolkit applies the features the active face supports.",
    }
}

/// Every key counts the shapes it names and nothing else, so a regex edit
/// that stops matching fails here instead of passing an empty audit.
#[test]
fn typography_patterns_match_exactly_the_shapes_they_name() {
    let ratchet = typography_ratchet();
    let cases: [(&str, &[&str], &[&str]); 6] = [
        (
            "literal_font_size",
            &[
                "MathField::new(\"\").with_size(20.0)",
                "TextStyle::new(13.0)",
                "label.with_scaled_size(11.5)",
                "t.with_fixed_size(12)",
                "input.text_size(14.0_f32)",
                "Text::new(s).with_font_size( 15.0 )",
                "editor.set_font_size(16.0)",
            ],
            &[
                "Container::with_size(10.0, 20.0)",
                "TextStyle::content(px)",
                "Text::new(s).with_size(size)",
                "TextStyle::role(TextRole::Body)",
            ],
        ),
        (
            "legacy_size_api",
            &[
                "tr.draw_text(scene, \"x\", 13.0, ink, p)",
                "tr.draw_text_weighted(scene, s, 12.0, w, ink, p)",
                "let (w, h) = tr.measure(\"x\", 12.0, None);",
                "tr.measure_weighted(s, 11.0, None, TextWeight::Bold)",
                "tr.measure_styled(s, &style)",
                "tr.elide_to_width(s, 12.0, w)",
                "tr.elide_to_width_weighted(s, 12.0, w, wt)",
            ],
            &[
                "tr.measure_role(s, TextRole::Body)",
                "layout.measured_size()",
                "draw_text_role(scene, s)",
            ],
        ),
        (
            "font_size_const",
            &[
                "const TITLE_FONT: f32 = 15.0;",
                "pub const LABEL_SIZE: f32 = 12.0;",
                "pub(crate) const CAPTION_TEXT: f32 = 11.5;",
                "    const HEADING: f32 = 18.0;",
            ],
            &[
                "const READING_CONTENT_PX: f32 = 18.0;",
                "const TITLE_FONT: f64 = 15.0;",
                "const PAD: f32 = 12.0;",
                "const LABEL_SIZE: f32 = theme_size();",
                "let title_size: f32 = 12.0;",
            ],
        ),
        (
            "type_step_arithmetic",
            &[
                "theme.tokens.typography.scale.sm - 1.0",
                "t.typography.scale.base * 1.2",
                "typography.scale.xs/2.0",
            ],
            &["theme.tokens.typography.scale.sm", "typography.scale.lg)"],
        ),
        (
            "raw_type_step",
            &["theme.tokens.typography.scale.sm", "TypeScale::default()"],
            &["TextRole::Caption", "TypeScale::for_base(b)"],
        ),
        (
            "renderer_scale_helper",
            &[
                "let style = renderer_input_style(tr, theme);",
                "renderer_type_size(tr, 14.0)",
            ],
            &["renderer_scale(tr)"],
        ),
    ];
    for (key, hits, misses) in cases {
        let pattern = ratchet.pattern(key);
        for line in hits {
            assert!(pattern.hits(line) > 0, "{key} must count {line:?}");
        }
        for line in misses {
            assert_eq!(pattern.hits(line), 0, "{key} must not count {line:?}");
        }
    }
    let exempt = ratchet.exempt;
    for path in [
        "crates/sdk/ui-core/src/tokens/typography.rs",
        "crates/sdk/ui-core/src/text_render.rs",
        "crates/sdk/ui-core/src/text_render/shaping.rs",
        "crates/sdk/ui-core/src/text_render_cache.rs",
        "crates/sdk/ui-core/src/text_metrics.rs",
    ] {
        assert!(exempt(Path::new(path)), "{path} is the type engine");
    }
    assert!(!exempt(Path::new("apps/mail/src/app.rs")));
}

#[test]
fn layout_measurement_does_not_hide_pixel_text_calls() {
    let ratchet = typography_ratchet();
    let legacy = ratchet.pattern("legacy_size_api");
    for line in [
        "tr.measure(format!(\"{}, {}\", a, b).as_str(), size, Some(w))",
        "tr.measure(",
        "tr.measure(s, size, None,)",
    ] {
        assert_eq!(legacy.hits(line), 1);
    }
    for line in [
        "composer.measure(width, max_height, theme, text)",
        "host.measure(width, f32::INFINITY, theme, tr);",
        "let measured = ctx.measure(1, constraint)?;",
    ] {
        assert_eq!(legacy.hits(line), 0);
    }
    assert_eq!(
        legacy.hits("composer.measure(w, h, theme, tr); tr.measure(s, px, None)"),
        1
    );
    assert_eq!(
        legacy.hits("tr.measure(a, px, None); tr.measure(b, px, None)"),
        2
    );
}

/// Literal sizes, size constants and pixel-size text calls may not grow.
#[test]
fn typography_literals_do_not_grow() {
    typography_ratchet().check();
}

#[test]
fn opentype_pattern_matches_raw_feature_settings_only() {
    let ratchet = opentype_ratchet();
    let pattern = ratchet.pattern("raw_opentype_feature");
    for line in [
        ".with_numeric_style(NumericStyle::TabularLining)",
        "style.with_font_feature(OpenTypeTag::new(*b\"ss01\"), 1)",
        "Text::new(s).with_kerning(false)",
        "FeatureTag::new(b\"zero\")",
        "let tag = *b\"cv11\";",
    ] {
        assert!(pattern.hits(line) > 0, "must count {line:?}");
    }
    for line in [
        "Text::new(s).numeric()",
        "Text::new(code).with_kind(TextKind::Secret)",
        "ColumnDef::new(k, h, w).with_kind(TextKind::Numeric)",
        "let label = b\"kernel\";",
    ] {
        assert_eq!(pattern.hits(line), 0, "must not count {line:?}");
    }
}

/// Raw OpenType feature settings in apps may not grow.
#[test]
fn opentype_features_stay_out_of_apps() {
    opentype_ratchet().check();
}
