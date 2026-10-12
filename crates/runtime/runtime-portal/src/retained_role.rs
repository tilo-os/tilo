//! Fixed installed retained-helper role over genuine kernel process custody.
//! This role is not a source/file grant or an identity certificate.
#[cfg(target_os = "linux")]
use crate::process_binding::ExecutingPeer;
use std::io;
#[cfg(target_os = "linux")]
use std::{
    fs::File,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    sync::Arc,
};

pub const HELPER_PATH: &str = "/usr/libexec/tilo-prepared-setup";
pub const FS_DAEMON_PATH: &str = "/usr/bin/fs";
fn refused() -> io::Error {
    io::ErrorKind::PermissionDenied.into()
}
#[cfg(target_os = "linux")]
fn installed_program(path: &str, parent: &str) -> io::Result<File> {
    for path in ["/", "/usr", parent] {
        let metadata = std::fs::symlink_metadata(path)?;
        if !metadata.is_dir() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
            return Err(refused());
        }
    }
    let helper = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let metadata = helper.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != 0
        || metadata.mode() & 0o022 != 0
        || metadata.mode() & 0o111 == 0
        || metadata.len() == 0
    {
        return Err(refused());
    }
    Ok(helper)
}
/// Opaque root-local role minted from a genuine `ExecutingPeer` and the one
/// protected installed helper image. No PID, program or UID wire constructor.
#[cfg(target_os = "linux")]
pub struct VerifiedHelperPeer {
    peer: Arc<ExecutingPeer>,
    helper: File,
    start: u64,
}
#[cfg(target_os = "linux")]
impl VerifiedHelperPeer {
    /// # Errors
    /// Refuses a non-root verifier, wrong/dead task or changed executable.
    pub fn admit(peer: Arc<ExecutingPeer>) -> io::Result<Self> {
        if unsafe { libc::getuid() } != 0
            || unsafe { libc::geteuid() } != 0
            || peer.uid() != 0
            || !peer.current()
        {
            return Err(refused());
        }
        let start = peer.start_time()?;
        let role = Self {
            peer,
            helper: installed_program(HELPER_PATH, "/usr/libexec")?,
            start,
        };
        role.check_current()?;
        Ok(role)
    }
    /// # Errors
    /// Every use observes the same held pidfd/start and fixed installed image.
    pub fn check_current(&self) -> io::Result<()> {
        if unsafe { libc::getuid() } != 0
            || unsafe { libc::geteuid() } != 0
            || self.peer.uid() != 0
            || !self.peer.current()
            || self.peer.start_time()? != self.start
        {
            return Err(refused());
        }
        let named = installed_program(HELPER_PATH, "/usr/libexec")?.metadata()?;
        let held = self.helper.metadata()?;
        let image = File::open(format!("/proc/{}/exe", self.peer.pid()))?.metadata()?;
        let expected = (held.dev(), held.ino());
        if (named.dev(), named.ino()) != expected
            || (image.dev(), image.ino()) != expected
            || !self.peer.current()
            || self.peer.start_time()? != self.start
        {
            return Err(refused());
        }
        Ok(())
    }
    /// Kernel-observed recipient of the retained person proof, never authority
    /// derived from a serialized root-PID claim.
    pub fn peer(&self) -> &ExecutingPeer {
        &self.peer
    }
}

/// Opaque root-local role minted from a genuine `ExecutingPeer` and the one
/// protected installed filesystem daemon image. No PID, program or UID wire constructor.
#[cfg(target_os = "linux")]
pub struct VerifiedDaemonPeer {
    peer: Arc<ExecutingPeer>,
    helper: File,
    start: u64,
}
#[cfg(target_os = "linux")]
impl VerifiedDaemonPeer {
    /// # Errors
    /// Refuses a non-root verifier, wrong/dead task or changed executable.
    pub fn admit(peer: Arc<ExecutingPeer>) -> io::Result<Self> {
        if unsafe { libc::getuid() } != 0
            || unsafe { libc::geteuid() } != 0
            || peer.uid() != 0
            || !peer.current()
        {
            return Err(refused());
        }
        let start = peer.start_time()?;
        let role = Self {
            peer,
            helper: installed_program(FS_DAEMON_PATH, "/usr/bin")?,
            start,
        };
        role.check_current()?;
        Ok(role)
    }
    /// # Errors
    /// Every use observes the same held pidfd/start and fixed installed image.
    pub fn check_current(&self) -> io::Result<()> {
        if unsafe { libc::getuid() } != 0
            || unsafe { libc::geteuid() } != 0
            || self.peer.uid() != 0
            || !self.peer.current()
            || self.peer.start_time()? != self.start
        {
            return Err(refused());
        }
        let named = installed_program(FS_DAEMON_PATH, "/usr/bin")?.metadata()?;
        let held = self.helper.metadata()?;
        let image = File::open(format!("/proc/{}/exe", self.peer.pid()))?.metadata()?;
        let expected = (held.dev(), held.ino());
        if (named.dev(), named.ino()) != expected
            || (image.dev(), image.ino()) != expected
            || !self.peer.current()
            || self.peer.start_time()? != self.start
        {
            return Err(refused());
        }
        Ok(())
    }
    /// Exact fixed protected executable observation for the existing owned
    /// helper bootstrap delegation; these numbers alone never mint a role.
    /// # Errors
    /// Refuses a retired task or changed fixed installed image.
    pub fn program_identity(&self) -> io::Result<[u64; 2]> {
        self.check_current()?;
        let metadata = self.helper.metadata()?;
        Ok([metadata.dev(), metadata.ino()])
    }
    /// Held task start observed at admission, rechecked before delegation.
    /// # Errors
    /// Refuses retired or substituted process/image custody.
    pub fn start_time(&self) -> io::Result<u64> {
        self.check_current()?;
        Ok(self.start)
    }
    /// Kernel-observed recipient of the filesystem daemon proof, never authority
    /// derived from a serialized root-PID claim.
    pub fn peer(&self) -> &ExecutingPeer {
        &self.peer
    }
}

/// Descriptive root-sealed verifier vocabulary, not an admission constructor.
/// Consumers must independently authenticate the actual sealed root descriptor
/// against the same current original-source custody before using these values.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeHistoryVerifierRecord {
    pub schema: u32,
    pub source_owner: [u8; 32],
    pub source_operation: String,
    pub person: [u8; 32],
    pub personal_room: String,
    pub device: [u8; 32],
}
fn canonical_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if [8, 13, 18, 23].contains(&index) {
                byte == b'-'
            } else {
                byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
            }
        })
        && value != "00000000-0000-0000-0000-000000000000"
}
impl NativeHistoryVerifierRecord {
    pub fn valid(&self) -> bool {
        self.schema == 1
            && self.source_owner != [0; 32]
            && self.person != [0; 32]
            && self.device != [0; 32]
            && canonical_uuid(&self.source_operation)
            && canonical_uuid(&self.personal_room)
    }
    /// # Errors
    /// Refuses malformed descriptive context; this never grants source access.
    pub fn encode(&self) -> io::Result<Vec<u8>> {
        if !self.valid() {
            return Err(refused());
        }
        let bytes = serde_json::to_vec(self).map_err(io::Error::other)?;
        if bytes.len() > 4096 {
            return Err(refused());
        }
        Ok(bytes)
    }
    /// # Errors
    /// Root sealed bytes have one canonical representation, no unknown fields.
    pub fn decode(bytes: &[u8]) -> io::Result<Self> {
        if bytes.is_empty() || bytes.len() > 4096 {
            return Err(refused());
        }
        let value: Self = serde_json::from_slice(bytes).map_err(io::Error::other)?;
        if value.encode()? != bytes {
            return Err(refused());
        }
        Ok(value)
    }
    /// # Errors
    /// Hash of exact bounded canonical bytes for existing launcher token pins.
    pub fn sha256(&self) -> io::Result<[u8; 32]> {
        use sha2::Digest;
        Ok(sha2::Sha256::digest(self.encode()?).into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn record() -> NativeHistoryVerifierRecord {
        NativeHistoryVerifierRecord {
            schema: 1,
            source_owner: [1; 32],
            source_operation: "11111111-1111-4111-8111-111111111111".into(),
            person: [2; 32],
            personal_room: "22222222-2222-4222-8222-222222222222".into(),
            device: [3; 32],
        }
    }
    #[test]
    fn sealed_vocabulary_has_one_closed_bounded_encoding() {
        let record = record();
        let bytes = record.encode().unwrap();
        assert_eq!(NativeHistoryVerifierRecord::decode(&bytes).unwrap(), record);
        let mut padded = bytes;
        padded.push(b' ');
        assert!(NativeHistoryVerifierRecord::decode(&padded).is_err());
        let mut unknown = serde_json::to_value(&record).unwrap();
        unknown["grant"] = true.into();
        assert!(
            NativeHistoryVerifierRecord::decode(&serde_json::to_vec(&unknown).unwrap()).is_err()
        );
        assert!(NativeHistoryVerifierRecord::decode(&vec![b' '; 4097]).is_err());
        let mut changed = record.clone();
        changed.device[0] ^= 1;
        assert_ne!(record.sha256().unwrap(), changed.sha256().unwrap());
    }
    #[test]
    fn descriptive_record_refuses_nil_or_noncanonical_context() {
        let mut record = record();
        record.source_operation = "00000000-0000-0000-0000-000000000000".into();
        assert!(record.encode().is_err());
        record.source_operation = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa".to_uppercase();
        assert!(record.encode().is_err());
        record.source_operation = "11111111-1111-4111-8111-111111111111".into();
        record.source_owner = [0; 32];
        assert!(record.encode().is_err());
    }
}

/// Descriptive pending handoff carried only in an authenticated helper's sealed
/// file. Decoding this record neither authenticates a worker nor admits history.
/// The daemon must observe the same actual worker QPE, held program and source
/// capability and independently refresh the enrolled person before using it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeHistoryPendingHandoff {
    pub schema: u32,
    pub enrollment_context_digest: [u8; 32],
    pub source_owner: [u8; 32],
    pub operation_id: String,
    pub intent_digest: [u8; 32],
    pub worker_pid: u32,
    pub worker_start: u64,
    pub worker_uid: u32,
    pub worker_gid: u32,
    pub worker_program: [u64; 2],
    pub ticket: [u8; 16],
    pub challenge: [u8; 32],
}
impl NativeHistoryPendingHandoff {
    pub fn valid(&self) -> bool {
        self.schema == 1
            && self.enrollment_context_digest != [0; 32]
            && self.source_owner != [0; 32]
            && canonical_uuid(&self.operation_id)
            && self.intent_digest != [0; 32]
            && self.worker_pid != 0
            && self.worker_start != 0
            && self.worker_program.last().is_some_and(|inode| *inode != 0)
            && self.ticket != [0; 16]
            && self.challenge != [0; 32]
    }
    /// # Errors
    /// Refuses malformed or oversized descriptive records; no authority is minted.
    pub fn encode(&self) -> io::Result<Vec<u8>> {
        if !self.valid() {
            return Err(refused());
        }
        let bytes = serde_json::to_vec(self).map_err(io::Error::other)?;
        if bytes.len() > 4096 {
            return Err(refused());
        }
        Ok(bytes)
    }
    /// # Errors
    /// Requires exact closed canonical JSON and bounded descriptive fields.
    pub fn decode(bytes: &[u8]) -> io::Result<Self> {
        if bytes.is_empty() || bytes.len() > 4096 {
            return Err(refused());
        }
        let record: Self = serde_json::from_slice(bytes).map_err(io::Error::other)?;
        if record.encode()? != bytes {
            return Err(refused());
        }
        Ok(record)
    }
}

#[cfg(test)]
mod pending_handoff_tests {
    use super::*;

    fn record() -> NativeHistoryPendingHandoff {
        NativeHistoryPendingHandoff {
            schema: 1,
            enrollment_context_digest: [1; 32],
            source_owner: [2; 32],
            operation_id: "11111111-2222-4333-8444-555555555555".into(),
            intent_digest: [3; 32],
            worker_pid: 42,
            worker_start: 71,
            worker_uid: 1000,
            worker_gid: 1000,
            worker_program: [4, 5],
            ticket: [6; 16],
            challenge: [7; 32],
        }
    }
    #[test]
    fn descriptive_handoff_roundtrip_is_closed_and_canonical_only() {
        let record = record();
        let bytes = record.encode().unwrap();
        assert_eq!(NativeHistoryPendingHandoff::decode(&bytes).unwrap(), record);
        let mut unknown: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        unknown
            .as_object_mut()
            .unwrap()
            .insert("admitted".into(), true.into());
        assert!(
            NativeHistoryPendingHandoff::decode(&serde_json::to_vec(&unknown).unwrap()).is_err()
        );
        let padded = [bytes, b" ".to_vec()].concat();
        assert!(NativeHistoryPendingHandoff::decode(&padded).is_err());
        assert!(NativeHistoryPendingHandoff::decode(&vec![b' '; 4097]).is_err());
    }
    #[test]
    fn missing_context_task_ticket_or_intent_refuses() {
        let base = record();
        let mut changed = base.clone();
        changed.enrollment_context_digest = [0; 32];
        assert!(changed.encode().is_err());
        changed = base.clone();
        changed.worker_start = 0;
        assert!(changed.encode().is_err());
        changed = base.clone();
        changed.ticket = [0; 16];
        assert!(changed.encode().is_err());
        changed = base;
        changed.intent_digest = [0; 32];
        assert!(changed.encode().is_err());
    }
}

/// Descriptive expectation for the distinct authenticated source Read channel.
/// Only actual helper-sealed delivery, current certified Files admission and
/// kernel source proof establish custody; this codec supplies none of them.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeHistoryReadHandoff {
    pub schema: u32,
    pub read_request_id: String,
    pub completed_request_id: Option<String>,
    pub enrollment_context_digest: [u8; 32],
    pub source_owner: [u8; 32],
    pub worker_pid: u32,
    pub worker_start: u64,
    pub worker_uid: u32,
    pub worker_gid: u32,
    pub worker_program: [u64; 2],
    pub ticket: [u8; 16],
    pub challenge: [u8; 32],
}
impl NativeHistoryReadHandoff {
    pub fn valid(&self) -> bool {
        self.schema == 1
            && canonical_uuid(&self.read_request_id)
            && self
                .completed_request_id
                .as_ref()
                .is_none_or(|id| canonical_uuid(id))
            && self.enrollment_context_digest != [0; 32]
            && self.source_owner != [0; 32]
            && self.worker_pid != 0
            && self.worker_start != 0
            && self.worker_program[1] != 0
            && self.ticket != [0; 16]
            && self.challenge != [0; 32]
    }
    /// # Errors
    /// Refuses malformed or overlarge descriptive context; never mints authority.
    pub fn encode(&self) -> io::Result<Vec<u8>> {
        if !self.valid() {
            return Err(refused());
        }
        let bytes = serde_json::to_vec(self).map_err(io::Error::other)?;
        if bytes.len() > 4096 {
            return Err(refused());
        }
        Ok(bytes)
    }
    /// # Errors
    /// Requires exact closed canonical bytes of the Read protocol only.
    pub fn decode(bytes: &[u8]) -> io::Result<Self> {
        if bytes.is_empty() || bytes.len() > 4096 {
            return Err(refused());
        }
        let value: Self = serde_json::from_slice(bytes).map_err(io::Error::other)?;
        if value.encode()? != bytes {
            return Err(refused());
        }
        Ok(value)
    }
}

#[cfg(test)]
mod read_handoff_tests {
    use super::*;
    fn record() -> NativeHistoryReadHandoff {
        NativeHistoryReadHandoff {
            schema: 1,
            read_request_id: "11111111-2222-4333-8444-555555555555".into(),
            completed_request_id: None,
            enrollment_context_digest: [1; 32],
            source_owner: [2; 32],
            worker_pid: 42,
            worker_start: 71,
            worker_uid: 1000,
            worker_gid: 1000,
            worker_program: [4, 5],
            ticket: [6; 16],
            challenge: [7; 32],
        }
    }
    #[test]
    fn read_handoff_requires_closed_canonical_distinct_protocol() {
        let mut value = record();
        let bytes = value.encode().unwrap();
        assert_eq!(NativeHistoryReadHandoff::decode(&bytes).unwrap(), value);
        value.completed_request_id = Some("aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee".into());
        assert_eq!(
            NativeHistoryReadHandoff::decode(&value.encode().unwrap()).unwrap(),
            value
        );
        let mut unknown: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        unknown
            .as_object_mut()
            .unwrap()
            .insert("admitted".into(), true.into());
        assert!(NativeHistoryReadHandoff::decode(&serde_json::to_vec(&unknown).unwrap()).is_err());
        assert!(NativeHistoryPendingHandoff::decode(&bytes).is_err());
        assert!(NativeHistoryReadHandoff::decode(&[bytes, b" ".to_vec()].concat()).is_err());
        value.completed_request_id = Some("00000000-0000-0000-0000-000000000000".into());
        assert!(value.encode().is_err());
        value = record();
        value.worker_start = 0;
        assert!(value.encode().is_err());
        value = record();
        value.challenge = [0; 32];
        assert!(value.encode().is_err());
    }
}
