use super::*;
use crate::canonical_records::creation::CreationLedgerV1;
use qos_peer_state::join::TransportHint;
use tilo_relay::delivery_v3::commit::{
    DeliveryLedgerV1, MAX_REFERENCES, PublicationPool, PublicationRef,
};

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct LegacyAggregate {
    pub(super) version: u32,
    pub(super) identity_root: PathBuf,
    pub(super) host_identity_root: PathBuf,
    pub(super) room: String,
    pub(super) authority: DeviceCert,
    pub(super) directory: Vec<u8>,
    /// Versioned canonical StatePeer export owns roster, revocations and replay floors.
    pub(super) peer: Vec<u8>,
    pub(super) outgoing: Vec<Vec<u8>>,
    pub(super) pending: Vec<PendingOperation>,
    pub(super) generation: u64,
}
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct AggregateV2 {
    pub(super) version: u32,
    pub(super) identity_root: PathBuf,
    pub(super) host_identity_root: PathBuf,
    pub(super) room: String,
    pub(super) authority: DeviceCert,
    pub(super) directory: Vec<u8>,
    /// Versioned canonical StatePeer export owns roster, revocations and replay floors.
    pub(super) peer: Vec<u8>,
    pub(super) outgoing: Vec<Vec<u8>>,
    pub(super) pending: Vec<PendingOperation>,
    pub(super) generation: u64,
    pub(super) issuances: Vec<crate::enrollment::Issuance>,
}
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct AggregateV3 {
    pub(super) version: u32,
    pub(super) identity_root: PathBuf,
    pub(super) host_identity_root: PathBuf,
    pub(super) room: String,
    pub(super) authority: DeviceCert,
    pub(super) directory: Vec<u8>,
    /// Versioned canonical StatePeer export owns roster, revocations and replay floors.
    pub(super) peer: Vec<u8>,
    pub(super) outgoing: Vec<Vec<u8>>,
    pub(super) pending: Vec<PendingOperation>,
    pub(super) generation: u64,
    pub(super) issuances: Vec<crate::enrollment::Issuance>,
    pub(super) publication: PublicationState,
}
impl From<LegacyAggregate> for AggregateV2 {
    fn from(old: LegacyAggregate) -> Self {
        Self {
            version: 2,
            identity_root: old.identity_root,
            host_identity_root: old.host_identity_root,
            room: old.room,
            authority: old.authority,
            directory: old.directory,
            peer: old.peer,
            outgoing: old.outgoing,
            pending: old.pending,
            generation: old.generation,
            issuances: vec![],
        }
    }
}
/// Bounds redundant publication tail, not canonical Automerge history or baseline size.
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct PublicationState {
    pub(super) kinds: Vec<Retention>,
    pub(super) baseline_len: usize,
    pub(super) baseline_generation: u64,
}
impl PublicationState {
    pub(super) fn checkpoint(len: usize, generation: u64) -> Self {
        Self {
            kinds: vec![Retention::Checkpoint; len],
            baseline_len: len,
            baseline_generation: generation,
        }
    }
}
impl From<AggregateV2> for AggregateV3 {
    fn from(old: AggregateV2) -> Self {
        let publication = PublicationState::checkpoint(old.outgoing.len(), old.generation);
        Self {
            version: 3,
            identity_root: old.identity_root,
            host_identity_root: old.host_identity_root,
            room: old.room,
            authority: old.authority,
            directory: old.directory,
            peer: old.peer,
            outgoing: old.outgoing,
            pending: old.pending,
            generation: old.generation,
            issuances: old.issuances,
            publication,
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct AggregateV4 {
    pub(super) version: u32,
    pub(super) identity_root: PathBuf,
    pub(super) host_identity_root: PathBuf,
    pub(super) room: String,
    pub(super) authority: DeviceCert,
    pub(super) directory: Vec<u8>,
    pub(super) peer: Vec<u8>,
    pub(super) outgoing: Vec<Vec<u8>>,
    pub(super) pending: Vec<PendingOperation>,
    pub(super) generation: u64,
    pub(super) issuances: Vec<crate::enrollment::Issuance>,
    pub(super) publication: PublicationState,
    pub(super) collaboration: crate::collaboration::CollaborationStateV4,
}
impl From<AggregateV3> for AggregateV4 {
    fn from(old: AggregateV3) -> Self {
        Self {
            version: 4,
            identity_root: old.identity_root,
            host_identity_root: old.host_identity_root,
            room: old.room,
            authority: old.authority,
            directory: old.directory,
            peer: old.peer,
            outgoing: old.outgoing,
            pending: old.pending,
            generation: old.generation,
            issuances: old.issuances,
            publication: old.publication,
            collaboration: crate::collaboration::CollaborationStateV4::default(),
        }
    }
}

#[derive(Clone)]
pub(super) struct Aggregate {
    pub(super) sender: Option<SenderSelection>,
    pub(super) identity_root: PathBuf,
    pub(super) host_identity_root: PathBuf,
    pub(super) room: String,
    pub(super) authority: DeviceCert,
    pub(super) directory: Vec<u8>,
    pub(super) peer: Vec<u8>,
    pub(super) pending: Vec<PendingOperation>,
    pub(super) generation: u64,
    pub(super) issuances: Vec<crate::enrollment::Issuance>,
    pub(super) collaboration: crate::collaboration::CollaborationState,
    pub(super) publication: PublicationStorage,
    pub(super) creation: Option<CreationLedgerV1>,
}
pub(super) enum PublicationStorage {
    Legacy {
        outgoing: Vec<Vec<u8>>,
        state: PublicationState,
    },
    V5 {
        pool: PublicationPool,
        roots: PublicationRootsV1,
        delivery: DeliveryLedgerV1,
        transport_hint: Option<TransportHint>,
    },
}
impl Clone for PublicationStorage {
    fn clone(&self) -> Self {
        match self {
            Self::Legacy { outgoing, state } => Self::Legacy {
                outgoing: outgoing.clone(),
                state: state.clone(),
            },
            Self::V5 {
                pool,
                roots,
                delivery,
                transport_hint,
            } => Self::V5 {
                pool: pool.fork(),
                roots: roots.clone(),
                delivery: delivery.clone(),
                transport_hint: transport_hint.clone(),
            },
        }
    }
}
impl From<AggregateV4> for Aggregate {
    fn from(old: AggregateV4) -> Self {
        Self {
            sender: None,
            identity_root: old.identity_root,
            host_identity_root: old.host_identity_root,
            room: old.room,
            authority: old.authority,
            directory: old.directory,
            peer: old.peer,
            pending: old.pending,
            generation: old.generation,
            issuances: old.issuances,
            collaboration: old.collaboration.into(),
            creation: None,
            publication: PublicationStorage::Legacy {
                outgoing: old.outgoing,
                state: old.publication,
            },
        }
    }
}
/// Fixed, signed administrative metadata. No owner or sender capability.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct SenderSelection(pub(super) [u8; 187]);

#[derive(Serialize, Deserialize)]
pub(super) struct AggregateV7<'a> {
    pub(super) version: u32,
    #[serde(borrow)]
    pub(super) base: &'a [u8],
    #[serde(borrow)]
    pub(super) sender: &'a [u8],
}
impl SenderSelection {
    pub(super) fn decode(bytes: &[u8]) -> Result<Self> {
        let bytes: [u8; 187] = bytes
            .try_into()
            .map_err(|_| anyhow::anyhow!("invalid sender selection length"))?;
        if bytes[0] != 1
            || bytes[1] > 1
            || bytes[2] != 1
            || bytes[3..19] == [0; 16]
            || bytes[19..51] == [0; 32]
            || bytes[51..83] == [0; 32]
            || bytes[83..99] == [0; 16]
            || bytes[99..107] != 1_u64.to_le_bytes()
            || bytes[107..115] != 0_u64.to_le_bytes()
            || bytes[115..147] != *blake3::hash(b"NSO1\x01\x00\x00\x00").as_bytes()
        {
            bail!("invalid sender selection invariant");
        }
        Ok(Self(bytes))
    }
    pub(super) fn validate_scope(&self, aggregate: &Aggregate) -> Result<()> {
        let room = uuid::Uuid::parse_str(&aggregate.room)?;
        if self.0[3..19] != *room.as_bytes()
            || self.0[19..51] != aggregate.authority.user_public_key
            || self.0[51..83] != aggregate.authority.device_public_key
            || self.0[147..155] != aggregate.generation.to_le_bytes()
        {
            bail!("sender selection differs from authenticated native scope");
        }
        Ok(())
    }
}
/// Borrow the one canonical base; nesting and historical automatic migrations
/// are deliberately unavailable to the administrative path.
pub(super) fn administrative_base(payload: &[u8]) -> Result<&[u8]> {
    let (version, _) = postcard::take_from_bytes::<u32>(payload)?;
    match version {
        4..=6 => Ok(payload),
        7 => {
            let wire: AggregateV7<'_> = exact(payload)?;
            let (inner, _) = postcard::take_from_bytes::<u32>(wire.base)?;
            if !(4..=6).contains(&inner) {
                bail!("selected base must be exactly V4, V5 or V6");
            }
            SenderSelection::decode(wire.sender)?;
            Ok(wire.base)
        }
        _ => bail!("administrative selection requires canonical V4, V5 or V6"),
    }
}
#[derive(Clone, Serialize)]
pub(super) struct PublicationRootsV1 {
    pub(super) baseline_generation: u64,
    pub(super) baseline: Vec<PublicationRef>,
    pub(super) tail: Vec<PublicationRef>,
}
impl<'de> Deserialize<'de> for PublicationRootsV1 {
    fn deserialize<D: serde::Deserializer<'de>>(decoder: D) -> Result<Self, D::Error> {
        struct Roots;
        impl<'de> serde::de::Visitor<'de> for Roots {
            type Value = PublicationRootsV1;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("bounded publication roots")
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut input: A,
            ) -> Result<Self::Value, A::Error> {
                let missing = || serde::de::Error::custom("missing publication roots");
                let baseline_generation = input.next_element()?.ok_or_else(missing)?;
                let baseline = input
                    .next_element_seed(RootsSeed(MAX_REFERENCES))?
                    .ok_or_else(missing)?;
                let tail = input
                    .next_element_seed(RootsSeed(MAX_REFERENCES - baseline.len()))?
                    .ok_or_else(missing)?;
                Ok(PublicationRootsV1 {
                    baseline_generation,
                    baseline,
                    tail,
                })
            }
        }
        decoder.deserialize_struct(
            "PublicationRootsV1",
            &["baseline_generation", "baseline", "tail"],
            Roots,
        )
    }
}
struct RootsSeed(usize);
impl<'de> serde::de::DeserializeSeed<'de> for RootsSeed {
    type Value = Vec<PublicationRef>;
    fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        d.deserialize_seq(self)
    }
}

impl<'de> serde::de::Visitor<'de> for RootsSeed {
    type Value = Vec<PublicationRef>;
    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("bounded publication references")
    }
    fn visit_seq<A: serde::de::SeqAccess<'de>>(
        self,
        mut input: A,
    ) -> Result<Self::Value, A::Error> {
        // Postcard hides the declared count when remaining input is shorter.
        // That cannot be a valid fixed-size reference vector; reject before reads.
        let count = input.size_hint().filter(|n| *n <= self.0).ok_or_else(|| {
            serde::de::Error::custom("publication reference pressure or truncation")
        })?;
        let mut refs = Vec::new();
        while let Some(r) = input.next_element()? {
            if refs.len() >= count {
                return Err(serde::de::Error::custom("publication reference pressure"));
            }
            refs.push(r);
        }
        Ok(refs)
    }
}
// Exact outer V5 order; blobs borrow the original signed input before bounded codec allocation.
#[derive(Serialize, Deserialize)]
struct AggregateV5<'a> {
    version: u32,
    identity_root: PathBuf,
    host_identity_root: PathBuf,
    room: String,
    authority: DeviceCert,
    directory: Vec<u8>,
    peer: Vec<u8>,
    #[serde(borrow)]
    publications: &'a [u8],
    pending: Vec<PendingOperation>,
    generation: u64,
    issuances: Vec<crate::enrollment::Issuance>,
    publication: PublicationRootsV1,
    collaboration: crate::collaboration::CollaborationState,
    #[serde(borrow)]
    delivery: &'a [u8],
    #[serde(borrow)]
    transport_hint: Option<&'a [u8]>,
}
// The V6 wire appends one mandatory creation blob to the exact V5 field order.
// Postcard structs carry no wrapper marker; nesting avoids duplicating that DTO.
#[derive(Serialize, Deserialize)]
struct AggregateV6<'a> {
    #[serde(borrow)]
    base: AggregateV5<'a>,
    #[serde(borrow)]
    creation: &'a [u8],
}
// No large common-field clones on encode. Field order equals the frozen wire DTOs.
#[derive(Serialize)]
struct V4View<'a> {
    version: u32,
    identity_root: &'a PathBuf,
    host_identity_root: &'a PathBuf,
    room: &'a str,
    authority: &'a DeviceCert,
    directory: &'a [u8],
    peer: &'a [u8],
    outgoing: &'a [Vec<u8>],
    pending: &'a [PendingOperation],
    generation: u64,
    issuances: &'a [crate::enrollment::Issuance],
    publication: &'a PublicationState,
    collaboration: crate::collaboration::CollaborationV4View<'a>,
}
#[derive(Serialize)]
struct V5View<'a> {
    version: u32,
    identity_root: &'a PathBuf,
    host_identity_root: &'a PathBuf,
    room: &'a str,
    authority: &'a DeviceCert,
    directory: &'a [u8],
    peer: &'a [u8],
    publications: &'a [u8],
    pending: &'a [PendingOperation],
    generation: u64,
    issuances: &'a [crate::enrollment::Issuance],
    publication: &'a PublicationRootsV1,
    collaboration: &'a crate::collaboration::CollaborationState,
    delivery: &'a [u8],
    transport_hint: Option<&'a [u8]>,
}
#[derive(Serialize)]
struct V6View<'a> {
    base: V5View<'a>,
    creation: &'a [u8],
}
pub(super) const V5_READ_ONLY_REASON: &str =
    "SessionOuterV5 is read-only until native replay commit integration is activated";

impl Aggregate {
    pub(super) fn is_read_only(&self) -> bool {
        matches!(self.publication, PublicationStorage::V5 { .. }) && self.peer.first() != Some(&5)
    }
    pub(super) fn require_writable(&self) -> Result<()> {
        if self.is_read_only() {
            bail!(V5_READ_ONLY_REASON);
        }
        Ok(())
    }
    pub(super) fn legacy_mut(&mut self) -> Result<(&mut Vec<Vec<u8>>, &mut PublicationState)> {
        let PublicationStorage::Legacy { outgoing, state } = &mut self.publication else {
            bail!("legacy publication mutation requires V4 storage")
        };
        Ok((outgoing, state))
    }
    pub(super) fn baseline(&self) -> (u64, usize) {
        match &self.publication {
            PublicationStorage::Legacy { state, .. } => {
                (state.baseline_generation, state.baseline_len)
            }
            PublicationStorage::V5 { roots, .. } => {
                (roots.baseline_generation, roots.baseline.len())
            }
        }
    }
    pub(super) fn outgoing(&self) -> OutgoingPublications<'_> {
        OutgoingPublications {
            storage: &self.publication,
        }
    }
    pub(super) fn validate(&self) -> Result<()> {
        if let Some(creation) = &self.creation {
            if !matches!(self.publication, PublicationStorage::V5 { .. })
                || self.peer.first() != Some(&5)
            {
                bail!("creation V6 requires selected native V5 delivery");
            }
            creation.validate().map_err(anyhow::Error::msg)?;
            let room = uuid::Uuid::parse_str(&self.room)?;
            if creation.rows().any(|row| {
                row.intent.key.room != *room.as_bytes()
                    || row.result.body.owner_generation > self.generation
            }) {
                bail!("creation ledger differs from owner room or generation");
            }
        }
        let outgoing = self.outgoing();
        let (generation, baseline_len) = self.baseline();
        if baseline_len > outgoing.len()
            || generation == 0
            || generation > self.generation
            || (!outgoing.is_empty() && baseline_len == 0)
        {
            bail!("invalid durable publication baseline");
        }
        match &self.publication {
            PublicationStorage::Legacy { outgoing, state } => {
                if outgoing.len() != state.kinds.len() {
                    bail!("invalid durable publication kinds");
                }
            }
            PublicationStorage::V5 {
                pool,
                roots,
                delivery,
                transport_hint,
            } => {
                if roots
                    .baseline
                    .len()
                    .checked_add(roots.tail.len())
                    .is_none_or(|n| n > MAX_REFERENCES)
                {
                    bail!("publication reference pressure");
                }
                if delivery.resolved_through != self.generation {
                    bail!("delivery generation differs from native aggregate");
                }
                let refs = roots
                    .baseline
                    .iter()
                    .chain(&roots.tail)
                    .cloned()
                    .collect::<Vec<_>>();
                pool.reference_counts(&refs, delivery)?;
                // Validate retained replay/pin message kinds as well as ordinary roots.
                for r in delivery
                    .replay
                    .values()
                    .flat_map(|r| &r.publications)
                    .chain(delivery.pins.values().flat_map(|p| &p.publications))
                {
                    validate_message(pool.resolve(&r.payload)?, r.kind)?;
                }
                if let Some(hint) = transport_hint {
                    counted(hint, tilo_relay::delivery_v3::descriptor::MAX_HINT_BYTES)?;
                }
            }
        }
        for p in outgoing.iter() {
            validate_message(p.bytes, p.kind)?;
        }
        for r in &self.collaboration.administration {
            if let Some(result) = &r.media_result {
                result
                    .validate()
                    .map_err(|e| anyhow::anyhow!("invalid media result: {e:?}"))?;
            }
        }
        Ok(())
    }
    pub(super) fn decode_payload(bytes: &[u8]) -> Result<Self> {
        Self::decode_payload_inner(bytes, false)
    }
    pub(super) fn decode_administrative_payload(bytes: &[u8]) -> Result<Self> {
        administrative_base(bytes)?;
        Self::decode_payload_inner(bytes, true)
    }
    fn decode_payload_inner(bytes: &[u8], strict_v4: bool) -> Result<Self> {
        let (version, _) = postcard::take_from_bytes::<u32>(bytes)?;
        let value: Self = match version {
            1 => AggregateV4::from(AggregateV3::from(AggregateV2::from(
                postcard::from_bytes::<LegacyAggregate>(bytes)?,
            )))
            .into(),
            2 => AggregateV4::from(AggregateV3::from(postcard::from_bytes::<AggregateV2>(
                bytes,
            )?))
            .into(),
            3 => AggregateV4::from(postcard::from_bytes::<AggregateV3>(bytes)?).into(),
            4 => {
                let wire: AggregateV4 = postcard::from_bytes(bytes)?;
                if strict_v4 {
                    canonical(&wire, bytes)?;
                }
                wire.into()
            }
            7 => {
                let base = administrative_base(bytes)?;
                let wire: AggregateV7<'_> = exact(bytes)?;
                let mut value = Self::decode_payload_inner(base, true)?;
                let sender = SenderSelection::decode(wire.sender)?;
                sender.validate_scope(&value)?;
                value.sender = Some(sender);
                value
            }
            5 | 6 => {
                let (old, creation) = if version == 6 {
                    let wire: AggregateV6<'_> = exact(bytes)?;
                    (
                        wire.base,
                        Some(CreationLedgerV1::decode(wire.creation).map_err(anyhow::Error::msg)?),
                    )
                } else {
                    (exact::<AggregateV5<'_>>(bytes)?, None)
                };
                if old.delivery.len() > tilo_relay::delivery_v3::commit::MAX_LEDGER_BYTES
                    || old.publications.len() > crate::store::MAX_AGGREGATE_BYTES
                    || old.transport_hint.is_some_and(|b| {
                        b.len() > tilo_relay::delivery_v3::descriptor::MAX_HINT_BYTES
                    })
                {
                    bail!("V5 encoded field exceeds storage bound");
                }
                let delivery = DeliveryLedgerV1::decode(old.delivery)?;
                let pool =
                    PublicationPool::decode(old.publications, crate::store::MAX_AGGREGATE_BYTES)?;
                let transport_hint = old.transport_hint.map(exact::<TransportHint>).transpose()?;
                Self {
                    sender: None,
                    identity_root: old.identity_root,
                    host_identity_root: old.host_identity_root,
                    room: old.room,
                    authority: old.authority,
                    directory: old.directory,
                    peer: old.peer,
                    pending: old.pending,
                    generation: old.generation,
                    issuances: old.issuances,
                    collaboration: old.collaboration,
                    creation,
                    publication: PublicationStorage::V5 {
                        pool,
                        roots: old.publication,
                        delivery,
                        transport_hint,
                    },
                }
            }
            _ => bail!("unsupported aggregate version; explicit migration required"),
        };
        // Legacy incomplete metadata is checked at the existing restore boundary.
        if matches!(value.publication, PublicationStorage::V5 { .. }) {
            value.validate()?;
        }
        Ok(value)
    }
    /// Allocation-free wire count; semantic validation remains inside the reserved turn.
    pub(super) fn encoded_payload_len(&self) -> Result<usize> {
        if self.sender.is_some() {
            bail!("selected aggregate requires the private funded administrative encoder");
        }
        match &self.publication {
            PublicationStorage::Legacy { outgoing, state } => counted(
                &V4View {
                    version: 4,
                    identity_root: &self.identity_root,
                    host_identity_root: &self.host_identity_root,
                    room: &self.room,
                    authority: &self.authority,
                    directory: &self.directory,
                    peer: &self.peer,
                    outgoing,
                    pending: &self.pending,
                    generation: self.generation,
                    issuances: &self.issuances,
                    publication: state,
                    collaboration: crate::collaboration::CollaborationV4View(&self.collaboration),
                },
                crate::store::MAX_AGGREGATE_BYTES,
            ),
            PublicationStorage::V5 {
                pool,
                roots,
                delivery,
                transport_hint,
            } => {
                let wire = V5View {
                    version: if self.creation.is_some() { 6 } else { 5 },
                    identity_root: &self.identity_root,
                    host_identity_root: &self.host_identity_root,
                    room: &self.room,
                    authority: &self.authority,
                    directory: &self.directory,
                    peer: &self.peer,
                    publications: &[],
                    pending: &self.pending,
                    generation: self.generation,
                    issuances: &self.issuances,
                    publication: roots,
                    collaboration: &self.collaboration,
                    delivery: &[],
                    transport_hint: transport_hint.as_ref().map(|_| &[][..]),
                };
                let mut size = counted(&wire, crate::store::MAX_AGGREGATE_BYTES)?;
                for length in [
                    Some(pool.encoded_len(crate::store::MAX_AGGREGATE_BYTES)?),
                    Some(counted(
                        delivery,
                        tilo_relay::delivery_v3::commit::MAX_LEDGER_BYTES,
                    )?),
                    transport_hint
                        .as_ref()
                        .map(|hint| {
                            counted(hint, tilo_relay::delivery_v3::descriptor::MAX_HINT_BYTES)
                        })
                        .transpose()?,
                ]
                .into_iter()
                .flatten()
                {
                    size = size
                        .checked_add(blob_size(length)? - 1)
                        .context("aggregate size overflow")?;
                }
                if let Some(creation) = &self.creation {
                    size = size
                        .checked_add(blob_size(
                            creation.encoded_len().map_err(anyhow::Error::msg)?,
                        )?)
                        .context("creation aggregate size overflow")?;
                }
                Ok(size)
            }
        }
    }
    pub(super) fn encode_payload(&self) -> Result<Vec<u8>> {
        self.encode_payload_bounded(crate::store::MAX_AGGREGATE_BYTES)
    }
    pub(super) fn encode_payload_bounded(&self, limit: usize) -> Result<Vec<u8>> {
        if signed_size(self.encoded_payload_len()?)? > limit {
            return Err(serialized_capacity(anyhow::anyhow!(
                "aggregate exceeds reserved serialized cap"
            )));
        }
        match &self.publication {
            PublicationStorage::Legacy { outgoing, state } => {
                let wire = V4View {
                    version: 4,
                    identity_root: &self.identity_root,
                    host_identity_root: &self.host_identity_root,
                    room: &self.room,
                    authority: &self.authority,
                    directory: &self.directory,
                    peer: &self.peer,
                    outgoing,
                    pending: &self.pending,
                    generation: self.generation,
                    issuances: &self.issuances,
                    publication: state,
                    collaboration: crate::collaboration::CollaborationV4View(&self.collaboration),
                };
                check_signed_size(counted(&wire, crate::store::MAX_AGGREGATE_BYTES)?)?;
                encode_bounded(&wire, crate::store::MAX_AGGREGATE_BYTES)
            }
            PublicationStorage::V5 {
                pool,
                roots,
                delivery,
                transport_hint,
            } => {
                self.validate()?;
                let mut wire = V5View {
                    version: if self.creation.is_some() { 6 } else { 5 },
                    identity_root: &self.identity_root,
                    host_identity_root: &self.host_identity_root,
                    room: &self.room,
                    authority: &self.authority,
                    directory: &self.directory,
                    peer: &self.peer,
                    publications: &[],
                    pending: &self.pending,
                    generation: self.generation,
                    issuances: &self.issuances,
                    publication: roots,
                    collaboration: &self.collaboration,
                    delivery: &[],
                    transport_hint: transport_hint.as_ref().map(|_| &[][..]),
                };
                let pool_len = pool.encoded_len(crate::store::MAX_AGGREGATE_BYTES)?;
                let ledger_len = delivery.encoded_len()?;
                let hint_len = transport_hint
                    .as_ref()
                    .map(|h| counted(h, tilo_relay::delivery_v3::descriptor::MAX_HINT_BYTES))
                    .transpose()?;
                let mut length = counted(&wire, crate::store::MAX_AGGREGATE_BYTES)?;
                for n in [Some(pool_len), Some(ledger_len), hint_len]
                    .into_iter()
                    .flatten()
                {
                    length = length
                        .checked_add(blob_size(n)? - 1)
                        .context("aggregate size overflow")?;
                }
                if let Some(creation) = &self.creation {
                    length = length
                        .checked_add(blob_size(
                            creation.encoded_len().map_err(anyhow::Error::msg)?,
                        )?)
                        .context("creation aggregate size overflow")?;
                }
                check_signed_size(length)?; // Before allocating any nested encoded blob.
                let pool = pool.encode(crate::store::MAX_AGGREGATE_BYTES)?;
                let ledger = delivery.encode()?;
                let hint = transport_hint
                    .as_ref()
                    .map(|hint| {
                        encode_bounded(hint, tilo_relay::delivery_v3::descriptor::MAX_HINT_BYTES)
                    })
                    .transpose()?;
                wire.publications = &pool;
                wire.delivery = &ledger;
                wire.transport_hint = hint.as_deref();
                if let Some(creation) = &self.creation {
                    let bytes = creation.encode().map_err(anyhow::Error::msg)?;
                    encode_bounded(
                        &V6View {
                            base: wire,
                            creation: &bytes,
                        },
                        crate::store::MAX_AGGREGATE_BYTES,
                    )
                } else {
                    encode_bounded(&wire, crate::store::MAX_AGGREGATE_BYTES)
                }
            }
        }
    }
}
fn validate_message(bytes: &[u8], kind: Retention) -> Result<()> {
    let message: NeighbourMsg = postcard::from_bytes(bytes)?;
    if retention(&message, kind == Retention::Checkpoint).map(|(_, k)| k) != Some(kind) {
        bail!("invalid durable publication retention kind");
    }
    Ok(())
}
fn blob_size(length: usize) -> Result<usize> {
    let mut value = length;
    let mut prefix = 1usize;
    while value >= 128 {
        value >>= 7;
        prefix += 1;
    }
    length
        .checked_add(prefix)
        .context("aggregate size overflow")
}
pub(super) fn signed_size(payload: usize) -> Result<usize> {
    blob_size(payload)?
        .checked_add(blob_size(64)?)
        .context("signed size overflow")
}
/// The administrative path keeps newly introduced sizing failures inline.
pub(super) fn signed_size_raw(payload: usize) -> Option<usize> {
    let mut value = payload;
    let mut prefix = 1usize;
    while value >= 128 {
        value >>= 7;
        prefix += 1;
    }
    payload.checked_add(prefix)?.checked_add(65)
}
fn check_signed_size(payload: usize) -> Result<()> {
    if blob_size(payload)?
        .checked_add(blob_size(64)?)
        .is_none_or(|n| n > crate::store::MAX_AGGREGATE_BYTES)
    {
        bail!(
            "aggregate exceeds the local storage size limit; retain existing data and free storage pressure before retry"
        );
    }
    Ok(())
}
fn serialized_capacity(error: anyhow::Error) -> anyhow::Error {
    delivery::error::failure(
        delivery::error::OwnerDataFailureKind::Capacity(delivery::error::OwnerCapacityKind::Bytes),
        error,
    )
}
pub(super) fn counted<T: Serialize>(value: &T, limit: usize) -> Result<usize> {
    counted_raw(value, limit).map_err(|error| {
        if error == postcard::Error::SerializeBufferFull {
            serialized_capacity(error.into())
        } else {
            error.into()
        }
    })
}
pub(super) fn counted_raw<T: Serialize>(value: &T, limit: usize) -> postcard::Result<usize> {
    postcard::serialize_with_flavor(value, Count { bytes: 0, limit })
}
struct Count {
    bytes: usize,
    limit: usize,
}
impl postcard::ser_flavors::Flavor for Count {
    type Output = usize;
    fn try_push(&mut self, _: u8) -> postcard::Result<()> {
        self.add(1)
    }
    fn try_extend(&mut self, bytes: &[u8]) -> postcard::Result<()> {
        self.add(bytes.len())
    }
    fn finalize(self) -> postcard::Result<usize> {
        Ok(self.bytes)
    }
}
impl Count {
    fn add(&mut self, len: usize) -> postcard::Result<()> {
        self.bytes = self
            .bytes
            .checked_add(len)
            .filter(|n| *n <= self.limit)
            .ok_or(postcard::Error::SerializeBufferFull)?;
        Ok(())
    }
}
fn exact<'de, T: Deserialize<'de> + Serialize>(bytes: &'de [u8]) -> Result<T> {
    let (value, trailing) = postcard::take_from_bytes(bytes)?;
    if !trailing.is_empty() {
        bail!("trailing V5 bytes");
    }
    canonical(&value, bytes)?;
    Ok(value)
}
pub(super) fn canonical<T: Serialize>(value: &T, bytes: &[u8]) -> Result<()> {
    postcard::serialize_with_flavor(value, Exact(bytes))?;
    Ok(())
}
struct Exact<'a>(&'a [u8]);
impl postcard::ser_flavors::Flavor for Exact<'_> {
    type Output = ();
    fn try_push(&mut self, byte: u8) -> postcard::Result<()> {
        self.try_extend(&[byte])
    }
    fn try_extend(&mut self, bytes: &[u8]) -> postcard::Result<()> {
        if !self.0.starts_with(bytes) {
            return Err(postcard::Error::SerializeBufferFull);
        }
        self.0 = &self.0[bytes.len()..];
        Ok(())
    }
    fn finalize(self) -> postcard::Result<()> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(postcard::Error::SerializeBufferFull)
        }
    }
}

/// Borrowed exact publication bytes and their paired retention metadata.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PublicationView<'a> {
    pub bytes: &'a [u8],
    pub kind: Retention,
}
/// A view of the committed baseline plus tail. It never copies payload bytes.
#[derive(Clone, Copy)]
pub struct OutgoingPublications<'a> {
    storage: &'a PublicationStorage,
}
impl<'a> OutgoingPublications<'a> {
    pub fn len(&self) -> usize {
        match self.storage {
            PublicationStorage::Legacy { outgoing, .. } => outgoing.len(),
            PublicationStorage::V5 { roots, .. } => roots.baseline.len() + roots.tail.len(),
        }
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn get(&self, index: usize) -> Option<PublicationView<'a>> {
        match self.storage {
            PublicationStorage::Legacy { outgoing, state } => {
                outgoing.get(index).map(|bytes| PublicationView {
                    bytes,
                    kind: state.kinds[index],
                })
            }
            PublicationStorage::V5 { pool, roots, .. } => {
                let reference = if index < roots.baseline.len() {
                    roots.baseline.get(index)
                } else {
                    roots.tail.get(index - roots.baseline.len())
                }?;
                Some(PublicationView {
                    bytes: pool
                        .resolve(&reference.payload)
                        .expect("validated publication root"),
                    kind: reference.kind,
                })
            }
        }
    }
    pub fn iter(
        &self,
    ) -> impl DoubleEndedIterator<Item = PublicationView<'a>> + ExactSizeIterator + 'a {
        let view = *self;
        (0..view.len()).map(move |i| view.get(i).expect("bounded publication index"))
    }
}

#[cfg(test)]
mod tests;

/// Count before allocating a fixed-size output. Capacity is part of the byte budget.
pub(super) fn encode_bounded<T: Serialize>(value: &T, limit: usize) -> Result<Vec<u8>> {
    let length = counted(value, limit)?;
    let mut bytes = vec![0; length];
    let written = postcard::to_slice(value, &mut bytes)?.len();
    if written != length || bytes.capacity() != length {
        bail!("exact serialization size changed");
    }
    Ok(bytes)
}
