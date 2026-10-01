//! Spacing and geometry ratchet: the pixel literals the `Theme` should own.
//!
//! `design_token_audit.rs` ratchets colour escape hatches. Nothing did the
//! same for spacing, so `const PAD: f64 = 12.0;` and `length(16.0)` kept
//! entering widget, chrome, shell and app code with no record. This file
//! counts them per file and pins the count: a file may never carry more than
//! `spacing_token_baseline.txt` says; one that sheds some is noted so its
//! line can be lowered. The number only moves down.
//!
//! Two shapes are counted, both outside comments and `#[cfg(test)]` modules:
//!
//! * `pixel_const` — a module-level `const NAME: f32|f64 = <whole number ≥ 1>;`.
//!   Whole numbers because pixel constants are whole; `0.1`, `0.9` and
//!   `1.25` are rates, fractions and ratios and are not counted. Arithmetic
//!   (`PAD + CTRL_H`, `x / 2.0`) is not a literal and is not counted.
//! * `taffy_length` — a non-zero literal handed to taffy's `length(...)`
//!   inside a `Style` (`padding`, `gap`, `margin`, `size`). `length(0.0)`
//!   is a deliberate "none" and is not counted; `length(theme.gap_standard)`
//!   is what the ratchet wants and is not a literal.
//!
//! Regenerate with `UPDATE_F1_SPACING_BASELINE=1` only after reading the diff;
//! CI never sets it. A new hit is a request to read the value from
//! `theme.tokens.spacing` / `theme.tokens.metrics`, or to say in a comment on
//! the line why it genuinely cannot be a token.

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
    Counts, is_skipped, note_stale, ratchet_failures, read_baseline, test_module_spans,
    write_baseline,
};

/// Recorded counts, relative to the `ui` crate root.
const BASELINE: &str = "tests/spacing_token_baseline.txt";

const BASELINE_HEADER: &[&str] = &[
    "Spacing and geometry literals in production code (see spacing_token_audit.rs).",
    "Format: <path relative to repository root> <pattern> <count>",
    "This is a ratchet, not approval: counts may only move down.",
];

/// The production trees the ratchet covers, relative to the repository root.
/// `apps/*/src` is expanded at run time so a new app is covered the day it
/// lands.
const ROOTS: &[&str] = &[
    "crates/sdk/ui-core/src/widget",
    "crates/sdk/ui-core/src/chrome",
    "crates/session/shell/src",
];

/// Test fixtures are allowed to state pixel values: integration test
/// directories, in-tree test modules split into their own file, and
/// `#[cfg(test)]` blocks (see `test_module_spans`).
fn is_exempt(path: &Path) -> bool {
    let s = path.to_string_lossy();
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    s.contains("/tests/") || name == "tests.rs" || name.ends_with("_tests.rs")
}

fn walk(root: &Path, out: &mut Vec<PathBuf>) {
    crate::support::ratchet::walk(root, &is_exempt, out);
}

fn production_files(repository: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for root in ROOTS {
        walk(&repository.join(root), &mut files);
    }
    if let Ok(apps) = fs::read_dir(repository.join("apps")) {
        for app in apps.flatten() {
            let src = app.path().join("src");
            if src.is_dir() {
                walk(&src, &mut files);
            }
        }
    }
    files.sort();
    files
}

/// `"12"`, `"12.0"`, `"12.00"` → a whole pixel count of at least one.
/// `"0"`, `"0.0"`, `"0.5"`, `"1.25"` → not a pixel constant.
fn is_whole_pixel_literal(literal: &str) -> bool {
    let (int, frac) = literal.split_once('.').unwrap_or((literal, ""));
    int.parse::<u64>().is_ok_and(|v| v >= 1) && frac.chars().all(|c| c == '0')
}

/// `"0"`, `"0.0"`, `"0.00"` → the deliberate zero; anything else is a value.
fn is_nonzero_literal(literal: &str) -> bool {
    literal.chars().any(|c| c.is_ascii_digit() && c != '0')
}

struct Pattern {
    key: &'static str,
    regex: Regex,
    counts: fn(&str) -> bool,
    message: &'static str,
}

fn patterns() -> Vec<Pattern> {
    vec![
        Pattern {
            key: "pixel_const",
            regex: Regex::new(
                r"^\s*(?:pub(?:\([a-z]+\))?\s+)?const\s+[A-Z][A-Z0-9_]*\s*:\s*f(?:32|64)\s*=\s*(\d+(?:\.\d+)?)(?:_f(?:32|64))?\s*;",
            )
            .unwrap(),
            counts: is_whole_pixel_literal,
            message: "module-level pixel constant — read it from theme.tokens.spacing / theme.tokens.metrics",
        },
        Pattern {
            key: "taffy_length",
            regex: Regex::new(r"\blength\(\s*(\d+(?:\.\d+)?)(?:_f(?:32|64))?\s*\)").unwrap(),
            counts: is_nonzero_literal,
            message: "literal taffy length in a Style — pass a spacing token (insets_all / insets_xy take one)",
        },
    ]
}

/// Count every pattern per file, skipping comments and `#[cfg(test)]` items
/// (`support::ratchet::test_module_spans`, shared with the colour audit).
fn counts(files: &[PathBuf], repository: &Path) -> Counts {
    let patterns = patterns();
    let mut out: Counts = BTreeMap::new();
    for f in files {
        let content = fs::read_to_string(f).unwrap();
        let skip = test_module_spans(&content);
        let rel = f
            .strip_prefix(repository)
            .unwrap_or(f)
            .to_string_lossy()
            .replace('\\', "/");
        for (i, line) in content.lines().enumerate() {
            if is_skipped(line, i, &skip) {
                continue;
            }
            for pattern in &patterns {
                let hits = pattern
                    .regex
                    .captures_iter(line)
                    .filter(|c| (pattern.counts)(&c[1]))
                    .count();
                if hits > 0 {
                    let entry = out
                        .entry((rel.clone(), pattern.key.to_string()))
                        .or_insert(0);
                    *entry = entry.saturating_add(hits);
                }
            }
        }
    }
    out
}

/// What a new hit of `pattern` is told to do.
fn ratchet_message(pattern: &str) -> String {
    patterns()
        .into_iter()
        .find(|p| p.key == pattern)
        .map(|p| p.message.to_string())
        .unwrap_or_default()
}

fn repository_root() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("UI crate lives under crates/sdk in the repository");
    assert!(
        root.join("Cargo.toml").is_file() && root.join("apps").is_dir(),
        "spacing audit repository anchor must contain Cargo.toml and apps"
    );
    root.to_path_buf()
}

#[allow(
    clippy::too_many_lines,
    reason = "merged scenario keeps its original cases together; each case builds a fresh fixture"
)]
#[test]
fn spacing_audit_sees_what_it_polices_and_literals_do_not_grow() {
    // Case `spacing_patterns_match_the_shapes_they_claim`.
    // The scanner must be able to see what it polices. Each rule is exercised
    // on a line that must count and one that must not, so a regex edit that
    // silently stops matching fails here instead of passing an empty audit.
    {
        let patterns = patterns();
        let pixel = &patterns[0];
        let length = &patterns[1];
        let count = |p: &Pattern, line: &str| {
            p.regex
                .captures_iter(line)
                .filter(|c| (p.counts)(&c[1]))
                .count()
        };

        assert_eq!(count(pixel, "const PAD: f64 = 12.0;"), 1);
        assert_eq!(
            count(pixel, "pub const DEFAULT_SHELL_BREAKPOINT: f64 = 720.0;"),
            1
        );
        assert_eq!(count(pixel, "pub(crate) const BAR: f32 = 8.0_f32;"), 1);
        assert_eq!(count(pixel, "    const CHIP_SIZE: f64 = 36.0;"), 1);
        assert_eq!(
            count(pixel, "const MAX_FRAME_DT: f64 = 0.1;"),
            0,
            "a rate is not a pixel"
        );
        assert_eq!(
            count(pixel, "const OVERDRAW: f64 = 1.25;"),
            0,
            "a ratio is not a pixel"
        );
        assert_eq!(
            count(pixel, "const NONE: f32 = 0.0;"),
            0,
            "zero is the deliberate absence"
        );
        assert_eq!(
            count(pixel, "const BAND: f64 = PAD + CTRL_H + PAD;"),
            0,
            "arithmetic is not a literal"
        );
        assert_eq!(count(pixel, "const LIMIT: usize = 8;"), 0, "not a float");
        assert_eq!(
            count(pixel, "let pad: f32 = 12.0;"),
            0,
            "a local is not a module constant"
        );

        assert_eq!(count(length, "left: length(12.0),"), 1);
        assert_eq!(
            count(length, "width: length(8.0_f32), height: length(8.0_f32),"),
            2
        );
        assert_eq!(count(length, "size: length(320.0)"), 1);
        assert_eq!(
            count(length, "length(0.5)"),
            1,
            "a half-pixel hairline is still a literal"
        );
        assert_eq!(
            count(length, "left: length(0.0_f32),"),
            0,
            "zero is the deliberate absence"
        );
        assert_eq!(
            count(length, "length(theme.gap_standard)"),
            0,
            "a token is the goal"
        );
        assert_eq!(count(length, "length(pad)"), 0);
        assert_eq!(
            count(length, "length(w / 2.0)"),
            0,
            "arithmetic is not a literal"
        );
        assert_eq!(count(length, "Dimension::length(24.0)"), 1);
    }

    // Case `spacing_literals_do_not_grow`.
    // The `#[cfg(test)]` span finder's shapes are pinned once in
    // `support::ratchet` (both audits scan through the same finder).
    //
    // Spacing literals in widget, chrome, shell and app production code may
    // not grow, and a file that has shed some has to record it.
    {
        fn spacing_literals_do_not_grow() {
            let repository = repository_root();
            let files = production_files(&repository);
            assert!(!files.is_empty(), "spacing audit found no files to scan");
            for root in ROOTS.iter().copied().chain(["apps/"]) {
                assert!(
                    files.iter().any(|f| f
                        .strip_prefix(&repository)
                        .is_ok_and(|r| r.starts_with(root))),
                    "spacing audit did not reach {root}; the scan cannot observe its subject"
                );
            }

            let found = counts(&files, &repository);
            let baseline_path = Path::new(env!("CARGO_MANIFEST_DIR")).join(BASELINE);
            if std::env::var_os("UPDATE_F1_SPACING_BASELINE").is_some_and(|value| value == "1") {
                write_baseline(&baseline_path, BASELINE_HEADER, &found);
                return;
            }
            let baseline = read_baseline(&baseline_path);
            assert!(
                !baseline.is_empty(),
                "{BASELINE} is empty; a ratchet with nothing recorded pins nothing"
            );
            let (grew, stale) = ratchet_failures(&found, &baseline, &ratchet_message);

            assert!(
                grew.is_empty(),
                "{} file(s) gained a spacing literal the Theme should own.\n\
         Read the value from theme.tokens.spacing or theme.tokens.metrics (insets_all / \
         insets_xy build a padding Rect from one). If it genuinely cannot be a token, say \
         why in a comment on the line and raise its count in {BASELINE}.\n{}",
                grew.len(),
                grew.join("\n"),
            );
            note_stale(BASELINE, "UPDATE_F1_SPACING_BASELINE", &stale);
        }
        spacing_literals_do_not_grow();
    }
}
