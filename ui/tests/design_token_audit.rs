//! Mechanical audit that fails if a hard-coded styling literal sneaks into
//! any widget or shell source file. Every literal must flow through
//! `theme.*` so the OS can enforce one design language.

use std::{fs, path::{Path, PathBuf}};
use regex::Regex;

/// Files exempt from the audit:
///  - theme.rs (the single source of truth for literal color values)
///  - tokens/** (palette sub-modules — colors.rs, elevation.rs, terminal_palette.rs
///    declare the literal values that theme.rs composes; they are the token
///    origin, not token consumers)
///  - presence/render.rs (the HSL helper intentionally encodes per-user hue math)
///  - tests/**                                                            (test fixtures)
fn is_exempt(path: &Path) -> bool {
    let s = path.to_string_lossy();
    s.contains("/theme.rs")
        || s.contains("/tokens/")
        || s.contains("/presence/render.rs")
        || s.contains("/tests/")
}

fn walk(root: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(read) = fs::read_dir(root) {
        for entry in read.flatten() {
            let p = entry.path();
            if p.is_dir() { walk(&p, out); continue; }
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
        (Regex::new(r"Color::rgb8?\s*\(\s*\d").unwrap(),
         "hard-coded Color::rgb8/rgb — use theme.* tokens"),
        // Hard-coded radius literals on rounded rects
        (Regex::new(r"\.to_rounded_rect\(\s*\d+(\.\d+)?\s*\)").unwrap(),
         "hard-coded rounded-rect radius — use theme.radius_standard"),
        // Hard-coded shadow blur literals
        (Regex::new(r"blur\s*:\s*\d").unwrap(),
         "hard-coded shadow blur — use theme.elevation.*"),
        // Hard-coded spacing like Padding::all(12.0) (allow zero)
        (Regex::new(r"Padding::all\(\s*[1-9]\d*(\.\d+)?\s*\)").unwrap(),
         "hard-coded padding — use theme.padding_standard"),
    ]
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
        for (i, line) in content.lines().enumerate() {
            // Ignore comment lines.
            if line.trim_start().starts_with("//") { continue; }
            for (re, msg) in &patterns {
                if re.is_match(line) {
                    violations.push(format!(
                        "{}:{}: {} — line: {}",
                        f.display(), i + 1, msg, line.trim()
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
