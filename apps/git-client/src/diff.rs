//! Unified-diff parsing.
//!
//! [`parse_unified`] turns standard `git diff` text into a structured
//! [`FileDiff`] with per-line old/new numbering and hunk boundaries, so
//! both the unified and side-by-side renderers can address lines without
//! re-scanning the raw text.

use crate::model::{DiffHunk, DiffLine, DiffLineKind, FileDiff};

/// Parse a single-file unified diff (the body git emits for one path) into
/// a [`FileDiff`]. `path` and `language` are supplied by the caller since
/// the headers may be ambiguous (renames, new files).
pub fn parse_unified(path: &str, language: &str, text: &str) -> FileDiff {
    let mut diff = FileDiff {
        path: path.to_string(),
        old_path: None,
        language: language.to_string(),
        ..Default::default()
    };

    let mut state = HunkState::default();

    for raw in text.lines() {
        if raw.starts_with("@@") {
            state.open_hunk(&mut diff, raw);
            continue;
        }

        if is_meta_line(raw) {
            push_meta_line(&mut diff, raw, state.hunk_idx);
            continue;
        }

        if !state.in_hunk {
            continue;
        }

        state.push_content_line(&mut diff, raw);
    }

    state.close_hunk(&mut diff);
    diff
}

/// Running position while walking a unified diff: which hunk is open and
/// the next old/new line numbers to hand out.
#[derive(Default)]
struct HunkState {
    old_no: u32,
    new_no: u32,
    hunk_idx: usize,
    in_hunk: bool,
}

impl HunkState {
    /// Close the currently-open hunk's line range, if one is open.
    fn close_hunk(&self, diff: &mut FileDiff) {
        if self.in_hunk
            && let Some(h) = diff.hunks.last_mut()
        {
            h.line_range.1 = diff.lines.len();
        }
    }

    /// Close the previous hunk (if any) and open a new one at `raw`'s
    /// `@@ … @@` header.
    fn open_hunk(&mut self, diff: &mut FileDiff, raw: &str) {
        self.close_hunk(diff);
        let (old_start, new_start, header) = parse_hunk_header(raw);
        self.old_no = old_start;
        self.new_no = new_start;
        self.hunk_idx = diff.hunks.len();
        let hunk_start_line = diff.lines.len();
        diff.hunks.push(DiffHunk {
            header,
            old_start,
            new_start,
            line_range: (hunk_start_line, hunk_start_line),
        });
        diff.lines.push(DiffLine {
            kind: DiffLineKind::Hunk,
            old_lineno: None,
            new_lineno: None,
            content: raw.to_string(),
            hunk: self.hunk_idx,
        });
        self.in_hunk = true;
    }

    /// Classify and push one `+`/`-`/` ` content line, advancing the
    /// relevant line counters.
    fn push_content_line(&mut self, diff: &mut FileDiff, raw: &str) {
        let (kind, body) = match raw.chars().next() {
            Some('+') => (DiffLineKind::Addition, raw.get(1..).unwrap_or("")),
            Some('-') => (DiffLineKind::Deletion, raw.get(1..).unwrap_or("")),
            Some(' ') => (DiffLineKind::Context, raw.get(1..).unwrap_or("")),
            None => (DiffLineKind::Context, ""),
            _ => (DiffLineKind::Context, raw),
        };

        let (old_l, new_l) = match kind {
            DiffLineKind::Addition => {
                let n = self.new_no;
                self.new_no += 1;
                (None, Some(n))
            }
            DiffLineKind::Deletion => {
                let o = self.old_no;
                self.old_no += 1;
                (Some(o), None)
            }
            _ => {
                let o = self.old_no;
                let n = self.new_no;
                self.old_no += 1;
                self.new_no += 1;
                (Some(o), Some(n))
            }
        };

        diff.lines.push(DiffLine {
            kind,
            old_lineno: old_l,
            new_lineno: new_l,
            content: body.to_string(),
            hunk: self.hunk_idx,
        });
    }
}

/// Whether `raw` is a file-level metadata line (`diff --git`, `index`,
/// `--- a/…`, rename/similarity notes, binary markers, …) rather than hunk
/// content.
fn is_meta_line(raw: &str) -> bool {
    raw.starts_with("diff ")
        || raw.starts_with("index ")
        || raw.starts_with("--- ")
        || raw.starts_with("+++ ")
        || raw.starts_with("new file")
        || raw.starts_with("deleted file")
        || raw.starts_with("rename ")
        || raw.starts_with("similarity ")
        || raw.starts_with("Binary ")
}

/// Record a metadata line's file-level flags and append it to `diff.lines`.
fn push_meta_line(diff: &mut FileDiff, raw: &str, hunk_idx: usize) {
    if raw.starts_with("new file") {
        diff.is_new = true;
    }
    if raw.starts_with("deleted file") {
        diff.is_deleted = true;
    }
    if raw.starts_with("Binary ") {
        diff.binary = true;
    }
    diff.lines.push(DiffLine {
        kind: DiffLineKind::Meta,
        old_lineno: None,
        new_lineno: None,
        content: raw.to_string(),
        hunk: hunk_idx,
    });
}

/// Parse an `@@ -a,b +c,d @@ trailing @@` header into (`old_start`, `new_start`,
/// remaining-section-text).
fn parse_hunk_header(line: &str) -> (u32, u32, String) {
    // Expected shape: @@ -12,7 +12,9 @@ fn foo()
    let mut old_start = 0;
    let mut new_start = 0;
    if let Some(rest) = line.strip_prefix("@@ ") {
        for tok in rest.split_whitespace() {
            if let Some(neg) = tok.strip_prefix('-') {
                old_start = neg
                    .split(',')
                    .next()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
            } else if let Some(pos) = tok.strip_prefix('+') {
                new_start = pos
                    .split(',')
                    .next()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
            }
        }
    }
    // The section heading after the closing `@@`.
    let header = line
        .splitn(3, "@@")
        .nth(2)
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    (old_start.max(1), new_start.max(1), header)
}

/// The grammar token the diff pane highlights `path` with: its extension, or
/// the file name itself for extensionless files such as `Dockerfile`.
pub fn language_for(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => ext.to_ascii_lowercase(),
        _ => name.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Diff parsing: hunk line numbering, new/deleted file detection and language
    /// detection. Each phase is a former test with its own fresh fixture.
    #[test]
    fn unified_diff_parsing_numbers_lines_and_classifies_files() {
        parses_hunk_line_numbering();
        detects_new_and_deleted_files();
        language_detection();
    }

    fn parses_hunk_line_numbering() {
        let text = "\
@@ -1,3 +1,4 @@ fn main()
 context
-removed
+added one
+added two
 tail";
        let d = parse_unified("src/main.rs", "rust", text);
        assert_eq!(d.hunks.len(), 1);
        assert_eq!(d.additions(), 2);
        assert_eq!(d.deletions(), 1);
        // First context line is old 1 / new 1.
        let ctx = d.lines.iter().find(|l| l.content == "context").unwrap();
        assert_eq!(ctx.old_lineno, Some(1));
        assert_eq!(ctx.new_lineno, Some(1));
        // Addition has only a new line number.
        let add = d.lines.iter().find(|l| l.content == "added one").unwrap();
        assert_eq!(add.old_lineno, None);
        assert_eq!(add.new_lineno, Some(2));
    }

    fn detects_new_and_deleted_files() {
        let new = parse_unified(
            "a.txt",
            "text",
            "new file mode 100644\n@@ -0,0 +1,1 @@\n+hi",
        );
        assert!(new.is_new);
        let del = parse_unified(
            "b.txt",
            "text",
            "deleted file mode 100644\n@@ -1,1 +0,0 @@\n-bye",
        );
        assert!(del.is_deleted);
    }

    fn language_detection() {
        assert_eq!(language_for("src/main.rs"), "rs");
        assert_eq!(language_for("Cargo.toml"), "toml");
        assert_eq!(language_for("docs/README.MD"), "md");
        assert_eq!(language_for("os/Dockerfile"), "Dockerfile");
        assert_eq!(language_for(".gitignore"), ".gitignore");
    }
}
