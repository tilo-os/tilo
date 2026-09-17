//! Spacing and geometry ratchet: the pixel literals the `Theme` should own.
//!
//! `design_token_audit.rs` ratchets colour escape hatches. Nothing did the
//! same for spacing, so `const PAD: f64 = 12.0;` and `length(16.0)` kept
//! entering widget, chrome, shell and app code with no record. This file
//! counts them per file and pins the count: a file may never carry more than
//! `f1_spacing_token_baseline.txt` says, and one that sheds some has to lower
//! its line in the same commit. The number only moves down.
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
use std::fmt::Write as _;
use std::{
    fs,
    path::{Path, PathBuf},
};

/// Recorded counts, relative to the `ui` crate root.
const BASELINE: &str = "tests/f1_spacing_token_baseline.txt";

/// The production trees the ratchet covers, relative to the repository root.
/// `apps/*/src` is expanded at run time so a new app is covered the day it
/// lands.
const ROOTS: &[&str] = &["ui/src/widget", "ui/src/chrome", "shell/src"];

/// Test fixtures are allowed to state pixel values: integration test
/// directories, in-tree test modules split into their own file, and
/// `#[cfg(test)]` blocks (see `test_module_spans`).
fn is_exempt(path: &Path) -> bool {
    let s = path.to_string_lossy();
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    s.contains("/tests/") || name == "tests.rs" || name.ends_with("_tests.rs")
}

fn walk(root: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(read) = fs::read_dir(root) {
        for entry in read.flatten() {
            let p = entry.path();
            if p.is_dir() {
                walk(&p, out);
                continue;
            }
            if p.extension().and_then(|e| e.to_str()) == Some("rs") && !is_exempt(&p) {
                out.push(p);
            }
        }
    }
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

/// Line ranges (half-open, 0-based) covered by a `#[cfg(test)]` item.
///
/// The attribute applies to exactly one item. When that item opens a body
/// (`mod tests {`, `fn helper() {`, `impl Fixture {`, possibly after further
/// attributes or a multi-line signature) the span is the brace-counted body.
/// When it ends with `;` before any brace (`mod tests;`, `const MAX: usize =
/// 120;`, `use a::b;`) only that item is exempt and the production item on
/// the next line is audited — treating every attribute as a braced module
/// would hand the following `impl Widget` a free pass. A brace inside a
/// string literal would still fool the count; the failure mode is a skipped
/// line.
fn test_module_spans(content: &str) -> Vec<(usize, usize)> {
    const ATTRIBUTE: &str = "#[cfg(test)]";
    let lines: Vec<&str> = content.lines().collect();
    let mut spans = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let trimmed = lines[i].trim_start();
        if !trimmed.starts_with(ATTRIBUTE) {
            i = i.saturating_add(1);
            continue;
        }
        // The item starts on the attribute's own line if anything follows the
        // attribute there (`#[cfg(test)] mod tests {`), otherwise on the first
        // following line that is not itself an attribute.
        let mut j = if trimmed[ATTRIBUTE.len()..].trim().is_empty() {
            i.saturating_add(1)
        } else {
            i
        };
        while j < lines.len() && j != i && lines[j].trim_start().starts_with("#[") {
            j = j.saturating_add(1);
        }
        // Walk a multi-line signature to the line that decides the shape.
        while j < lines.len() && !lines[j].contains('{') && !lines[j].contains(';') {
            j = j.saturating_add(1);
        }
        if j == lines.len() {
            spans.push((i, lines.len()));
            break;
        }
        let decisive = if j == i {
            &lines[j].trim_start()[ATTRIBUTE.len()..]
        } else {
            lines[j]
        };
        let opens_body = match (decisive.find('{'), decisive.find(';')) {
            (Some(brace), Some(semi)) => brace < semi,
            (Some(_), None) => true,
            (None, _) => false,
        };
        if !opens_body {
            spans.push((i, j.saturating_add(1)));
            i = j.saturating_add(1);
            continue;
        }
        let mut depth = 0i32;
        let mut k = j;
        'outer: while k < lines.len() {
            let text = if k == i { decisive } else { lines[k] };
            for c in text.chars() {
                match c {
                    '{' => depth = depth.saturating_add(1),
                    '}' => {
                        depth = depth.saturating_sub(1);
                        if depth == 0 {
                            break 'outer;
                        }
                    }
                    _ => {}
                }
            }
            k = k.saturating_add(1);
        }
        spans.push((i, k.saturating_add(1).min(lines.len())));
        i = k.saturating_add(1);
    }
    spans
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

/// Count every pattern per file, skipping comments and `#[cfg(test)]` modules.
fn counts(files: &[PathBuf], repository: &Path) -> BTreeMap<(String, String), usize> {
    let patterns = patterns();
    let mut out: BTreeMap<(String, String), usize> = BTreeMap::new();
    for f in files {
        let content = fs::read_to_string(f).unwrap();
        let skip = test_module_spans(&content);
        let rel = f
            .strip_prefix(repository)
            .unwrap_or(f)
            .to_string_lossy()
            .replace('\\', "/");
        for (i, line) in content.lines().enumerate() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            if skip.iter().any(|(a, b)| i >= *a && i < *b) {
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

fn read_baseline(path: &Path) -> BTreeMap<(String, String), usize> {
    let raw = fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("the ratchet baseline at {} must exist: {e}", path.display()));
    let mut out = BTreeMap::new();
    for (n, line) in raw.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        let line_no = n.saturating_add(1);
        assert_eq!(
            parts.len(),
            3,
            "{}:{}: expected `<path> <pattern> <count>`, got: {line}",
            path.display(),
            line_no
        );
        let count: usize = parts[2]
            .parse()
            .unwrap_or_else(|_| panic!("{}:{}: count must be a number", path.display(), line_no));
        out.insert((parts[0].to_string(), parts[1].to_string()), count);
    }
    out
}

fn write_baseline(path: &Path, counts: &BTreeMap<(String, String), usize>) {
    let mut output = String::from(
        "# Spacing and geometry literals in production code (see f1_spacing_token_audit.rs).\n\
         # Format: <path relative to repository root> <pattern> <count>\n\
         # This is a ratchet, not approval: counts may only move down.\n",
    );
    for ((path, pattern), count) in counts {
        writeln!(&mut output, "{path} {pattern} {count}")
            .expect("writing a baseline line to a String is infallible");
    }
    fs::write(path, output).unwrap_or_else(|e| panic!("could not write {}: {e}", path.display()));
}

fn ratchet_failures(
    found: &BTreeMap<(String, String), usize>,
    baseline: &BTreeMap<(String, String), usize>,
) -> (Vec<String>, Vec<String>) {
    let messages: BTreeMap<&str, &str> =
        patterns().into_iter().map(|p| (p.key, p.message)).collect();
    let mut grew = Vec::new();
    let mut stale = Vec::new();
    for (key, count) in found {
        let allowed = baseline.get(key).copied().unwrap_or(0);
        if *count > allowed {
            grew.push(format!(
                "{} [{}]: {count} occurrence(s), baseline {allowed} — {}",
                key.0,
                key.1,
                messages.get(key.1.as_str()).copied().unwrap_or_default(),
            ));
        }
    }
    for (key, allowed) in baseline {
        let count = found.get(key).copied().unwrap_or(0);
        if count < *allowed {
            stale.push(format!(
                "{} [{}]: now {count}, baseline still {allowed} — lower it to {count}",
                key.0, key.1,
            ));
        }
    }
    (grew, stale)
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("ui crate lives in the repository")
        .to_path_buf()
}

/// The scanner must be able to see what it polices. Each rule is exercised
/// on a line that must count and one that must not, so a regex edit that
/// silently stops matching fails here instead of passing an empty audit.
#[test]
fn spacing_patterns_match_the_shapes_they_claim() {
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

/// `#[cfg(test)]` items are fixtures, not draw paths: the span finder must
/// cover exactly the item the attribute applies to, so a real constant right
/// after it is seen.
#[test]
fn test_module_spans_cover_only_the_test_module() {
    let source = "const REAL: f64 = 12.0;\n#[cfg(test)]\nmod tests {\n    const FIXTURE: f64 = 99.0;\n}\nconst AFTER: f64 = 8.0;\n";
    let spans = test_module_spans(source);
    assert_eq!(spans, vec![(1, 5)]);
    let in_span = |line: usize| spans.iter().any(|(a, b)| line >= *a && line < *b);
    assert!(!in_span(0), "the constant before the module is audited");
    assert!(in_span(3), "the fixture inside the module is skipped");
    assert!(!in_span(5), "the constant after the module is audited");
}

/// A `#[cfg(test)]` on a one-line item (`mod tests;`, a test-only `const`)
/// exempts that line only; the production item that follows stays audited.
#[test]
fn test_module_spans_do_not_exempt_the_item_after_a_bodyless_test_item() {
    let source = "#[cfg(test)]\nconst MAX_HISTORY: usize = 120;\nimpl Widget for Terminal {\n    const PAD: f64 = 12.0;\n}\n#[cfg(test)]\nmod tests;\nconst AFTER: f64 = 8.0;\n";
    let spans = test_module_spans(source);
    assert_eq!(spans, vec![(0, 2), (5, 7)]);
    let in_span = |line: usize| spans.iter().any(|(a, b)| line >= *a && line < *b);
    assert!(in_span(1), "the test-only const is exempt");
    assert!(
        !in_span(3),
        "the impl after it is production code and is audited"
    );
    assert!(in_span(6), "`mod tests;` is exempt");
    assert!(!in_span(7), "the constant after `mod tests;` is audited");
}

/// Further attributes, a multi-line signature and a same-line module all
/// resolve to the body the attribute governs.
#[test]
fn test_module_spans_follow_attributes_signatures_and_same_line_modules() {
    let source = "#[cfg(test)]\n#[allow(dead_code)]\nmod tests {\n    const F: f64 = 1.0;\n}\nconst A: f64 = 8.0;\n#[cfg(test)]\npub(crate) fn helper(\n    px: f32,\n) -> f32 {\n    px\n}\nconst B: f64 = 9.0;\n#[cfg(test)] mod inline { const G: f64 = 2.0; }\nconst C: f64 = 10.0;\n";
    let spans = test_module_spans(source);
    assert_eq!(spans, vec![(0, 5), (6, 12), (13, 14)]);
    let in_span = |line: usize| spans.iter().any(|(a, b)| line >= *a && line < *b);
    assert!(in_span(3) && !in_span(5));
    assert!(in_span(10) && !in_span(12));
    assert!(in_span(13) && !in_span(14));
}

/// Spacing literals in widget, chrome, shell and app production code may
/// not grow, and a file that has shed some has to record it.
#[test]
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
        write_baseline(&baseline_path, &found);
        return;
    }
    let baseline = read_baseline(&baseline_path);
    assert!(
        !baseline.is_empty(),
        "{BASELINE} is empty; a ratchet with nothing recorded pins nothing"
    );
    let (grew, stale) = ratchet_failures(&found, &baseline);

    assert!(
        grew.is_empty(),
        "{} file(s) gained a spacing literal the Theme should own.\n\
         Read the value from theme.tokens.spacing or theme.tokens.metrics (insets_all / \
         insets_xy build a padding Rect from one). If it genuinely cannot be a token, say \
         why in a comment on the line and raise its count in {BASELINE}.\n{}",
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
