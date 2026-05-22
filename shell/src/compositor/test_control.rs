//! Real-input e2e test control surface — Phase 1 (pure-logic dispatch).
//!
//! Spec: `docs/superpowers/specs/2026-05-16-real-input-e2e-design.md`.
//!
//! The compositor exposes an opt-in JSON control protocol when launched
//! with `NIRIOS_TEST_CONTROL=/run/user/N/nirios-test-N.sock`. This module
//! holds the pure-logic command/response types + dispatcher; the
//! actual unix-socket binding + calloop integration is Phase 1b (a
//! follow-up unit that wires `dispatch_command` into a calloop event
//! source on the native compositor loop).
//!
//! **Why this module exists separate from the socket**: the dispatcher
//! is the thing that needs broad test coverage (every command shape,
//! every error path). The socket wiring is a thin transport. Keeping
//! the dispatcher pure means we can grow the command set without
//! touching transport code, and tests run without spawning a process
//! or binding a socket.
//!
//! ## Available commands (Phase 1)
//!
//! - `{"op":"list_targets"}` — drains the host's widget registry.
//!   Returns every addressable widget by its `test_id` (when opted-in)
//!   or its path-based name (`Parent/Child[index]`), paired with the
//!   widget's bounding rect.
//!
//! Future commands (Phase 2+, separate units): `click`, `click_at`,
//! `drag`, `key`, `type`, `modifiers`, `snapshot`, `state_dump`.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use vello::kurbo::Rect;

/// Top-level command envelope. Tag is `op`; remaining fields are
/// per-variant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Command {
    /// Drain the host's widget registry. The compositor walks the
    /// live widget tree and returns every addressable widget with its
    /// rect. Same shape as `ShellDriver::widget_registry()` returns
    /// for in-process tests, but reachable over a unix socket.
    ListTargets,
    /// Synthesise a left-click at raw (x, y) — a CursorMoved followed
    /// by a Left button Pressed + Released. The compositor's chrome
    /// receives the events through the same path winit input would.
    ClickAt { x: f64, y: f64 },
    /// Resolve `name` against the widget registry, then `click_at` the
    /// centre of the resolved rect. Returns an `unknown_target` error
    /// if the name doesn't resolve or the resolved rect has zero area
    /// (widget exists but isn't laid out yet).
    ClickNamed { name: String },
}

/// Top-level response envelope. Either an `ok` payload (per-command
/// shape) or a structured `err`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Response {
    /// `list_targets` response — sorted by key for deterministic output
    /// across runs (HashMap iteration order would otherwise leak into
    /// test assertions).
    Targets(Vec<TargetEntry>),
    /// Successful command with no payload (`click_at`, `click_named`,
    /// future `key`/`type`/`drag`). Carries the post-action frame
    /// counter so clients can poll for state changes against a known
    /// frame number rather than wall time. Currently always `0` —
    /// the frame counter wireup is a follow-up.
    Ok { frame: u64 },
    /// Structured error. `code` is a stable machine-readable string;
    /// `message` is human prose for log output.
    Err { code: String, message: String },
}

/// One entry in a `list_targets` response. `rect` is `(x, y, w, h)` in
/// the compositor's own coordinate space — clients can click via
/// `click_at` (Phase 2) using the centre, or hit-test against arbitrary
/// points.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TargetEntry {
    pub name: String,
    pub rect: [f64; 4],
}

impl TargetEntry {
    fn from_kv(name: &str, rect: Rect) -> Self {
        Self {
            name: name.to_string(),
            rect: [rect.x0, rect.y0, rect.width(), rect.height()],
        }
    }
}

/// Trait the compositor's main loop implements to provide live state
/// to the dispatcher. Tests pass mock implementations that return
/// canned data; the real binary passes a closure that walks the
/// current `FrameBuilder`'s host.
pub trait ControlState {
    /// Snapshot of every addressable widget in the live widget tree.
    fn widget_registry(&self) -> HashMap<String, Rect>;
}

/// Side-effect actions the transport layer must execute after a
/// `dispatch_command` call to fulfil the command. Pure values; no
/// I/O. The transport synthesises `winit::WindowEvent`s from these
/// and dispatches them through the shell's `host_and_taffy_mut`
/// path — same wire as `WinitBackend` and `ShellDriver`.
#[derive(Debug, Clone, PartialEq)]
pub enum PlannedAction {
    /// Move the cursor to `(x, y)` in compositor coordinates.
    CursorMoved { x: f64, y: f64 },
    /// Inject a left mouse button event. `pressed=true` is button-down,
    /// `false` is button-up.
    MouseLeftButton { pressed: bool },
}

/// Dispatch a single command against the live compositor state.
///
/// Returns a tuple of `(response, actions)`. Read-only commands (like
/// `list_targets`) return an empty action list; commands that mutate
/// (click_at, click_named, future key/type/drag) return the sequence
/// of [`PlannedAction`]s the transport must execute before writing
/// the response. The split lets the dispatcher stay pure (no I/O, no
/// `&mut` shell state) while still encoding "do X then reply Y" in
/// one round-trip.
pub fn dispatch_command<S: ControlState>(
    cmd: &Command,
    state: &S,
) -> (Response, Vec<PlannedAction>) {
    match cmd {
        Command::ListTargets => {
            let registry = state.widget_registry();
            let mut entries: Vec<TargetEntry> = registry
                .iter()
                .filter(|(_, r)| r.area() > 0.0)
                .map(|(name, rect)| TargetEntry::from_kv(name, *rect))
                .collect();
            entries.sort_by(|a, b| a.name.cmp(&b.name));
            (Response::Targets(entries), Vec::new())
        }
        Command::ClickAt { x, y } => (
            Response::Ok { frame: 0 },
            vec![
                PlannedAction::CursorMoved { x: *x, y: *y },
                PlannedAction::MouseLeftButton { pressed: true },
                PlannedAction::MouseLeftButton { pressed: false },
            ],
        ),
        Command::ClickNamed { name } => {
            let registry = state.widget_registry();
            match registry.get(name) {
                Some(rect) if rect.area() > 0.0 => {
                    let c = rect.center();
                    (
                        Response::Ok { frame: 0 },
                        vec![
                            PlannedAction::CursorMoved { x: c.x, y: c.y },
                            PlannedAction::MouseLeftButton { pressed: true },
                            PlannedAction::MouseLeftButton { pressed: false },
                        ],
                    )
                }
                _ => (
                    Response::Err {
                        code: "unknown_target".to_string(),
                        message: format!(
                            "no addressable widget named `{name}` (or its rect \
                             has zero area; not yet laid out?)"
                        ),
                    },
                    Vec::new(),
                ),
            }
        }
    }
}

/// Parse a single JSON line into a [`Command`]. Returns a structured
/// [`Response::Err`] on parse failure rather than bubbling up a
/// `serde_json::Error` — the transport layer should be transparent
/// about parse failures so clients see machine-readable error codes.
pub fn parse_command(line: &str) -> Result<Command, Response> {
    serde_json::from_str::<Command>(line.trim()).map_err(|e| Response::Err {
        code: "parse_error".to_string(),
        message: format!("invalid JSON command: {e}"),
    })
}

/// Serialise a response to a single JSON line. Always succeeds —
/// [`Response`]'s shape is finite + serde-derived. The newline is
/// implicit (the transport adds it).
pub fn serialise_response(resp: &Response) -> String {
    // unwrap: Response is a finite enum of Serialize-deriving variants
    // with no non-serialisable fields. A failure here would be a
    // programming error, not a runtime one.
    serde_json::to_string(resp).expect("serialise Response")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `ControlState` impl that returns a fixed registry — lets us
    /// drive the dispatcher without spinning up a real compositor.
    struct StaticState {
        registry: HashMap<String, Rect>,
    }

    impl StaticState {
        fn new(entries: &[(&str, Rect)]) -> Self {
            Self {
                registry: entries
                    .iter()
                    .map(|(k, v)| (k.to_string(), *v))
                    .collect(),
            }
        }
    }

    impl ControlState for StaticState {
        fn widget_registry(&self) -> HashMap<String, Rect> {
            self.registry.clone()
        }
    }

    #[test]
    fn parse_list_targets_command_round_trips() {
        let cmd = parse_command(r#"{"op":"list_targets"}"#).unwrap();
        assert_eq!(cmd, Command::ListTargets);
    }

    #[test]
    fn parse_unknown_command_returns_structured_error() {
        let err = parse_command(r#"{"op":"go_fishing"}"#).unwrap_err();
        let Response::Err { code, .. } = err else {
            panic!("expected Response::Err");
        };
        assert_eq!(code, "parse_error");
    }

    #[test]
    fn parse_malformed_json_returns_structured_error() {
        let err = parse_command(r#"this is not json"#).unwrap_err();
        let Response::Err { code, .. } = err else {
            panic!("expected Response::Err");
        };
        assert_eq!(code, "parse_error");
    }

    #[test]
    fn list_targets_returns_sorted_entries() {
        let state = StaticState::new(&[
            ("zeta", Rect::new(0.0, 0.0, 10.0, 10.0)),
            ("alpha", Rect::new(100.0, 100.0, 20.0, 20.0)),
            ("mid", Rect::new(50.0, 50.0, 15.0, 15.0)),
        ]);
        let (resp, actions) = dispatch_command(&Command::ListTargets, &state);
        assert!(actions.is_empty(), "list_targets has no side effects");
        let Response::Targets(entries) = resp else {
            panic!("expected Targets response");
        };
        let names: Vec<_> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "mid", "zeta"]);
    }

    #[test]
    fn list_targets_drops_zero_area_widgets() {
        // Widgets that haven't been laid out yet have Rect::ZERO; the
        // dispatcher filters them out so clients only see clickable
        // targets.
        let state = StaticState::new(&[
            ("clickable", Rect::new(0.0, 0.0, 10.0, 10.0)),
            ("not_laid_out", Rect::ZERO),
        ]);
        let (resp, _) = dispatch_command(&Command::ListTargets, &state);
        let Response::Targets(entries) = resp else {
            panic!("expected Targets response");
        };
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "clickable");
    }

    #[test]
    fn list_targets_carries_rect_in_xywh_order() {
        // Dispatcher contract: rect field is `[x, y, width, height]`,
        // regardless of how the caller built the source `Rect`.
        let state = StaticState::new(&[(
            "tab",
            Rect::from_origin_size(
                vello::kurbo::Point::new(40.0, 80.0),
                vello::kurbo::Size::new(120.0, 36.0),
            ),
        )]);
        let (resp, _) = dispatch_command(&Command::ListTargets, &state);
        let Response::Targets(entries) = resp else {
            panic!("expected Targets response");
        };
        assert_eq!(entries[0].rect, [40.0, 80.0, 120.0, 36.0]);
    }

    #[test]
    fn click_at_emits_motion_press_release_in_order() {
        let state = StaticState::new(&[]);
        let (resp, actions) =
            dispatch_command(&Command::ClickAt { x: 42.0, y: 7.0 }, &state);
        assert!(matches!(resp, Response::Ok { .. }));
        assert_eq!(
            actions,
            vec![
                PlannedAction::CursorMoved { x: 42.0, y: 7.0 },
                PlannedAction::MouseLeftButton { pressed: true },
                PlannedAction::MouseLeftButton { pressed: false },
            ]
        );
    }

    #[test]
    fn click_named_resolves_to_rect_centre() {
        let state = StaticState::new(&[(
            "btn",
            Rect::from_origin_size(
                vello::kurbo::Point::new(100.0, 200.0),
                vello::kurbo::Size::new(40.0, 20.0),
            ),
        )]);
        let (resp, actions) = dispatch_command(
            &Command::ClickNamed {
                name: "btn".to_string(),
            },
            &state,
        );
        assert!(matches!(resp, Response::Ok { .. }));
        // Centre of (100, 200, 40, 20) is (120, 210).
        assert_eq!(actions[0], PlannedAction::CursorMoved { x: 120.0, y: 210.0 });
    }

    #[test]
    fn click_named_unknown_target_returns_error_with_no_actions() {
        let state = StaticState::new(&[]);
        let (resp, actions) = dispatch_command(
            &Command::ClickNamed {
                name: "ghost".to_string(),
            },
            &state,
        );
        assert!(actions.is_empty());
        let Response::Err { code, .. } = resp else {
            panic!("expected Err");
        };
        assert_eq!(code, "unknown_target");
    }

    #[test]
    fn click_named_zero_area_returns_error() {
        // Widget exists in the registry but isn't laid out yet (Rect::ZERO).
        // The dispatcher treats this as "not clickable" to match
        // ShellDriver::resolve_named_rect's contract.
        let state = StaticState::new(&[("pending", Rect::ZERO)]);
        let (resp, actions) = dispatch_command(
            &Command::ClickNamed {
                name: "pending".to_string(),
            },
            &state,
        );
        assert!(actions.is_empty());
        assert!(matches!(resp, Response::Err { .. }));
    }

    #[test]
    fn serialise_response_emits_kebab_friendly_json() {
        let resp = Response::Targets(vec![TargetEntry {
            name: "btn".to_string(),
            rect: [0.0, 0.0, 10.0, 10.0],
        }]);
        let out = serialise_response(&resp);
        // Shape check — exact JSON layout is implementation detail, but
        // the consumer-visible keys must be `targets`, `name`, `rect`.
        assert!(out.contains("\"targets\""));
        assert!(out.contains("\"name\":\"btn\""));
        assert!(out.contains("\"rect\""));
    }

    #[test]
    fn serialise_error_emits_code_and_message() {
        let resp = Response::Err {
            code: "parse_error".to_string(),
            message: "oops".to_string(),
        };
        let out = serialise_response(&resp);
        assert!(out.contains("\"code\":\"parse_error\""));
        assert!(out.contains("\"message\":\"oops\""));
        assert!(out.contains("\"err\""));
    }
}
