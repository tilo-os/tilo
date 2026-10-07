//! Transient, content-free native broker delivery and draw correlation.
//! An invocation UUID is correlation only; this observer grants no authority.
use crate::time::{Duration, Instant};
use serde_json::Value;
use tilo_actions::InvocationId;

#[derive(Default)]
pub(crate) struct Trace {
    pending: Vec<InvocationId>,
    drawn: Vec<InvocationId>,
    frames: u8,
    until: Option<Instant>,
}

impl Trace {
    pub(crate) fn has_pending(&self) -> bool {
        !self.pending.is_empty()
            || (self.frames > 0 && self.until.is_some_and(|until| crate::time::now() < until))
    }

    pub(crate) fn begin(
        &mut self,
        invocation: InvocationId,
        snapshot: Option<Value>,
    ) -> Option<Value> {
        let before = settings(snapshot)?;
        if self.pending.contains(&invocation) || self.pending.len() >= 64 {
            return None;
        }
        self.pending.push(invocation);
        let (_, clock) = crate::native_scenario::host_clock::sample();
        Some(serde_json::json!({
            "invocation": invocation, "process_id": std::process::id(),
            "observer_session": crate::native_scenario::observer_session_id(),
            "clock": crate::native_scenario::host_clock::CLOCK_NAME,
            "handler_dispatch_lo_s": clock.map(|sample| sample.lo_s),
            "handler_dispatch_hi_s": clock.map(|sample| sample.hi_s),
            "before": before,
        }))
    }

    pub(crate) fn effect(
        &mut self,
        mut row: Value,
        snapshot: Option<Value>,
        entered: bool,
    ) -> Value {
        let (_, clock) = crate::native_scenario::host_clock::sample();
        row["handler_returned_ok"] = Value::Bool(entered);
        row["handler_return_lo_s"] = serde_json::json!(clock.map(|sample| sample.lo_s));
        row["handler_return_hi_s"] = serde_json::json!(clock.map(|sample| sample.hi_s));
        row["after"] = settings(snapshot).unwrap_or(Value::Null);
        if !entered
            && let Ok(id) = serde_json::from_value::<InvocationId>(row["invocation"].clone())
        {
            self.pending.retain(|pending| *pending != id);
        }
        row
    }

    pub(crate) fn drawn(
        &mut self,
        cpu_frame_id: Option<u64>,
        snapshot: Option<Value>,
    ) -> Option<Value> {
        let state = settings(snapshot)?;
        let cpu_frame_id = cpu_frame_id?;
        if !self.pending.is_empty() {
            self.drawn = std::mem::take(&mut self.pending);
            self.frames = 32;
            self.until = crate::time::now().checked_add(Duration::from_secs(2));
        }
        if self.frames == 0 || self.until.is_none_or(|until| crate::time::now() >= until) {
            return None;
        }
        self.frames = self.frames.saturating_sub(1);
        Some(serde_json::json!({
            "invocations": self.drawn, "cpu_frame_id": cpu_frame_id,
            "process_id": std::process::id(),
            "observer_session": crate::native_scenario::observer_session_id(),
            "state": state,
        }))
    }
}

fn settings(snapshot: Option<Value>) -> Option<Value> {
    let value = crate::physical_input_trace::safe_snapshot(snapshot);
    (value["app"] == "so.tilo.settings").then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(rendered: bool) -> Option<Value> {
        let navigation = serde_json::json!({
            "selected_pane":"network", "requested_pane":"network",
            "pending":false, "ready":true, "content_visible":true,
            "narrow":false, "sidebar_open":true,
            "query":"private search", "label":"private label",
        });
        Some(serde_json::json!({
            "app":"so.tilo.settings", "navigation":navigation,
            "rendered_navigation":if rendered { navigation } else { Value::Null },
            "password":"private credential",
        }))
    }

    #[test]
    fn broker_trace_preserves_invocation_and_only_actual_drawn_navigation() {
        let mut trace = Trace::default();
        let invocation = InvocationId::new();
        let dispatch = trace.begin(invocation, snapshot(false)).unwrap();
        assert_eq!(dispatch["invocation"], serde_json::json!(invocation));
        assert!(dispatch["before"].get("rendered_navigation").is_none());
        let returned = trace.effect(dispatch, snapshot(false), true);
        assert_eq!(returned["handler_returned_ok"], true);
        assert!(returned["after"].get("rendered_navigation").is_none());
        assert!(trace.drawn(None, snapshot(true)).is_none());
        assert_eq!(trace.pending, vec![invocation]);
        let undrawn = trace.drawn(Some(17), snapshot(false)).unwrap();
        assert!(undrawn["state"].get("rendered_navigation").is_none());
        let drawn = trace.drawn(Some(18), snapshot(true)).unwrap();
        assert_eq!(drawn["cpu_frame_id"], 18);
        assert_eq!(drawn["invocations"], serde_json::json!([invocation]));
        assert_eq!(
            drawn["state"]["rendered_navigation"]["path"],
            "settings://network"
        );
        for row in [returned, undrawn, drawn] {
            assert!(!row.to_string().contains("private"));
            assert!(row.get("presented").is_none());
        }
    }

    #[test]
    fn broker_trace_refusals_duplicates_and_flood_remain_bounded() {
        let mut trace = Trace::default();
        let refused = InvocationId::new();
        let row = trace.begin(refused, snapshot(false)).unwrap();
        assert!(trace.begin(refused, snapshot(false)).is_none());
        trace.effect(row, snapshot(false), false);
        assert!(trace.drawn(Some(1), snapshot(true)).is_none());
        assert!(
            trace
                .begin(
                    InvocationId::new(),
                    Some(serde_json::json!({"app":"unknown"}))
                )
                .is_none()
        );
        for _ in 0..64 {
            assert!(trace.begin(InvocationId::new(), snapshot(false)).is_some());
        }
        for _ in 0..256 {
            assert!(trace.begin(InvocationId::new(), snapshot(false)).is_none());
        }
        assert_eq!(trace.pending.len(), 64);
        for frame in 1..=32 {
            assert_eq!(
                trace.drawn(Some(frame), snapshot(true)).unwrap()["invocations"]
                    .as_array()
                    .unwrap()
                    .len(),
                64
            );
        }
        assert!(trace.drawn(Some(33), snapshot(true)).is_none());
    }
}
