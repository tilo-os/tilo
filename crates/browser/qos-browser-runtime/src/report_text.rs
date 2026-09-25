//! Text of the peer's status reports and the rule for not re-delivering one.
//!
//! The full report (`peer::report`) recomputes health on every call because
//! durability decisions are never cached; only its presentation may be
//! reused. Between commit boundaries the light report carries just the
//! retained phase and the persistence trigger: the host merges statuses field
//! by field, so everything omitted keeps the value of the last full report,
//! and the full report at the next commit boundary refreshes rows, outbox
//! counts and health.
use std::hash::{Hash, Hasher};

pub(crate) fn text_hash(text: &str) -> u64 {
    let mut hasher = std::hash::DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

/// Report between commit boundaries. `phase` is the last full report's phase.
pub(crate) fn light_report(phase: &str, persistence_needed: bool) -> String {
    serde_json::json!({"phase": phase, "persistenceNeeded": persistence_needed}).to_string()
}

/// Skips handing the host a report whose text it already holds, which saves
/// the host's `JSON.parse` and snapshot comparison. A report that asks the
/// host to persist is always delivered: the request is a trigger, not state.
#[derive(Default)]
pub(crate) struct ReportDedup {
    last: Option<u64>,
}
impl ReportDedup {
    /// Whether `text` must reach the host; records it as the last emitted text.
    pub(crate) fn should_emit(&mut self, text: &str, persistence_needed: bool) -> bool {
        let hash = text_hash(text);
        if self.last == Some(hash) && !persistence_needed {
            return false;
        }
        self.last = Some(hash);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identical_reports_are_delivered_once_unless_they_request_persistence() {
        let mut dedup = ReportDedup::default();
        let ready = light_report("ready", false);
        assert!(dedup.should_emit(&ready, false));
        assert!(
            !dedup.should_emit(&ready, false),
            "the host already holds this text"
        );
        let pending = light_report("ready", true);
        assert!(dedup.should_emit(&pending, true));
        assert!(
            dedup.should_emit(&pending, true),
            "a persistence trigger is always delivered"
        );
        assert!(
            dedup.should_emit(&ready, false),
            "a different text is delivered again"
        );
        assert!(!dedup.should_emit(&ready, false));
    }
    #[test]
    fn the_light_report_carries_only_the_phase_and_the_trigger() {
        let value: serde_json::Value = serde_json::from_str(&light_report("ready", true)).unwrap();
        assert_eq!(
            value,
            serde_json::json!({"phase": "ready", "persistenceNeeded": true})
        );
        assert_eq!(
            value.as_object().unwrap().len(),
            2,
            "rows, outbox counts and health come from the full report"
        );
    }
}

#[cfg(test)]
mod report_benchmark {
    use super::*;
    /// A full report of the shape `peer::report` emits for a session desktop
    /// with three admitted records and no failures.
    fn full_report(committed_at: f64) -> serde_json::Value {
        let hex = |seed: u8| {
            (0..32)
                .map(|i| format!("{:02x}", seed.wrapping_mul(31).wrapping_add(i)))
                .collect::<String>()
        };
        let heads = |seed: u8| vec![hex(seed), hex(seed.wrapping_add(7))];
        let record = |app: &str, id: &str, seed: u8| {
            serde_json::json!({
                "record":{"roomId":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa","appId":app,"recordId":id},
                "admission":"admitted","current":{"heads":heads(seed)},"editing":{"mode":"online"},
                "local":{"state":"durable","durableBasis":{"heads":heads(seed)},"recordRevision":{"epoch":hex(1),"revision":"4242"},"journalRevision":{"epoch":hex(1),"revision":"4243"},"pendingWrites":0,"pendingBytes":0,"committedAtMs":committed_at},
                "outgoing":{"pendingOperations":3,"durableOperations":3},
                "acknowledgement":{"state":"covered","targetPeerId":hex(9),"coveredBasis":{"heads":heads(seed)},"coversCurrentBasis":true,"receiptId":hex(seed.wrapping_add(3)),"observedAtMs":committed_at},
                "failureIds":[]
            })
        };
        let rows = |seed: u8| {
            serde_json::json!([
                {"app_id":"so.tilo.word-processor","record_id":"tab:1","heads":heads(seed)},
                {"app_id":"so.tilo.chat","record_id":"primary","heads":heads(seed.wrapping_add(1))},
                {"app_id":"so.tilo.notes","record_id":"tab:3","heads":heads(seed.wrapping_add(2))}
            ])
        };
        serde_json::json!({
            "phase":"ready","desktop":{"version":1,"mode":"session"},"room":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa","deviceId":hex(5),
            "records":rows(11),"pendingPrerequisites":0,"persistenceNeeded":false,"outgoingPending":3,"outgoingBlocked":0,"synchronization":"acknowledged","error":null,
            "health":{"version":1,"transport":{"phase":"connected","authenticated":true,"lastRelayContactAtMs":committed_at,"lastSessionPeerContactAtMs":committed_at},
                "admission":{"phase":"ready","requiredRecords":[{"roomId":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa","appId":"so.tilo.word-processor","recordId":"tab:1"},{"roomId":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa","appId":"so.tilo.chat","recordId":"primary"}],"pendingPrerequisites":0,"deadlineAtMs":null},
                "records":[record("so.tilo.word-processor","tab:1",11),record("so.tilo.chat","primary",12),record("so.tilo.notes","tab:3",13)],
                "capabilities":{"automaticRecovery":true,"retrySaving":true,"recoverRenderer":true,"durablePeerAcknowledgement":true},"failures":[]}
        })
    }
    /// Manual CPU microbenchmark of what the committed and light report paths
    /// spend on the report Value itself, apart from computing health: the
    /// Stage 5 cache stored the full Value (dropping the previous one), and the
    /// light path deep-cloned it, patched three fields and serialized it again.
    #[test]
    #[ignore = "manual CPU microbenchmark, not a browser frame-rate gate"]
    fn report_value_costs() {
        let ms = |start: std::time::Instant| start.elapsed().as_secs_f64() * 1000.0;
        let mut build = Vec::new();
        let mut serialize = Vec::new();
        let mut clone = Vec::new();
        let mut drop_previous = Vec::new();
        let mut old_light = Vec::new();
        let mut new_light = Vec::new();
        let mut hash = Vec::new();
        let mut cached: Option<serde_json::Value>;
        let mut bytes = 0;
        for i in 0..200 {
            let start = std::time::Instant::now();
            let value = full_report(1_700_000_000_000.0 + i as f64);
            build.push(ms(start));
            let start = std::time::Instant::now();
            let text = value.to_string();
            serialize.push(ms(start));
            bytes = text.len();
            let start = std::time::Instant::now();
            let _ = text_hash(&text);
            hash.push(ms(start));
            let start = std::time::Instant::now();
            let copy = value.clone();
            clone.push(ms(start));
            // Stage 5 stored the new Value, which dropped the previous report.
            let start = std::time::Instant::now();
            cached = Some(value);
            drop_previous.push(ms(start));
            drop(copy);
            // Stage 5 light path: clone the cache, patch three fields, serialize everything.
            let start = std::time::Instant::now();
            let mut light = cached.clone().unwrap();
            light["records"] = full_report(0.0)["records"].clone();
            light["persistenceNeeded"] = serde_json::json!(true);
            light["outgoingPending"] = serde_json::json!(4);
            let _ = light.to_string();
            old_light.push(ms(start));
            let start = std::time::Instant::now();
            let _ = light_report("ready", true);
            new_light.push(ms(start));
        }
        let percentiles = |samples: &mut Vec<f64>| {
            samples.sort_by(f64::total_cmp);
            (
                samples[samples.len() / 2],
                samples[samples.len() * 99 / 100],
            )
        };
        let (build_p50, build_p99) = percentiles(&mut build);
        let (serialize_p50, serialize_p99) = percentiles(&mut serialize);
        let (hash_p50, hash_p99) = percentiles(&mut hash);
        let (clone_p50, clone_p99) = percentiles(&mut clone);
        let (drop_p50, drop_p99) = percentiles(&mut drop_previous);
        let (old_p50, old_p99) = percentiles(&mut old_light);
        let (new_p50, new_p99) = percentiles(&mut new_light);
        eprintln!(
            "report Value costs (200 samples, 3 records, {bytes} bytes JSON, native {}): build json! p50={build_p50:.3} p99={build_p99:.3} ms; to_string p50={serialize_p50:.3} p99={serialize_p99:.3} ms; text hash p50={hash_p50:.3} p99={hash_p99:.3} ms; clone p50={clone_p50:.3} p99={clone_p99:.3} ms; store+drop previous p50={drop_p50:.3} p99={drop_p99:.3} ms; stage-5 light (clone+patch+to_string) p50={old_p50:.3} p99={old_p99:.3} ms; minimal light p50={new_p50:.3} p99={new_p99:.3} ms",
            if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            }
        );
    }
}
