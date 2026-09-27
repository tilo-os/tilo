//! Keeps English prose out of the shell's copy builders.
//!
//! Every user-visible string comes from the Fluent catalogue. This scan finds
//! string literals handed straight to the builders that put text on screen
//! (notifications, access names, labels, tooltips, painted text) or to the
//! struct fields that carry it, in non-test code, and fails on any that reads
//! as English. Ids, brand names and other values that are not prose go in
//! [`ALLOWED`], with the file they live in.
#![allow(
    clippy::arithmetic_side_effects,
    reason = "offsets into a source file that was just read, far below usize::MAX"
)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Calls whose first string argument (after an optional leading
/// identifier, as in `draw_text(scene, "…")`) is shown to people.
const BUILDER_CALLS: &[&str] = &[
    "Notification::new(",
    ".with_name(",
    ".with_label(",
    ".with_tooltip(",
    ".with_description(",
    ".with_title(",
    ".with_body(",
    ".with_placeholder(",
    ".with_hint(",
    ".with_subtitle(",
    "push_error_notification(",
    "push_notification(",
    ".draw_text(",
];

/// Struct fields that carry copy.
const COPY_FIELDS: &[&str] = &[
    "title",
    "label",
    "subtitle",
    "tooltip",
    "description",
    "placeholder",
    "body",
    "message",
    "hint",
    "summary",
    "detail",
    "caption",
];

/// Literals that look like prose but are not copy: ids, brand names,
/// protocol values. Each entry is `(path under src/, literal)`.
const ALLOWED: &[(&str, &str)] = &[
    // Default app, workspace and tab names are written to the replicated
    // desktop tree, then shown as the user's own.
    ("main.rs", "Files"),
    ("shell/state.rs", "Available on another device"),
    ("shell/state.rs", "Document"),
    ("shell/state.rs", "Files"),
    ("shell/state.rs", "Notes"),
    ("shell/state.rs", "Terminal"),
    ("shell/tab_binding.rs", "While you were away"),
    // Fixed app names.
    ("shell/editor_app.rs", "Editor"),
    ("shell/notes_app.rs", "Note"),
    // wlr-output-management head description; display tools match on it.
    ("compositor/output_heads.rs", "Virtual display"),
    // The developer window backend's title.
    ("compositor/backend/winit.rs", "tilo compositor blit"),
    // Error payloads returned to apps and callers, not painted.
    (
        "shell/launcher.rs",
        "the command does not fit a terminal launch",
    ),
    ("shell/tool_intents.rs", "invalid tool request"),
    ("shell/tool_intents.rs", "tool request is too large"),
    ("shell/tool_intents.rs", "unsupported tool protocol"),
];

/// A literal that reads as English words rather than an id or key.
fn reads_as_english(literal: &str) -> bool {
    let bytes = literal.as_bytes();
    let has_word = bytes
        .windows(2)
        .any(|pair| pair[0].is_ascii_alphabetic() && pair[1].is_ascii_alphabetic());
    if !has_word {
        return false;
    }
    let spaced_words = bytes.windows(4).any(|window| {
        window[0].is_ascii_alphabetic()
            && window[1].is_ascii_alphabetic()
            && window[2] == b' '
            && window[3].is_ascii_alphabetic()
    });
    let capitalised = bytes.first().is_some_and(u8::is_ascii_uppercase)
        && bytes.get(1).is_some_and(u8::is_ascii_lowercase);
    spaced_words || capitalised
}

/// `source` with every `#[cfg(test)]` item blanked out, line count kept.
fn without_test_items(source: &str) -> String {
    let mut kept = String::with_capacity(source.len());
    let mut rest = source;
    while let Some(start) = rest.find("#[cfg(test)]") {
        kept.push_str(&rest[..start]);
        let item = &rest[start..];
        let open = item.find('{');
        let semi = item.find(';');
        let end = match (open, semi) {
            (_, Some(semi)) if open.is_none_or(|open| semi < open) => semi + 1,
            (Some(open), _) => {
                let mut depth = 0_usize;
                let mut end = item.len();
                for (offset, byte) in item.bytes().enumerate().skip(open) {
                    match byte {
                        b'{' => depth += 1,
                        b'}' => {
                            depth = depth.saturating_sub(1);
                            if depth == 0 {
                                end = offset + 1;
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                end
            }
            _ => item.len(),
        };
        kept.extend(item[..end].chars().filter(|c| *c == '\n'));
        rest = &item[end..];
    }
    kept.push_str(rest);
    kept
}

/// The string literal starting at `text` (just past its opening quote).
fn literal_body(text: &str) -> Option<&str> {
    let mut escaped = false;
    for (offset, c) in text.char_indices() {
        match c {
            '\\' if !escaped => escaped = true,
            '"' if !escaped => return Some(&text[..offset]),
            _ => escaped = false,
        }
    }
    None
}

/// Every copy literal in `source`, with its 1-based line.
fn copy_literals(source: &str) -> Vec<(usize, String)> {
    let line_of = |offset: usize| source[..offset].matches('\n').count() + 1;
    let mut found = Vec::new();
    for call in BUILDER_CALLS {
        for (start, _) in source.match_indices(call) {
            let mut rest = source[start + call.len()..].trim_start();
            // A leading plain identifier argument (`scene,`).
            let ident_len = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(0);
            if ident_len > 0 && rest[ident_len..].trim_start().starts_with(',') {
                rest = rest[ident_len..].trim_start()[1..].trim_start();
            }
            let rest = rest.strip_prefix('&').unwrap_or(rest);
            if let Some(body) = rest.strip_prefix('"').and_then(literal_body) {
                found.push((line_of(start), body.to_string()));
            }
        }
    }
    for field in COPY_FIELDS {
        let pattern = format!("{field}:");
        for (start, _) in source.match_indices(&pattern) {
            // A field starts a line or follows `{`, `(` or `,`; anything else
            // (an identifier, a path, the inside of a literal) is not one.
            let starts_field = source[..start]
                .chars()
                .next_back()
                .is_none_or(|c| c.is_whitespace() || matches!(c, '{' | '(' | ','));
            if !starts_field {
                continue;
            }
            let rest = source[start + pattern.len()..].trim_start();
            let rest = rest.strip_prefix("Some(").unwrap_or(rest);
            if let Some(body) = rest.strip_prefix('"').and_then(literal_body) {
                found.push((line_of(start), body.to_string()));
            }
        }
    }
    found
}

fn shell_sources(dir: &Path, found: &mut Vec<PathBuf>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .expect("read source directory")
        .map(|entry| entry.expect("source entry").path())
        .collect();
    entries.sort();
    for path in entries {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if path.is_dir() {
            // Developer binaries and test trees are not desktop copy.
            if name != "bin" && name != "tests" {
                shell_sources(&path, found);
            }
        } else if path.extension().is_some_and(|ext| ext == "rs")
            && !name.ends_with("_tests.rs")
            && name != "tests.rs"
            && name != "test_support.rs"
            // The test control socket speaks to test drivers, not people.
            && !name.contains("test_control")
        {
            found.push(path);
        }
    }
}

#[test]
fn copy_builders_take_catalogue_strings_not_english_literals() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    shell_sources(&root, &mut files);
    assert!(
        files.len() > 100,
        "the source walk found {} files",
        files.len()
    );
    let allowed: BTreeSet<(&str, &str)> = ALLOWED.iter().copied().collect();
    let mut used_allowances = BTreeSet::new();
    let mut offenders = Vec::new();
    for path in files {
        let relative = path
            .strip_prefix(&root)
            .expect("under src")
            .to_string_lossy()
            .replace('\\', "/");
        let source = std::fs::read_to_string(&path).expect("read source file");
        for (line, literal) in copy_literals(&without_test_items(&source)) {
            if !reads_as_english(&literal) {
                continue;
            }
            let allowance = ALLOWED
                .iter()
                .find(|(file, allowed)| *file == relative && *allowed == literal);
            if let Some(allowance) = allowance {
                used_allowances.insert(*allowance);
            } else {
                offenders.push(format!("{relative}:{line}: \"{literal}\""));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "English copy bypasses the catalogue; move it to locales/*.ftl and resolve it with \
         `t`/`t_args`, or add an id that only looks like prose to ALLOWED:\n{}",
        offenders.join("\n")
    );
    let stale: Vec<_> = allowed.difference(&used_allowances).collect();
    assert!(
        stale.is_empty(),
        "ALLOWED lists literals that no longer exist: {stale:?}"
    );
}

#[test]
fn copy_scan_sees_builders_fields_and_skips_ids_and_tests() {
    let source = r#"
        let n = Notification::new("Saved your file", body);
        text.draw_text(scene, &"Hello there", size);
        let row = Row { title: "Open settings".into(), id: "open-settings" };
        let key = Row { label: t("search-kind-files"), hint: Some("x-scheme") };
        let prefix = hint.starts_with("x-vpn-message:");
        #[cfg(test)]
        mod tests { fn f() { Notification::new("Only in tests", b); } }
    "#;
    let found: Vec<_> = copy_literals(&without_test_items(source))
        .into_iter()
        .filter(|(_, literal)| reads_as_english(literal))
        .map(|(_, literal)| literal)
        .collect();
    assert_eq!(found, ["Saved your file", "Hello there", "Open settings"]);
}
