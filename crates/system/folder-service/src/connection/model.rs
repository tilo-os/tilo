//! Bounded data supplied by future verified observation and durable-store adapters.
use std::collections::BTreeMap;

pub const MAX_ENTRIES: usize = 4096;
pub const MAX_PATH_UNITS: usize = 4096;
pub const MAX_COMPONENTS: usize = 64;
pub const MAX_NAME_UNITS: usize = 255;
pub const MAX_PLAN_ENTRIES: usize = MAX_ENTRIES * 3;
pub const MAX_CANONICAL_KEY_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelError {
    InvalidPath,
    LimitExceeded,
    DuplicatePath,
    InvalidTree,
    InvalidKeys,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    Unix,
    Windows,
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum NativeName {
    Unix(Vec<u8>),
    Windows(Vec<u16>),
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct RelativePath {
    components: Vec<NativeName>,
}
impl RelativePath {
    /// Construct a bounded relative path without interpreting Unix bytes as text.
    ///
    /// # Errors
    /// Returns `InvalidPath` for empty, absolute, traversal or NUL-containing paths,
    /// and `LimitExceeded` when a path, component or depth exceeds its bound.
    pub fn unix(path: &[u8]) -> Result<Self, ModelError> {
        if path.len() > MAX_PATH_UNITS {
            return Err(ModelError::LimitExceeded);
        }
        let components: Vec<_> = path
            .split(|b| *b == b'/')
            .map(|n| NativeName::Unix(n.to_vec()))
            .collect();
        Self::validate(components)
    }
    /// Construct a bounded relative path preserving native Windows UTF-16 units.
    ///
    /// # Errors
    /// Returns `InvalidPath` for empty, absolute, traversal, reserved or unsafe
    /// names, and `LimitExceeded` when a path, component or depth exceeds its bound.
    pub fn windows(path: &[u16]) -> Result<Self, ModelError> {
        if path.len() > MAX_PATH_UNITS {
            return Err(ModelError::LimitExceeded);
        }
        let components: Vec<_> = path
            .split(|b| *b == 92)
            .map(|n| NativeName::Windows(n.to_vec()))
            .collect();
        Self::validate(components)
    }
    fn validate(components: Vec<NativeName>) -> Result<Self, ModelError> {
        if components.len() > MAX_COMPONENTS {
            return Err(ModelError::LimitExceeded);
        }
        for c in &components {
            let units: Vec<u16> = match c {
                NativeName::Unix(v) => v.iter().map(|b| u16::from(*b)).collect(),
                NativeName::Windows(v) => v.clone(),
            };
            if units.is_empty() || units == [46] || units == [46, 46] || units.contains(&0) {
                return Err(ModelError::InvalidPath);
            }
            if units.len() > MAX_NAME_UNITS {
                return Err(ModelError::LimitExceeded);
            }
            if matches!(c, NativeName::Windows(_)) {
                if units
                    .iter()
                    .any(|b| *b < 32 || [34, 42, 47, 58, 60, 62, 63, 92, 124].contains(b))
                    || matches!(units.last(), Some(32 | 46))
                {
                    return Err(ModelError::InvalidPath);
                }
                // Win32 device aliases also apply before an extension.
                let stem: Vec<u16> = units
                    .iter()
                    .take_while(|u| **u != 46)
                    .map(|u| if (97..=122).contains(u) { u - 32 } else { *u })
                    .collect();
                let reserved = ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"];
                let numbered = matches!(stem.as_slice(), [67, 79, 77, number] | [76, 80, 84, number]
                    if [49, 50, 51, 52, 53, 54, 55, 56, 57, 185, 178, 179].contains(number));
                if numbered
                    || reserved
                        .iter()
                        .any(|s| stem == s.encode_utf16().collect::<Vec<_>>())
                {
                    return Err(ModelError::InvalidPath);
                }
            }
        }
        Ok(Self { components })
    }
    pub fn components(&self) -> &[NativeName] {
        &self.components
    }
    pub fn encoding(&self) -> Encoding {
        match self.components.first() {
            Some(NativeName::Windows(_)) => Encoding::Windows,
            // Constructors require at least one homogeneous native component.
            Some(NativeName::Unix(_)) | None => Encoding::Unix,
        }
    }
    /// Lossless conversion only; never replacement characters or host-OS guessing.
    ///
    /// # Errors
    /// Returns `InvalidPath` if text or names cannot be represented safely in the
    /// requested encoding, or `LimitExceeded` if the converted path exceeds a bound.
    pub fn to_encoding(&self, encoding: Encoding) -> Result<Self, ModelError> {
        if self.encoding() == encoding {
            return Ok(self.clone());
        }
        let components = self
            .components
            .iter()
            .map(|n| match n {
                NativeName::Unix(v) => std::str::from_utf8(v)
                    .map(|s| NativeName::Windows(s.encode_utf16().collect()))
                    .map_err(|_| ModelError::InvalidPath),
                NativeName::Windows(v) => String::from_utf16(v)
                    .map(|s| NativeName::Unix(s.into_bytes()))
                    .map_err(|_| ModelError::InvalidPath),
            })
            .collect::<Result<Vec<_>, _>>()?;
        let path = Self::validate(components)?;
        let units: usize = path
            .components
            .iter()
            .map(|n| match n {
                NativeName::Unix(v) => v.len(),
                NativeName::Windows(v) => v.len(),
            })
            .sum();
        if units + path.components.len() - 1 > MAX_PATH_UNITS {
            return Err(ModelError::LimitExceeded);
        }
        Ok(path)
    }
    pub(crate) fn logical(&self) -> Self {
        self.to_encoding(Encoding::Unix)
            .unwrap_or_else(|_| self.clone())
    }
    pub(crate) fn contains(&self, other: &Self) -> bool {
        self.components.len() <= other.components.len()
            && self
                .components
                .iter()
                .zip(&other.components)
                .all(|(a, b)| match (a, b) {
                    (NativeName::Unix(a), NativeName::Windows(b))
                    | (NativeName::Windows(b), NativeName::Unix(a)) => std::str::from_utf8(a)
                        .ok()
                        .zip(String::from_utf16(b).ok())
                        .is_some_and(|(a, b)| a == b),
                    _ => a == b,
                })
    }
    pub(crate) fn overlaps(&self, other: &Self) -> bool {
        self.contains(other) || other.contains(self)
    }
    pub(crate) fn parent(&self) -> Option<Self> {
        let (_, parent) = self.components.split_last()?;
        (!parent.is_empty()).then(|| Self {
            components: parent.to_vec(),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConnectionId(pub [u8; 16]);
/// Adapter-attested stable volume AND selected root identity. Paths/drive letters
/// and same-boot inode fallbacks are insufficient. This record grants no access.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EndpointIdentity {
    pub volume: [u8; 32],
    pub root: [u8; 32],
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EntryState {
    /// SHA-256 of completely read, stable content, supplied by the adapter.
    File {
        sha256: [u8; 32],
        bytes: u64,
        executable: bool,
    },
    Directory,
}
impl EntryState {
    pub(crate) fn is_dir(&self) -> bool {
        matches!(self, Self::Directory)
    }
}
/// Explicit endpoint facts. Canonical keys must come from that endpoint's
/// actual comparison semantics, including case and normalization, for EVERY
/// candidate native path and uncertain observation scope. Missing evidence
/// blocks comparisons whose scope disjointness cannot be established.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NamePolicy {
    Exact,
    Canonical(BTreeMap<RelativePath, Vec<Vec<u8>>>),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    pub identity: EndpointIdentity,
    pub encoding: Encoding,
    pub names: NamePolicy,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PendingReason {
    Offline,
    PermissionDenied,
    Unstable,
    Unsupported,
    IdentityChanged,
    IncompleteScope,
    MissingNameEvidence,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopeIssue {
    pub path: Option<RelativePath>,
    pub reason: PendingReason,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Observation {
    /// Adapter attests complete enumeration of the selected root, stable hashes,
    /// unchanged identity and successful access throughout this scope. Skipped
    /// entries, unsupported metadata/types, or instability require Incomplete.
    Complete,
    /// All unobserved/uncertain scopes must be listed. None means the root;
    /// these scopes block both ancestor and descendant mutation proposals.
    Incomplete(Vec<ScopeIssue>),
    Unavailable(PendingReason),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObservedTree {
    pub(crate) endpoint: Endpoint,
    pub(crate) entries: BTreeMap<RelativePath, EntryState>,
    pub(crate) observation: Observation,
}
impl ObservedTree {
    /// Validate bounded adapter observations and endpoint namespace evidence.
    ///
    /// # Errors
    /// Rejects excessive entries, scope issues or name evidence; duplicate paths;
    /// absent/non-directory parents; wrong native encodings; entries supplied for
    /// an unavailable endpoint; and malformed or inconsistent canonical keys.
    pub fn new(
        endpoint: Endpoint,
        entries: impl IntoIterator<Item = (RelativePath, EntryState)>,
        observation: Observation,
    ) -> Result<Self, ModelError> {
        let entries = bounded_entries(entries)?;
        if entries.keys().any(|p| p.encoding() != endpoint.encoding) {
            return Err(ModelError::InvalidTree);
        }
        if let Observation::Incomplete(issues) = &observation {
            if issues.is_empty() || issues.len() > MAX_ENTRIES {
                return Err(ModelError::LimitExceeded);
            }
            if issues.iter().any(|i| {
                i.path
                    .as_ref()
                    .is_some_and(|p| p.encoding() != endpoint.encoding)
            }) {
                return Err(ModelError::InvalidTree);
            }
        }
        if matches!(observation, Observation::Unavailable(_)) && !entries.is_empty() {
            return Err(ModelError::InvalidTree);
        }
        if let NamePolicy::Canonical(keys) = &endpoint.names {
            if keys.len() > MAX_PLAN_ENTRIES {
                return Err(ModelError::LimitExceeded);
            }
            let mut total_bytes = 0usize;
            for (path, key) in keys {
                if path.encoding() != endpoint.encoding
                    || key.len() != path.components.len()
                    || key.iter().any(|k| k.is_empty() || k.len() > MAX_PATH_UNITS)
                {
                    return Err(ModelError::InvalidKeys);
                }
                total_bytes += key.iter().map(Vec::len).sum::<usize>();
                if total_bytes > MAX_CANONICAL_KEY_BYTES {
                    return Err(ModelError::LimitExceeded);
                }
                if let Some(parent) = path.parent()
                    && keys.get(&parent).map(Vec::as_slice)
                        != key.split_last().map(|(_, prefix)| prefix)
                {
                    return Err(ModelError::InvalidKeys);
                }
            }
        }
        Ok(Self {
            endpoint,
            entries,
            observation,
        })
    }
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }
    pub fn entries(&self) -> &BTreeMap<RelativePath, EntryState> {
        &self.entries
    }
    pub fn observation(&self) -> &Observation {
        &self.observation
    }
}
fn bounded_entries(
    entries: impl IntoIterator<Item = (RelativePath, EntryState)>,
) -> Result<BTreeMap<RelativePath, EntryState>, ModelError> {
    let mut result = BTreeMap::new();
    for (path, entry) in entries {
        if result.len() == MAX_ENTRIES {
            return Err(ModelError::LimitExceeded);
        }
        if result.insert(path, entry).is_some() {
            return Err(ModelError::DuplicatePath);
        }
    }
    for path in result.keys() {
        if let Some(parent) = path.parent()
            && result
                .get(&parent)
                .or_else(|| result.get(&parent.logical()))
                != Some(&EntryState::Directory)
        {
            return Err(ModelError::InvalidTree);
        }
    }
    Ok(result)
}
/// Absent is an explicit state; loading an empty map is NOT establishing a
/// baseline. `from_verified` is an adapter assertion of durable prior evidence,
/// not a verification operation or mutation capability.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedBaseline {
    pub connection: ConnectionId,
    pub source: EndpointIdentity,
    pub destination: EndpointIdentity,
    pub(crate) entries: Option<BTreeMap<RelativePath, EntryState>>,
}
impl VerifiedBaseline {
    pub fn absent(
        connection: ConnectionId,
        source: EndpointIdentity,
        destination: EndpointIdentity,
    ) -> Self {
        Self {
            connection,
            source,
            destination,
            entries: None,
        }
    }
    /// Accept a bounded store-adapter assertion of a durably verified baseline.
    ///
    /// # Errors
    /// Rejects excessive entries, duplicate paths (including lossless native
    /// aliases), and paths whose parent is absent or is not a directory.
    pub fn from_verified(
        connection: ConnectionId,
        source: EndpointIdentity,
        destination: EndpointIdentity,
        entries: impl IntoIterator<Item = (RelativePath, EntryState)>,
    ) -> Result<Self, ModelError> {
        let entries = bounded_entries(entries.into_iter().map(|(p, e)| (p.logical(), e)))?;
        Ok(Self {
            connection,
            source,
            destination,
            entries: Some(entries),
        })
    }
    pub fn entries(&self) -> Option<&BTreeMap<RelativePath, EntryState>> {
        self.entries.as_ref()
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Source,
    Destination,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Conflict {
    DivergentEdits,
    EditDelete,
    FileDirectory,
    NameCollision,
    UnrepresentableName,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Show both observed states and request explicit baseline establishment.
    InitialPreview,
    /// Also needs durable baseline acknowledgement, including equal deletions.
    VerifyEqual,
    Propagate {
        from: Side,
    },
    /// MUST preserve the target in recoverable storage before publication.
    RecoverDeletion {
        at: Side,
    },
    /// Neither side may be modified until resolution preserves both versions.
    Conflict(Conflict),
    Pending(PendingReason),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PathProposal {
    pub path: RelativePath,
    pub baseline: Option<EntryState>,
    pub source: Option<EntryState>,
    pub destination: Option<EntryState>,
    pub decision: Decision,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionStatus {
    ChangesPending,
    SourceUnavailable,
    DestinationUnavailable,
    /// Reserved for the durable service; a pure `ReconcilePlan` never emits this.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "reserved durable-service status; pure reconciliation only asserts that it is not emitted"
        )
    )]
    UpToDate,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReconcilePlan {
    pub connection: ConnectionId,
    pub source: EndpointIdentity,
    pub destination: EndpointIdentity,
    pub status: ConnectionStatus,
    pub issues: Vec<(Side, ScopeIssue)>,
    pub proposals: Vec<PathProposal>,
    /// Always true: even an empty/equal comparison is not a durable receipt.
    pub requires_durable_verification: bool,
    pub initial_comparison: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    fn id(n: u8) -> EndpointIdentity {
        EndpointIdentity {
            volume: [n; 32],
            root: [n; 32],
        }
    }
    fn endpoint() -> Endpoint {
        Endpoint {
            identity: id(1),
            encoding: Encoding::Unix,
            names: NamePolicy::Exact,
        }
    }
    fn path(s: &str) -> RelativePath {
        RelativePath::unix(s.as_bytes()).unwrap()
    }
    #[test]
    fn observations_reject_duplicates_orphan_children_and_wrong_native_encoding() {
        assert_eq!(
            ObservedTree::new(
                endpoint(),
                [
                    (path("a"), EntryState::Directory),
                    (path("a"), EntryState::Directory)
                ],
                Observation::Complete
            ),
            Err(ModelError::DuplicatePath)
        );
        assert_eq!(
            ObservedTree::new(
                endpoint(),
                [(path("a/b"), EntryState::Directory)],
                Observation::Complete
            ),
            Err(ModelError::InvalidTree)
        );
        assert_eq!(
            ObservedTree::new(
                endpoint(),
                [(RelativePath::windows(&[97]).unwrap(), EntryState::Directory)],
                Observation::Complete
            ),
            Err(ModelError::InvalidTree)
        );
        assert_eq!(
            ObservedTree::new(
                endpoint(),
                [(path("a"), EntryState::Directory)],
                Observation::Unavailable(PendingReason::Offline)
            ),
            Err(ModelError::InvalidTree)
        );
        assert!(ObservedTree::new(endpoint(), [], Observation::Incomplete(vec![])).is_err());
    }
    #[test]
    fn entry_and_issue_limits_reject_instead_of_truncating() {
        let entries =
            || (0..MAX_ENTRIES + 1).map(|n| (path(&format!("{n}")), EntryState::Directory));
        assert_eq!(
            ObservedTree::new(endpoint(), entries(), Observation::Complete),
            Err(ModelError::LimitExceeded)
        );
        assert_eq!(
            VerifiedBaseline::from_verified(ConnectionId([0; 16]), id(1), id(2), entries()),
            Err(ModelError::LimitExceeded)
        );
        assert!(
            ObservedTree::new(
                endpoint(),
                entries().take(MAX_ENTRIES),
                Observation::Complete
            )
            .is_ok()
        );
        let issues = vec![
            ScopeIssue {
                path: None,
                reason: PendingReason::Unstable
            };
            MAX_ENTRIES + 1
        ];
        assert_eq!(
            ObservedTree::new(endpoint(), [], Observation::Incomplete(issues)),
            Err(ModelError::LimitExceeded)
        );
    }
    #[test]
    fn baseline_does_not_silently_collapse_native_aliases() {
        let entries = [
            (path("a"), EntryState::Directory),
            (RelativePath::windows(&[97]).unwrap(), EntryState::Directory),
        ];
        assert_eq!(
            VerifiedBaseline::from_verified(ConnectionId([0; 16]), id(1), id(2), entries),
            Err(ModelError::DuplicatePath)
        );
    }
    #[test]
    fn canonical_evidence_requires_consistent_parent_keys() {
        let names = NamePolicy::Canonical(BTreeMap::from([
            (path("a"), vec![b"one".to_vec()]),
            (path("a/b"), vec![b"two".to_vec(), b"b".to_vec()]),
        ]));
        assert_eq!(
            ObservedTree::new(
                Endpoint {
                    names,
                    ..endpoint()
                },
                [],
                Observation::Complete
            ),
            Err(ModelError::InvalidKeys)
        );
    }
    #[test]
    fn canonical_evidence_has_an_aggregate_memory_bound() {
        let names = NamePolicy::Canonical(
            (0..MAX_ENTRIES)
                .map(|n| (path(&format!("{n}")), vec![vec![b'x'; 2048]]))
                .collect(),
        );
        assert_eq!(
            ObservedTree::new(
                Endpoint {
                    names,
                    ..endpoint()
                },
                [],
                Observation::Complete
            )
            .err(),
            Some(ModelError::LimitExceeded)
        );
    }
    #[test]
    fn transcode_rejects_unrepresentable_names_in_both_directions() {
        assert!(
            RelativePath::unix(b"a:stream")
                .unwrap()
                .to_encoding(Encoding::Windows)
                .is_err()
        );
        assert!(
            RelativePath::unix(b"a\\b")
                .unwrap()
                .to_encoding(Encoding::Windows)
                .is_err()
        );
        assert!(
            RelativePath::unix(&[255])
                .unwrap()
                .to_encoding(Encoding::Windows)
                .is_err()
        );
        assert!(
            RelativePath::windows(&[0xd800])
                .unwrap()
                .to_encoding(Encoding::Unix)
                .is_err()
        );
        let original = RelativePath::windows(&"ä\\😀".encode_utf16().collect::<Vec<_>>()).unwrap();
        assert_eq!(
            original
                .to_encoding(Encoding::Unix)
                .unwrap()
                .to_encoding(Encoding::Windows)
                .unwrap(),
            original
        );
    }
    #[test]
    fn native_paths_preserve_non_unicode_names() {
        assert_eq!(
            RelativePath::unix(b"dir/\xff").unwrap().components,
            vec![
                NativeName::Unix(b"dir".to_vec()),
                NativeName::Unix(vec![255])
            ]
        );
        assert_eq!(
            RelativePath::windows(&[0xd800]).unwrap().components,
            vec![NativeName::Windows(vec![0xd800])]
        );
    }
    #[test]
    fn paths_reject_absolute_traversal_empty_and_windows_aliases() {
        for path in [
            b"".as_slice(),
            b"/a",
            b"a/../b",
            b"./a",
            b"a//b",
            b"a/",
            b"a\0b",
        ] {
            assert!(RelativePath::unix(path).is_err(), "{path:?}");
        }
        for path in [
            "", "C:\\x", "\\x", "a\\..\\b", "a/b", "a\\", "a.", "aux", "COM1.txt", "a:b",
        ] {
            assert!(
                RelativePath::windows(&path.encode_utf16().collect::<Vec<_>>()).is_err(),
                "{path}"
            );
        }
    }
    #[test]
    fn path_size_and_depth_are_bounded() {
        assert!(RelativePath::unix(&vec![b'x'; 256]).is_err());
        assert!(RelativePath::unix(vec!["a"; 65].join("/").as_bytes()).is_err());
        assert!(RelativePath::unix(vec!["a"; 64].join("/").as_bytes()).is_ok());
        assert!(RelativePath::unix(vec!["x".repeat(255); 17].join("/").as_bytes()).is_err());
    }
}
