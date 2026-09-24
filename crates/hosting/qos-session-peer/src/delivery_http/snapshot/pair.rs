//! Local envelope only. Members remain signed inert bytes requiring native checks.
use super::SnapshotFailure;
const LIMIT: usize = 128 * 1024;
const MEMBER: usize = 64 * 1024;
const HEADER: usize = 20;
#[derive(Debug)]
pub struct ManifestPair<'a> {
    current: &'a [u8],
    previous: Option<&'a [u8]>,
    renewed_through: u64,
}
pub struct AuthenticatedPair {
    current: tilo_relay::delivery_v3::state::SignedSnapshotManifest,
    previous: Option<tilo_relay::delivery_v3::state::SignedSnapshotManifest>,
    renewed_through: u64,
}
/// Timeless signature/canonical evidence only. Never a live retained owner pin.
pub struct RetainedAuthenticatedPair {
    current: tilo_relay::delivery_v3::state::SignedSnapshotManifest,
    previous: Option<tilo_relay::delivery_v3::state::SignedSnapshotManifest>,
    renewed_through: u64,
}
impl RetainedAuthenticatedPair {
    pub fn current(&self) -> &tilo_relay::delivery_v3::state::SignedSnapshotManifest {
        &self.current
    }
    pub fn previous(&self) -> Option<&tilo_relay::delivery_v3::state::SignedSnapshotManifest> {
        self.previous.as_ref()
    }
    pub fn renewed_through(&self) -> u64 {
        self.renewed_through
    }
    /// Time validation only; fresh concrete owner/pin matching remains required.
    pub fn validate_current(self, now_ns: u64) -> Result<AuthenticatedPair, SnapshotFailure> {
        // Structure/signature/content/floor were already checked by the only
        // constructor. Match the shared manifest's five-minute current-time rule.
        let expiry = self.current.issue().expires_at_ns;
        if expiry <= now_ns
            || expiry
                .checked_sub(now_ns)
                .is_none_or(|n| n > 300_000_000_000)
        {
            return Err(SnapshotFailure::Invalid);
        }
        Ok(AuthenticatedPair {
            current: self.current,
            previous: self.previous,
            renewed_through: self.renewed_through,
        })
    }
}
impl AuthenticatedPair {
    /// Signed inert claims only; the concrete owner must select the live pin.
    pub fn current(&self) -> &tilo_relay::delivery_v3::state::SignedSnapshotManifest {
        &self.current
    }
    /// Timeless recovery anchor, never independent current authorization.
    pub fn previous(&self) -> Option<&tilo_relay::delivery_v3::state::SignedSnapshotManifest> {
        self.previous.as_ref()
    }
    pub fn renewed_through(&self) -> u64 {
        self.renewed_through
    }
}
impl<'a> ManifestPair<'a> {
    pub fn new(current: &'a [u8], previous: Option<&'a [u8]>) -> Result<Self, SnapshotFailure> {
        if !(65..=MEMBER).contains(&current.len())
            || previous.is_some_and(|p| !(65..=MEMBER).contains(&p.len()))
        {
            return Err(SnapshotFailure::Invalid);
        }
        let length = HEADER
            .checked_add(current.len())
            .and_then(|n| n.checked_add(previous.map_or(0, |p| p.len())))
            .ok_or(SnapshotFailure::Capacity)?;
        if length > LIMIT {
            return Err(SnapshotFailure::Capacity);
        }
        Ok(Self {
            current,
            previous,
            renewed_through: 0,
        })
    }
    pub fn decode(bytes: &'a [u8]) -> Result<Self, SnapshotFailure> {
        if bytes.len() < HEADER || bytes.len() > LIMIT || &bytes[..4] != b"QSP2" {
            return Err(SnapshotFailure::Invalid);
        }
        let current = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
        let previous = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
        let end = HEADER
            .checked_add(current)
            .ok_or(SnapshotFailure::Invalid)?;
        if end.checked_add(previous) != Some(bytes.len()) {
            return Err(SnapshotFailure::Invalid);
        }
        Self::new(
            bytes.get(HEADER..end).ok_or(SnapshotFailure::Invalid)?,
            if previous == 0 {
                None
            } else {
                Some(bytes.get(end..).ok_or(SnapshotFailure::Invalid)?)
            },
        )?
        .with_renewed_through(u64::from_le_bytes(bytes[12..20].try_into().unwrap()))
    }
    pub fn with_renewed_through(mut self, floor: u64) -> Result<Self, SnapshotFailure> {
        if floor > u64::from(tilo_relay::delivery_v3::state::MAX_SNAPSHOT_CHUNKS) {
            return Err(SnapshotFailure::Invalid);
        }
        self.renewed_through = floor;
        Ok(self)
    }
    pub fn renewed_through(&self) -> u64 {
        self.renewed_through
    }
    pub fn encoded_len(&self) -> usize {
        HEADER + self.current.len() + self.previous.map_or(0, |p| p.len())
    }
    pub fn write_to(&self, output: &mut [u8]) -> Result<usize, SnapshotFailure> {
        let length = self.encoded_len();
        if output.len() < length {
            return Err(SnapshotFailure::Capacity);
        }
        output[..4].copy_from_slice(b"QSP2");
        output[4..8].copy_from_slice(&(self.current.len() as u32).to_le_bytes());
        output[8..12].copy_from_slice(&(self.previous.map_or(0, |p| p.len()) as u32).to_le_bytes());
        output[12..20].copy_from_slice(&self.renewed_through.to_le_bytes());
        let end = HEADER + self.current.len();
        output[HEADER..end].copy_from_slice(self.current);
        if let Some(previous) = self.previous {
            output[end..length].copy_from_slice(previous);
        }
        Ok(length)
    }
    /// Authenticate private recovery candidates without asserting current time.
    /// Only the actual durable owner pin may select a candidate for live use.
    pub fn authenticate_retained(
        &self,
        key: &ed25519_dalek::VerifyingKey,
    ) -> Result<RetainedAuthenticatedPair, SnapshotFailure> {
        use tilo_relay::delivery_v3::state::SnapshotManifest;
        let current = SnapshotManifest::from_bytes(self.current)
            .map_err(|_| SnapshotFailure::Invalid)?
            .authenticate_retained(key)
            .map_err(|_| SnapshotFailure::Invalid)?;
        let previous = self
            .previous
            .map(|bytes| {
                SnapshotManifest::from_bytes(bytes)
                    .and_then(|manifest| manifest.authenticate_retained(key))
            })
            .transpose()
            .map_err(|_| SnapshotFailure::Invalid)?;
        if previous.as_ref().is_some_and(|previous| {
            !current.same_content(previous)
                || previous.issue().expires_at_ns >= current.issue().expires_at_ns
        }) {
            return Err(SnapshotFailure::Invalid);
        }
        if self.renewed_through > current.issue().chunk_digests.len() as u64 {
            return Err(SnapshotFailure::Invalid);
        }
        Ok(RetainedAuthenticatedPair {
            current,
            previous,
            renewed_through: self.renewed_through,
        })
    }
    /// Timed signed-byte evidence only; actual owner pin/current authority is separate.
    pub fn authenticate(
        &self,
        key: &ed25519_dalek::VerifyingKey,
        now_ns: u64,
    ) -> Result<AuthenticatedPair, SnapshotFailure> {
        self.authenticate_retained(key)?.validate_current(now_ns)
    }
    pub fn current(&self) -> &'a [u8] {
        self.current
    }
    pub fn previous(&self) -> Option<&'a [u8]> {
        self.previous
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn snapshot_pair_literal_borrows_exact_opaque_members() {
        let current = [3; 65];
        let previous = [4; 66];
        let pair = ManifestPair::new(&current, Some(&previous)).unwrap();
        let mut literal = b"QSP2".to_vec();
        literal.extend(65u32.to_le_bytes());
        literal.extend(66u32.to_le_bytes());
        literal.extend(0u64.to_le_bytes());
        literal.extend(current);
        literal.extend(previous);
        let mut output = vec![0; pair.encoded_len()];
        assert_eq!(pair.write_to(&mut output).unwrap(), literal.len());
        assert_eq!(output, literal);
        let decoded = ManifestPair::decode(&literal).unwrap();
        assert_eq!(decoded.current(), current);
        assert_eq!(decoded.previous(), Some(previous.as_slice()));
        assert_eq!(decoded.current().as_ptr(), literal[20..].as_ptr());
        assert_eq!(decoded.previous().unwrap().as_ptr(), literal[85..].as_ptr());
        for end in 0..literal.len() {
            assert!(ManifestPair::decode(&literal[..end]).is_err());
        }
        literal.push(0);
        assert!(ManifestPair::decode(&literal).is_err());
        let pair = ManifestPair::new(&current, None).unwrap();
        let mut output = vec![0; pair.encoded_len()];
        pair.write_to(&mut output).unwrap();
        assert!(ManifestPair::decode(&output).unwrap().previous().is_none());
    }
    #[test]
    fn snapshot_pair_combined_budget_includes_header_before_any_renewal_write() {
        let maximum = vec![0; MEMBER];
        // Both are individually size-valid inert SnapshotManifest envelopes.
        assert!(tilo_relay::delivery_v3::state::SnapshotManifest::from_bytes(&maximum).is_ok());
        assert!(ManifestPair::new(&maximum, Some(&maximum)).is_err());
        let boundary = vec![0; MEMBER - 20];
        let pair = ManifestPair::new(&maximum, Some(&boundary)).unwrap();
        assert_eq!(pair.encoded_len(), LIMIT);
        assert!(ManifestPair::new(&[], None).is_err());
        assert!(ManifestPair::new(&[0; 64], None).is_err());
        assert!(ManifestPair::new(&maximum, Some(&[])).is_err());
        let mut old = vec![9; pair.encoded_len() - 1];
        let before = old.clone();
        assert!(pair.write_to(&mut old).is_err());
        assert_eq!(old, before, "refusal cannot damage old backing");
        for (a, b) in [(u32::MAX, 1), (65, u32::MAX), (0, 65), (65, 64)] {
            let mut bytes = b"QSP2".to_vec();
            bytes.extend(a.to_le_bytes());
            bytes.extend(b.to_le_bytes());
            bytes.resize(150, 0);
            assert!(ManifestPair::decode(&bytes).is_err());
        }
    }
}

#[cfg(test)]
mod authenticity_tests {
    use super::*;
    use tilo_relay::delivery_v3::{
        commit::CommitRef,
        cursor::CursorIssue,
        state::{ManifestIssue, SnapshotKind, SnapshotManifest},
    };
    fn issue(key: &ed25519_dalek::SigningKey, expires: u64) -> ManifestIssue {
        ManifestIssue {
            scope: CursorIssue {
                room: "00000000-0000-0000-0000-000000000001".into(),
                workspace: 1,
                workspace_root: [1; 32],
                host_device: key.verifying_key().to_bytes(),
                host_root: [2; 32],
                consumer_device: [3; 32],
                consumer_root: [4; 32],
                consumer: [5; 16],
                store_generation: [6; 16],
                authorization_view: [7; 32],
                committed: CommitRef {
                    sequence: 9,
                    digest: [8; 32],
                },
            },
            pin: [9; 16],
            kind: SnapshotKind::Baseline,
            predecessor: None,
            baseline_generation: 8,
            expires_at_ns: expires,
            item_count: 1,
            stream_bytes: 46,
            chunk_size: 240 * 1024,
            chunk_digests: vec![[10; 32]],
        }
    }
    #[test]
    fn snapshot_pair_retained_authentication_keeps_expired_candidates_inert() {
        let key = ed25519_dalek::SigningKey::from_bytes(&[71; 32]);
        let old = SnapshotManifest::issue(issue(&key, 200), &key, 100).unwrap();
        let current = SnapshotManifest::issue(issue(&key, 400), &key, 250).unwrap();
        let canonical = ManifestPair::new(old.as_bytes(), None).unwrap();
        let pending = ManifestPair::new(current.as_bytes(), Some(old.as_bytes())).unwrap();
        let expired = canonical
            .authenticate_retained(&key.verifying_key())
            .unwrap();
        assert_eq!(expired.current().issue().expires_at_ns, 200);
        assert!(expired.validate_current(300).is_err());
        let newer = pending.authenticate_retained(&key.verifying_key()).unwrap();
        assert_eq!(newer.previous().unwrap().issue().expires_at_ns, 200);
        assert_eq!(newer.renewed_through(), 0);
        assert_eq!(
            newer
                .validate_current(300)
                .unwrap()
                .current()
                .issue()
                .expires_at_ns,
            400
        );
        assert!(
            pending
                .authenticate_retained(&key.verifying_key())
                .unwrap()
                .validate_current(400)
                .is_err()
        );
        let wrong = ed25519_dalek::SigningKey::from_bytes(&[72; 32]);
        assert!(
            pending
                .authenticate_retained(&wrong.verifying_key())
                .is_err()
        );
        let mut changed = issue(&key, 400);
        changed.chunk_digests[0] = [11; 32];
        let changed = SnapshotManifest::issue(changed, &key, 250).unwrap();
        assert!(
            ManifestPair::new(changed.as_bytes(), Some(old.as_bytes()))
                .unwrap()
                .authenticate_retained(&key.verifying_key())
                .is_err()
        );
        assert!(
            ManifestPair::new(current.as_bytes(), Some(old.as_bytes()))
                .unwrap()
                .with_renewed_through(2)
                .unwrap()
                .authenticate_retained(&key.verifying_key())
                .is_err()
        );
    }
    #[test]
    fn snapshot_pair_expired_anchor_requires_live_current_matching_content_and_signatures() {
        let key = ed25519_dalek::SigningKey::from_bytes(&[71; 32]);
        let old = SnapshotManifest::issue(issue(&key, 200), &key, 100).unwrap();
        let current = SnapshotManifest::issue(issue(&key, 400), &key, 250).unwrap();
        let pair = ManifestPair::new(current.as_bytes(), Some(old.as_bytes())).unwrap();
        let signed = pair.authenticate(&key.verifying_key(), 300).unwrap();
        assert_eq!(signed.current.issue().expires_at_ns, 400);
        assert_eq!(signed.previous.unwrap().issue().expires_at_ns, 200);
        assert!(pair.authenticate(&key.verifying_key(), 400).is_err());
        assert!(
            ManifestPair::new(current.as_bytes(), Some(old.as_bytes()))
                .unwrap()
                .with_renewed_through(2)
                .unwrap()
                .authenticate(&key.verifying_key(), 300)
                .is_err(),
            "in-range private floor must still fit actual authenticated chunk count"
        );
        let retained = ManifestPair::new(current.as_bytes(), Some(old.as_bytes()))
            .unwrap()
            .with_renewed_through(1)
            .unwrap();
        let mut retained_bytes = vec![0; retained.encoded_len()];
        retained.write_to(&mut retained_bytes).unwrap();
        assert_eq!(
            ManifestPair::decode(&retained_bytes)
                .unwrap()
                .authenticate(&key.verifying_key(), 300)
                .unwrap()
                .renewed_through,
            1
        );

        let mut changed = issue(&key, 400);
        changed.chunk_digests[0] = [11; 32];
        let changed = SnapshotManifest::issue(changed, &key, 250).unwrap();
        assert!(
            ManifestPair::new(changed.as_bytes(), Some(old.as_bytes()))
                .unwrap()
                .authenticate(&key.verifying_key(), 300)
                .is_err()
        );
        let mut bad = old.as_bytes().to_vec();
        *bad.last_mut().unwrap() ^= 1;
        assert!(
            ManifestPair::new(current.as_bytes(), Some(&bad))
                .unwrap()
                .authenticate(&key.verifying_key(), 300)
                .is_err()
        );
        // Resigning malformed member bytes cannot make its canonical claims valid.
        use ed25519_dalek::Signer;
        let mut noncanonical = old.as_bytes()[..old.as_bytes().len() - 64].to_vec();
        noncanonical.push(0);
        let mut hash = blake3::Hasher::new();
        hash.update(b"tilo-delivery-v3-snapshot-manifest-v1\0");
        hash.update(&noncanonical);
        let sig = key.sign(hash.finalize().as_bytes());
        noncanonical.extend(sig.to_bytes());
        assert!(
            ManifestPair::new(current.as_bytes(), Some(&noncanonical))
                .unwrap()
                .authenticate(&key.verifying_key(), 300)
                .is_err()
        );
    }
}

#[cfg(test)]
mod progress_tests {
    use super::*;
    #[test]
    fn snapshot_pair_qsp2_persists_canonical_progress_floor_and_rejects_old_format() {
        let current = [3; 65];
        let pair = ManifestPair::new(&current, None)
            .unwrap()
            .with_renewed_through(257)
            .unwrap();
        let mut literal = b"QSP2".to_vec();
        literal.extend(65u32.to_le_bytes());
        literal.extend(0u32.to_le_bytes());
        literal.extend(257u64.to_le_bytes());
        literal.extend(current);
        let mut output = vec![0; pair.encoded_len()];
        pair.write_to(&mut output).unwrap();
        assert_eq!(output, literal);
        assert_eq!(
            ManifestPair::decode(&literal).unwrap().renewed_through(),
            257
        );
        literal[..4].copy_from_slice(b"QSP1");
        assert!(ManifestPair::decode(&literal).is_err());
        assert!(
            ManifestPair::new(&current, None)
                .unwrap()
                .with_renewed_through(1106)
                .is_err()
        );
    }
}
