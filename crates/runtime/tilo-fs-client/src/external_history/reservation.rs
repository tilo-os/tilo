//! Bounded descriptive parsing of the actual source worker's held reservation.
//! Neither these bytes nor the intent hash construct a grant or prove origin.
//! A coordinator must obtain them through an independently authenticated source
//! pending capability and keep that capability live through its effect boundary.
use std::{collections::BTreeMap, io};

const LIMIT: usize = 2 * 1024 * 1024;
const DOMAIN: &[u8] = b"tilo-native-retained-replacement-intent-v2\0";
type Identity = [u64; 2];
type Permissions = (u32, u32, u32);
type FactsWire = (u64, [i64; 2], [i64; 2], u64);

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct NativeIdentity {
    device: u64,
    inode: u64,
}
impl NativeIdentity {
    fn observed(&self) -> Identity {
        [self.device, self.inode]
    }
    fn valid(&self) -> bool {
        self.inode != 0
    }
}
/// Raw source-root-relative observation, never a pathname to reopen or a grant.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NativeReservationLocator {
    root: NativeIdentity,
    parents: Vec<Vec<u8>>,
    name: Vec<u8>,
    identity: Option<NativeIdentity>,
}
impl NativeReservationLocator {
    pub fn root_identity(&self) -> Identity {
        self.root.observed()
    }
    pub fn parents(&self) -> &[Vec<u8>] {
        &self.parents
    }
    pub fn name(&self) -> &[u8] {
        &self.name
    }
    pub fn identity(&self) -> Option<Identity> {
        self.identity.as_ref().map(NativeIdentity::observed)
    }
    fn valid(&self) -> bool {
        self.root.valid()
            && self.parents.len() <= 128
            && self.parents.iter().all(|name| component(name))
            && component(&self.name)
            && self.identity.as_ref().is_some_and(NativeIdentity::valid)
    }
}
fn component(name: &[u8]) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && name != b"."
        && name != b".."
        && !name.contains(&b'/')
        && !name.contains(&0)
}
fn attributes_valid(attributes: &BTreeMap<Vec<u8>, Vec<u8>>) -> bool {
    if attributes.len() > 128 {
        return false;
    }
    let total = attributes.iter().try_fold(0_usize, |total, (name, value)| {
        if name.is_empty() || name.len() > 255 || name.contains(&0) || value.len() > 65_536 {
            return None;
        }
        total
            .checked_add(name.len())?
            .checked_add(1)?
            .checked_add(value.len())
    });
    total.is_some_and(|total| total <= 1_048_576)
}
fn refused() -> io::Error {
    io::ErrorKind::PermissionDenied.into()
}
/// Metadata observed on the actual held native inode. It claims no content hash,
/// immutable archive, successful replacement or independent read permission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NativeReservationFacts {
    pub byte_len: u64,
    pub mtime: [i64; 2],
    pub ctime: [i64; 2],
    pub nlink: u64,
}
impl NativeReservationFacts {
    fn from_wire(value: FactsWire) -> Self {
        Self {
            byte_len: value.0,
            mtime: value.1,
            ctime: value.2,
            nlink: value.3,
        }
    }
    fn valid(&self) -> bool {
        self.nlink != 0
            && self
                .mtime
                .last()
                .is_some_and(|nanos| (0..1_000_000_000).contains(nanos))
            && self
                .ctime
                .last()
                .is_some_and(|nanos| (0..1_000_000_000).contains(nanos))
    }
}
// Exact unpublished source getter tuple: operation, owner, source/destination
// parents, protected custody, permissions, held facts, attributes and locators.
// NativeAttributes is a newtype BTreeMap and has the same postcard map encoding.
/// Source-minted description of namespace replacement intent. Observing this
/// value admits neither an operation nor canonical history authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum NativeReplacementPurpose {
    ReplacementRename,
    AtomicSave,
}
type Wire = (
    [u8; 16],
    [u8; 32],
    NativeIdentity,
    NativeIdentity,
    NativeIdentity,
    (Permissions, Permissions),
    (FactsWire, FactsWire),
    BTreeMap<Vec<u8>, Vec<u8>>,
    NativeReservationLocator,
    NativeReservationLocator,
    NativeReservationLocator,
    NativeReplacementPurpose,
);
/// Closed, canonical observations bound to an expected reservation and its hash.
/// This type is deliberately not a replacement, source or history admission.
#[derive(Clone, Debug)]
pub struct NativeReservationDescription {
    wire: Wire,
}
impl NativeReservationDescription {
    /// # Errors
    /// Refuses overlong, noncanonical, malformed or mismatched observations.
    /// The expected values must themselves come from actual pending custody.
    pub fn decode(
        bytes: &[u8],
        operation: [u8; 16],
        owner: [u8; 32],
        intent: [u8; 32],
    ) -> io::Result<Self> {
        if bytes.is_empty()
            || bytes.len() > LIMIT
            || owner == [0; 32]
            || intent == [0; 32]
            || uuid::Uuid::from_bytes(operation).is_nil()
        {
            return Err(refused());
        }
        let mut hash = blake3::Hasher::new();
        hash.update(DOMAIN);
        hash.update(bytes);
        if hash.finalize().as_bytes() != &intent {
            return Err(refused());
        }
        let wire: Wire = postcard::from_bytes(bytes).map_err(io::Error::other)?;
        if postcard::to_allocvec(&wire).map_err(io::Error::other)? != bytes
            || wire.0 != operation
            || wire.1 != owner
            || !wire.2.valid()
            || !wire.3.valid()
            || !wire.4.valid()
            || wire.5.0.2 & !0o7777 != 0
            || wire.5.1.2 & !0o7777 != 0
            || !NativeReservationFacts::from_wire(wire.6.0).valid()
            || !NativeReservationFacts::from_wire(wire.6.1).valid()
            || wire.6.0.3 != 1
            || !attributes_valid(&wire.7)
            || !wire.8.valid()
            || !wire.9.valid()
            || !wire.10.valid()
            || wire.8.root != wire.9.root
            || wire.8.root != wire.10.root
            || wire.9.parents != wire.10.parents
            || wire.9.name != wire.10.name
            || wire.8.identity != wire.10.identity
        {
            return Err(refused());
        }
        Ok(Self { wire })
    }
    pub fn purpose(&self) -> NativeReplacementPurpose {
        self.wire.11
    }
    pub fn operation(&self) -> [u8; 16] {
        self.wire.0
    }
    pub fn source_owner(&self) -> [u8; 32] {
        self.wire.1
    }
    pub fn source_parent_identity(&self) -> Identity {
        self.wire.2.observed()
    }
    pub fn destination_parent_identity(&self) -> Identity {
        self.wire.3.observed()
    }
    pub fn source_permissions(&self) -> Permissions {
        self.wire.5.0
    }
    pub fn victim_permissions(&self) -> Permissions {
        self.wire.5.1
    }
    pub fn source_facts(&self) -> NativeReservationFacts {
        NativeReservationFacts::from_wire(self.wire.6.0)
    }
    pub fn victim_facts(&self) -> NativeReservationFacts {
        NativeReservationFacts::from_wire(self.wire.6.1)
    }
    pub fn source(&self) -> &NativeReservationLocator {
        &self.wire.8
    }
    pub fn victim(&self) -> &NativeReservationLocator {
        &self.wire.9
    }
    pub fn destination(&self) -> &NativeReservationLocator {
        &self.wire.10
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(inode: u64) -> NativeIdentity {
        NativeIdentity { device: 8, inode }
    }
    fn wire() -> Wire {
        let root = identity(10);
        let locator = |name: &[u8], inode| NativeReservationLocator {
            root: root.clone(),
            parents: vec![b"Documents".to_vec()],
            name: name.to_vec(),
            identity: Some(identity(inode)),
        };
        (
            [1; 16],
            [2; 32],
            identity(20),
            identity(20),
            identity(30),
            ((1000, 1000, 0o600), (1000, 1000, 0o640)),
            (
                (21, [1_700_000_000, 40], [1_700_000_001, 50], 1),
                (33, [1_690_000_000, 60], [1_690_000_001, 70], 2),
            ),
            BTreeMap::new(),
            locator(b".save-temp", 40),
            locator(b"report.txt", 50),
            locator(b"report.txt", 40),
            NativeReplacementPurpose::ReplacementRename,
        )
    }
    fn encode(wire: &Wire) -> (Vec<u8>, [u8; 32]) {
        let bytes = postcard::to_allocvec(wire).unwrap();
        let mut hash = blake3::Hasher::new();
        hash.update(DOMAIN);
        hash.update(&bytes);
        (bytes, *hash.finalize().as_bytes())
    }
    #[test]
    fn intent_v2_binds_source_minted_atomic_purpose_and_refuses_legacy_or_relabelled_bytes() {
        let mut value = wire();
        let (generic, generic_digest) = encode(&value);
        assert_eq!(
            NativeReservationDescription::decode(&generic, value.0, value.1, generic_digest)
                .unwrap()
                .purpose(),
            NativeReplacementPurpose::ReplacementRename
        );
        value.11 = NativeReplacementPurpose::AtomicSave;
        let (atomic, atomic_digest) = encode(&value);
        assert_ne!(generic_digest, atomic_digest);
        assert_eq!(
            NativeReservationDescription::decode(&atomic, value.0, value.1, atomic_digest)
                .unwrap()
                .purpose(),
            NativeReplacementPurpose::AtomicSave
        );
        assert!(
            NativeReservationDescription::decode(&atomic, value.0, value.1, generic_digest)
                .is_err()
        );
        let mut hash = blake3::Hasher::new();
        hash.update(b"tilo-native-retained-replacement-intent-v1\0");
        hash.update(&atomic);
        assert!(
            NativeReservationDescription::decode(
                &atomic,
                value.0,
                value.1,
                *hash.finalize().as_bytes()
            )
            .is_err()
        );
    }
    #[test]
    fn descriptive_source_tuple_keeps_actual_facts_and_raw_locators() {
        let wire = wire();
        let (bytes, digest) = encode(&wire);
        let parsed = NativeReservationDescription::decode(&bytes, wire.0, wire.1, digest).unwrap();
        assert_eq!(parsed.source_facts().byte_len, 21);
        assert_eq!(parsed.victim_facts().byte_len, 33);
        assert_eq!(parsed.victim_facts().nlink, 2);
        assert_eq!(parsed.victim().name(), b"report.txt");
        assert_eq!(parsed.destination().identity(), Some([8, 40]));
        assert!(NativeReservationDescription::decode(&bytes, [3; 16], wire.1, digest).is_err());
        assert!(NativeReservationDescription::decode(&bytes, wire.0, [4; 32], digest).is_err());
        assert!(NativeReservationDescription::decode(&bytes, wire.0, wire.1, [5; 32]).is_err());
    }
    #[test]
    fn matching_hash_does_not_admit_bad_names_times_or_destination() {
        let mut wire = wire();
        wire.9.name = b"..".to_vec();
        let (bytes, digest) = encode(&wire);
        assert!(NativeReservationDescription::decode(&bytes, wire.0, wire.1, digest).is_err());
        wire = super::tests::wire();
        wire.6.1.1 = [1, 1_000_000_000];
        let (bytes, digest) = encode(&wire);
        assert!(NativeReservationDescription::decode(&bytes, wire.0, wire.1, digest).is_err());
        wire = super::tests::wire();
        wire.10.identity = Some(identity(99));
        let (bytes, digest) = encode(&wire);
        assert!(NativeReservationDescription::decode(&bytes, wire.0, wire.1, digest).is_err());
    }
}
