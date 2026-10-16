//! The reclaim record the installer wrote, read as strictly as the original:
//! schema 1, exactly its fields, lowercase GUIDs, bounded integers, distinct
//! identities, and staging partitions after the Tilo root.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use serde_json::{Map, Value};

use crate::{Error, py};

const RECORD_MAX_BYTES: usize = 16 * 1024;
const RECORD_KEYS: [&str; 9] = [
    "schema",
    "operation_id",
    "disk_guid",
    "disk_size_bytes",
    "sector_bytes",
    "staging_esp",
    "staging_root",
    "target_root",
    "target_esp_partuuid",
];

/// One recorded partition: its GUID and its first and last LBA.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extent {
    pub partuuid: String,
    pub first_lba: i128,
    pub last_lba: i128,
}

impl Extent {
    #[must_use]
    pub fn overlaps(&self, first: i128, last: i128) -> bool {
        self.first_lba <= last && first <= self.last_lba
    }
}

/// The installer's record of the staging partitions and the Tilo root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub disk_guid: String,
    pub disk_size_bytes: i128,
    pub sector_bytes: i128,
    pub staging_esp: Extent,
    pub staging_root: Extent,
    pub target_root: Extent,
}

fn mismatch(message: impl Into<String>) -> Error {
    Error::Mismatch(message.into())
}

/// `GUID_RE.match(text)`: a lowercase GUID (`$` also matches before one
/// trailing newline).
#[must_use]
pub fn is_guid(text: &str) -> bool {
    let text = text.strip_suffix('\n').unwrap_or(text);
    let groups: Vec<&str> = text.split('-').collect();
    groups.len() == 5
        && groups.iter().zip([8, 4, 4, 4, 12]).all(|(group, len)| {
            group.len() == len
                && group
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
}

fn bounded_int(value: Option<&Value>, label: &str, minimum: i128) -> Result<i128, Error> {
    match value.and_then(py::as_int) {
        Some(value) if value >= minimum && value < (1i128 << 62) => Ok(value),
        _ => Err(mismatch(format!(
            "record field {label} is not a bounded integer"
        ))),
    }
}

fn extent(value: Option<&Value>, label: &str) -> Result<Extent, Error> {
    let fields = match value {
        Some(Value::Object(fields))
            if fields.len() == 3
                && ["partuuid", "first_lba", "last_lba"]
                    .iter()
                    .all(|k| fields.contains_key(*k)) =>
        {
            fields
        }
        _ => {
            return Err(mismatch(format!(
                "record field {label} has unexpected shape"
            )));
        }
    };
    let partuuid = match fields.get("partuuid") {
        Some(Value::String(text)) if is_guid(text) => text.clone(),
        _ => {
            return Err(mismatch(format!(
                "record field {label}.partuuid is not a lowercase GUID"
            )));
        }
    };
    let first_lba = bounded_int(fields.get("first_lba"), &format!("{label}.first_lba"), 1)?;
    let last_lba = bounded_int(fields.get("last_lba"), &format!("{label}.last_lba"), 1)?;
    if last_lba < first_lba {
        return Err(mismatch(format!(
            "record field {label} ends before it starts"
        )));
    }
    Ok(Extent {
        partuuid,
        first_lba,
        last_lba,
    })
}

fn guid_field(raw: &Map<String, Value>, key: &str) -> Result<String, Error> {
    match raw.get(key) {
        Some(Value::String(text)) if is_guid(text) => Ok(text.clone()),
        _ => Err(mismatch(format!(
            "record field {key} is not a lowercase GUID"
        ))),
    }
}

/// Reads and validates the record at `path` (never through a symlink).
///
/// # Errors
/// `Error::Os` when it cannot be opened or read; `Error::Mismatch` for
/// anything that is not exactly a schema-1 record.
pub fn load(path: &Path) -> Result<Record, Error> {
    let fd = rustix::fs::open(
        path,
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map_err(|e| Error::os(&e.into(), path))?;
    let mut file = File::from(fd);
    // One read, as the original read it.
    let mut data = vec![0u8; RECORD_MAX_BYTES.saturating_add(1)];
    let read = loop {
        match file.read(&mut data) {
            Ok(read) => break read,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(Error::os(&e, path)),
        }
    };
    data.truncate(read);
    if data.len() > RECORD_MAX_BYTES {
        return Err(mismatch("record is unexpectedly large"));
    }
    let text = std::str::from_utf8(&data)
        .map_err(|e| mismatch(format!("record is not valid JSON: {e}")))?;
    let raw: Value = serde_json::from_str(text)
        .map_err(|e| mismatch(format!("record is not valid JSON: {e}")))?;
    let Value::Object(raw) = raw else {
        return Err(mismatch("record fields differ from schema 1"));
    };
    let keys_match =
        raw.len() == RECORD_KEYS.len() && RECORD_KEYS.iter().all(|k| raw.contains_key(*k));
    if !keys_match || !raw.get("schema").is_some_and(|s| py::equals_number(s, 1)) {
        return Err(mismatch("record fields differ from schema 1"));
    }
    let disk_guid = guid_field(&raw, "disk_guid")?;
    guid_field(&raw, "operation_id")?;
    guid_field(&raw, "target_esp_partuuid")?;
    let disk_size_bytes = bounded_int(raw.get("disk_size_bytes"), "disk_size_bytes", 1)?;
    let sector = raw.get("sector_bytes");
    let sector_bytes = match sector {
        Some(value) if py::equals_number(value, 512) => 512,
        Some(value) if py::equals_number(value, 4096) => 4096,
        _ => return Err(mismatch("record sector size is unsupported")),
    };
    let staging_esp = extent(raw.get("staging_esp"), "staging_esp")?;
    let staging_root = extent(raw.get("staging_root"), "staging_root")?;
    let target_root = extent(raw.get("target_root"), "target_root")?;
    let mut guids = vec![
        &staging_esp.partuuid,
        &staging_root.partuuid,
        &target_root.partuuid,
        &disk_guid,
    ];
    guids.sort_unstable();
    guids.dedup();
    if guids.len() != 4 {
        return Err(mismatch("record GUIDs are not distinct"));
    }
    if target_root.last_lba >= staging_esp.first_lba
        || staging_esp.overlaps(staging_root.first_lba, staging_root.last_lba)
    {
        return Err(mismatch(
            "record places the staging partitions before or inside the Tilo root",
        ));
    }
    Ok(Record {
        disk_guid,
        disk_size_bytes,
        sector_bytes,
        staging_esp,
        staging_root,
        target_root,
    })
}
