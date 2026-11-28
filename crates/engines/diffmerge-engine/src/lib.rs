//! The device's work for Diff & Merge: two texts aligned line by line, three texts merged into
//! hunks and composed back into one, two folder trees compared, and the files a person names read
//! and written.
//!
//! An app runs sandboxed and can reach no disk of its own, so this crate is what it reaches
//! through `engine.call`, loaded as the engine library of `so.tilo.diffmerge`
//! (crates/engines/loadable/diffmerge). It answers that app alone; every answer is JSON the
//! package reads into its own records.
//!
//! Answers (`engine.call`):
//!
//! | op | asks | answers |
//! |---|---|---|
//! | `open` | `{path}` or `{address, name}`, `text` for a merge's side | `{label, address, length}`, and `text` when asked |
//! | `diff` | `{left_address, right_address}` | `{block_*, rows, added, removed, modified, identical}` |
//! | `rows` | `{left_address, right_address, first, count}` | `{left, right, left_numbers, right_numbers, kinds, rows}` |
//! | `merge` | `{base, ours, theirs}` | `{hunks, merged, changes, conflicts}` |
//! | `compose` | `{base, ours, theirs, choices, written}` | `{merged, changes, conflicts}` |
//! | `compare-folders` | `{left, right}` | `{left_label, right_label, left_root, right_root, entries, truncated}` |
//! | `save` | `{path, text}` | `{name}` |
//! | `sample` | `{kind}` | the sample's inputs |

use std::collections::{BTreeMap, VecDeque};
use std::io::Write as _;
use std::path::Path;
use std::sync::Mutex;

use serde::Deserialize;
use serde_json::{Value as Json, json};

pub mod folder;
pub mod model;
pub mod samples;

/// The app this engine answers.
pub const APP: &str = "so.tilo.diffmerge";

/// The most two files may hold together to be compared: the alignment is held by the document, as
/// both sides are.
pub const MAX_COMPARE_BYTES: usize = 8 * 1024 * 1024;

/// The engine of Diff & Merge, as the session keeps it for the app.
#[derive(Default)]
pub struct DiffmergeEngine {
    /// The alignments made, newest last, by the pair of addresses they were made of: a window
    /// reads its rows from here, so scrolling reads no file again.
    alignments: Mutex<VecDeque<(String, String, std::sync::Arc<model::DiffModel>)>>,
}

impl DiffmergeEngine {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The alignment of the two sides at `left` and `right`, as the window reads it: made once
    /// for a pair of addresses, from the bytes the content store holds at them. An empty address
    /// is a side with no file.
    fn alignment(
        &self,
        left: &str,
        right: &str,
    ) -> Result<std::sync::Arc<model::DiffModel>, String> {
        let held = {
            let alignments = self
                .alignments
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            alignments
                .iter()
                .rev()
                .find(|(one, other, _)| one == left && other == right)
                .map(|(_, _, model)| std::sync::Arc::clone(model))
        };
        if let Some(model) = held {
            return Ok(model);
        }
        // The files are read with the cache open to no one: two windows may ask at once.
        let left_text = side_text(left)?;
        let right_text = side_text(right)?;
        let model = model::comparison(&left_text, &right_text);
        let mut alignments = self
            .alignments
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if alignments.len() >= 2 {
            alignments.pop_front();
        }
        alignments.push_back((
            left.to_owned(),
            right.to_owned(),
            std::sync::Arc::clone(&model),
        ));
        drop(alignments);
        Ok(model)
    }

    fn op_open(input: &str) -> Result<String, String> {
        #[derive(Deserialize)]
        struct Ask {
            #[serde(default)]
            path: String,
            #[serde(default)]
            address: String,
            #[serde(default)]
            name: String,
            /// Whether the answer carries the text as well as its address: a merge needs the
            /// text, a comparison reads it address by address.
            #[serde(default)]
            text: bool,
        }
        let ask: Ask = read_ask(input)?;
        let (bytes, label) = if ask.address.is_empty() {
            if !ask.path.is_empty() {
                absolute(&ask.path)?;
            }
            let path = Path::new(&ask.path);
            let label = if ask.name.is_empty() {
                label_of(path)
            } else {
                ask.name
            };
            (read_bytes(path)?, label)
        } else {
            let bytes = read_address(&ask.address)
                .ok_or_else(|| "The file is not on this device.".to_owned())?;
            (bytes, ask.name)
        };
        let address = keep(&bytes)?;
        let mut answer = json!({
            "label": label,
            "address": address,
            "length": bytes.len(),
        });
        if ask.text {
            let text = String::from_utf8(bytes).map_err(|_| "it is not UTF-8 text".to_owned())?;
            if let Some(fields) = answer.as_object_mut() {
                fields.insert("text".to_owned(), json!(text));
            }
        }
        Ok(answer.to_string())
    }

    fn op_diff(&self, input: &str) -> Result<String, String> {
        #[derive(Deserialize)]
        struct Ask {
            /// The address of each side's bytes in the content store; empty for a side with no
            /// file.
            #[serde(default)]
            left_address: String,
            #[serde(default)]
            right_address: String,
        }
        let ask: Ask = read_ask(input)?;
        let model = self.alignment(&ask.left_address, &ask.right_address)?;
        let mut firsts = Vec::with_capacity(model.blocks.len());
        let mut lasts = Vec::with_capacity(model.blocks.len());
        let mut kinds = Vec::with_capacity(model.blocks.len());
        let mut previews = Vec::with_capacity(model.blocks.len());
        for block in &model.blocks {
            firsts.push(block.first_row);
            lasts.push(block.last_row);
            kinds.push(block_word(block.kind));
            previews.push(preview(&model, block.first_row, block.last_row));
        }
        let row_kinds: String = model
            .rows
            .iter()
            .map(|row| match row.kind {
                model::RowKind::Equal => ' ',
                model::RowKind::Insert => 'a',
                model::RowKind::Delete => 'r',
                model::RowKind::Modify => 'm',
            })
            .collect();
        Ok(json!({
            "block_firsts": firsts,
            "block_lasts": lasts,
            "block_kinds": kinds,
            "block_previews": previews,
            "kinds": row_kinds,
            "rows": model.rows.len(),
            "added": model.added,
            "removed": model.removed,
            "modified": model.modified,
            "identical": model.is_identical(),
        })
        .to_string())
    }

    /// The rows `first` to `first + count` of the alignment, as the window shows them: each
    /// side's line (an empty line where it has none), the row's number on each side, and one
    /// character per row saying what it is.
    fn op_rows(&self, input: &str) -> Result<String, String> {
        #[derive(Deserialize)]
        struct Ask {
            #[serde(default)]
            left_address: String,
            #[serde(default)]
            right_address: String,
            #[serde(default)]
            first: usize,
            #[serde(default)]
            count: usize,
        }
        let ask: Ask = read_ask(input)?;
        let model = self.alignment(&ask.left_address, &ask.right_address)?;
        let window = window_of(&model, ask.first, ask.count.min(4096));
        Ok(json!({
            "left": window.left,
            "right": window.right,
            "left_numbers": window.left_numbers,
            "right_numbers": window.right_numbers,
            "kinds": window.kinds,
            "rows": model.rows.len(),
        })
        .to_string())
    }

    fn op_merge(input: &str) -> Result<String, String> {
        #[derive(Deserialize)]
        struct Ask {
            #[serde(default)]
            base: String,
            #[serde(default)]
            ours: String,
            #[serde(default)]
            theirs: String,
        }
        let ask: Ask = read_ask(input)?;
        checked_size(&ask.base, &format!("{}{}", ask.ours, ask.theirs))?;
        let model = model::MergeModel::compute(&ask.base, &ask.ours, &ask.theirs);
        let mut answer = hunks_json(&model);
        let fields = answer
            .as_object_mut()
            .ok_or_else(|| "the answer is no object".to_owned())?;
        fields.insert("merged".to_owned(), json!(model.merged_text()));
        fields.insert("changes".to_owned(), json!(model.change_count()));
        fields.insert("conflicts".to_owned(), json!(model.conflict_count()));
        Ok(answer.to_string())
    }

    fn op_compose(input: &str) -> Result<String, String> {
        #[derive(Deserialize)]
        struct Ask {
            #[serde(default)]
            base: String,
            #[serde(default)]
            ours: String,
            #[serde(default)]
            theirs: String,
            #[serde(default)]
            choices: Vec<String>,
            #[serde(default)]
            written: Vec<String>,
        }
        let ask: Ask = read_ask(input)?;
        checked_size(&ask.base, &format!("{}{}", ask.ours, ask.theirs))?;
        let model = model::MergeModel::compute(&ask.base, &ask.ours, &ask.theirs);
        let composed = composed(&model, &ask.choices, &ask.written)?;
        Ok(json!({
            "merged": composed.text,
            "changes": composed.changes,
            "conflicts": composed.conflicts,
        })
        .to_string())
    }

    fn op_compare_folders(input: &str) -> Result<String, String> {
        #[derive(Deserialize)]
        struct Ask {
            left: String,
            right: String,
        }
        let ask: Ask = read_ask(input)?;
        absolute(&ask.left)?;
        absolute(&ask.right)?;
        let comparison = folder::compare(Path::new(&ask.left), Path::new(&ask.right))?;
        Ok(comparison.to_json().to_string())
    }

    fn op_save(input: &str) -> Result<String, String> {
        #[derive(Deserialize)]
        struct Ask {
            path: String,
            text: String,
        }
        let ask: Ask = read_ask(input)?;
        absolute(&ask.path)?;
        let path = Path::new(&ask.path);
        write_atomic(path, ask.text.as_bytes())?;
        Ok(json!({ "name": label_of(path) }).to_string())
    }

    fn op_sample(input: &str) -> Result<String, String> {
        #[derive(Deserialize)]
        struct Ask {
            kind: String,
        }
        let ask: Ask = read_ask(input)?;
        samples::sample(&ask.kind)
    }
}

impl DiffmergeEngine {
    /// One request answered now, as the app asks it.
    ///
    /// # Errors
    ///
    /// The ask is not one this engine reads, or the work it names cannot be done.
    pub fn call(&self, op: &str, input: &str) -> Result<String, String> {
        match op {
            "open" => Self::op_open(input),
            "diff" => self.op_diff(input),
            "rows" => self.op_rows(input),
            "merge" => Self::op_merge(input),
            "compose" => Self::op_compose(input),
            "compare-folders" => Self::op_compare_folders(input),
            "save" => Self::op_save(input),
            "sample" => Self::op_sample(input),
            other => Err(format!("`{other}` is no op this engine runs")),
        }
    }

    /// No ask of this engine runs as a job.
    ///
    /// # Errors
    ///
    /// Every op, since none runs as work a person waits for or stops.
    pub fn start(&self, _op: &str, _input: &str) -> Result<String, String> {
        Err("Diff & Merge asks no job of its device".to_owned())
    }

    /// No ask of this engine runs as a job.
    ///
    /// # Errors
    ///
    /// Every job, since this engine starts none.
    pub fn status(&self, job: &str) -> Result<String, String> {
        Err(format!("no job `{job}` runs on this device"))
    }

    /// No ask of this engine runs as a job, so there is none to stop.
    #[must_use]
    pub fn cancel(&self, _job: &str) -> bool {
        false
    }
}

/// The changes of a merge as the app reads them: one list per fact of a change, so a change is a
/// place among them. A clean run is no change and is not listed: the app shows what a person
/// decides about.
fn hunks_json(model: &model::MergeModel) -> Json {
    let mut indices = Vec::new();
    let mut resolutions = Vec::new();
    let mut bases = Vec::new();
    let mut ours = Vec::new();
    let mut theirs = Vec::new();
    for (index, hunk) in model.hunks.iter().enumerate() {
        if !hunk.is_change {
            continue;
        }
        indices.push(index);
        resolutions.push(resolution_word(hunk.resolution));
        bases.push(hunk.base.join("\n"));
        ours.push(hunk.ours.join("\n"));
        theirs.push(hunk.theirs.join("\n"));
    }
    json!({
        "hunk_indices": indices,
        "hunk_resolutions": resolutions,
        "hunk_bases": bases,
        "hunk_ours": ours,
        "hunk_theirs": theirs,
    })
}

/// The merged result and what it still asks of the person, with the choices the app recorded:
/// each `choice` is `"<hunk>:<ours|theirs|both>"`, each `written` is `"<hunk>:<text>"`.
fn composed(
    model: &model::MergeModel,
    choices: &[String],
    written: &[String],
) -> Result<Composed, String> {
    let mut chosen: BTreeMap<usize, model::Resolution> = BTreeMap::new();
    for choice in choices {
        let (hunk, word) = choice
            .split_once(':')
            .ok_or_else(|| format!("`{choice}` is not a hunk and its choice"))?;
        let hunk: usize = hunk
            .parse()
            .map_err(|_| format!("`{choice}` does not name a hunk by its number"))?;
        let resolution =
            choice_of(word).ok_or_else(|| format!("`{word}` is not a side of a hunk"))?;
        chosen.insert(hunk, resolution);
    }
    let mut texts: BTreeMap<usize, String> = BTreeMap::new();
    for entry in written {
        let (hunk, text) = entry
            .split_once(':')
            .ok_or_else(|| format!("`{entry}` is not a hunk and its text"))?;
        let hunk: usize = hunk
            .parse()
            .map_err(|_| format!("`{entry}` does not name a hunk by its number"))?;
        texts.insert(hunk, text.to_owned());
    }
    let mut composed = model.clone();
    for (index, hunk) in composed.hunks.iter_mut().enumerate() {
        if !hunk.is_change {
            continue;
        }
        if let Some(text) = texts.get(&index) {
            hunk.resolution = model::Resolution::Written;
            hunk.written = text.lines().map(str::to_owned).collect();
        } else if let Some(resolution) = chosen.get(&index) {
            hunk.resolution = *resolution;
        }
    }
    Ok(Composed {
        text: composed.merged_text(),
        changes: composed.change_count(),
        conflicts: composed.conflict_count(),
    })
}

struct Composed {
    text: String,
    changes: usize,
    conflicts: usize,
}

fn choice_of(word: &str) -> Option<model::Resolution> {
    match word {
        "ours" => Some(model::Resolution::TookOurs),
        "theirs" => Some(model::Resolution::TookTheirs),
        "both" => Some(model::Resolution::TookBoth),
        _ => None,
    }
}

/// The word a hunk's resolution reads as: `unchanged` for a clean run, `auto-ours` and
/// `auto-theirs` for a side only one moved, `conflict` while the person has not said, and
/// `ours`, `theirs`, `both` or `written` for what they said.
fn resolution_word(resolution: model::Resolution) -> &'static str {
    match resolution {
        model::Resolution::Unchanged => "unchanged",
        model::Resolution::Ours => "auto-ours",
        model::Resolution::Theirs => "auto-theirs",
        model::Resolution::TookOurs => "ours",
        model::Resolution::TookTheirs => "theirs",
        model::Resolution::TookBoth => "both",
        model::Resolution::Written => "written",
        model::Resolution::Conflict => "conflict",
    }
}

fn block_word(kind: model::BlockKind) -> &'static str {
    match kind {
        model::BlockKind::Added => "added",
        model::BlockKind::Removed => "removed",
        model::BlockKind::Modified => "modified",
    }
}

/// The first line of what a change block touches, as its row in a list says it.
fn preview(model: &model::DiffModel, first: usize, last: usize) -> String {
    let rows = model.rows.get(first..=last).unwrap_or(&[]);
    let line = rows
        .iter()
        .flat_map(|row| [row.left.as_deref(), row.right.as_deref()])
        .flatten()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("");
    let joined: String = line.split_whitespace().collect::<Vec<_>>().join(" ");
    if joined.chars().count() > 44 {
        joined.chars().take(41).collect::<String>() + "…"
    } else {
        joined
    }
}

/// What one window of an alignment shows: each side's line for every row it covers (an empty
/// line where that side has none, so both columns stand level), each side's line number, and one
/// character per row saying what it is (`' '` equal, `'a'` added, `'r'` removed, `'m'` modified).
struct Window {
    left: Vec<String>,
    right: Vec<String>,
    left_numbers: Vec<String>,
    right_numbers: Vec<String>,
    kinds: String,
}

/// The rows `first` to `first + count` of `model`.
fn window_of(model: &model::DiffModel, first: usize, count: usize) -> Window {
    let end = first.saturating_add(count).min(model.rows.len());
    let rows = model.rows.get(first..end).unwrap_or_default();
    let mut window = Window {
        left: Vec::with_capacity(rows.len()),
        right: Vec::with_capacity(rows.len()),
        left_numbers: Vec::with_capacity(rows.len()),
        right_numbers: Vec::with_capacity(rows.len()),
        kinds: String::with_capacity(rows.len()),
    };
    for row in rows {
        window.left.push(row.left.clone().unwrap_or_default());
        window.right.push(row.right.clone().unwrap_or_default());
        window
            .left_numbers
            .push(row.left_no.map_or_else(String::new, |no| no.to_string()));
        window
            .right_numbers
            .push(row.right_no.map_or_else(String::new, |no| no.to_string()));
        window.kinds.push(match row.kind {
            model::RowKind::Equal => ' ',
            model::RowKind::Insert => 'a',
            model::RowKind::Delete => 'r',
            model::RowKind::Modify => 'm',
        });
    }
    window
}

/// Refuses a path that names no place whole: a relative one would read from wherever the session
/// happens to stand, which is neither the person's choice nor the same on another device.
fn absolute(path: &str) -> Result<(), String> {
    if Path::new(path).is_absolute() {
        Ok(())
    } else {
        Err(format!(
            "`{path}` is not an absolute path; give the whole path, like /home/ada/notes.txt"
        ))
    }
}

/// Refuses two texts whose bytes together are past what a document holds well.
fn checked_size(first: &str, second: &str) -> Result<(), String> {
    let total = first.len().saturating_add(second.len());
    if total > MAX_COMPARE_BYTES {
        return Err(format!(
            "the files are larger than {} MiB together",
            MAX_COMPARE_BYTES / (1024 * 1024)
        ));
    }
    Ok(())
}

/// The bytes of a `tilo-blob://` address, when this device holds them. The language names a
/// file's content so; the content store names the same bytes by the reference under it.
fn read_address(address: &str) -> Option<Vec<u8>> {
    let reference = address.strip_prefix("tilo-blob://").unwrap_or(address);
    tilo_resource::content::read_hash(reference).ok()
}

fn read_ask<T: serde::de::DeserializeOwned>(input: &str) -> Result<T, String> {
    serde_json::from_str(input).map_err(|error| format!("the ask does not read: {error}"))
}

/// The name a path is shown by: its file name, or the path itself.
fn label_of(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// Reads a file as UTF-8 text, refusing one that is not text.
/// A file's bytes, refused where it is no file a person reads as text.
fn read_bytes(path: &Path) -> Result<Vec<u8>, String> {
    let failed = |error: &dyn std::fmt::Display| format!("{}: {error}", path.display());
    let meta = std::fs::metadata(path).map_err(|error| failed(&error))?;
    if meta.is_dir() {
        return Err(format!("{} is a folder", path.display()));
    }
    std::fs::read(path).map_err(|error| failed(&error))
}

/// The text one side of a comparison holds at `address`, in the content store; empty for a side
/// with no file.
fn side_text(address: &str) -> Result<String, String> {
    if address.is_empty() {
        return Ok(String::new());
    }
    let bytes =
        read_address(address).ok_or_else(|| "The file is not on this device.".to_owned())?;
    String::from_utf8(bytes).map_err(|_| "it is not UTF-8 text".to_owned())
}

/// Bytes kept where every device of the workspace reads them by their address.
fn keep(bytes: &[u8]) -> Result<String, String> {
    let store = tilo_resource::content::current()
        .ok_or_else(|| "This device keeps no content store for the window.".to_owned())?;
    let reference = store.put(bytes).map_err(|error| error.to_string())?;
    Ok(format!("tilo-blob://{reference}"))
}

/// Write `bytes` to `path` whole or not at all: a temporary file beside it, then a rename.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let folder = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path.file_name().map_or_else(
        || "merged.txt".into(),
        |name| name.to_string_lossy().into_owned(),
    );
    let temporary = folder.join(format!(".{name}.{}", std::process::id()));
    {
        let mut file = std::fs::File::create(&temporary).map_err(|error| error.to_string())?;
        file.write_all(bytes).map_err(|error| error.to_string())?;
        file.sync_all().map_err(|error| error.to_string())?;
    }
    std::fs::rename(&temporary, path).map_err(|error| {
        let _ = std::fs::remove_file(&temporary);
        error.to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    tilo_test_env::scenario! {
        fn the_engine_composes_the_sides_a_person_chose;

        phase a_choice_and_a_written_text_compose_into_the_merged_result;
        {
            let base = "a\nb\nc\n";
            let ours = "a\nOURS\nc\n";
            let theirs = "a\nTHEIRS\nc\n";
            let model = model::MergeModel::compute(base, ours, theirs);
            let conflict = model
                .hunks
                .iter()
                .position(|h| h.resolution.is_conflict())
                .expect("a conflict");
            let took = composed(&model, &[format!("{conflict}:theirs")], &[]).unwrap();
            assert_eq!(took.text, "a\nTHEIRS\nc\n");
            assert_eq!(took.conflicts, 0);
            let both = composed(&model, &[], &[format!("{conflict}:mine")]).unwrap();
            assert_eq!(both.text, "a\nmine\nc\n");
            assert!(composed(&model, &[format!("{conflict}:sideways")], &[]).is_err());
            assert!(composed(&model, &["not-a-hunk:ours".into()], &[]).is_err());
        }

        phase a_window_stands_level_and_names_each_rows_own_line;
        {
            let model = model::DiffModel::compute("a\nc\n", "a\nb\nc\n");
            let window = window_of(&model, 0, 96);
            assert_eq!(window.left, ["a", "", "c"]);
            assert_eq!(window.right, ["a", "b", "c"]);
            assert_eq!(window.left_numbers, ["1", "", "2"]);
            assert_eq!(window.right_numbers, ["1", "2", "3"]);
            assert_eq!(window.kinds, " a ");
            assert_eq!(preview(&model, 1, 1), "b");
            // The window asks for no more than the alignment holds.
            let tail = window_of(&model, 2, 96);
            assert_eq!(tail.kinds, " ");
            assert!(window_of(&model, 9, 96).left.is_empty());
        }

        phase a_pair_of_twenty_megabytes_is_a_window_like_any_other;
        {
            // Two 20 MB files that differ by one line: the alignment is rows, and a window of it
            // costs what the window shows, not what the files hold.
            let line = "the garden planner writes its beds here\n";
            let left = line.repeat(500_000);
            let right = left.replacen(line, "the garden planner writes its beds there\n", 1);
            let model = model::comparison(&left, &right);
            assert_eq!(model.left_lines, 500_000);
            assert!(!model.blocks.is_empty(), "the one changed line is a change");
            let first = 499_990;
            let window = window_of(&model, first, 96);
            assert_eq!(window.left.len(), 10);
            assert_eq!(window.right.len(), 10);
            let changed = model
                .rows
                .iter()
                .position(|row| row.kind != model::RowKind::Equal)
                .expect("a changed row");
            let at = window_of(&model, changed, 3);
            assert!(!at.kinds.trim().is_empty(), "{:?}", at.kinds);
        }

        phase a_relative_path_reads_nothing_and_writes_nothing;
        {
            assert!(absolute("notes.txt").is_err());
            assert!(absolute("./notes.txt").is_err());
            assert!(absolute("/home/ada/notes.txt").is_ok());
            let refused = DiffmergeEngine::op_open(&json!({ "path": "notes.txt" }).to_string())
                .expect_err("a relative path is no place on this device");
            assert!(refused.contains("absolute"), "{refused}");
            let refused = DiffmergeEngine::op_save(
                &json!({ "path": "merged.txt", "text": "a\n" }).to_string(),
            )
            .expect_err("a relative path is no place on this device");
            assert!(refused.contains("absolute"), "{refused}");
            let refused = DiffmergeEngine::op_compare_folders(
                &json!({ "left": "/home/ada/a", "right": "b" }).to_string(),
            )
            .expect_err("a relative path is no place on this device");
            assert!(refused.contains("absolute"), "{refused}");
        }

        phase a_file_is_read_as_text_and_written_back_whole;
        {
            let base = std::env::temp_dir().join(format!(
                "tilo-diffmerge-engine-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&base);
            std::fs::create_dir_all(&base).unwrap();
            let path = base.join("merged.txt");
            write_atomic(&path, "a\nb\n".as_bytes()).unwrap();
            assert_eq!(read_bytes(&path).unwrap(), b"a\nb\n");
            assert_eq!(label_of(&path), "merged.txt");
            std::fs::write(base.join("bytes.bin"), [0xff, 0xfe]).unwrap();
            assert!(read_bytes(&base.join("bytes.bin")).is_ok(), "bytes are bytes");
            assert!(String::from_utf8(read_bytes(&base.join("bytes.bin")).unwrap()).is_err());
            assert!(read_bytes(&base.join("missing.txt")).is_err());
            std::fs::remove_dir_all(&base).unwrap();
        }
    }
}
