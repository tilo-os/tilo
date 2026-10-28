//! Administrative native Copy. Session facts are expectations, never restart grants.
use super::migration::{
    self, CopyProgress, CopyReport, PinnedDirectory, StagedFile, changed, check_cancel,
};
use super::migration_native_journal::{NativeJournal, Receipt};
use super::migration_source::{CopyFileSource, CopyPass, CopyTimes, SourcePass, sealed};
use super::{migration_durability as durability, nofollow::metadata_nofollow};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    fs::File,
    io,
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
};
use tilo_ntfs_protocol::{
    CHUNK_BYTES, Entry, HASH_BYTES, Identity, Issue, Manifest, NativePath, ntfs_time_to_system_time,
};

mod transport_sealed {
    pub trait Sealed {}
}
trait Transport: transport_sealed::Sealed {
    fn identity(&self) -> &Identity;
    fn begin(&mut self) -> io::Result<Manifest>;
    fn read_chunk(&mut self, token: &str, offset: u64, length: u32) -> io::Result<Vec<u8>>;
    fn finish(&mut self) -> io::Result<(BTreeMap<String, String>, Vec<Issue>)>;
}
#[cfg(target_os = "linux")]
impl transport_sealed::Sealed for tilo_ntfs_protocol::client::Client {}
#[cfg(target_os = "linux")]
impl Transport for tilo_ntfs_protocol::client::Client {
    fn identity(&self) -> &Identity {
        self.identity()
    }
    fn begin(&mut self) -> io::Result<Manifest> {
        self.begin()
    }
    fn read_chunk(&mut self, token: &str, offset: u64, length: u32) -> io::Result<Vec<u8>> {
        self.read_chunk(token, offset, length)
    }
    fn finish(&mut self) -> io::Result<(BTreeMap<String, String>, Vec<Issue>)> {
        self.finish()
    }
}
/// Durable source facts deliberately exclude observation tokens.
pub mod entry_facts {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use tilo_ntfs_protocol::Entry;
    pub fn serialize<S: Serializer>(entry: &Entry, serializer: S) -> Result<S::Ok, S::Error> {
        if !entry.token.is_empty() {
            return Err(serde::ser::Error::custom(
                "observation tokens cannot be persisted",
            ));
        }
        let mut value = serde_json::to_value(entry).map_err(serde::ser::Error::custom)?;
        value
            .as_object_mut()
            .ok_or_else(|| serde::ser::Error::custom("native facts are not an object"))?
            .remove("token");
        value.serialize(serializer)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Entry, D::Error> {
        let mut value = serde_json::Value::deserialize(deserializer)?;
        let object = value
            .as_object_mut()
            .ok_or_else(|| serde::de::Error::custom("native facts are not an object"))?;
        if object.contains_key("token") {
            return Err(serde::de::Error::custom(
                "persisted observation token refused",
            ));
        }
        object.insert("token".into(), serde_json::Value::String(String::new()));
        serde_json::from_value(value).map_err(serde::de::Error::custom)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanEntry {
    #[serde(with = "entry_facts")]
    pub entry: Entry,
    pub mapped: Option<PathBuf>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub identity: Identity,
    pub entries: Vec<PlanEntry>,
    pub issues: Vec<Issue>,
}
impl Plan {
    pub fn revision(&self) -> io::Result<[u8; 32]> {
        Ok(Sha256::digest(serde_json::to_vec(&(
            &self.identity,
            &self.entries,
            &self.issues,
        ))?)
        .into())
    }
    fn from_manifest(identity: &Identity, manifest: &Manifest) -> io::Result<Self> {
        if manifest.entries.is_empty()
            || manifest.entries.len() > 4096
            || manifest.issues.len() > 16384
            || serde_json::to_vec(&(&manifest.entries, &manifest.issues))?.len() > 4 * 1024 * 1024
        {
            return Err(changed("native inventory exceeds bounds or is empty"));
        }
        let mut paths = BTreeSet::new();
        let mut tokens = BTreeSet::new();
        let mut keys = BTreeSet::new();
        let mut entries = Vec::new();
        for entry in &manifest.entries {
            if !entry.copy_times_valid()
                || entry.path.len() > 64
                || entry.path.iter().map(Vec::len).sum::<usize>() > 4096
                || entry.path.iter().any(|c| c.is_empty() || c.len() > 255)
                || !paths.insert(entry.path.clone())
                || !keys.insert(entry.key.clone())
                || entry.token.is_empty()
                || !tokens.insert(entry.token.clone())
            {
                return Err(changed("invalid or ambiguous native manifest"));
            }
            let mut persistent = entry.clone();
            persistent.token.clear();
            entries.push(PlanEntry {
                entry: persistent,
                mapped: map_path(&entry.path).ok(),
            });
        }
        entries.sort_by(|a, b| a.entry.path.cmp(&b.entry.path));
        if !entries.first().is_some_and(|e| {
            e.entry.path.is_empty() && e.entry.directory && e.entry.enumeration_complete
        }) {
            return Err(changed("native root enumeration incomplete"));
        }
        for e in &entries {
            if !e.entry.path.is_empty() {
                let (_, parent) = e
                    .entry
                    .path
                    .split_last()
                    .ok_or_else(|| changed("native parent missing"))?;
                if !entries
                    .iter()
                    .any(|p| p.entry.path == parent && p.entry.directory)
                {
                    return Err(changed("native manifest parent missing"));
                }
            }
        }
        Ok(Self {
            identity: identity.clone(),
            entries,
            issues: manifest.issues.clone(),
        })
    }
}
fn map_path(path: &NativePath) -> io::Result<PathBuf> {
    let mut mapped = PathBuf::new();
    for component in path {
        let text =
            String::from_utf16(component).map_err(|_| changed("unpaired UTF-16 surrogate"))?;
        if text.is_empty()
            || text.len() > 255
            || text == "."
            || text == ".."
            || text.contains(['/', '\\', '\0'])
            || text.encode_utf16().collect::<Vec<_>>() != *component
        {
            return Err(changed("native name cannot map exactly"));
        }
        mapped.push(text);
    }
    if mapped.as_os_str().len() > 4096 {
        return Err(changed("mapped path exceeds destination bound"));
    }
    Ok(mapped)
}
fn fidelity(reason: &str) -> bool {
    matches!(
        reason,
        "windows-acl-policy-unverified"
            | "named-data-stream"
            | "dos-metadata"
            | "hardlink-or-dos-alias"
            | "dos-alias-relationship-pending"
    )
}
fn entry_fidelity(entry: &Entry, reason: &str) -> bool {
    fidelity(reason)
        || (reason == "reparse-or-hardlink-group"
            && entry.content_readable
            && entry
                .fidelity_pending
                .iter()
                .any(|r| r == "hardlink-or-dos-alias")
            && !entry
                .fidelity_pending
                .iter()
                .any(|r| r == "unsupported-dos-flags"))
}
pub(crate) fn times(entry: &Entry) -> io::Result<CopyTimes> {
    let value = entry
        .copy_times
        .ok_or_else(|| changed("exact native timestamps unavailable"))?;
    Ok(CopyTimes {
        accessed: ntfs_time_to_system_time(value.last_access_100ns)
            .ok_or_else(|| changed("native access time out of range"))?,
        modified: ntfs_time_to_system_time(value.last_data_change_100ns)
            .ok_or_else(|| changed("native modification time out of range"))?,
    })
}
fn request_cost(size: u64, remaining: u64) -> io::Result<u64> {
    let bytes = size
        .checked_mul(2)
        .ok_or_else(|| changed("native two-pass byte overflow"))?;
    let chunks = size
        .div_ceil(u64::from(CHUNK_BYTES))
        .checked_mul(2)
        .ok_or_else(|| changed("native request overflow"))?;
    if bytes > HASH_BYTES || chunks > remaining {
        return Err(changed(
            "native two-pass budget exhausted; fresh selected resume required",
        ));
    }
    Ok(chunks)
}
struct Session<T: Transport> {
    transport: T,
    plan: Plan,
    remaining: u64,
    generation: u64,
}
impl<T: Transport> Session<T> {
    fn selected(mut transport: T, cancel: &AtomicBool) -> io::Result<Self> {
        check_cancel(cancel)?;
        let result = transport.begin();
        check_cancel(cancel)?;
        let manifest = result?;
        let plan = Plan::from_manifest(transport.identity(), &manifest)?;
        let mut session = Self {
            transport,
            plan,
            remaining: 4096,
            generation: manifest.generation,
        };
        session.finish(&manifest, None, cancel)?;
        Ok(session)
    }
    fn begin(&mut self, cancel: &AtomicBool) -> io::Result<Manifest> {
        check_cancel(cancel)?;
        let result = self.transport.begin();
        check_cancel(cancel)?;
        let manifest = result?;
        if manifest.generation <= self.generation
            || Plan::from_manifest(self.transport.identity(), &manifest)? != self.plan
        {
            return Err(changed(
                "native identity, generation or reviewed inventory changed",
            ));
        }
        self.generation = manifest.generation;
        Ok(manifest)
    }
    fn finish(
        &mut self,
        manifest: &Manifest,
        selected: Option<(&Entry, [u8; 32])>,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        check_cancel(cancel)?;
        let result = self.transport.finish();
        check_cancel(cancel)?;
        let (hashes, issues) = result?;
        let empty = hex::encode(Sha256::digest([]));
        let mut expected = BTreeMap::new();
        let mut expected_issues = Vec::new();
        for e in &manifest.entries {
            if !e.directory {
                if e.size == 0 && e.content_readable {
                    expected.insert(e.token.clone(), empty.clone());
                } else if selected.is_none_or(|(s, _)| s.token != e.token) {
                    expected_issues.push(Issue {
                        path: Some(e.path.clone()),
                        reason: "content-hash-incomplete".into(),
                    });
                }
            }
        }
        if let Some((entry, digest)) = selected {
            expected.insert(entry.token.clone(), hex::encode(digest));
        }
        if hashes != expected {
            return Err(changed("native postflight hashes disagree"));
        }
        // Multiset equality rejects unknown, duplicate and selected incomplete issues.
        let mut actual: Vec<_> = issues
            .iter()
            .map(serde_json::to_string)
            .collect::<Result<_, _>>()?;
        let mut expected: Vec<_> = expected_issues
            .iter()
            .map(serde_json::to_string)
            .collect::<Result<_, _>>()?;
        actual.sort();
        expected.sort();
        if actual != expected {
            return Err(changed("unexpected native postflight issue"));
        }
        Ok(())
    }
    fn metadata(&mut self, cancel: &AtomicBool) -> io::Result<()> {
        let manifest = self.begin(cancel)?;
        self.finish(&manifest, None, cancel)
    }
    fn file<'a>(
        &'a mut self,
        entry: &Entry,
        cancel: &'a AtomicBool,
    ) -> io::Result<NativeFile<'a, T>> {
        let cost = request_cost(entry.size, self.remaining)?;
        let manifest = self.begin(cancel)?;
        let selected = manifest
            .entries
            .iter()
            .find(|e| e.path == entry.path)
            .ok_or_else(|| changed("native selected file missing"))?
            .clone();
        if selected.directory || !selected.content_readable {
            return Err(changed("native selected content unreadable"));
        }
        self.remaining -= cost;
        Ok(NativeFile {
            session: self,
            manifest,
            entry: selected,
            cancel,
            offset: 0,
            buffer: Vec::new(),
            at: 0,
            passes: 0,
            attested: false,
        })
    }
}
struct NativeFile<'a, T: Transport> {
    session: &'a mut Session<T>,
    manifest: Manifest,
    entry: Entry,
    cancel: &'a AtomicBool,
    offset: u64,
    buffer: Vec<u8>,
    at: usize,
    passes: u8,
    attested: bool,
}
impl<T: Transport> sealed::Sealed for NativeFile<'_, T> {}
impl<T: Transport> CopyFileSource for NativeFile<'_, T> {
    type Receipt = Receipt;
    fn expected_len(&self) -> u64 {
        self.entry.size
    }
    fn times(&self) -> io::Result<CopyTimes> {
        times(&self.entry)
    }
    fn begin_pass(&mut self, pass: CopyPass) -> io::Result<()> {
        let valid = matches!(
            (self.passes, pass),
            (0, CopyPass::Initial) | (1, CopyPass::IndependentReread)
        );
        if !valid || (self.passes == 1 && self.offset != self.entry.size) {
            return Err(changed("native pass is incomplete"));
        }
        self.passes += 1;
        self.offset = 0;
        self.buffer.clear();
        self.at = 0;
        Ok(())
    }
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        check_cancel(self.cancel)?;
        if self.offset == self.entry.size || out.is_empty() {
            return Ok(0);
        }
        if self.at == self.buffer.len() {
            let length = u32::try_from((self.entry.size - self.offset).min(u64::from(CHUNK_BYTES)))
                .map_err(|_| changed("native range overflow"))?;
            let result = self
                .session
                .transport
                .read_chunk(&self.entry.token, self.offset, length);
            check_cancel(self.cancel)?;
            self.buffer = result?;
            self.at = 0;
            if self.buffer.len() != length as usize {
                return Err(changed("native range length mismatch"));
            }
        }
        let count = out.len().min(self.buffer.len() - self.at);
        out.get_mut(..count)
            .ok_or_else(|| changed("native output buffer bounds"))?
            .copy_from_slice(
                self.buffer
                    .get(self.at..self.at + count)
                    .ok_or_else(|| changed("native range buffer bounds"))?,
            );
        self.at += count;
        self.offset += count as u64;
        Ok(count)
    }
    fn attest(&mut self, bytes: u64, digest: [u8; 32], cancel: &AtomicBool) -> io::Result<()> {
        if self.passes != 2 || self.offset != self.entry.size || bytes != self.entry.size {
            return Err(changed("native independent passes incomplete"));
        }
        self.session
            .finish(&self.manifest, Some((&self.entry, digest)), cancel)?;
        self.attested = true;
        Ok(())
    }
    fn receipt(
        &self,
        destination: &File,
        bytes: u64,
        digest: [u8; 32],
        _: bool,
    ) -> io::Result<Receipt> {
        if !self.attested {
            return Err(changed("native source lacks final attestation"));
        }
        let mut entry = self.entry.clone();
        entry.token.clear();
        Ok(Receipt {
            source: entry,
            destination: durability::identity(destination)?,
            bytes,
            digest: Some(digest),
            completed: true,
        })
    }
}
fn revalidate<T: Transport>(
    session: &mut Session<T>,
    receipt: &Receipt,
    parent: &Arc<PinnedDirectory>,
    name: &OsStr,
    cancel: &AtomicBool,
) -> io::Result<()> {
    use super::migration_native_journal::open_file;
    let mut destination = open_file(parent, name)?;
    let before = cap_primitives::fs::Metadata::from_file(&destination)?;
    let expected_times = times(&receipt.source)?;
    let meta = destination.metadata()?;
    if durability::identity(&destination)? != receipt.destination
        || meta.len() != receipt.bytes
        || meta.accessed()? != expected_times.accessed
        || meta.modified()? != expected_times.modified
    {
        return Err(changed(
            "receipted destination identity, size or timestamps changed",
        ));
    }
    let digest = receipt
        .digest
        .ok_or_else(|| changed("native file receipt hash missing"))?;
    let mut source = session.file(&receipt.source, cancel)?;
    // Resumed receipts receive the same independent native pass and postflight checks.
    for pass in [CopyPass::Initial, CopyPass::IndependentReread] {
        source.begin_pass(pass)?;
        if migration::stream(&mut SourcePass(&mut source), None, receipt.bytes, cancel)?
            != (receipt.bytes, digest)
        {
            return Err(changed("receipted native bytes changed"));
        }
    }
    if migration::stream(&mut destination, None, receipt.bytes, cancel)? != (receipt.bytes, digest)
    {
        return Err(changed("receipted destination bytes changed"));
    }
    let validate = || {
        parent.validate_destination()?;
        if !migration::same_snapshot(
            &before,
            &cap_primitives::fs::Metadata::from_file(&destination)?,
        ) || !migration::same_snapshot(
            &before,
            &metadata_nofollow(&parent.file, Path::new(name))?,
        ) {
            return Err(changed("receipted destination changed during verification"));
        }
        Ok(())
    };
    validate()?;
    source.attest(receipt.bytes, digest, cancel)?;
    check_cancel(cancel)?;
    validate()?;
    destination.set_times(std::fs::FileTimes::new().set_accessed(expected_times.accessed))?;
    if destination.metadata()?.modified()? != expected_times.modified {
        return Err(changed("receipted destination timestamp changed"));
    }
    durability::sync_file(&destination)
}
fn enter_directory<T: Transport>(
    session: &mut Session<T>,
    journal: &mut NativeJournal<'_>,
    parent: &Arc<PinnedDirectory>,
    entry: &Entry,
    name: &OsStr,
    cancel: &AtomicBool,
) -> io::Result<Arc<PinnedDirectory>> {
    session.metadata(cancel)?;
    let directory = if let Some(receipt) = journal.receipts.get(&entry.path) {
        let meta = metadata_nofollow(&parent.file, Path::new(name))?;
        let child = parent.open_child(name, &meta)?;
        if super::migration_native_journal::directory_identity(&child)? != receipt.destination {
            return Err(changed("native destination directory changed"));
        }
        journal.validate_completed(entry, &child)?;
        child
    } else {
        let child = parent.create_child(name)?;
        super::migration_native_journal::sync_created_directory(&child, parent)?;
        journal.record_directory(entry, &child, false)?;
        child
    };
    Ok(directory)
}
fn finish_directories<T: Transport>(
    session: &mut Session<T>,
    journal: &mut NativeJournal<'_>,
    plan: &Plan,
    directories: &BTreeMap<NativePath, Arc<PinnedDirectory>>,
    cancel: &AtomicBool,
) -> io::Result<()> {
    for item in plan.entries.iter().rev().filter(|i| i.entry.directory) {
        if let Some(directory) = directories.get(&item.entry.path) {
            session.metadata(cancel)?;
            check_cancel(cancel)?;
            let values = times(&item.entry)?;
            super::migration_journal::persistence_result(
                (|| {
                    if journal.validate_completed(&item.entry, directory)? {
                        let writable = durability::readable_directory(&directory.file)?;
                        writable
                            .set_times(std::fs::FileTimes::new().set_accessed(values.accessed))?;
                        if writable.metadata()?.modified()? != values.modified {
                            return Err(changed(
                                "completed destination directory timestamp changed",
                            ));
                        }
                    } else {
                        migration::preserve_directory_times_values(
                            directory,
                            values.accessed,
                            values.modified,
                        )?;
                    }
                    durability::sync_directory(&directory.file)
                })(),
                super::migration_journal::PersistenceBoundary::OutputPublicationUncertain,
            )?;
            journal.record_directory(&item.entry, directory, true)?;
        }
    }
    session.metadata(cancel)?;
    check_cancel(cancel)?;
    Ok(())
}
fn scope_issues(plan: &Plan, report: &mut CopyReport) -> Vec<NativePath> {
    for item in &plan.entries {
        for reason in &item.entry.fidelity_pending {
            report
                .metadata_limitations
                .push(format!("{:04x?}: {reason}", item.entry.path));
        }
    }
    let mut blocked = Vec::<NativePath>::new();
    for issue in &plan.issues {
        if !fidelity(&issue.reason)
            && !issue.path.as_ref().is_some_and(|path| {
                plan.entries
                    .iter()
                    .any(|e| e.entry.path == *path && entry_fidelity(&e.entry, &issue.reason))
            })
        {
            let native = issue.path.as_deref().unwrap_or(&[]);
            report.skipped.push((
                map_path(&native.to_vec()).unwrap_or_default(),
                format!("native scope {native:04x?}: {}", issue.reason),
            ));
            // An ambiguous root remains incomplete, but independently readable siblings may copy.
            if !native.is_empty() {
                blocked.push(native.to_vec());
            }
        }
    }
    blocked
}
fn enter_root<T: Transport>(
    session: &mut Session<T>,
    journal: &mut NativeJournal<'_>,
    plan: &Plan,
    cancel: &AtomicBool,
) -> io::Result<Arc<PinnedDirectory>> {
    let root = plan
        .entries
        .first()
        .ok_or_else(|| changed("native root missing"))?;
    times(&root.entry)?;
    if root
        .entry
        .fidelity_pending
        .iter()
        .any(|r| !entry_fidelity(&root.entry, r))
    {
        return Err(changed("native root metadata safety is incomplete"));
    }
    session.metadata(cancel)?;
    let root_directory = journal.root(&root.entry)?;
    Ok(root_directory)
}
fn run<T: Transport>(
    transport: T,
    destination: Option<&Path>,
    journal_path: &Path,
    cancel: &AtomicBool,
    mut progress: impl FnMut(CopyProgress),
) -> io::Result<CopyReport> {
    let mut session = Session::selected(transport, cancel)?;
    let plan = session.plan.clone();
    let mut journal = if let Some(destination) = destination {
        NativeJournal::create(&plan, destination, journal_path, cancel)?
    } else {
        NativeJournal::open(&plan, journal_path, cancel, || session.metadata(cancel))?
    };
    let mut report = CopyReport::default();
    let result: io::Result<()> = (|| {
        let root_directory = enter_root(&mut session, &mut journal, &plan, cancel)?;
        let mut directories = BTreeMap::new();
        directories.insert(Vec::<Vec<u16>>::new(), root_directory);
        let mut blocked = scope_issues(&plan, &mut report);
        for item in &plan.entries {
            check_cancel(cancel)?;
            let entry = &item.entry;
            if entry.path.is_empty() {
                continue;
            }
            let Some(relative) = &item.mapped else {
                report.skipped.push((
                    PathBuf::new(),
                    format!("unmappable native UTF-16 path {:04x?}", entry.path),
                ));
                blocked.push(entry.path.clone());
                continue;
            };
            if blocked.iter().any(|p| entry.path.starts_with(p))
                || times(entry).is_err()
                || !entry.enumeration_complete
                || entry
                    .fidelity_pending
                    .iter()
                    .any(|r| !entry_fidelity(entry, r))
                || (!entry.directory && !entry.content_readable)
            {
                report.skipped.push((
                    relative.clone(),
                    "native content, enumeration, exact times or fidelity scope pending".into(),
                ));
                if entry.directory {
                    blocked.push(entry.path.clone());
                }
                continue;
            }
            let (_, native_parent) = entry
                .path
                .split_last()
                .ok_or_else(|| changed("native parent missing"))?;
            let parent = directories
                .get(native_parent)
                .ok_or_else(|| changed("native destination parent missing"))?
                .clone();
            let name = relative
                .file_name()
                .ok_or_else(|| changed("mapped native filename missing"))?;
            if entry.directory {
                let directory =
                    enter_directory(&mut session, &mut journal, &parent, entry, name, cancel)?;
                directories.insert(entry.path.clone(), directory);
            } else {
                let receipt = if let Some(receipt) = journal.receipts.get(&entry.path).cloned() {
                    revalidate(&mut session, &receipt, &parent, name, cancel)?;
                    receipt
                } else {
                    let source = session.file(entry, cancel)?;
                    let receipt = StagedFile::copy_source(source, &parent, name, cancel)?
                        .publish(cancel, true)?;
                    journal.save(receipt.clone())?;
                    receipt
                };
                report.verified_bytes = report
                    .verified_bytes
                    .checked_add(receipt.bytes)
                    .ok_or_else(|| changed("native progress overflow"))?;
                report.verified_files.push(relative.clone());
                progress(CopyProgress {
                    verified_files: report.verified_files.len() as u64,
                    verified_bytes: report.verified_bytes,
                    current_relative_path: relative.clone(),
                });
            }
        }
        finish_directories(&mut session, &mut journal, &plan, &directories, cancel)?;
        report.scan_completed = true;
        Ok(())
    })();
    if let Err(error) = result {
        if error.kind() == io::ErrorKind::Interrupted {
            report.cancelled = true;
        } else {
            report.failed.push((
                PathBuf::new(),
                super::migration_journal::persistence_description(&error),
            ));
        }
    }
    journal.finish(&plan, &report)?;
    Ok(report)
}
/// Authenticated native source capability. The transport remains backend-owned;
/// persisted identities and raw socket paths alone never authorize a copy.
pub struct NativeSelection {
    #[cfg(target_os = "linux")]
    client: tilo_ntfs_protocol::client::Client,
    #[cfg(not(target_os = "linux"))]
    _unavailable: std::convert::Infallible,
}

/// Authenticate a freshly selected broker against an independently expected identity.
///
/// # Errors
/// Refuses unsafe socket paths, unauthenticated brokers and identity disagreement.
/// Returns [`io::ErrorKind::Unsupported`] on hosts without the native client.
pub fn connect_native_selected(
    socket: &Path,
    nonce: String,
    expected: Identity,
) -> io::Result<NativeSelection> {
    #[cfg(target_os = "linux")]
    {
        Ok(NativeSelection {
            client: tilo_ntfs_protocol::client::Client::connect(socket, nonce, expected)?,
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (socket, nonce, expected);
        Err(native_unsupported())
    }
}

// Preserve callers that already hold an authenticated Linux client. New callers
// should enter through connect_native_selected rather than handling transports.
#[cfg(target_os = "linux")]
impl From<tilo_ntfs_protocol::client::Client> for NativeSelection {
    fn from(client: tilo_ntfs_protocol::client::Client) -> Self {
        Self { client }
    }
}

#[cfg(not(target_os = "linux"))]
fn native_unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "Administrative native copy is currently supported on Linux only.",
    )
}

/// Copies through a newly authenticated, administrator-selected native session.
/// # Errors
/// Refuses unsafe grants, unavailable durability or journal persistence failures.
pub fn copy_native_selected(
    selection: impl Into<NativeSelection>,
    destination: &Path,
    journal: &Path,
    cancel: &AtomicBool,
    progress: impl FnMut(CopyProgress),
) -> io::Result<CopyReport> {
    #[cfg(target_os = "linux")]
    {
        run(
            selection.into().client,
            Some(destination),
            journal,
            cancel,
            progress,
        )
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (selection, destination, journal, cancel, progress);
        Err(native_unsupported())
    }
}
/// Resume requires a fresh authenticated selection; saved receipts never reconnect.
/// # Errors
/// Refuses changed selection, journal location, output or persistence.
pub fn resume_native_selected(
    selection: impl Into<NativeSelection>,
    journal: &Path,
    cancel: &AtomicBool,
    progress: impl FnMut(CopyProgress),
) -> io::Result<CopyReport> {
    #[cfg(target_os = "linux")]
    {
        run(selection.into().client, None, journal, cancel, progress)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (selection, journal, cancel, progress);
        Err(native_unsupported())
    }
}
/// Early administrative command, before graphics, portals or device discovery.
#[cfg(target_os = "linux")]
pub fn maybe_run_command(args: &[String]) -> io::Result<Option<i32>> {
    static CANCEL: AtomicBool = AtomicBool::new(false);
    extern "C" fn cancel(_: libc::c_int) {
        CANCEL.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let Some(mode) = args.get(1).filter(|m| {
        m.as_str() == "--native-copy-selected" || m.as_str() == "--native-resume-selected"
    }) else {
        return Ok(None);
    };
    let resume = mode == "--native-resume-selected";
    if args.len() != if resume { 5 } else { 6 } {
        return Err(changed(
            "usage: --native-copy-selected DESCRIPTOR EXPECTED_IDENTITY_JSON DESTINATION JOURNAL; --native-resume-selected DESCRIPTOR EXPECTED_IDENTITY_JSON JOURNAL",
        ));
    }
    let descriptor = args.get(2).ok_or_else(|| changed("descriptor missing"))?;
    let expected: Identity = serde_json::from_str(
        args.get(3)
            .ok_or_else(|| changed("expected identity missing"))?,
    )?;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(descriptor)?;
    if !file.metadata()?.is_file() {
        return Err(changed("descriptor must be a regular file"));
    }
    let mut bytes = Vec::new();
    file.take(65_537).read_to_end(&mut bytes)?;
    if bytes.len() > 65_536 {
        return Err(changed("descriptor exceeds bound"));
    }
    let value: serde_json::Value = serde_json::from_slice(&bytes)?;
    let socket = value
        .get("socket")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| changed("descriptor socket missing"))?;
    let nonce = value
        .get("nonce")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| changed("descriptor nonce missing"))?;
    let described: Identity = serde_json::from_value(
        value
            .get("identity")
            .cloned()
            .ok_or_else(|| changed("descriptor identity missing"))?,
    )?;
    if described != expected {
        return Err(changed(
            "descriptor disagrees with independently expected identity",
        ));
    }
    let client = connect_native_selected(Path::new(socket), nonce.to_owned(), expected)?;
    // SAFETY: dedicated early command; handler only stores to an atomic boolean.
    unsafe {
        libc::signal(libc::SIGINT, cancel as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, cancel as *const () as libc::sighandler_t);
    }
    let report = if resume {
        resume_native_selected(
            client,
            Path::new(args.get(4).ok_or_else(|| changed("journal missing"))?),
            &CANCEL,
            |_| {},
        )?
    } else {
        copy_native_selected(
            client,
            Path::new(args.get(4).ok_or_else(|| changed("destination missing"))?),
            Path::new(args.get(5).ok_or_else(|| changed("journal missing"))?),
            &CANCEL,
            |_| {},
        )?
    };
    println!(
        "{}",
        serde_json::json!({"backend":"native-exclusive-read","acceptance":"unverified-administrative-only","complete":report.complete(),"verified_files":report.verified_files,"verified_bytes":report.verified_bytes,"skipped":report.skipped,"failed":report.failed,"cancelled":report.cancelled,"scan_completed":report.scan_completed,"metadata_limitations":report.metadata_limitations})
    );
    Ok(Some(if report.complete() { 0 } else { 2 }))
}

/// Dispatch the administrative command on unsupported hosts without inspecting
/// descriptors or touching source and destination paths.
///
/// # Errors
/// Recognized native-copy commands return [`io::ErrorKind::Unsupported`].
#[cfg(not(target_os = "linux"))]
pub fn maybe_run_command(args: &[String]) -> io::Result<Option<i32>> {
    if args.get(1).is_some_and(|mode| {
        matches!(
            mode.as_str(),
            "--native-copy-selected" | "--native-resume-selected"
        )
    }) {
        Err(native_unsupported())
    } else {
        Ok(None)
    }
}

#[cfg(all(test, not(target_os = "linux")))]
mod unsupported_tests {
    use super::*;

    #[test]
    fn native_factory_and_commands_refuse_without_accessing_paths() {
        let result = connect_native_selected(
            Path::new("missing/read.sock"),
            String::new(),
            Identity {
                volume: String::new(),
                root: String::new(),
                upcase: String::new(),
            },
        );
        assert!(matches!(result, Err(error) if error.kind() == io::ErrorKind::Unsupported));
        assert_eq!(maybe_run_command(&[]).unwrap(), None);
        for mode in ["--native-copy-selected", "--native-resume-selected"] {
            let error = maybe_run_command(&["imager".into(), mode.into()]).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod factory_tests {
    use super::*;

    #[test]
    fn native_factory_refuses_an_unvalidated_broker_path() {
        let result = connect_native_selected(
            Path::new("relative/read.sock"),
            "a".repeat(64),
            Identity {
                volume: "expected-volume".into(),
                root: "expected-root".into(),
                upcase: "expected-upcase".into(),
            },
        );
        assert!(result.is_err());
    }
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;
    use std::{cell::RefCell, rc::Rc, sync::atomic::Ordering};
    #[derive(Default)]
    struct State {
        reads: Vec<(u64, u32)>,
        fault: Option<&'static str>,
        during_finish: Option<Box<dyn FnOnce()>>,
    }
    struct Script {
        state: Rc<RefCell<State>>,
        cancel: Arc<AtomicBool>,
        identity: Identity,
        entries: Vec<Entry>,
        generation: u64,
        hashes: BTreeMap<String, String>,
        content: Vec<u8>,
        passes: u8,
    }
    fn entry(path: &[&str], directory: bool, size: u64) -> Entry {
        Entry {
            token: String::new(),
            path: path.iter().map(|s| s.encode_utf16().collect()).collect(),
            key: path.iter().map(|s| s.encode_utf16().collect()).collect(),
            record: if directory { 5 } else { 20 },
            sequence: 1,
            size,
            directory,
            metadata_digest: "a".repeat(64),
            content_readable: !directory,
            enumeration_complete: true,
            copy_times: Some(tilo_ntfs_protocol::CopyTimes {
                last_access_100ns: 116_444_735_999_999_999,
                last_data_change_100ns: 116_444_736_012_345_678,
            }),
            copy_times_unavailable_reason: None,
            fidelity_pending: vec!["windows-acl-policy-unverified".into()],
        }
    }
    fn script(state: Rc<RefCell<State>>, cancel: Arc<AtomicBool>) -> Script {
        let content: Vec<u8> = (0..1_048_579)
            .map(|n| ((n % 256 + n / 1_048_576) % 256) as u8)
            .collect();
        Script {
            state,
            cancel,
            identity: Identity {
                volume: "v".into(),
                root: "r".into(),
                upcase: "u".into(),
            },
            entries: vec![
                entry(&[], true, 0),
                entry(&["large.bin"], false, content.len() as u64),
                entry(&["empty"], false, 0),
                entry(&["nested"], true, 0),
                entry(&["nested", "empty-dir"], true, 0),
            ],
            generation: 0,
            hashes: BTreeMap::new(),
            content,
            passes: 0,
        }
    }
    impl transport_sealed::Sealed for Script {}
    impl Transport for Script {
        fn identity(&self) -> &Identity {
            &self.identity
        }
        fn begin(&mut self) -> io::Result<Manifest> {
            self.generation += 1;
            if self.generation >= 4 {
                match self.state.borrow().fault {
                    Some("begin-loss") => {
                        return Err(changed("scripted native disconnect during Begin"));
                    }
                    Some("stale-generation") => self.generation -= 1,
                    Some("changed-manifest") => self.entries[1].metadata_digest = "b".repeat(64),
                    Some("begin-cancel") => self.cancel.store(true, Ordering::Relaxed),
                    _ => {}
                }
            }
            self.hashes.clear();
            self.passes = 0;
            for e in &mut self.entries {
                e.token = format!("{}-{:?}", self.generation, e.path);
                if e.content_readable && e.size == 0 {
                    self.hashes
                        .insert(e.token.clone(), hex::encode(Sha256::digest([])));
                }
            }
            Ok(Manifest {
                generation: self.generation,
                entries: self.entries.clone(),
                issues: vec![],
                upcase: vec![],
            })
        }
        fn read_chunk(&mut self, token: &str, offset: u64, length: u32) -> io::Result<Vec<u8>> {
            if offset == 0 {
                self.passes += 1;
            }
            self.state.borrow_mut().reads.push((offset, length));
            if self.passes == 2 && self.state.borrow().fault == Some("read-loss") {
                return Err(changed("scripted native range disconnect"));
            }
            let mut bytes =
                self.content[offset as usize..offset as usize + length as usize].to_vec();
            if self.state.borrow().fault == Some("mutation") && self.passes == 2 {
                bytes[0] ^= 1;
            }
            if self.state.borrow().fault == Some("short") {
                bytes.pop();
            }
            if self.state.borrow().fault == Some("extra") {
                bytes.push(0);
            }
            if offset + u64::from(length) == self.content.len() as u64 {
                self.hashes
                    .insert(token.into(), hex::encode(Sha256::digest(&self.content)));
                if self.state.borrow().fault == Some("read-cancel") {
                    self.cancel.store(true, Ordering::Relaxed);
                }
            }
            Ok(bytes)
        }
        fn finish(&mut self) -> io::Result<(BTreeMap<String, String>, Vec<Issue>)> {
            let mut issues: Vec<Issue> = self
                .entries
                .iter()
                .filter(|e| !e.directory && !self.hashes.contains_key(&e.token))
                .map(|e| Issue {
                    path: Some(e.path.clone()),
                    reason: "content-hash-incomplete".into(),
                })
                .collect();
            if self.passes > 0 && self.state.borrow().fault == Some("finish-cancel") {
                self.cancel.store(true, Ordering::Relaxed);
            }
            if self.passes > 0 && self.state.borrow().fault == Some("hash") {
                self.hashes.insert("unknown".into(), "b".repeat(64));
            }
            if self.passes > 0 {
                if let Some(action) = self.state.borrow_mut().during_finish.take() {
                    action();
                }
                match self.state.borrow().fault {
                    Some("finish-loss") => {
                        return Err(changed("scripted native postflight disconnect"));
                    }
                    Some("selected-incomplete") => issues.push(Issue {
                        path: Some(vec!["large.bin".encode_utf16().collect()]),
                        reason: "content-hash-incomplete".into(),
                    }),
                    Some("missing-empty-hash") => {
                        let token = self
                            .entries
                            .iter()
                            .find(|e| e.size == 0 && !e.directory)
                            .unwrap()
                            .token
                            .clone();
                        self.hashes.remove(&token);
                    }
                    _ => {}
                }
            }
            Ok((self.hashes.clone(), issues))
        }
    }
    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let temp = tempfile::Builder::new()
            .prefix("tilo-native-copy-consumer-")
            .tempdir_in("/tmp")
            .unwrap();
        let base = temp.path().canonicalize().unwrap();
        let parent = PinnedDirectory::open_absolute(&base).unwrap();
        parent.create_child(OsStr::new("journal")).unwrap();
        (temp, base.join("destination"), base.join("journal/receipt"))
    }
    /// Two-pass native copy, second-pass mutation, fixtures and failed receipts.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn native_copy_passes_publish_only_verified_bytes() {
        {
            let (_temp, dest, journal) = fixture();
            let state = Rc::new(RefCell::new(State::default()));
            let cancel = Arc::new(AtomicBool::new(false));
            let transport = script(state.clone(), cancel.clone());
            let expected = transport.content.clone();
            let report = run(transport, Some(&dest), &journal, &cancel, |_| {}).unwrap();
            assert!(report.complete(), "{report:?}");
            assert_eq!(report.verified_bytes, 1_048_579);
            assert_eq!(
                state.borrow().reads,
                vec![
                    (0, 1_048_576),
                    (1_048_576, 3),
                    (0, 1_048_576),
                    (1_048_576, 3)
                ]
            );
            assert!(dest.join("nested/empty-dir").is_dir());
            assert_eq!(std::fs::metadata(dest.join("empty")).unwrap().len(), 0);
            let expected_time = std::time::UNIX_EPOCH + std::time::Duration::new(1, 234_567_800);
            assert_eq!(
                std::fs::metadata(dest.join("large.bin"))
                    .unwrap()
                    .modified()
                    .unwrap(),
                expected_time
            );
            assert!(std::fs::read(&journal).unwrap().starts_with(b"TILOJL3\n"));
            let resumed = run(
                script(state.clone(), cancel.clone()),
                None,
                &journal,
                &cancel,
                |_| {},
            )
            .unwrap();
            assert!(resumed.complete(), "{resumed:?}");
            assert_eq!(state.borrow().reads.len(), 8);
            assert_eq!(std::fs::read(dest.join("large.bin")).unwrap(), expected);
            assert!(
                super::super::migration_journal::resume_folder(&journal, &cancel, |_| {})
                    .unwrap_err()
                    .contains("fresh native selection")
            );
        }

        {
            for fault in [
                "mutation",
                "short",
                "extra",
                "hash",
                "read-cancel",
                "finish-cancel",
                "begin-loss",
                "stale-generation",
                "changed-manifest",
                "begin-cancel",
                "read-loss",
                "finish-loss",
                "selected-incomplete",
                "missing-empty-hash",
            ] {
                let (_temp, dest, journal) = fixture();
                let state = State {
                    fault: Some(fault),
                    ..State::default()
                };
                let state = Rc::new(RefCell::new(state));
                let cancel = Arc::new(AtomicBool::new(false));
                let report = run(
                    script(state, cancel.clone()),
                    Some(&dest),
                    &journal,
                    &cancel,
                    |_| {},
                )
                .unwrap();
                assert!(!report.complete(), "{fault}");
                assert!(!dest.join("large.bin").exists(), "{fault}");
                assert!(dest.join("empty").exists());
            }
        }

        {
            let temp = tempfile::tempdir().unwrap();
            let original = temp.path().join("times.img");
            let scoped = temp.path().join("copy-scoped-times.img");
            std::fs::write(&original, b"original").unwrap();
            std::fs::write(&scoped, b"scoped sibling").unwrap();
            assert_eq!(scoped_fixture(temp.path(), "times"), Some(scoped.clone()));
            std::fs::remove_file(scoped).unwrap();
            assert_eq!(scoped_fixture(temp.path(), "times"), Some(original));
        }

        {
            let (_temp, dest, journal) = fixture();
            let cancel = Arc::new(AtomicBool::new(false));
            let _failure = super::super::migration_journal::persistence_injection::Failure::at(
                "native_file_receipt",
                1,
            );
            let report = run(
                script(Rc::new(RefCell::new(State::default())), cancel.clone()),
                Some(&dest),
                &journal,
                &cancel,
                |_| {},
            )
            .unwrap();
            assert!(!report.complete());
            assert!(
                report
                    .failed
                    .iter()
                    .any(|(_, message)| message.contains("published output has no saved receipt")),
                "{report:?}"
            );
            assert!(report.verified_files.is_empty());
            assert!(dest.join("empty").exists());
            assert!(!dest.join("large.bin").exists());
        }
    }
    #[cfg(unix)]
    struct ImageTransport {
        program: PathBuf,
        image: PathBuf,
        original: serde_json::Value,
        identity: Identity,
        generation: u64,
        manifest: Option<Manifest>,
        hashes: BTreeMap<String, String>,
        active: Option<(String, u64, Sha256)>,
        reads: Rc<RefCell<Vec<(NativePath, u64, u32)>>>,
    }
    #[cfg(unix)]
    impl ImageTransport {
        fn operation(&self, arguments: &[String]) -> io::Result<serde_json::Value> {
            let output = std::process::Command::new(&self.program)
                .arg(&self.image)
                .args(arguments)
                .output()?;
            if !output.status.success() {
                return Err(io::Error::other(
                    String::from_utf8_lossy(&output.stderr).into_owned(),
                ));
            }
            let value: serde_json::Value = serde_json::from_slice(&output.stdout)?;
            if value
                .pointer("/stats/writes")
                .and_then(serde_json::Value::as_u64)
                != Some(0)
            {
                return Err(changed("diagnostic attempted native write"));
            }
            Ok(value)
        }
        fn revalidate(&self) -> io::Result<serde_json::Value> {
            let fresh = self.operation(&["--copy-manifest".into()])?;
            for key in [
                "identity",
                "native_identity",
                "entries",
                "issues",
                "upcase",
                "facts_digest",
                "begin-loss",
                "stale-generation",
                "changed-manifest",
                "begin-cancel",
                "read-loss",
                "extra",
                "finish-loss",
                "selected-incomplete",
                "missing-empty-hash",
            ] {
                if fresh.get(key) != self.original.get(key) {
                    return Err(changed("real image manifest or identity changed"));
                }
            }
            Ok(fresh)
        }
    }
    #[cfg(unix)]
    impl transport_sealed::Sealed for ImageTransport {}
    #[cfg(unix)]
    impl Transport for ImageTransport {
        fn identity(&self) -> &Identity {
            &self.identity
        }
        fn begin(&mut self) -> io::Result<Manifest> {
            let value = self.revalidate()?;
            self.generation += 1;
            self.hashes.clear();
            self.active = None;
            let mut entries: Vec<Entry> = serde_json::from_value(value["entries"].clone())?;
            for e in &mut entries {
                e.token = format!("{}-{:?}", self.generation, e.path);
                if e.content_readable && e.size == 0 {
                    self.hashes
                        .insert(e.token.clone(), hex::encode(Sha256::digest([])));
                }
            }
            let manifest = Manifest {
                generation: self.generation,
                entries,
                issues: serde_json::from_value(value["issues"].clone())?,
                upcase: serde_json::from_value(value["upcase"].clone())?,
            };
            self.manifest = Some(manifest.clone());
            Ok(manifest)
        }
        fn read_chunk(&mut self, token: &str, offset: u64, length: u32) -> io::Result<Vec<u8>> {
            self.revalidate()?;
            let entry = self
                .manifest
                .as_ref()
                .ok_or_else(|| changed("diagnostic has no observation"))?
                .entries
                .iter()
                .find(|e| e.token == token)
                .ok_or_else(|| changed("diagnostic token missing"))?
                .clone();
            let identity = &self.original["native_identity"];
            let serial = u64::from_str_radix(
                identity["serial"]
                    .as_str()
                    .ok_or_else(|| changed("native serial missing"))?,
                16,
            )
            .map_err(io::Error::other)?;
            let request = serde_json::json!({"serial":serial,"root_record":identity["root_record"],"root_sequence":identity["root_sequence"],"root_creation":identity["root_creation"],"object_record":entry.record,"object_sequence":entry.sequence,"size":entry.size,"offset":offset,"length":length});
            let value = self.operation(&["--read-chunk".into(), request.to_string()])?;
            if value["status"] != 0 {
                return Err(changed("native diagnostic range refused"));
            }
            let mut bytes = Vec::new();
            for fact in value["facts"]
                .as_array()
                .ok_or_else(|| changed("native range facts missing"))?
                .iter()
                .filter(|f| f["kind"] == "data")
            {
                if fact["offset"].as_u64() != Some(bytes.len() as u64) {
                    return Err(changed("native diagnostic range gap"));
                }
                let encoded = fact["hex"]
                    .as_str()
                    .ok_or_else(|| changed("native data missing"))?;
                for pair in encoded.as_bytes().chunks_exact(2) {
                    bytes.push(
                        u8::from_str_radix(
                            std::str::from_utf8(pair).map_err(io::Error::other)?,
                            16,
                        )
                        .map_err(io::Error::other)?,
                    );
                }
            }
            if bytes.len() != length as usize {
                return Err(changed("native diagnostic range short"));
            }
            self.reads
                .borrow_mut()
                .push((entry.path.clone(), offset, length));
            self.hashes.remove(token);
            if offset == 0 {
                self.active = Some((token.into(), 0, Sha256::new()));
            }
            let (selected, position, digest) = self
                .active
                .as_mut()
                .ok_or_else(|| changed("native diagnostic discontinuity"))?;
            if selected != token || *position != offset {
                return Err(changed("native diagnostic range reordered"));
            }
            digest.update(&bytes);
            *position += bytes.len() as u64;
            if *position == entry.size {
                self.hashes
                    .insert(token.into(), hex::encode(digest.clone().finalize()));
                self.active = None;
            }
            self.revalidate()?;
            Ok(bytes)
        }
        fn finish(&mut self) -> io::Result<(BTreeMap<String, String>, Vec<Issue>)> {
            self.revalidate()?;
            let manifest = self
                .manifest
                .take()
                .ok_or_else(|| changed("native diagnostic observation missing"))?;
            let issues = manifest
                .entries
                .iter()
                .filter(|e| !e.directory && !self.hashes.contains_key(&e.token))
                .map(|e| Issue {
                    path: Some(e.path.clone()),
                    reason: "content-hash-incomplete".into(),
                })
                .collect();
            Ok((self.hashes.clone(), issues))
        }
    }
    #[cfg(unix)]
    #[test]
    #[ignore = "requires explicitly prepared actual NTFS image and diagnostic executable; no production claims"]
    fn actual_native_image_bytes_reach_shared_copy_and_v3_receipts() {
        let (_temp, dest, journal) = fixture();
        let program = PathBuf::from(std::env::var_os("TILO_NATIVE_COPY_DIAGNOSTIC").unwrap());
        let image = std::env::var_os("TILO_NTFS_COPY_TREE_IMAGE").map_or_else(
            || PathBuf::from(std::env::var_os("TILO_NTFS_FIXTURES").unwrap()).join("large.img"),
            PathBuf::from,
        );
        let before = Sha256::digest(std::fs::read(&image).unwrap());
        let reads = Rc::new(RefCell::new(Vec::new()));
        let mut transport = ImageTransport {
            program,
            image: image.clone(),
            original: serde_json::Value::Null,
            identity: Identity {
                volume: String::new(),
                root: String::new(),
                upcase: String::new(),
            },
            generation: 0,
            manifest: None,
            hashes: BTreeMap::new(),
            active: None,
            reads: reads.clone(),
        };
        transport.original = transport.operation(&["--copy-manifest".into()]).unwrap();
        transport.identity =
            serde_json::from_value(transport.original["identity"].clone()).unwrap();
        let cancel = AtomicBool::new(false);
        let report = run(transport, Some(&dest), &journal, &cancel, |_| {}).unwrap();
        assert!(
            report.scan_completed && report.failed.is_empty(),
            "{report:?}"
        );
        assert!(!report.skipped.is_empty());
        assert!(
            report
                .skipped
                .iter()
                .all(|(p, _)| p.to_string_lossy().starts_with('$'))
        );
        let expected: Vec<u8> = (0..12 * 1_048_576)
            .map(|n| ((n % 256 + n / 1_048_576) % 256) as u8)
            .collect();
        assert_eq!(
            Sha256::digest(std::fs::read(dest.join("large.bin")).unwrap()),
            Sha256::digest(&expected)
        );
        assert_eq!(
            std::fs::read(dest.join("tail.bin")).unwrap(),
            (0..4267).map(|n| (n % 251) as u8).collect::<Vec<_>>()
        );
        assert_eq!(std::fs::metadata(dest.join("empty.bin")).unwrap().len(), 0);
        assert_eq!(
            std::fs::read(dest.join("hello.txt")).unwrap(),
            b"Connected NTFS fixture\n"
        );
        assert_eq!(
            std::fs::read(dest.join("cancel/last.bin")).unwrap(),
            b"final chunk"
        );
        assert_eq!(report.verified_files.len(), 5);
        assert_eq!(report.verified_bytes, 12_587_213);
        if std::env::var_os("TILO_NTFS_COPY_TREE_IMAGE").is_some() {
            assert!(dest.join("nested/empty").is_dir());
            assert_eq!(
                std::fs::read_dir(dest.join("nested/empty"))
                    .unwrap()
                    .count(),
                0
            );
        }
        let large: NativePath = vec!["large.bin".encode_utf16().collect()];
        let offsets: Vec<_> = reads
            .borrow()
            .iter()
            .filter(|(p, _, _)| *p == large)
            .map(|(_, o, _)| *o)
            .collect();
        assert_eq!(
            offsets,
            (0..12)
                .chain(0..12)
                .map(|n| n * 1_048_576)
                .collect::<Vec<_>>()
        );
        assert_eq!(before, Sha256::digest(std::fs::read(&image).unwrap()));
        println!(
            "actual-image sha256={} native_ranges={} bytes={} complete={} destination={}",
            hex::encode(before),
            reads.borrow().len(),
            report.verified_bytes,
            report.complete(),
            dest.display()
        );
    }

    fn scoped_fixture(root: &Path, name: &str) -> Option<PathBuf> {
        [format!("copy-scoped-{name}.img"), format!("{name}.img")]
            .into_iter()
            .map(|filename| root.join(filename))
            .find(|candidate| candidate.is_file())
    }

    #[cfg(unix)]
    fn image_transport(name: &str) -> io::Result<ImageTransport> {
        let mut transport = ImageTransport {
            program: PathBuf::from(
                std::env::var_os("TILO_NATIVE_COPY_DIAGNOSTIC")
                    .ok_or_else(|| changed("diagnostic executable missing"))?,
            ),
            image: PathBuf::from(
                std::env::var_os("TILO_NTFS_FIXTURES")
                    .ok_or_else(|| changed("fixture directory missing"))?,
            )
            .join(format!("{name}.img")),
            original: serde_json::Value::Null,
            identity: Identity {
                volume: String::new(),
                root: String::new(),
                upcase: String::new(),
            },
            generation: 0,
            manifest: None,
            hashes: BTreeMap::new(),
            active: None,
            reads: Rc::new(RefCell::new(Vec::new())),
        };
        if let Some(root) = std::env::var_os("TILO_NTFS_COPY_SCOPED_FIXTURES")
            && let Some(candidate) = scoped_fixture(Path::new(&root), name)
        {
            transport.image = candidate;
        }
        transport.original = transport.operation(&["--copy-manifest".into()])?;
        transport.identity = serde_json::from_value(transport.original["identity"].clone())?;
        Ok(transport)
    }
    #[cfg(unix)]
    fn assert_scoped_image(name: &str) {
        let (_temp, dest, journal) = fixture();
        let source = image_transport(name).unwrap();
        let image = source.image.clone();
        let before = Sha256::digest(std::fs::read(&image).unwrap());
        let report = run(
            source,
            Some(&dest),
            &journal,
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        assert!(
            report.scan_completed && report.failed.is_empty(),
            "{name}: {report:?}"
        );
        if name == "times" || name == "times-v1" {
            let meta = std::fs::metadata(dest.join("hello.txt")).unwrap();
            assert_eq!(
                meta.modified().unwrap(),
                std::time::UNIX_EPOCH - std::time::Duration::from_nanos(100)
            );
            assert_eq!(
                meta.accessed().unwrap(),
                std::time::UNIX_EPOCH + std::time::Duration::new(1, 234_567_800)
            );
        } else if name.starts_with("times-") {
            assert!(!dest.join("hello.txt").exists());
            assert!(
                report
                    .skipped
                    .iter()
                    .any(|(p, _)| p == Path::new("hello.txt"))
            );
        } else {
            assert!(dest.join("hello.txt").exists(), "{name}");
            assert!(
                report
                    .metadata_limitations
                    .iter()
                    .any(|r| r.contains("windows-acl-policy-unverified"))
            );
        }
        if name == "metadata" {
            assert!(!dest.join("link").exists());
            assert!(report.skipped.iter().any(|(p, _)| p == Path::new("link")));
            assert!(
                report
                    .metadata_limitations
                    .iter()
                    .any(|m| m.contains("named-data-stream"))
            );
        }
        if name == "hardlink" {
            assert_eq!(
                std::fs::read(dest.join("second")).unwrap(),
                b"Connected NTFS fixture\n"
            );
            assert!(
                report.skipped.iter().any(|(p, m)| p.as_os_str().is_empty()
                    && m.contains("external-alias-closure-unknown"))
            );
        }
        if name == "dos-alias" {
            assert!(!dest.join("HELLO~1.TXT").exists());
            assert!(
                report
                    .skipped
                    .iter()
                    .any(|(_, m)| m.contains("external-alias-closure-unknown"))
            );
        }
        if std::env::var_os("TILO_NTFS_COPY_SCOPED_FIXTURES").is_some() {
            assert_eq!(
                std::fs::read(dest.join("ordinary.txt")).unwrap(),
                b"ordinary sibling\n"
            );
        }
        assert_eq!(before, Sha256::digest(std::fs::read(&image).unwrap()));
        println!(
            "actual scoped fixture {name}: bytes={} skipped={} sha256={}",
            report.verified_bytes,
            report.skipped.len(),
            hex::encode(before)
        );
    }
    #[cfg(unix)]
    #[test]
    #[ignore = "requires actual diagnostic image fixtures; no production claims"]
    fn actual_native_times_and_fidelity_remain_scoped_and_generation_changes_refuse() {
        for name in [
            "times",
            "times-v1",
            "times-duplicate",
            "times-invalid-flags",
            "metadata",
            "acl",
            "hardlink",
            "dos-alias",
        ] {
            assert_scoped_image(name);
        }
        assert!(image_transport("times-missing").is_err());
        let (_temp, dest, journal) = fixture();
        let mut source = image_transport("clean").unwrap();
        source.image = source.image.with_file_name("generation.img");
        assert!(
            run(
                source,
                Some(&dest),
                &journal,
                &AtomicBool::new(false),
                |_| {}
            )
            .is_err()
        );
        assert!(!dest.exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "production Linux administrative selection; requires separately authorized fixture coordinator"]
    fn native_selected_copy_command_worker() {
        let args: Vec<String> =
            serde_json::from_str(&std::env::var("TILO_NATIVE_COPY_COMMAND_ARGS").unwrap()).unwrap();
        assert!(maybe_run_command(&args).unwrap().is_some());
    }
    #[test]
    fn native_facts_metadata_and_names_are_durable_safe_and_bounded() {
        {
            let mut item = PlanEntry {
                entry: entry(&["file"], false, 0),
                mapped: Some(PathBuf::from("file")),
            };
            item.entry.token = "ephemeral-secret-token".into();
            assert!(serde_json::to_value(&item).is_err());
            item.entry.token.clear();
            let mut value = serde_json::to_value(&item).unwrap();
            assert!(value["entry"].get("token").is_none());
            value["entry"]["token"] = serde_json::json!("old-session-token");
            assert!(serde_json::from_value::<PlanEntry>(value).is_err());
        }

        {
            let (_temp, dest, journal) = fixture();
            let cancel = Arc::new(AtomicBool::new(false));
            let mut source = script(Rc::new(RefCell::new(State::default())), cancel.clone());
            source.entries[0]
                .fidelity_pending
                .push("unsupported-dos-flags".into());
            let report = run(source, Some(&dest), &journal, &cancel, |_| {}).unwrap();
            assert!(!report.complete());
            assert!(report.verified_files.is_empty());
            assert!(!dest.join("large.bin").exists());
        }

        {
            assert_eq!(request_cost(536_870_912, 4096).unwrap(), 1024);
            assert!(request_cost(536_870_913, 4096).is_err());
            assert!(request_cost(u64::MAX, 4096).is_err());
            assert!(request_cost(1, 1).is_err());
            assert_eq!(request_cost(0, 0).unwrap(), 0);
            for name in [
                vec![0xd800],
                vec![0],
                vec![47],
                vec![92],
                vec![46, 46],
                vec![0x4e00; 100],
            ] {
                assert!(map_path(&vec![name]).is_err());
            }
            assert_eq!(
                map_path(&vec!["café".encode_utf16().collect()]).unwrap(),
                PathBuf::from("café")
            );
        }
    }

    /// Resume after relocation, edits, directory changes and torn journals.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn native_resume_repairs_only_consistent_journals_and_directories() {
        {
            let (_temp, dest, journal) = fixture();
            let cancel = Arc::new(AtomicBool::new(false));
            let state = Rc::new(RefCell::new(State {
                fault: Some("read-cancel"),
                ..State::default()
            }));
            assert!(
                run(
                    script(state, cancel.clone()),
                    Some(&dest),
                    &journal,
                    &cancel,
                    |_| {}
                )
                .unwrap()
                .cancelled
            );
            std::fs::write(dest.join("large.bin"), b"user conflict").unwrap();
            cancel.store(false, Ordering::Relaxed);
            let fresh = || script(Rc::new(RefCell::new(State::default())), cancel.clone());
            let relocated = journal.with_file_name("relocated");
            std::fs::copy(&journal, &relocated).unwrap();
            assert!(run(fresh(), None, &relocated, &cancel, |_| {}).is_err());
            assert!(run(fresh(), None, &journal, &cancel, |_| {}).is_err());
            assert_eq!(
                std::fs::read(dest.join("large.bin")).unwrap(),
                b"user conflict"
            );
        }

        {
            for replace_name in [false, true] {
                let (_temp, dest, journal) = fixture();
                let state = Rc::new(RefCell::new(State::default()));
                let cancel = Arc::new(AtomicBool::new(false));
                assert!(
                    run(
                        script(state.clone(), cancel.clone()),
                        Some(&dest),
                        &journal,
                        &cancel,
                        |_| {}
                    )
                    .unwrap()
                    .complete()
                );
                let target = dest.join("large.bin");
                let changed_time = std::time::UNIX_EPOCH + std::time::Duration::from_secs(345);
                let edited = target.clone();
                state.borrow_mut().during_finish = Some(Box::new(move || {
                    if replace_name {
                        std::fs::rename(&edited, edited.with_extension("retained")).unwrap();
                    }
                    std::fs::write(&edited, b"user edit during native Finish").unwrap();
                    std::fs::File::options()
                        .write(true)
                        .open(&edited)
                        .unwrap()
                        .set_times(std::fs::FileTimes::new().set_modified(changed_time))
                        .unwrap();
                }));
                let report = run(
                    script(state, cancel.clone()),
                    None,
                    &journal,
                    &cancel,
                    |_| {},
                )
                .unwrap();
                assert!(
                    !report.verified_files.contains(&PathBuf::from("large.bin")),
                    "edited output certified: {report:?}"
                );
                assert!(!report.failed.is_empty());
                assert_eq!(
                    std::fs::metadata(&target).unwrap().modified().unwrap(),
                    changed_time
                );
                assert_eq!(
                    std::fs::read(&target).unwrap(),
                    b"user edit during native Finish"
                );
            }
        }

        {
            use std::io::Write;
            for relative in ["nested/empty-dir", ""] {
                let (_temp, dest, journal) = fixture();
                let cancel = Arc::new(AtomicBool::new(false));
                let fresh = || script(Rc::new(RefCell::new(State::default())), cancel.clone());
                assert!(
                    run(fresh(), Some(&dest), &journal, &cancel, |_| {})
                        .unwrap()
                        .complete()
                );
                let changed_time = std::time::UNIX_EPOCH + std::time::Duration::from_secs(345);
                let directory = dest.join(relative);
                std::fs::File::open(&directory)
                    .unwrap()
                    .set_times(std::fs::FileTimes::new().set_modified(changed_time))
                    .unwrap();
                std::fs::OpenOptions::new()
                    .append(true)
                    .open(&journal)
                    .unwrap()
                    .write_all(b"torn")
                    .unwrap();
                let before = std::fs::read(&journal).unwrap();
                assert!(
                    run(fresh(), None, &journal, &cancel, |_| {}).is_err(),
                    "changed directory accepted: {relative}"
                );
                assert_eq!(
                    std::fs::metadata(directory).unwrap().modified().unwrap(),
                    changed_time
                );
                assert_eq!(std::fs::read(&journal).unwrap(), before);
            }
        }

        {
            let (_temp, dest, journal) = fixture();
            let cancel = Arc::new(AtomicBool::new(false));
            let state = Rc::new(RefCell::new(State {
                fault: Some("read-cancel"),
                ..State::default()
            }));
            assert!(
                run(
                    script(state, cancel.clone()),
                    Some(&dest),
                    &journal,
                    &cancel,
                    |_| {}
                )
                .unwrap()
                .cancelled
            );
            cancel.store(false, Ordering::Relaxed);
            let report = run(
                script(Rc::new(RefCell::new(State::default())), cancel.clone()),
                None,
                &journal,
                &cancel,
                |_| {},
            )
            .unwrap();
            assert!(report.complete(), "{report:?}");
            let expected = std::time::UNIX_EPOCH + std::time::Duration::new(1, 234_567_800);
            assert_eq!(
                std::fs::metadata(&dest).unwrap().modified().unwrap(),
                expected
            );
            assert_eq!(
                std::fs::metadata(dest.join("nested/empty-dir"))
                    .unwrap()
                    .modified()
                    .unwrap(),
                expected
            );
        }

        {
            use std::io::Write;
            let (_temp, dest, journal) = fixture();
            let cancel = Arc::new(AtomicBool::new(false));
            let fresh = || script(Rc::new(RefCell::new(State::default())), cancel.clone());
            assert!(
                run(fresh(), Some(&dest), &journal, &cancel, |_| {})
                    .unwrap()
                    .complete()
            );
            std::fs::write(dest.join(".tilo-copy-abandoned"), b"preserve").unwrap();
            std::fs::OpenOptions::new()
                .append(true)
                .open(&journal)
                .unwrap()
                .write_all(b"torn")
                .unwrap();
            let before = std::fs::read(&journal).unwrap();
            assert!(run(fresh(), None, &journal, &cancel, |_| {}).is_err());
            assert_eq!(std::fs::read(&journal).unwrap(), before);
            assert_eq!(
                std::fs::read(dest.join(".tilo-copy-abandoned")).unwrap(),
                b"preserve"
            );
        }

        {
            use std::io::Write;
            let (_temp, dest, journal) = fixture();
            let state = Rc::new(RefCell::new(State::default()));
            let cancel = Arc::new(AtomicBool::new(false));
            assert!(
                run(
                    script(state.clone(), cancel.clone()),
                    Some(&dest),
                    &journal,
                    &cancel,
                    |_| {}
                )
                .unwrap()
                .complete()
            );
            std::fs::OpenOptions::new()
                .append(true)
                .open(&journal)
                .unwrap()
                .write_all(b"torn")
                .unwrap();
            let before = std::fs::read(&journal).unwrap();
            let mut source = script(state.clone(), cancel.clone());
            source.identity.volume = "changed".into();
            assert!(run(source, None, &journal, &cancel, |_| {}).is_err());
            assert_eq!(std::fs::read(&journal).unwrap(), before);
            assert!(
                run(
                    script(state, cancel.clone()),
                    None,
                    &journal,
                    &cancel,
                    |_| {}
                )
                .unwrap()
                .complete()
            );
        }
    }
}
