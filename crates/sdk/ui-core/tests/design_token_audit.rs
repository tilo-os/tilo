//! Colour, radius and paint audits. A hard-coded styling literal with no
//! inherited debt fails outright (`forbidden_patterns`); the rest are
//! ratchets over the toolkit, the shell and every app (`theme_ratchet`,
//! `app_paint_ratchet`, `chrome_border_ratchet`) whose counts may only fall.
//! Every value must flow through the theme or a widget so the OS can enforce
//! one design language.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use regex::Regex;
use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::support::ratchet::{
    Pattern, Ratchet, is_skipped, is_test_file, no_exemptions, repository_root, test_module_spans,
};

/// Files the hard-fail scan skips: the theme's own definitions
/// ([`is_theme_source`]) and test files (`support::ratchet::is_test_file`).
fn walk(root: &Path, out: &mut Vec<PathBuf>) {
    crate::support::ratchet::walk(root, &|p: &Path| is_test_file(p) || is_theme_source(p), out);
}

/// Hard failures. Each pattern names a constructor that exists in this tree
/// and currently has zero production hits, so a first hit is a regression,
/// not inherited debt. peniko colour literals go through `Color::from_rgb8`
/// / `from_rgba8` and are ratcheted below (`theme_ratchet`), not
/// forbidden here; spacing literals are ratcheted by
/// `spacing_token_audit.rs`. (An earlier `Padding::all(` pattern named a
/// type that does not exist and was dropped; the colour pattern once also
/// spelled a non-existent `Color::rgb8(` and now names only the live call.)
fn forbidden_patterns() -> Vec<(Regex, &'static str)> {
    vec![
        // A literal glyph colour through `cosmic_text::Color::rgb(r, g, b)`.
        // A digit as the first argument keeps variable-arg calls that route
        // real data through the constructor (`Color::rgb(cr, cg, cb)`) out.
        (
            Regex::new(r"Color::rgb\s*\(\s*\d").unwrap(),
            "hard-coded Color::rgb glyph colour — read it from the Theme",
        ),
        // Hard-coded radius literals on rounded rects
        (
            Regex::new(r"\.to_rounded_rect\(\s*\d+(\.\d+)?\s*\)").unwrap(),
            "hard-coded rounded-rect radius — use theme.radius_standard",
        ),
        // Hard-coded shadow blur literals
        (
            Regex::new(r"blur\s*:\s*\d").unwrap(),
            "hard-coded shadow blur — use theme.elevation.*",
        ),
    ]
}

#[test]
fn audit_patterns_match_exactly_the_shapes_they_name() {
    // Case `forbidden_patterns_match_the_shapes_they_name`.
    // The forbidden set must still bite: a pattern that no longer matches the
    // shape it names would let the audit pass empty.
    {
        let patterns = forbidden_patterns();
        assert_eq!(patterns.len(), 3);
        assert!(
            patterns[0]
                .0
                .is_match("Some(cosmic_text::Color::rgb(100, 110, 120))")
        );
        assert!(
            !patterns[0]
                .0
                .is_match("cosmic_text::Color::rgb(cr, cg, cb)"),
            "real data routed through the constructor is not a literal"
        );
        assert!(
            !patterns[0].0.is_match("Color::from_rgb8(12, 120, 220)"),
            "peniko literals belong to the ratchet, not the hard-fail set"
        );
        assert!(patterns[1].0.is_match("rect.to_rounded_rect(8.0)"));
        assert!(
            !patterns[1]
                .0
                .is_match("rect.to_rounded_rect(theme.radius_standard)")
        );
        assert!(patterns[2].0.is_match("blur: 16.0,"));
        assert!(
            !patterns[2]
                .0
                .is_match("blur: theme.tokens.elevation.raised.blur,")
        );
    }

    // Case `a_file_gated_to_tests_has_no_production_style_lines`.
    // The `#[cfg(test)]` span finder and the baseline plumbing live in
    // `support::ratchet`, shared by every ratchet. The finder
    // covers exactly the item the attribute governs — an earlier copy here
    // treated every `#[cfg(test)]` as a braced module, so a `#[cfg(test)]
    // const …;` or `mod tests;` exempted whatever braced production item
    // followed it.
    {
        let source = "#![cfg(test)]\nuse super::*;\nfn fixture() { let ink = Color::WHITE; }\n";
        assert_eq!(test_module_spans(source), vec![(0, 3)]);
        assert!(test_module_spans("fn draw() { let ink = Color::WHITE; }\n").is_empty());
    }
}

#[test]
fn no_hardcoded_style_literals_in_tree() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    walk(&root, &mut files);
    assert!(!files.is_empty(), "audit found no files to scan");

    let patterns = forbidden_patterns();
    let mut violations = Vec::new();
    for f in &files {
        let content = fs::read_to_string(f).unwrap();
        let skip = test_module_spans(&content);
        for (i, line) in content.lines().enumerate() {
            // The audit polices *draw paths*; a `#[cfg(test)]` module is a
            // fixture living in the same file as the code it covers, and a
            // test that pins how a token is painted has to name its values.
            if is_skipped(line, i, &skip) {
                continue;
            }
            for (re, msg) in &patterns {
                if re.is_match(line) {
                    violations.push(format!(
                        "{}:{}: {} — line: {}",
                        f.display(),
                        i + 1,
                        msg,
                        line.trim()
                    ));
                }
            }
        }
    }
    assert!(
        violations.is_empty(),
        "design-token audit failed with {} violation(s):\n{}",
        violations.len(),
        violations.join("\n")
    );
}

// ===========================================================================
// Ratchets: what is counted per file and may only fall. See
// `support::ratchet` for the engine and the shard layout under
// `tests/baselines/<audit>/<unit>.txt`.

/// The theme's own definitions state literal values by design.
fn is_theme_source(path: &Path) -> bool {
    let s = path.to_string_lossy();
    s.contains("/theme.rs") || s.contains("/tokens/") || s.contains("/presence/render.rs")
}

/// Escape hatches the hard-fail set above cannot see: the real
/// `Color::from_rgb8`/`from_rgba8` constructors, `Color::WHITE`/`BLACK`, and
/// the `surface_white_*`/`border_white_*` tiers HIG §9.5 deprecates. Counted
/// in the toolkit, the shell and every app.
pub fn theme_ratchet() -> Ratchet {
    Ratchet {
        name: "theme",
        roots: &[
            "crates/sdk/ui-core/src",
            "crates/session/shell/src",
            "apps/*/src",
        ],
        patterns: vec![
            // The constructor this tree actually calls, with a literal first
            // argument. A variable first argument (`Color::from_rgba8(c.r(), …)`,
            // the re-alpha idiom) is passing real colour through, not stating one.
            Pattern::new(
                "literal_color",
                r"Color::from_rgba?8\s*\(\s*(0x)?[0-9]",
                "literal colour in a draw path — read it from the Theme",
            ),
            Pattern::new(
                "white_or_black",
                r"Color::(WHITE|BLACK)",
                "Color::WHITE / Color::BLACK — use the theme's ink or scrim token",
            ),
            Pattern::new(
                "theme_blind_white",
                r"\b(surface|border)_white_\d+",
                "surface_white_* / border_white_* declare a strength instead of measuring it \
                 (HIG §9.5) — use Colors::lift() or Colors::hairline_on()",
            ),
        ],
        exempt: is_theme_source,
        advice: "Read every colour from the Theme passed to draw.",
    }
}

/// Glyphs apps have drawn as icons. Emoji (U+1F300–U+1FAFF) are added to the
/// class in [`app_paint_ratchet`].
const ICON_GLYPHS: &str = "✕×⟳↻⌫⇅«»‹›▾▸▴◍◎◷⚓➤⏰";

/// Appearance an app or the shell states for itself instead of taking it
/// from a widget, a region or a theme token. Genuine content drawing keeps a
/// painting surface; its counts stay here and shrink as values move onto
/// tokens.
pub fn app_paint_ratchet() -> Ratchet {
    let glyph = format!("[{ICON_GLYPHS}\\x{{1F300}}-\\x{{1FAFF}}]");
    let glyph_icon = format!(r#""[^"\n]*{glyph}|'{glyph}'"#);
    Ratchet {
        name: "app-paint",
        roots: &["crates/session/shell/src", "apps/*/src"],
        patterns: vec![
            Pattern::new(
                "literal_color_new",
                r"Color::new\(\s*\[\s*[0-9.]",
                "literal colour — read it from the Theme (only status, presence and the \
                 user's own content carry colour)",
            ),
            Pattern::new(
                "ramp_slot",
                r"\.neutral\[|\baccent\[\d",
                "colour ramp slot — use the semantic tier (surfaces.*, text tiers, the \
                 selection lift)",
            ),
            Pattern::new(
                "legacy_alias",
                r"\.(?:bg_app|bg_primary|bg_hover|bg_active)\b",
                "removed theme alias — the framework paints the ground; hover and selection \
                 come from the widget",
            ),
            Pattern::new(
                "desktop_ground",
                r"surfaces\.(?:base|window_card)\b",
                "an app painting a ground — the framework owns the window ground; declare a \
                 region (Content, Sidebar, Inspector) instead",
            ),
            Pattern::new(
                "literal_radius",
                r"\.to_rounded_rect\(\s*\d|RoundedRect::from_rect\([^;]*,\s*\d+(?:\.\d+)?\s*\)|fill_round\([^,]+,[^,]+,\s*\d|\.radius\(\s*\d",
                "literal corner radius — widgets take their radius from their role; content \
                 drawing reads theme.tokens.radii",
            ),
            Pattern::new(
                "literal_alpha",
                r"with_alpha(?:_factor)?\((?:[^)]*[^\w.])?0?\.\d|multiply_alpha\(\s*0?\.\d",
                "literal alpha — use the theme's state, scrim or material token",
            ),
            Pattern::new(
                "raw_paint",
                r"scene\.(?:fill|stroke)\(",
                "app-painted chrome — compose ui widgets; only content drawing paints, from \
                 theme values",
            ),
            Pattern::new(
                "glyph_icon",
                &glyph_icon,
                "a Unicode glyph or emoji used as an icon — use ui::icons",
            ),
            Pattern::new(
                "transparent_shot",
                r"render_png\([^)]*TRANSPARENT",
                "a transparent screenshot — shots render on the framework ground, like the \
                 live window",
            ),
        ],
        exempt: is_theme_source,
        advice: "Say what a thing is (a widget, a role, a region) and let the toolkit draw it.",
    }
}

/// Declared edges on chrome surfaces (HIG §9.5). `border_standard` is one
/// fixed colour per theme; stroked on a raised or overlay surface it is three
/// shades from its ground on the dark preset and vanishes. Chrome derives
/// its edges (`tilo_ui::surface::{edge_on, card, separator}`,
/// `chrome::{panel_border, chrome_hairline, selection_tier}`) instead.
pub fn chrome_border_ratchet() -> Ratchet {
    Ratchet {
        name: "chrome-border",
        roots: &[
            "crates/sdk/ui-core/src/widget",
            "crates/sdk/ui-core/src/chrome",
            "crates/sdk/ui-core/src/surface.rs",
            "crates/sdk/ui-core/src/status_bubble.rs",
            "crates/session/shell/src",
        ],
        patterns: vec![Pattern::new(
            "declared_border",
            r"\.border_standard\b|\.border_strong\b|overlay_border_strong|colors\.neutral\[",
            "declared border/ramp token on a chrome surface (HIG §9.5) — derive the edge with \
             tilo_ui::surface::{edge_on, card, separator} or tilo_ui::chrome::{panel_border, \
             chrome_hairline, selection_tier}",
        )],
        exempt: no_exemptions,
        advice: "Derive chrome edges from the ground they sit on.",
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "one table of must-count and must-not-count lines per key"
)]
#[test]
fn ratcheted_colour_patterns_match_exactly_the_shapes_they_name() {
    let cases: [(Ratchet, &str, &[&str], &[&str]); 12] = [
        (
            theme_ratchet(),
            "literal_color",
            &[
                "Color::from_rgb8(12, 120, 220)",
                "Color::from_rgba8(0x10, 0, 0, 255)",
            ],
            &["Color::from_rgba8(c.r(), c.g(), c.b(), 128)"],
        ),
        (
            theme_ratchet(),
            "white_or_black",
            &["fill(Color::WHITE)", "Color::BLACK.with_alpha(0.4)"],
            &["theme.colors.on_accent"],
        ),
        (
            theme_ratchet(),
            "theme_blind_white",
            &["colors.surface_white_8", "c.border_white_12"],
            &["colors.surface_raised"],
        ),
        (
            app_paint_ratchet(),
            "literal_color_new",
            &[
                "Color::new([0.2, 0.3, 0.4, 1.0])",
                "Color::new([ .5, 0.0, 0.0, 1.0])",
            ],
            &["Color::new([r, g, b, a])", "Color::new(rgba)"],
        ),
        (
            app_paint_ratchet(),
            "ramp_slot",
            &["colors.neutral[3]", "palette.accent[5]"],
            &["colors.neutral_text", "accent[i]", "self.accent_fill"],
        ),
        (
            app_paint_ratchet(),
            "legacy_alias",
            &[
                "theme.bg_app",
                "theme.bg_hover.with_alpha(a)",
                "t.bg_primary",
            ],
            &["theme.bg_application", "surfaces.content"],
        ),
        (
            app_paint_ratchet(),
            "desktop_ground",
            &["c.surfaces.base", "theme.surfaces.window_card"],
            &["surfaces.base_tint", "surfaces.content", "surfaces.raised"],
        ),
        (
            app_paint_ratchet(),
            "literal_radius",
            &[
                "rect.to_rounded_rect(8.0)",
                "RoundedRect::from_rect(r, 6.0)",
                "fill_round(scene, rect, 4.0, ink)",
                "Button::new(\"x\").radius(12.0)",
            ],
            &[
                "rect.to_rounded_rect(theme.tokens.radii.md)",
                "RoundedRect::from_rect(r, radius)",
                "fill_round(scene, rect, radii.sm, ink)",
            ],
        ),
        (
            app_paint_ratchet(),
            "literal_alpha",
            &[
                "ink.with_alpha(0.5)",
                "ink.with_alpha(.35)",
                "ink.with_alpha_factor(x * 0.4)",
                "ink.multiply_alpha(0.6)",
            ],
            &[
                "ink.with_alpha(theme.alpha.hover)",
                "ink.with_alpha(1.0)",
                "ink.with_alpha(12.5 * a)",
            ],
        ),
        (
            app_paint_ratchet(),
            "raw_paint",
            &[
                "scene.fill(Fill::NonZero, t, ink, None, &rect)",
                "ctx.scene.stroke(&s, t, ink, None, &line)",
            ],
            &[
                "fill_rect(scene, rect, ink)",
                "scene.push_layer(blend, 1.0, t, &clip)",
            ],
        ),
        (
            app_paint_ratchet(),
            "glyph_icon",
            &[
                "Button::new(\"✕\")",
                "label: \"⟳ Sync\"",
                "'▾'",
                "\"🔍 Search\"",
            ],
            &["Icon::Close", "\"Close\"", "'a'"],
        ),
        (
            app_paint_ratchet(),
            "transparent_shot",
            &["render_png(&mut app, w, h, Color::TRANSPARENT)"],
            &["render_png(&mut app, w, h, theme.surfaces.content)"],
        ),
    ];
    for (ratchet, key, hits, misses) in cases {
        let pattern = ratchet.pattern(key);
        for line in hits {
            assert!(pattern.hits(line) > 0, "{key} must count {line:?}");
        }
        for line in misses {
            assert_eq!(pattern.hits(line), 0, "{key} must not count {line:?}");
        }
    }
    // The chrome-edge pattern: declared edges hit, derived ones do not.
    let border = chrome_border_ratchet();
    let declared = border.pattern("declared_border");
    for hit in [
        "theme.tokens.colors.border_standard,",
        "let bc = theme.border_standard;",
        "stroke(&Stroke::new(1.0), t, colors.border_strong, None, &shape);",
        "c.overlay_border_strong",
        "colors.neutral[3]",
    ] {
        assert!(declared.hits(hit) > 0, "{hit:?} is a declared edge");
    }
    for miss in [
        "theme.border_focus",
        "Colors::hairline_on(ground, HAIRLINE_ALPHA)",
        "border_standard: p.border,",
        "tilo_ui::surface::edge_on(fill)",
        "let neutral = colors.text_dim;",
    ] {
        assert_eq!(declared.hits(miss), 0, "{miss:?} is not a declared edge");
    }
}

/// Theme escape hatches may not grow in the toolkit, the shell or any app.
#[test]
fn theme_escape_hatches_do_not_grow() {
    theme_ratchet().check();
}

/// App and shell appearance knobs may not grow.
#[test]
fn app_and_shell_paint_knobs_do_not_grow() {
    app_paint_ratchet().check();
}

/// Chrome may not gain a declared edge.
#[test]
fn declared_borders_in_chrome_do_not_grow() {
    chrome_border_ratchet().check();
}

/// The resource app framework's entry points. Each builds its launch from
/// `LaunchSpec`, so an app started through one is on the shared launcher.
/// `ui::main!` expands to `framework::run` or a definition's launch spec.
const FRAMEWORK_LAUNCHERS: [&str; 3] = [
    "framework::run::<",
    "framework::launch_spec::<",
    "ui::main!(",
];
const FRAMEWORK_APP: &str = "crates/sdk/tilo-app/src/framework/app.rs";

/// The signature and body of the top-level `pub fn NAME<` in `source`, up to
/// its closing brace at column zero.
fn function_body<'a>(source: &'a str, name: &str) -> &'a str {
    let (_, rest) = source
        .split_once(&format!("\npub fn {name}<"))
        .unwrap_or_else(|| panic!("{FRAMEWORK_APP} no longer defines `pub fn {name}<`"));
    rest.split_once("\n}\n").map_or(rest, |(body, _)| body)
}

#[test]
fn launchers_build_on_the_shared_launch_specs() {
    // Case `framework_launchers_build_on_the_shared_launch_spec`.
    // Accepting the framework's entry points is sound only while they stay on
    // `LaunchSpec`; a framework that grew its own launcher would take every
    // framework app off the acceptance matrix at once.
    {
        let source = fs::read_to_string(repository_root().join(FRAMEWORK_APP))
            .expect("framework app module is readable");
        let spec = function_body(&source, "launch_spec");
        assert!(
            spec.contains("-> LaunchSpec") && spec.contains("LaunchSpec::from_definition("),
            "framework::launch_spec must build the shared LaunchSpec:\n{spec}"
        );
        let run = function_body(&source, "run");
        assert!(
            run.contains("launch::run_or_exit(&launch_spec::<A>())"),
            "framework::run must launch through framework::launch_spec:\n{run}"
        );
    }

    // Case `product_apps_use_the_shared_responsive_launcher`.
    // Product apps share one viewport/large-type/pseudo-locale audit path. A new
    // bespoke launcher silently loses that acceptance matrix, so keep the fleet on
    // `LaunchSpec` — directly or through a framework entry point that builds it —
    // unless the binary is itself an automation agent rather than a user-facing
    // app window, or a package with no window at all (a `cli` entrypoint).
    {
        let apps = repository_root().join("apps");
        let cli_entrypoint = Regex::new(r#"(?m)^\[entrypoint\]\s*$[^\[]*?^type\s*=\s*"cli""#)
            .expect("entrypoint pattern compiles");
        let mut missing = Vec::new();
        for entry in fs::read_dir(&apps)
            .expect("apps directory exists")
            .flatten()
        {
            let app = entry.path();
            let Ok(manifest) = fs::read_to_string(app.join("tilo-app.toml")) else {
                continue;
            };
            let main = app.join("src/main.rs");
            if !main.is_file()
                || app.file_name().is_some_and(|name| name == "agent")
                || cli_entrypoint.is_match(&manifest)
            {
                continue;
            }
            let source = fs::read_to_string(&main).expect("app main is readable");
            // `launch::run_or_exit` takes only a `&LaunchSpec`, so calling it
            // is the shared launcher even when the spec comes from a library
            // function rather than being named here.
            let launched = source.contains("LaunchSpec")
                || source.contains("launch::run_or_exit(")
                || FRAMEWORK_LAUNCHERS
                    .iter()
                    .any(|entry| source.contains(entry));
            if !launched {
                missing.push(
                    app.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned(),
                );
            }
        }
        assert!(
            missing.is_empty(),
            "product apps missing the shared responsive launcher: {}",
            missing.join(", ")
        );
    }
}
