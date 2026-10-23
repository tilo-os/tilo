//! Campaign replay: re-execute a recorded campaign benchmark run from its
//! action log, headless, and compare what the replay makes with what the
//! run made. Ignored by default; run it on one run directory:
//!
//! ```text
//! TILO_REPLAY_RUN=<run dir> cargo test --release -p tilo-agent-participant \
//!     --test campaign_replay -- --ignored --nocapture
//! ```
//!
//! The run directory is one `run_tilo.py` wrote: `actions.jsonl` (every
//! action an agent invoked, `TILO_AGENT_ACTION_LOG`), `scenario.json` (its
//! input content by absolute path), `timing.json` and the exported outputs.
//! Every record the log names is opened again, as a fresh record of its app
//! in a confined worker lent one content store, and every action that
//! succeeded is applied to it in the order the agents started them. Ids the
//! replay mints differently (a new sheet's, an exported picture's address)
//! are mapped from the run's answers onto the replay's and rewritten in the
//! actions that follow. After each action the record is exported through
//! the app's own export as a frame. The replay writes `<run dir>/replay/`:
//!
//! - `frames/<seq>-<record8>.<ext>` and `frames.jsonl`, one line per logged
//!   action: `seq`, `t_ms` (its end since the run's start), `agent`, `app`,
//!   `record`, `action`, `ok` (in the run), `replayed`, `error`, `file` (the
//!   record's latest frame, run-dir relative to `replay/`).
//! - `events.jsonl`: what the replay did with each action (the rewritten
//!   address and arguments, its answer, how ids were mapped).
//! - `outputs/`: the replay's counterpart of each of the run's outputs.
//! - `verify.json`: each output compared with the run's (identical bytes, or
//!   pixel difference, cell values, text).
//! - `survey.json`: the operations each app's fresh record publishes;
//!   `survey_final.json`: what each record offers at the end.
//!
//! An action the room's owner ran (`"as": "owner"` in the log: a
//! scenario's setup no agent's grant reaches) is applied as the owner, its
//! change merged into the record the agents' actions are applied to.
//!
//! `TILO_REPLAY_ORDER=ended` applies the actions in the order they ended
//! instead of the order they started.
//!
//! A frame is the record's own export kept as content: Canvas's picture,
//! Sheets' workbook, the Word Processor's document, Notes' notebook as
//! Markdown, Slides' window as a picture (else its deck), Calendar's and
//! Contacts' files, Mail's held mail and draft, Messages' conversations and
//! Chat's channels as transcripts, a PDF's first page, and a Video Editor
//! frame from the host's render service (its timeline when this host has no
//! render helper). An export that answers its content as `value.content`
//! with no resources (Mail, Notes, Calendar) is read too.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::string_slice,
    clippy::similar_names,
    clippy::many_single_char_names,
    clippy::too_many_lines,
    reason = "a replay tool, not product code: a malformed run directory is a \
              failure to report; ids and markup are ASCII, sliced at ASCII \
              boundaries; the zip and deflate readers index fixed-layout bytes"
)]

// Records are held in confined app workers (this binary, started again);
// their changes are admitted through the Automerge isolate.
tilo_agent_participant::app_worker!();
tilo_automerge_isolate::test_worker!();

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};
use tilo_actions::door::{ActionAddress, ActionDescriptor};
use tilo_agent_participant::{AppCatalogue, ConfinedReplica, ReplicaActor, Seed};
use tilo_content::ContentStore;

const ROOM: &str = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";
const CANVAS: &str = "so.tilo.canvas";
const SHEETS: &str = "so.tilo.sheets";
const WRITER: &str = "so.tilo.word-processor";
const NOTES: &str = "so.tilo.notes";
const SLIDES: &str = "so.tilo.slides";
const CALENDAR: &str = "so.tilo.calendar";
const CONTACTS: &str = "so.tilo.contacts";
const MAIL: &str = "so.tilo.mail";
const MESSAGES: &str = "so.tilo.messages";
const CHAT: &str = "so.tilo.chat";
const PDF: &str = "so.tilo.pdf-viewer";
const VIDEO: &str = "so.tilo.video-editor";

/// One line of `actions.jsonl`.
#[derive(Clone, Debug, serde::Deserialize)]
struct Logged {
    started_unix_ms: u64,
    ended_unix_ms: u64,
    agent: String,
    app: String,
    record: String,
    resource: String,
    action: String,
    #[serde(default)]
    arguments: Map<String, Value>,
    ok: bool,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    produced: Vec<String>,
    #[serde(default)]
    resources: Option<Vec<String>>,
    #[serde(default)]
    value: Option<Value>,
    /// `owner` for a step the room's owner ran (a scenario's setup the
    /// agents' grants do not reach); an agent's otherwise.
    #[serde(default, rename = "as")]
    actor: Option<String>,
}

/// One record the run worked in, held again for the replay.
struct Held {
    replica: ConfinedReplica,
    /// The latest frame, relative to `replay/`.
    frame: Option<String>,
}

/// Ids the run minted, mapped onto those the replay minted for the same
/// action.
#[derive(Default)]
struct IdMap {
    map: BTreeMap<String, String>,
}

impl IdMap {
    fn insert(&mut self, original: &str, replayed: &str, how: &str, notes: &mut Vec<String>) {
        if original == replayed || original.is_empty() || replayed.is_empty() {
            return;
        }
        if self
            .map
            .get(original)
            .is_some_and(|known| known == replayed)
        {
            return;
        }
        notes.push(format!("{how}: {original} -> {replayed}"));
        self.map.insert(original.to_owned(), replayed.to_owned());
    }

    /// `original` and `replayed` name the same thing: map them whole, and
    /// the ids inside them pairwise.
    fn pair(&mut self, original: &str, replayed: &str, how: &str, notes: &mut Vec<String>) {
        if original == replayed {
            return;
        }
        self.insert(original, replayed, how, notes);
        let (from, to) = (id_tokens(original), id_tokens(replayed));
        if from.len() == to.len() {
            for (from, to) in from.iter().zip(&to) {
                self.insert(from, to, how, notes);
            }
        }
    }

    /// `text` with every mapped id replaced, longest first so an id is never
    /// replaced inside a longer one.
    fn rewrite(&self, text: &str) -> String {
        let mut keys: Vec<&String> = self.map.keys().collect();
        keys.sort_by_key(|key| std::cmp::Reverse(key.len()));
        let mut text = text.to_owned();
        for key in keys {
            if text.contains(key.as_str()) {
                text = text.replace(key.as_str(), &self.map[key]);
            }
        }
        text
    }

    fn rewrite_arguments(&self, arguments: &Map<String, Value>) -> Map<String, Value> {
        let text = serde_json::to_string(arguments).unwrap();
        serde_json::from_str(&self.rewrite(&text)).unwrap_or_else(|_| arguments.clone())
    }

    fn targets(&self) -> BTreeSet<&str> {
        self.map.values().map(String::as_str).collect()
    }
}

/// The ids inside `text`: UUIDs and 64-digit hex content hashes, in order.
fn id_tokens(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let hex = |byte: u8| byte.is_ascii_hexdigit();
    let mut tokens = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        if !hex(bytes[at]) || (at > 0 && hex(bytes[at - 1])) {
            at += 1;
            continue;
        }
        let run = bytes[at..]
            .iter()
            .take_while(|&&byte| hex(byte) || byte == b'-')
            .count();
        let candidate = &text[at..at + run];
        let groups: Vec<usize> = candidate.split('-').map(str::len).collect();
        if groups.len() >= 5 && groups[..5] == [8, 4, 4, 4, 12] {
            tokens.push(candidate[..36].to_owned());
        } else if run >= 64 && groups[0] >= 64 {
            tokens.push(candidate[..groups[0]].to_owned());
        }
        at += run.max(1);
    }
    tokens
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(
        &std::fs::read_to_string(path)
            .unwrap_or_else(|error| panic!("{}: {error}", path.display())),
    )
    .unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn append(path: &Path, line: &Value) {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    writeln!(file, "{line}").unwrap();
}

fn content_of(store: &ContentStore, address: &str) -> Option<Vec<u8>> {
    store.read(tilo_actions::content_hash(address)?)
}

/// Every content address in `arguments`' strings.
fn named_content(arguments: &Map<String, Value>) -> Vec<String> {
    let text = serde_json::to_string(arguments).unwrap_or_default();
    id_tokens(&text)
        .into_iter()
        .filter(|token| token.len() >= 64 && text.contains(&format!("tilo.content:{token}")))
        .collect()
}

/// A short name of a record for file names.
fn record8(app: &str, record: &str) -> String {
    // A record id reads `uuid:<id>`; the id alone names the file.
    let id = record.rsplit(':').next().unwrap_or(record);
    if id.is_empty() {
        app.rsplit('.')
            .next()
            .unwrap_or(app)
            .chars()
            .take(8)
            .collect()
    } else {
        id.chars().take(8).collect()
    }
}

/// The record's current state through its app's own export: the extension
/// and bytes of one frame.
fn capture(held: &Held, store: &ContentStore, app: &str) -> Result<(String, Vec<u8>), String> {
    let offered = held.replica.survey();
    let find = |action: &str| {
        offered
            .iter()
            .find(|descriptor| descriptor.address.action == action)
    };
    let run = |descriptor: &ActionDescriptor, arguments: Value| -> Result<Value, String> {
        let Value::Object(arguments) = arguments else {
            return Err("arguments are an object".into());
        };
        let output = held
            .replica
            .invoke(&descriptor.address, &arguments, false)
            .map_err(|error| format!("{}: {error}", descriptor.address.action))?;
        // A read or an export changes nothing; a stray change is dropped.
        let _ = held.replica.take_mutation();
        Ok(json!({"resources": output.resources, "value": output.value}))
    };
    let export = |descriptor: &ActionDescriptor, arguments: Value| -> Result<Vec<u8>, String> {
        let answer = run(descriptor, arguments)?;
        kept_content(store, &answer)
            .ok_or_else(|| format!("{} kept no content", descriptor.address.action))
    };
    match app {
        CALENDAR | CONTACTS => {
            let (action, extension) = if app == CALENDAR {
                ("export-ics-content", "ics")
            } else {
                ("export-vcard-content", "vcf")
            };
            let descriptor = find(action).ok_or_else(|| format!("no {action}"))?;
            Ok((extension.into(), export(descriptor, json!({}))?))
        }
        MAIL => {
            // Held mail with the draft being written.
            let descriptor = find("export-content").ok_or("the mailbox offers no export")?;
            Ok(("md".into(), export(descriptor, json!({"format": "md"}))?))
        }
        MESSAGES => {
            // Every conversation's transcript.
            let descriptor = find("export-content").ok_or("no transcript export")?;
            Ok(("md".into(), export(descriptor, json!({"format": "md"}))?))
        }
        CHAT => {
            // Each channel's transcript, by the channels `read` lists.
            let read = find("read").ok_or("the chat offers no read")?;
            let listing = run(read, json!({}))?;
            let descriptor = find("export-content").ok_or("no transcript export")?;
            let mut text = Vec::new();
            for channel in listing["value"]["channels"]
                .as_array()
                .into_iter()
                .flatten()
            {
                let Some(id) = channel["id"].as_str() else {
                    continue;
                };
                if let Ok(transcript) = export(descriptor, json!({"channel": id, "format": "md"})) {
                    if !text.is_empty() {
                        text.extend_from_slice(b"\n\n");
                    }
                    let _ = writeln!(text, "# {}", channel["name"].as_str().unwrap_or(id));
                    text.extend(transcript);
                }
            }
            if text.is_empty() {
                return Err("no channel has a transcript".into());
            }
            Ok(("md".into(), text))
        }
        PDF => {
            let descriptor = find("look-page").ok_or("the document offers no look-page")?;
            Ok((
                "png".into(),
                export(descriptor, json!({"page": 1, "size": 1024}))?,
            ))
        }
        VIDEO => {
            // A frame from the host's render service, else the timeline.
            if let Some(descriptor) = find("view-frame-content")
                && tilo_agent_participant::render::available()
                && let Ok(bytes) = export(descriptor, json!({"at": 0.0, "width": 640}))
            {
                return Ok(("png".into(), bytes));
            }
            let descriptor =
                find("export-timeline-content").ok_or("the project offers no timeline export")?;
            Ok(("json".into(), export(descriptor, json!({}))?))
        }
        CANVAS => {
            let descriptor = find("export").ok_or("the artwork offers no export")?;
            Ok((
                "png".into(),
                export(descriptor, json!({"scale": 0.5, "format": "png"}))?,
            ))
        }
        SHEETS => {
            let descriptor =
                find("export-xlsx-content").ok_or("the workbook offers no xlsx export")?;
            Ok(("xlsx".into(), export(descriptor, json!({}))?))
        }
        WRITER => {
            let descriptor = find("export-content").ok_or("the document offers no export")?;
            let choices: Vec<String> = descriptor
                .params
                .iter()
                .filter(|param| param.name == "format")
                .flat_map(|param| {
                    param
                        .choices
                        .iter()
                        .map(|choice| choice.as_ref().to_owned())
                })
                .collect();
            // HTML carries placed pictures inline; Markdown drops them.
            let format = ["html", "md", "markdown"]
                .into_iter()
                .find(|format| choices.iter().any(|choice| choice == format))
                .unwrap_or("markdown");
            let extension = if format == "html" { "html" } else { "md" };
            Ok((
                extension.into(),
                export(descriptor, json!({"format": format}))?,
            ))
        }
        NOTES => {
            // The whole notebook, each note under its title.
            let descriptor =
                find("export-markdown-content").ok_or("the notebook offers no export")?;
            Ok(("md".into(), export(descriptor, json!({}))?))
        }
        SLIDES => {
            // The deck as its window shows it, else the deck itself.
            if let Some(descriptor) = find("render")
                && let Ok(bytes) = export(descriptor, json!({"scale": 0.5}))
            {
                return Ok((sniff(&bytes, "render").to_owned(), bytes));
            }
            let descriptor = find("export-pptx-content").ok_or("the deck offers no export")?;
            Ok(("pptx".into(), export(descriptor, json!({}))?))
        }
        _ => generic_frame(&offered, &export),
    }
}

/// The content an answer (`{"resources", "value"}`) kept: its first content
/// resource, else the address its value carries as `content`.
fn kept_content(store: &ContentStore, answer: &Value) -> Option<Vec<u8>> {
    answer["resources"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .chain(answer["value"]["content"].as_str())
        .find_map(|address| content_of(store, address))
}

/// Run one export of the record with these arguments: the bytes it kept.
type Export<'a> = dyn Fn(&ActionDescriptor, Value) -> Result<Vec<u8>, String> + 'a;

/// A frame of an app this replay has no export of its own for: its
/// content exports (each row's, joined when they are text, as a calendar's
/// events or a book's contacts), else the app's own picture of its window.
fn generic_frame(
    offered: &[ActionDescriptor],
    export: &Export<'_>,
) -> Result<(String, Vec<u8>), String> {
    let exports: Vec<&ActionDescriptor> = offered
        .iter()
        .filter(|descriptor| {
            descriptor.address.action.starts_with("export")
                && descriptor.address.action.ends_with("-content")
        })
        .collect();
    if let Some(first) = exports.first() {
        let action = first.address.action.clone();
        let mut parts = Vec::new();
        for descriptor in exports
            .iter()
            .filter(|descriptor| descriptor.address.action == action)
            .take(60)
        {
            let format = descriptor
                .params
                .iter()
                .filter(|param| param.name == "format")
                .flat_map(|param| {
                    param
                        .choices
                        .iter()
                        .map(|choice| choice.as_ref().to_owned())
                })
                .min_by_key(|choice| {
                    ["png", "pdf", "pptx", "ics", "vcf", "html", "md", "txt"]
                        .iter()
                        .position(|preferred| preferred == choice)
                        .unwrap_or(usize::MAX)
                });
            let arguments = format.map_or_else(|| json!({}), |format| json!({"format": format}));
            parts.push(export(descriptor, arguments)?);
        }
        let extension = sniff(parts.first().map_or(&[][..], Vec::as_slice), &action);
        let text = ["ics", "vcf", "md", "txt", "csv", "json", "html"].contains(&extension);
        let bytes = if text {
            parts.join(&b"\n"[..])
        } else {
            parts.into_iter().next().unwrap_or_default()
        };
        return Ok((extension.to_owned(), bytes));
    }
    let render = offered
        .iter()
        .find(|descriptor| descriptor.address.action == "render")
        .ok_or("no content export and no render")?;
    let bytes = export(render, json!({"scale": 0.5}))?;
    Ok((sniff(&bytes, "render").to_owned(), bytes))
}

/// The file extension `bytes` read as, by their signature, else by the
/// export's name.
fn sniff(bytes: &[u8], action: &str) -> &'static str {
    if bytes.starts_with(b"\x89PNG") {
        return "png";
    }
    if bytes.starts_with(&[0xff, 0xd8]) {
        return "jpg";
    }
    if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(&b"WEBP"[..]) {
        return "webp";
    }
    if bytes.starts_with(b"%PDF") {
        return "pdf";
    }
    if bytes.starts_with(b"PK") {
        let names: Vec<String> = zip_directory(bytes)
            .into_iter()
            .map(|entry| entry.name)
            .collect();
        let has = |prefix: &str| names.iter().any(|name| name.starts_with(prefix));
        return if has("ppt/") {
            "pptx"
        } else if has("word/") {
            "docx"
        } else if has("xl/") {
            "xlsx"
        } else {
            "zip"
        };
    }
    let Ok(text) = std::str::from_utf8(bytes) else {
        return "bin";
    };
    let head = text.trim_start().to_ascii_lowercase();
    if head.starts_with("begin:vcalendar") || head.starts_with("begin:vevent") {
        "ics"
    } else if head.starts_with("begin:vcard") {
        "vcf"
    } else if head.starts_with("<svg") || (head.starts_with("<?xml") && head.contains("<svg")) {
        "svg"
    } else if head.starts_with("<!doctype html") || head.starts_with("<html") {
        "html"
    } else if head.starts_with('{') || head.starts_with('[') {
        "json"
    } else if action.contains("markdown") || action.contains("-md") {
        "md"
    } else if action.contains("csv") {
        "csv"
    } else {
        "txt"
    }
}

/// The resource the replayed action should name, when the run's (mapped)
/// one is not published by the replay's record: the one record resource of
/// the same kind that offers the action and no mapping claims yet.
fn survey_match(
    replica: &ConfinedReplica,
    original: &str,
    rewritten: &str,
    action: &str,
    ids: &IdMap,
) -> Option<String> {
    let offered = replica.survey();
    if offered
        .iter()
        .any(|descriptor| descriptor.address.resource == rewritten)
    {
        return None;
    }
    // A resource's kind: what it names with its ids left out, so a path
    // such as `notes.notebook/page/<id>/body` matches its replayed twin.
    let kind = |resource: &str| {
        let kind = resource.split_once(':').map_or(resource, |(kind, _)| kind);
        kind.split('/')
            .map(|part| if is_id(part) { "{id}" } else { part })
            .collect::<Vec<_>>()
            .join("/")
    };
    let wanted = kind(original);
    let claimed = ids.targets();
    let candidates: Vec<&ActionDescriptor> = offered
        .iter()
        .filter(|descriptor| descriptor.address.action == action)
        .filter(|descriptor| {
            kind(&descriptor.address.resource) == wanted || descriptor.resource_kind == wanted
        })
        .collect();
    candidates
        .iter()
        .find(|descriptor| !claimed.contains(descriptor.address.resource.as_str()))
        .or_else(|| candidates.first())
        .map(|descriptor| descriptor.address.resource.clone())
}

/// The `<kind>:<uuid>` resources an action's arguments name.
fn named_resources(arguments: &Map<String, Value>) -> Vec<String> {
    let mut found = Vec::new();
    let mut stack: Vec<&Value> = arguments.values().collect();
    while let Some(value) = stack.pop() {
        match value {
            Value::String(text) => {
                if let Some((kind, id)) = text.trim().split_once(':')
                    && kind.contains('.')
                    && is_id(id)
                {
                    found.push(text.trim().to_owned());
                }
            }
            Value::Array(items) => stack.extend(items),
            Value::Object(fields) => stack.extend(fields.values()),
            _ => {}
        }
    }
    found
}

/// The resource of `rewritten`'s kind the replay's record publishes and no
/// mapping claims yet, when `rewritten` itself is not published and there is
/// exactly one such.
fn argument_match(replica: &ConfinedReplica, rewritten: &str, ids: &IdMap) -> Option<String> {
    let (kind, _) = rewritten.split_once(':')?;
    let offered = replica.survey();
    let resources: BTreeSet<&str> = offered
        .iter()
        .map(|descriptor| descriptor.address.resource.as_str())
        .collect();
    if resources.contains(rewritten) {
        return None;
    }
    let claimed = ids.targets();
    let mut twins = resources.into_iter().filter(|resource| {
        resource.split_once(':').is_some_and(|(k, _)| k == kind) && !claimed.contains(resource)
    });
    let twin = twins.next()?;
    twins.next().is_none().then(|| twin.to_owned())
}

/// Run `address` as the room's owner on the record `held` holds, as the
/// run's owner did through the host's room actions: on an owner's copy of
/// the record as it stands, whose change is then merged into `held`, as the
/// host publishes an owner's change to the room. The answer, and whether the
/// record changed.
fn as_owner(
    catalogue: &AppCatalogue,
    store: &ContentStore,
    held: &ConfinedReplica,
    address: &ActionAddress,
    arguments: &Map<String, Value>,
) -> Result<(tilo_actions::ActionOutput, bool), String> {
    let bytes = held
        .read(|doc| doc.clone().save())
        .ok_or("the record is not held")?;
    let owner = ConfinedReplica::open(
        catalogue,
        held.app_id(),
        ROOM,
        "owner",
        &ReplicaActor::Owner,
        Seed::Existing {
            record: held.record_id().clone(),
            changes: vec![bytes],
        },
    )
    .map_err(|error| error.to_string())?
    .lending(store.clone());
    let output = owner
        .invoke(address, arguments, true)
        .map_err(|error| error.to_string())?;
    let mut changed = false;
    if let Some(mutation) = owner.take_mutation().map_err(|error| error.to_string())? {
        held.apply(&mutation).map_err(|error| error.to_string())?;
        changed = true;
    }
    for (record, mutation) in owner
        .take_record_mutations()
        .map_err(|error| error.to_string())?
    {
        held.apply_record(&record, &mutation)
            .map_err(|error| error.to_string())?;
        changed = true;
    }
    Ok((output, changed))
}

/// Whether `part` is a minted id (a UUID).
fn is_id(part: &str) -> bool {
    part.len() == 36
        && part.chars().enumerate().all(|(at, c)| {
            if matches!(at, 8 | 13 | 18 | 23) {
                c == '-'
            } else {
                c.is_ascii_hexdigit()
            }
        })
}

#[test]
#[ignore = "replays the campaign run TILO_REPLAY_RUN names"]
fn campaign_replay() {
    let run = PathBuf::from(
        std::env::var_os("TILO_REPLAY_RUN").expect("TILO_REPLAY_RUN names a run directory"),
    );
    let out = run.join("replay");
    if out.exists() {
        std::fs::remove_dir_all(&out).unwrap();
    }
    std::fs::create_dir_all(out.join("frames")).unwrap();
    std::fs::create_dir_all(out.join("outputs")).unwrap();

    let scenario = read_json(&run.join("scenario.json"));
    let timing = read_json(&run.join("timing.json"));
    let started_ms = (timing["started_at"].as_f64().unwrap_or(0.0) * 1000.0) as u64;
    let mut logged: Vec<Logged> = std::fs::read_to_string(run.join("actions.jsonl"))
        .expect("the run has an action log")
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    // In the order the agents started them (the log is in the order they
    // ended), or with `TILO_REPLAY_ORDER=ended` in the order they ended:
    // nearer the order the host committed concurrent edits of one record
    // (two agents' new sheets), and still after whatever each one used.
    let by_end = std::env::var("TILO_REPLAY_ORDER").is_ok_and(|order| order == "ended");
    if by_end {
        logged.sort_by_key(|action| (action.ended_unix_ms, action.started_unix_ms));
    } else {
        logged.sort_by_key(|action| (action.started_unix_ms, action.ended_unix_ms));
    }

    // Every app an agent's worker can hold: the browser desktop's and the
    // agent-only ones (Mail).
    let catalogue = AppCatalogue::native();
    let store = ContentStore::memory();
    // The scenario's input content: content addresses are hashes, so each
    // input gets the address the run's tasks named.
    let mut inputs = Vec::new();
    for (name, path) in scenario["content"].as_object().into_iter().flatten() {
        let path = path.as_str().unwrap_or_default();
        let bytes = std::fs::read(path).unwrap_or_else(|error| panic!("{path}: {error}"));
        let hash = store.put(&bytes).unwrap();
        inputs.push(
            json!({"name": name, "path": path, "address": tilo_actions::content_resource(&hash)}),
        );
    }

    // The operations each app's fresh record publishes, for whoever reads
    // the replay.
    let apps: BTreeSet<&str> = logged.iter().map(|action| action.app.as_str()).collect();
    let mut survey = Map::new();
    for app in &apps {
        if let Ok(replica) = ConfinedReplica::open(
            &catalogue,
            app,
            ROOM,
            "replay",
            &ReplicaActor::Agent,
            Seed::New,
        ) {
            let replica = replica.lending(store.clone());
            let operations: Vec<Value> = replica
                .survey()
                .iter()
                .map(|descriptor| {
                    json!({
                        "resource": descriptor.address.resource,
                        "kind": descriptor.resource_kind,
                        "action": descriptor.address.action,
                        "label": descriptor.action_label,
                        "params": descriptor.params.iter().map(|param| json!({
                            "name": param.name.as_ref(),
                            "choices": param.choices.iter().map(AsRef::as_ref).collect::<Vec<&str>>(),
                        })).collect::<Vec<_>>(),
                    })
                })
                .collect();
            survey.insert((*app).to_owned(), Value::Array(operations));
        }
    }
    std::fs::write(
        out.join("survey.json"),
        serde_json::to_string_pretty(&survey).unwrap(),
    )
    .unwrap();

    let mut ids = IdMap::default();
    let mut held: BTreeMap<(String, String), Held> = BTreeMap::new();
    // What each logged action answered in the replay, by its sequence.
    let mut answers: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    let mut regressions = Vec::new();
    let mut borrowed = BTreeSet::new();
    let mut frames = 0usize;
    let mut applied = 0usize;
    let mut skipped = 0usize;

    for (seq, action) in logged.iter().enumerate() {
        let key = (action.app.clone(), action.record.clone());
        let mut notes = Vec::new();
        if !held.contains_key(&key) {
            let replica = ConfinedReplica::open(
                &catalogue,
                &action.app,
                ROOM,
                "replay",
                &ReplicaActor::Agent,
                Seed::New,
            )
            .unwrap_or_else(|error| panic!("{}: {error}", action.app))
            .lending(store.clone());
            replica.take_mutation().unwrap();
            ids.pair(&action.record, replica.wire_record(), "record", &mut notes);
            ids.pair(
                &action.record,
                replica.record_id().as_str(),
                "record",
                &mut notes,
            );
            held.insert(
                key.clone(),
                Held {
                    replica,
                    frame: None,
                },
            );
        }
        let record = held.get_mut(&key).unwrap();
        let t_ms = action.ended_unix_ms.saturating_sub(started_ms);
        let mut line = json!({
            "seq": seq,
            "t_ms": t_ms,
            "start_ms": action.started_unix_ms.saturating_sub(started_ms),
            "agent": action.agent,
            "app": action.app,
            "record": action.record,
            "resource": action.resource,
            "action": action.action,
            "ok": action.ok,
            "replayed": false,
            "error": action.error,
        });
        if !action.ok {
            skipped += 1;
            line["file"] = json!(record.frame);
            append(&out.join("frames.jsonl"), &line);
            append(
                &out.join("events.jsonl"),
                &json!({"seq": seq, "skipped": "failed in the run", "error": action.error}),
            );
            continue;
        }

        let mut resource = ids.rewrite(&action.resource);
        if let Some(found) = survey_match(
            &record.replica,
            &action.resource,
            &resource,
            &action.action,
            &ids,
        ) {
            ids.pair(&action.resource, &found, "survey", &mut notes);
            resource = found;
        }
        // A resource the arguments name (`"note": "notes.page:<id>"`) that the
        // replay's record does not publish: its one unclaimed twin of that kind.
        for named in named_resources(&action.arguments) {
            let rewritten = ids.rewrite(&named);
            if let Some(found) = argument_match(&record.replica, &rewritten, &ids) {
                ids.pair(&named, &found, "argument", &mut notes);
            }
        }
        let arguments = ids.rewrite_arguments(&action.arguments);
        // Content the action names that the replay never made: the run's
        // copy, so the action can still run (reported as borrowed).
        for hash in named_content(&arguments) {
            if !store.has(&hash)
                && let Ok(bytes) = std::fs::read(run.join("content").join(&hash))
            {
                store.put(&bytes).unwrap();
                borrowed.insert(hash.clone());
                notes.push(format!("content borrowed from the run: {hash}"));
            }
        }
        let address = ActionAddress::new(resource.clone(), action.action.clone());
        let (outcome, changed) = if action.actor.as_deref() == Some("owner") {
            match as_owner(&catalogue, &store, &record.replica, &address, &arguments) {
                Ok((output, changed)) => (Ok(output), changed),
                Err(error) => (Err(error), false),
            }
        } else {
            let outcome = record
                .replica
                .invoke(&address, &arguments, true)
                .map_err(|error| error.to_string());
            (
                outcome,
                matches!(record.replica.take_mutation(), Ok(Some(_))),
            )
        };
        let mut event = json!({
            "seq": seq,
            "resource": resource,
            "arguments": arguments,
        });
        match outcome {
            Ok(mut output) => {
                applied += 1;
                // Content an export answers as `value.content` (Mail, Notes,
                // Calendar) is among what it made, as the run logged it.
                if let Some(address) = output
                    .value
                    .as_ref()
                    .and_then(|value| value["content"].as_str())
                    .filter(|address| tilo_actions::content_hash(address).is_some())
                    .map(str::to_owned)
                    && !output.resources.contains(&address)
                {
                    output.resources.push(address);
                }
                line["replayed"] = json!(true);
                // The run's answer onto the replay's, by position.
                if let Some(original) = &action.resources {
                    for (from, to) in original.iter().zip(&output.resources) {
                        ids.pair(from, to, "answer", &mut notes);
                    }
                }
                let produced: Vec<&String> = output
                    .resources
                    .iter()
                    .filter(|resource| tilo_actions::content_hash(resource).is_some())
                    .collect();
                for (from, to) in action.produced.iter().zip(&produced) {
                    ids.pair(from, to, "produced", &mut notes);
                }
                if let (Some(original), Some(replayed)) = (&action.value, &output.value) {
                    map_values(original, replayed, &mut ids, &mut notes);
                }
                answers.insert(seq, output.resources.clone());
                event["resources"] = json!(output.resources);
                event["value"] = output.value.unwrap_or(Value::Null);
            }
            Err(error) => {
                regressions.push(json!({
                    "seq": seq,
                    "agent": action.agent,
                    "app": action.app,
                    "action": action.action,
                    "resource": resource,
                    "error": error,
                }));
                line["error"] = json!(error);
                event["error"] = json!(error);
            }
        }
        if changed || record.frame.is_none() {
            match capture(record, &store, &action.app) {
                Ok((extension, bytes)) => {
                    let file = format!(
                        "frames/{seq:05}-{}.{extension}",
                        record8(&action.app, &action.record)
                    );
                    std::fs::write(out.join(&file), bytes).unwrap();
                    record.frame = Some(file);
                    frames += 1;
                }
                Err(error) => notes.push(format!("no frame: {error}")),
            }
        }
        line["file"] = json!(record.frame);
        event["ids"] = json!(notes);
        append(&out.join("frames.jsonl"), &line);
        append(&out.join("events.jsonl"), &event);
    }

    // What each record offers at the end, effects too: the exports a frame
    // could use for an app this replay does not know yet.
    let mut final_survey = Map::new();
    for ((app, record), held) in &held {
        let actions: BTreeSet<String> = held
            .replica
            .survey()
            .iter()
            .map(|descriptor| {
                format!(
                    "{} {} ({:?})",
                    descriptor.resource_kind, descriptor.address.action, descriptor.kind
                )
            })
            .collect();
        final_survey.insert(format!("{app} {record}"), json!(actions));
    }
    std::fs::write(
        out.join("survey_final.json"),
        serde_json::to_string_pretty(&final_survey).unwrap(),
    )
    .unwrap();

    // Each of the run's outputs beside the replay's counterpart: the
    // content the replay's run of the same action made.
    let mut checks = Vec::new();
    for output in scenario["outputs"].as_array().into_iter().flatten() {
        let file = output["file"].as_str().unwrap_or_default();
        let mut check = json!({"file": file});
        let Ok(original) = std::fs::read(run.join(file)) else {
            check["status"] = json!("missing in the run");
            checks.push(check);
            continue;
        };
        let address = tilo_actions::content_resource(&tilo_content::hash_bytes(&original))
            .unwrap_or_default();
        let source = logged
            .iter()
            .enumerate()
            .rev()
            .find(|(_, action)| action.produced.contains(&address));
        let Some((seq, action)) = source else {
            check["status"] = json!("no logged action produced it");
            checks.push(check);
            continue;
        };
        check["seq"] = json!(seq);
        check["action"] = json!(format!("{} {}", action.app, action.action));
        let position = action
            .produced
            .iter()
            .position(|produced| *produced == address)
            .unwrap_or(0);
        let replayed = answers.get(&seq).and_then(|resources| {
            resources
                .iter()
                .filter(|resource| tilo_actions::content_hash(resource).is_some())
                .nth(position)
                .and_then(|resource| content_of(&store, resource))
        });
        let Some(replayed) = replayed else {
            check["status"] = json!("the replay did not make it");
            checks.push(check);
            continue;
        };
        std::fs::write(out.join("outputs").join(file), &replayed).unwrap();
        compare(file, &original, &replayed, &mut check);
        checks.push(check);
    }

    let reproduced = checks
        .iter()
        .filter(|check| check["match"].as_bool() == Some(true))
        .count();
    let verify = json!({
        "run": run,
        "order": if by_end { "ended" } else { "started" },
        "actions": logged.len(),
        "applied": applied,
        "skipped_failed_in_run": skipped,
        "failed_in_replay": regressions,
        "records": held.len(),
        "frames": frames,
        "inputs": inputs,
        "borrowed_content": borrowed,
        "outputs": checks,
        "reproduced": format!("{reproduced}/{}", checks.len()),
    });
    std::fs::write(
        out.join("verify.json"),
        serde_json::to_string_pretty(&verify).unwrap(),
    )
    .unwrap();

    let mut summary = format!(
        "replay of {}: {} actions, {applied} applied, {skipped} failed in the run, {} failed in the replay; {} records, {frames} frames\n",
        run.display(),
        logged.len(),
        verify["failed_in_replay"].as_array().map_or(0, Vec::len),
        held.len(),
    );
    for regression in verify["failed_in_replay"].as_array().into_iter().flatten() {
        let _ = writeln!(
            summary,
            "  replay failed #{} {} {} {}: {}",
            regression["seq"],
            regression["agent"].as_str().unwrap_or_default(),
            regression["app"].as_str().unwrap_or_default(),
            regression["action"].as_str().unwrap_or_default(),
            regression["error"].as_str().unwrap_or_default(),
        );
    }
    for check in &checks {
        let _ = writeln!(
            summary,
            "  {:<18} {}",
            check["file"].as_str().unwrap_or_default(),
            check["summary"]
                .as_str()
                .or_else(|| check["status"].as_str())
                .unwrap_or_default(),
        );
    }
    let _ = writeln!(summary, "  reproduced {reproduced}/{}", checks.len());
    print!("{summary}");
}

/// Map the ids inside the run's answer onto the replay's, walking both.
fn map_values(original: &Value, replayed: &Value, ids: &mut IdMap, notes: &mut Vec<String>) {
    match (original, replayed) {
        (Value::String(from), Value::String(to)) if from != to => {
            let (a, b) = (id_tokens(from), id_tokens(to));
            if !a.is_empty() && a.len() == b.len() {
                for (from, to) in a.iter().zip(&b) {
                    ids.insert(from, to, "value", notes);
                }
            }
        }
        (Value::Array(from), Value::Array(to)) => {
            for (from, to) in from.iter().zip(to) {
                map_values(from, to, ids, notes);
            }
        }
        (Value::Object(from), Value::Object(to)) => {
            for (key, from) in from {
                if let Some(to) = to.get(key) {
                    map_values(from, to, ids, notes);
                }
            }
        }
        _ => {}
    }
}

/// Compare one output file with the replay's, by what its kind holds.
fn compare(file: &str, original: &[u8], replayed: &[u8], check: &mut Value) {
    if original == replayed {
        check["status"] = json!("identical");
        check["match"] = json!(true);
        check["summary"] = json!("identical bytes");
        return;
    }
    check["status"] = json!("different bytes");
    let extension = file.rsplit('.').next().unwrap_or_default();
    match extension {
        "png" => compare_png(original, replayed, check),
        "xlsx" => compare_xlsx(original, replayed, check),
        "docx" => {
            let text = |bytes: &[u8]| {
                zip_entry(bytes, "word/document.xml")
                    .map(|xml| docx_text(&String::from_utf8_lossy(&xml)))
            };
            match (text(original), text(replayed)) {
                (Some(a), Some(b)) => compare_text(&a, &b, check),
                _ => check["summary"] = json!("a docx has no readable word/document.xml"),
            }
            let media = |bytes: &[u8]| -> BTreeSet<(String, u32, u32)> {
                zip_directory(bytes)
                    .into_iter()
                    .filter(|entry| entry.name.starts_with("word/media/"))
                    .map(|entry| (entry.name, entry.crc, entry.size))
                    .collect()
            };
            let (a, b) = (media(original), media(replayed));
            check["media_equal"] = json!(a == b);
            check["media"] = json!({"run": a.len(), "replay": b.len()});
            if a != b {
                check["match"] = json!(false);
                let summary = check["summary"].as_str().unwrap_or_default().to_owned();
                check["summary"] = json!(format!("{summary}; embedded media differ"));
            }
        }
        _ => compare_text(
            &String::from_utf8_lossy(original),
            &String::from_utf8_lossy(replayed),
            check,
        ),
    }
}

fn compare_png(original: &[u8], replayed: &[u8], check: &mut Value) {
    let (Ok(a), Ok(b)) = (
        ui::png::decode_rgba8(original),
        ui::png::decode_rgba8(replayed),
    ) else {
        check["summary"] = json!("a PNG did not decode");
        check["match"] = json!(false);
        return;
    };
    check["size"] = json!({"run": [a.width, a.height], "replay": [b.width, b.height]});
    if (a.width, a.height) != (b.width, b.height) {
        check["match"] = json!(false);
        check["summary"] = json!(format!(
            "size differs: run {}x{}, replay {}x{}",
            a.width, a.height, b.width, b.height
        ));
        return;
    }
    let mut total = 0u64;
    let mut differing = 0u64;
    for (pa, pb) in a.rgba.chunks_exact(4).zip(b.rgba.chunks_exact(4)) {
        let mut most = 0u8;
        for (ca, cb) in pa.iter().zip(pb) {
            let d = ca.abs_diff(*cb);
            total += u64::from(d);
            most = most.max(d);
        }
        if most > 8 {
            differing += 1;
        }
    }
    let pixels = (a.rgba.len() / 4).max(1) as f64;
    let mean = total as f64 / (pixels * 4.0);
    let share = differing as f64 / pixels;
    check["mean_abs_difference"] = json!(mean);
    check["share_of_pixels_differing"] = json!(share);
    // Encoder or rounding noise only: no pixel visibly different.
    let matched = share < 0.001;
    check["match"] = json!(matched);
    check["summary"] = json!(format!(
        "same size, mean |difference| {mean:.3}/255, {:.2}% of pixels differ by more than 8",
        share * 100.0
    ));
}

fn compare_xlsx(original: &[u8], replayed: &[u8], check: &mut Value) {
    let cells = |bytes: &[u8]| -> Result<BTreeMap<String, BTreeMap<String, String>>, String> {
        let tables = sheets::xlsx::parse_xlsx(bytes).map_err(|error| error.to_string())?;
        Ok(tables
            .into_iter()
            .map(|table| {
                let mut cells = BTreeMap::new();
                for (row, cols) in table.rows.iter().enumerate() {
                    for (col, cell) in cols.iter().enumerate() {
                        if let Some(cell) = cell
                            && !cell.raw.is_empty()
                        {
                            let value = cell.formula.as_ref().map_or_else(
                                || cell.raw.clone(),
                                |formula| format!("{} ({formula})", cell.raw),
                            );
                            cells.insert(format!("R{}C{}", row + 1, col + 1), value);
                        }
                    }
                }
                (table.name, cells)
            })
            .collect())
    };
    let (a, b) = match (cells(original), cells(replayed)) {
        (Ok(a), Ok(b)) => (a, b),
        (a, b) => {
            check["match"] = json!(false);
            check["summary"] = json!(format!(
                "an xlsx did not parse: run {:?}, replay {:?}",
                a.err(),
                b.err()
            ));
            return;
        }
    };
    let names = |sheets: &BTreeMap<String, BTreeMap<String, String>>| -> Vec<String> {
        sheets.keys().cloned().collect()
    };
    let mut same = 0usize;
    let mut differences = Vec::new();
    for (name, run_cells) in &a {
        let empty = BTreeMap::new();
        let replay_cells = b.get(name).unwrap_or(&empty);
        let keys: BTreeSet<&String> = run_cells.keys().chain(replay_cells.keys()).collect();
        for key in keys {
            let (x, y) = (run_cells.get(key), replay_cells.get(key));
            if x == y {
                same += 1;
            } else {
                differences.push(json!({"sheet": name, "cell": key, "run": x, "replay": y}));
            }
        }
    }
    for (name, replay_cells) in &b {
        if !a.contains_key(name) {
            for (key, value) in replay_cells {
                differences.push(json!({"sheet": name, "cell": key, "run": null, "replay": value}));
            }
        }
    }
    let matched = differences.is_empty() && names(&a) == names(&b);
    check["sheets"] = json!({"run": names(&a), "replay": names(&b)});
    check["cells_equal"] = json!(same);
    check["cells_differing"] = json!(differences.len());
    check["differences"] = json!(differences.iter().take(40).collect::<Vec<_>>());
    check["match"] = json!(matched);
    check["summary"] = json!(format!(
        "{} sheets (replay {}), {same} cells equal, {} differ",
        a.len(),
        b.len(),
        differences.len()
    ));
}

fn compare_text(original: &str, replayed: &str, check: &mut Value) {
    let lines = |text: &str| -> Vec<String> {
        text.lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect()
    };
    let (a, b) = (lines(original), lines(replayed));
    let only_run: Vec<&String> = a.iter().filter(|line| !b.contains(line)).collect();
    let only_replay: Vec<&String> = b.iter().filter(|line| !a.contains(line)).collect();
    let matched = a == b;
    check["match"] = json!(matched);
    check["lines"] = json!({"run": a.len(), "replay": b.len()});
    check["only_in_run"] = json!(only_run.iter().take(20).collect::<Vec<_>>());
    check["only_in_replay"] = json!(only_replay.iter().take(20).collect::<Vec<_>>());
    check["summary"] = json!(if matched {
        "same text".to_owned()
    } else {
        format!(
            "text differs: {} of {} lines only in the run, {} of {} only in the replay",
            only_run.len(),
            a.len(),
            only_replay.len(),
            b.len()
        )
    });
}

/// A docx body's text: one line per paragraph.
fn docx_text(xml: &str) -> String {
    let mut text = String::new();
    let mut rest = xml;
    while let Some(open) = rest.find('<') {
        text.push_str(&rest[..open]);
        let Some(close) = rest[open..].find('>') else {
            break;
        };
        let tag = &rest[open..=open + close];
        if tag == "</w:p>" || tag.starts_with("<w:br") {
            text.push('\n');
        } else if tag.starts_with("<w:tab") && !tag.starts_with("<w:tabs") {
            text.push('\t');
        }
        rest = &rest[open + close + 1..];
    }
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

struct ZipEntry {
    name: String,
    method: u16,
    crc: u32,
    compressed: usize,
    size: u32,
    local: usize,
}

/// A zip's central directory.
fn zip_directory(zip: &[u8]) -> Vec<ZipEntry> {
    let u16_at = |at: usize| u16::from_le_bytes([zip[at], zip[at + 1]]);
    let u32_at = |at: usize| u32::from_le_bytes([zip[at], zip[at + 1], zip[at + 2], zip[at + 3]]);
    let mut entries = Vec::new();
    let mut at = 0;
    while at + 46 <= zip.len() {
        if zip[at..at + 4] != [0x50, 0x4b, 0x01, 0x02] {
            at += 1;
            continue;
        }
        let name = usize::from(u16_at(at + 28));
        if at + 46 + name > zip.len() {
            break;
        }
        entries.push(ZipEntry {
            name: String::from_utf8_lossy(&zip[at + 46..at + 46 + name]).into_owned(),
            method: u16_at(at + 10),
            crc: u32_at(at + 16),
            compressed: u32_at(at + 20) as usize,
            size: u32_at(at + 24),
            local: u32_at(at + 42) as usize,
        });
        at += 46 + name + usize::from(u16_at(at + 30)) + usize::from(u16_at(at + 32));
    }
    entries
}

/// One entry of a zip, stored or deflated.
fn zip_entry(zip: &[u8], name: &str) -> Option<Vec<u8>> {
    let entry = zip_directory(zip)
        .into_iter()
        .find(|entry| entry.name == name)?;
    let local = entry.local;
    if zip.get(local..local + 4)? != [0x50, 0x4b, 0x03, 0x04] {
        return None;
    }
    let skip = usize::from(u16::from_le_bytes([zip[local + 26], zip[local + 27]]))
        + usize::from(u16::from_le_bytes([zip[local + 28], zip[local + 29]]));
    let data = zip.get(local + 30 + skip..local + 30 + skip + entry.compressed)?;
    match entry.method {
        0 => Some(data.to_vec()),
        8 => inflate(data),
        _ => None,
    }
}

/// Raw DEFLATE (RFC 1951), as zip entries carry it.
fn inflate(data: &[u8]) -> Option<Vec<u8>> {
    struct Bits<'a> {
        data: &'a [u8],
        at: usize,
        buffer: u32,
        count: u32,
    }
    impl Bits<'_> {
        fn take(&mut self, n: u32) -> Option<u32> {
            while self.count < n {
                let byte = *self.data.get(self.at)?;
                self.at += 1;
                self.buffer |= u32::from(byte) << self.count;
                self.count += 8;
            }
            let value = self.buffer & ((1u32 << n) - 1);
            self.buffer >>= n;
            self.count -= n;
            Some(value)
        }
    }
    struct Huffman {
        counts: [u16; 16],
        symbols: Vec<u16>,
    }
    fn build(lengths: &[u8]) -> Huffman {
        let mut counts = [0u16; 16];
        for &length in lengths {
            counts[usize::from(length)] += 1;
        }
        counts[0] = 0;
        let mut offsets = [0u16; 16];
        for length in 1..16 {
            offsets[length] = offsets[length - 1] + counts[length - 1];
        }
        let mut symbols = vec![0u16; lengths.len()];
        for (symbol, &length) in lengths.iter().enumerate() {
            if length != 0 {
                symbols[usize::from(offsets[usize::from(length)])] = symbol as u16;
                offsets[usize::from(length)] += 1;
            }
        }
        Huffman { counts, symbols }
    }
    fn decode(bits: &mut Bits<'_>, huffman: &Huffman) -> Option<u16> {
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        for length in 1..16 {
            code |= bits.take(1)? as i32;
            let count = i32::from(huffman.counts[length]);
            if code - count < first {
                return huffman
                    .symbols
                    .get((index + code - first) as usize)
                    .copied();
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        None
    }
    const LENGTH_BASE: [u16; 29] = [
        3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115,
        131, 163, 195, 227, 258,
    ];
    const LENGTH_EXTRA: [u8; 29] = [
        0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
    ];
    const DISTANCE_BASE: [u16; 30] = [
        1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
        2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
    ];
    const DISTANCE_EXTRA: [u8; 30] = [
        0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12,
        13, 13,
    ];
    const ORDER: [usize; 19] = [
        16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
    ];
    let mut bits = Bits {
        data,
        at: 0,
        buffer: 0,
        count: 0,
    };
    let mut out: Vec<u8> = Vec::new();
    loop {
        let last = bits.take(1)? == 1;
        match bits.take(2)? {
            0 => {
                bits.buffer = 0;
                bits.count = 0;
                let at = bits.at;
                let length = usize::from(u16::from_le_bytes([*data.get(at)?, *data.get(at + 1)?]));
                out.extend_from_slice(data.get(at + 4..at + 4 + length)?);
                bits.at = at + 4 + length;
            }
            kind @ (1 | 2) => {
                let (literals, distances) = if kind == 1 {
                    let mut lengths = [0u8; 288];
                    for (symbol, length) in lengths.iter_mut().enumerate() {
                        *length = match symbol {
                            144..=255 => 9,
                            256..=279 => 7,
                            _ => 8,
                        };
                    }
                    (build(&lengths), build(&[5u8; 30]))
                } else {
                    let literal_count = bits.take(5)? as usize + 257;
                    let distance_count = bits.take(5)? as usize + 1;
                    let code_count = bits.take(4)? as usize + 4;
                    let mut code_lengths = [0u8; 19];
                    for &slot in ORDER.iter().take(code_count) {
                        code_lengths[slot] = bits.take(3)? as u8;
                    }
                    let codes = build(&code_lengths);
                    let mut lengths = Vec::with_capacity(literal_count + distance_count);
                    while lengths.len() < literal_count + distance_count {
                        let symbol = decode(&mut bits, &codes)?;
                        let (value, repeat) = match symbol {
                            0..=15 => (symbol as u8, 1),
                            16 => (*lengths.last()?, 3 + bits.take(2)?),
                            17 => (0, 3 + bits.take(3)?),
                            _ => (0, 11 + bits.take(7)?),
                        };
                        for _ in 0..repeat {
                            lengths.push(value);
                        }
                    }
                    lengths.truncate(literal_count + distance_count);
                    (
                        build(&lengths[..literal_count]),
                        build(&lengths[literal_count..]),
                    )
                };
                loop {
                    let symbol = decode(&mut bits, &literals)?;
                    match symbol {
                        0..=255 => out.push(symbol as u8),
                        256 => break,
                        _ => {
                            let index = usize::from(symbol - 257);
                            let length = usize::from(*LENGTH_BASE.get(index)?)
                                + bits.take(u32::from(LENGTH_EXTRA[index]))? as usize;
                            let code = usize::from(decode(&mut bits, &distances)?);
                            let distance = usize::from(*DISTANCE_BASE.get(code)?)
                                + bits.take(u32::from(DISTANCE_EXTRA[code]))? as usize;
                            let start = out.len().checked_sub(distance)?;
                            for offset in 0..length {
                                out.push(out[start + offset]);
                            }
                        }
                    }
                }
            }
            _ => return None,
        }
        if last {
            return Some(out);
        }
    }
}
