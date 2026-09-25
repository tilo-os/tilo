//! Concrete private1/2 shadow wire. No Value tree, clone, or untagged retry.
//! Decode is inert; live/semantic validation belongs to the paid Store turn.
use qos_peer_state::owner_commit::*;
use qos_user_identity::{DeviceCapabilities, DeviceCert};
use serde::{
    Deserialize, Deserializer,
    de::{self, MapAccess, SeqAccess, Visitor},
};
use std::{collections::BTreeMap, fmt, marker::PhantomData};

pub(super) struct PrivateWire(pub(super) PrivateAgentState);
#[derive(Default)]
enum Present<T> {
    #[default]
    Missing,
    Value(T),
}
impl<'de, T: Deserialize<'de>> Deserialize<'de> for Present<T> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        T::deserialize(d).map(Self::Value)
    }
}
impl<T> Present<T> {
    fn missing(&self) -> bool {
        matches!(self, Self::Missing)
    }
    fn take<E: de::Error>(self) -> Result<T, E> {
        match self {
            Self::Value(v) => Ok(v),
            Self::Missing => Err(E::custom("missing private field")),
        }
    }
}
fn nullable<'de, D: Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Option<T>, D::Error> {
    Option::deserialize(d)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Root {
    version: u32,
    #[serde(default)]
    records: Present<Records>,
    #[serde(default)]
    mutations: Present<Mutations>,
    #[serde(default)]
    writer_floor: Present<u32>,
    #[serde(default)]
    legacy_v1: Present<Legacy>,
    #[serde(default)]
    facts: Present<Facts>,
}
impl<'de> Deserialize<'de> for PrivateWire {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let w = Root::deserialize(d)?;
        let value = match w.version {
            1 if w.writer_floor.missing() && w.legacy_v1.missing() && w.facts.missing() => {
                PrivateAgentState::V1(PrivateAgentLedger {
                    version: 1,
                    records: w.records.take()?.0,
                    mutations: w.mutations.take()?.0,
                })
            }
            2 if w.records.missing() && w.mutations.missing() => {
                PrivateAgentState::V2(PrivateAgentLedgerV2 {
                    version: 2,
                    writer_floor: w.writer_floor.take()?,
                    legacy_v1: w.legacy_v1.take()?.0,
                    facts: w.facts.take()?.0,
                })
            }
            _ => return Err(de::Error::custom("private version/field mismatch")),
        };
        Ok(Self(value))
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyFields {
    version: u32,
    records: Records,
    mutations: Mutations,
}
struct Legacy(PrivateAgentLedger);
impl<'de> Deserialize<'de> for Legacy {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let w = LegacyFields::deserialize(d)?;
        Ok(Self(PrivateAgentLedger {
            version: w.version,
            records: w.records.0,
            mutations: w.mutations.0,
        }))
    }
}
// One direct native map insertion per entry. No map-to-map conversion or hidden
// allocation after the allocation-aware caller has observed the typed seed.
trait MapWire {
    type Value;
    fn value(self) -> Self::Value;
}
fn unique_map<'de, D: Deserializer<'de>, W: Deserialize<'de> + MapWire>(
    d: D,
) -> Result<BTreeMap<String, W::Value>, D::Error> {
    struct Entries<W>(PhantomData<W>);
    impl<'de, W: Deserialize<'de> + MapWire> Visitor<'de> for Entries<W> {
        type Value = BTreeMap<String, W::Value>;
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("unique private map")
        }
        fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<Self::Value, A::Error> {
            let mut out = BTreeMap::new();
            while let Some(key) = a.next_key::<String>()? {
                if out.contains_key(&key) {
                    return Err(de::Error::custom("duplicate private key"));
                }
                let value = a.next_value::<W>()?.value();
                out.insert(key, value);
            }
            Ok(out)
        }
    }
    d.deserialize_map(Entries::<W>(PhantomData))
}
macro_rules! map {
    ($name:ident,$wire:ident,$native:ty) => {
        struct $name(BTreeMap<String, $native>);
        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                unique_map::<D, $wire>(d).map(Self)
            }
        }
        impl MapWire for $wire {
            type Value = $native;
            fn value(self) -> Self::Value {
                self.0
            }
        }
    };
}
map!(Records, Record, PrivateAgentRecord);
map!(Mutations, MutationReceipt, AgentMutationReceipt);
map!(Facts, FactReceipt, AgentRuntimeFactReceipt);

// The previous canonical Value comparison required expiry to be present even
// though ordinary DeviceCert derive permits its nullable field to be omitted.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CertFields {
    user_public_key: [u8; 32],
    device_public_key: [u8; 32],
    device_label: String,
    capabilities: DeviceCapabilities,
    issued_at_ns: u64,
    #[serde(deserialize_with = "nullable")]
    expires_at_ns: Option<u64>,
    signature: Signature,
}
struct Cert(DeviceCert);
impl<'de> Deserialize<'de> for Cert {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let w = CertFields::deserialize(d)?;
        Ok(Self(DeviceCert {
            user_public_key: w.user_public_key,
            device_public_key: w.device_public_key,
            device_label: w.device_label,
            capabilities: w.capabilities,
            issued_at_ns: w.issued_at_ns,
            expires_at_ns: w.expires_at_ns,
            signature: w.signature.0,
        }))
    }
}
struct Signature([u8; 64]);
impl<'de> Deserialize<'de> for Signature {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Bytes;
        impl<'de> Visitor<'de> for Bytes {
            type Value = Signature;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("64 signature bytes")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<Self::Value, A::Error> {
                let mut bytes = [0; 64];
                for byte in &mut bytes {
                    *byte = a
                        .next_element()?
                        .ok_or_else(|| de::Error::custom("short signature"))?;
                }
                if a.next_element::<u8>()?.is_some() {
                    return Err(de::Error::custom("long signature"));
                }
                Ok(Signature(bytes))
            }
        }
        d.deserialize_seq(Bytes)
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordFields {
    ids: AgentIds,
    invocation: String,
    caller: Cert,
    request: Vec<u8>,
    grants: Vec<GrantAction>,
    phase: AgentControlPhase,
    admitted_generation: u64,
    last_generation: u64,
    #[serde(deserialize_with = "nullable")]
    error: Option<AgentError>,
}
struct Record(PrivateAgentRecord);
impl<'de> Deserialize<'de> for Record {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let w = RecordFields::deserialize(d)?;
        Ok(Self(PrivateAgentRecord {
            ids: w.ids,
            invocation: w.invocation,
            caller: w.caller.0,
            request: w.request,
            grants: w.grants,
            phase: w.phase,
            admitted_generation: w.admitted_generation,
            last_generation: w.last_generation,
            error: w.error,
        }))
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
enum Mutation {
    Prepare {
        invocation: String,
        request: Vec<u8>,
        grants: Vec<GrantAction>,
    },
    Transition {
        phase: AgentControlPhase,
        #[serde(deserialize_with = "nullable")]
        error: Option<AgentError>,
    },
}
impl Mutation {
    fn value(self) -> AgentMutation {
        match self {
            Self::Prepare {
                invocation,
                request,
                grants,
            } => AgentMutation::Prepare {
                invocation,
                request,
                grants,
            },
            Self::Transition { phase, error } => AgentMutation::Transition { phase, error },
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChangeFields {
    ids: AgentIds,
    caller: Cert,
    expected_generation: u64,
    expected_governance: String,
    payload_digest: [u8; 32],
    mutation: Mutation,
}
impl ChangeFields {
    fn value(self) -> AgentChange {
        AgentChange {
            ids: self.ids,
            caller: self.caller.0,
            expected_generation: self.expected_generation,
            expected_governance: self.expected_governance,
            payload_digest: self.payload_digest,
            mutation: self.mutation.value(),
        }
    }
}
// UUID must retain the exact JSON string that the old from/to-Value equality
// admitted; an upper-case/unhyphenated string must not silently normalize.
struct ExactUuid(uuid::Uuid);
impl<'de> Deserialize<'de> for ExactUuid {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Uuid;
        impl Visitor<'_> for Uuid {
            type Value = ExactUuid;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("canonical UUID")
            }
            fn visit_str<E: de::Error>(self, s: &str) -> Result<Self::Value, E> {
                let id = uuid::Uuid::parse_str(s).map_err(|_| E::custom("invalid UUID"))?;
                let mut b = [0; 36];
                if id.hyphenated().encode_lower(&mut b) != s {
                    return Err(E::custom("noncanonical UUID"));
                }
                Ok(ExactUuid(id))
            }
        }
        d.deserialize_str(Uuid)
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
enum Target {
    Document { app: String, record: String },
    EffectReceipt { invocation: ExactUuid },
    DesktopCommand { invocation: ExactUuid },
    Agent { mutation: String },
    AgentRuntime { mutation: String },
}
impl Target {
    fn value(self) -> CommitTarget {
        match self {
            Self::Document { app, record } => CommitTarget::Document { app, record },
            Self::EffectReceipt { invocation } => CommitTarget::EffectReceipt {
                invocation: invocation.0,
            },
            Self::DesktopCommand { invocation } => CommitTarget::DesktopCommand {
                invocation: invocation.0,
            },
            Self::Agent { mutation } => CommitTarget::Agent { mutation },
            Self::AgentRuntime { mutation } => CommitTarget::AgentRuntime { mutation },
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Binding {
    room: String,
    target: Target,
    request_digest: [u8; 32],
    basis: [u8; 32],
}
impl Binding {
    fn value(self) -> CommitBinding {
        CommitBinding {
            room: self.room,
            target: self.target.value(),
            request_digest: self.request_digest,
            basis: self.basis,
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MutationReceiptFields {
    binding: Binding,
    generation: u64,
    invocation: String,
    change: ChangeFields,
}
struct MutationReceipt(AgentMutationReceipt);
impl<'de> Deserialize<'de> for MutationReceipt {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let w = MutationReceiptFields::deserialize(d)?;
        Ok(Self(AgentMutationReceipt {
            binding: w.binding.value(),
            generation: w.generation,
            invocation: w.invocation,
            change: w.change.value(),
        }))
    }
}

// No internally-tagged Content tree: exact flat field selection reconstructs
// one concrete native fact and moves every owned value once.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Scope {
    machine: String,
    room: String,
    host: String,
    caller: Cert,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Intent {
    scope: Scope,
    route: AgentProviderRouteV2,
    descriptor_revision: String,
    request: String,
    grants: Vec<GrantAction>,
}
impl Intent {
    fn value(self) -> AgentIntentV2 {
        AgentIntentV2 {
            scope: AgentInvocationScopeV2 {
                machine: self.scope.machine,
                room: self.scope.room,
                host: self.scope.host,
                caller: self.scope.caller.0,
            },
            route: self.route,
            descriptor_revision: self.descriptor_revision,
            request: self.request,
            grants: self.grants,
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FactFields {
    kind: String,
    #[serde(default)]
    ids: Present<AgentExecutionIdsV2>,
    #[serde(default)]
    intent: Present<Intent>,
    #[serde(default)]
    parent_invocation: Present<Option<String>>,
    #[serde(default)]
    reservation: Present<AgentReservationV2>,
    #[serde(default)]
    timestamp_ms: Present<u64>,
    #[serde(default)]
    binding: Present<ManagedBindingFactsV2>,
    #[serde(default)]
    runtime: Present<ManagedRuntimeFactsV2>,
    #[serde(default)]
    launch_fact: Present<String>,
    #[serde(default)]
    process: Present<ChannelProcessFactsV2>,
    #[serde(default)]
    process_fact: Present<String>,
    #[serde(default)]
    envelope: Present<String>,
    #[serde(default)]
    dispatch_fact: Present<String>,
    #[serde(default)]
    acceptance: Present<RecipientAcceptanceV2>,
    #[serde(default)]
    error: Present<AgentError>,
    #[serde(default)]
    completed_fact: Present<String>,
    #[serde(default)]
    acceptance_sequence: Present<u64>,
    #[serde(default)]
    update_sequence: Present<u64>,
    #[serde(default)]
    outcome: Present<TaskOutcomeV2>,
    #[serde(default)]
    text: Present<String>,
    #[serde(default)]
    exit: Present<SupervisorExitV2>,
}
struct Fact(AgentRuntimeFact);
impl<'de> Deserialize<'de> for Fact {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let w = FactFields::deserialize(d)?;
        let present = [
            !w.ids.missing(),
            !w.intent.missing(),
            !w.parent_invocation.missing(),
            !w.reservation.missing(),
            !w.timestamp_ms.missing(),
            !w.binding.missing(),
            !w.runtime.missing(),
            !w.launch_fact.missing(),
            !w.process.missing(),
            !w.process_fact.missing(),
            !w.envelope.missing(),
            !w.dispatch_fact.missing(),
            !w.acceptance.missing(),
            !w.error.missing(),
            !w.completed_fact.missing(),
            !w.acceptance_sequence.missing(),
            !w.update_sequence.missing(),
            !w.outcome.missing(),
            !w.text.missing(),
            !w.exit.missing(),
        ];
        let expected: &[usize] = match w.kind.as_str() {
            "admitted" => &[0, 1, 2, 3, 4],
            "launch_intended" => &[5, 6],
            "process_bound" => &[7, 8],
            "dispatch_intended" => &[9, 10],
            "completed" => &[11, 12, 4],
            "failed" | "unknown" | "task_unknown" => &[13, 4],
            "task_finished" => &[14, 15, 16, 17, 18, 4],
            "process_exited" => &[7, 19, 4],
            _ => return Err(de::Error::custom("unknown private fact")),
        };
        if present
            .iter()
            .enumerate()
            .any(|(i, p)| *p != expected.contains(&i))
        {
            return Err(de::Error::custom("private fact field mismatch"));
        }
        let fact = match w.kind.as_str() {
            "admitted" => AgentRuntimeFact::Admitted {
                ids: w.ids.take()?,
                intent: w.intent.take()?.value(),
                parent_invocation: w.parent_invocation.take()?,
                reservation: w.reservation.take()?,
                timestamp_ms: w.timestamp_ms.take()?,
            },
            "launch_intended" => AgentRuntimeFact::LaunchIntended {
                binding: w.binding.take()?,
                runtime: w.runtime.take()?,
            },
            "process_bound" => AgentRuntimeFact::ProcessBound {
                launch_fact: w.launch_fact.take()?,
                process: w.process.take()?,
            },
            "dispatch_intended" => AgentRuntimeFact::DispatchIntended {
                process_fact: w.process_fact.take()?,
                envelope: w.envelope.take()?,
            },
            "completed" => AgentRuntimeFact::Completed {
                dispatch_fact: w.dispatch_fact.take()?,
                acceptance: w.acceptance.take()?,
                timestamp_ms: w.timestamp_ms.take()?,
            },
            "failed" => AgentRuntimeFact::Failed {
                error: w.error.take()?,
                timestamp_ms: w.timestamp_ms.take()?,
            },
            "unknown" => AgentRuntimeFact::Unknown {
                error: w.error.take()?,
                timestamp_ms: w.timestamp_ms.take()?,
            },
            "task_unknown" => AgentRuntimeFact::TaskUnknown {
                error: w.error.take()?,
                timestamp_ms: w.timestamp_ms.take()?,
            },
            "task_finished" => AgentRuntimeFact::TaskFinished {
                completed_fact: w.completed_fact.take()?,
                acceptance_sequence: w.acceptance_sequence.take()?,
                update_sequence: w.update_sequence.take()?,
                outcome: w.outcome.take()?,
                text: w.text.take()?,
                timestamp_ms: w.timestamp_ms.take()?,
            },
            "process_exited" => AgentRuntimeFact::ProcessExited {
                launch_fact: w.launch_fact.take()?,
                exit: w.exit.take()?,
                timestamp_ms: w.timestamp_ms.take()?,
            },
            _ => unreachable!(),
        };
        Ok(Self(fact))
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeChange {
    mutation: String,
    invocation: String,
    expected_generation: u64,
    expected_governance: String,
    payload_digest: [u8; 32],
    fact: Fact,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FactReceiptFields {
    binding: Binding,
    generation: u64,
    change: RuntimeChange,
}
struct FactReceipt(AgentRuntimeFactReceipt);
impl<'de> Deserialize<'de> for FactReceipt {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let w = FactReceiptFields::deserialize(d)?;
        Ok(Self(AgentRuntimeFactReceipt {
            binding: w.binding.value(),
            generation: w.generation,
            change: AgentRuntimeChange {
                mutation: w.change.mutation,
                invocation: w.change.invocation,
                expected_generation: w.change.expected_generation,
                expected_governance: w.change.expected_governance,
                payload_digest: w.change.payload_digest,
                fact: w.change.fact.0,
            },
        }))
    }
}
#[cfg(test)]
mod tests;
