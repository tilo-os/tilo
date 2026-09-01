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
use std::fmt::Write as _;
use std::{
    fs,
    path::{Path, PathBuf},
};

/// Files exempt from the audit:
///  - theme.rs (the single source of truth for literal color values)
///  - tokens/** (palette sub-modules — the token origin, not token consumers)
///  - presence/render.rs (the HSL helper intentionally encodes per-user hue math)
///  - tests/** and `#[cfg(test)]` modules inside a src file (test fixtures —
///    see `test_module_spans`)
fn is_exempt(path: &Path) -> bool {
    let s = path.to_string_lossy();
    s.contains("/theme.rs")
        || s.contains("/tokens/")
        || s.contains("/presence/render.rs")
        || s.contains("/tests/")
        || path.file_name().is_some_and(|name| name == "tests.rs")
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

fn forbidden_patterns() -> Vec<(Regex, &'static str)> {
    vec![
        // Hard-coded RGB literal palette values. Require a digit as the first
        // argument so variable-arg calls like `Color::rgb(c.color[0] as f64, …)`
        // that route real collaborator data through the constructor don't get
        // flagged — only *literal* palette values do.
        (
            Regex::new(r"Color::rgb8?\s*\(\s*\d").unwrap(),
            "hard-coded Color::rgb8/rgb — use theme.* tokens",
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
        // Hard-coded spacing like Padding::all(12.0) (allow zero)
        (
            Regex::new(r"Padding::all\(\s*[1-9]\d*(\.\d+)?\s*\)").unwrap(),
            "hard-coded padding — use theme.padding_standard",
        ),
    ]
}

/// Line ranges (half-open, 0-based) covered by a `#[cfg(test)]` module.
///
/// Found by brace-counting from the `mod ... {` that follows the
/// attribute, so a test module anywhere in the file is handled, not just
/// one conventionally parked at the bottom. Braces inside string or char
/// literals would fool this; no test module in the tree contains one, and
/// the failure mode is a skipped line rather than a false accusation.
fn test_module_spans(content: &str) -> Vec<(usize, usize)> {
    let lines: Vec<&str> = content.lines().collect();
    let mut spans = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim_start().starts_with("#[cfg(test)]") {
            // Find the opening brace of the module body.
            let mut j = i;
            while j < lines.len() && !lines[j].contains('{') {
                j = j.saturating_add(1);
            }
            if j == lines.len() {
                break;
            }
            let mut depth = 0i32;
            let start = i;
            let mut k = j;
            'outer: while k < lines.len() {
                for c in lines[k].chars() {
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
            spans.push((start, k.saturating_add(1).min(lines.len())));
            i = k.saturating_add(1);
            continue;
        }
        i = i.saturating_add(1);
    }
    spans
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
            if line.trim_start().starts_with("//") {
                continue;
            }
            // The audit polices *draw paths*; a `#[cfg(test)]` module is a
            // fixture living in the same file as the code it covers, and a
            // test that pins how a token is painted has to name its values.
            if skip.iter().any(|(a, b)| i >= *a && i < *b) {
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
fn ratcheted_counts(files: &[PathBuf], root: &Path) -> BTreeMap<(String, String), usize> {
    let patterns = ratcheted_patterns();
    let mut counts: BTreeMap<(String, String), usize> = BTreeMap::new();
    for f in files {
        let content = fs::read_to_string(f).unwrap();
        let skip = test_module_spans(&content);
        let rel = f
            .strip_prefix(root)
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
            for (key, re, _) in &patterns {
                if re.is_match(line) {
                    let hit = counts.entry((rel.clone(), (*key).to_string())).or_insert(0);
                    *hit = hit.saturating_add(1);
                }
            }
        }
    }
    counts
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
        "# Theme escape hatches in app and shell production code.\n\
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
    let messages: BTreeMap<&str, &str> = ratcheted_patterns()
        .into_iter()
        .map(|(key, _, message)| (key, message))
        .collect();
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
    let baseline = read_baseline(&root.join(BASELINE));
    let (grew, stale) = ratchet_failures(&found, &baseline);

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
    let repository = ui_root.parent().expect("ui crate lives in the repository");
    let mut files = Vec::new();
    walk(&repository.join("apps"), &mut files);
    walk(&repository.join("shell/src"), &mut files);
    assert!(
        !files.is_empty(),
        "cross-product audit found no files to scan"
    );
    files.sort();

    let found = ratcheted_counts(&files, repository);
    let baseline_path = ui_root.join(APP_SHELL_BASELINE);
    if std::env::var_os("UPDATE_APP_SHELL_DESIGN_BASELINE").is_some_and(|value| value == "1") {
        write_baseline(&baseline_path, &found);
        return;
    }
    let baseline = read_baseline(&baseline_path);
    let (grew, stale) = ratchet_failures(&found, &baseline);

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
