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
//! ## Available commands (Phases 1–3)
//!
//! - `{"op":"list_targets"}` — drains the host's widget registry.
//!   Returns every addressable widget by its `test_id` (when opted-in)
//!   or its path-based name (`Parent/Child[index]`), paired with the
//!   widget's bounding rect.
//! - `{"op":"click_at","x":<f64>,"y":<f64>}` — synthesise a left
//!   click at raw coordinates.
//! - `{"op":"click_named","name":<str>}` — click the centre of a
//!   widget resolved against the registry.
//! - `{"op":"drag","x_from":<f64>,"y_from":<f64>,"x_to":<f64>,
//!   "y_to":<f64>,"steps":<u32?>}` — press at `(x_from, y_from)`,
//!   interpolate `steps` (default 8) `CursorMoved`s including both
//!   endpoints, release at `(x_to, y_to)`.
//! - `{"op":"key","key":"Enter"}` — single press+release of a
//!   named key or single character.
//! - `{"op":"type","text":"hello"}` — emits one press+release pair
//!   per character.
//! - `{"op":"modifiers","ctrl":<bool>,"shift":<bool>,"alt":<bool>,
//!   "logo":<bool>}` — update the modifier mask carried into the
//!   next keyboard event.
//!
//! Future commands (Phase 4+, separate units): `snapshot`, `state_dump`.

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
    /// Drag from (x_from, y_from) to (x_to, y_to) over `steps`
    /// intermediate cursor positions (default 8). Emits a single
    /// `CursorMoved` to the start, then `MouseLeftButton{pressed:true}`,
    /// then `steps` `CursorMoved`s interpolated linearly between the
    /// endpoints (inclusive of both), then `MouseLeftButton{pressed:false}`.
    Drag {
        x_from: f64,
        y_from: f64,
        x_to: f64,
        y_to: f64,
        steps: Option<u32>,
    },
    /// Inject a single key press + release. `key` is a winit-style
    /// name — `"Enter"`, `"Escape"`, `"Tab"`, `"ArrowLeft"/Up/Right/Down`,
    /// `"Backspace"`, `"Delete"`, `"Home"`, `"End"`, `"PageUp"/Down`,
    /// `"Space"`, or a single character like `"a"`, `"1"`. Unknown
    /// names return a `parse_error`.
    Key { key: String },
    /// Inject a press + release pair for each character in `text`.
    /// Useful for typing into a focused text input (palette,
    /// rename-workspace field, …). Empty string emits no actions and
    /// returns `Ok`.
    Type { text: String },
    /// Update the modifier mask. The mask applies to subsequent
    /// keyboard events on the same connection until the next
    /// `Modifiers` command. Emits a single `SetModifiers` planned
    /// action; no winit `ModifiersChanged` event is dispatched (the
    /// compositor's host reads the mask directly from
    /// `TestControlCtx::modifiers`).
    Modifiers {
        ctrl: bool,
        shift: bool,
        alt: bool,
        logo: bool,
    },
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
    /// Inject a keyboard press or release for `key`. The transport
    /// translates `KeyTag` to `winit::keyboard::Key` and synthesises
    /// a `WindowEvent::KeyboardInput`.
    KeyboardKey { key: KeyTag, pressed: bool },
    /// Update the modifier mask carried by the executor. Modifiers
    /// are state, not events — the transport mutates its
    /// `TestControlCtx::modifiers` directly rather than dispatching
    /// a `WindowEvent::ModifiersChanged`, because the host's chord
    /// handlers read the snapshot inline (mirrors how `ShellDriver`
    /// uses `set_modifiers`).
    SetModifiers {
        ctrl: bool,
        shift: bool,
        alt: bool,
        logo: bool,
    },
}

/// Wire-friendly key identifier. Mirrors the subset of
/// `winit::keyboard::Key` the test surface supports. `Named` covers
/// the chord keys the compositor's chrome handles (Enter, Escape,
/// Tab, arrows, …); `Char` covers literal input characters used by
/// the palette / rename-workspace text field.
///
/// Extensions to this enum are additive — add a `NamedKeyTag` variant
/// + parser branch + winit-mapping branch in `execute_planned_action`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyTag {
    /// One of the well-known winit `NamedKey`s.
    Named(NamedKeyTag),
    /// A single character (typically ASCII printable, but any
    /// `char` is permitted — the executor wraps it in
    /// `winit::keyboard::Key::Character`).
    Char(char),
}

/// Wire-friendly `NamedKey`. One variant per supported winit
/// `NamedKey` — the parser's string-table and the executor's
/// winit-mapping live in lockstep with this enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NamedKeyTag {
    Enter,
    Escape,
    Tab,
    ArrowLeft,
    ArrowRight,
    ArrowUp,
    ArrowDown,
    Backspace,
    Delete,
    Home,
    End,
    PageUp,
    PageDown,
    Space,
}

/// Parse a `key` field (from a `Key`/`Type` command) into a [`KeyTag`].
/// Returns `None` if the string is empty, multi-character but doesn't
/// match a `NamedKeyTag`, or otherwise unrepresentable.
pub fn parse_key_tag(key: &str) -> Option<KeyTag> {
    match key {
        "Enter" => Some(KeyTag::Named(NamedKeyTag::Enter)),
        "Escape" => Some(KeyTag::Named(NamedKeyTag::Escape)),
        "Tab" => Some(KeyTag::Named(NamedKeyTag::Tab)),
        "ArrowLeft" => Some(KeyTag::Named(NamedKeyTag::ArrowLeft)),
        "ArrowRight" => Some(KeyTag::Named(NamedKeyTag::ArrowRight)),
        "ArrowUp" => Some(KeyTag::Named(NamedKeyTag::ArrowUp)),
        "ArrowDown" => Some(KeyTag::Named(NamedKeyTag::ArrowDown)),
        "Backspace" => Some(KeyTag::Named(NamedKeyTag::Backspace)),
        "Delete" => Some(KeyTag::Named(NamedKeyTag::Delete)),
        "Home" => Some(KeyTag::Named(NamedKeyTag::Home)),
        "End" => Some(KeyTag::Named(NamedKeyTag::End)),
        "PageUp" => Some(KeyTag::Named(NamedKeyTag::PageUp)),
        "PageDown" => Some(KeyTag::Named(NamedKeyTag::PageDown)),
        "Space" => Some(KeyTag::Named(NamedKeyTag::Space)),
        other => {
            // Single character (any `char`, including multibyte) — map to
            // `KeyTag::Char`. Multi-character strings that didn't match a
            // named-key are rejected so typos like "enter" or "escpe"
            // surface as `parse_error` rather than silently typed.
            let mut chars = other.chars();
            let first = chars.next()?;
            if chars.next().is_some() {
                None
            } else {
                Some(KeyTag::Char(first))
            }
        }
    }
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
        Command::Drag {
            x_from,
            y_from,
            x_to,
            y_to,
            steps,
        } => {
            let steps = steps.unwrap_or(8).max(1);
            let mut actions = Vec::with_capacity(2 + steps as usize + 2);
            // Park the cursor at the start, press, then linearly
            // interpolate over `steps` motions (inclusive of both
            // endpoints), then release. Matches `ShellDriver::drag`
            // — same wire as the in-process scenarios use.
            actions.push(PlannedAction::CursorMoved {
                x: *x_from,
                y: *y_from,
            });
            actions.push(PlannedAction::MouseLeftButton { pressed: true });
            let n = steps as f64;
            for s in 0..steps {
                // s=0 → x_from, s=steps-1 → x_to. The `steps==1`
                // edge case lands a single motion at x_from (matches
                // the press location); this keeps the action count
                // predictable for assertions.
                let t = if steps == 1 {
                    0.0
                } else {
                    s as f64 / (n - 1.0)
                };
                let x = x_from + (x_to - x_from) * t;
                let y = y_from + (y_to - y_from) * t;
                actions.push(PlannedAction::CursorMoved { x, y });
            }
            actions.push(PlannedAction::MouseLeftButton { pressed: false });
            (Response::Ok { frame: 0 }, actions)
        }
        Command::Key { key } => match parse_key_tag(key) {
            Some(tag) => (
                Response::Ok { frame: 0 },
                vec![
                    PlannedAction::KeyboardKey {
                        key: tag,
                        pressed: true,
                    },
                    PlannedAction::KeyboardKey {
                        key: tag,
                        pressed: false,
                    },
                ],
            ),
            None => (
                Response::Err {
                    code: "parse_error".to_string(),
                    message: format!("unrecognised key name `{key}`"),
                },
                Vec::new(),
            ),
        },
        Command::Type { text } => {
            let mut actions = Vec::with_capacity(text.chars().count() * 2);
            for c in text.chars() {
                let tag = KeyTag::Char(c);
                actions.push(PlannedAction::KeyboardKey {
                    key: tag,
                    pressed: true,
                });
                actions.push(PlannedAction::KeyboardKey {
                    key: tag,
                    pressed: false,
                });
            }
            (Response::Ok { frame: 0 }, actions)
        }
        Command::Modifiers {
            ctrl,
            shift,
            alt,
            logo,
        } => (
            Response::Ok { frame: 0 },
            vec![PlannedAction::SetModifiers {
                ctrl: *ctrl,
                shift: *shift,
                alt: *alt,
                logo: *logo,
            }],
        ),
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

    // ===================================================================
    // Phase 3 — drag / key / type / modifiers
    // ===================================================================

    /// `Drag { steps: 4 }` emits: move-to-start, press, four
    /// interpolated motions inclusive of both endpoints, release. The
    /// first motion lands at the start, the last at the end — that
    /// way both endpoints get a CursorMoved before/after the
    /// press/release, matching the "press then drag then release"
    /// pattern the host's drag handlers expect.
    #[test]
    fn drag_emits_press_motions_release() {
        let state = StaticState::new(&[]);
        let (resp, actions) = dispatch_command(
            &Command::Drag {
                x_from: 0.0,
                y_from: 0.0,
                x_to: 100.0,
                y_to: 100.0,
                steps: Some(4),
            },
            &state,
        );
        assert!(matches!(resp, Response::Ok { .. }));
        // Shape: move-to-start, press, 4 motions inclusive of both
        // endpoints, release. Exact coordinates checked via
        // approximate equality on the intermediates to dodge f64
        // round-off (300.0 / 3.0 isn't exactly equal to
        // (0..=3).map(|s| 100 * s/3.0).nth(_) under all evaluation
        // orders — only the endpoints are exact integers).
        assert_eq!(actions.len(), 7);
        assert_eq!(actions[0], PlannedAction::CursorMoved { x: 0.0, y: 0.0 });
        assert_eq!(actions[1], PlannedAction::MouseLeftButton { pressed: true });
        assert_eq!(actions[6], PlannedAction::MouseLeftButton { pressed: false });
        // Interpolated motions, t = 0/3, 1/3, 2/3, 3/3.
        let expected_ts = [0.0_f64, 1.0 / 3.0, 2.0 / 3.0, 1.0];
        for (idx, t) in expected_ts.iter().enumerate() {
            let PlannedAction::CursorMoved { x, y } = actions[2 + idx] else {
                panic!("expected CursorMoved at index {}", 2 + idx);
            };
            let expected = 100.0 * t;
            assert!(
                (x - expected).abs() < 1e-9,
                "x mismatch at step {idx}: got {x}, want {expected}"
            );
            assert!(
                (y - expected).abs() < 1e-9,
                "y mismatch at step {idx}: got {y}, want {expected}"
            );
        }
    }

    /// `Drag` without an explicit `steps` field defaults to 8
    /// intermediate motions. The total action count is therefore
    /// 1 (move-to-start) + 1 (press) + 8 (motions) + 1 (release) = 11.
    #[test]
    fn drag_default_steps_is_eight() {
        let state = StaticState::new(&[]);
        let (_, actions) = dispatch_command(
            &Command::Drag {
                x_from: 0.0,
                y_from: 0.0,
                x_to: 80.0,
                y_to: 0.0,
                steps: None,
            },
            &state,
        );
        assert_eq!(actions.len(), 11);
    }

    /// `Key { key: "Enter" }` resolves to `NamedKeyTag::Enter` and
    /// emits one press + one release. Round-trip checks both the
    /// parser path and the action shape.
    #[test]
    fn key_named_emits_press_then_release() {
        let state = StaticState::new(&[]);
        let (resp, actions) = dispatch_command(
            &Command::Key {
                key: "Enter".to_string(),
            },
            &state,
        );
        assert!(matches!(resp, Response::Ok { .. }));
        assert_eq!(
            actions,
            vec![
                PlannedAction::KeyboardKey {
                    key: KeyTag::Named(NamedKeyTag::Enter),
                    pressed: true,
                },
                PlannedAction::KeyboardKey {
                    key: KeyTag::Named(NamedKeyTag::Enter),
                    pressed: false,
                },
            ]
        );
    }

    /// `Key { key: "a" }` resolves to a single character.
    #[test]
    fn key_single_char_maps_to_char_variant() {
        let state = StaticState::new(&[]);
        let (_, actions) = dispatch_command(
            &Command::Key {
                key: "a".to_string(),
            },
            &state,
        );
        assert_eq!(
            actions[0],
            PlannedAction::KeyboardKey {
                key: KeyTag::Char('a'),
                pressed: true,
            }
        );
    }

    /// An unrecognised key name returns a `parse_error` and no actions.
    /// Mirrors `parse_unknown_command_returns_structured_error` — the
    /// contract is "if we can't translate, the harness sees the
    /// error code, not a silent no-op".
    #[test]
    fn key_unknown_returns_parse_error() {
        let state = StaticState::new(&[]);
        let (resp, actions) = dispatch_command(
            &Command::Key {
                key: "go_fishing".to_string(),
            },
            &state,
        );
        assert!(actions.is_empty());
        let Response::Err { code, .. } = resp else {
            panic!("expected Response::Err");
        };
        assert_eq!(code, "parse_error");
    }

    /// `Type { text: "ab" }` emits two characters → four actions
    /// (press 'a', release 'a', press 'b', release 'b'). The ordering
    /// matches how a real key-repeat-free typist would generate
    /// events on a focused input.
    #[test]
    fn type_text_emits_press_release_per_char() {
        let state = StaticState::new(&[]);
        let (resp, actions) = dispatch_command(
            &Command::Type {
                text: "ab".to_string(),
            },
            &state,
        );
        assert!(matches!(resp, Response::Ok { .. }));
        assert_eq!(actions.len(), 4);
        assert_eq!(
            actions,
            vec![
                PlannedAction::KeyboardKey {
                    key: KeyTag::Char('a'),
                    pressed: true,
                },
                PlannedAction::KeyboardKey {
                    key: KeyTag::Char('a'),
                    pressed: false,
                },
                PlannedAction::KeyboardKey {
                    key: KeyTag::Char('b'),
                    pressed: true,
                },
                PlannedAction::KeyboardKey {
                    key: KeyTag::Char('b'),
                    pressed: false,
                },
            ]
        );
    }

    /// Empty `text` is a valid no-op — returns Ok with zero actions.
    #[test]
    fn type_empty_text_is_a_noop() {
        let state = StaticState::new(&[]);
        let (resp, actions) = dispatch_command(
            &Command::Type {
                text: String::new(),
            },
            &state,
        );
        assert!(matches!(resp, Response::Ok { .. }));
        assert!(actions.is_empty());
    }

    /// `Modifiers` round-trips all four flags into a single
    /// `SetModifiers` action — exact bit-for-bit match. This is the
    /// state setup commands like `Ctrl+K` rely on: the harness sends
    /// `Modifiers { ctrl: true, .. }`, then `Key { key: "k" }`.
    #[test]
    fn modifiers_command_emits_single_action_with_mask() {
        let state = StaticState::new(&[]);
        let (resp, actions) = dispatch_command(
            &Command::Modifiers {
                ctrl: true,
                shift: false,
                alt: true,
                logo: false,
            },
            &state,
        );
        assert!(matches!(resp, Response::Ok { .. }));
        assert_eq!(
            actions,
            vec![PlannedAction::SetModifiers {
                ctrl: true,
                shift: false,
                alt: true,
                logo: false,
            }]
        );
    }

    /// All-false modifiers still emit one action (the masks may need
    /// to be cleared between chords). Round-trips the falsy state
    /// exactly — no implicit default.
    #[test]
    fn modifiers_all_false_still_emits_action() {
        let state = StaticState::new(&[]);
        let (_, actions) = dispatch_command(
            &Command::Modifiers {
                ctrl: false,
                shift: false,
                alt: false,
                logo: false,
            },
            &state,
        );
        assert_eq!(actions.len(), 1);
    }

    /// Parser smoke for every supported named key — guards against
    /// typos in the dispatch table (each branch is hand-rolled).
    #[test]
    fn parse_key_tag_covers_all_named_keys() {
        let cases = [
            ("Enter", NamedKeyTag::Enter),
            ("Escape", NamedKeyTag::Escape),
            ("Tab", NamedKeyTag::Tab),
            ("ArrowLeft", NamedKeyTag::ArrowLeft),
            ("ArrowRight", NamedKeyTag::ArrowRight),
            ("ArrowUp", NamedKeyTag::ArrowUp),
            ("ArrowDown", NamedKeyTag::ArrowDown),
            ("Backspace", NamedKeyTag::Backspace),
            ("Delete", NamedKeyTag::Delete),
            ("Home", NamedKeyTag::Home),
            ("End", NamedKeyTag::End),
            ("PageUp", NamedKeyTag::PageUp),
            ("PageDown", NamedKeyTag::PageDown),
            ("Space", NamedKeyTag::Space),
        ];
        for (name, expected) in cases {
            assert_eq!(
                parse_key_tag(name),
                Some(KeyTag::Named(expected)),
                "named-key parse failed for `{name}`"
            );
        }
    }

    /// Empty key string is parse_error — guards against `Type { "" }`
    /// or a buggy client sending `Key { key: "" }`.
    #[test]
    fn parse_key_tag_rejects_empty_string() {
        assert!(parse_key_tag("").is_none());
    }
}
