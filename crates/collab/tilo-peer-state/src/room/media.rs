//! Governed media data. Parsing this module's public values never grants authority.

use automerge::{AutoCommit, ROOT, ReadDoc, ScalarValue};
use serde::{
    Deserialize, Deserializer, Serialize,
    de::{MapAccess, Visitor},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    marker::PhantomData,
};
use tilo_user_identity::canonical::{canonical_bytes, from_canonical_bytes};

pub(crate) mod basis;
pub(crate) mod binding;
pub(crate) mod fingerprint;
pub(crate) mod read;

pub const FIELD: &str = "media_policy_v1";
pub const MAX_POLICY_BYTES: usize = 256 * 1024;
pub const MAX_CALLS: usize = 64;
pub const MAX_DENIED_PER_CALL: usize = 128;
pub const MAX_DENIED_TOTAL: usize = 512;
pub const MAX_RECORD_BYTES: usize = 1024;
pub const MAX_CHANNEL_BYTES: usize = 128;
pub const MAX_OBJECT_BYTES: usize = 256;
pub const MAX_RESULT_BYTES: usize = 2048;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoomMediaV1 {
    pub version: u8,
    pub room: [u8; 16],
    pub authorization_revision: u64,
    pub authorization_basis: [u8; 32],
    #[serde(deserialize_with = "calls_map")]
    pub calls: BTreeMap<[u8; 32], CallV1>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CallV1 {
    pub binding: ChatCallBindingV1,
    pub epoch: u64,
    pub phase: CallPhaseV1,
    pub allowed: DirectionsV1,
    #[serde(deserialize_with = "denied_map")]
    pub denied: BTreeMap<[u8; 32], DirectionsV1>,
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatCallBindingV1 {
    #[serde(deserialize_with = "app_string")]
    pub app_id: String,
    #[serde(deserialize_with = "record_string")]
    pub record_id: String,
    #[serde(deserialize_with = "channel_string")]
    pub channel_id: String,
    #[serde(deserialize_with = "object_string")]
    pub channel_object: String,
    #[serde(deserialize_with = "channel_string")]
    pub chat_workspace_id: String,
}
/// Frozen Postcard order: Active = 0, Ended = 1.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CallPhaseV1 {
    Active,
    Ended,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectionsV1 {
    pub publish: bool,
    pub subscribe: bool,
}
/// Safe receipt payload. V4 administration records migrate to None, never fabricated results.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaCallResultV1 {
    pub version: u8,
    pub call_id: [u8; 32],
    pub binding: ChatCallBindingV1,
    pub epoch: u64,
    pub authorization_revision: u64,
    pub phase: CallPhaseV1,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaPolicyError {
    Malformed,
    Conflict,
    UnsupportedVersion,
    Binding,
    Counter,
    Exhausted,
    Capacity,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransitionMode {
    Authoring,
    Admission,
    Restore,
}

/// Canonical security consistency digest, never an authority credential. Only
/// governed security fields participate; presentation labels and media itself do not.
/// Extra revocations are verified and intersect local knowledge before publication.
///
/// # Errors
///
/// Returns a [`MediaPolicyError`] if governed security fields are malformed or an extra
/// revocation fails verification.
pub fn security_basis(
    doc: &AutoCommit,
    extra_revocations: &[tilo_user_identity::RevocationEntry],
) -> Result<[u8; 32], MediaPolicyError> {
    basis::legacy(doc, extra_revocations)
}

/// Strict governed-document validation. Missing policy remains disabled. This
/// checks consistency only; callers must independently authenticate the writer.
///
/// # Errors
///
/// Returns a [`MediaPolicyError`] if either document's policy is malformed or the
/// transition is not permitted in `mode`.
pub fn validate_transition(
    before: Option<&AutoCommit>,
    after: &AutoCommit,
    mode: TransitionMode,
) -> Result<Option<RoomMediaV1>, MediaPolicyError> {
    let previous = before.map(parse).transpose()?.flatten();
    let current = parse(after)?;
    validate_policy_transition(previous.as_ref(), current.as_ref(), mode)?;
    if let Some(policy) = &current {
        if let Some(before) = before {
            let previous_revocations = revocation_floors(before)?;
            let current_revocations = revocation_floors(after)?;
            if previous_revocations.iter().any(|(key, floor)| {
                current_revocations
                    .get(key)
                    .is_none_or(|current| current > floor)
            }) {
                return Err(MediaPolicyError::Binding);
            }
        }
        if !after.get_missing_deps(&[]).is_empty()
            || policy.authorization_basis != security_basis(after, &[])?
        {
            return Err(MediaPolicyError::Binding);
        }
    }
    Ok(current)
}

// An observed root revocation is irreversible, even when a pinned checkpoint
// supplies a higher media revision and a self-consistent replacement digest.
/// Observed revocation sequence floors by (user root, revoked device).
type RevocationFloors = BTreeMap<([u8; 32], [u8; 32]), u64>;
fn revocation_floors(doc: &AutoCommit) -> Result<RevocationFloors, MediaPolicyError> {
    // A joined peer can project its room before its first governed snapshot.
    // Absence carries no observed floor; present evidence remains strictly
    // decoded, including legacy documents that have no media policy yet.
    if doc
        .get_all(ROOT, "peer_identities_v1")
        .map_err(|_| MediaPolicyError::Malformed)?
        .is_empty()
    {
        return Ok(BTreeMap::new());
    }
    let identities = strict_map(doc, &ROOT, "peer_identities_v1")?;
    let mut floors = BTreeMap::new();
    for key in doc.keys(&identities) {
        let row: crate::state::GovernedIdentity =
            serde_json::from_slice(&strict_bytes(doc, &identities, &key)?)
                .map_err(|_| MediaPolicyError::Malformed)?;
        for entry in row.revocations {
            if entry.user_public_key != row.device_cert.user_public_key {
                return Err(MediaPolicyError::Binding);
            }
            tilo_user_identity::verify_revocation_entry(&entry)
                .map_err(|_| MediaPolicyError::Binding)?;
            let floor = floors
                .entry((entry.user_public_key, entry.revoked_device_public_key))
                .or_insert(entry.revoked_at_ns);
            *floor = (*floor).min(entry.revoked_at_ns);
        }
    }
    Ok(floors)
}

/// Reconcile an already enabled policy within a verified governance transaction.
/// Does not initialize media or authorize the transaction's caller.
///
/// # Errors
///
/// Returns a [`MediaPolicyError`] if either policy is malformed or cannot be
/// reconciled.
pub fn refresh_authorization(
    before: &AutoCommit,
    after: &mut AutoCommit,
) -> Result<(), MediaPolicyError> {
    use automerge::transaction::Transactable;
    let Some(previous) = validate_transition(None, before, TransitionMode::Restore)? else {
        return if parse(after)?.is_none() {
            Ok(())
        } else {
            Err(MediaPolicyError::Binding)
        };
    };
    let mut current = parse(after)?.ok_or(MediaPolicyError::Binding)?;
    let basis = security_basis(after, &[])?;
    if basis != previous.authorization_basis {
        current.authorization_basis = basis;
        current.authorization_revision = next_counter(previous.authorization_revision)?;
        after
            .put(ROOT, FIELD, ScalarValue::Bytes(encode(&current)?))
            .map_err(|_| MediaPolicyError::Malformed)?;
        after.commit();
    }
    validate_transition(Some(before), after, TransitionMode::Authoring)?;
    Ok(())
}

/// Validate every active binding selecting this exact admitted app/record route.
/// Unrelated routes and ended tombstones carry no Chat mutation restriction.
///
/// # Errors
///
/// Returns a [`MediaPolicyError`] if a binding for this route no longer resolves in
/// `candidate`.
pub fn validate_bound_chat_change(
    policy: &RoomMediaV1,
    app: &str,
    record: &str,
    candidate: &impl ReadDoc,
) -> Result<(), MediaPolicyError> {
    binding::validate_legacy(policy, app, record, candidate)
}
/// Resolve one exact admitted Chat object, never a synthesized/default channel.
///
/// # Errors
///
/// Returns a [`MediaPolicyError`] if the route or channel does not resolve to one exact
/// admitted Chat object.
pub fn resolve_chat_binding(
    app: &str,
    record: &str,
    channel: &str,
    candidate: &impl ReadDoc,
) -> Result<ChatCallBindingV1, MediaPolicyError> {
    binding::resolve_legacy(app, record, channel, candidate).map(binding::Binding::into_legacy)
}

fn canonical_key(value: &str) -> Result<[u8; 32], MediaPolicyError> {
    parse_call_id(value)
}
fn strict_map(
    doc: &impl ReadDoc,
    parent: &automerge::ObjId,
    key: &str,
) -> Result<automerge::ObjId, MediaPolicyError> {
    super::read::strict_map(&super::read::LegacyRead(doc), parent, key)
        .map_err(super::read::legacy_policy_error)
}
#[cfg(test)]
fn strict_string(
    doc: &impl ReadDoc,
    parent: &automerge::ObjId,
    key: &str,
) -> Result<String, MediaPolicyError> {
    super::read::strict_string(&super::read::LegacyRead(doc), parent, key)
        .map_err(super::read::legacy_policy_error)
}
fn strict_bytes(
    doc: &impl ReadDoc,
    parent: &automerge::ObjId,
    key: &str,
) -> Result<Vec<u8>, MediaPolicyError> {
    super::read::strict_bytes(&super::read::LegacyRead(doc), parent, key)
        .map_err(super::read::legacy_policy_error)
}

/// Canonical encoding of a validated room media policy.
///
/// # Errors
///
/// Returns a [`MediaPolicyError`] if the policy is invalid or cannot be encoded.
pub fn encode(value: &RoomMediaV1) -> Result<Vec<u8>, MediaPolicyError> {
    validate(value)?;
    let bytes = canonical_bytes(value).map_err(|_| MediaPolicyError::Malformed)?;
    if bytes.len() > MAX_POLICY_BYTES {
        return Err(MediaPolicyError::Capacity);
    }
    Ok(bytes)
}
/// Strict data parsing only: the caller still verifies writer, security basis and current authority.
///
/// # Errors
///
/// Returns a [`MediaPolicyError`] if a present policy is malformed or invalid.
pub fn parse(doc: &AutoCommit) -> Result<Option<RoomMediaV1>, MediaPolicyError> {
    read::parse(&super::read::LegacyRead(doc), &read::LegacyPolicy).map_err(read::legacy_error)
}

/// Structural validation of a room media policy.
///
/// # Errors
///
/// Returns a [`MediaPolicyError`] naming the first invalid field.
pub fn validate(value: &RoomMediaV1) -> Result<(), MediaPolicyError> {
    if value.version != 1 {
        return Err(MediaPolicyError::UnsupportedVersion);
    }
    if value.room == [0; 16] {
        return Err(MediaPolicyError::Binding);
    }
    if value.authorization_revision == 0 {
        return Err(MediaPolicyError::Counter);
    }
    if value.calls.len() > MAX_CALLS {
        return Err(MediaPolicyError::Capacity);
    }
    let mut denied_total = 0usize;
    for (id, call) in &value.calls {
        if *id == [0; 32] {
            return Err(MediaPolicyError::Binding);
        }
        call.binding.validate()?;
        // At most 64 rows: compare only earlier keys without constructing a
        // second heap-backed set. This preserves the prior validation order.
        if value.calls.range(..*id).any(|(_, prior)| {
            prior.binding.app_id == call.binding.app_id
                && prior.binding.record_id == call.binding.record_id
                && prior.binding.channel_object == call.binding.channel_object
        }) {
            return Err(MediaPolicyError::Binding);
        }
        if call.epoch == 0 {
            return Err(MediaPolicyError::Counter);
        }
        denied_total = denied_total
            .checked_add(call.denied.len())
            .ok_or(MediaPolicyError::Capacity)?;
        if call.denied.len() > MAX_DENIED_PER_CALL || denied_total > MAX_DENIED_TOTAL {
            return Err(MediaPolicyError::Capacity);
        }
        if call
            .denied
            .iter()
            .any(|(root, denied)| *root == [0; 32] || !(denied.publish || denied.subscribe))
        {
            return Err(MediaPolicyError::Binding);
        }
    }
    Ok(())
}

impl ChatCallBindingV1 {
    /// Whether this binds channel `channel` of Chat record `record` of app
    /// `app`: what identifies a call's channel. Its object and the
    /// workspace it once named are recorded with it, not compared (see
    /// [`resolve_chat_binding`]).
    #[must_use]
    pub fn names_channel(&self, app: &str, record: &str, channel: &str) -> bool {
        self.app_id == app && self.record_id == record && self.channel_id == channel
    }

    /// Whether `self` and `other` bind the same channel.
    #[must_use]
    pub fn same_channel(&self, other: &Self) -> bool {
        other.names_channel(&self.app_id, &self.record_id, &self.channel_id)
    }

    /// Structural validation of a Chat call binding.
    ///
    /// # Errors
    ///
    /// Returns a [`MediaPolicyError`] if a field is not canonical.
    pub fn validate(&self) -> Result<(), MediaPolicyError> {
        binding::validate_fields(
            &self.app_id,
            &self.record_id,
            &self.channel_id,
            &self.channel_object,
            &self.chat_workspace_id,
        )
    }
}

fn text_valid(value: &str, max: usize, empty: bool) -> bool {
    (empty || !value.is_empty()) && value.len() <= max && !value.chars().any(char::is_control)
}
fn lower_hex(value: &str) -> bool {
    value
        .bytes()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Structural fences only. In particular, the basis digest must independently be derived from
/// the current committed security inputs; accepting two matching claims is not that derivation.
///
/// # Errors
///
/// Returns a [`MediaPolicyError`] if the transition breaks a structural fence for
/// `mode`.
pub fn validate_policy_transition(
    before: Option<&RoomMediaV1>,
    after: Option<&RoomMediaV1>,
    mode: TransitionMode,
) -> Result<(), MediaPolicyError> {
    if let Some(before) = before {
        validate(before)?;
    }
    let Some(after) = after else {
        return if before.is_some() {
            Err(MediaPolicyError::Counter)
        } else {
            Ok(())
        };
    };
    validate(after)?;
    let Some(before) = before else {
        if mode == TransitionMode::Authoring
            && (after.authorization_revision != 1
                || after
                    .calls
                    .values()
                    .any(|call| call.epoch != 1 || call.phase != CallPhaseV1::Active))
        {
            return Err(MediaPolicyError::Counter);
        }
        return Ok(());
    };
    if before.room != after.room {
        return Err(MediaPolicyError::Binding);
    }
    counter_transition(
        before.authorization_revision,
        after.authorization_revision,
        before.authorization_basis != after.authorization_basis,
        mode,
    )?;
    for (id, old) in &before.calls {
        let new = after.calls.get(id).ok_or(MediaPolicyError::Binding)?;
        if old.binding != new.binding {
            return Err(MediaPolicyError::Binding);
        }
        let changed =
            old.phase != new.phase || old.allowed != new.allowed || old.denied != new.denied;
        counter_transition(old.epoch, new.epoch, changed, mode)?;
    }
    if mode == TransitionMode::Authoring
        && after.calls.iter().any(|(id, call)| {
            !before.calls.contains_key(id) && (call.epoch != 1 || call.phase != CallPhaseV1::Active)
        })
    {
        return Err(MediaPolicyError::Counter);
    }
    Ok(())
}
fn counter_transition(
    old: u64,
    new: u64,
    changed: bool,
    mode: TransitionMode,
) -> Result<(), MediaPolicyError> {
    if mode == TransitionMode::Authoring {
        let expected = if changed { next_counter(old)? } else { old };
        if new != expected {
            return Err(MediaPolicyError::Counter);
        }
    } else if new < old || (changed && new == old) {
        return Err(MediaPolicyError::Counter);
    }
    Ok(())
}
/// The counter after `value`.
///
/// # Errors
///
/// Returns [`MediaPolicyError::Counter`] for zero and [`MediaPolicyError::Exhausted`] at
/// `u64::MAX`.
pub fn next_counter(value: u64) -> Result<u64, MediaPolicyError> {
    if value == 0 {
        return Err(MediaPolicyError::Counter);
    }
    value.checked_add(1).ok_or(MediaPolicyError::Exhausted)
}
/// Parse a canonical lowercase-hex, nonzero 32-byte call id.
///
/// # Errors
///
/// Returns [`MediaPolicyError::Binding`] if `value` is not canonical.
pub fn parse_call_id(value: &str) -> Result<[u8; 32], MediaPolicyError> {
    if value.len() != 64 || !lower_hex(value) {
        return Err(MediaPolicyError::Binding);
    }
    let mut key = [0; 32];
    hex::decode_to_slice(value, &mut key).map_err(|_| MediaPolicyError::Binding)?;
    if key == [0; 32] {
        return Err(MediaPolicyError::Binding);
    }
    Ok(key)
}
impl MediaCallResultV1 {
    /// Aggregate restore must call this when deserializing a nested receipt directly.
    ///
    /// # Errors
    ///
    /// Returns a [`MediaPolicyError`] if the receipt is malformed.
    pub fn validate(&self) -> Result<(), MediaPolicyError> {
        if self.version != 1 {
            return Err(MediaPolicyError::UnsupportedVersion);
        }
        if self.call_id == [0; 32] {
            return Err(MediaPolicyError::Binding);
        }
        if self.epoch == 0 || self.authorization_revision == 0 {
            return Err(MediaPolicyError::Counter);
        }
        self.binding.validate()
    }
    /// Canonical encoding of a validated call result.
    ///
    /// # Errors
    ///
    /// Returns a [`MediaPolicyError`] if the result is invalid or cannot be encoded.
    pub fn encode(&self) -> Result<Vec<u8>, MediaPolicyError> {
        self.validate()?;
        canonical_bytes(self).map_err(|_| MediaPolicyError::Malformed)
    }
    /// Decode and validate a canonical call result.
    ///
    /// # Errors
    ///
    /// Returns a [`MediaPolicyError`] if the bytes are malformed or not canonical.
    pub fn decode(bytes: &[u8]) -> Result<Self, MediaPolicyError> {
        if bytes.len() > MAX_RESULT_BYTES {
            return Err(MediaPolicyError::Capacity);
        }
        preflight_result(bytes)?;
        let value: Self = from_canonical_bytes(bytes).map_err(|_| MediaPolicyError::Malformed)?;
        if value.encode()? != bytes {
            return Err(MediaPolicyError::Malformed);
        }
        Ok(value)
    }
}

/// Read the frozen Postcard field layout without building strings or maps. Postcard erases
/// Serde custom error categories, so capacity must be checked before owned deserialization.
/// A policy shares one denial budget across all calls; no over-budget map is constructed.
struct WireCursor<'a> {
    remaining: &'a [u8],
}
impl<'a> WireCursor<'a> {
    fn take<T: Deserialize<'a>>(&mut self) -> Result<T, MediaPolicyError> {
        let (value, remaining) =
            postcard::take_from_bytes(self.remaining).map_err(|_| MediaPolicyError::Malformed)?;
        self.remaining = remaining;
        Ok(value)
    }

    fn count(&mut self, max: usize) -> Result<usize, MediaPolicyError> {
        // Postcard sequence/string lengths use unsigned varints. Read u64 first so a
        // count exceeding a 32-bit usize still receives the typed capacity rejection.
        let count = usize::try_from(self.take::<u64>()?).map_err(|_| MediaPolicyError::Capacity)?;
        if count > max {
            return Err(MediaPolicyError::Capacity);
        }
        Ok(count)
    }

    fn string(&mut self, max: usize) -> Result<(), MediaPolicyError> {
        let count = self.count(max)?;
        let (value, remaining) = self
            .remaining
            .split_at_checked(count)
            .ok_or(MediaPolicyError::Malformed)?;
        std::str::from_utf8(value).map_err(|_| MediaPolicyError::Malformed)?;
        self.remaining = remaining;
        Ok(())
    }

    fn binding(&mut self) -> Result<(), MediaPolicyError> {
        self.string(64)?;
        self.string(MAX_RECORD_BYTES)?;
        self.string(MAX_CHANNEL_BYTES)?;
        self.string(MAX_OBJECT_BYTES)?;
        self.string(MAX_CHANNEL_BYTES)
    }

    fn version(&mut self) -> Result<(), MediaPolicyError> {
        if self.take::<u8>()? != 1 {
            return Err(MediaPolicyError::UnsupportedVersion);
        }
        Ok(())
    }

    fn finish(self) -> Result<(), MediaPolicyError> {
        if !self.remaining.is_empty() {
            return Err(MediaPolicyError::Malformed);
        }
        Ok(())
    }
}

fn preflight_policy(bytes: &[u8]) -> Result<(), MediaPolicyError> {
    let mut wire = WireCursor { remaining: bytes };
    wire.version()?;
    wire.take::<[u8; 16]>()?;
    wire.take::<u64>()?;
    wire.take::<[u8; 32]>()?;
    let calls = wire.count(MAX_CALLS)?;
    let mut denial_budget = MAX_DENIED_TOTAL;
    for _ in 0..calls {
        wire.take::<[u8; 32]>()?;
        wire.binding()?;
        wire.take::<u64>()?;
        wire.take::<CallPhaseV1>()?;
        wire.take::<DirectionsV1>()?;
        let denials = wire.count(MAX_DENIED_PER_CALL.min(denial_budget))?;
        denial_budget = denial_budget
            .checked_sub(denials)
            .ok_or(MediaPolicyError::Capacity)?;
        for _ in 0..denials {
            wire.take::<[u8; 32]>()?;
            wire.take::<DirectionsV1>()?;
        }
    }
    wire.finish()
}

fn preflight_result(bytes: &[u8]) -> Result<(), MediaPolicyError> {
    let mut wire = WireCursor { remaining: bytes };
    wire.version()?;
    wire.take::<[u8; 32]>()?;
    wire.binding()?;
    wire.take::<u64>()?;
    wire.take::<u64>()?;
    wire.take::<CallPhaseV1>()?;
    wire.finish()
}

/// Value comparison includes every conflicting field value, with domain-tagged fingerprints
/// avoiding a second copy of hostile byte strings. This is not validity or writer evidence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MediaGovernanceValue {
    Absent,
    Values(BTreeSet<[u8; 32]>),
    Unreadable,
}
pub fn governed_value(doc: Option<&AutoCommit>) -> MediaGovernanceValue {
    use fingerprint::FingerprintRead;
    let Some(doc) = doc else {
        return MediaGovernanceValue::Absent;
    };
    let mut fingerprints = BTreeSet::new();
    match super::read::LegacyRead(doc).visit_media_fingerprints(|hash| {
        fingerprints.insert(hash);
        Ok::<_, std::convert::Infallible>(())
    }) {
        Ok(0) => MediaGovernanceValue::Absent,
        Ok(_) => MediaGovernanceValue::Values(fingerprints),
        Err(_) => MediaGovernanceValue::Unreadable,
    }
}

fn bounded_map<'de, D, V, const MAX: usize>(
    deserializer: D,
) -> Result<BTreeMap<[u8; 32], V>, D::Error>
where
    D: Deserializer<'de>,
    V: Deserialize<'de>,
{
    struct MapVisitor<V, const MAX: usize>(PhantomData<V>);
    impl<'de, V: Deserialize<'de>, const MAX: usize> Visitor<'de> for MapVisitor<V, MAX> {
        type Value = BTreeMap<[u8; 32], V>;
        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            write!(formatter, "at most {MAX} unique media rows")
        }
        fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> Result<Self::Value, A::Error> {
            if access.size_hint().is_some_and(|size| size > MAX) {
                return Err(serde::de::Error::custom("media row bound exceeded"));
            }
            let mut rows = BTreeMap::new();
            while let Some(key) = access.next_key()? {
                if rows.len() == MAX || rows.contains_key(&key) {
                    return Err(serde::de::Error::custom("media row bound or duplicate key"));
                }
                rows.insert(key, access.next_value()?);
            }
            Ok(rows)
        }
    }
    deserializer.deserialize_map(MapVisitor::<V, MAX>(PhantomData))
}
fn calls_map<'de, D: Deserializer<'de>>(value: D) -> Result<BTreeMap<[u8; 32], CallV1>, D::Error> {
    bounded_map::<D, CallV1, MAX_CALLS>(value)
}
fn denied_map<'de, D: Deserializer<'de>>(
    value: D,
) -> Result<BTreeMap<[u8; 32], DirectionsV1>, D::Error> {
    bounded_map::<D, DirectionsV1, MAX_DENIED_PER_CALL>(value)
}
fn bounded_string<'de, D: Deserializer<'de>>(value: D, max: usize) -> Result<String, D::Error> {
    struct TextVisitor(usize);
    impl Visitor<'_> for TextVisitor {
        type Value = String;
        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            write!(formatter, "at most {} UTF-8 bytes", self.0)
        }
        fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<String, E> {
            if value.len() > self.0 {
                return Err(E::custom("media text bound exceeded"));
            }
            Ok(value.to_owned())
        }
    }
    value.deserialize_str(TextVisitor(max))
}
fn app_string<'de, D: Deserializer<'de>>(value: D) -> Result<String, D::Error> {
    bounded_string(value, 64)
}
fn record_string<'de, D: Deserializer<'de>>(value: D) -> Result<String, D::Error> {
    bounded_string(value, MAX_RECORD_BYTES)
}
fn channel_string<'de, D: Deserializer<'de>>(value: D) -> Result<String, D::Error> {
    bounded_string(value, MAX_CHANNEL_BYTES)
}
fn object_string<'de, D: Deserializer<'de>>(value: D) -> Result<String, D::Error> {
    bounded_string(value, MAX_OBJECT_BYTES)
}

impl fmt::Display for MediaPolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Malformed => "malformed media policy",
            Self::Conflict => "conflicting media policy",
            Self::UnsupportedVersion => "unsupported media policy version",
            Self::Binding => "invalid media binding",
            Self::Counter => "invalid media policy counter",
            Self::Exhausted => "media policy counter exhausted",
            Self::Capacity => "media policy capacity exceeded",
        })
    }
}
impl std::error::Error for MediaPolicyError {}

#[cfg(test)]
mod room_media_tests {
    use super::*;
    use automerge::{ActorId, ObjType, ROOT, transaction::Transactable};
    use tilo_automerge::ApplyIncoming;

    fn policy() -> RoomMediaV1 {
        RoomMediaV1 {
            version: 1,
            room: [1; 16],
            authorization_revision: 1,
            authorization_basis: [3; 32],
            calls: BTreeMap::from([(
                [7; 32],
                CallV1 {
                    binding: ChatCallBindingV1 {
                        app_id: "so.tilo.chat".into(),
                        record_id: String::new(),
                        channel_id: "voice".into(),
                        channel_object: "1@01".into(),
                        chat_workspace_id: "main".into(),
                    },
                    epoch: 1,
                    phase: CallPhaseV1::Active,
                    allowed: DirectionsV1 {
                        publish: true,
                        subscribe: true,
                    },
                    denied: BTreeMap::new(),
                },
            )]),
        }
    }

    fn room() -> AutoCommit {
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "room", uuid::Uuid::from_bytes([1; 16]).to_string())
            .unwrap();
        doc.commit();
        doc
    }

    fn raw(doc: &mut AutoCommit, value: &RoomMediaV1) {
        doc.put(
            ROOT,
            FIELD,
            ScalarValue::Bytes(postcard::to_allocvec(value).unwrap()),
        )
        .unwrap();
        doc.commit();
    }

    fn edited(mut edit: impl FnMut(&mut RoomMediaV1)) -> RoomMediaV1 {
        let mut value = policy();
        edit(&mut value);
        value
    }

    fn governed_legacy_room() -> (AutoCommit, ed25519_dalek::SigningKey, String) {
        let root = ed25519_dalek::SigningKey::from_bytes(&[91; 32]);
        let device = ed25519_dalek::SigningKey::from_bytes(&[92; 32]);
        let mut certificate = tilo_user_identity::DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "Governor".into(),
            capabilities: tilo_user_identity::DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        tilo_user_identity::sign_device_cert(&mut certificate, &root).unwrap();
        let changes = crate::state::seed_room(
            &uuid::Uuid::from_bytes([1; 16]).to_string(),
            &certificate,
            std::slice::from_ref(&certificate),
        )
        .unwrap();
        (
            super::super::candidate(None, &changes).unwrap(),
            root,
            hex::encode(certificate.device_public_key),
        )
    }

    fn enable_media(doc: &mut AutoCommit) -> RoomMediaV1 {
        let mut value = policy();
        value.authorization_basis = security_basis(doc, &[]).unwrap();
        raw(doc, &value);
        value
    }

    #[test]
    fn legacy_identity_absence_permits_first_media_checkpoint() {
        // A joined shell guest can project its room id before receiving the
        // owner's governance. No identity map has existed in this projection.
        let before = room();
        let (mut after, _, _) = governed_legacy_room();
        let value = enable_media(&mut after);
        assert_eq!(
            validate_transition(Some(&before), &after, TransitionMode::Admission),
            Ok(Some(value)),
        );
    }

    #[test]
    fn legacy_identity_present_malformed_or_conflicting_evidence_is_rejected() {
        let (mut after, _, _) = governed_legacy_room();
        let value = enable_media(&mut after);
        assert_eq!(
            validate_transition(None, &after, TransitionMode::Restore),
            Ok(Some(value))
        );
        let mut scalar = room();
        scalar.put(ROOT, "peer_identities_v1", "not a map").unwrap();
        let mut list = room();
        list.put_object(ROOT, "peer_identities_v1", ObjType::List)
            .unwrap();
        let mut left = room();
        let mut right = left.fork();
        left.put_object(ROOT, "peer_identities_v1", ObjType::Map)
            .unwrap();
        right
            .put_object(ROOT, "peer_identities_v1", ObjType::Map)
            .unwrap();
        left.merge_incoming(&mut right).unwrap();
        assert_eq!(left.get_all(ROOT, "peer_identities_v1").unwrap().len(), 2);
        let (mut row, _, device) = governed_legacy_room();
        let identities = row.get(ROOT, "peer_identities_v1").unwrap().unwrap().1;
        row.put(&identities, device, "not identity bytes").unwrap();
        for before in [&scalar, &list, &left, &row] {
            assert!(validate_transition(Some(before), &after, TransitionMode::Admission).is_err());
        }
    }

    #[test]
    fn legacy_identity_revocation_floor_survives_media_bootstrap() {
        let (mut before, root, device) = governed_legacy_room();
        let identities = before.get(ROOT, "peer_identities_v1").unwrap().unwrap().1;
        let mut row: crate::state::GovernedIdentity =
            serde_json::from_slice(&strict_bytes(&before, &identities, &device).unwrap()).unwrap();
        let mut revoked = tilo_user_identity::RevocationEntry {
            user_public_key: root.verifying_key().to_bytes(),
            revoked_device_public_key: [99; 32],
            revoked_at_ns: 10,
            reason: tilo_user_identity::RevocationReason::Retired,
            signature: [0; 64],
        };
        tilo_user_identity::sign_revocation_entry(&mut revoked, &root).unwrap();
        row.revocations.push(revoked.clone());
        before
            .put(
                &identities,
                &device,
                ScalarValue::Bytes(serde_json::to_vec(&row).unwrap()),
            )
            .unwrap();
        before.commit();
        assert_eq!(parse(&before), Ok(None));
        let mut retained = before.clone();
        let expected = enable_media(&mut retained);
        assert_eq!(
            validate_transition(Some(&before), &retained, TransitionMode::Admission),
            Ok(Some(expected)),
        );
        // A self-consistent first media checkpoint cannot erase a revocation
        // observed while the room was still legacy/disabled, or delay its floor.
        revoked.revoked_at_ns = 20;
        tilo_user_identity::sign_revocation_entry(&mut revoked, &root).unwrap();
        for revocations in [Vec::new(), vec![revoked]] {
            let mut after = before.clone();
            row.revocations = revocations;
            after
                .put(
                    &identities,
                    &device,
                    ScalarValue::Bytes(serde_json::to_vec(&row).unwrap()),
                )
                .unwrap();
            let value = enable_media(&mut after);
            assert_eq!(
                validate_transition(None, &after, TransitionMode::Restore),
                Ok(Some(value))
            );
            assert_eq!(
                validate_transition(Some(&before), &after, TransitionMode::Admission),
                Err(MediaPolicyError::Binding),
            );
        }
    }

    #[test]
    fn canonical_policy_round_trips_and_bound_route_is_exact() {
        let value = policy();
        let mut doc = room();
        doc.put(ROOT, FIELD, ScalarValue::Bytes(encode(&value).unwrap()))
            .unwrap();
        assert_eq!(parse(&doc), Ok(Some(value)));
        for changed in [
            edited(|v| v.calls.get_mut(&[7; 32]).unwrap().binding.app_id = "so.tilo.notes".into()),
            edited(|v| v.room = [2; 16]),
            edited(|v| v.calls.get_mut(&[7; 32]).unwrap().binding.channel_object = "_root".into()),
        ] {
            raw(&mut doc, &changed);
            assert!(parse(&doc).is_err());
        }
    }

    #[test]
    fn schema_version_zero_counters_keys_and_empty_denials_are_rejected() {
        for changed in [
            edited(|v| v.version = 2),
            edited(|v| v.authorization_revision = 0),
            edited(|v| v.room = [0; 16]),
            edited(|v| v.calls.get_mut(&[7; 32]).unwrap().epoch = 0),
            edited(|v| {
                let call = v.calls.remove(&[7; 32]).unwrap();
                v.calls.insert([0; 32], call);
            }),
            edited(|v| {
                v.calls.get_mut(&[7; 32]).unwrap().denied.insert(
                    [8; 32],
                    DirectionsV1 {
                        publish: false,
                        subscribe: false,
                    },
                );
            }),
            edited(|v| {
                v.calls.get_mut(&[7; 32]).unwrap().denied.insert(
                    [0; 32],
                    DirectionsV1 {
                        publish: true,
                        subscribe: false,
                    },
                );
            }),
        ] {
            assert!(encode(&changed).is_err());
            let mut doc = room();
            raw(&mut doc, &changed);
            assert!(parse(&doc).is_err());
        }
    }

    #[test]
    fn malformed_conflicting_and_trailing_values_are_never_legacy_absence() {
        let mut doc = room();
        doc.put(ROOT, FIELD, "bad").unwrap();
        assert!(parse(&doc).is_err());
        doc.put_object(ROOT, FIELD, ObjType::Map).unwrap();
        assert!(parse(&doc).is_err());
        let mut bytes = postcard::to_allocvec(&policy()).unwrap();
        bytes.push(0);
        doc.put(ROOT, FIELD, ScalarValue::Bytes(bytes)).unwrap();
        assert_eq!(parse(&doc), Err(MediaPolicyError::Malformed));
        let mut left = room();
        let mut right = left.fork();
        left.set_actor(ActorId::from(vec![1]));
        right.set_actor(ActorId::from(vec![2]));
        raw(&mut left, &policy());
        raw(&mut right, &edited(|v| v.authorization_revision = 2));
        left.merge_incoming(&mut right).unwrap();
        assert_eq!(parse(&left), Err(MediaPolicyError::Conflict));
    }

    #[test]
    fn identical_governed_values_collapse_without_actor_authority() {
        let mut left = room();
        let mut right = left.fork();
        left.set_actor(ActorId::from(vec![1]));
        right.set_actor(ActorId::from(vec![2]));
        raw(&mut left, &policy());
        raw(&mut right, &policy());
        left.merge_incoming(&mut right).unwrap();
        assert_eq!(parse(&left), Ok(Some(policy())));
    }

    #[test]
    fn unknown_hidden_shapes_and_changed_policy_are_governed() {
        let before = room();
        let mut after = before.clone();
        raw(&mut after, &policy());
        assert_ne!(
            super::super::governance_of(Some(&before)),
            super::super::governance_of(Some(&after))
        );
        assert!(!super::super::governance_change_allowed(
            Some(&before),
            &after,
            false,
            Some("owner"),
            "guest"
        ));
        let mut malformed = before.clone();
        malformed.put_object(ROOT, FIELD, ObjType::Map).unwrap();
        assert_ne!(
            super::super::governance_of(Some(&before)),
            super::super::governance_of(Some(&malformed))
        );
    }

    #[test]
    fn collection_string_and_wire_bounds_reject_before_use() {
        let mut too_many = policy();
        for key in 1u8..=65 {
            let mut call = policy().calls.remove(&[7; 32]).unwrap();
            call.binding.channel_id = format!("voice-{key}");
            call.binding.channel_object = format!("{key}@01");
            too_many.calls.insert([key; 32], call);
        }
        let mut denied = policy();
        for key in 1u8..=129 {
            denied.calls.get_mut(&[7; 32]).unwrap().denied.insert(
                [key; 32],
                DirectionsV1 {
                    publish: true,
                    subscribe: false,
                },
            );
        }
        for value in [
            too_many,
            denied,
            edited(|v| v.calls.get_mut(&[7; 32]).unwrap().binding.record_id = "x".repeat(1025)),
            edited(|v| v.calls.get_mut(&[7; 32]).unwrap().binding.channel_id = "x".repeat(129)),
            edited(|v| v.calls.get_mut(&[7; 32]).unwrap().binding.channel_id = "bad\0key".into()),
        ] {
            assert!(encode(&value).is_err());
            let mut doc = room();
            raw(&mut doc, &value);
            assert!(parse(&doc).is_err());
        }
        let mut doc = room();
        doc.put(ROOT, FIELD, ScalarValue::Bytes(vec![0; 256 * 1024 + 1]))
            .unwrap();
        assert_eq!(parse(&doc), Err(MediaPolicyError::Capacity));
    }

    #[test]
    fn duplicate_bindings_and_noncanonical_map_order_are_rejected() {
        let mut value = policy();
        value.calls.insert([8; 32], value.calls[&[7; 32]].clone());
        assert!(encode(&value).is_err());
        let mut doc = room();
        raw(&mut doc, &value);
        assert!(parse(&doc).is_err());
        // Map ABI is length followed by key/value pairs; descending keys are noncanonical.
        let call = policy().calls[&[7; 32]].clone();
        let other = edited(|v| {
            let binding = &mut v.calls.get_mut(&[7; 32]).unwrap().binding;
            binding.channel_id = "other".into();
            binding.channel_object = "2@01".into();
        })
        .calls[&[7; 32]]
            .clone();
        let mut valid = policy();
        valid.calls.insert([8; 32], other.clone());
        assert!(
            encode(&valid).is_ok(),
            "the descending-map fixture has valid bindings"
        );
        let mut bytes = postcard::to_allocvec(&(1u8, [1u8; 16], 1u64, [3u8; 32], 2u8)).unwrap();
        bytes.extend(postcard::to_allocvec(&([8u8; 32], other)).unwrap());
        bytes.extend(postcard::to_allocvec(&([7u8; 32], call)).unwrap());
        doc.put(ROOT, FIELD, ScalarValue::Bytes(bytes)).unwrap();
        assert_eq!(parse(&doc), Err(MediaPolicyError::Malformed));
    }

    #[test]
    fn new_authoring_starts_at_one_but_authenticated_history_may_start_later() {
        let value = edited(|v| {
            v.authorization_revision = 5;
            v.calls.get_mut(&[7; 32]).unwrap().epoch = 9;
        });
        assert!(validate_policy_transition(None, Some(&value), TransitionMode::Authoring).is_err());
        assert!(validate_policy_transition(None, Some(&value), TransitionMode::Admission).is_ok());
        assert!(validate_policy_transition(None, Some(&value), TransitionMode::Restore).is_ok());
        assert!(
            validate_policy_transition(None, Some(&policy()), TransitionMode::Authoring).is_ok()
        );
    }

    #[test]
    fn basis_and_moderation_cannot_change_at_the_same_counter() {
        for value in [
            edited(|v| v.authorization_basis = [4; 32]),
            edited(|v| v.calls.get_mut(&[7; 32]).unwrap().allowed.publish = false),
            edited(|v| v.calls.get_mut(&[7; 32]).unwrap().phase = CallPhaseV1::Ended),
        ] {
            assert!(
                validate_policy_transition(
                    Some(&policy()),
                    Some(&value),
                    TransitionMode::Admission
                )
                .is_err()
            );
        }
    }

    #[test]
    fn authoring_requires_one_step_and_replay_cannot_erase_or_rebind() {
        let old = policy();
        let changed = edited(|v| {
            v.calls.get_mut(&[7; 32]).unwrap().binding.record_id = "uuid:other".into();
            v.calls.get_mut(&[7; 32]).unwrap().epoch = 2;
        });
        assert!(
            validate_policy_transition(Some(&old), Some(&changed), TransitionMode::Admission)
                .is_err()
        );
        assert!(validate_policy_transition(Some(&old), None, TransitionMode::Admission).is_err());
        let mut erased = old.clone();
        erased.calls.clear();
        assert!(
            validate_policy_transition(Some(&old), Some(&erased), TransitionMode::Admission)
                .is_err()
        );
        let mut changed = old.clone();
        changed.authorization_basis = [4; 32];
        changed.authorization_revision = 3;
        assert!(
            validate_policy_transition(Some(&old), Some(&changed), TransitionMode::Authoring)
                .is_err()
        );
        assert!(
            validate_policy_transition(Some(&old), Some(&changed), TransitionMode::Admission)
                .is_ok()
        );
    }

    #[test]
    fn ended_restart_and_moderation_use_epoch_without_global_revision() {
        let old = policy();
        let mut ended = old.clone();
        ended.calls.get_mut(&[7; 32]).unwrap().phase = CallPhaseV1::Ended;
        ended.calls.get_mut(&[7; 32]).unwrap().epoch = 2;
        assert!(
            validate_policy_transition(Some(&old), Some(&ended), TransitionMode::Authoring).is_ok()
        );
        let mut restart = old.clone();
        restart.calls.get_mut(&[7; 32]).unwrap().epoch = 3;
        assert!(
            validate_policy_transition(Some(&ended), Some(&restart), TransitionMode::Authoring)
                .is_ok()
        );
        assert!(
            validate_policy_transition(Some(&restart), Some(&old), TransitionMode::Admission)
                .is_err()
        );
        assert!(
            validate_policy_transition(Some(&old), Some(&old), TransitionMode::Authoring).is_ok()
        );
    }

    #[test]
    fn counter_exhaustion_is_not_wraparound_or_implicit_reset() {
        assert_eq!(next_counter(u64::MAX), Err(MediaPolicyError::Exhausted));
        assert_eq!(next_counter(0), Err(MediaPolicyError::Counter));
        assert_eq!(next_counter(1), Ok(2));
        let mut old = policy();
        old.authorization_revision = u64::MAX;
        assert!(
            validate_policy_transition(Some(&old), Some(&policy()), TransitionMode::Admission)
                .is_err()
        );
    }

    #[test]
    fn call_id_strings_accept_only_exact_nonzero_lowercase_hex() {
        assert_eq!(parse_call_id(&"07".repeat(32)), Ok([7; 32]));
        for input in [
            "00".repeat(32),
            "AB".repeat(32),
            "07".repeat(31),
            format!("{} ", "07".repeat(32)),
        ] {
            assert!(parse_call_id(&input).is_err());
        }
    }

    #[test]
    fn safe_media_result_has_frozen_v1_postcard_shape() {
        let result = MediaCallResultV1 {
            version: 1,
            call_id: [7; 32],
            binding: policy().calls[&[7; 32]].binding.clone(),
            epoch: 2,
            authorization_revision: 3,
            phase: CallPhaseV1::Ended,
        };
        let mut expected = vec![1];
        expected.extend([7; 32]);
        expected.extend(b"\x0cso.tilo.chat\x00\x05voice\x041@01\x04main\x02\x03\x01");
        assert_eq!(result.encode().unwrap(), expected);
        assert_eq!(MediaCallResultV1::decode(&expected), Ok(result));
        let mut extra = expected;
        extra.push(0);
        assert_eq!(
            MediaCallResultV1::decode(&extra),
            Err(MediaPolicyError::Malformed)
        );
    }

    #[test]
    fn receipt_restore_rejects_semantically_invalid_nested_results() {
        let result = MediaCallResultV1 {
            version: 1,
            call_id: [7; 32],
            binding: policy().calls[&[7; 32]].binding.clone(),
            epoch: 1,
            authorization_revision: 1,
            phase: CallPhaseV1::Active,
        };
        for index in 0..5 {
            let mut invalid = result.clone();
            match index {
                0 => invalid.version = 2,
                1 => invalid.call_id = [0; 32],
                2 => invalid.epoch = 0,
                3 => invalid.authorization_revision = 0,
                _ => invalid.binding.app_id = "so.tilo.notes".into(),
            }
            assert!(invalid.validate().is_err());
            assert!(MediaCallResultV1::decode(&postcard::to_allocvec(&invalid).unwrap()).is_err());
        }
    }

    #[test]
    fn total_denial_bound_and_maximum_definition_inventory_are_enforced() {
        let mut value = policy();
        value.calls.clear();
        for key in 1u8..=64 {
            let mut call = policy().calls.remove(&[7; 32]).unwrap();
            call.binding.channel_id = format!("voice-{key}");
            call.binding.channel_object = format!("{key}@01");
            if key <= 4 {
                for root in 1u8..=128 {
                    call.denied.insert(
                        [root; 32],
                        DirectionsV1 {
                            publish: true,
                            subscribe: false,
                        },
                    );
                }
            }
            value.calls.insert([key; 32], call);
        }
        let mut doc = room();
        raw(&mut doc, &value);
        assert_eq!(parse(&doc), Ok(Some(value.clone())));
        value.calls.get_mut(&[5; 32]).unwrap().denied.insert(
            [8; 32],
            DirectionsV1 {
                publish: true,
                subscribe: false,
            },
        );
        assert_eq!(encode(&value), Err(MediaPolicyError::Capacity));
        raw(&mut doc, &value);
        assert_eq!(parse(&doc), Err(MediaPolicyError::Capacity));
    }

    #[test]
    fn duplicate_wire_keys_cannot_be_normalized_into_a_valid_map() {
        let call = policy().calls[&[7; 32]].clone();
        let mut bytes = postcard::to_allocvec(&(1u8, [1u8; 16], 1u64, [3u8; 32], 2u8)).unwrap();
        for _ in 0..2 {
            bytes.extend(postcard::to_allocvec(&([7u8; 32], &call)).unwrap());
        }
        let mut doc = room();
        doc.put(ROOT, FIELD, ScalarValue::Bytes(bytes)).unwrap();
        assert_eq!(parse(&doc), Err(MediaPolicyError::Malformed));
    }

    fn wire_prefix(calls: usize) -> Vec<u8> {
        postcard::to_allocvec(&(1u8, [1u8; 16], 1u64, [3u8; 32], calls)).unwrap()
    }

    fn parse_wire(bytes: Vec<u8>) -> Result<Option<RoomMediaV1>, MediaPolicyError> {
        let mut doc = room();
        doc.put(ROOT, FIELD, ScalarValue::Bytes(bytes)).unwrap();
        parse(&doc)
    }

    fn append_call_prefix(bytes: &mut Vec<u8>, id: u8, denials: usize) {
        let mut binding = policy().calls[&[7; 32]].binding.clone();
        binding.channel_id = format!("voice-{id}");
        binding.channel_object = format!("{id}@01");
        bytes.extend(
            postcard::to_allocvec(&(
                [id; 32],
                binding,
                1u64,
                CallPhaseV1::Active,
                DirectionsV1 {
                    publish: true,
                    subscribe: true,
                },
                denials,
            ))
            .unwrap(),
        );
    }

    #[test]
    fn excessive_map_counts_return_capacity_before_reading_missing_rows() {
        // No call rows follow: Capacity must be detected at the count, before EOF.
        assert_eq!(parse_wire(wire_prefix(65)), Err(MediaPolicyError::Capacity));
        let mut bytes = wire_prefix(1);
        append_call_prefix(&mut bytes, 1, 129);
        // No denial rows follow, so a decoder that starts them reports Malformed.
        assert_eq!(parse_wire(bytes), Err(MediaPolicyError::Capacity));
    }

    #[test]
    fn shared_denial_budget_stops_before_decoding_the_513th_row() {
        let mut rows = Vec::new();
        for id in 1u8..=4 {
            append_call_prefix(&mut rows, id, 128);
            for root in 1u8..=128 {
                rows.extend(
                    postcard::to_allocvec(&(
                        [root; 32],
                        DirectionsV1 {
                            publish: true,
                            subscribe: false,
                        },
                    ))
                    .unwrap(),
                );
            }
        }
        let mut exact = wire_prefix(4);
        exact.extend(&rows);
        assert!(parse_wire(exact).is_ok(), "512 complete rows are valid");
        let mut over = wire_prefix(5);
        over.extend(rows);
        append_call_prefix(&mut over, 5, 1);
        // The 513th row is absent: a post-allocation global check cannot pass this.
        assert_eq!(parse_wire(over), Err(MediaPolicyError::Capacity));
    }

    #[test]
    fn excessive_wire_string_lengths_are_capacity_before_text_allocation() {
        let fields = [
            ("so.tilo.chat", 64usize),
            ("", 1024),
            ("voice", 128),
            ("1@01", 256),
            ("main", 128),
        ];
        for index in 0..fields.len() {
            let mut prefix = Vec::new();
            for (value, _) in fields.iter().take(index) {
                prefix.extend(postcard::to_allocvec(value).unwrap());
            }
            prefix.extend(postcard::to_allocvec(&(fields[index].1 + 1)).unwrap());
            // The oversized string bytes are deliberately absent.
            let mut policy = wire_prefix(1);
            policy.extend([7; 32]);
            policy.extend(&prefix);
            assert_eq!(parse_wire(policy), Err(MediaPolicyError::Capacity));
            let mut receipt = vec![1];
            receipt.extend([7; 32]);
            receipt.extend(prefix);
            assert_eq!(
                MediaCallResultV1::decode(&receipt),
                Err(MediaPolicyError::Capacity)
            );
        }
    }
}
