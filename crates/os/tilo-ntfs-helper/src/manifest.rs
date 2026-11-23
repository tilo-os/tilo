//! Native report schema-2 positive completion verifier. Native facts are untrusted, bounded data.
use crate::protocol::{Report, identity};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io,
};
use tilo_ntfs_protocol::{CopyTimes, CopyTimesUnavailableReason, Entry, Issue, NativePath};
pub struct Manifest {
    pub entries: Vec<Entry>,
    pub issues: Vec<Issue>,
    pub upcase: Vec<u16>,
}
fn bad() -> io::Error {
    io::Error::other("invalid or incomplete native manifest")
}
fn num(v: &Value, k: &str) -> io::Result<u64> {
    v.get(k).and_then(Value::as_u64).ok_or_else(bad)
}
fn text<'a>(v: &'a Value, k: &str) -> io::Result<&'a str> {
    v.get(k).and_then(Value::as_str).ok_or_else(bad)
}
fn kind(v: &Value) -> io::Result<&str> {
    text(v, "kind")
}
fn unhex(s: &str) -> io::Result<Vec<u8>> {
    hex::decode(s).map_err(|_| bad())
}
fn units(s: &str) -> io::Result<Vec<u16>> {
    let b = unhex(s)?;
    if !b.len().is_multiple_of(2) {
        return Err(bad());
    }
    b.chunks_exact(2)
        .map(|p| Ok(u16::from_le_bytes(p.try_into().map_err(io::Error::other)?)))
        .collect()
}
/// Digest of the actual bounded per-inode metadata returned by both operations.
/// # Errors
/// Refuses malformed JSON serialization.
pub fn metadata_digest(facts: &[Value], record: u64) -> io::Result<String> {
    let mut selected: Vec<_> = facts
        .iter()
        .filter(|f| {
            f.get("record").and_then(Value::as_u64) == Some(record)
                && f.get("reason").and_then(Value::as_str) != Some("reparse-or-hardlink-group")
                && matches!(
                    f.get("kind").and_then(Value::as_str),
                    Some("attribute" | "chunk" | "security" | "incomplete" | "attributes_end")
                )
        })
        .collect();
    let ends: Vec<_> = selected
        .iter()
        .enumerate()
        .filter(|(_, f)| f.get("kind").and_then(Value::as_str) == Some("attributes_end"))
        .map(|(i, _)| i)
        .collect();
    match ends.as_slice() {
        [_] => {}
        [first, _] => {
            let split = first.checked_add(1).ok_or_else(bad)?;
            let before = selected.get(..split).ok_or_else(bad)?;
            let after = selected.get(split..).ok_or_else(bad)?;
            if before != after {
                return Err(bad());
            }
            selected.truncate(split);
        }
        _ => return Err(bad()),
    }
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(&selected)?)))
}
fn groups(report: &Report) -> io::Result<(Vec<Vec<Value>>, Vec<u16>)> {
    let mut table = Vec::new();
    let mut groups: Vec<Vec<Value>> = Vec::new();
    for f in &report.facts {
        match kind(f)? {
            "read" | "volume" | "logfile" | "hibernation" => {}
            "entry" => groups.push(vec![f.clone()]),
            "chunk" if groups.is_empty() => {
                if num(f, "record")? != 10
                    || num(f, "attribute")? != 128
                    || num(f, "offset")? != u64::try_from(table.len()).map_err(io::Error::other)?
                {
                    return Err(bad());
                }
                table.extend(unhex(text(f, "hex")?)?);
                if table.len() > 131_072 {
                    return Err(bad());
                }
            }
            "attribute" | "chunk" | "security" | "incomplete" | "attributes_end"
            | "directory_end" => groups.last_mut().ok_or_else(bad)?.push(f.clone()),
            _ => return Err(bad()),
        }
    }
    if table.len() != 131_072 || groups.is_empty() || groups.len() > 4096 {
        return Err(bad());
    }
    let upcase: Vec<u16> = table
        .chunks_exact(2)
        .map(|p| {
            p.try_into()
                .map(u16::from_le_bytes)
                .map_err(io::Error::other)
        })
        .collect::<io::Result<_>>()?;
    Ok((groups, upcase))
}
#[derive(Default)]
struct Metadata {
    attributes_end: u32,
    directory_end: u32,
    size: Option<u64>,
    readable: bool,
    pending: Vec<String>,
    coverage: BTreeMap<(u64, String, Option<u64>), u64>,
    expected: BTreeMap<(u64, String, Option<u64>), u64>,
    security: Option<u64>,
    standard_information: StandardInformation,
}
#[derive(Default)]
struct StandardInformation {
    declarations: u32,
    instance: Option<u64>,
    length: Option<u64>,
    bytes: Vec<u8>,
    invalid: bool,
}
impl StandardInformation {
    fn attribute(&mut self, length: u64, flags: u64, instance: Option<u64>) {
        self.declarations = self.declarations.saturating_add(1);
        if self.declarations == 1 {
            self.length = Some(length);
            self.instance = instance;
            self.invalid = flags != 0 || instance.is_none();
        }
    }

    fn chunk(&mut self, instance: Option<u64>, offset: Option<u64>, hex: Option<&str>) {
        if self.declarations != 1 || instance != self.instance {
            self.invalid = true;
            return;
        }
        let Some(offset) = offset else {
            self.invalid = true;
            return;
        };
        let Ok(expected) = u64::try_from(self.bytes.len()) else {
            self.invalid = true;
            return;
        };
        let Some(hex) = hex else {
            self.invalid = true;
            return;
        };
        let Ok(data) = unhex(hex) else {
            self.invalid = true;
            return;
        };
        if offset != expected
            || data.is_empty()
            || data.len() > 4096
            || self.bytes.len().saturating_add(data.len()) > 72
        {
            self.invalid = true;
            return;
        }
        self.bytes.extend_from_slice(&data);
    }

    fn result(&self) -> (Option<CopyTimes>, Option<CopyTimesUnavailableReason>) {
        use CopyTimesUnavailableReason::{
            DuplicateStandardInformation, InvalidStandardInformationCoverage,
            MissingStandardInformation, UnsupportedStandardInformationLayout,
        };
        if self.declarations == 0 {
            return (None, Some(MissingStandardInformation));
        }
        if self.declarations != 1 {
            return (None, Some(DuplicateStandardInformation));
        }
        if !matches!(self.length, Some(48 | 72)) {
            return (None, Some(UnsupportedStandardInformationLayout));
        }
        if self.invalid
            || usize::try_from(self.length.unwrap_or_default()).ok() != Some(self.bytes.len())
        {
            return (None, Some(InvalidStandardInformationCoverage));
        }
        let Some(last_data_change) = self.bytes.get(8..16) else {
            return (None, Some(InvalidStandardInformationCoverage));
        };
        let Some(last_access) = self.bytes.get(24..32) else {
            return (None, Some(InvalidStandardInformationCoverage));
        };
        let Ok(last_data_change) = <[u8; 8]>::try_from(last_data_change) else {
            return (None, Some(InvalidStandardInformationCoverage));
        };
        let Ok(last_access) = <[u8; 8]>::try_from(last_access) else {
            return (None, Some(InvalidStandardInformationCoverage));
        };
        (
            Some(CopyTimes {
                last_access_100ns: i64::from_le_bytes(last_access),
                last_data_change_100ns: i64::from_le_bytes(last_data_change),
            }),
            None,
        )
    }
}
impl Metadata {
    fn attribute(&mut self, f: &Value, directory: bool) -> io::Result<()> {
        if self.attributes_end != 0 {
            return Err(bad());
        }
        let ty = num(f, "type")?;
        let attr_name = text(f, "name")?.to_owned();
        let length = num(f, "size")?;
        let flags = num(f, "flags")?;
        let instance = f.get("instance").and_then(Value::as_u64);
        let standard_information = ty == 16 && attr_name.is_empty();
        if standard_information {
            self.standard_information.attribute(length, flags, instance);
        }
        if ty == 128 && attr_name.is_empty() && !directory {
            if self.size.replace(length).is_some() {
                self.pending.push("attribute-extents".into());
                self.readable = false;
            }
            if flags != 0 {
                self.readable = false;
                self.pending.push("data-flags".into());
            }
        } else {
            let instance = if standard_information {
                instance
            } else {
                Some(num(f, "instance")?)
            };
            self.expected
                .insert((ty, attr_name.clone(), instance), length);
        }
        if ty == 128 && !attr_name.is_empty() {
            self.pending.push("named-data-stream".into());
        }
        if ![16, 48, 80, 128, 144, 160, 176].contains(&ty) {
            self.pending.push("custom-or-extent-metadata".into());
        }
        Ok(())
    }
    fn fact(&mut self, f: &Value, record: u64, sequence: u16, directory: bool) -> io::Result<()> {
        if num(f, "record")? != record {
            return Err(bad());
        }
        match kind(f)? {
            "attributes_end" => {
                if num(f, "sequence")? != u64::from(sequence) {
                    return Err(bad());
                }
                self.attributes_end = self.attributes_end.saturating_add(1);
            }
            "directory_end" => {
                if self.attributes_end != 1 {
                    return Err(bad());
                }
                if num(f, "sequence")? != u64::from(sequence) {
                    return Err(bad());
                }
                self.directory_end = self.directory_end.saturating_add(1);
            }
            "attribute" => self.attribute(f, directory)?,
            "chunk" => {
                if self.attributes_end != 0 {
                    return Err(bad());
                }
                let ty = num(f, "attribute")?;
                let name = text(f, "name")?.to_owned();
                let instance = f.get("instance").and_then(Value::as_u64);
                if ty == 16 && name.is_empty() {
                    self.standard_information.chunk(
                        instance,
                        f.get("offset").and_then(Value::as_u64),
                        f.get("hex").and_then(Value::as_str),
                    );
                    self.coverage.insert(
                        (ty, name, instance),
                        u64::try_from(self.standard_information.bytes.len())
                            .map_err(io::Error::other)?,
                    );
                    return Ok(());
                }
                let k = (ty, name, instance);
                let at = self.coverage.entry(k).or_default();
                if num(f, "offset")? != *at {
                    return Err(bad());
                }
                let data = unhex(text(f, "hex")?)?;
                if data.is_empty() || data.len() > 4096 {
                    return Err(bad());
                }
                *at = at
                    .checked_add(u64::try_from(data.len()).map_err(io::Error::other)?)
                    .ok_or_else(bad)?;
            }
            "security" => {
                if self.attributes_end != 0 {
                    return Err(bad());
                }
                if self.security.replace(num(f, "size")?).is_some()
                    || !["attribute", "sds"].contains(&text(f, "source")?)
                {
                    return Err(bad());
                }
            }
            "incomplete" => self.pending.push(text(f, "reason")?.to_owned()),
            _ => return Err(bad()),
        }

        Ok(())
    }
}
/// Requires positive scoped completion, native key agreement and exact metadata coverage.
/// No ACL policy is promoted: content readability and fidelity are separate facts.
/// # Errors
/// Refuses structural uncertainty, missing or duplicate completions and malformed facts.
pub fn verify(report: &Report, root: u64) -> io::Result<Manifest> {
    if report.schema != 2 || report.status != 0 || report.stats.writes != 0 {
        return Err(bad());
    }
    identity(report, root)?;
    let (groups, upcase) = groups(report)?;
    let payload = serde_json::to_vec(&upcase)?.len();
    let mut v = Verifier {
        payload,
        root,
        upcase,
        paths: BTreeMap::new(),
        keys: BTreeSet::new(),
        entries: vec![],
        issues: vec![],
    };
    for group in &groups {
        v.accept(group)?;
    }
    Ok(Manifest {
        entries: v.entries,
        issues: v.issues,
        upcase: v.upcase,
    })
}
struct Verifier {
    payload: usize,
    root: u64,
    upcase: Vec<u16>,
    paths: BTreeMap<u64, (NativePath, NativePath, bool)>,
    keys: BTreeSet<NativePath>,
    entries: Vec<Entry>,
    issues: Vec<Issue>,
}
impl Verifier {
    fn locate(&mut self, head: &Value) -> io::Result<(u64, u16, bool, NativePath, NativePath)> {
        let record = num(head, "record")?;
        let sequence = u16::try_from(num(head, "sequence")?).map_err(io::Error::other)?;
        let directory = head
            .get("directory")
            .and_then(Value::as_bool)
            .ok_or_else(bad)?;
        let name = units(text(head, "name")?)?;
        let key = units(text(head, "key")?)?;
        let (mut path, mut canonical) = if self.entries.is_empty() {
            if record != self.root || !name.is_empty() || !directory {
                return Err(bad());
            }
            (vec![], vec![])
        } else {
            if name.is_empty()
                || name.len() > 255
                || name.contains(&0)
                || name == [46]
                || name == [46, 46]
                || name.contains(&92)
                || name.contains(&47)
            {
                return Err(bad());
            }
            let (p, k, isdir) = self.paths.get(&num(head, "parent")?).ok_or_else(bad)?;
            if !isdir {
                return Err(bad());
            }
            (p.clone(), k.clone())
        };
        let calculated: Vec<u16> = name
            .iter()
            .map(|u| self.upcase.get(usize::from(*u)).copied().ok_or_else(bad))
            .collect::<io::Result<_>>()?;
        if key != calculated {
            return Err(bad());
        }
        if !self.entries.is_empty() {
            path.push(name);
            canonical.push(key);
        }
        if path.len() > 64
            || path
                .iter()
                .map(Vec::len)
                .sum::<usize>()
                .saturating_add(path.len())
                > 4096
            || !self.keys.insert(canonical.clone())
        {
            return Err(bad());
        }
        Ok((record, sequence, directory, path, canonical))
    }
    fn budget(&mut self, entry: &Entry) -> io::Result<()> {
        self.payload = self
            .payload
            .saturating_add(serde_json::to_vec(&entry)?.len());
        // Reserve bounded framing and issue text in addition to entry/name bytes.
        for reason in &entry.fidelity_pending {
            self.payload = self
                .payload
                .saturating_add(
                    serde_json::to_vec(&Issue {
                        path: if entry.path.is_empty() {
                            None
                        } else {
                            Some(entry.path.clone())
                        },
                        reason: reason.clone(),
                    })?
                    .len(),
                )
                .saturating_add(256);
        }
        if self.payload > 4 * 1024 * 1024 {
            return Err(bad());
        }
        Ok(())
    }
    fn record_pending(&mut self, path: &[Vec<u16>], pending: &[String]) {
        for reason in pending {
            self.issues.push(Issue {
                path: if path.is_empty() {
                    None
                } else {
                    Some(path.to_vec())
                },
                reason: reason.clone(),
            });
        }
    }
    fn accept(&mut self, group: &[Value]) -> io::Result<()> {
        let head = group.first().ok_or_else(bad)?;
        let (record, sequence, directory, path, canonical) = self.locate(head)?;
        let mut meta = Metadata {
            readable: !directory,
            pending: vec!["windows-acl-policy-unverified".into()],
            ..Metadata::default()
        };
        for f in group.iter().skip(1) {
            meta.fact(f, record, sequence, directory)?;
        }
        if meta.attributes_end != 1
            || meta.directory_end > 1
            || (!directory && meta.directory_end != 0)
        {
            return Err(bad());
        }
        if directory
            && meta.directory_end == 0
            && num(head, "dos_flags")? & 0x400 == 0
            && num(head, "namespace")? != 2
        {
            return Err(bad());
        }
        if num(head, "namespace")? == 2 {
            self.issues.push(Issue {
                path: Some(path),
                reason: "dos-alias-relationship-pending".into(),
            });
            self.issues.push(Issue {
                path: None,
                reason: "external-alias-closure-unknown".into(),
            });
            return Ok(());
        }
        let enumeration_complete = !directory || meta.directory_end == 1;
        if !enumeration_complete {
            meta.pending.push("directory-enumeration".into());
        }
        let flags = num(head, "dos_flags")?;
        if flags & (0x400 | 0x4000 | 0x800 | 0x200) != 0 {
            meta.readable = false;
            meta.pending.push("unsupported-dos-flags".into());
        }
        if flags & !0x20 != 0 {
            meta.pending.push("dos-metadata".into());
        }
        if num(head, "links")? > 1 || num(head, "namespace")? == 2 {
            meta.pending.push("hardlink-or-dos-alias".into());
            self.issues.push(Issue {
                path: None,
                reason: "external-alias-closure-unknown".into(),
            });
        }
        for ((ty, name, instance), length) in meta.expected {
            let covered = meta
                .coverage
                .get(&(ty, name.clone(), instance))
                .or_else(|| meta.coverage.get(&(ty, name, None)))
                .copied()
                .unwrap_or(0);
            if covered != length {
                meta.pending.push("metadata-meta.coverage".into());
            }
        }
        if meta.security.is_none() {
            meta.pending.push("meta.security-provenance".into());
        }
        meta.readable &= meta.size.is_some() && record >= 16;
        let (copy_times, copy_times_unavailable_reason) = meta.standard_information.result();
        self.record_pending(&path, &meta.pending);
        let entry = Entry {
            token: String::new(),
            path: path.clone(),
            key: canonical.clone(),
            record,
            sequence,
            size: meta.size.unwrap_or(0),
            directory,
            metadata_digest: metadata_digest(group, record)?,
            content_readable: meta.readable,
            enumeration_complete,
            copy_times,
            copy_times_unavailable_reason,
            fidelity_pending: meta.pending,
        };
        if self
            .paths
            .insert(record, (path, canonical, directory))
            .is_some()
            && directory
        {
            return Err(bad());
        }
        self.budget(&entry)?;
        self.entries.push(entry);
        Ok(())
    }
}
