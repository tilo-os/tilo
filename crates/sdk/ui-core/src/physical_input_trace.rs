//! Content-free evidence from ordinary native platform input and drawn frames.
//! A drawn frame is not display completion: only matching presentation feedback
//! can close the timeline. Missing IDs/state/feedback never qualify a measurement.
use crate::time::{Duration, Instant};
use serde_json::Value;
use winit::event::{ElementState, Ime, WindowEvent};

#[derive(Default)]
pub struct Trace {
    next_id: u64,
    pending: Vec<u64>,
    drawn_inputs: Vec<u64>,
    redraws_remaining: u8,
    redraw_until: Option<Instant>,
}

impl Trace {
    pub(crate) fn when_enabled(value: Option<&str>) -> Option<Self> {
        (value == Some("1")).then(Self::default)
    }

    pub(crate) fn observes(event: &WindowEvent) -> bool {
        matches!(event,
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed
        ) || matches!(event, WindowEvent::Ime(Ime::Commit(_)))
    }

    pub(crate) fn has_pending(&self) -> bool {
        !self.pending.is_empty()
            || (self.redraws_remaining > 0
                && self
                    .redraw_until
                    .is_some_and(|until| crate::time::now() < until))
    }

    pub fn begin(&mut self, event: &WindowEvent, snapshot: Option<Value>) -> Option<Value> {
        let kind = match event {
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                "key_pressed"
            }
            WindowEvent::Ime(Ime::Commit(_)) => "ime_commit",
            _ => return None,
        };
        let (key_category, text) = match event {
            WindowEvent::KeyboardInput { event, .. } => {
                let category = match &event.logical_key {
                    winit::keyboard::Key::Character(value)
                        if value.chars().next().is_some_and(|c| !c.is_control()) =>
                    {
                        "printable"
                    }
                    winit::keyboard::Key::Named(_) => "named",
                    _ => "other",
                };
                (category, event.text.as_deref())
            }
            WindowEvent::Ime(Ime::Commit(text)) => ("other", Some(text.as_str())),
            _ => return None,
        };
        let scalar_count = text.map(|text| text.chars().take(4097).count());
        let stamp = tilo_presentation::key_stamp(event);
        self.next_id = self.next_id.checked_add(1)?;
        let id = self.next_id;
        if self.pending.is_empty() {
            self.drawn_inputs.clear();
            self.redraws_remaining = 0;
            self.redraw_until = None;
        }
        self.pending.push(id);
        let (_, time) = crate::native_scenario::host_clock::sample();
        Some(serde_json::json!({
            "id": id, "kind": kind, "process_id": std::process::id(), "observer_session": crate::native_scenario::observer_session_id(),
            "wayland_serial": stamp.map(|stamp| stamp.0),
            "wayland_time_ms": stamp.map(|stamp| stamp.1),
            "clock": crate::native_scenario::host_clock::CLOCK_NAME,
            "dispatch_lo_s": time.map(|time| time.lo_s),
            "dispatch_hi_s": time.map(|time| time.hi_s),
            "key_category": key_category,
            "text_present": text.is_some(),
            "text_scalar_count": scalar_count.map(|count| count.min(4096)),
            "text_count_truncated": scalar_count.is_some_and(|count| count > 4096),
            "before": safe_snapshot(snapshot),
        }))
    }

    pub fn effect(input: Value, snapshot: Option<Value>) -> Value {
        let mut row = input;
        row["after"] = safe_snapshot(snapshot);
        row
    }

    pub fn drawn(&mut self, cpu_frame_id: Option<u64>, snapshot: Option<Value>) -> Option<Value> {
        if !self.pending.is_empty() {
            self.drawn_inputs = std::mem::take(&mut self.pending);
            // A submitted buffer can be discarded before display. Preserve
            // a bounded content-free witness on subsequent actual draws.
            self.redraws_remaining = 32;
            self.redraw_until = crate::time::now().checked_add(Duration::from_secs(2));
        }
        if !self.has_pending() {
            return None;
        }
        self.redraws_remaining = self.redraws_remaining.saturating_sub(1);
        Some(serde_json::json!({
            "cpu_frame_id": cpu_frame_id, "process_id": std::process::id(), "observer_session": crate::native_scenario::observer_session_id(),
            "input_ids": self.drawn_inputs,
            "state": safe_snapshot(snapshot),
        }))
    }
}

// Existing app snapshots may contain names or paths. This physical-input
// observer carries only the declared content-free Notes state evidence.
fn safe_snapshot(snapshot: Option<Value>) -> Value {
    let Some(snapshot) = snapshot.filter(|snapshot| snapshot["app"] == "so.tilo.notes") else {
        return Value::Null;
    };
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
    for key in [
        "active_line_present",
        "active_line_focused",
        "active_line_disabled",
        "editor_read_only",
        "editor_composing",
        "editor_route_available",
        "sidebar_renaming",
        "sidebar_menu_open",
        "editor_menu_open",
    ] {
        if let Some(value) = snapshot.get(key).and_then(Value::as_bool) {
            safe.insert(key.into(), Value::Bool(value));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_observer_requires_the_explicit_opt_in() {
        for value in [None, Some(""), Some("0"), Some("true"), Some("2")] {
            assert!(Trace::when_enabled(value).is_none());
        }
        assert!(Trace::when_enabled(Some("1")).is_some());
    }

    #[test]
    fn key_metadata_is_content_free_and_text_count_has_a_strict_bound() {
        let key = winit::keyboard::Key::Character("private key text".into());
        let mut event = crate::synthetic_input::char_key("private key text");
        let mut trace = Trace::default();
        let row = trace.begin(&event, None).unwrap();
        assert_eq!(row["key_category"], "printable");
        assert_eq!(row["text_present"], true);
        assert_eq!(row["text_scalar_count"], 16);
        assert!(!row.to_string().contains("private key text"));
        let named = crate::synthetic_input::named_key(winit::keyboard::NamedKey::Enter);
        assert_eq!(trace.begin(&named, None).unwrap()["key_category"], "named");
        if let WindowEvent::KeyboardInput { event, .. } = &mut event {
            event.text = None;
        }
        let row = trace.begin(&event, None).unwrap();
        assert_eq!(row["text_present"], false);
        assert!(row["text_scalar_count"].is_null());
        let row = trace
            .begin(&WindowEvent::Ime(Ime::Commit("é".repeat(5000))), None)
            .unwrap();
        assert_eq!(row["text_scalar_count"], 4096);
        assert_eq!(row["text_count_truncated"], true);
        assert!(!row.to_string().contains('é'));
        let released = crate::synthetic_input::keyboard_input(crate::synthetic_input::key_event(
            key,
            None,
            ElementState::Released,
        ));
        assert!(trace.begin(&released, None).is_none());
    }

    #[test]
    fn route_snapshot_flags_are_boolean_only_and_cannot_export_private_values() {
        let safe = safe_snapshot(Some(serde_json::json!({
            "app":"so.tilo.notes", "active_line_focused":true,
            "sidebar_renaming":"private rename value", "editor_composing":false,
        })));
        assert_eq!(safe["active_line_focused"], true);
        assert_eq!(safe["editor_composing"], false);
        assert!(safe.get("sidebar_renaming").is_none());
        assert!(!safe.to_string().contains("private rename value"));
    }

    #[test]
    fn redraw_witness_is_bounded_and_a_new_input_replaces_it() {
        let mut trace = Trace {
            next_id: 1,
            ..Trace::default()
        };
        trace.pending.push(1);
        let state = serde_json::json!({"app": "so.tilo.notes", "body_chars": 1});
        for frame in 1..=32 {
            assert!(trace.drawn(Some(frame), Some(state.clone())).is_some());
        }
        assert!(!trace.has_pending());
        assert!(trace.drawn(Some(33), Some(state.clone())).is_none());
        let event = WindowEvent::Ime(Ime::Commit("private input".to_owned()));
        let input = trace.begin(&event, Some(state.clone())).unwrap();
        assert_eq!(input["id"], 2);
        let row = trace.drawn(Some(34), Some(state)).unwrap();
        assert_eq!(row["input_ids"], serde_json::json!([2]));
        assert!(!input.to_string().contains("private input"));
        trace.redraw_until = crate::time::now().checked_sub(Duration::from_secs(1));
        assert!(trace.drawn(Some(35), None).is_none());
    }

    #[test]
    fn discarded_draw_keeps_a_content_free_witness_for_its_redraw() {
        let mut trace = Trace::default();
        trace.pending.push(1);
        let state = serde_json::json!({
            "app": "so.tilo.notes", "body_hash": "1".repeat(64),
            "body_chars": 1, "title": "private title", "text": "private text"
        });
        let first = trace.drawn(Some(433), Some(state.clone())).unwrap();
        assert_eq!(first["input_ids"], serde_json::json!([1]));
        let retry = trace
            .drawn(Some(435), Some(state))
            .expect("a discarded first buffer must not erase the input witness for a later draw");
        assert_eq!(retry["cpu_frame_id"], 435);
        assert_eq!(retry["input_ids"], first["input_ids"]);
        assert_eq!(retry["state"], first["state"]);
        assert!(retry["state"].get("title").is_none());
        assert!(retry["state"].get("text").is_none());
    }
}
