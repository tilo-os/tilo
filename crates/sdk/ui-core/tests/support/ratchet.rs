//! The literal ratchets: one engine behind every audit that counts a pattern
//! per file and lets the count only fall (`design_token_audit.rs`,
//! `spacing_token_audit.rs`, `typography_token_audit.rs`, `copy_audit.rs`).
//!
//! A [`Ratchet`] walks its roots, counts each [`Pattern`] per production file
//! (outside comments and `#[cfg(test)]` items), and compares the counts with
//! its baseline. Growth fails; a drop is only noted ([`note_stale`]), so a
//! change that removes debt never has to touch a baseline to stay green.
//!
//! Baselines are sharded so app changes never conflict with each other: one
//! file per top-level unit at `tests/baselines/<audit>/<unit>.txt`, where the
//! unit is `apps-<name>`, `ui-core` or `shell` ([`shard_of`]). A missing file
//! allows zero, so a unit that reaches zero deletes its file and is locked
//! there.
//!
//! Regenerate with `UPDATE_RATCHET_BASELINES=1`. `RATCHET_SCOPE=apps/mail`
//! rewrites only that unit's shard. An update refuses to raise any count
//! unless `RATCHET_ALLOW_GROWTH=1` is also set. A cargo drop-in that runs
//! tests on a remote host forwards no environment and keeps a test's writes
//! there, so through one run `cargo test -p tilo-ui --test it do_not_grow
//! --success-output immediate`: a passing ratchet with progress prints the text each affected
//! shard should now hold ([`note_stale`]).

use regex::{Captures, Regex};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

/// `(path relative to the repository root, pattern key) → count`.
pub type Counts = BTreeMap<(String, String), usize>;

/// Set to `1` to rewrite baselines from the tree instead of checking them.
pub const UPDATE_VAR: &str = "UPDATE_RATCHET_BASELINES";
/// Restricts an update to one unit (`apps/mail`, `crates/session/shell`,
/// `crates/sdk/ui-core`, or a unit name such as `apps-mail`).
pub const SCOPE_VAR: &str = "RATCHET_SCOPE";
/// Set to `1` to let an update raise a count. Review the diff first.
pub const GROWTH_VAR: &str = "RATCHET_ALLOW_GROWTH";

/// The command that regenerates the ratchet shards. Every ratchet test name
/// ends in `do_not_grow`, so the filter reaches each of them.
pub const UPDATE_COMMAND: &str =
    "UPDATE_RATCHET_BASELINES=1 RATCHET_SCOPE=<dir> cargo test -p tilo-ui --test it do_not_grow";

/// One counted shape.
pub struct Pattern {
    /// What the baseline names.
    pub key: &'static str,
    pub regex: Regex,
    /// What a new hit is told to do instead.
    pub message: &'static str,
    /// Whether a match counts. `None` counts every match.
    keep: Option<fn(&Captures<'_>) -> bool>,
    keep_line: Option<fn(&str, &Captures<'_>) -> bool>,
}

impl Pattern {
    pub fn new(key: &'static str, regex: &str, message: &'static str) -> Self {
        Self {
            key,
            regex: Regex::new(regex)
                .unwrap_or_else(|e| panic!("ratchet pattern `{key}` must compile: {e}")),
            message,
            keep: None,
            keep_line: None,
        }
    }

    /// Counts only the matches `keep` accepts, for rules the regex dialect
    /// cannot express (an excluded name, an acronym list).
    #[must_use]
    pub fn keep(mut self, keep: fn(&Captures<'_>) -> bool) -> Self {
        self.keep = Some(keep);
        self
    }

    /// Inspect surrounding source without consuming it from the regex scan.
    /// This preserves multiple calls on one line when classifying arguments.
    #[must_use]
    pub fn keep_line(mut self, keep: fn(&str, &Captures<'_>) -> bool) -> Self {
        self.keep_line = Some(keep);
        self
    }

    /// How many times this pattern counts on `line`.
    pub fn hits(&self, line: &str) -> usize {
        self.regex
            .captures_iter(line)
            .filter(|c| {
                self.keep.is_none_or(|keep| keep(c))
                    && self.keep_line.is_none_or(|keep| keep(line, c))
            })
            .count()
    }
}

/// One audit: what it scans, what it counts and where its shards live.
pub struct Ratchet {
    /// The baseline directory under `tests/baselines/`.
    pub name: &'static str,
    /// Files or directories relative to the repository root. One `*`
    /// component expands to every entry (`apps/*/src` reaches each app, so a
    /// new app is covered the day it lands). Every root must yield a file.
    pub roots: &'static [&'static str],
    pub patterns: Vec<Pattern>,
    /// Files the audit skips in addition to test files ([`is_test_file`]).
    pub exempt: fn(&Path) -> bool,
    /// What a failing change is told, after the per-pattern messages.
    pub advice: &'static str,
}

/// Exempts nothing beyond test files.
pub fn no_exemptions(_: &Path) -> bool {
    false
}

/// Integration test directories and in-tree test modules split into their
/// own file (`tests.rs`, `*_tests.rs`). `#[cfg(test)]` items inside a
/// production file are skipped line by line ([`test_module_spans`]).
pub fn is_test_file(path: &Path) -> bool {
    let s = path.to_string_lossy();
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    s.contains("/tests/") || name == "tests.rs" || name.ends_with("_tests.rs")
}

/// The repository checkout this crate lives in.
pub fn repository_root() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("UI crate lives under crates/sdk in the repository");
    assert!(
        root.join("Cargo.toml").is_file() && root.join("apps").is_dir(),
        "ratchet repository anchor must contain Cargo.toml and apps"
    );
    root.to_path_buf()
}

/// `crates/sdk/ui-core/tests/baselines`.
pub fn baselines_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/baselines")
}

/// The shard a repository-relative path belongs to: `apps/mail/src/x.rs` →
/// `apps-mail`, `crates/sdk/ui-core/src/x.rs` → `ui-core`,
/// `crates/session/shell/src/x.rs` → `shell`, any other crate → its
/// directory name.
pub fn shard_of(rel: &str) -> String {
    let parts: Vec<&str> = rel.split('/').filter(|p| !p.is_empty()).collect();
    match parts.as_slice() {
        ["apps", app, ..] => format!("apps-{app}"),
        ["crates", _, name, ..] => (*name).to_string(),
        [first, ..] => (*first).to_string(),
        [] => String::new(),
    }
}

/// The shard `RATCHET_SCOPE` names: a directory (`apps/mail`) or a unit name
/// (`apps-mail`).
pub fn scope_unit(scope: &str) -> String {
    let scope = scope.trim().trim_end_matches('/');
    if scope.contains('/') {
        shard_of(scope)
    } else {
        scope.to_string()
    }
}

/// How a run treats the baselines; [`Ratchet::check`] reads it from the
/// environment.
#[derive(Default)]
pub struct Mode {
    pub update: bool,
    pub scope: Option<String>,
    pub allow_growth: bool,
}

impl Mode {
    pub fn from_env() -> Self {
        let flag = |name: &str| std::env::var_os(name).is_some_and(|v| v == "1");
        Self {
            update: flag(UPDATE_VAR),
            scope: std::env::var(SCOPE_VAR)
                .ok()
                .filter(|s| !s.trim().is_empty()),
            allow_growth: flag(GROWTH_VAR),
        }
    }
}

/// What a passing run found below its baseline.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Progress {
    /// Baseline entries above the tree, one line each.
    pub stale: Vec<String>,
    /// `(unit, shard text)` for each unit with a stale entry: what its shard
    /// should now read. An empty text means the unit reached zero and its
    /// shard is deleted.
    pub rewrites: Vec<(String, String)>,
}

/// Every `.rs` file under `root` (or `root` itself when it is a file), minus
/// those `is_exempt` names.
pub fn walk(root: &Path, is_exempt: &dyn Fn(&Path) -> bool, out: &mut Vec<PathBuf>) {
    if root.is_file() {
        if !is_exempt(root) {
            out.push(root.to_path_buf());
        }
        return;
    }
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

/// `root` under `repository`, with one `*` component expanded against the
/// file system.
fn expand(repository: &Path, root: &str) -> Vec<PathBuf> {
    let Some((prefix, suffix)) = root.split_once("/*") else {
        return vec![repository.join(root)];
    };
    let suffix = suffix.trim_start_matches('/');
    let mut out: Vec<PathBuf> = fs::read_dir(repository.join(prefix))
        .map(|read| {
            read.flatten()
                .map(|entry| entry.path())
                .filter(|p| p.is_dir())
                .map(|p| if suffix.is_empty() { p } else { p.join(suffix) })
                .filter(|p| p.exists())
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

fn relative(path: &Path, repository: &Path) -> String {
    path.strip_prefix(repository)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

impl Ratchet {
    /// What a new hit of `key` is told to do.
    pub fn message(&self, key: &str) -> &'static str {
        self.patterns
            .iter()
            .find(|p| p.key == key)
            .map_or("", |p| p.message)
    }

    /// The pattern named `key`, for self-tests.
    pub fn pattern(&self, key: &str) -> &Pattern {
        self.patterns
            .iter()
            .find(|p| p.key == key)
            .unwrap_or_else(|| panic!("the {} ratchet has no pattern `{key}`", self.name))
    }

    /// The production files under every root, and the roots that yielded
    /// none (a scan that cannot observe its subject would pass vacuously).
    pub fn files(&self, repository: &Path) -> (Vec<PathBuf>, Vec<&'static str>) {
        let exempt = |p: &Path| is_test_file(p) || (self.exempt)(p);
        let mut files = Vec::new();
        let mut unreached = Vec::new();
        for root in self.roots {
            let before = files.len();
            for dir in expand(repository, root) {
                walk(&dir, &exempt, &mut files);
            }
            if files.len() == before {
                unreached.push(*root);
            }
        }
        files.sort();
        files.dedup();
        (files, unreached)
    }

    /// Every pattern per file, skipping comments and `#[cfg(test)]` items.
    pub fn count(&self, repository: &Path, files: &[PathBuf]) -> Counts {
        let mut out = Counts::new();
        for f in files {
            let content = fs::read_to_string(f)
                .unwrap_or_else(|e| panic!("could not read {}: {e}", f.display()));
            let skip = test_module_spans(&content);
            let rel = relative(f, repository);
            for (i, line) in content.lines().enumerate() {
                if is_skipped(line, i, &skip) {
                    continue;
                }
                for pattern in &self.patterns {
                    let hits = pattern.hits(line);
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

    /// Checks this ratchet's shards against the repository, or rewrites them
    /// under `UPDATE_RATCHET_BASELINES=1`. Panics on growth.
    pub fn check(&self) {
        match self.run(&repository_root(), &baselines_root(), &Mode::from_env()) {
            Ok(progress) => note_stale(self.name, &progress),
            Err(failure) => panic!("{failure}"),
        }
    }

    /// The check behind [`Ratchet::check`], against any tree and baseline
    /// directory. `Ok` carries the progress to lock in.
    pub fn run(
        &self,
        repository: &Path,
        baselines: &Path,
        mode: &Mode,
    ) -> Result<Progress, String> {
        let (files, unreached) = self.files(repository);
        if !unreached.is_empty() {
            return Err(format!(
                "the {} ratchet did not reach {}; the scan cannot observe its subject",
                self.name,
                unreached.join(", ")
            ));
        }
        let found = self.count(repository, &files);
        let dir = baselines.join(self.name);
        let baseline = read_shards(&dir)?;
        if mode.update {
            self.update(&dir, &found, &baseline, mode)?;
            return Ok(Progress::default());
        }
        let (grew, stale) =
            ratchet_failures(&found, &baseline, &|key| self.message(key).to_string());
        if grew.is_empty() {
            let units: BTreeSet<String> = stale
                .iter()
                .map(|line| shard_of(line.split(' ').next().unwrap_or("")))
                .collect();
            let rewrites = units
                .into_iter()
                .map(|unit| {
                    let text = shard_text(self.name, &unit, &shard_counts(&found, &unit));
                    (unit, text)
                })
                .collect();
            return Ok(Progress { stale, rewrites });
        }
        Err(format!(
            "{n} file(s) grew in the {name} ratchet \
             (crates/sdk/ui-core/tests/baselines/{name}/<unit>.txt).\n{grew}\n\n{advice}\n\
             docs/ui-migration.md maps each pattern to the API that replaces it. Moving a \
             literal into a local, renaming a constant or reshaping a call so the pattern stops \
             matching is not a fix: it hides the debt from the ratchet (HIG §9.13).",
            n = grew.len(),
            name = self.name,
            grew = grew.join("\n"),
            advice = self.advice,
        ))
    }

    fn update(
        &self,
        dir: &Path,
        found: &Counts,
        baseline: &Counts,
        mode: &Mode,
    ) -> Result<(), String> {
        let units: BTreeSet<String> = match &mode.scope {
            Some(scope) => std::iter::once(scope_unit(scope)).collect(),
            None => found
                .keys()
                .chain(baseline.keys())
                .map(|(path, _)| shard_of(path))
                .collect(),
        };
        if !mode.allow_growth {
            let grew: Vec<String> = found
                .iter()
                .filter(|((path, _), _)| units.contains(&shard_of(path)))
                .filter_map(|(key, count)| {
                    let allowed = baseline.get(key).copied().unwrap_or(0);
                    (*count > allowed)
                        .then(|| format!("{} [{}]: {allowed} → {count}", key.0, key.1))
                })
                .collect();
            if !grew.is_empty() {
                return Err(format!(
                    "refusing to raise {} count(s) in the {} ratchet; set {GROWTH_VAR}=1 only \
                     after reviewing why each one grew:\n{}",
                    grew.len(),
                    self.name,
                    grew.join("\n")
                ));
            }
        }
        for unit in &units {
            write_shard(
                &dir.join(format!("{unit}.txt")),
                self.name,
                unit,
                &shard_counts(found, unit),
            )?;
        }
        Ok(())
    }
}

/// Every `<unit>.txt` under `dir`, merged. A missing directory or file is an
/// allowance of zero. An entry filed under the wrong unit is an error, so a
/// shard can be deleted knowing it holds only its own unit.
pub fn read_shards(dir: &Path) -> Result<Counts, String> {
    let mut out = Counts::new();
    let Ok(read) = fs::read_dir(dir) else {
        return Ok(out);
    };
    let mut paths: Vec<PathBuf> = read
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("txt"))
        .collect();
    paths.sort();
    for path in paths {
        let unit = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_string();
        for ((file, key), count) in read_baseline(&path)? {
            if shard_of(&file) != unit {
                return Err(format!(
                    "{}: {file} belongs in the {} shard, not {unit}",
                    path.display(),
                    shard_of(&file)
                ));
            }
            out.insert((file, key), count);
        }
    }
    Ok(out)
}

/// `<path> <pattern> <count>` lines; `#` comments and blank lines ignored.
pub fn read_baseline(path: &Path) -> Result<Counts, String> {
    let raw =
        fs::read_to_string(path).map_err(|e| format!("could not read {}: {e}", path.display()))?;
    let mut out = BTreeMap::new();
    for (n, line) in raw.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line_no = n.saturating_add(1);
        let parts: Vec<&str> = line.split_whitespace().collect();
        let [file, key, count] = parts.as_slice() else {
            return Err(format!(
                "{}:{line_no}: expected `<path> <pattern> <count>`, got: {line}",
                path.display()
            ));
        };
        let count: usize = count
            .parse()
            .map_err(|_| format!("{}:{line_no}: count must be a number", path.display()))?;
        out.insert(((*file).to_string(), (*key).to_string()), count);
    }
    Ok(out)
}

/// The entries of `found` that belong to `unit`.
pub fn shard_counts(found: &Counts, unit: &str) -> Counts {
    found
        .iter()
        .filter(|((path, _), _)| shard_of(path) == unit)
        .map(|(k, v)| (k.clone(), *v))
        .collect()
}

/// The text of one shard, or an empty string when `counts` is empty (a
/// missing shard allows zero).
pub fn shard_text(audit: &str, unit: &str, counts: &Counts) -> String {
    let mut output = String::new();
    if counts.is_empty() {
        return output;
    }
    for line in [
        format!("The {audit} ratchet for {unit}: counts may only fall (HIG §9.13)."),
        "Format: <path relative to the repository root> <pattern> <count>".to_string(),
        format!("Regenerate: {UPDATE_COMMAND}"),
    ] {
        writeln!(&mut output, "# {line}").expect("writing to a String is infallible");
    }
    for ((file, key), count) in counts {
        writeln!(&mut output, "{file} {key} {count}").expect("writing to a String is infallible");
    }
    output
}

/// Writes one shard, or deletes it when `counts` is empty: a missing shard
/// allows zero, so the unit is then locked there.
pub fn write_shard(path: &Path, audit: &str, unit: &str, counts: &Counts) -> Result<(), String> {
    let text = shard_text(audit, unit, counts);
    if text.is_empty() {
        return match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("could not delete {}: {e}", path.display())),
        };
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
    }
    fs::write(path, text).map_err(|e| format!("could not write {}: {e}", path.display()))
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

/// Whether line `index` (0-based) is a comment line or inside a test span:
/// the two things every ratchet skips.
pub fn is_skipped(line: &str, index: usize, spans: &[(usize, usize)]) -> bool {
    line.trim_start().starts_with("//") || spans.iter().any(|(a, b)| index >= *a && index < *b)
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

/// Reports baseline entries the tree has dropped below, with the text each
/// affected shard should now hold. An improvement never fails a change; the
/// record is lowered in that change or a later one. Under nextest the note
/// of a passing test shows with `--success-output immediate`.
pub fn note_stale(audit: &str, progress: &Progress) {
    if progress.stale.is_empty() {
        return;
    }
    let mut note = format!(
        "{} entr(ies) in crates/sdk/ui-core/tests/baselines/{audit}/ are above the tree; lock \
         the progress in with {UPDATE_COMMAND}, or write the shards below (a remote cargo \
         drop-in forwards no environment and keeps its test writes on the remote host):\n{}\n",
        progress.stale.len(),
        progress.stale.join("\n"),
    );
    for (unit, text) in &progress.rewrites {
        let path = format!("crates/sdk/ui-core/tests/baselines/{audit}/{unit}.txt");
        if text.is_empty() {
            let _ = writeln!(note, "\ndelete {path} (the unit reached zero)");
        } else {
            let _ = write!(note, "\n{path} now reads:\n{text}");
        }
    }
    eprintln!("notice: {note}");
    if let Some(summary) = std::env::var_os("GITHUB_STEP_SUMMARY") {
        use std::io::Write as _;
        if let Ok(mut file) = fs::OpenOptions::new().append(true).open(summary) {
            let _ = writeln!(file, "### Ratchet progress\n\n```\n{note}```");
        }
    }
}

/// The span finder is what every ratchet's coverage rests on, so its shapes
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

#[test]
fn shards_are_one_file_per_app_toolkit_and_shell() {
    assert_eq!(shard_of("apps/mail/src/app.rs"), "apps-mail");
    assert_eq!(shard_of("apps/git-client/src/a/b.rs"), "apps-git-client");
    assert_eq!(shard_of("crates/sdk/ui-core/src/widget/x.rs"), "ui-core");
    assert_eq!(shard_of("crates/session/shell/src/main.rs"), "shell");
    assert_eq!(scope_unit("apps/mail"), "apps-mail");
    assert_eq!(scope_unit("apps/mail/"), "apps-mail");
    assert_eq!(scope_unit("crates/session/shell"), "shell");
    assert_eq!(scope_unit("crates/sdk/ui-core"), "ui-core");
    assert_eq!(scope_unit("apps-mail"), "apps-mail");
}

/// A synthetic two-app tree run through the real engine: a shard that grows
/// fails, one that shrinks passes with a note, a missing shard allows zero,
/// and a scoped update rewrites only its own shard, refuses growth unless it
/// is allowed, and deletes a shard that reaches zero.
#[test]
fn a_synthetic_shard_fails_when_it_grows_and_passes_when_it_shrinks() {
    let tree = tempfile::tempdir().expect("temporary tree");
    let repo = tree.path().join("repo");
    let baselines = tree.path().join("baselines");
    let source = |app: &str, body: &str| {
        let dir = repo.join("apps").join(app).join("src");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("lib.rs"), body).unwrap();
    };
    let shard = |unit: &str, body: &str| {
        let dir = baselines.join("demo");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(format!("{unit}.txt")), body).unwrap();
    };
    let ratchet = Ratchet {
        name: "demo",
        roots: &["apps/*/src"],
        patterns: vec![Pattern::new("white", r"Color::WHITE", "use a token")],
        exempt: no_exemptions,
        advice: "read it from the theme",
    };
    let check = || ratchet.run(&repo, &baselines, &Mode::default());
    let stale = || check().map(|progress| progress.stale);

    source(
        "mail",
        "fn a() { Color::WHITE; Color::WHITE; }\n// Color::WHITE\n#[cfg(test)]\nmod tests { fn f() { Color::WHITE; } }\n",
    );
    source("chat", "fn b() { Color::WHITE; }\n");
    shard("apps-mail", "# header\napps/mail/src/lib.rs white 2\n");
    shard("apps-chat", "apps/chat/src/lib.rs white 1\n");
    assert_eq!(
        stale(),
        Ok(Vec::new()),
        "comments and test items are not counted"
    );

    // Grows: one more hit than the shard allows.
    source(
        "mail",
        "fn a() { Color::WHITE; Color::WHITE; Color::WHITE; }\n",
    );
    let failure = check().expect_err("growth fails");
    assert!(
        failure.contains("apps/mail/src/lib.rs [white]: 3 occurrence(s), baseline 2 — use a token"),
        "{failure}"
    );
    assert!(failure.contains("read it from the theme"), "{failure}");

    // Shrinks: passes, and the drop is reported as progress with the text
    // the shard should now hold.
    source("mail", "fn a() { Color::WHITE; }\n");
    let progress = check().expect("a drop passes");
    assert_eq!(
        progress.stale,
        vec!["apps/mail/src/lib.rs [white]: now 1, baseline still 2 — lower it to 1".to_string()]
    );
    assert_eq!(progress.rewrites.len(), 1);
    assert_eq!(progress.rewrites[0].0, "apps-mail");
    assert!(
        progress.rewrites[0]
            .1
            .ends_with("\napps/mail/src/lib.rs white 1\n"),
        "{}",
        progress.rewrites[0].1
    );

    // A missing shard allows zero.
    fs::remove_file(baselines.join("demo/apps-chat.txt")).unwrap();
    assert!(
        check().is_err(),
        "chat has no shard, so its one hit is growth"
    );

    // A scoped update rewrites only its own shard and refuses growth.
    source(
        "mail",
        "fn a() { Color::WHITE; Color::WHITE; Color::WHITE; }\n",
    );
    let scoped = |unit: &str, allow_growth| Mode {
        update: true,
        scope: Some(unit.to_string()),
        allow_growth,
    };
    let refused = ratchet
        .run(&repo, &baselines, &scoped("apps/mail", false))
        .expect_err("an update may not raise a count");
    assert!(
        refused.contains("apps/mail/src/lib.rs [white]: 2 → 3"),
        "{refused}"
    );
    ratchet
        .run(&repo, &baselines, &scoped("apps/mail", true))
        .expect("growth is allowed when asked for");
    let mail = read_shards(&baselines.join("demo")).unwrap();
    assert_eq!(
        mail.get(&("apps/mail/src/lib.rs".to_string(), "white".to_string())),
        Some(&3)
    );
    assert!(
        !baselines.join("demo/apps-chat.txt").exists(),
        "a scoped update leaves other shards alone"
    );

    // Reaching zero deletes the shard, which then locks the unit at zero.
    source("mail", "fn a() {}\n");
    ratchet
        .run(&repo, &baselines, &scoped("apps-mail", false))
        .expect("lowering needs no permission");
    assert!(!baselines.join("demo/apps-mail.txt").exists());

    // An entry filed in the wrong shard is rejected.
    shard("apps-mail", "apps/chat/src/lib.rs white 1\n");
    let misfiled = check().expect_err("a misfiled entry is an error");
    assert!(
        misfiled.contains("belongs in the apps-chat shard"),
        "{misfiled}"
    );
}

#[test]
fn a_root_that_yields_no_file_fails_the_ratchet() {
    let tree = tempfile::tempdir().expect("temporary tree");
    let ratchet = Ratchet {
        name: "demo",
        roots: &["apps/*/src", "crates/session/shell/src"],
        patterns: vec![Pattern::new("x", "x", "")],
        exempt: no_exemptions,
        advice: "",
    };
    let failure = ratchet
        .run(tree.path(), &tree.path().join("b"), &Mode::default())
        .expect_err("an empty scan is not a pass");
    assert!(
        failure.contains("did not reach apps/*/src, crates/session/shell/src"),
        "{failure}"
    );
}
