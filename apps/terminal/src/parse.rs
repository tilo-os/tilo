//! What a command run printed, read as items: the generic reading every run
//! gets (escape sequences stripped; `path:line[:column]` locations, URLs,
//! error and warning lines) and a tool's own reading, chosen by the command
//! (`cargo`/`rustc` diagnostics in the human and JSON formats, libtest
//! results, `git status`). docs/app-model/ush.md, "Parsers".

use crate::resource::{ItemKind, RunItem};

/// The most items one run keeps.
pub const MAX_ITEMS: usize = 512;
/// The longest text an item keeps.
const MAX_TEXT: usize = 512;

/// The tool whose reading a command line gets: `cargo`, `rustc`, `git`, or
/// none.
#[must_use]
pub fn tool_of(command: &str) -> &'static str {
    let words: Vec<&str> = command
        .split_whitespace()
        .skip_while(|word| word.contains('=') && !word.starts_with('-'))
        .collect();
    match (words.first().copied(), words.get(1).copied()) {
        (Some("cargo"), _) => "cargo",
        (Some("rustc"), _) => "rustc",
        (Some("git"), Some("status")) => "git-status",
        _ => "",
    }
}

/// The items of a run of `command` that printed `output`.
#[must_use]
pub fn items(command: &str, output: &[u8]) -> Vec<RunItem> {
    let text = strip_ansi(&String::from_utf8_lossy(output));
    let mut items = match tool_of(command) {
        "cargo" | "rustc" => {
            let mut found = rust_json(&text);
            if found.is_empty() {
                found = rust_human(&text);
            }
            found.extend(libtest(&text));
            found
        }
        "git-status" => git_status(&text),
        _ => Vec::new(),
    };
    let known: Vec<(Option<String>, Option<u32>)> = items
        .iter()
        .map(|item| (item.path.clone(), item.line))
        .collect();
    // A tool that reports diagnostics reads its own error and warning lines.
    let diagnostics = !matches!(tool_of(command), "cargo" | "rustc");
    for item in generic(&text)
        .into_iter()
        .filter(|item| diagnostics || !matches!(item.kind, ItemKind::Error | ItemKind::Warning))
    {
        let duplicate =
            item.kind == ItemKind::Location && known.contains(&(item.path.clone(), item.line));
        if !duplicate {
            items.push(item);
        }
    }
    items.truncate(MAX_ITEMS);
    items
}

/// `text` without its escape sequences (CSI, OSC, other `ESC x`), carriage
/// returns resolved to the last overwrite of each line.
#[must_use]
pub fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => match chars.next() {
                Some('[') => {
                    for next in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&next) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    while let Some(next) = chars.next() {
                        if next == '\u{7}' {
                            break;
                        }
                        if next == '\u{1b}' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                _ => {}
            },
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    continue;
                }
                let start = out.rfind('\n').map_or(0, |at| at + 1);
                out.truncate(start);
            }
            c if c.is_control() && c != '\n' && c != '\t' => {}
            c => out.push(c),
        }
    }
    out
}

fn clip(text: &str) -> String {
    let text = text.trim();
    match text.char_indices().nth(MAX_TEXT) {
        Some((at, _)) => text.get(..at).unwrap_or(text).to_owned(),
        None => text.to_owned(),
    }
}

fn item(kind: ItemKind, text: &str) -> RunItem {
    RunItem {
        kind,
        text: clip(text),
        path: None,
        line: None,
        column: None,
        code: None,
    }
}

/// A `path:line[:column]` at the start of `word`, the path relative or
/// absolute with a file extension or a slash.
fn location(word: &str) -> Option<(String, u32, Option<u32>)> {
    let word = word.trim_matches(|c: char| matches!(c, '(' | ')' | ',' | '\'' | '"' | '[' | ']'));
    let mut parts = word.split(':');
    let path = parts.next()?;
    let line: u32 = parts.next()?.parse().ok()?;
    let column = parts.next().and_then(|c| c.parse().ok());
    let looks_like_path = !path.is_empty()
        && !path.contains("//")
        && (path.contains('/')
            || path.rsplit_once('.').is_some_and(|(_, ext)| {
                (1..=5).contains(&ext.len()) && ext.chars().all(|c| c.is_ascii_alphanumeric())
            }))
        && path.chars().all(|c| !c.is_whitespace());
    (looks_like_path && line > 0).then(|| (path.to_owned(), line, column))
}

/// The generic reading: locations, URLs, and error and warning lines.
#[must_use]
pub fn generic(text: &str) -> Vec<RunItem> {
    let mut out = Vec::new();
    for line in text.lines() {
        let lower = line.to_ascii_lowercase();
        let trimmed = lower.trim_start();
        if trimmed.starts_with("error")
            || trimmed.contains(" error:")
            || trimmed.starts_with("fatal:")
        {
            out.push(item(ItemKind::Error, line));
        } else if trimmed.starts_with("warning") {
            out.push(item(ItemKind::Warning, line));
        }
        for word in line.split_whitespace() {
            if let Some(at) = word.find("https://").or_else(|| word.find("http://")) {
                let url = word
                    .get(at..)
                    .unwrap_or_default()
                    .trim_end_matches(['.', ',', ')', '\'', '"', '>']);
                let mut found = item(ItemKind::Url, url);
                found.path = Some(url.to_owned());
                out.push(found);
            } else if let Some((path, line_no, column)) = location(word) {
                let mut found = item(ItemKind::Location, line);
                found.path = Some(path);
                found.line = Some(line_no);
                found.column = column;
                out.push(found);
            }
        }
    }
    out
}

/// `cargo`/`rustc` diagnostics in the human format: `error[E0308]: …` with
/// its `--> path:line:column` underneath.
#[must_use]
pub fn rust_human(text: &str) -> Vec<RunItem> {
    let mut out: Vec<RunItem> = Vec::new();
    let mut open: Option<usize> = None;
    for line in text.lines() {
        let (kind, rest) = if let Some(rest) = line.strip_prefix("error") {
            (ItemKind::Error, rest)
        } else if let Some(rest) = line.strip_prefix("warning") {
            (ItemKind::Warning, rest)
        } else {
            if let Some(at) = open
                && let Some(arrow) = line.trim_start().strip_prefix("--> ")
                && let Some((path, line_no, column)) = location(arrow)
                && let Some(found) = out.get_mut(at)
                && found.path.is_none()
            {
                found.path = Some(path);
                found.line = Some(line_no);
                found.column = column;
            }
            continue;
        };
        let (code, message) = match rest.strip_prefix('[') {
            Some(coded) => match coded.split_once("]:") {
                Some((code, message)) => (Some(code.to_owned()), message),
                None => continue,
            },
            None => match rest.strip_prefix(':') {
                Some(message) => (None, message),
                None => continue,
            },
        };
        // The summary lines cargo prints after the diagnostics.
        if message.trim_start().starts_with("could not compile")
            || message.trim_start().starts_with("aborting due to")
            || message.contains("generated") && message.contains("warning")
        {
            open = None;
            continue;
        }
        let mut found = item(kind, message);
        found.code = code;
        out.push(found);
        open = Some(out.len() - 1);
    }
    out
}

/// `cargo --message-format=json` (and `rustc --error-format=json`): one
/// object per line, each `compiler-message` an error or warning with its
/// code and primary span.
#[must_use]
pub fn rust_json(text: &str) -> Vec<RunItem> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if !line.starts_with('{') {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let message = match value.get("reason").and_then(serde_json::Value::as_str) {
            Some("compiler-message") => value.get("message"),
            Some(_) => continue,
            None => Some(&value),
        };
        let Some(message) = message else { continue };
        let kind = match message.get("level").and_then(serde_json::Value::as_str) {
            Some("error" | "error: internal compiler error") => ItemKind::Error,
            Some("warning") => ItemKind::Warning,
            _ => continue,
        };
        let text = message
            .get("message")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        if text.starts_with("aborting due to") || text.contains("warnings emitted") {
            continue;
        }
        let mut found = item(kind, text);
        found.code = message
            .pointer("/code/code")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        if let Some(span) = message
            .get("spans")
            .and_then(serde_json::Value::as_array)
            .and_then(|spans| {
                spans
                    .iter()
                    .find(|span| span.get("is_primary") == Some(&serde_json::Value::Bool(true)))
            })
        {
            found.path = span
                .get("file_name")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned);
            found.line = span
                .get("line_start")
                .and_then(serde_json::Value::as_u64)
                .and_then(|n| u32::try_from(n).ok());
            found.column = span
                .get("column_start")
                .and_then(serde_json::Value::as_u64)
                .and_then(|n| u32::try_from(n).ok());
        }
        out.push(found);
    }
    out
}

/// Test results: libtest's `test path::name ... ok | FAILED | ignored` and
/// nextest's `PASS|FAIL|SKIP [ 0.01s] (1/3) crate path::name`, one item per
/// test with its name as the code.
#[must_use]
pub fn libtest(text: &str) -> Vec<RunItem> {
    let mut out: Vec<RunItem> = Vec::new();
    let mut add = |kind: ItemKind, name: &str| {
        if name.is_empty() || name.contains(' ') {
            return;
        }
        if out
            .iter()
            .any(|seen| seen.kind == kind && seen.code.as_deref() == Some(name))
        {
            return;
        }
        let mut found = item(kind, name);
        found.code = Some(name.to_owned());
        out.push(found);
    };
    for line in text.lines() {
        let trimmed = line.trim();
        let nextest = [
            ("PASS [", ItemKind::TestPassed),
            ("FAIL [", ItemKind::TestFailed),
            ("SKIP [", ItemKind::TestIgnored),
        ]
        .iter()
        .find_map(|(prefix, kind)| trimmed.strip_prefix(prefix).map(|rest| (*kind, rest)));
        if let Some((kind, rest)) = nextest {
            if let Some(name) = rest.split_whitespace().last() {
                add(kind, name);
            }
            continue;
        }
        let Some(rest) = trimmed.strip_prefix("test ") else {
            continue;
        };
        let Some((name, outcome)) = rest.rsplit_once(" ... ") else {
            continue;
        };
        let kind = match outcome
            .split(|c: char| c.is_whitespace() || c == ',')
            .next()
        {
            Some("ok") => ItemKind::TestPassed,
            Some("FAILED") => ItemKind::TestFailed,
            Some("ignored") => ItemKind::TestIgnored,
            _ => continue,
        };
        add(kind, name.trim());
    }
    out
}

/// `git status`: the short and porcelain formats (`XY path`) and the long
/// one (`modified:   path`), one item per path with its status as the code.
#[must_use]
pub fn git_status(text: &str) -> Vec<RunItem> {
    let mut out = Vec::new();
    let mut untracked = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.ends_with(':') && !line.starts_with(char::is_whitespace) {
            untracked = trimmed == "Untracked files:";
            continue;
        }
        if untracked && line.starts_with('\t') && !trimmed.is_empty() {
            let mut found = item(ItemKind::Change, trimmed);
            found.path = Some(trimmed.to_owned());
            found.code = Some("??".to_owned());
            out.push(found);
            continue;
        }
        let long = [
            "modified:",
            "new file:",
            "deleted:",
            "renamed:",
            "typechange:",
            "both modified:",
        ]
        .iter()
        .find_map(|label| trimmed.strip_prefix(label).map(|path| (*label, path)));
        let (code, path) = if let Some((label, path)) = long {
            (label.trim_end_matches(':').to_owned(), path.trim())
        } else if line.len() > 3
            && line.is_char_boundary(2)
            && line.get(2..3) == Some(" ")
            && line
                .get(..2)
                .is_some_and(|xy| xy.chars().all(|c| "MADRCU?! ".contains(c)) && xy != "  ")
        {
            (
                line.get(..2).unwrap_or_default().trim().to_owned(),
                line.get(3..).unwrap_or_default().trim(),
            )
        } else {
            continue;
        };
        let path = path.rsplit(" -> ").next().unwrap_or(path);
        if path.is_empty() {
            continue;
        }
        let mut found = item(ItemKind::Change, path);
        found.path = Some(path.to_owned());
        found.code = Some(code);
        out.push(found);
    }
    out
}

/// The command that runs the one test `test` of a run of `command` by its
/// tool, or `None` when the tool has no such command.
#[must_use]
pub fn rerun_test(command: &str, test: &str) -> Option<String> {
    if tool_of(command) != "cargo"
        || test.is_empty()
        || !test
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | ':'))
    {
        return None;
    }
    // `cargo test -p crate …` keeps its package selection.
    let words: Vec<&str> = command.split_whitespace().collect();
    let mut package = String::new();
    let mut words_iter = words.iter();
    while let Some(word) = words_iter.next() {
        if (*word == "-p" || *word == "--package")
            && let Some(name) = words_iter.next()
        {
            package = format!(" -p {name}");
        }
    }
    let program = words.first().copied().unwrap_or("cargo");
    Some(format!("{program} test{package} -- --exact {test}"))
}

/// What showing one of a run's items opens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Shown {
    /// A web page, in the browser.
    Page(String),
    /// A file at a line, a path relative to the run's directory made
    /// absolute.
    File {
        path: std::path::PathBuf,
        line: Option<u32>,
        column: Option<u32>,
    },
}

/// What showing the item at `position` of a run in `cwd` opens.
///
/// # Errors
/// Returns why the item shows nothing: there is none at `position`, or it
/// names neither a page nor a file.
pub fn shown(cwd: &str, items: &[RunItem], position: usize) -> Result<Shown, String> {
    let item = items
        .get(position)
        .ok_or_else(|| format!("the run has {} items", items.len()))?;
    let path = item
        .path
        .as_deref()
        .ok_or("this item names neither a page nor a file")?;
    if item.kind == ItemKind::Url {
        return Ok(Shown::Page(path.to_owned()));
    }
    let path = std::path::Path::new(path);
    Ok(Shown::File {
        path: if path.is_absolute() {
            path.to_owned()
        } else {
            std::path::Path::new(cwd).join(path)
        },
        line: item.line,
        column: item.column,
    })
}

#[cfg(test)]
mod tests;
