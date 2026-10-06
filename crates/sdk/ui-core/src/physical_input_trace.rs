//! Content-free evidence from ordinary native platform input and drawn frames.
//! A drawn frame is not display completion: only matching presentation feedback
//! can close the timeline. Missing IDs/state/feedback never qualify a measurement.
use serde_json::Value;
use winit::event::{ElementState, Ime, WindowEvent};

#[derive(Default)]
pub struct Trace {
    next_id: u64,
    pending: Vec<u64>,
}

impl Trace {
    pub(crate) fn requested(profile: Option<&str>) -> bool {
        profile == Some("1")
    }

    pub(crate) fn observes(event: &WindowEvent) -> bool {
        matches!(event,
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed
        ) || matches!(event, WindowEvent::Ime(Ime::Commit(_)))
    }

    pub(crate) fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    pub fn begin(&mut self, event: &WindowEvent, snapshot: Option<Value>) -> Option<Value> {
        let kind = match event {
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                "key_pressed"
            }
            WindowEvent::Ime(Ime::Commit(_)) => "ime_commit",
            _ => return None,
        };
        let stamp = tilo_presentation::key_stamp(event);
        self.next_id = self.next_id.checked_add(1)?;
        let id = self.next_id;
        self.pending.push(id);
        let (_, time) = crate::native_scenario::host_clock::sample();
        Some(serde_json::json!({
            "id": id, "kind": kind, "process_id": std::process::id(), "observer_session": crate::native_scenario::observer_session_id(),
            "wayland_serial": stamp.map(|stamp| stamp.0),
            "wayland_time_ms": stamp.map(|stamp| stamp.1),
            "clock": crate::native_scenario::host_clock::CLOCK_NAME,
            "dispatch_lo_s": time.map(|time| time.lo_s),
            "dispatch_hi_s": time.map(|time| time.hi_s),
            "before": safe_snapshot(snapshot),
        }))
    }

    pub fn effect(input: Value, snapshot: Option<Value>) -> Value {
        let mut row = input;
        row["after"] = safe_snapshot(snapshot);
        row
    }

    pub fn drawn(&mut self, cpu_frame_id: Option<u64>, snapshot: Option<Value>) -> Option<Value> {
        if self.pending.is_empty() {
            return None;
        }
        Some(serde_json::json!({
            "cpu_frame_id": cpu_frame_id, "process_id": std::process::id(), "observer_session": crate::native_scenario::observer_session_id(),
            "input_ids": std::mem::take(&mut self.pending),
            "state": safe_snapshot(snapshot),
        }))
    }
}

// Existing app snapshots may contain names or paths. This physical-input
// observer carries only the declared content-free Notes state evidence.
fn safe_snapshot(snapshot: Option<Value>) -> Value {
    let Some(snapshot) = snapshot else {
        return Value::Null;
    };
    if snapshot["app"] == "so.tilo.chat" {
        let agent_task = snapshot
            .get("agent_task")
            .map_or(Some(Value::Null), safe_chat_agent_task)
            .unwrap_or(Value::Null);
        return serde_json::json!({
            "app": "so.tilo.chat",
            "agent_task": agent_task,
        });
    }
    if snapshot["app"] != "so.tilo.notes" {
        return Value::Null;
    }
    let mut safe = serde_json::Map::new();
    for key in [
        "app",
        "editor_id",
        "selected_page",
        "bound_page",
        "main_page",
        "editor_binding_page",
        "pending_page_events",
        "adopted",
        "hash_algorithm",
        "body_hash",
        "body_chars",
        "body_bytes",
        "rendered_body_hash",
        "rendered_body_chars",
        "editor_focused",
        "editor_rect",
    ] {
        if let Some(value) = snapshot.get(key) {
            safe.insert(key.into(), value.clone());
        }
    }
    if let Some(caret) = snapshot.get("caret") {
        let mut safe_caret = serde_json::Map::new();
        for key in ["block_id", "block_index", "byte_offset"] {
            if let Some(value) = caret.get(key) {
                safe_caret.insert(key.into(), value.clone());
            }
        }
        safe.insert("caret".into(), Value::Object(safe_caret));
    }
    Value::Object(safe)
}

fn safe_chat_agent_task(value: &Value) -> Option<Value> {
    if value.is_null() {
        return Some(Value::Null);
    }
    let task = value.as_object()?;
    let task_id = task.get("task_id")?.as_str()?;
    let uuid = task_id.strip_prefix("task:")?;
    let parsed_task_id = uuid::Uuid::parse_str(uuid).ok()?;
    if parsed_task_id.is_nil() || parsed_task_id.hyphenated().to_string() != uuid {
        return None;
    }
    let run_epoch = task.get("run_epoch")?.as_u64()?;
    let execution = match task.get("execution") {
        None | Some(Value::Null) => Value::Null,
        Some(value) => {
            let execution = value.as_object()?;
            let execution_epoch = execution.get("run_epoch")?.as_u64()?;
            let sequence = execution.get("sequence")?.as_u64()?;
            let phase = execution.get("phase")?.as_str()?;
            if !matches!(phase, "accepted" | "planning" | "executing") {
                return None;
            }
            serde_json::json!({
                "run_epoch": execution_epoch,
                "sequence": sequence,
                "phase": phase,
            })
        }
    };
    Some(serde_json::json!({
        "task_id": task_id,
        "run_epoch": run_epoch,
        "execution": execution,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observer_requires_the_exact_fixed_presentation_flag() {
        assert!(Trace::requested(Some("1")));
        for value in [None, Some(""), Some("0"), Some("true"), Some("1\n")] {
            assert!(!Trace::requested(value));
        }
    }

    #[test]
    fn chat_trace_keeps_only_the_content_free_owner_task_frontier() {
        let safe = safe_snapshot(Some(serde_json::json!({
            "app": "so.tilo.chat",
            "agent_task": {
                "task_id": "task:00000000-0000-4000-8000-000000000007",
                "run_epoch": 7,
                "execution": {
                    "run_epoch": 7,
                    "sequence": 3,
                    "phase": "planning",
                    "instruction": "nested private value"
                },
                "title": "private title",
                "instruction": "never export this either"
            },
            "agent_task_presentation": {"pending_count": 2},
            "private_draft": "never export this",
        })));

        assert_eq!(safe["app"], "so.tilo.chat");
        assert_eq!(
            safe["agent_task"]["task_id"],
            "task:00000000-0000-4000-8000-000000000007"
        );
        assert_eq!(safe["agent_task"]["execution"]["phase"], "planning");
        assert!(safe["agent_task"].get("title").is_none());
        assert!(safe["agent_task"].get("instruction").is_none());
        assert!(safe["agent_task"]["execution"].get("instruction").is_none());
        assert!(safe.get("agent_task_presentation").is_none());
        assert!(safe.get("private_draft").is_none());
    }

    #[test]
    fn chat_trace_rejects_malformed_or_unknown_task_frontiers() {
        for agent_task in [
            serde_json::json!({
                "task_id": "task:00000000-0000-4000-8000-000000000007",
                "run_epoch": "7"
            }),
            serde_json::json!({
                "task_id": "task:00000000-0000-4000-8000-000000000007",
                "run_epoch": 7,
                "execution": {"run_epoch": 7, "sequence": 3, "phase": "private"}
            }),
            serde_json::json!({
                "task_id": "private instructions masquerading as an opaque id",
                "run_epoch": 7,
                "execution": null
            }),
            serde_json::json!({
                "task_id": "task:00000000-0000-0000-0000-000000000000",
                "run_epoch": 7,
                "execution": null
            }),
            serde_json::json!({
                "task_id": "00000000-0000-4000-8000-000000000007",
                "run_epoch": 7,
                "execution": null
            }),
            serde_json::json!({
                "task_id": "task:00000000-0000-4000-8000-000000000007:private",
                "run_epoch": 7,
                "execution": null
            }),
            serde_json::json!(["task", 7]),
        ] {
            let safe = safe_snapshot(Some(serde_json::json!({
                "app": "so.tilo.chat",
                "agent_task": agent_task,
            })));
            assert_eq!(safe["agent_task"], Value::Null);
        }
    }
}
