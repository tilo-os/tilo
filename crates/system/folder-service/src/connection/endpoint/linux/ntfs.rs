//! Administrative NTFS read adapter. Native parsing stays in the helper process.
use super::super::model::{
    Encoding, Endpoint, EndpointIdentity, EntryState, NamePolicy, NativeName, Observation,
    ObservedTree, PendingReason, RelativePath, ScopeIssue,
};
use super::{Access, Capabilities, Capability, EndpointError};
use qos_ntfs_protocol::{NativePath, client::Client};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
pub struct NtfsReadEndpoint {
    client: Arc<Mutex<Client>>,
    identity: EndpointIdentity,
    upcase: Mutex<Option<Vec<u16>>>,
}
fn digest(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut text, byte| {
        let _ = write!(text, "{byte:02x}");
        text
    })
}
fn relative(path: &NativePath) -> Result<RelativePath, EndpointError> {
    let mut units = Vec::new();
    for c in path {
        if !units.is_empty() {
            units.push(92);
        }
        units.extend(c);
    }
    RelativePath::windows(&units).map_err(|_| EndpointError::Unsupported)
}
impl NtfsReadEndpoint {
    pub(super) fn new(
        client: Arc<Mutex<Client>>,
        expected: EndpointIdentity,
    ) -> Result<Self, EndpointError> {
        {
            let guard = client.lock().map_err(|_| EndpointError::Unstable)?;
            if guard.identity().volume != digest(&expected.volume)
                || guard.identity().root != digest(&expected.root)
            {
                return Err(EndpointError::IdentityChanged);
            }
        }
        Ok(Self {
            client,
            identity: expected,
            upcase: Mutex::new(None),
        })
    }
    pub(super) fn same_session(&self, client: &Arc<Mutex<Client>>) -> bool {
        Arc::ptr_eq(&self.client, client)
    }
    pub fn capabilities(&self) -> Capabilities {
        Capabilities {
            safe_read: Capability::Refused(EndpointError::Unverified),
            safe_write: Capability::Refused(EndpointError::ReadOnly),
            durable_transactions: Capability::Refused(EndpointError::ReadOnly),
        }
    }
    /// Canonicalizes candidates absent from the manifest using the same actual native table.
    /// # Errors
    /// Refuses use before native table validation and non-Windows paths.
    pub fn canonicalize(&self, path: &RelativePath) -> Result<Vec<Vec<u8>>, EndpointError> {
        let guard = self.upcase.lock().map_err(|_| EndpointError::Unstable)?;
        let table = guard.as_ref().ok_or(EndpointError::Unverified)?;
        let result = path
            .components()
            .iter()
            .map(|component| {
                let NativeName::Windows(units) = component else {
                    return Err(EndpointError::Unsupported);
                };
                units
                    .iter()
                    .map(|unit| {
                        table
                            .get(usize::from(*unit))
                            .copied()
                            .ok_or(EndpointError::Unsupported)
                    })
                    .collect::<Result<Vec<_>, _>>()
                    .map(|c| c.into_iter().flat_map(u16::to_le_bytes).collect())
            })
            .collect();
        drop(guard);
        result
    }
    pub fn observe(&self, cancelled: &AtomicBool) -> ObservedTree {
        self.observe_inner(cancelled)
            .unwrap_or_else(|error| ObservedTree {
                endpoint: Endpoint {
                    identity: self.identity,
                    encoding: Encoding::Windows,
                    names: NamePolicy::Canonical(BTreeMap::new()),
                },
                entries: BTreeMap::new(),
                observation: Observation::Unavailable(error.reason()),
            })
    }
    fn observe_inner(&self, cancelled: &AtomicBool) -> Result<ObservedTree, EndpointError> {
        let mut client = self.client.lock().map_err(|_| EndpointError::Unstable)?;
        check_cancelled(&mut client, cancelled)?;
        let manifest = client.begin();
        check_cancelled(&mut client, cancelled)?;
        let m = manifest?;
        *self.upcase.lock().map_err(|_| EndpointError::Unstable)? = Some(m.upcase);
        let mut entries = BTreeMap::new();
        let mut keys = BTreeMap::new();
        let mut issues = BTreeMap::new();
        let mut bytes = 0u64;
        let mut expected = BTreeMap::new();
        for e in &m.entries {
            check_cancelled(&mut client, cancelled)?;
            if e.path.is_empty() {
                continue;
            }
            let Ok(path) = relative(&e.path) else {
                issues.insert(None, PendingReason::Unsupported);
                continue;
            };
            keys.insert(path.clone(), self.canonicalize(&path)?);
            if e.directory {
                entries.insert(path, EntryState::Directory);
                continue;
            }
            if !e.content_readable || bytes.saturating_add(e.size) > qos_ntfs_protocol::HASH_BYTES {
                issues.insert(Some(path), PendingReason::IncompleteScope);
                continue;
            }
            let mut hash = Sha256::new();
            let mut offset = 0u64;
            while offset < e.size {
                check_cancelled(&mut client, cancelled)?;
                let length = u32::try_from(
                    e.size
                        .saturating_sub(offset)
                        .min(u64::from(qos_ntfs_protocol::CHUNK_BYTES)),
                )
                .map_err(|_| EndpointError::LimitExceeded)?;
                let content = client.read_chunk(&e.token, offset, length);
                check_cancelled(&mut client, cancelled)?;
                hash.update(&content?);
                offset = offset
                    .checked_add(u64::from(length))
                    .ok_or(EndpointError::LimitExceeded)?;
            }
            bytes = bytes
                .checked_add(e.size)
                .ok_or(EndpointError::LimitExceeded)?;
            let hash: [u8; 32] = hash.finalize().into();
            expected.insert(e.token.clone(), digest(&hash));
            entries.insert(
                path,
                EntryState::File {
                    sha256: hash,
                    bytes: e.size,
                    executable: false,
                },
            );
        }
        let completion = client.finish();
        check_cancelled(&mut client, cancelled)?;
        let (hashes, pending) = completion?;
        if hashes != expected {
            return Err(EndpointError::IdentityChanged);
        }
        for issue in m.issues.into_iter().chain(pending) {
            let path = issue.path.as_ref().and_then(|p| relative(p).ok());
            if let Some(path) = &path {
                keys.insert(path.clone(), self.canonicalize(path)?);
            }
            issues.insert(path, pending_reason(&issue.reason));
        }
        let issues = issues
            .into_iter()
            .map(|(path, reason)| ScopeIssue { path, reason })
            .collect::<Vec<_>>();
        let observation = if issues.is_empty() {
            Observation::Complete
        } else {
            Observation::Incomplete(issues)
        };
        let tree = ObservedTree::new(
            Endpoint {
                identity: self.identity,
                encoding: Encoding::Windows,
                names: NamePolicy::Canonical(keys),
            },
            entries,
            observation,
        )
        .map_err(|_| EndpointError::Unsupported)?;
        check_cancelled(&mut client, cancelled)?;
        drop(client);
        Ok(tree)
    }
}

fn check_cancelled(client: &mut Client, cancelled: &AtomicBool) -> Result<(), EndpointError> {
    if cancelled.load(Ordering::Acquire) {
        client.cancel();
        Err(EndpointError::Cancelled)
    } else {
        Ok(())
    }
}

fn pending_reason(reason: &str) -> PendingReason {
    if matches!(
        reason,
        "content-hash-incomplete" | "metadata-budget" | "directory-enumeration"
    ) {
        PendingReason::IncompleteScope
    } else {
        PendingReason::Unsupported
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn cancellation_at_native_phase() {
        use std::{
            io::{Read, Write},
            os::{fd::FromRawFd, unix::net::UnixStream},
            time::Duration,
        };
        let locator: serde_json::Value =
            serde_json::from_str(&std::env::var("QOS_NTFS_SESSION_LOCATOR").unwrap()).unwrap();
        let identity: qos_ntfs_protocol::Identity =
            serde_json::from_value(locator["identity"].clone()).unwrap();
        let decode = |s: &str| -> [u8; 32] {
            s.as_bytes()
                .chunks_exact(2)
                .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap())
                .collect::<Vec<_>>()
                .try_into()
                .unwrap()
        };
        let expected = EndpointIdentity {
            volume: decode(&identity.volume),
            root: decode(&identity.root),
        };
        let client = Arc::new(Mutex::new(
            Client::connect(
                std::path::Path::new(locator["socket"].as_str().unwrap()),
                locator["nonce"].as_str().unwrap().to_owned(),
                identity,
            )
            .unwrap(),
        ));
        let mut context = super::super::EndpointContext::default();
        context
            .register_ntfs_session(Arc::clone(&client), expected)
            .unwrap();
        let endpoint = context.open_read_endpoint(&expected).unwrap();
        let fd: i32 = std::env::var("QOS_NTFS_CANCELLATION_CONTROL_FD")
            .unwrap()
            .parse()
            .unwrap();
        assert!(fd > 2);
        // Test-only inherited control socket, never a production authority constructor.
        // The coordinator stops the actual final native child before signaling this flag.
        let mut control = unsafe { UnixStream::from_raw_fd(fd) };
        control
            .set_read_timeout(Some(Duration::from_secs(150)))
            .unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&cancelled);
        let trigger = std::thread::spawn(move || {
            let mut byte = [0];
            control.read_exact(&mut byte).unwrap();
            assert_eq!(byte, [b'C']);
            flag.store(true, Ordering::Release);
            control.write_all(b"A").unwrap();
        });
        let result = super::super::observe_read(&endpoint, &cancelled);
        trigger.join().unwrap();
        assert!(cancelled.load(Ordering::Acquire));
        assert!(
            result.entries().is_empty(),
            "cancelled observation leaked completed entries"
        );
        assert_eq!(
            result.observation(),
            &Observation::Unavailable(PendingReason::IncompleteScope)
        );
        assert!(
            client.lock().unwrap().begin().is_err(),
            "cancelled session remained usable"
        );
    }
    #[test]
    #[ignore = "real Linux broker/endpoint cancellation coordinator required"]
    fn native_session_cancellation_during_last_chunk() {
        cancellation_at_native_phase();
    }
    #[test]
    #[ignore = "real Linux broker/endpoint cancellation coordinator required"]
    fn native_session_cancellation_during_postflight() {
        cancellation_at_native_phase();
    }

    #[test]
    #[ignore = "native disposable Linux administrative session required; no public capability promotion"]
    fn native_session_context_reads_windows_names_and_refuses_write_trial() {
        let locator: serde_json::Value =
            serde_json::from_str(&std::env::var("QOS_NTFS_SESSION_LOCATOR").unwrap()).unwrap();
        let identity: qos_ntfs_protocol::Identity =
            serde_json::from_value(locator["identity"].clone()).unwrap();
        let decode = |s: &str| -> [u8; 32] {
            s.as_bytes()
                .chunks_exact(2)
                .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap())
                .collect::<Vec<_>>()
                .try_into()
                .unwrap()
        };
        let expected = EndpointIdentity {
            volume: decode(&identity.volume),
            root: decode(&identity.root),
        };
        let client = Arc::new(Mutex::new(
            Client::connect(
                std::path::Path::new(locator["socket"].as_str().unwrap()),
                locator["nonce"].as_str().unwrap().to_owned(),
                identity,
            )
            .unwrap(),
        ));
        let mut context = super::super::EndpointContext::default();
        context
            .register_ntfs_session(Arc::clone(&client), expected)
            .unwrap();
        context.register_ntfs_session(client, expected).unwrap();
        assert!(matches!(
            context.open_endpoint(&expected, Access::Write),
            Err(EndpointError::ReadOnly)
        ));
        let endpoint = context.open_read_endpoint(&expected).unwrap();
        let super::super::ReadEndpoint::Ntfs(native) = &endpoint else {
            panic!("native dispatch");
        };
        assert_eq!(
            native.capabilities().safe_read,
            Capability::Refused(EndpointError::Unverified)
        );
        let result = super::super::observe_read(&endpoint, &AtomicBool::new(false));
        assert_eq!(result.endpoint().encoding, Encoding::Windows);
        assert!(matches!(result.observation(), Observation::Incomplete(_)));
        let path = RelativePath::windows(&"large.bin".encode_utf16().collect::<Vec<_>>()).unwrap();
        let EntryState::File { sha256, bytes, .. } = result.entries().get(&path).unwrap() else {
            panic!("native content missing");
        };
        assert_eq!(*bytes, 12 * 1024 * 1024);
        assert_eq!(
            digest(sha256),
            std::env::var("QOS_NTFS_NATIVE_FILE_SHA256").unwrap()
        );
        let absent =
            RelativePath::windows(&"NotInManifest.txt".encode_utf16().collect::<Vec<_>>()).unwrap();
        assert!(!native.canonicalize(&absent).unwrap().is_empty());
    }
}
