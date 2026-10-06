//! Plumbing shared by the two literal ratchets (`design_token_audit.rs`,
//! `spacing_token_audit.rs`): the production-file walk, the
//! `#[cfg(test)]` span finder, the `<path> <pattern> <count>` baseline file
//! and the grew/stale comparison. One copy, so a fix to what the scanners
//! can see reaches both audits at once.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

/// `(path relative to the audited root, pattern key) → count`.
pub type Counts = BTreeMap<(String, String), usize>;

/// Every `.rs` file under `root`, minus those `is_exempt` names.
pub fn walk(root: &Path, is_exempt: &dyn Fn(&Path) -> bool, out: &mut Vec<PathBuf>) {
    if let Ok(read) = fs::read_dir(root) {
        for entry in read.flatten() {
            let p = entry.path();
            if p.is_dir() {
                walk(&p, is_exempt, out);
                continue;
            }
            if p.extension().and_then(|e| e.to_str()) == Some("rs") && !is_exempt(&p) {
                out.push(p);
            }
        }
    }
}

/// Line ranges (half-open, 0-based) covered by a `#[cfg(test)]` item, or the
/// whole file for a `#![cfg(test)]` crate attribute.
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
pub fn test_module_spans(content: &str) -> Vec<(usize, usize)> {
    const ATTRIBUTE: &str = "#[cfg(test)]";
    let lines: Vec<&str> = content.lines().collect();
    if lines.iter().any(|line| line.trim() == "#![cfg(test)]") {
        return vec![(0, lines.len())];
    }
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

/// Whether line `index` (0-based) is a comment line or inside a test span —
/// the two things both ratchets skip.
pub fn is_skipped(line: &str, index: usize, spans: &[(usize, usize)]) -> bool {
    line.trim_start().starts_with("//") || spans.iter().any(|(a, b)| index >= *a && index < *b)
}

/// `<path> <pattern> <count>` lines; `#` comments and blank lines ignored.
pub fn read_baseline(path: &Path) -> Counts {
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

/// Write `counts` under `header` (each header line is prefixed with `# `).
pub fn write_baseline(path: &Path, header: &[&str], counts: &Counts) {
    let mut output = String::new();
    for line in header {
        writeln!(&mut output, "# {line}")
            .expect("writing a baseline line to a String is infallible");
    }
    for ((path, pattern), count) in counts {
        writeln!(&mut output, "{path} {pattern} {count}")
            .expect("writing a baseline line to a String is infallible");
    }
    fs::write(path, output).unwrap_or_else(|e| panic!("could not write {}: {e}", path.display()));
}

/// `(grew, stale)`: files whose count exceeds the baseline (with
/// `message_for(pattern)` appended), and baseline entries higher than the
/// tree. Growth fails; a stale entry is progress and only noted
/// ([`note_stale`]).
pub fn ratchet_failures(
    found: &Counts,
    baseline: &Counts,
    message_for: &dyn Fn(&str) -> String,
) -> (Vec<String>, Vec<String>) {
    let mut grew = Vec::new();
    let mut stale = Vec::new();
    for (key, count) in found {
        let allowed = baseline.get(key).copied().unwrap_or(0);
        if *count > allowed {
            grew.push(format!(
                "{} [{}]: {count} occurrence(s), baseline {allowed} — {}",
                key.0,
                key.1,
                message_for(&key.1),
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

/// The span finder is what both audits' coverage rests on, so its shapes
/// are pinned here once: exactly the item the attribute governs, whether a
/// braced module, a bodyless item, a multi-line signature, a same-line
/// module or a whole test-gated file.

#[test]
fn test_module_spans_cover_exactly_the_test_code() {
    // Phase `test_module_spans_cover_only_the_test_module`.
    {
        let source = "const REAL: f64 = 12.0;\n#[cfg(test)]\nmod tests {\n    const FIXTURE: f64 = 99.0;\n}\nconst AFTER: f64 = 8.0;\n";
        let spans = test_module_spans(source);
        assert_eq!(spans, vec![(1, 5)]);
        let in_span = |line: usize| spans.iter().any(|(a, b)| line >= *a && line < *b);
        assert!(!in_span(0), "the constant before the module is audited");
        assert!(in_span(3), "the fixture inside the module is skipped");
        assert!(!in_span(5), "the constant after the module is audited");
    }
    // Phase `test_module_spans_do_not_exempt_the_item_after_a_bodyless_test_item`.
    {
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
    // Phase `test_module_spans_follow_attributes_signatures_and_same_line_modules`.
    {
        let source = "#[cfg(test)]\n#[allow(dead_code)]\nmod tests {\n    const F: f64 = 1.0;\n}\nconst A: f64 = 8.0;\n#[cfg(test)]\npub(crate) fn helper(\n    px: f32,\n) -> f32 {\n    px\n}\nconst B: f64 = 9.0;\n#[cfg(test)] mod inline { const G: f64 = 2.0; }\nconst C: f64 = 10.0;\n";
        let spans = test_module_spans(source);
        assert_eq!(spans, vec![(0, 5), (6, 12), (13, 14)]);
        let in_span = |line: usize| spans.iter().any(|(a, b)| line >= *a && line < *b);
        assert!(in_span(3) && !in_span(5));
        assert!(in_span(10) && !in_span(12));
        assert!(in_span(13) && !in_span(14));
    }
    // Phase `a_file_gated_to_tests_has_no_production_lines`.
    {
        let source = "#![cfg(test)]\nuse super::*;\nfn fixture() { let ink = Color::WHITE; }\n";
        assert_eq!(test_module_spans(source), vec![(0, 3)]);
        assert!(test_module_spans("fn draw() { let ink = Color::WHITE; }\n").is_empty());
    }
}

#[test]
fn ratchet_reports_growth_and_stale_entries_separately() {
    let key = |p: &str, k: &str| (p.to_string(), k.to_string());
    let found: Counts = [(key("a.rs", "x"), 3), (key("b.rs", "x"), 1)]
        .into_iter()
        .collect();
    let baseline: Counts = [(key("a.rs", "x"), 2), (key("c.rs", "x"), 1)]
        .into_iter()
        .collect();
    let (grew, stale) = ratchet_failures(&found, &baseline, &|k| format!("fix {k}"));
    assert_eq!(grew.len(), 2, "a.rs grew and b.rs is new");
    assert!(grew[0].starts_with("a.rs [x]: 3 occurrence(s), baseline 2 — fix x"));
    assert!(grew[1].starts_with("b.rs [x]: 1 occurrence(s), baseline 0"));
    assert_eq!(
        stale,
        vec!["c.rs [x]: now 0, baseline still 1 — lower it to 0"]
    );
}

/// Reports baseline entries the tree has dropped below. An improvement never
/// fails a change; the record is lowered by regenerating it with
/// `update_var=1`, in that change or a later one.
pub fn note_stale(baseline: &str, update_var: &str, stale: &[String]) {
    if stale.is_empty() {
        return;
    }
    let note = format!(
        "{} entr(ies) in {baseline} are above the tree; lower them (or regenerate with \
         {update_var}=1) to lock the progress in:\n{}\n",
        stale.len(),
        stale.join("\n"),
    );
    eprintln!("notice: {note}");
    if let Some(summary) = std::env::var_os("GITHUB_STEP_SUMMARY") {
        use std::io::Write as _;
        if let Ok(mut file) = fs::OpenOptions::new().append(true).open(summary) {
            let _ = writeln!(file, "### Ratchet progress\n\n```\n{note}```");
        }
    }
}
