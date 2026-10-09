//! Bounded, content-free witnesses for native palette input and actual drawing.
//!
//! Submission is not presentation. Only the independent presenter retirement
//! with this exact pipe serial and scene revision can establish a KMS outcome.
use serde_json::{Value, json};
use uuid::Uuid;

const MAX_INPUTS: usize = 32;
const MAX_EVENTS: u64 = 2048;
const MAX_SNAPSHOT_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy)]
pub(crate) struct InputIdentity {
    pub session: Uuid,
    pub id: u64,
    pub kind: &'static str,
    pub input_ns: u64,
    pub dispatch_ns: u128,
}

pub(crate) struct Input {
    pub identity: InputIdentity,
    before: Value,
}

pub(crate) struct PaletteTrace {
    session: Uuid,
    next_id: u64,
    pending: Vec<u64>,
    submitted: bool,
    last_drawn: Option<Value>,
}

impl Default for PaletteTrace {
    fn default() -> Self {
        Self {
            session: Uuid::new_v4(),
            next_id: 1,
            pending: Vec::new(),
            submitted: false,
            last_drawn: None,
        }
    }
}

fn live(mut state: Value) -> Option<Value> {
    state.as_object_mut()?.remove("rendered");
    (serde_json::to_vec(&state).ok()?.len() <= MAX_SNAPSHOT_BYTES).then_some(state)
}

impl PaletteTrace {
    pub(crate) fn reset(&mut self) {
        self.pending.clear();
        self.last_drawn = None;
        self.submitted = false;
    }

    pub(crate) fn begin(
        &mut self,
        kind: &'static str,
        input_ns: u64,
        before: Value,
        dispatch_ns: u128,
    ) -> Option<Input> {
        if self.next_id > MAX_EVENTS || u128::from(input_ns) > dispatch_ns {
            return None;
        }
        let before = live(before)?;
        let identity = InputIdentity {
            session: self.session,
            id: self.next_id,
            kind,
            input_ns,
            dispatch_ns,
        };
        Some(Input { identity, before })
    }

    pub(crate) fn finish(&mut self, input: Input, after: Value, end_ns: u128) -> Option<Value> {
        let after = live(after)?;
        if (input.before["visible"] != true && after["visible"] != true)
            || input.before == after
            || end_ns < input.identity.dispatch_ns
        {
            return None;
        }
        self.next_id += 1;
        // Retain a cause across later background result publication. A new
        // physical change replaces it only after a scene was submitted; input
        // changes coalesced before the first scene stay explicitly multiple.
        if self.submitted {
            self.pending.clear();
            self.last_drawn = None;
            self.submitted = false;
        }
        if self.pending.len() >= MAX_INPUTS {
            self.reset();
            return None;
        }
        self.pending.push(input.identity.id);
        Some(json!({
            "schema": 1, "clock_id": libc::CLOCK_MONOTONIC,
            "process_id": std::process::id(), "observer_session": self.session,
            "id": input.identity.id, "kind": input.identity.kind,
            "input_time_ns": input.identity.input_ns,
            "dispatch_start_ns": input.identity.dispatch_ns, "dispatch_end_ns": end_ns,
            "before": input.before, "after": after,
        }))
    }

    pub(crate) fn frame(
        &mut self,
        serial: u64,
        revision: u64,
        drawn: Value,
        draw_ns: u128,
        submit_ns: u128,
    ) -> Option<Value> {
        if serial == 0
            || revision == 0
            || self.pending.is_empty()
            || draw_ns > submit_ns
            || drawn["visible"] != true
            || drawn["mounted"] != true
            || serde_json::to_vec(&drawn).ok()?.len() > MAX_SNAPSHOT_BYTES
            || self.last_drawn.as_ref() == Some(&drawn)
        {
            return None;
        }
        self.last_drawn = Some(drawn.clone());
        self.submitted = true;
        Some(json!({
            "schema": 1, "clock_id": libc::CLOCK_MONOTONIC,
            "process_id": std::process::id(), "observer_session": self.session,
            "pipe_serial": serial, "scene_revision": revision, "input_ids": self.pending,
            "draw_time_ns": draw_ns, "submit_time_ns": submit_ns, "state": drawn,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(visible: bool, hover: &str) -> Value {
        json!({"visible":visible,"mounted":visible,"hovered_entry_id":hover,
               "rendered":{"private":"must not leave live witness"}})
    }

    #[test]
    fn native_palette_witness_retains_coalesced_changes_and_distinct_submission() {
        let mut trace = PaletteTrace::default();
        let first = trace
            .begin("physical_key", 10, snapshot(false, ""), 20)
            .unwrap();
        let first = trace.finish(first, snapshot(true, ""), 30).unwrap();
        let next = trace
            .begin("physical_pointer_motion", 40, snapshot(true, ""), 50)
            .unwrap();
        let next = trace.finish(next, snapshot(true, "result:1"), 60).unwrap();
        assert!(first["before"].get("rendered").is_none());
        let drawn = snapshot(true, "result:1");
        let frame = trace.frame(7, 9, drawn.clone(), 70, 80).unwrap();
        assert_eq!(frame["input_ids"], json!([first["id"], next["id"]]));
        assert_eq!(frame["pipe_serial"], 7);
        assert_eq!(frame["scene_revision"], 9);
        assert!(frame.get("presented_time_ns").is_none());
        assert!(trace.frame(8, 10, drawn, 90, 100).is_none());
    }

    #[test]
    fn native_palette_witness_retains_async_publication_and_replaces_on_next_input() {
        let mut trace = PaletteTrace::default();
        let input = trace
            .begin("physical_key", 10, snapshot(false, ""), 20)
            .unwrap();
        trace.finish(input, snapshot(true, ""), 30).unwrap();
        let early = trace.frame(1, 1, snapshot(true, ""), 40, 50).unwrap();
        let ready = trace
            .frame(2, 2, snapshot(true, "result:1"), 60, 70)
            .unwrap();
        assert_eq!(early["input_ids"], ready["input_ids"]);
        let input = trace
            .begin(
                "physical_pointer_motion",
                80,
                snapshot(true, "result:1"),
                90,
            )
            .unwrap();
        let event = trace
            .finish(input, snapshot(true, "result:2"), 100)
            .unwrap();
        assert_eq!(
            trace
                .frame(3, 3, snapshot(true, "result:2"), 110, 120)
                .unwrap()["input_ids"],
            json!([event["id"]])
        );
    }

    #[test]
    fn native_palette_witness_refuses_missing_draw_unstamped_or_unchanged_work() {
        let mut trace = PaletteTrace::default();
        assert!(
            trace
                .begin("physical_key", 30, snapshot(true, ""), 20)
                .is_none()
        );
        let input = trace
            .begin("physical_key", 10, snapshot(true, ""), 20)
            .unwrap();
        assert!(trace.finish(input, snapshot(true, ""), 30).is_none());
        assert!(trace.frame(1, 1, snapshot(true, ""), 40, 50).is_none());
        let input = trace
            .begin("physical_key", 60, snapshot(false, ""), 70)
            .unwrap();
        trace.finish(input, snapshot(true, ""), 80).unwrap();
        assert!(trace.frame(0, 1, snapshot(true, ""), 90, 100).is_none());
        assert!(trace.frame(1, 1, Value::Null, 90, 100).is_none());
        assert!(trace.frame(1, 1, snapshot(true, ""), 110, 100).is_none());
        trace.reset();
        assert!(trace.frame(1, 1, snapshot(true, ""), 90, 100).is_none());
    }
}
