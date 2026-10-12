//! Qualify native recorded-project history against an explicit on-disk root.
//! See `../SOURCE_HISTORY.md` for the deliberately limited qualification scope.

use code_editor::CodeEditorShell;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fs;
use std::io::Read as _;
use std::path::{Component, Path, PathBuf};
use ui::framework::testing::TestApp;

type App = TestApp<CodeEditorShell>;
type Result<T> = std::result::Result<T, Box<dyn Error>>;
const READ_CAP: u64 = 200 * 1024;

struct Options {
    root: PathBuf,
    exclusions: Vec<PathBuf>,
    focused: bool,
}

#[derive(Debug, PartialEq, Eq)]
struct DiskEntry {
    kind: &'static str,
    bytes: u64,
    mode: u32,
    hash: String,
    text: Option<String>,
}

fn options() -> Result<Options> {
    let mut args = std::env::args().skip(1);
    let mut root = None;
    let mut exclusions = Vec::new();
    let mut focused = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--root" if root.is_none() => {
                root = Some(PathBuf::from(args.next().ok_or("--root needs a path")?));
            }
            "--exclude" => {
                let path = PathBuf::from(args.next().ok_or("--exclude needs a relative path")?);
                if path.as_os_str().is_empty()
                    || path
                        .components()
                        .any(|part| !matches!(part, Component::Normal(_)))
                {
                    return Err(
                        "exclusions must be nonempty root-relative paths, without . or ..".into(),
                    );
                }
                exclusions.push(path);
            }
            "--focused-text" => focused = true,
            _ => {
                return Err(format!(
                    "unknown argument {arg}; use --root PATH [--exclude RELATIVE] [--focused-text]"
                )
                .into());
            }
        }
    }
    let root =
        root.ok_or("--root is required; there is no fixture or current-directory default")?;
    if !fs::symlink_metadata(&root)?.is_dir() {
        return Err("--root must be a real directory, not a symlink".into());
    }
    let root = fs::canonicalize(root)?;
    for exclusion in &exclusions {
        fs::symlink_metadata(root.join(exclusion))?;
    }
    exclusions.sort();
    exclusions.dedup();
    Ok(Options {
        root,
        exclusions,
        focused,
    })
}

fn excluded(path: &Path, options: &Options) -> bool {
    options
        .exclusions
        .iter()
        .any(|entry| path.starts_with(entry))
}

#[cfg(unix)]
fn mode(metadata: &fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt as _;
    metadata.permissions().mode()
}

#[cfg(not(unix))]
fn mode(_metadata: &fs::Metadata) -> u32 {
    0
}

fn disk(options: &Options) -> Result<BTreeMap<String, DiskEntry>> {
    let root_metadata = fs::symlink_metadata(&options.root)?;
    let mut entries = BTreeMap::from([(
        String::new(),
        DiskEntry {
            kind: "folder",
            bytes: 0,
            mode: mode(&root_metadata),
            hash: String::new(),
            text: None,
        },
    )]);
    let mut pending = vec![PathBuf::new()];
    let mut text_bytes = 0usize;
    while let Some(directory) = pending.pop() {
        for child in fs::read_dir(options.root.join(&directory))? {
            let child = child?;
            let relative = directory.join(child.file_name());
            if excluded(&relative, options) {
                continue;
            }
            let path = relative
                .to_str()
                .ok_or("non-UTF-8 source path is unsupported")?
                .to_owned();
            let metadata = fs::symlink_metadata(child.path())?;
            let mut entry = DiskEntry {
                kind: "unsupported",
                bytes: metadata.len(),
                mode: mode(&metadata),
                hash: String::new(),
                text: None,
            };
            if metadata.is_symlink() {
                entry.kind = "symlink";
                fs::read_link(child.path())?
                    .to_str()
                    .ok_or("non-UTF-8 symlink target")?
                    .clone_into(&mut entry.hash);
            } else if metadata.is_dir() {
                entry.kind = "folder";
                // Directory size is not source content and varies by filesystem.
                entry.bytes = 0;
                pending.push(relative);
            } else if metadata.is_file() {
                entry.kind = "oversized";
                if metadata.len() <= READ_CAP {
                    let mut bytes = Vec::new();
                    fs::File::open(child.path())?
                        .take(READ_CAP + 1)
                        .read_to_end(&mut bytes)?;
                    if bytes.len() as u64 > READ_CAP {
                        return Err(format!("source changed during audit: {path}").into());
                    }
                    entry.hash = hex::encode(Sha256::digest(&bytes));
                    entry.text = String::from_utf8(bytes)
                        .ok()
                        .filter(|text| !text.contains('\0'));
                    text_bytes =
                        text_bytes.saturating_add(entry.text.as_ref().map_or(0, String::len));
                    if text_bytes > 64 * 1024 * 1024 {
                        return Err("qualification audit exceeds 64 MiB of text; narrow the root or exclusions".into());
                    }
                    entry.kind = if entry.text.is_some() {
                        "file"
                    } else {
                        "binary"
                    };
                }
            }
            entries.insert(path, entry);
        }
    }
    Ok(entries)
}

fn answer(app: &mut App, action: &str, args: Value) -> Result<Value> {
    app.call(action, args)?
        .ok_or_else(|| format!("{action} returned no observable result").into())
}

fn inventory(app: &mut App, version: &str) -> Result<Vec<Value>> {
    let mut entries = Vec::new();
    let mut item = Some("inventory:0".to_owned());
    while let Some(page) = item {
        let read = answer(app, "read", json!({"version": version, "item": page}))?;
        let content = read.get("content").ok_or("typed read omitted content")?;
        if content["scope"] != "recorded-project" || content["complete"] != true {
            return Err("typed read must identify a complete recorded-project inventory (not whole-source coverage)".into());
        }
        entries.extend(
            content["entries"]
                .as_array()
                .ok_or("typed read omitted entries")?
                .iter()
                .cloned(),
        );
        item = content["next"].as_str().map(str::to_owned);
    }
    Ok(entries)
}

fn relative<'a>(entry: &'a Value, options: &Options) -> Result<&'a str> {
    let path = entry["path"].as_str().ok_or("entry has no path")?;
    let root_name = options
        .root
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("root needs a UTF-8 directory name")?;
    if path == root_name {
        return Ok("");
    }
    path.strip_prefix(root_name)
        .and_then(|tail| tail.strip_prefix('/'))
        .ok_or_else(|| format!("recorded path is not under the launched root: {path}").into())
}

fn audit(
    entries: &[Value],
    baseline: &BTreeMap<String, DiskEntry>,
    options: &Options,
) -> Result<Vec<String>> {
    let mut recorded = BTreeSet::new();
    let mut addresses = BTreeSet::new();
    let mut ids = BTreeSet::new();
    for entry in entries {
        let path = relative(entry, options)?;
        if !recorded.insert(path.to_owned()) {
            return Err(format!("duplicate recorded path: {path}").into());
        }
        let id = entry["id"].as_str().ok_or("entry has no id")?;
        let address = entry["resource_id"]
            .as_str()
            .ok_or("entry has no resource_id")?;
        if !ids.insert(id) || !addresses.insert(address) {
            return Err("inventory repeats an id or resource address".into());
        }
        if excluded(Path::new(path), options) {
            continue;
        }
        let expected = baseline
            .get(path)
            .ok_or_else(|| format!("unexpected recorded path: {path}"))?;
        if (expected.kind == "file" || expected.kind == "folder") && entry["kind"] != expected.kind
        {
            return Err(format!("kind mismatch: {path}").into());
        }
    }
    Ok(baseline
        .keys()
        .filter(|path| !recorded.contains(*path))
        .cloned()
        .collect())
}

fn item_text(app: &mut App, version: &str, entry: &Value) -> Result<String> {
    let item = entry["resource_id"]
        .as_str()
        .ok_or("missing item resource address")?;
    let read = answer(app, "read", json!({"version": version, "item": item}))?;
    Ok(read
        .get("content")
        .and_then(|content| content.get("text"))
        .and_then(Value::as_str)
        .ok_or("item typed read has no text")?
        .to_owned())
}

fn exercise(
    app: &mut App,
    entries: &[Value],
    baseline: &BTreeMap<String, DiskEntry>,
    options: &Options,
) -> Result<()> {
    for entry in entries {
        let path = relative(entry, options)?;
        let Some(expected) = baseline.get(path).and_then(|entry| entry.text.as_ref()) else {
            continue;
        };
        if entry["text_available"] != true
            || item_text(app, "qualification-baseline", entry)? != *expected
        {
            return Err(format!("imported text differs from disk: {path}").into());
        }
    }
    let selected = entries
        .iter()
        .find(|entry| {
            relative(entry, options)
                .ok()
                .and_then(|path| baseline.get(path))
                .is_some_and(|entry| entry.text.is_some())
        })
        .ok_or("focused workload has no included text file")?;
    let path = relative(selected, options)?;
    let original = baseline[path]
        .text
        .as_ref()
        .ok_or("selected file has no text")?;
    // Keep the mutation small and inside the typed read bound.
    let changed = if original.is_empty() {
        "qualification\n".to_owned()
    } else {
        String::new()
    };
    let address = selected["resource_id"]
        .as_str()
        .ok_or("missing selected resource address")?;
    app.call_on(address, "replace-text", json!({"text": changed}))?;
    app.call("checkpoint", json!({"name": "qualification-edited"}))?;
    if item_text(app, "qualification-edited", selected)? != changed
        || item_text(app, "qualification-baseline", selected)? != *original
    {
        return Err("historical typed reads do not preserve both versions".into());
    }
    let diff = answer(
        app,
        "diff",
        json!({"from": "qualification-baseline", "to": "qualification-edited"}),
    )?;
    let restore_path = format!(
        "buffers/{}",
        selected["id"].as_str().ok_or("selected entry has no id")?
    );
    if !diff
        .get("items")
        .and_then(Value::as_array)
        .ok_or("diff has no items")?
        .iter()
        .any(|item| item["item"] == restore_path)
    {
        return Err("native diff omitted the edited buffer".into());
    }
    let reviewed = answer(
        app,
        "review-restore",
        json!({"version": "qualification-baseline", "paths": [restore_path]}),
    )?;
    let review = reviewed.get("review").ok_or("restore review missing")?;
    let restored = answer(app, "restore-reviewed", json!({"review": review}))?;
    if restored.get("restored") != Some(&json!([restore_path])) {
        return Err("restore did not report the reviewed buffer selection".into());
    }
    app.call("checkpoint", json!({"name": "qualification-restored"}))?;
    for entry in entries {
        let path = relative(entry, options)?;
        if let Some(expected) = baseline.get(path).and_then(|entry| entry.text.as_ref())
            && item_text(app, "qualification-restored", entry)? != *expected
        {
            return Err(format!("restored text differs: {path}").into());
        }
    }
    eprintln!("native checkpoint/read/diff/reviewed-restore passed for {path}");
    Ok(())
}

fn main() -> Result<()> {
    let started = std::time::Instant::now();
    let options = options()?;
    eprintln!(
        "root={} exclusions={:?} scope={}",
        options.root.display(),
        options.exclusions,
        if options.focused {
            "focused-text; NOT whole-source qualification"
        } else {
            "whole-source requested"
        }
    );
    let baseline = disk(&options)?;
    let mut app = App::launched(&[options.root.to_str().ok_or("root is not UTF-8")?])?;
    app.call("checkpoint", json!({"name": "qualification-launch"}))?;
    let initial = inventory(&mut app, "qualification-launch")?;
    let missing = audit(&initial, &baseline, &options)?;
    eprintln!(
        "launch importer: disk_entries={} recorded_entries={} missing={missing:?}",
        baseline.len(),
        initial.len()
    );
    let unsupported: Vec<_> = baseline
        .iter()
        .filter(|(_, entry)| !matches!(entry.kind, "file" | "folder"))
        .map(|(path, entry)| format!("{path}: {} ({} bytes)", entry.kind, entry.bytes))
        .collect();
    eprintln!("unsupported included entries={unsupported:?}");
    if !options.focused {
        return Err("whole-source qualification UNSUPPORTED: native Project records text and tree metadata, not binary bytes, POSIX modes or symlink targets; lazy import is not a whole-source snapshot. Use explicit --focused-text only for a labeled text workload".into());
    }
    if !unsupported.is_empty() {
        return Err("focused-text refuses binary, oversized (>200 KiB), symlink and special entries; exclude them explicitly or narrow --root".into());
    }
    // Hydrate through the app's published disk effect, never by seeding its document.
    for (path, entry) in &baseline {
        if entry.kind == "file" {
            app.call(
                "open-file",
                json!({"path": options.root.join(path).to_str().ok_or("file path not UTF-8")?}),
            )?;
        }
    }
    app.call("checkpoint", json!({"name": "qualification-baseline"}))?;
    eprintln!("hydration_elapsed_ms={}", started.elapsed().as_millis());
    let entries = inventory(&mut app, "qualification-baseline")?;
    let missing = audit(&entries, &baseline, &options)?;
    if !missing.is_empty() {
        return Err(format!("importer incomplete after explicit file opens (including nested empty directories): {missing:?}").into());
    }
    exercise(&mut app, &entries, &baseline, &options)?;
    if disk(&options)? != baseline {
        return Err(
            "included disk tree changed: text history must not silently write files or modes"
                .into(),
        );
    }
    if !app.harness().native_app_audit().is_certified() {
        return Err("native ResourceApp custody audit failed".into());
    }
    eprintln!("total_elapsed_ms={}", started.elapsed().as_millis());
    eprintln!(
        "PASS focused-text: {} included disk entries; disk unchanged; NOT binary/mode/symlink/whole-source qualification",
        baseline.len()
    );
    Ok(())
}
