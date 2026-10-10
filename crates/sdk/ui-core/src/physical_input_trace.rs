//! Content-free evidence from ordinary native platform input and drawn frames.
//! A drawn frame is not display completion: only matching presentation feedback
//! can close the timeline. Missing IDs/state/feedback never qualify a measurement.
use serde_json::Value;
use winit::event::{ElementState, Ime, MouseButton, WindowEvent};

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
            || matches!(
                event,
                WindowEvent::MouseInput {
                    state: ElementState::Released,
                    button: MouseButton::Left,
                    ..
                }
            )
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
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } if snapshot
                .as_ref()
                .is_some_and(|value| value["app"] == "so.tilo.chat") =>
            {
                "pointer_released"
            }
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

    /// A genuine model poll, not input, drawing or presentation completion.
    pub fn polled(snapshot: Option<Value>) -> Value {
        serde_json::json!({"state": safe_snapshot(snapshot)})
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
// observer carries only declared content-free Notes and Chat state evidence.
fn safe_snapshot(snapshot: Option<Value>) -> Value {
    let Some(snapshot) = snapshot else {
        return Value::Null;
    };
    if snapshot["app"] == "so.tilo.chat" {
        let agent_task = snapshot
            .get("agent_task")
            .map_or(Some(Value::Null), safe_chat_agent_task)
            .unwrap_or(Value::Null);
        let mut safe = serde_json::json!({
            "app": "so.tilo.chat",
            "agent_task": agent_task,
            "agent_message_progress": snapshot.get("agent_message_progress")
                .and_then(safe_chat_message_progress).unwrap_or(Value::Null),
            "agent_message_submission": snapshot.get("agent_message_submission")
                .and_then(safe_chat_message_submission).unwrap_or(Value::Null),
            "agent_float_frontier": snapshot.get("agent_float_frontier")
                .and_then(safe_chat_float_frontier).unwrap_or(Value::Null),
        });
        if let Some(control) = snapshot.get("keyboard_control").and_then(Value::as_str)
            && matches!(
                control,
                "none" | "send" | "return" | "float" | "editor" | "other"
            )
        {
            safe["keyboard_control"] = control.into();
        }
        if let Some(focused) = snapshot.get("composer_focused").and_then(Value::as_bool) {
            safe["composer_focused"] = focused.into();
        }
        if let Some(poll) = snapshot
            .get("agent_message_submission")
            .and_then(safe_chat_message_poll)
        {
            safe["agent_message_poll"] = poll;
        }
        return safe;
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

fn safe_chat_message_progress(value: &Value) -> Option<Value> {
    let rows = value.as_array()?;
    if rows.len() > 32 {
        return None;
    }
    let mut originals = std::collections::BTreeSet::new();
    let safe = rows
        .iter()
        .map(|row| {
            let original = row.get("original_invocation")?.as_str()?;
            let request_row = row.get("request_row")?.as_str()?;
            runtime_collab::validate_agent_message_progress_query(original, request_row).ok()?;
            if !originals.insert(original) {
                return None;
            }
            let unavailable = row.get("unavailable")?.as_bool()?;
            let phase = row.get("phase")?;
            let generation = row.get("generation")?;
            let mut result = serde_json::json!({
                "original_invocation": original,
                "request_row": request_row,
                "unavailable": unavailable,
                "phase": null,
                "generation": null,
            });
            if phase.is_null() {
                if !generation.is_null() || row.get("reply_row").is_some_and(|v| !v.is_null()) {
                    return None;
                }
            } else {
                let phase = phase.as_str()?;
                let generation = generation.as_u64()?;
                if generation == 0
                    || !matches!(
                        phase,
                        "waiting_for_reply" | "publishing" | "needs_attention" | "committed"
                    )
                {
                    return None;
                }
                if phase == "committed" {
                    let reply_row = row.get("reply_row")?.as_str()?;
                    runtime_collab::validate_agent_message_public_progress(
                        &runtime_collab::AgentMessagePublicProgress {
                            generation,
                            phase: runtime_collab::AgentMessagePublicPhase::Committed {
                                reply_row: reply_row.into(),
                            },
                        },
                    )
                    .ok()?;
                    result["reply_row"] = Value::String(reply_row.into());
                } else if row.get("reply_row").is_some_and(|v| !v.is_null()) {
                    return None;
                }
                result["phase"] = Value::String(phase.into());
                result["generation"] = Value::from(generation);
            }
            Some(result)
        })
        .collect::<Option<Vec<_>>>()?;
    Some(Value::Array(safe))
}

fn safe_chat_message_poll(value: &Value) -> Option<Value> {
    let phase = value.get("phase")?.as_str()?;
    let count = value.get("pending_count")?.as_u64()?;
    if count > 64
        || !matches!(
            phase,
            "providers"
                | "launch_rows"
                | "launch_confirm"
                | "launch_invoke"
                | "await_publisher"
                | "rows"
                | "confirm"
                | "invoke"
                | "uninitialized"
        )
    {
        return None;
    }
    Some(serde_json::json!({
        "phase": phase,
        "pending_count": count,
        "poll_scheduled": value.get("poll_scheduled")?.as_bool()?,
        "poll_due": value.get("poll_due")?.as_bool()?,
        "resolve_expired": value.get("resolve_expired")?.as_bool()?,
    }))
}

fn safe_chat_message_submission(value: &Value) -> Option<Value> {
    Some(serde_json::json!({
        "draft_revision": value.get("draft_revision")?.as_u64()?,
        "current_revision": value.get("current_revision")?.as_u64()?,
        "draft_empty": value.get("draft_empty")?.as_bool()?,
    }))
}

fn safe_chat_float_frontier(value: &Value) -> Option<Value> {
    const BOOLEAN_FIELDS: [&str; 6] = [
        "context_matches_active_workspace",
        "context_workspace_present",
        "current_agent_present",
        "current_agent_admitted",
        "retained_surface_present",
        "retained_attachment_app",
    ];
    let object = value.as_object()?;
    if object.len() != BOOLEAN_FIELDS.len() + 1 {
        return None;
    }
    let mut safe = serde_json::Map::new();
    for field in BOOLEAN_FIELDS {
        safe.insert(field.into(), Value::Bool(object.get(field)?.as_bool()?));
    }
    let pending = object.get("pending_present_request_count")?.as_u64()?;
    if pending > 64 {
        return None;
    }
    safe.insert("pending_present_request_count".into(), pending.into());
    Some(Value::Object(safe))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message_progress() -> Value {
        serde_json::json!({
            "original_invocation": "00000000-0000-4000-8000-000000000007",
            "request_row": "m_00000000000040008000000000000007",
            "generation": 1,
            "phase": "waiting_for_reply",
            "unavailable": false,
            "private_output": "must never leave the app",
        })
    }

    fn float_frontier() -> Value {
        serde_json::json!({
            "context_matches_active_workspace": true,
            "context_workspace_present": true,
            "current_agent_present": true,
            "current_agent_admitted": true,
            "retained_surface_present": true,
            "retained_attachment_app": true,
            "pending_present_request_count": 1,
        })
    }

    #[test]
    fn pointer_observation_is_chat_only_and_uses_the_actual_release() {
        let released = WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(),
            state: ElementState::Released,
            button: MouseButton::Left,
        };
        assert!(Trace::observes(&released));
        let mut trace = Trace::default();
        assert!(
            trace
                .begin(&released, Some(serde_json::json!({"app": "so.tilo.notes"})))
                .is_none()
        );
        assert!(trace.begin(&released, None).is_none());
        let input = trace
            .begin(&released, Some(serde_json::json!({"app": "so.tilo.chat"})))
            .unwrap();
        assert_eq!(input["kind"], "pointer_released");
        assert!(input["wayland_serial"].is_null());
        let effect = Trace::effect(
            input,
            Some(serde_json::json!({
                "app": "so.tilo.chat", "agent_message_submission": {
                    "draft_revision": 3, "current_revision": 3, "draft_empty": false,
                }
            })),
        );
        assert_eq!(
            effect["after"]["agent_message_submission"]["draft_revision"],
            3
        );
        assert!(trace.drawn(Some(1), None).is_some());
        assert!(!trace.has_pending());
        let pressed = WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        };
        assert!(!Trace::observes(&pressed));
    }

    #[test]
    fn chat_message_trace_reconstructs_only_closed_public_milestones() {
        let safe = safe_snapshot(Some(serde_json::json!({
            "app": "so.tilo.chat",
            "agent_message_progress": [message_progress()],
            "agent_message_submission": {
                "draft_revision": 3, "current_revision": 4, "draft_empty": false,
                "text": "private draft"
            },
        })));
        let progress = &safe["agent_message_progress"][0];
        assert_eq!(progress["phase"], "waiting_for_reply");
        assert_eq!(progress["generation"], 1);
        assert!(progress.get("private_output").is_none());
        assert_eq!(safe["agent_message_submission"]["current_revision"], 4);
        assert!(safe["agent_message_submission"].get("text").is_none());
        let mut committed = message_progress();
        committed["phase"] = Value::String("committed".into());
        committed["reply_row"] = Value::String("m_00000000000040008000000000000008".into());
        assert!(safe_chat_message_progress(&serde_json::json!([committed])).is_some());
        let mut unknown = message_progress();
        unknown["phase"] = Value::Null;
        unknown["generation"] = Value::Null;
        unknown["unavailable"] = Value::Bool(true);
        assert!(safe_chat_message_progress(&serde_json::json!([unknown])).is_some());
    }

    #[test]
    fn chat_input_effect_preserves_closed_poll_evidence_without_changing_revision_shape() {
        for phase in [
            "providers",
            "launch_rows",
            "launch_confirm",
            "launch_invoke",
            "await_publisher",
            "rows",
            "confirm",
            "invoke",
            "uninitialized",
        ] {
            for control in ["none", "send", "return", "float", "editor", "other"] {
                let effect = Trace::effect(
                    serde_json::json!({}),
                    Some(serde_json::json!({
                        "app": "so.tilo.chat", "keyboard_control": control, "composer_focused": true,
                        "agent_message_submission": {
                            "phase": phase, "pending_count": 64, "poll_scheduled": true,
                            "poll_due": false, "resolve_expired": false,
                            "draft_revision": 3, "current_revision": 4, "draft_empty": false,
                            "text": "PRIVATE-CANARY", "task_id": "PRIVATE-ID",
                        },
                    })),
                );
                let safe = &effect["after"];
                assert_eq!(safe["keyboard_control"], control);
                assert_eq!(safe["composer_focused"], true);
                assert_eq!(
                    safe["agent_message_poll"],
                    serde_json::json!({
                        "phase": phase, "pending_count": 64, "poll_scheduled": true,
                        "poll_due": false, "resolve_expired": false,
                    })
                );
                assert_eq!(
                    safe["agent_message_submission"],
                    serde_json::json!({
                        "draft_revision": 3, "current_revision": 4, "draft_empty": false,
                    })
                );
                assert!(!effect.to_string().contains("PRIVATE"));
            }
        }
    }

    #[test]
    fn chat_input_effect_drops_private_or_malformed_poll_fields() {
        let snapshot = serde_json::json!({
            "app": "so.tilo.chat", "keyboard_control": "send", "composer_focused": true,
            "agent_message_submission": {
                "phase": "rows", "pending_count": 1, "poll_scheduled": true,
                "poll_due": false, "resolve_expired": false,
                "draft_revision": 3, "current_revision": 4, "draft_empty": false,
            },
        });
        for (key, invalid) in [
            ("phase", serde_json::json!("PRIVATE-PHASE")),
            ("phase", Value::Null),
            ("pending_count", serde_json::json!(65)),
            ("pending_count", serde_json::json!(u64::MAX)),
            ("pending_count", serde_json::json!(-1)),
            ("pending_count", serde_json::json!(1.5)),
            ("pending_count", serde_json::json!(true)),
            ("poll_scheduled", serde_json::json!("PRIVATE-FLAG")),
            ("poll_due", serde_json::json!(1)),
            ("resolve_expired", Value::Null),
        ] {
            let mut changed = snapshot.clone();
            changed["agent_message_submission"][key] = invalid;
            let effect = Trace::effect(serde_json::json!({}), Some(changed));
            assert!(effect["after"].get("agent_message_poll").is_none());
            assert_eq!(
                effect["after"]["agent_message_submission"]
                    .as_object()
                    .unwrap()
                    .len(),
                3
            );
            assert!(!effect.to_string().contains("PRIVATE"));
        }
        for invalid in [
            serde_json::json!("PRIVATE-CONTROL"),
            serde_json::json!(7),
            Value::Null,
        ] {
            let mut changed = snapshot.clone();
            changed["keyboard_control"] = invalid.clone();
            changed["composer_focused"] = invalid;
            let effect = Trace::effect(serde_json::json!({}), Some(changed));
            assert!(effect["after"].get("keyboard_control").is_none());
            assert!(effect["after"].get("composer_focused").is_none());
            assert!(!effect.to_string().contains("PRIVATE"));
        }
    }

    #[test]
    fn chat_float_frontier_is_closed_strict_and_bounded() {
        let snapshot = serde_json::json!({
            "app": "so.tilo.chat",
            "keyboard_control": "float",
            "agent_float_frontier": float_frontier(),
        });
        let safe = safe_snapshot(Some(snapshot.clone()));
        assert_eq!(safe["agent_float_frontier"], float_frontier());

        for (field, invalid) in [
            ("context_matches_active_workspace", serde_json::json!(1)),
            ("context_workspace_present", Value::Null),
            ("current_agent_present", serde_json::json!("PRIVATE")),
            ("current_agent_admitted", serde_json::json!(0)),
            ("retained_surface_present", serde_json::json!([])),
            ("retained_attachment_app", serde_json::json!({})),
            ("pending_present_request_count", serde_json::json!(65)),
        ] {
            let mut changed = snapshot.clone();
            changed["agent_float_frontier"][field] = invalid;
            let safe = safe_snapshot(Some(changed));
            assert!(safe["agent_float_frontier"].is_null());
            assert!(!safe.to_string().contains("PRIVATE"));
        }

        let mut with_private = snapshot;
        with_private["agent_float_frontier"]["private_agent"] = serde_json::json!("PRIVATE-NAME");
        let safe = safe_snapshot(Some(with_private));
        assert!(safe["agent_float_frontier"].is_null());
        assert!(!safe.to_string().contains("PRIVATE"));
    }

    #[test]
    fn silent_poll_uses_identical_closed_snapshot_without_consuming_input_or_claiming_frames() {
        let snapshot = serde_json::json!({
            "app": "so.tilo.chat", "keyboard_control": "send", "composer_focused": true,
            "agent_message_submission": {
                "phase": "rows", "pending_count": 1, "poll_scheduled": true,
                "poll_due": false, "resolve_expired": false,
                "draft_revision": 3, "current_revision": 4, "draft_empty": false,
                "text": "PRIVATE-CANARY", "task_id": "PRIVATE-ID",
            },
        });
        let mut trace = Trace::default();
        trace.pending.push(7);
        let polled = Trace::polled(Some(snapshot.clone()));
        let effect = Trace::effect(serde_json::json!({}), Some(snapshot));
        assert_eq!(polled["state"], effect["after"]);
        assert_eq!(polled.as_object().unwrap().len(), 1);
        assert!(polled.get("cpu_frame_id").is_none());
        assert!(polled.get("input_ids").is_none());
        assert!(trace.has_pending());
        assert_eq!(trace.pending, [7]);
        assert!(!polled.to_string().contains("PRIVATE"));
        assert!(Trace::polled(None)["state"].is_null());
        let mut invalid = effect["after"].clone();
        invalid["agent_message_submission"]["phase"] = "PRIVATE-PHASE".into();
        invalid["agent_message_submission"]["pending_count"] = serde_json::json!(u64::MAX);
        let refused = Trace::polled(Some(invalid));
        assert!(refused["state"].get("agent_message_poll").is_none());
        assert!(!refused.to_string().contains("PRIVATE"));
    }

    #[test]
    fn chat_message_trace_rejects_malformed_duplicate_or_unbounded_evidence() {
        for (key, value) in [
            ("original_invocation", serde_json::json!("private")),
            (
                "request_row",
                serde_json::json!("m_00000000000040008000000000000008"),
            ),
            ("phase", serde_json::json!("thinking private output")),
            ("generation", serde_json::json!(0)),
            ("generation", Value::Null),
            ("unavailable", serde_json::json!("false")),
            ("reply_row", serde_json::json!("private output")),
        ] {
            let mut row = message_progress();
            row[key] = value;
            assert!(safe_chat_message_progress(&serde_json::json!([row])).is_none());
        }
        assert!(
            safe_chat_message_progress(&serde_json::json!([
                message_progress(),
                message_progress()
            ]))
            .is_none()
        );
        let bounded: Vec<_> = (1..=33)
            .map(|index| {
                let original = uuid::Uuid::from_u128(index);
                let mut row = message_progress();
                row["original_invocation"] = serde_json::json!(original.to_string());
                row["request_row"] = serde_json::json!(format!("m_{}", original.simple()));
                row
            })
            .collect();
        assert!(safe_chat_message_progress(&serde_json::json!(&bounded[..32])).is_some());
        assert!(safe_chat_message_progress(&serde_json::json!(bounded)).is_none());
        let mut committed = message_progress();
        committed["phase"] = serde_json::json!("committed");
        committed["reply_row"] = serde_json::json!("m_00000000000000000000000000000000");
        assert!(safe_chat_message_progress(&serde_json::json!([committed])).is_none());
        assert!(
            safe_chat_message_submission(&serde_json::json!({"draft_revision": "private"}))
                .is_none()
        );
    }

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
