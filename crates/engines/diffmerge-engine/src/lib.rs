//! The device's work for Diff & Merge: two texts aligned line by line, three texts merged into
//! hunks and composed back into one, two folder trees compared, and the files a person names read
//! and written.
//!
//! An app runs sandboxed and can reach no disk of its own, so this crate is what it reaches
//! through `engine.call`. It registers as the engine of `so.tilo.diffmerge` and answers that app
//! alone; every answer is JSON the package reads into its own records.
//!
//! Answers (`engine.call`):
//!
//! | op | asks | answers |
//! |---|---|---|
//! | `open` | `{path}` | `{label, text}` |
//! | `diff` | `{left, right}` | `{blocks, left, right, kinds, added, removed, modified, identical}` |
//! | `merge` | `{base, ours, theirs}` | `{hunks, merged, changes, conflicts}` |
//! | `compose` | `{base, ours, theirs, choices, written}` | `{merged, changes, conflicts}` |
//! | `compare-folders` | `{left, right}` | `{left_label, right_label, left_root, right_root, entries, truncated}` |
//! | `save` | `{path, text}` | `{name}` |
//! | `sample` | `{kind}` | the sample's inputs |

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::Path;

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
pub struct DiffmergeEngine;

impl DiffmergeEngine {
    #[must_use]
    pub fn new() -> Self {
        Self
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
        }
        let ask: Ask = read_ask(input)?;
        if !ask.address.is_empty() {
            let bytes = read_address(&ask.address)
                .ok_or_else(|| "The file is not on this device.".to_owned())?;
            let text = String::from_utf8(bytes).map_err(|_| "it is not UTF-8 text".to_owned())?;
            return Ok(json!({ "label": ask.name, "text": text }).to_string());
        }
        let path = Path::new(&ask.path);
        let text = read_text(path)?;
        let label = if ask.name.is_empty() {
            label_of(path)
        } else {
            ask.name
        };
        Ok(json!({ "label": label, "text": text }).to_string())
    }

    fn op_diff(input: &str) -> Result<String, String> {
        #[derive(Deserialize)]
        struct Ask {
            #[serde(default)]
            left: String,
            #[serde(default)]
            right: String,
        }
        let ask: Ask = read_ask(input)?;
        checked_size(&ask.left, &ask.right)?;
        let model = model::DiffModel::compute(&ask.left, &ask.right);
        let (left_aligned, right_aligned, rows) = aligned_text(&model);
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
        Ok(json!({
            "block_firsts": firsts,
            "block_lasts": lasts,
            "block_kinds": kinds,
            "block_previews": previews,
            "left": left_aligned,
            "right": right_aligned,
            "rows": rows,
            "added": model.added,
            "removed": model.removed,
            "modified": model.modified,
            "identical": model.is_identical(),
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

impl tilolang_tilo::engine::Provider for DiffmergeEngine {
    fn call(&self, op: &str, input: &str) -> Result<String, String> {
        match op {
            "open" => Self::op_open(input),
            "diff" => Self::op_diff(input),
            "merge" => Self::op_merge(input),
            "compose" => Self::op_compose(input),
            "compare-folders" => Self::op_compare_folders(input),
            "save" => Self::op_save(input),
            "sample" => Self::op_sample(input),
            other => Err(format!("`{other}` is no op this engine runs")),
        }
    }

    fn start(&self, _op: &str, _input: &str) -> Result<String, String> {
        Err("Diff & Merge asks no job of its device".to_owned())
    }

    fn status(&self, job: &str) -> Result<String, String> {
        Err(format!("no job `{job}` runs on this device"))
    }

    fn cancel(&self, _job: &str) -> bool {
        false
    }
}

/// Register Diff & Merge's engine for the session's lifetime.
pub fn register() {
    tilolang_tilo::engine::register(APP, std::sync::Arc::new(DiffmergeEngine::new()));
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

/// Each side as the aligned text a window shows: one line per aligned row, an empty line where
/// that side has none, so both columns stand level, and one character per row saying what the row
/// is (`' '` equal, `'a'` added, `'r'` removed, `'m'` modified).
fn aligned_text(model: &model::DiffModel) -> (String, String, String) {
    let mut left = String::new();
    let mut right = String::new();
    let mut kinds = String::with_capacity(model.rows.len());
    for (index, row) in model.rows.iter().enumerate() {
        if index > 0 {
            left.push('\n');
            right.push('\n');
        }
        if let Some(text) = &row.left {
            left.push_str(text);
        }
        if let Some(text) = &row.right {
            right.push_str(text);
        }
        kinds.push(match row.kind {
            model::RowKind::Equal => ' ',
            model::RowKind::Insert => 'a',
            model::RowKind::Delete => 'r',
            model::RowKind::Modify => 'm',
        });
    }
    (left, right, kinds)
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
fn read_text(path: &Path) -> Result<String, String> {
    let failed = |error: &dyn std::fmt::Display| format!("{}: {error}", path.display());
    let meta = std::fs::metadata(path).map_err(|error| failed(&error))?;
    if meta.is_dir() {
        return Err(format!("{} is a folder", path.display()));
    }
    let bytes = std::fs::read(path).map_err(|error| failed(&error))?;
    String::from_utf8(bytes).map_err(|_| format!("{}: not UTF-8 text", path.display()))
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

        phase the_aligned_texts_stand_level_and_name_their_own_lines;
        {
            let model = model::DiffModel::compute("a\nc\n", "a\nb\nc\n");
            let (left, right, kinds) = aligned_text(&model);
            assert_eq!(left.lines().count(), right.lines().count());
            assert_eq!(right, "a\nb\nc");
            assert_eq!(kinds, " a ");
            assert_eq!(preview(&model, 1, 1), "b");
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
            assert_eq!(read_text(&path).unwrap(), "a\nb\n");
            assert_eq!(label_of(&path), "merged.txt");
            std::fs::write(base.join("bytes.bin"), [0xff, 0xfe]).unwrap();
            assert!(read_text(&base.join("bytes.bin")).is_err());
            assert!(read_text(&base.join("missing.txt")).is_err());
            std::fs::remove_dir_all(&base).unwrap();
        }
    }
}
