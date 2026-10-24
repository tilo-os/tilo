//! Every record of what went wrong on this computer, listed and read
//! through one door.
//!
//! The private crash directory (`crate::crash_log::directory`) holds a file
//! for each time the desktop panicked (`<timestamp>-<pid>.log`) or stopped
//! responding (`…-freeze.log`). This module adds the computer itself
//! stopping: the kernel keeps its log of a panic or oops in persistent
//! memory across the reset that follows (pstore, a ramoops region on boards
//! that reserve one). At the next start [`take_in`] reads those records,
//! from `/sys/fs/pstore` where this account can read it and from this
//! boot's journal, where `systemd-pstore` moves them before any session
//! starts, and keeps each as a `…-stopped.log` record beside the others.
//! A record already kept is known by its text, so reading the same ones at
//! every start keeps each once.
//!
//! The records are served under the host's provider
//! ([`tilo_actions::remote::crash_records`]): `list` says what each is
//! without its text, `read` returns one record's text, stacks included,
//! when it is asked for by name. Settings lists them through this door, as
//! an agent or a script does. Nothing leaves the computer.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{Local, TimeZone as _};
use tilo_actions::remote::crash_records::{
    self as wire, CrashKind, CrashRecord, CrashRecordText, StopReason,
};
use tilo_actions::remote::{Pending, pending};
use tilo_actions::{ActionArgs, ActionParam, ParamType};

use crate::crash_log::{FILE_TIMESTAMP, FREEZE_SUFFIX};
use crate::shell::crash_notice::written_at;

/// The end of the name of a record of the computer stopping.
pub const STOPPED_SUFFIX: &str = "-stopped.log";

/// The heading of a record of the computer stopping.
pub const STOPPED_TITLE: &str = "tilo computer stopped";

/// How much of a record `list` reads: its facts, never its stacks.
const HEAD_BYTES: u64 = 4 * 1024;
/// The most of one record `read` returns.
const MAX_TEXT: u64 = 1024 * 1024;
/// The most records `list` answers, newest first.
const MAX_LISTED: usize = 200;
/// The most of one persistent-memory record read at the start.
const MAX_PSTORE_ENTRY: u64 = 1024 * 1024;

/// Whether `name` is a record's: a file name the crash log writes.
fn is_record_name(name: &str) -> bool {
    !name.starts_with('.')
        && !name.contains(['/', '\\'])
        && Path::new(name).extension() == Some("log".as_ref())
        && written_at(name).is_some()
}

/// The value of the header field `key` (`version:   0.1.0`), from the facts
/// a record starts with, up to its first blank line. A placeholder the crash
/// log writes for a fact it could not read (`<none>`) is no value.
fn field<'a>(head: &'a str, key: &str) -> Option<&'a str> {
    head.lines()
        .take_while(|line| !line.trim().is_empty())
        .find_map(|line| line.strip_prefix(key)?.strip_prefix(':'))
        .map(str::trim)
        .filter(|value| !value.is_empty() && !value.starts_with('<'))
}

/// What the record `name`, starting with `head`, says happened. `None` for
/// a name the crash log does not write.
#[must_use]
pub fn summarize(name: &str, head: &str) -> Option<CrashRecord> {
    if !is_record_name(name) {
        return None;
    }
    let at = Local
        .from_local_datetime(&written_at(name)?)
        .earliest()?
        .timestamp();
    let kind = if name.ends_with(FREEZE_SUFFIX) {
        CrashKind::Froze
    } else if name.ends_with(STOPPED_SUFFIX) {
        CrashKind::ComputerStopped
    } else if field(head, "contained").is_some_and(|contained| contained.starts_with("yes")) {
        CrashKind::PartRestarted
    } else {
        CrashKind::Crashed
    };
    let reason = (kind == CrashKind::ComputerStopped)
        .then(|| field(head, "reason").and_then(reason_named))
        .flatten();
    Some(CrashRecord {
        id: name.to_owned(),
        at,
        kind,
        reason,
        version: field(head, "version").map(str::to_owned),
        build: field(head, "build-id").map(str::to_owned),
    })
}

/// The start of `path`, at most `limit` bytes, as text; whether more
/// followed. Opened without following a link.
fn read_start(path: &Path, limit: u64) -> std::io::Result<(String, bool)> {
    let mut bytes = Vec::new();
    tilo_files::native::private::read(path)?
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)?;
    let truncated = u64::try_from(bytes.len()).unwrap_or(u64::MAX) > limit;
    bytes.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
    Ok((String::from_utf8_lossy(&bytes).into_owned(), truncated))
}

/// Every record in `dir`, newest first, without its text.
#[must_use]
pub fn list(dir: &Path) -> Vec<CrashRecord> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let name = entry.file_name().into_string().ok()?;
            (is_record_name(&name) && entry.file_type().ok()?.is_file()).then_some(name)
        })
        .collect();
    names.sort_unstable_by(|a, b| b.cmp(a));
    names
        .into_iter()
        .take(MAX_LISTED)
        .filter_map(|name| {
            let (head, _) = read_start(&dir.join(&name), HEAD_BYTES).ok()?;
            summarize(&name, &head)
        })
        .collect()
}

/// The record `id` in `dir`, with its text.
///
/// # Errors
///
/// No record is called `id`, or it could not be read.
pub fn read(dir: &Path, id: &str) -> Result<CrashRecordText, String> {
    if !is_record_name(id) {
        return Err(format!("no record is called {id}"));
    }
    let (text, truncated) =
        read_start(&dir.join(id), MAX_TEXT).map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => format!("no record is called {id}"),
            _ => format!("the record {id} could not be read: {error}"),
        })?;
    let record = summarize(id, &text).ok_or_else(|| format!("no record is called {id}"))?;
    Ok(CrashRecordText {
        record,
        text,
        truncated,
    })
}

/// One record the kernel kept in persistent memory: its name there
/// (`dmesg-ramoops-0`), what it holds, and when it was written, if known.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PstoreEntry {
    pub name: String,
    pub content: Vec<u8>,
    /// Seconds since the Unix epoch.
    pub written: Option<i64>,
}

/// The computer stopping, as the kernel recorded it: why, when if known,
/// and its log up to then.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stop {
    pub reason: StopReason,
    pub written: Option<i64>,
    pub log: String,
}

/// The reason a record names, as the crash log and the kernel write it.
fn reason_named(name: &str) -> Option<StopReason> {
    match name.trim().to_ascii_lowercase().as_str() {
        "panic" => Some(StopReason::Panic),
        "oops" => Some(StopReason::Oops),
        "emergency" => Some(StopReason::Emergency),
        _ => None,
    }
}

const fn reason_word(reason: StopReason) -> &'static str {
    match reason {
        StopReason::Panic => "panic",
        StopReason::Oops => "oops",
        StopReason::Emergency => "emergency",
    }
}

/// The kernel's heading of one part of a log it dumped (`Panic#1 Part1`):
/// why, which dump of the boot and which part, the newest part first. A
/// dump at a clean shutdown is no stop.
fn dump_heading(first_line: &str) -> Option<(StopReason, u32, u32)> {
    let (why, rest) = first_line.trim().split_once('#')?;
    let (count, part) = rest.split_once(" Part")?;
    Some((
        reason_named(why)?,
        count.trim().parse().ok()?,
        part.trim().parse().ok()?,
    ))
}

/// The stops `entries` record: each dumped log, its parts joined oldest
/// first. The console log and other records the kernel keeps beside them
/// are not stops of their own.
#[must_use]
pub fn stops(entries: &[PstoreEntry]) -> Vec<Stop> {
    type Part<'a> = (u32, &'a str, Option<i64>);
    let texts: Vec<(String, &PstoreEntry)> = entries
        .iter()
        .filter(|entry| entry.name.starts_with("dmesg-") && !entry.name.ends_with(".enc.z"))
        .map(|entry| (String::from_utf8_lossy(&entry.content).into_owned(), entry))
        .collect();
    let mut dumps: Vec<((StopReason, u32), Vec<Part<'_>>)> = Vec::new();
    for (text, entry) in &texts {
        let (first, body) = text.split_once('\n').unwrap_or((text.as_str(), ""));
        let Some((reason, count, part)) = dump_heading(first) else {
            continue;
        };
        let key = (reason, count);
        let part = (part, body, entry.written);
        match dumps.iter_mut().find(|(held, _)| *held == key) {
            Some((_, parts)) => parts.push(part),
            None => dumps.push((key, vec![part])),
        }
    }
    dumps
        .into_iter()
        .map(|((reason, _), mut parts)| {
            parts.sort_by_key(|part| std::cmp::Reverse(part.0));
            Stop {
                reason,
                written: parts.iter().filter_map(|part| part.2).max(),
                log: parts
                    .iter()
                    .map(|part| part.1.trim_end_matches('\n'))
                    .collect::<Vec<_>>()
                    .join("\n"),
            }
        })
        .collect()
}

/// The records in the persistent-memory filesystem `dir`, as far as this
/// account may read them.
#[must_use]
pub fn read_pstore_dir(dir: &Path) -> Vec<PstoreEntry> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let name = entry.file_name().into_string().ok()?;
            if !entry.file_type().ok()?.is_file() {
                return None;
            }
            let file = std::fs::File::open(entry.path()).ok()?;
            // The kernel writes the dump's own time as the file's.
            let written = file
                .metadata()
                .and_then(|meta| meta.modified())
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .and_then(|since| i64::try_from(since.as_secs()).ok())
                .filter(|secs| *secs > 0);
            let mut content = Vec::new();
            file.take(MAX_PSTORE_ENTRY).read_to_end(&mut content).ok()?;
            Some(PstoreEntry {
                name,
                content,
                written,
            })
        })
        .collect()
}

/// The bytes a journal field holds in `journalctl -o json`: a string, or an
/// array of bytes for one that is not text.
fn journal_bytes(value: &serde_json::Value) -> Option<Vec<u8>> {
    match value {
        serde_json::Value::String(text) => Some(text.as_bytes().to_vec()),
        serde_json::Value::Array(items) => items
            .iter()
            .map(|item| item.as_u64().and_then(|byte| u8::try_from(byte).ok()))
            .collect(),
        _ => None,
    }
}

/// The records `systemd-pstore` moved into the journal, from the lines of
/// `journalctl -o json`: each names the record in its message
/// (`PStore dmesg-ramoops-0.`) and carries it as its `FILE` field. The
/// journal keeps no time of the dump itself.
#[must_use]
pub fn journal_entries(json_lines: &str) -> Vec<PstoreEntry> {
    json_lines
        .lines()
        .filter_map(|line| {
            let entry: serde_json::Value = serde_json::from_str(line).ok()?;
            let message = entry.get("MESSAGE")?.as_str()?;
            let named = message.strip_prefix("PStore ")?;
            let name = named
                .split_once(" moved to ")
                .map_or(named, |(name, _)| name)
                .trim_end_matches('.');
            Some(PstoreEntry {
                name: name.to_owned(),
                content: journal_bytes(entry.get("FILE")?)?,
                written: None,
            })
        })
        .collect()
}

/// The records `systemd-pstore` moved into this boot's journal, when this
/// account may read the system journal.
#[cfg(target_os = "linux")]
fn read_journal() -> Vec<PstoreEntry> {
    const JOURNALCTL: &str = "/usr/bin/journalctl";
    if !Path::new(JOURNALCTL).exists() {
        return Vec::new();
    }
    let output = bounded_command::run(
        JOURNALCTL,
        &[
            "-b",
            "0",
            "-q",
            "--no-pager",
            "-o",
            "json",
            "--all",
            "--output-fields=MESSAGE,FILE",
            "_SYSTEMD_UNIT=systemd-pstore.service",
        ],
        None,
        8 * 1024 * 1024,
        std::time::Duration::from_secs(5),
    );
    match output {
        Ok(output) => journal_entries(&String::from_utf8_lossy(&output.stdout)),
        Err(error) => {
            tracing::debug!(%error, "the journal's records of the computer stopping are unreadable");
            Vec::new()
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn read_journal() -> Vec<PstoreEntry> {
    Vec::new()
}

/// The record kept for `stop`, written as `stamp` (local time, the crash
/// log's file-name form).
#[must_use]
pub fn render_stop(stop: &Stop, stamp: &str) -> String {
    let rule = "=".repeat(STOPPED_TITLE.len());
    let when = if stop.written.is_some() {
        "when the system recorded it"
    } else {
        "the start that found it"
    };
    format!(
        "{STOPPED_TITLE}\n{rule}\n\
         timestamp: {stamp}\n\
         time-of:   {when}\n\
         reason:    {}\n\
         outcome:   the computer stopped and started again\n\n\
         {}\n",
        reason_word(stop.reason),
        stop.log,
    )
}

/// The name a kept stop ends with: its text's digest, which knows it again.
fn stop_tail(stop: &Stop) -> String {
    let digest = blake3::hash(stop.log.as_bytes()).to_hex();
    let short = digest.as_str().get(..16).unwrap_or_default();
    format!("-{short}{STOPPED_SUFFIX}")
}

/// Keep each of `stops` not already kept as a record in `dir`, timed when
/// the system recorded it or else `found_at` (seconds since the Unix epoch).
/// The names written.
///
/// # Errors
///
/// The directory could not be read; a record that could not be written is
/// left for the next start.
pub fn keep_stops(dir: &Path, stops: &[Stop], found_at: i64) -> std::io::Result<Vec<String>> {
    let mut held: Vec<String> = std::fs::read_dir(dir)?
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .collect();
    let mut written = Vec::new();
    for stop in stops {
        let tail = stop_tail(stop);
        if held.iter().any(|name| name.ends_with(&tail)) {
            continue;
        }
        let at = stop.written.unwrap_or(found_at);
        let Some(when) = Local.timestamp_opt(at, 0).earliest() else {
            continue;
        };
        // format: machine (the record's file name)
        let stamp = when.format(FILE_TIMESTAMP).to_string();
        let name = format!("{stamp}{tail}");
        match crate::crash_log::write_record(dir, &name, &render_stop(stop, &stamp)) {
            Ok(_) => {
                held.push(name.clone());
                written.push(name);
            }
            Err(error) => tracing::warn!(%error, "a record of the computer stopping was not kept"),
        }
    }
    Ok(written)
}

/// Keep the records of the computer stopping that the kernel left for this
/// start in `dir`.
pub fn take_in(dir: &Path) {
    // Each source's dumps are joined on their own: a dump numbered alike in
    // both is two dumps.
    let mut found = stops(&read_pstore_dir(Path::new("/sys/fs/pstore")));
    found.extend(stops(&read_journal()));
    if found.is_empty() {
        return;
    }
    if let Err(error) = keep_stops(dir, &found, Local::now().timestamp()) {
        tracing::warn!(%error, "the records of the computer stopping were not kept");
    }
}

const RECORD: ActionParam = ActionParam::required(
    "id",
    ParamType::Text,
    "The record's id, as the list answers it.",
);

const READ_PARAMS: &[ActionParam] = &[RECORD];

fn params(step: &str) -> &'static [ActionParam] {
    if step == wire::READ { READ_PARAMS } else { &[] }
}

/// `step`, described for the broker's catalogue: both only read.
fn descriptor(step: &str) -> tilo_actions::ActionDescriptor {
    use crate::shell::i18n::t;
    let (label, description, result) = if step == wire::READ {
        (
            "host-crash-records-read",
            "host-crash-records-read-detail",
            serde_json::json!({ "type": "object", "required": ["record", "text"] }),
        )
    } else {
        (
            "host-crash-records-list",
            "host-crash-records-list-detail",
            serde_json::json!({
                "type": "array",
                "items": { "type": "object", "required": ["id", "at", "kind"] }
            }),
        )
    };
    tilo_actions::ActionDescriptor {
        semantic_identity: None,
        address: tilo_actions::ActionAddress::new(wire::RESOURCE, step),
        resource_kind: wire::RESOURCE.into(),
        resource_label: t("host-crash-records-resource"),
        action_label: t(label),
        description: Some(t(description)),
        locus: tilo_actions::ExecutionLocus::default(),
        kind: tilo_actions::OperationKind::View,
        contract: tilo_actions::ActionContract {
            result: tilo_actions::ResultSpec::Json { schema: result },
            ..tilo_actions::ActionContract::view()
        },
        input: tilo_actions::InputSpec::None,
        params: params(step).to_vec(),
        view: None,
    }
}

/// `argument`, in its slot form, bound to `params`.
fn bind_slot(params: &[ActionParam], argument: Option<&str>) -> Result<ActionArgs, String> {
    let supplied = match argument {
        None | Some("") => serde_json::Map::new(),
        Some(slot) => {
            let json = slot
                .strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX)
                .ok_or("the records take JSON arguments")?;
            serde_json::from_str(json).map_err(|error| error.to_string())?
        }
    };
    ActionArgs::bind_json(params, &supplied).map_err(|error| error.to_string())
}

/// What `step` answers over the records in `dir`.
fn answer(
    step: &str,
    id: Option<&str>,
    dir: Option<&Path>,
) -> tilo_action_broker::services::HostStepOutcome {
    let failure = |message: String| {
        Box::new(tilo_actions::ActionFailure {
            code: "crash_records_unreadable".into(),
            message,
            details: None,
            retryable: false,
        })
    };
    let value = match (step, dir) {
        (wire::LIST, None) => Ok(serde_json::Value::Array(Vec::new())),
        (wire::LIST, Some(dir)) => serde_json::to_value(list(dir)).map_err(|e| e.to_string()),
        (_, None) => Err("no record is kept on this computer".to_owned()),
        (_, Some(dir)) => read(dir, id.unwrap_or_default())
            .and_then(|text| serde_json::to_value(text).map_err(|e| e.to_string())),
    }
    .map_err(failure)?;
    Ok(tilo_actions::ActionOutput {
        value: Some(value),
        resources: Vec::new(),
    })
}

/// This session's door to the records, for the broker.
pub(crate) fn broker_door() -> Arc<dyn tilo_action_broker::services::HostStepServices> {
    Arc::new(BrokerDoor { dir: None })
}

/// The records served to the broker: the session's crash directory, or
/// `dir` in a test.
pub(crate) struct BrokerDoor {
    pub(crate) dir: Option<PathBuf>,
}

impl tilo_action_broker::services::HostStepServices for BrokerDoor {
    fn provider(&self) -> &'static str {
        wire::PROVIDER
    }

    fn app_id(&self) -> &str {
        settings_sections::APP_ID
    }

    fn descriptors(&self) -> Vec<tilo_actions::ActionDescriptor> {
        wire::STEPS.into_iter().map(descriptor).collect()
    }

    fn start(
        &self,
        request: &tilo_action_broker::services::HostStepRequest<'_>,
    ) -> Result<Pending<tilo_action_broker::services::HostStepOutcome>, String> {
        let step = wire::STEPS
            .into_iter()
            .find(|step| {
                request.address.resource == wire::RESOURCE && request.address.action == *step
            })
            .ok_or("unknown step of the records")?;
        let args = bind_slot(params(step), request.argument)?;
        let id = args.text("id").map(|id| id.trim().to_owned());
        if step == wire::READ && id.as_deref().is_none_or(str::is_empty) {
            return Err("a record id is required".into());
        }
        let dir = self.dir.clone();
        let (complete, answered) = pending();
        // The files are read on a thread of their own, never the broker's.
        std::thread::Builder::new()
            .name("crash-records".into())
            .spawn(move || {
                let dir = dir.or_else(crate::crash_log::directory);
                complete.complete(answer(step, id.as_deref(), dir.as_deref()));
            })
            .map_err(|error| error.to_string())?;
        Ok(answered)
    }
}

#[cfg(test)]
#[path = "crash_records_tests.rs"]
mod tests;
