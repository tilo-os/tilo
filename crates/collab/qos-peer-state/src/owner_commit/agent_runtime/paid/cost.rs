//! Concrete allocation model for the unchanged validators. All arithmetic is
//! checked; this module never invokes a decoder, clones data, or allocates.
use super::*;
use serde::Serialize;
#[path = "json.rs"]
mod json;
mod strings;
#[derive(Clone, Copy, Debug)]
pub(super) struct Capacity;
impl std::fmt::Display for Capacity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("private validation capacity")
    }
}
impl std::error::Error for Capacity {}
impl serde::ser::Error for Capacity {
    fn custom<T: std::fmt::Display>(_: T) -> Self {
        Self
    }
}
type Result<T> = std::result::Result<T, Capacity>;
fn sum(values: &[usize]) -> Result<usize> {
    values
        .iter()
        .try_fold(0usize, |a, b| a.checked_add(*b).ok_or(Capacity))
}
fn mul(a: usize, b: usize) -> Result<usize> {
    a.checked_mul(b).ok_or(Capacity)
}
// Rust RawVec grow_amortized: new=max(2*old,required,minimum). For an
// eventually written n-element output, old+new<=3*max(n,minimum).
fn growth(n: usize, item: usize) -> Result<usize> {
    if n == 0 {
        Ok(0)
    } else {
        mul(mul(n.max(if item == 1 { 8 } else { 4 }), 3)?, item)
    }
}
#[repr(C)]
struct Node<K, V> {
    parent: usize,
    parent_index: u16,
    length: u16,
    keys: [K; 11],
    values: [V; 11],
    edges: [usize; 12],
}
fn nodes<K, V>(n: usize) -> Result<usize> {
    if n == 0 {
        Ok(0)
    } else {
        mul(n.checked_add(1).ok_or(Capacity)?, size_of::<Node<K, V>>())
    }
}
fn refs(n: usize) -> Result<usize> {
    growth(n, size_of::<usize>())
}
fn text<T: Serialize>(v: &T) -> Result<usize> {
    strings::bytes(v)
}
struct Count(usize);
impl postcard::ser_flavors::Flavor for Count {
    type Output = usize;
    fn try_push(&mut self, _: u8) -> postcard::Result<()> {
        self.try_extend(&[0])
    }
    fn try_extend(&mut self, b: &[u8]) -> postcard::Result<()> {
        self.0 = self
            .0
            .checked_add(b.len())
            .ok_or(postcard::Error::SerializeBufferFull)?;
        Ok(())
    }
    fn finalize(self) -> postcard::Result<usize> {
        Ok(self.0)
    }
}
fn encoded<T: Serialize>(v: &T) -> Result<usize> {
    postcard::serialize_with_flavor(v, Count(0)).map_err(|_| Capacity)
}
fn business_v1(c: &AgentChange) -> Result<usize> {
    encoded(&(&c.ids, &c.caller, &c.expected_governance, &c.mutation))
}
fn business_v2(c: &AgentRuntimeChange) -> Result<usize> {
    encoded(&(&c.mutation, &c.invocation, &c.expected_governance, &c.fact))
}
// Binding target wire is enum discriminant + its one string. The enclosing
// tuple is room, target, byte sequence; Postcard sequence length is <=10 bytes.
fn binding(room: &str, mutation: &str, business: usize) -> Result<usize> {
    let target = encoded(&mutation)?;
    let total = sum(&[encoded(&room)?, 1, target, 10, business])?;
    sum(&[
        growth(business, 1)?,
        growth(total, 1)?,
        room.len(),
        mutation.len(),
        36,
    ])
}
fn clone_v1(c: &AgentChange) -> Result<usize> {
    let extra = match &c.mutation {
        AgentMutation::Prepare {
            request, grants: g, ..
        } => sum(&[request.len(), mul(g.len(), size_of::<GrantAction>())?])?,
        AgentMutation::Transition { .. } => 0,
    };
    sum(&[size_of::<AgentChange>(), text(c)?, extra])
}
fn clone_v2(c: &AgentRuntimeChange) -> Result<usize> {
    // Exhaustive: only Admitted contains a Vec. All other owned fields are
    // inline structs/enums/String, accounted by text plus outer Box layout.
    let extra = match &c.fact {
        AgentRuntimeFact::Admitted { intent, .. } => {
            mul(intent.grants.len(), size_of::<GrantAction>())?
        }
        AgentRuntimeFact::LaunchIntended { .. }
        | AgentRuntimeFact::ProcessBound { .. }
        | AgentRuntimeFact::DispatchIntended { .. }
        | AgentRuntimeFact::Completed { .. }
        | AgentRuntimeFact::Failed { .. }
        | AgentRuntimeFact::Unknown { .. }
        | AgentRuntimeFact::TaskUnknown { .. }
        | AgentRuntimeFact::TaskFinished { .. }
        | AgentRuntimeFact::ProcessExited { .. }
        | AgentRuntimeFact::TaskStepBound { .. }
        | AgentRuntimeFact::TaskStepCommitted { .. } => 0,
    };
    sum(&[size_of::<AgentRuntimeChange>(), text(c)?, extra])
}
fn intent(request: &[u8], grant_count: usize) -> Result<usize> {
    sum(&[
        json::request(request)?,
        nodes::<(&String, &String, &String, &String), ()>(grant_count)?,
    ])
}
fn v1(v: &PrivateAgentLedger, room: &str) -> Result<usize> {
    let retained = mul(nodes::<&String, ()>(v.records.len())?, 3)?;
    let mut peak = 0;
    for r in v.records.values() {
        let count = v
            .mutations
            .values()
            .filter(|m| m.invocation == r.invocation)
            .count();
        let diagnostics = v
            .mutations
            .values()
            .filter(|m| m.invocation == r.invocation)
            .filter_map(|m| match &m.change.mutation {
                AgentMutation::Transition { error, .. } => error.as_ref(),
                _ => None,
            })
            .map(|e| e.message.len())
            .max()
            .unwrap_or(0);
        peak = peak.max(intent(&r.request, r.grants.len())?).max(sum(&[
            refs(count)?,
            mul(count, size_of::<&AgentMutationReceipt>())?,
            mul(diagnostics, 2)?,
        ])?);
    }
    for m in v.mutations.values() {
        let c = &m.change;
        let b = business_v1(c)?;
        let nested = match &c.mutation {
            AgentMutation::Prepare {
                request, grants, ..
            } => intent(request, grants.len())?,
            _ => 0,
        };
        peak = peak.max(sum(&[
            room.len(),
            clone_v1(c)?,
            binding(room, &c.ids.mutation, b)?,
            nested,
            c.ids.mutation.len(),
        ])?);
    }
    sum(&[retained, peak])
}
fn v2(v: &PrivateAgentLedgerV2, room: &str) -> Result<usize> {
    let n = v.facts.len();
    let legacy = v.legacy_v1.records.len();
    let a = v
        .facts
        .values()
        .filter(|r| matches!(r.change.fact, AgentRuntimeFact::Admitted { .. }))
        .count();
    let union = legacy.checked_add(a).ok_or(Capacity)?;
    // Every persistent validation table is named explicitly. References are
    // borrowed except task lease strings and acceptance map channel strings.
    let ordered = growth(n, size_of::<(&String, &AgentRuntimeFactReceipt)>())?;
    let collected = mul(
        sum(&[
            growth(legacy, size_of::<&str>())?,
            mul(legacy, size_of::<&str>())?,
        ])?,
        3,
    )?;
    let mut owned = 0usize;
    for r in v.facts.values() {
        match &r.change.fact {
            AgentRuntimeFact::DispatchIntended { envelope, .. } => {
                owned = sum(&[owned, json::string_bytes(envelope.as_bytes())?])?;
            }
            AgentRuntimeFact::Completed { acceptance, .. } => {
                owned = sum(&[owned, acceptance.channel_lease.len()])?;
            }
            _ => {}
        }
    }
    let retained = sum(&[
        ordered,
        collected,
        nodes::<&str, super::super::validation::Operation<'_>>(a)?,
        mul(nodes::<&str, ()>(union)?, 3)?,
        mul(nodes::<&String, ()>(n)?, 3)?,
        nodes::<&str, ()>(n)?,
        nodes::<&str, (usize, usize)>(n)?,
        nodes::<(&String, u64), ()>(n)?,
        nodes::<(&String, String), ()>(n)?,
        nodes::<String, u64>(n)?,
        // Step child ticket uniqueness set (borrowed keys).
        nodes::<&str, ()>(n)?,
        owned,
    ])?;
    let mut peak = mul(n, size_of::<(&String, &AgentRuntimeFactReceipt)>())?;
    let max_request = v
        .facts
        .values()
        .filter_map(|r| match &r.change.fact {
            AgentRuntimeFact::Admitted { intent, .. } => Some(intent.request.as_bytes()),
            _ => None,
        })
        .map(json::request)
        .try_fold(0usize, |a, n| Ok(a.max(n?)))?;
    for r in v.facts.values() {
        let c = &r.change;
        let b = business_v2(c)?;
        let extra = match &c.fact {
            AgentRuntimeFact::Admitted { intent: i, .. } => sum(&[
                intent(i.request.as_bytes(), i.grants.len())?,
                growth(sum(&["workspace:".len(), room.len(), ":agents".len()])?, 1)?,
                20,
                36,
            ])?,
            AgentRuntimeFact::DispatchIntended { envelope, .. } => {
                sum(&[json::envelope(envelope.as_bytes())?, max_request])?
            }
            // Completion reparses an earlier dispatch. Conservatively choose the
            // largest real retained envelope, not total bytes across all dispatches.
            AgentRuntimeFact::Completed { .. } => sum(&[
                v.facts
                    .values()
                    .filter_map(|r| match &r.change.fact {
                        AgentRuntimeFact::DispatchIntended { envelope, .. } => Some(envelope),
                        _ => None,
                    })
                    .map(|e| json::envelope(e.as_bytes()))
                    .try_fold(0usize, |a, n| Ok(a.max(n?)))?,
                max_request,
            ])?,
            AgentRuntimeFact::ProcessExited { .. } => {
                growth(sum(&[c.invocation.len(), ":process_bound".len()])?, 1)?
            }
            // Referenced key string plus decoded child and intent bytes.
            AgentRuntimeFact::TaskStepBound { .. } => sum(&[
                growth(sum(&[c.invocation.len(), ":dispatch_intended".len()])?, 1)?,
                growth(48, 1)?,
                growth(32, 1)?,
            ])?,
            // Referenced key, decoded result/continuation, result JSON parse.
            AgentRuntimeFact::TaskStepCommitted {
                result_json,
                continuation,
                ..
            } => sum(&[
                growth(
                    sum(&[c.invocation.len(), ":task_step_bound:".len(), 10])?,
                    1,
                )?,
                growth(32, 1)?,
                growth(continuation.len() / 2, 1)?,
                json::step_result(result_json.as_bytes())?,
            ])?,
            AgentRuntimeFact::LaunchIntended { .. }
            | AgentRuntimeFact::ProcessBound { .. }
            | AgentRuntimeFact::Failed { .. }
            | AgentRuntimeFact::Unknown { .. }
            | AgentRuntimeFact::TaskUnknown { .. }
            | AgentRuntimeFact::TaskFinished { .. } => 0,
        };
        peak = peak.max(sum(&[
            clone_v2(c)?,
            room.len(),
            binding(room, &c.mutation, b)?,
            // mutation_key: invocation, ':', kind and, for steps, ':' + index.
            growth(sum(&[c.invocation.len(), 1, c.fact.kind().len(), 11])?, 1)?,
            extra,
            nodes::<&String, &AgentRuntimeFactReceipt>(1)?,
        ])?);
    }
    Ok(v1(&v.legacy_v1, room)?.max(sum(&[retained, peak])?))
}
pub(super) fn validation(v: &PrivateAgentState, room: &str) -> Result<usize> {
    let body = match v {
        PrivateAgentState::V1(v) => v1(v, room)?,
        PrivateAgentState::V2(v) => v2(v, room)?,
    };
    // Error strings may mention any actual retained string (UUID/parser errors),
    // but only the largest diagnostic path lives with the validator's tables.
    let strings = strings::largest(v)?;
    sum(&[body, json::diagnostics(strings.max(room.len()))?])
}
