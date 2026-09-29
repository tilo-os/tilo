//! Mechanical audit that fails if a hard-coded styling literal sneaks into
//! any widget or shell source file. Every literal must flow through
//! `theme.*` so the OS can enforce one design language.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use regex::Regex;
use std::collections::BTreeMap;
use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::support::ratchet::{
    Counts, is_skipped, ratchet_failures, read_baseline, test_module_spans, write_baseline,
};

/// Files exempt from the audit:
///  - theme.rs (the single source of truth for literal color values)
///  - tokens/** (palette sub-modules — the token origin, not token consumers)
///  - presence/render.rs (the HSL helper intentionally encodes per-user hue math)
///  - tests/**, `*_tests.rs`, and `#[cfg(test)]` modules inside a src file
///    (test fixtures — see `support::ratchet::test_module_spans`). Path-based
///    test modules extracted beside production code use the `*_tests.rs`
///    suffix so they stay out of the ratchet the same way inline test bodies do.
fn is_exempt(path: &Path) -> bool {
    let s = path.to_string_lossy();
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_default();
    s.contains("/theme.rs")
        || s.contains("/tokens/")
        || s.contains("/presence/render.rs")
        || s.contains("/tests/")
        || name == "tests.rs"
        || name.ends_with("_tests.rs")
}

fn walk(root: &Path, out: &mut Vec<PathBuf>) {
    crate::support::ratchet::walk(root, &is_exempt, out);
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("UI crate lives under crates/sdk in the repository")
        .to_path_buf()
}

/// Hard failures. Each pattern names a constructor that exists in this tree
/// and currently has zero production hits, so a first hit is a regression,
/// not inherited debt. peniko colour literals go through `Color::from_rgb8`
/// / `from_rgba8` and are ratcheted below (`ratcheted_patterns`), not
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
    // `support::ratchet`, shared with `spacing_token_audit.rs`. The finder
    // covers exactly the item the attribute governs — an earlier copy here
    // treated every `#[cfg(test)]` as a braced module, so a `#[cfg(test)]
    // const …;` or `mod tests;` exempted whatever braced production item
    // followed it.
    {
        let source = "#![cfg(test)]\nuse super::*;\nfn fixture() { let ink = Color::WHITE; }\n";
        assert_eq!(test_module_spans(source), vec![(0, 3)]);
        assert!(test_module_spans("fn draw() { let ink = Color::WHITE; }\n").is_empty());
    }

    // Case `declared_border_pattern_matches_the_shapes_it_names`.
    // The chrome-edge pattern must match the shapes it names and nothing else.
    {
        let (_, re, _) = declared_border_patterns().remove(0);
        for hit in [
            "theme.tokens.colors.border_standard,",
            "let bc = theme.border_standard;",
            "stroke(&Stroke::new(1.0), t, colors.border_strong, None, &shape);",
            "c.overlay_border_strong",
            "colors.neutral[3]",
        ] {
            assert!(
                re.is_match(hit),
                "{hit:?} is a declared edge and must match"
            );
        }
        for miss in [
            "theme.border_focus",
            "Colors::hairline_on(ground, HAIRLINE_ALPHA)",
            "border_standard: p.border,",
            "tilo_ui::surface::edge_on(fill)",
            "let neutral = colors.text_dim;",
        ] {
            assert!(
                !re.is_match(miss),
                "{miss:?} is not a declared edge and must not match"
            );
        }
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
// Escape hatches `no_hardcoded_style_literals_in_tree` cannot see: the real
// `Color::from_rgb8`/`from_rgba8` constructors, `Color::WHITE`/`Color::BLACK`,
// and the `surface_white_*`/`border_white_*` tiers HIG §9.5 deprecates.
//
// Turning these into hard failures would break hundreds of existing lines, so
// they're ratcheted instead: `design_token_baseline.txt` records what each
// file carries today, a file may never carry more, and one that carries
// fewer lowers its record in the same commit. The number only goes down.

/// Path to the recorded counts, relative to the crate root.
const BASELINE: &str = "tests/design_token_baseline.txt";

/// Cross-product baseline. The toolkit has had a theme escape-hatch ratchet
/// for a while; without the same guard around apps and shell chrome, bespoke
/// colours can keep entering through the much larger product surface.
const APP_SHELL_BASELINE: &str = "tests/app_shell_design_token_baseline.txt";

/// Patterns that are ratcheted rather than forbidden outright. The key is what
/// the baseline file names; the message is what a new hit is told to do.
fn ratcheted_patterns() -> Vec<(&'static str, Regex, &'static str)> {
    vec![
        (
            "literal_color",
            // The constructor this tree actually calls, with a literal first
            // argument. A variable first argument (`Color::from_rgba8(c.r(), …)`,
            // the re-alpha idiom) is passing real colour through, not stating one.
            Regex::new(r"Color::from_rgba?8\s*\(\s*(0x)?[0-9]").unwrap(),
            "literal colour in a draw path — read it from the Theme",
        ),
        (
            "white_or_black",
            Regex::new(r"Color::(WHITE|BLACK)").unwrap(),
            "Color::WHITE / Color::BLACK — use the theme's on-accent ink or scrim token",
        ),
        (
            "theme_blind_white",
            Regex::new(r"\b(surface|border)_white_\d+").unwrap(),
            "surface_white_* / border_white_* declare a strength instead of measuring it \
             (HIG §9.5) — use Colors::lift() or Colors::hairline_on()",
        ),
    ]
}

/// Count every ratcheted pattern per file, skipping comments and `#[cfg(test)]`
/// modules exactly as the audit above does.
fn ratcheted_counts(files: &[PathBuf], root: &Path) -> Counts {
    counts_for(files, root, &ratcheted_patterns())
}

/// The shared counter behind every ratchet in this file: `patterns` per file,
/// keyed by the path relative to `root`.
fn counts_for(
    files: &[PathBuf],
    root: &Path,
    patterns: &[(&'static str, Regex, &'static str)],
) -> Counts {
    let mut counts: Counts = BTreeMap::new();
    for f in files {
        let content = fs::read_to_string(f).unwrap();
        let skip = test_module_spans(&content);
        let rel = f
            .strip_prefix(root)
            .unwrap_or(f)
            .to_string_lossy()
            .replace('\\', "/");
        for (i, line) in content.lines().enumerate() {
            if is_skipped(line, i, &skip) {
                continue;
            }
            for (key, re, _) in patterns {
                if re.is_match(line) {
                    let hit = counts.entry((rel.clone(), (*key).to_string())).or_insert(0);
                    *hit = hit.saturating_add(1);
                }
            }
        }
    }
    counts
}

/// What a new hit of `pattern` is told to do.
fn ratchet_message(pattern: &str) -> String {
    ratcheted_patterns()
        .into_iter()
        .find(|(key, _, _)| *key == pattern)
        .map(|(_, _, message)| message.to_string())
        .unwrap_or_default()
}

const BASELINE_HEADER: &[&str] = &[
    "Theme escape hatches in app and shell production code.",
    "Format: <path relative to repository root> <pattern> <count>",
    "This is a ratchet, not approval: counts may only move down.",
];

/// The escape hatches the older audit was blind to may not grow, and a file
/// that has shed some has to say so — the record only moves downward.
#[test]
fn theme_escape_hatches_do_not_grow() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    walk(&root.join("src"), &mut files);
    assert!(!files.is_empty(), "audit found no files to scan");
    files.sort();

    let found = ratcheted_counts(&files, root);
    let baseline_path = root.join(BASELINE);
    if std::env::var_os("UPDATE_DESIGN_BASELINE").is_some_and(|value| value == "1") {
        write_baseline(&baseline_path, BASELINE_HEADER, &found);
        return;
    }
    let baseline = read_baseline(&baseline_path);
    let (grew, stale) = ratchet_failures(&found, &baseline, &ratchet_message);

    assert!(
        grew.is_empty(),
        "{} file(s) gained a hard-coded colour the Theme should own.\n\
         Read the colour from the Theme instead. If it genuinely cannot be a \
         token, say why in a comment on the line and raise its count in {BASELINE}.\n{}",
        grew.len(),
        grew.join("\n"),
    );
    assert!(
        stale.is_empty(),
        "{} baseline entr(ies) in {BASELINE} are higher than the tree.\n\
         The ratchet only moves down — lower these so the progress is kept:\n{}",
        stale.len(),
        stale.join("\n"),
    );
}

/// App and shell styling debt may only shrink. Set
/// `UPDATE_APP_SHELL_DESIGN_BASELINE=1` only after reviewing the initial or
/// deliberately lowered counts; CI never sets it.
#[test]
fn app_and_shell_theme_escape_hatches_do_not_grow() {
    let ui_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let repository = repository_root();
    let mut files = Vec::new();
    walk(&repository.join("apps"), &mut files);
    walk(&repository.join("crates/session/shell/src"), &mut files);
    assert!(
        !files.is_empty(),
        "cross-product audit found no files to scan"
    );
    files.sort();

    let found = ratcheted_counts(&files, &repository);
    let baseline_path = ui_root.join(APP_SHELL_BASELINE);
    if std::env::var_os("UPDATE_APP_SHELL_DESIGN_BASELINE").is_some_and(|value| value == "1") {
        write_baseline(&baseline_path, BASELINE_HEADER, &found);
        return;
    }
    let baseline = read_baseline(&baseline_path);
    let (grew, stale) = ratchet_failures(&found, &baseline, &ratchet_message);

    assert!(
        grew.is_empty(),
        "{} app/shell file(s) gained theme-blind styling. Use shared Theme tokens.\n{}",
        grew.len(),
        grew.join("\n"),
    );
    assert!(
        stale.is_empty(),
        "{} app/shell baseline entr(ies) are stale. Keep the improvement by lowering {}.\n{}",
        stale.len(),
        APP_SHELL_BASELINE,
        stale.join("\n"),
    );
}

// ===========================================================================
// Declared edges on chrome surfaces (HIG §9.5). `border_standard` is one fixed
// colour per theme; stroked on a raised or overlay surface it is three shades
// from its ground on the dark preset and vanishes. Chrome derives its edges
// (`tilo_ui::surface::{edge_on, card, separator}`, `chrome::{panel_border,
// chrome_hairline, selection_tier}`) instead. Apps are not in this scope: §7
// allows `border_standard` as a rule on `surfaces.base` in app content.

/// Recorded counts of declared border / ramp tokens in chrome production code.
const CHROME_BORDER_BASELINE: &str = "tests/chrome_declared_border_baseline.txt";

/// The one ratcheted pattern for chrome edges, in the same shape as
/// [`ratcheted_patterns`] so the shared counter and baseline plumbing apply.
fn declared_border_patterns() -> Vec<(&'static str, Regex, &'static str)> {
    vec![(
        "declared_border",
        Regex::new(
            r"\.border_standard\b|\.border_strong\b|overlay_border_strong|colors\.neutral\[",
        )
        .unwrap(),
        "declared border/ramp token on a chrome surface (HIG §9.5) — derive the edge with \
         tilo_ui::surface::{edge_on, card, separator} or tilo_ui::chrome::{panel_border, \
         chrome_hairline, selection_tier}",
    )]
}

/// Chrome may not gain a declared edge, and a file that sheds one lowers its
/// record in the same commit. Refresh with `UPDATE_CHROME_BORDER_BASELINE=1`
/// only after reviewing that every change is a reduction.
#[test]
fn declared_borders_in_chrome_do_not_grow() {
    let ui_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let repository = repository_root();
    let mut files = Vec::new();
    walk(&ui_root.join("src/widget"), &mut files);
    walk(&ui_root.join("src/chrome"), &mut files);
    for extra in ["src/surface.rs", "src/status_bubble.rs"] {
        let path = ui_root.join(extra);
        if path.is_file() {
            files.push(path);
        }
    }
    walk(&repository.join("crates/session/shell/src"), &mut files);
    assert!(
        !files.is_empty(),
        "chrome edge audit found no files to scan"
    );
    files.sort();

    let patterns = declared_border_patterns();
    let found = counts_for(&files, &repository, &patterns);
    let baseline_path = ui_root.join(CHROME_BORDER_BASELINE);
    if std::env::var_os("UPDATE_CHROME_BORDER_BASELINE").is_some_and(|value| value == "1") {
        write_baseline(
            &baseline_path,
            &[
                "Declared border / ramp tokens on chrome surfaces (HIG §9.5).",
                "Format: <path relative to repository root> <pattern> <count>",
                "This is a ratchet, not approval: counts may only move down.",
            ],
            &found,
        );
        return;
    }
    let baseline = read_baseline(&baseline_path);
    let message = |pattern: &str| -> String {
        patterns
            .iter()
            .find(|(key, _, _)| *key == pattern)
            .map(|(_, _, message)| (*message).to_string())
            .unwrap_or_default()
    };
    let (grew, stale) = ratchet_failures(&found, &baseline, &message);
    assert!(
        grew.is_empty(),
        "{} chrome file(s) gained a declared edge.\n{}",
        grew.len(),
        grew.join("\n"),
    );
    assert!(
        stale.is_empty(),
        "{} entr(ies) in {CHROME_BORDER_BASELINE} are higher than the tree; the ratchet only moves down:\n{}",
        stale.len(),
        stale.join("\n"),
    );
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
    // app window.
    {
        let apps = repository_root().join("apps");
        let mut missing = Vec::new();
        for entry in fs::read_dir(&apps)
            .expect("apps directory exists")
            .flatten()
        {
            let app = entry.path();
            if !app.join("tilo-app.toml").is_file() {
                continue;
            }
            let main = app.join("src/main.rs");
            if !main.is_file() || app.file_name().is_some_and(|name| name == "agent") {
                continue;
            }
            let source = fs::read_to_string(&main).expect("app main is readable");
            let launched = source.contains("LaunchSpec")
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
