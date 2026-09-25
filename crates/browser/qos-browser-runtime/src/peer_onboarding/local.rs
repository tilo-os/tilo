//! Local authority comes from the original verified Complete proof, never a manifest alone.
use super::*;
use qos_peer_state::{enrollment, transport::Retention};
use serde::{
    Deserialize,
    de::{DeserializeSeed, Error, IgnoredAny, MapAccess, SeqAccess, Visitor},
};
use std::{cell::Cell, fmt, ops::Range};

pub(crate) struct BoundLocal {
    material: ChargedBytes,
    context: LocalContext,
}
impl BoundLocal {
    pub(crate) fn material(&self) -> &[u8] {
        &self.material.bytes
    }
    pub(crate) fn into_context(self) -> LocalContext {
        self.context
    }
}
pub(crate) struct LocalContext {
    material_digest: [u8; 32],
    raw_digest: [u8; 32],
    request_digest: [u8; 32],
    nonce: [u8; 32],
    root: [u8; 32],
    device: [u8; 32],
    room: uuid::Uuid,
    workspace: u64,
    origin: String,
    host: [u8; 32],
    expires: u64,
    consumed: Cell<bool>,
    cancel: OnboardingCancel,
    budget: IncomingBudget,
    _metadata: NativeReservation,
}
impl LocalContext {
    pub(crate) fn expires_at_ns(&self) -> u64 {
        self.expires
    }
    pub(crate) fn budget(&self) -> &IncomingBudget {
        &self.budget
    }
    pub(crate) fn cancel(&self) -> &OnboardingCancel {
        &self.cancel
    }
    fn claim(&self, material: &[u8], raw: &str, now: u64) -> Result<(), String> {
        self.check_continuation(now)?;
        if self.consumed.get()
            || canonical_item_digest(material) != self.material_digest
            || canonical_item_digest(raw.as_bytes()) != self.raw_digest
        {
            return Err("local onboarding context differs or expired".into());
        }
        self.consumed.set(true);
        Ok(())
    }
    pub(crate) fn inline_governance(
        &self,
        material: &[u8],
        raw: &str,
        now: u64,
    ) -> Result<GovernanceReply, String> {
        self.claim(material, raw, now)?;
        let extracted = extract(raw, &self.budget)?;
        if extracted.manifest {
            return Err("local governance requires HTTPS handoff".into());
        }
        Ok(extracted.reply)
    }
    pub(super) fn check_continuation(&self, now: u64) -> Result<(), String> {
        self.cancel.check()?;
        if now >= self.expires {
            return Err("original local onboarding context expired".into());
        }
        Ok(())
    }
    pub(super) fn manifest_context(&self) -> handoff::ManifestContext<'_> {
        handoff::ManifestContext {
            scope: self.scope(),
            purpose: wire::HandoffPurpose::LocalGovernance,
            device: self.device,
            request_digest: self.request_digest,
            request_nonce: self.nonce,
            expires_at_ns: self.expires,
        }
    }
    fn scope(&self) -> wire::OnboardingScope<'_> {
        wire::OnboardingScope {
            origin: &self.origin,
            host: self.host,
            workspace: self.workspace,
            root: self.root,
            room: self.room,
        }
    }
}
pub(crate) struct GovernanceReply {
    bytes: ChargedBytes,
    ranges: Vec<Range<usize>>,
    kinds: Vec<Retention>,
    generation: u64,
    _metadata: NativeReservation,
}
impl GovernanceReply {
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }
    pub(crate) fn messages(&self) -> impl Iterator<Item = (&[u8], Retention)> {
        self.ranges
            .iter()
            .zip(&self.kinds)
            .map(|(range, kind)| (&self.bytes.bytes[range.clone()], *kind))
    }
}
#[derive(Deserialize)]
struct Metadata {
    version: u32,
    root_public_key: [u8; 32],
    certificate: tilo_user_identity::DeviceCert,
    host_certificate: tilo_user_identity::DeviceCert,
    bootstrap: Room,
}
#[derive(Deserialize)]
struct Room {
    room: String,
    workspace_id: String,
    delivery_hint: qos_peer_state::join::TransportHint,
}
pub(crate) fn bind_local(
    material: &[u8],
    challenge: &str,
    raw: &str,
    budget: IncomingBudget,
    now: u64,
) -> Result<BoundLocal, String> {
    if material.len() > 4096 || challenge.len() > 8192 || raw.len() > 1024 * 1024 || now == 0 {
        return Err("local onboarding input exceeds bound".into());
    }
    // Only the fixed metadata schemas own strings: their total decoded bytes
    // per pass cannot exceed the input. Keep this projection plus identity::bind's
    // second projection covered while legacy certificate hashing builds its
    // Postcard Vec (including old/new growth overlap). Six input capacities
    // cover those phases; bounded proof/material scratch uses 64 KiB. This
    // does not qualify the later legacy StatePeer admission allocator.
    let metadata = budget.reserve(
        raw.len()
            .checked_mul(6)
            .and_then(|n| n.checked_add(64 * 1024))
            .ok_or("local charge overflow")?,
    )?;
    let info: Metadata =
        serde_json::from_str(raw).map_err(|_| "invalid local onboarding metadata")?;
    let workspace = decimal(&info.bootstrap.workspace_id)?;
    let room: uuid::Uuid = info
        .bootstrap
        .room
        .parse()
        .map_err(|_| "invalid local room")?;
    if info.version != 2 {
        return Err("unsupported local enrollment".into());
    }
    let proof: enrollment::CompleteRequest =
        serde_json::from_str(&crate::identity::complete(material, challenge)?)
            .map_err(|_| "invalid original completion")?;
    let verified = enrollment::verify(
        &proof,
        info.root_public_key,
        &info.bootstrap.room,
        workspace,
        now,
    )?;
    if info.certificate.device_public_key != verified.device_public_key
        || info.certificate.user_public_key != verified.root_public_key
        || info.host_certificate.user_public_key != verified.root_public_key
        || !info.host_certificate.capabilities.can_act_as_host
    {
        return Err("local enrollment certificate scope mismatch".into());
    }
    tilo_user_identity::verify_device_cert_at(&info.host_certificate, now)
        .map_err(|_| "invalid local delivery host")?;
    // This is an inert projection of the independently pinned signed native
    // descriptor. Current host membership/admission remains the parent's later
    // ordinary governance + verify_transport_hint check.
    let pin = tilo_relay::delivery_v3::descriptor::inspect_pinned_hint_scope(
        &info.bootstrap.delivery_hint,
        &info.host_certificate,
    )
    .map_err(|_| "invalid local delivery descriptor")?;
    if pin.workspace != workspace
        || pin.workspace_root != verified.root_public_key
        || now < pin.metadata.issued_at_ns
        || now >= pin.metadata.expires_at_ns
    {
        return Err("local delivery descriptor scope or lifetime mismatch".into());
    }
    let original = encode(&proof, 8192, &budget)?;
    let bound = crate::identity::bind(material, challenge, raw)?;
    let held = ChargedBytes::copy(&bound, &budget)?;
    let context = LocalContext {
        material_digest: canonical_item_digest(&held.bytes),
        raw_digest: canonical_item_digest(raw.as_bytes()),
        request_digest: canonical_item_digest(&original.bytes),
        nonce: verified.nonce,
        root: verified.root_public_key,
        device: verified.device_public_key,
        room,
        workspace,
        origin: pin.origin,
        host: info.host_certificate.device_public_key,
        expires: verified.expires_at_ns.min(pin.metadata.expires_at_ns),
        consumed: Cell::new(false),
        cancel: OnboardingCancel::new(),
        budget,
        _metadata: metadata,
    };
    Ok(BoundLocal {
        material: held,
        context,
    })
}
fn decimal(value: &str) -> Result<u64, String> {
    if value.is_empty()
        || value.len() > 20
        || value.starts_with('0')
        || !value.bytes().all(|b| b.is_ascii_digit())
    {
        return Err("invalid canonical generation/workspace".into());
    }
    value
        .parse()
        .map_err(|_| "generation/workspace overflow".into())
}

// Two passes use only inline parser state before reserving exact backing. Unknown
// fields are skipped with IgnoredAny; numeric arrays never become JSON Values.
#[derive(Default)]
struct Shape {
    total: usize,
    count: usize,
    kinds: usize,
    generation: Option<u64>,
    messages: bool,
    retention: bool,
    manifest: bool,
}
struct Extracted {
    reply: GovernanceReply,
    manifest: bool,
}
fn extract(raw: &str, budget: &IncomingBudget) -> Result<Extracted, String> {
    let mut shape = Shape::default();
    scan(raw, &mut shape, None)?;
    if shape.generation.is_none()
        || shape.total == 0
        || (!shape.manifest
            && (!shape.messages
                || !shape.retention
                || shape.count != shape.kinds
                || shape.count == 0))
        || (shape.manifest && (shape.messages || shape.retention))
    {
        return Err("missing or ambiguous local governance fields".into());
    }
    let metadata = budget.reserve(
        shape
            .count
            .checked_mul(std::mem::size_of::<Range<usize>>() + std::mem::size_of::<Retention>())
            .ok_or("governance metadata overflow")?,
    )?;
    let mut reply = GovernanceReply {
        bytes: ChargedBytes::zeroed(shape.total, budget)?,
        ranges: Vec::new(),
        kinds: Vec::new(),
        generation: shape.generation.unwrap(),
        _metadata: metadata,
    };
    reply
        .ranges
        .try_reserve_exact(shape.count)
        .map_err(|_| "governance allocation unavailable")?;
    reply
        .kinds
        .try_reserve_exact(shape.count)
        .map_err(|_| "governance allocation unavailable")?;
    if reply.ranges.capacity() > shape.count || reply.kinds.capacity() > shape.count {
        return Err("governance metadata exceeds credit".into());
    }
    let mut second = Shape::default();
    scan(raw, &mut second, Some(&mut reply))?;
    Ok(Extracted {
        reply,
        manifest: shape.manifest,
    })
}
fn scan(raw: &str, shape: &mut Shape, output: Option<&mut GovernanceReply>) -> Result<(), String> {
    let mut de = serde_json::Deserializer::from_str(raw);
    Fields {
        shape,
        output,
        nested: false,
    }
    .deserialize(&mut de)
    .map_err(|_| "invalid bounded local governance")?;
    de.end().map_err(|_| "trailing local governance".into())
}
struct Fields<'a> {
    shape: &'a mut Shape,
    output: Option<&'a mut GovernanceReply>,
    nested: bool,
}
impl<'de> DeserializeSeed<'de> for Fields<'_> {
    type Value = ();
    fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_map(self)
    }
}
impl<'de> Visitor<'de> for Fields<'_> {
    type Value = ();
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("local bootstrap fields")
    }
    fn visit_map<A: MapAccess<'de>>(mut self, mut map: A) -> Result<(), A::Error> {
        let mut seen_bootstrap = false;
        while let Some(key) = map.next_key::<&str>()? {
            match (self.nested, key) {
                (false, "bootstrap") => {
                    if seen_bootstrap {
                        return Err(A::Error::custom("duplicate bootstrap"));
                    }
                    seen_bootstrap = true;
                    map.next_value_seed(Fields {
                        shape: self.shape,
                        output: self.output.as_deref_mut(),
                        nested: true,
                    })?;
                }
                (true, "governance_generation") => {
                    if self.shape.generation.is_some() {
                        return Err(A::Error::custom("duplicate generation"));
                    }
                    let value = map.next_value::<&str>()?;
                    self.shape.generation = Some(decimal(value).map_err(A::Error::custom)?);
                }
                (true, "governance_messages") => {
                    if self.shape.messages {
                        return Err(A::Error::custom("duplicate messages"));
                    }
                    self.shape.messages = true;
                    map.next_value_seed(Messages {
                        shape: self.shape,
                        output: self.output.as_deref_mut(),
                    })?;
                }
                (true, "governance_retention") => {
                    if self.shape.retention {
                        return Err(A::Error::custom("duplicate retention"));
                    }
                    self.shape.retention = true;
                    map.next_value_seed(Kinds {
                        shape: self.shape,
                        output: self.output.as_deref_mut(),
                    })?;
                }
                (true, "governance_handoff") => {
                    if self.shape.manifest {
                        return Err(A::Error::custom("duplicate manifest"));
                    }
                    self.shape.manifest = true;
                    map.next_value_seed(Bytes {
                        shape: self.shape,
                        output: self.output.as_deref_mut(),
                        maximum: wire::MAX_MANIFEST_BYTES,
                    })?;
                }
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        if !self.nested && !seen_bootstrap {
            return Err(A::Error::custom("missing bootstrap"));
        }
        Ok(())
    }
}
struct Messages<'a> {
    shape: &'a mut Shape,
    output: Option<&'a mut GovernanceReply>,
}
impl<'de> DeserializeSeed<'de> for Messages<'_> {
    type Value = ();
    fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_seq(self)
    }
}
impl<'de> Visitor<'de> for Messages<'_> {
    type Value = ();
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("bounded message arrays")
    }
    fn visit_seq<A: SeqAccess<'de>>(mut self, mut seq: A) -> Result<(), A::Error> {
        loop {
            let start = self.shape.total;
            if seq
                .next_element_seed(Bytes {
                    shape: self.shape,
                    output: self.output.as_deref_mut(),
                    maximum: wire::MAX_GOVERNANCE_BYTES,
                })?
                .is_none()
            {
                break;
            }
            if self.shape.count == wire::MAX_GOVERNANCE_ITEMS || self.shape.total == start {
                return Err(A::Error::custom("message count/length"));
            }
            self.shape.count += 1;
            if let Some(out) = self.output.as_deref_mut() {
                out.ranges.push(start..self.shape.total);
            }
        }
        Ok(())
    }
}
struct Bytes<'a> {
    shape: &'a mut Shape,
    output: Option<&'a mut GovernanceReply>,
    maximum: usize,
}
impl<'de> DeserializeSeed<'de> for Bytes<'_> {
    type Value = ();
    fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_seq(self)
    }
}
impl<'de> Visitor<'de> for Bytes<'_> {
    type Value = ();
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("bounded bytes")
    }
    fn visit_seq<A: SeqAccess<'de>>(mut self, mut seq: A) -> Result<(), A::Error> {
        while let Some(byte) = seq.next_element::<u8>()? {
            if self.shape.total >= self.maximum {
                return Err(A::Error::custom("byte bound"));
            }
            if let Some(out) = self.output.as_deref_mut() {
                *out.bytes
                    .bytes
                    .get_mut(self.shape.total)
                    .ok_or_else(|| A::Error::custom("changed byte count"))? = byte;
            }
            self.shape.total += 1;
        }
        Ok(())
    }
}
struct Kinds<'a> {
    shape: &'a mut Shape,
    output: Option<&'a mut GovernanceReply>,
}
impl<'de> DeserializeSeed<'de> for Kinds<'_> {
    type Value = ();
    fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_seq(self)
    }
}
impl<'de> Visitor<'de> for Kinds<'_> {
    type Value = ();
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("bounded retention kinds")
    }
    fn visit_seq<A: SeqAccess<'de>>(mut self, mut seq: A) -> Result<(), A::Error> {
        while let Some(kind) = seq.next_element::<Retention>()? {
            if self.shape.kinds == wire::MAX_GOVERNANCE_ITEMS {
                return Err(A::Error::custom("retention count"));
            }
            self.shape.kinds += 1;
            if let Some(out) = self.output.as_deref_mut() {
                out.kinds.push(kind);
            }
        }
        Ok(())
    }
}

impl GovernanceReply {
    pub(super) fn bundle(
        bytes: ChargedBytes,
        generation: u64,
        budget: &IncomingBudget,
    ) -> Result<Self, String> {
        if generation == 0 {
            return Err("invalid governance generation".into());
        }
        let _decode = budget.reserve(
            3 * wire::MAX_GOVERNANCE_ITEMS * std::mem::size_of::<wire::GovernanceItem<'_>>(),
        )?;
        let decoded = wire::GovernanceBundle::decode(&bytes.bytes)
            .map_err(|_| "invalid canonical governance bundle")?;
        let count = decoded.items.len();
        let metadata = budget.reserve(
            count * (std::mem::size_of::<Range<usize>>() + std::mem::size_of::<Retention>()),
        )?;
        let mut ranges = Vec::new();
        let mut kinds = Vec::new();
        ranges
            .try_reserve_exact(count)
            .map_err(|_| "governance metadata unavailable")?;
        kinds
            .try_reserve_exact(count)
            .map_err(|_| "governance metadata unavailable")?;
        if ranges.capacity() > count || kinds.capacity() > count {
            return Err("governance metadata exceeds credit".into());
        }
        for item in &decoded.items {
            let start = (item.bytes.as_ptr() as usize)
                .checked_sub(bytes.bytes.as_ptr() as usize)
                .ok_or("governance backing mismatch")?;
            let end = start
                .checked_add(item.bytes.len())
                .filter(|end| *end <= bytes.bytes.len())
                .ok_or("governance range mismatch")?;
            ranges.push(start..end);
            kinds.push(item.retention);
        }
        drop(decoded);
        Ok(Self {
            bytes,
            ranges,
            kinds,
            generation,
            _metadata: metadata,
        })
    }
}

#[cfg(target_arch = "wasm32")]
pub(crate) async fn local_governance(
    context: &LocalContext,
    material: &[u8],
    raw: &str,
    current: impl Fn() -> Result<(), String>,
) -> Result<GovernanceReply, String> {
    let live = || {
        current()?;
        context.check_continuation(tilo_relay::clock::now_ns())
    };
    live()?;
    context.claim(material, raw, tilo_relay::clock::now_ns())?;
    let extracted = extract(raw, &context.budget)?;
    if !extracted.manifest {
        live()?;
        return Ok(extracted.reply);
    }
    let purpose = context.manifest_context();
    let _decode = context
        .budget
        .reserve(3 * wire::MAX_SEGMENTS * std::mem::size_of::<wire::HandoffSegment>())?;
    let manifest = wire::HandoffManifest::decode(&extracted.reply.bytes.bytes)
        .map_err(|_| "invalid local manifest")?;
    if manifest.owner_generation != extracted.reply.generation {
        return Err("local governance generation mismatch".into());
    }
    drop(manifest);
    let transfer = handoff::Handoff::start(
        &extracted.reply.bytes.bytes,
        purpose,
        &context.budget,
        tilo_relay::clock::now_ns(),
    )?;
    let deadline = transfer.expires_at_ns();
    let transfer_live = || {
        live()?;
        if tilo_relay::clock::now_ns() >= deadline {
            return Err("local governance handoff expired".into());
        }
        Ok(())
    };
    let key = crate::identity::key(material, raw)?;
    let bytes = wasm::transfer_all(
        transfer,
        &context.origin,
        &key,
        &context.budget,
        &context.cancel,
        &transfer_live,
    )
    .await?;
    transfer_live()?;
    let reply = GovernanceReply::bundle(bytes, extracted.reply.generation, &context.budget)?;
    transfer_live()?;
    Ok(reply)
}

#[cfg(test)]
mod validation_tests {
    use super::*;
    #[test]
    fn inline_fields_reject_duplicates_unpaired_kinds_and_noncanonical_generation() {
        let budget = IncomingBudget::new(1024 * 1024).unwrap();
        let before = budget.available();
        for fields in [
            r#""governance_messages":[[1]],"governance_messages":[[1]],"governance_retention":["Checkpoint"],"governance_generation":"1""#,
            r#""governance_messages":[[1]],"governance_retention":[],"governance_generation":"1""#,
            r#""governance_messages":[[1]],"governance_retention":["Checkpoint"],"governance_generation":"01""#,
            r#""governance_messages":[[256]],"governance_retention":["Checkpoint"],"governance_generation":"1""#,
            r#""governance_handoff":[1],"governance_messages":[[1]],"governance_retention":["Checkpoint"],"governance_generation":"1""#,
        ] {
            assert!(extract(&format!("{{\"bootstrap\":{{{fields}}}}}"), &budget).is_err());
            assert_eq!(budget.available(), before);
        }
        let cancel = OnboardingCancel::new();
        cancel.cancel();
        assert!(cancel.check().is_err());
    }
}
