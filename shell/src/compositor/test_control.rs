//! Real-input e2e test control protocol (`QOS_TEST_CONTROL=/run/user/N/nirios-test-N.sock`): pure command/response types + dispatcher, kept separate from the socket transport.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use vello::kurbo::Rect;

use crate::shell::search::action_api::ArgValue;

/// Which pointer button a [`Command::PointerButton`] means — spelled out because X11, evdev and winit each number buttons differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PointerButtonKind {
    #[default]
    Left,
    Right,
    Middle,
}

/// Top-level command envelope. Tag is `op`; remaining fields are per-variant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Command {
    /// Drain the host's widget registry. Same shape as `ShellDriver::widget_registry()`, reachable over a unix socket.
    ListTargets,
    /// Synthesise a left click at raw `(x, y)`.
    ClickAt { x: f64, y: f64 },
    /// Resolve `name` against the widget registry, then `click_at` its centre. `unknown_target` if unresolved or zero-area (not yet laid out).
    ClickNamed { name: String },
    /// Press at `(x_from, y_from)`, interpolate `steps` (default 8) `CursorMoved`s inclusive of both endpoints, release at `(x_to, y_to)`.
    Drag {
        x_from: f64,
        y_from: f64,
        x_to: f64,
        y_to: f64,
        steps: Option<u32>,
    },
    /// Per-surface frame counters for every tracked foreign toplevel. Read-only; lets a harness tell a painted app from one still showing the first-frame placeholder.
    ForeignFrames,
    /// Inject a key press + release. `key` is a winit-style name or a single character; unknown names return `parse_error`.
    Key { key: String },
    /// Press `key` and hold it (pairs with [`Command::KeyUp`]). Also accepts the bare modifiers `"Shift"`, `"Control"`, `"Alt"`, `"Super"` — holding a real modifier keycode across another keystroke is the only way to drive xkb modifier state like hardware does.
    KeyDown { key: String },
    /// Release a key held by [`Command::KeyDown`].
    KeyUp { key: String },
    /// Scroll `(dx, dy)` wheel notches at the current cursor position. Positive `dy` scrolls up (winit's `LineDelta` convention; inverted for Wayland when forwarding to a client).
    Scroll { dx: f32, dy: f32 },
    /// Inject a press + release pair for each character in `text`. Empty string is a no-op returning `Ok`.
    Type { text: String },
    /// Update the modifier mask carried into subsequent keyboard events until the next `Modifiers` command. Emits a single `SetModifiers` planned action, since the host's chord handlers read the mask straight off `TestControlCtx::modifiers`.
    Modifiers {
        ctrl: bool,
        shift: bool,
        alt: bool,
        logo: bool,
    },
    /// Re-paint the current scene into a fresh `OffscreenRenderer` and write the PNG to `path`. Only an empty `path` is pre-validated (`invalid_path`); other I/O errors are logged, not surfaced.
    Snapshot { path: String },
    /// Read-only snapshot of the live [`shell::state::WorkspaceTree`] plus [`ChromeState`], both as opaque `serde_json::Value` so future fields don't break the wire.
    StateDump,
    /// Grant a capability on the active workspace on behalf of the user at this session, through `Workspace::grant_as` — the production authority check, so a grantor without ownership or `Admin` is refused, not worked around.
    ///
    /// `subject` is `person:<id>` / `app:<id>` / `agent:<id>`; `scope` is `workspace`, `room`, `app:<id>` or `action:<app>/<action>`; `verb` is read / comment / write / invite / admin.
    Grant {
        subject: String,
        scope: String,
        verb: String,
        /// Stable capability id so a later `revoke` can name it; minted when omitted.
        #[serde(default)]
        id: Option<String>,
    },
    /// Revoke capability `id` on the active workspace, through `Workspace::revoke_as` — same authority check as `grant`.
    Revoke { id: String },
    /// Ask `shell::actions::check_action` whether `actor` may invoke `app`'s `action`, against the live workspace.
    CheckAction {
        actor: String,
        app: String,
        action: String,
        verb: String,
        #[serde(default)]
        destructive: bool,
    },
    /// Route the next window announcing `app_id` into its own group rather than a tab, via `shell::launcher::set_pending_placement` — needed because dragging between two client windows requires them in separate groups.
    PlaceNextWindow { app_id: String },
    /// Move the pointer to `(x, y)` without pressing anything. [`Command::ClickAt`]/[`Command::Drag`] bundle press+release, which can't express drag-and-drop: the source must be *holding* the button when it calls `start_drag`, with the drop on a release after the pointer has moved over another client. These ops decompose that.
    PointerMove { x: f64, y: f64 },
    /// Give every window group in the active strip the same share of it, in permille. A group defaults to about three quarters of its strip, so two never fit together without this.
    SetGroupWidth { permille: u16 },
    /// Press or release a pointer button at the current position. `button` defaults to the left one; the right button drives the "select, right-click" gesture this desktop leans on.
    PointerButton {
        pressed: bool,
        #[serde(default)]
        button: PointerButtonKind,
    },
    /// Copy `text` from in-process chrome, through `ui::text_selection::clipboard_copy` — lands on the real clipboard backend and is published as a Wayland selection on the next tick.
    ClipboardCopy {
        text: String,
        /// `true` targets the primary selection instead of the clipboard.
        #[serde(default)]
        primary: bool,
    },
    /// Paste from in-process chrome, through `ui::text_selection::clipboard_paste`. Returns [`Response::Clipboard`] with the text, or `null` when empty.
    ClipboardPaste {
        /// `true` reads the primary selection instead of the clipboard.
        #[serde(default)]
        primary: bool,
    },
    /// Enumerate the actions/resources the shell exports now, through the same provider registry that feeds the Ctrl+K palette. Read-only — no invoke handles in the reply. An empty `query` lists every provider's defaults.
    ActionsEnumerate {
        #[serde(default)]
        query: String,
    },
    /// Invoke one enumerated action by its `(provider, id)` address, with no window, scene or synthetic input.
    ///
    /// The address is resolved against a fresh catalogue first, so an unknown or hidden id is `unknown_action`, not silently ignored. `args` is checked against the resolved parameters; a bad argument is `invalid_arguments`, deliberately distinct — "address is wrong" vs. "arguments are wrong" are fixed differently. Omitting `args` is the nullary call.
    ActionsInvoke {
        provider: String,
        id: String,
        #[serde(default)]
        args: serde_json::Map<String, serde_json::Value>,
    },
}

/// Top-level response envelope: an `ok` payload (per-command shape) or a structured `err`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Response {
    /// `list_targets` response, sorted by key for deterministic output (`HashMap` order would otherwise leak into test assertions).
    Targets(Vec<TargetEntry>),
    /// Successful command with no payload. Carries the post-action frame counter (currently always `0`) so clients can poll against a known frame number.
    Ok { frame: u64 },
    /// Structured error: `code` is stable and machine-readable, `message` is human prose for logs.
    Err { code: String, message: String },
    /// `foreign_frames` response — one opaque entry per tracked foreign surface.
    ForeignFrames { frames: serde_json::Value },
    /// `state_dump` response — persisted [`WorkspaceTree`] + [`ChromeState`], both opaque.
    State {
        workspace_tree: serde_json::Value,
        chrome: serde_json::Value,
        /// The peers this session would paint, straight off the `PresenceCtx` the overlay reads.
        peers: serde_json::Value,
    },
    /// Answer to a capability query or admin command (`grant`, `revoke`, `check_action`). Opaque JSON so the shape can grow.
    Answer(serde_json::Value),
    /// `clipboard_paste` response. `None` means nothing has been copied — distinct from `Some("")`, an owned empty selection.
    Clipboard { text: Option<String> },
}

/// One `list_targets` entry. `rect` is `(x, y, w, h)` in the compositor's coordinate space.
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

/// Trait the compositor's main loop implements to give the dispatcher live state. Tests pass mocks with canned data; the real binary passes a closure walking the current `FrameBuilder`'s host.
pub trait ControlState {
    /// Snapshot of every addressable widget in the live widget tree.
    fn widget_registry(&self) -> HashMap<String, Rect>;
    /// JSON view of the persisted [`WorkspaceTree`] — same shape `NIRIOS_TEST_STATE_DUMP` writes on every frame tick. Defaults to `Null` so mock fixtures that only need `widget_registry` don't have to spell out a tree.
    fn workspace_tree_json(&self) -> serde_json::Value {
        serde_json::Value::Null
    }
    /// JSON view of [`ChromeState`] (`overlays`, `search_scope`, `ws_settings_tab`, `revision`, `editing_workspace_name`). Defaults to `Null`.
    fn chrome_state_json(&self) -> serde_json::Value {
        serde_json::Value::Null
    }
    /// The peers this session would paint right now, off the same `PresenceCtx` the overlay draws from — exposed because "no peer arrived" and "a peer arrived and wasn't drawn" otherwise look identical. Defaults to `Null`.
    fn peers_json(&self) -> serde_json::Value {
        serde_json::Value::Null
    }
    /// Per-surface ground truth of whether each foreign toplevel has ever presented. The placeholder tile is near-identical between apps and, for a legitimately-empty one, near-identical to a real first frame — `frame_seq > 0` is the fact a harness needs instead of guessing from pixels. Defaults to an empty list.
    fn foreign_frames_json(&self) -> serde_json::Value {
        serde_json::Value::Array(Vec::new())
    }

    /// Grant a capability on the active workspace via `Workspace::grant_as`. Default impl refuses: a `ControlState` with no live tree has nothing to administer, and answering anything else would let a harness believe it proved something about authority.
    ///
    /// # Errors
    ///
    /// Returns a human-readable refusal reason whenever the grant is not recorded — no live workspace, an unauthorised grantor, or an invalid subject/scope/verb.
    fn grant_capability(
        &self,
        _subject: &str,
        _scope: &str,
        _verb: &str,
        _id: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        Err("no live workspace on this control state".to_string())
    }

    /// Revoke a capability by id on the active workspace, via `Workspace::revoke_as`.
    ///
    /// # Errors
    ///
    /// Returns a human-readable refusal reason whenever the revoke is not recorded — no live workspace, an unauthorised revoker, or an unknown capability id.
    fn revoke_capability(&self, _id: &str) -> Result<serde_json::Value, String> {
        Err("no live workspace on this control state".to_string())
    }

    /// Run `shell::actions::check_action` against the active workspace.
    ///
    /// # Errors
    ///
    /// Returns a human-readable refusal reason whenever the check itself cannot be answered — no live workspace on this control state.
    fn check_action(
        &self,
        _actor: &str,
        _app: &str,
        _action: &str,
        _verb: &str,
        _destructive: bool,
    ) -> Result<serde_json::Value, String> {
        Err("no live workspace on this control state".to_string())
    }

    /// Enumerate the exported action surface for `query` as JSON. Default impl refuses, same reasoning as the capability methods: "exports nothing" and "cannot answer" must not share a wire shape.
    ///
    /// # Errors
    ///
    /// Returns a human-readable refusal reason whenever the surface cannot be enumerated at all — no live action registry on this control state.
    fn enumerate_actions(&self, _query: &str) -> Result<serde_json::Value, String> {
        Err("no live action registry on this control state".to_string())
    }

    /// Resolve `(provider, id)` against a fresh catalogue and report what it addresses, without invoking it — the validation half of `actions_invoke`, so the transport can refuse instead of planning an invocation that silently does nothing.
    ///
    /// # Errors
    ///
    /// Returns an [`ActionRefusal`] when the address names nothing this caller can see, the arguments don't match the resolved parameters, or the action failed to describe itself.
    fn resolve_action(
        &self,
        _provider: &str,
        _id: &str,
        _args: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<serde_json::Value, ActionRefusal> {
        Err(ActionRefusal::unknown(
            "no live action registry on this control state",
        ))
    }
}

/// A refused invocation, carrying the code the wire should report.
///
/// A plain string would collapse "address doesn't exist for you" into "wrong arguments" — those are corrected differently, one by looking again at what you may see, the other at what the action declared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionRefusal {
    pub code: &'static str,
    pub message: String,
}

impl ActionRefusal {
    /// The address names nothing this caller can see — deliberately the same answer for "no such action" and "hidden from you", so probing can't map what it may not see.
    pub fn unknown(message: impl Into<String>) -> Self {
        Self {
            code: "unknown_action",
            message: message.into(),
        }
    }

    /// The address is good; the arguments are not.
    pub fn invalid_arguments(message: impl Into<String>) -> Self {
        Self {
            code: "invalid_arguments",
            message: message.into(),
        }
    }

    /// The address and arguments are both good, and the action still could not do its job.
    pub fn failed(message: impl Into<String>) -> Self {
        Self {
            code: "action_failed",
            message: message.into(),
        }
    }
}

/// Translate one JSON argument object into the action vocabulary's values. Kept at the transport, not the action API, so the catalogue never learns a wire format. JSON shapes with no parameter type (null, arrays, objects, non-integer numbers) are refused by name.
///
/// # Errors
///
/// Returns [`ActionRefusal::invalid_arguments`] for a JSON value with no matching parameter type: a non-integer number, `null`, an array, or an object.
pub fn arg_values_from_json(
    args: &serde_json::Map<String, serde_json::Value>,
) -> Result<Vec<(String, ArgValue)>, ActionRefusal> {
    let mut out = Vec::with_capacity(args.len());
    for (name, value) in args {
        let bound = match value {
            serde_json::Value::String(s) => ArgValue::Text(s.clone()),
            serde_json::Value::Bool(b) => ArgValue::Boolean(*b),
            serde_json::Value::Number(n) => match n.as_i64() {
                Some(i) => ArgValue::Integer(i),
                None => {
                    return Err(ActionRefusal::invalid_arguments(format!(
                        "argument `{name}` must be a whole number, got {n}"
                    )));
                }
            },
            other => {
                return Err(ActionRefusal::invalid_arguments(format!(
                    "argument `{name}` has no parameter type: {}",
                    match other {
                        serde_json::Value::Null => "null",
                        serde_json::Value::Array(_) => "array",
                        _ => "object",
                    }
                )));
            }
        };
        out.push((name.clone(), bound));
    }
    Ok(out)
}

/// Side-effect actions the transport must execute after a `dispatch_command` call. Pure values, no I/O — the transport synthesises `winit::WindowEvent`s from these through `host_and_taffy_mut`, the same wire as `WinitBackend`/`ShellDriver`.
#[derive(Debug, Clone, PartialEq)]
pub enum PlannedAction {
    /// Move the cursor to `(x, y)` in compositor coordinates.
    CursorMoved { x: f64, y: f64 },
    /// Inject a left mouse button event: `pressed=true` is down, `false` is up.
    MouseLeftButton { pressed: bool },
    /// Press or release a pointer button other than left. Kept separate from [`Self::MouseLeftButton`] since call sites here and in `native.rs` only ever plan `Left`, and widening both for an unused case is churn.
    MousePointerButton {
        pressed: bool,
        button: PointerButtonKind,
    },
    /// Inject a keyboard press or release for `key`. The transport maps `KeyTag` to `winit::keyboard::Key` and synthesises a `WindowEvent::KeyboardInput`.
    KeyboardKey { key: KeyTag, pressed: bool },
    /// Inject a discrete wheel scroll of `(dx, dy)` notches at the cursor. The transport synthesises a `WindowEvent::MouseWheel` carrying a `LineDelta`.
    MouseWheel { dx: f32, dy: f32 },
    /// Update the modifier mask carried by the executor. Modifiers are state, not events — mutated on `TestControlCtx::modifiers` directly rather than via `WindowEvent::ModifiersChanged`, since chord handlers read the snapshot inline.
    SetModifiers {
        ctrl: bool,
        shift: bool,
        alt: bool,
        logo: bool,
    },
    /// Re-paint the current scene off-screen and write the RGBA buffer to `path` as a PNG. Failures are logged via `eprintln!`, not propagated to the wire response.
    Snapshot { path: PathBuf },
    /// Give every live window group the same share of its strip, in permille. Applied to the widget tree, where the laid-out width lives.
    SetGroupWidth { permille: u16 },
    /// Run one already-validated action against the live CRDT. Carried as a planned action rather than executed inside `dispatch_command`, since invoking mutates the document and dispatch only ever holds it immutably. The executor re-resolves the address and re-checks `args` against a fresh catalogue, since the document may have moved on since planning.
    InvokeAction {
        provider: String,
        id: String,
        args: serde_json::Map<String, serde_json::Value>,
    },
}

/// Wire-friendly key identifier mirroring the subset of `winit::keyboard::Key` the test surface supports. `Named` covers the chrome's chord keys; `Char` covers literal input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyTag {
    /// One of the well-known winit `NamedKey`s.
    Named(NamedKeyTag),
    /// A single character (typically ASCII printable, but any `char` is permitted — wrapped in `winit::keyboard::Key::Character`).
    Char(char),
}

/// Wire-friendly `NamedKey`. One variant per supported winit `NamedKey`; the parser's string-table and executor's mapping stay in lockstep with this enum.
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
    /// Bare modifier keys, reachable only through `key_down`/`key_up` (a `key` press+release pair would release the modifier before the next key arrives) — the only way to hold a real modifier across a keystroke, matching hardware/xkb.
    Shift,
    Control,
    Alt,
    Super,
}

/// Parse a `key` field (from `Key`/`Type`) into a [`KeyTag`]. `None` if empty, unmatched, or otherwise unrepresentable.
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
        "Shift" => Some(KeyTag::Named(NamedKeyTag::Shift)),
        "Control" => Some(KeyTag::Named(NamedKeyTag::Control)),
        "Alt" => Some(KeyTag::Named(NamedKeyTag::Alt)),
        "Super" => Some(KeyTag::Named(NamedKeyTag::Super)),
        other => {
            // Multi-character strings that don't match a named key are rejected — so typos like "enter" or "escpe" surface as `parse_error` rather than being typed silently.
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
/// Returns `(response, actions)`: read-only commands return no actions; mutating commands return the [`PlannedAction`]s the transport must run before replying — keeps the dispatcher pure (no I/O, no `&mut` state).
pub fn dispatch_command<S: ControlState>(
    cmd: &Command,
    state: &S,
) -> (Response, Vec<PlannedAction>) {
    match cmd {
        Command::ListTargets => list_targets(state),
        Command::ClickAt { x, y } => (
            Response::Ok { frame: 0 },
            vec![
                PlannedAction::CursorMoved { x: *x, y: *y },
                PlannedAction::MouseLeftButton { pressed: true },
                PlannedAction::MouseLeftButton { pressed: false },
            ],
        ),
        Command::ClickNamed { name } => click_named(state, name),
        Command::Drag {
            x_from,
            y_from,
            x_to,
            y_to,
            steps,
        } => drag(*x_from, *y_from, *x_to, *y_to, *steps),
        Command::Key { key } => key_press_release(key),
        Command::Scroll { dx, dy } => (
            Response::Ok { frame: 0 },
            vec![PlannedAction::MouseWheel { dx: *dx, dy: *dy }],
        ),
        Command::KeyDown { key } | Command::KeyUp { key } => {
            key_down_or_up(key, matches!(cmd, Command::KeyDown { .. }))
        }
        Command::Type { text } => type_text(text),
        Command::Modifiers {
            ctrl,
            shift,
            alt,
            logo,
        } => modifiers(PlannedAction::SetModifiers {
            ctrl: *ctrl,
            shift: *shift,
            alt: *alt,
            logo: *logo,
        }),
        Command::Snapshot { path } => snapshot(path),
        Command::ForeignFrames => foreign_frames(state),
        Command::StateDump => state_dump(state),
        Command::Grant {
            subject,
            scope,
            verb,
            id,
        } => grant(state, subject, scope, verb, id.as_deref()),
        Command::Revoke { id } => (
            answer_or_err(state.revoke_capability(id), "revoke_refused"),
            Vec::new(),
        ),
        Command::CheckAction {
            actor,
            app,
            action,
            verb,
            destructive,
        } => check_action(state, actor, app, action, verb, *destructive),
        Command::PlaceNextWindow { app_id } => {
            crate::shell::launcher::set_pending_placement(
                app_id,
                crate::compositor::tab_binding::Placement::NewGroup,
            );
            (Response::Ok { frame: 0 }, Vec::new())
        }
        Command::PointerMove { x, y } => (
            Response::Ok { frame: 0 },
            vec![PlannedAction::CursorMoved { x: *x, y: *y }],
        ),
        Command::SetGroupWidth { permille } => (
            Response::Ok { frame: 0 },
            vec![PlannedAction::SetGroupWidth {
                permille: *permille,
            }],
        ),
        Command::PointerButton { pressed, button } => pointer_button(*pressed, *button),
        Command::ClipboardCopy { text, primary } => clipboard_copy(text, *primary),
        Command::ClipboardPaste { primary } => clipboard_paste(*primary),
        Command::ActionsEnumerate { query } => (
            answer_or_err(state.enumerate_actions(query), "enumerate_failed"),
            Vec::new(),
        ),
        Command::ActionsInvoke { provider, id, args } => actions_invoke(state, provider, id, args),
    }
}

/// [`Command::ListTargets`]: every addressable widget, sorted by name for deterministic output.
fn list_targets<S: ControlState>(state: &S) -> (Response, Vec<PlannedAction>) {
    let registry = state.widget_registry();
    let mut entries: Vec<TargetEntry> = registry
        .iter()
        .filter(|(_, r)| r.area() > 0.0)
        .map(|(name, rect)| TargetEntry::from_kv(name, *rect))
        .collect();
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    (Response::Targets(entries), Vec::new())
}

/// [`Command::ClickNamed`]: resolve `name` against the widget registry, then click its centre.
fn click_named<S: ControlState>(state: &S, name: &str) -> (Response, Vec<PlannedAction>) {
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

/// [`Command::Drag`]: press, interpolate `steps` motions, release.
fn drag(
    x_from: f64,
    y_from: f64,
    x_to: f64,
    y_to: f64,
    steps: Option<u32>,
) -> (Response, Vec<PlannedAction>) {
    let steps = steps.unwrap_or(8).max(1);
    // `steps` came off the wire uncapped; saturate the capacity hint instead of letting a huge value overflow the addition.
    let mut actions = Vec::with_capacity((steps as usize).saturating_add(4));
    actions.push(PlannedAction::CursorMoved {
        x: x_from,
        y: y_from,
    });
    actions.push(PlannedAction::MouseLeftButton { pressed: true });
    let n = f64::from(steps);
    for s in 0..steps {
        // steps==1 lands a single motion at x_from, avoiding divide-by-zero.
        let t = if steps == 1 {
            0.0
        } else {
            f64::from(s) / (n - 1.0)
        };
        let x = x_from + (x_to - x_from) * t;
        let y = y_from + (y_to - y_from) * t;
        actions.push(PlannedAction::CursorMoved { x, y });
    }
    actions.push(PlannedAction::MouseLeftButton { pressed: false });
    (Response::Ok { frame: 0 }, actions)
}

/// [`Command::Key`]: a press+release pair for one key.
fn key_press_release(key: &str) -> (Response, Vec<PlannedAction>) {
    match parse_key_tag(key) {
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
    }
}

/// [`Command::KeyDown`]/[`Command::KeyUp`]: hold or release one key.
fn key_down_or_up(key: &str, pressed: bool) -> (Response, Vec<PlannedAction>) {
    match parse_key_tag(key) {
        Some(tag) => (
            Response::Ok { frame: 0 },
            vec![PlannedAction::KeyboardKey { key: tag, pressed }],
        ),
        None => (
            Response::Err {
                code: "parse_error".to_string(),
                message: format!("unrecognised key name `{key}`"),
            },
            Vec::new(),
        ),
    }
}

/// [`Command::Type`]: a press+release pair per character, bracketed with Shift for characters that need it.
fn type_text(text: &str) -> (Response, Vec<PlannedAction>) {
    // A `KeyTag::Char` reaches a Wayland client as a bare evdev keycode with no case (35 is `h` or `H` purely by whether Shift is held), so Shift must bracket it like real hardware. Chrome's own widgets read the synthesised `text` field and show the capital regardless, so this bug is invisible there — a focused client gets the lowercase letter, and typing "Hello" lands as "hello".
    let mut actions = Vec::with_capacity(text.chars().count().saturating_mul(2));
    for c in text.chars() {
        let tag = KeyTag::Char(c);
        let shift = KeyTag::Named(NamedKeyTag::Shift);
        // Shifted punctuation needs the same bracket as letters — both reach the client as a keyboard position, not a character. See `client_input::unshifted`.
        let (_, shifted) = crate::compositor::client_input::unshifted(c);
        if shifted {
            actions.push(PlannedAction::KeyboardKey {
                key: shift,
                pressed: true,
            });
        }
        actions.push(PlannedAction::KeyboardKey {
            key: tag,
            pressed: true,
        });
        actions.push(PlannedAction::KeyboardKey {
            key: tag,
            pressed: false,
        });
        if shifted {
            actions.push(PlannedAction::KeyboardKey {
                key: shift,
                pressed: false,
            });
        }
    }
    (Response::Ok { frame: 0 }, actions)
}

/// [`Command::Modifiers`]: update the modifier mask carried into subsequent keyboard events.
///
/// Takes the already-built [`PlannedAction::SetModifiers`] rather than the
/// four bare `ctrl`/`shift`/`alt`/`logo` bools — the caller destructures
/// [`Command::Modifiers`], which has the same shape, so building the action
/// there and handing it over avoids a four-`bool` parameter list here.
fn modifiers(action: PlannedAction) -> (Response, Vec<PlannedAction>) {
    (Response::Ok { frame: 0 }, vec![action])
}

/// [`Command::Snapshot`]: pre-validate the path, then plan the paint+write.
fn snapshot(path: &str) -> (Response, Vec<PlannedAction>) {
    // Pre-validate empty paths: `File::create("")` fails with a confusing `ENOENT`, and the harness wants a stable `invalid_path` code. Other path issues surface from the executor as `eprintln!` logs.
    if path.is_empty() {
        (
            Response::Err {
                code: "invalid_path".to_string(),
                message: "snapshot path must not be empty".to_string(),
            },
            Vec::new(),
        )
    } else {
        (
            Response::Ok { frame: 0 },
            vec![PlannedAction::Snapshot {
                path: PathBuf::from(path),
            }],
        )
    }
}

/// [`Command::ForeignFrames`]: per-surface frame counters, read-only.
fn foreign_frames<S: ControlState>(state: &S) -> (Response, Vec<PlannedAction>) {
    (
        Response::ForeignFrames {
            frames: state.foreign_frames_json(),
        },
        Vec::new(),
    )
}

/// [`Command::StateDump`]: the persisted tree, chrome state and peers, read-only.
fn state_dump<S: ControlState>(state: &S) -> (Response, Vec<PlannedAction>) {
    (
        Response::State {
            workspace_tree: state.workspace_tree_json(),
            chrome: state.chrome_state_json(),
            peers: state.peers_json(),
        },
        Vec::new(),
    )
}

/// [`Command::Grant`]: grant a capability through the real authority check.
fn grant<S: ControlState>(
    state: &S,
    subject: &str,
    scope: &str,
    verb: &str,
    id: Option<&str>,
) -> (Response, Vec<PlannedAction>) {
    (
        answer_or_err(
            state.grant_capability(subject, scope, verb, id),
            "grant_refused",
        ),
        Vec::new(),
    )
}

/// [`Command::CheckAction`]: ask the real authority check whether `actor` may invoke the action.
fn check_action<S: ControlState>(
    state: &S,
    actor: &str,
    app: &str,
    action: &str,
    verb: &str,
    destructive: bool,
) -> (Response, Vec<PlannedAction>) {
    (
        answer_or_err(
            state.check_action(actor, app, action, verb, destructive),
            "check_failed",
        ),
        Vec::new(),
    )
}

/// [`Command::PointerButton`]: press or release a pointer button, `Left` taking the dedicated planned action.
fn pointer_button(pressed: bool, button: PointerButtonKind) -> (Response, Vec<PlannedAction>) {
    (
        Response::Ok { frame: 0 },
        vec![match button {
            PointerButtonKind::Left => PlannedAction::MouseLeftButton { pressed },
            other => PlannedAction::MousePointerButton {
                pressed,
                button: other,
            },
        }],
    )
}

/// [`Command::ClipboardCopy`]: copy `text` onto the clipboard or primary selection in-process.
fn clipboard_copy(text: &str, primary: bool) -> (Response, Vec<PlannedAction>) {
    if primary {
        ui::text_selection::clipboard_copy_primary(text);
    } else {
        ui::text_selection::clipboard_copy(text);
    }
    (Response::Ok { frame: 0 }, Vec::new())
}

/// [`Command::ClipboardPaste`]: read the clipboard or primary selection in-process.
fn clipboard_paste(primary: bool) -> (Response, Vec<PlannedAction>) {
    let text = if primary {
        ui::text_selection::clipboard_paste_primary()
    } else {
        ui::text_selection::clipboard_paste()
    };
    (Response::Clipboard { text }, Vec::new())
}

/// [`Command::ActionsInvoke`]: resolve the address against a fresh catalogue, then plan the invocation.
fn actions_invoke<S: ControlState>(
    state: &S,
    provider: &str,
    id: &str,
    args: &serde_json::Map<String, serde_json::Value>,
) -> (Response, Vec<PlannedAction>) {
    match state.resolve_action(provider, id, args) {
        Ok(resolved) => (
            Response::Answer(resolved),
            vec![PlannedAction::InvokeAction {
                provider: provider.to_string(),
                id: id.to_string(),
                args: args.clone(),
            }],
        ),
        Err(refusal) => (
            Response::Err {
                code: refusal.code.to_string(),
                message: refusal.message,
            },
            Vec::new(),
        ),
    }
}

/// Wrap a capability-op result in the wire envelope. A refusal is a [`Response::Err`], not an `Answer` with `allowed: false` — conflating them would read a refused *grant* as a successful deny.
fn answer_or_err(result: Result<serde_json::Value, String>, code: &str) -> Response {
    match result {
        Ok(value) => Response::Answer(value),
        Err(message) => Response::Err {
            code: code.to_string(),
            message,
        },
    }
}

/// Parse a `person:` / `app:` / `agent:` subject string.
///
/// # Errors
///
/// Returns a human-readable message when `s` has no `person:`/`app:`/`agent:` prefix.
pub fn parse_subject(s: &str) -> Result<crate::shell::state::Subject, String> {
    use crate::shell::state::Subject;
    match s.split_once(':') {
        Some(("person", id)) => Ok(Subject::person(id)),
        Some(("app", id)) => Ok(Subject::app(id)),
        Some(("agent", id)) => Ok(Subject::agent(id)),
        _ => Err(format!(
            "subject must be person:<id>, app:<id> or agent:<id>, got `{s}`"
        )),
    }
}

/// Parse a scope string against the room `room_key` names. `room_key` is the workspace's own id, the same key `Workspace::set_owner`/`role_of` use for `Scope::Room`, so a scope written here matches what the compositor writes.
///
/// # Errors
///
/// Returns a human-readable message when `s` matches none of `workspace`, `room`, `app:<id>`, `fs:<path>`, `device:<id>` or `action:<app>/<action>`.
pub fn parse_scope(s: &str, room_key: &str) -> Result<crate::shell::state::Scope, String> {
    use crate::shell::state::Scope;
    match s {
        "workspace" => return Ok(Scope::Workspace),
        "room" => return Ok(Scope::Room(room_key.to_string())),
        _ => {}
    }
    match s.split_once(':') {
        Some(("app", id)) => Ok(Scope::App(id.to_string())),
        Some(("fs", path)) => Ok(Scope::FsPath(path.to_string())),
        Some(("device", d)) => Ok(Scope::Device(d.to_string())),
        Some(("action", rest)) => match rest.split_once('/') {
            Some((app, action)) => Ok(Scope::Action {
                app: app.to_string(),
                action: action.to_string(),
            }),
            None => Err(format!(
                "action scope must be action:<app>/<action>, got `{s}`"
            )),
        },
        _ => Err(format!(
            "scope must be workspace, room, app:<id>, fs:<path>, device:<id> \
             or action:<app>/<action>, got `{s}`"
        )),
    }
}

/// Parse a verb name.
///
/// # Errors
///
/// Returns a human-readable message when `s` is not one of read / comment / write / invite / admin.
pub fn parse_verb(s: &str) -> Result<crate::shell::state::Verb, String> {
    use crate::shell::state::Verb;
    match s {
        "read" => Ok(Verb::Read),
        "comment" => Ok(Verb::Comment),
        "write" => Ok(Verb::Write),
        "invite" => Ok(Verb::Invite),
        "admin" => Ok(Verb::Admin),
        _ => Err(format!(
            "verb must be read / comment / write / invite / admin, got `{s}`"
        )),
    }
}

/// Parse a single JSON line into a [`Command`]. Returns a structured [`Response::Err`] on parse failure rather than bubbling a `serde_json::Error`, so clients see machine-readable error codes.
///
/// # Errors
///
/// Returns [`Response::Err`] with code `parse_error` when `line` is not valid JSON for a [`Command`].
#[allow(
    clippy::result_large_err,
    reason = "the Err payload is the wire Response enum itself (deliberately opaque JSON \
              variants like State); boxing it would ripple into its one external caller in \
              native.rs, out of scope for this change"
)]
pub fn parse_command(line: &str) -> Result<Command, Response> {
    serde_json::from_str::<Command>(line.trim()).map_err(|e| Response::Err {
        code: "parse_error".to_string(),
        message: format!("invalid JSON command: {e}"),
    })
}

/// Serialise a response to a single JSON line (the transport adds the newline).
pub fn serialise_response(resp: &Response) -> String {
    serde_json::to_string(resp).unwrap_or_else(|error| {
        // Every real `Response` is built from JSON-safe primitives; a failure here would mean
        // something upstream produced non-finite JSON (e.g. a NaN/Infinity rect coordinate).
        // Fall back to a same-shape wire error rather than aborting the whole compositor over
        // one malformed reply — this crate builds with `panic = "abort"`.
        tracing::error!(
            target: "shell::compositor::test_control",
            %error,
            "failed to serialise Response; sending fallback error"
        );
        r#"{"err":{"code":"serialise_error","message":"internal: response failed to serialise"}}"#
            .to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StaticState {
        registry: HashMap<String, Rect>,
    }

    impl StaticState {
        fn new(entries: &[(&str, Rect)]) -> Self {
            Self {
                registry: entries.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
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
    fn parse_pointer_button_defaults_to_left() {
        let cmd = parse_command(r#"{"op":"pointer_button","pressed":true}"#).unwrap();
        assert_eq!(
            cmd,
            Command::PointerButton {
                pressed: true,
                button: PointerButtonKind::Left,
            }
        );
    }

    #[test]
    fn parse_pointer_button_reads_the_right_button() {
        let cmd =
            parse_command(r#"{"op":"pointer_button","pressed":true,"button":"right"}"#).unwrap();
        assert_eq!(
            cmd,
            Command::PointerButton {
                pressed: true,
                button: PointerButtonKind::Right,
            }
        );
    }

    #[test]
    fn dispatching_a_right_click_plans_a_typed_button_not_the_left_one() {
        let state = StaticState::new(&[]);
        let (_, actions) = dispatch_command(
            &Command::PointerButton {
                pressed: true,
                button: PointerButtonKind::Right,
            },
            &state,
        );
        assert_eq!(
            actions,
            vec![PlannedAction::MousePointerButton {
                pressed: true,
                button: PointerButtonKind::Right,
            }],
            "a right-click must not plan as MouseLeftButton, or the \
             harness would be pressing the wrong button on the wire",
        );
    }

    #[test]
    fn dispatching_a_left_click_still_plans_the_original_variant() {
        let state = StaticState::new(&[]);
        let (_, actions) = dispatch_command(
            &Command::PointerButton {
                pressed: false,
                button: PointerButtonKind::Left,
            },
            &state,
        );
        assert_eq!(
            actions,
            vec![PlannedAction::MouseLeftButton { pressed: false }],
        );
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
        let err = parse_command(r"this is not json").unwrap_err();
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
        let (resp, actions) = dispatch_command(&Command::ClickAt { x: 42.0, y: 7.0 }, &state);
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
        assert_eq!(
            actions[0],
            PlannedAction::CursorMoved { x: 120.0, y: 210.0 }
        );
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
        // Intermediates use approximate equality to dodge f64 round-off; endpoints are exact integers.
        assert_eq!(actions.len(), 7);
        assert_eq!(actions[0], PlannedAction::CursorMoved { x: 0.0, y: 0.0 });
        assert_eq!(actions[1], PlannedAction::MouseLeftButton { pressed: true });
        assert_eq!(
            actions[6],
            PlannedAction::MouseLeftButton { pressed: false }
        );
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

    /// Asserts the exact action sequence, not just a count — a missing Shift bracket is invisible to chrome (reads `text`) and only shows up on the delivered key stream.
    #[test]
    fn typing_a_capital_holds_shift_around_it() {
        let state = StaticState::new(&[]);
        let (resp, actions) = dispatch_command(
            &Command::Type {
                text: "Hi".to_string(),
            },
            &state,
        );
        assert!(matches!(resp, Response::Ok { .. }));
        assert_eq!(
            actions,
            vec![
                PlannedAction::KeyboardKey {
                    key: KeyTag::Named(NamedKeyTag::Shift),
                    pressed: true,
                },
                PlannedAction::KeyboardKey {
                    key: KeyTag::Char('H'),
                    pressed: true,
                },
                PlannedAction::KeyboardKey {
                    key: KeyTag::Char('H'),
                    pressed: false,
                },
                PlannedAction::KeyboardKey {
                    key: KeyTag::Named(NamedKeyTag::Shift),
                    pressed: false,
                },
                PlannedAction::KeyboardKey {
                    key: KeyTag::Char('i'),
                    pressed: true,
                },
                PlannedAction::KeyboardKey {
                    key: KeyTag::Char('i'),
                    pressed: false,
                },
            ],
            "a capital is typed with Shift held across it, and a lowercase \
             letter is not — a bare keycode would reach the client as its \
             lowercase letter either way",
        );
    }

    #[test]
    fn typing_a_shifted_punctuation_mark_holds_shift_around_it() {
        let state = StaticState::new(&[]);
        let (_, actions) = dispatch_command(
            &Command::Type {
                text: ":".to_string(),
            },
            &state,
        );
        assert_eq!(
            actions,
            vec![
                PlannedAction::KeyboardKey {
                    key: KeyTag::Named(NamedKeyTag::Shift),
                    pressed: true,
                },
                PlannedAction::KeyboardKey {
                    key: KeyTag::Char(':'),
                    pressed: true,
                },
                PlannedAction::KeyboardKey {
                    key: KeyTag::Char(':'),
                    pressed: false,
                },
                PlannedAction::KeyboardKey {
                    key: KeyTag::Named(NamedKeyTag::Shift),
                    pressed: false,
                },
            ],
        );
    }

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

    #[test]
    fn parse_key_tag_rejects_empty_string() {
        assert!(parse_key_tag("").is_none());
    }

    #[test]
    fn snapshot_returns_ok_and_planned_action() {
        let state = StaticState::new(&[]);
        let (resp, actions) = dispatch_command(
            &Command::Snapshot {
                path: "/tmp/x.png".to_string(),
            },
            &state,
        );
        assert!(matches!(resp, Response::Ok { .. }));
        assert_eq!(
            actions,
            vec![PlannedAction::Snapshot {
                path: PathBuf::from("/tmp/x.png"),
            }]
        );
    }

    #[test]
    fn snapshot_with_empty_path_returns_error() {
        let state = StaticState::new(&[]);
        let (resp, actions) = dispatch_command(
            &Command::Snapshot {
                path: String::new(),
            },
            &state,
        );
        assert!(actions.is_empty());
        let Response::Err { code, .. } = resp else {
            panic!("expected Response::Err for empty snapshot path");
        };
        assert_eq!(code, "invalid_path");
    }

    #[test]
    fn parse_snapshot_command_round_trips() {
        let cmd = parse_command(r#"{"op":"snapshot","path":"/tmp/round.png"}"#).unwrap();
        assert_eq!(
            cmd,
            Command::Snapshot {
                path: "/tmp/round.png".to_string()
            }
        );
    }

    struct StateDumpMockState {
        workspace_tree: serde_json::Value,
        chrome: serde_json::Value,
    }

    impl ControlState for StateDumpMockState {
        fn widget_registry(&self) -> HashMap<String, Rect> {
            HashMap::new()
        }
        fn workspace_tree_json(&self) -> serde_json::Value {
            self.workspace_tree.clone()
        }
        fn chrome_state_json(&self) -> serde_json::Value {
            self.chrome.clone()
        }
    }

    #[test]
    fn parse_state_dump_command_round_trips() {
        let cmd = parse_command(r#"{"op":"state_dump"}"#).unwrap();
        assert_eq!(cmd, Command::StateDump);
    }

    #[test]
    fn state_dump_returns_state_response() {
        let state = StateDumpMockState {
            workspace_tree: serde_json::json!({
                "workspaces": [{ "id": "w-1", "name": "demo" }],
                "active": "w-1",
            }),
            chrome: serde_json::json!({
                "overlays": { "overview": false, "quick_settings": false },
                "revision": 7_u64,
            }),
        };
        let (resp, actions) = dispatch_command(&Command::StateDump, &state);
        assert!(actions.is_empty(), "state_dump must have no side effects");
        let Response::State {
            workspace_tree,
            chrome,
            peers: _,
        } = resp
        else {
            panic!("expected Response::State, got something else");
        };
        assert!(
            workspace_tree.get("workspaces").is_some(),
            "workspace_tree should carry `workspaces` key; got {workspace_tree:?}"
        );
        let workspaces = workspace_tree["workspaces"].as_array().unwrap();
        assert_eq!(workspaces.len(), 1);
        assert!(
            chrome.get("overlays").is_some(),
            "chrome should carry `overlays` key; got {chrome:?}"
        );
    }

    #[test]
    fn state_dump_default_impl_returns_nulls() {
        let state = StaticState::new(&[]);
        let (resp, actions) = dispatch_command(&Command::StateDump, &state);
        assert!(actions.is_empty());
        let Response::State {
            workspace_tree,
            chrome,
            peers: _,
        } = resp
        else {
            panic!("expected Response::State");
        };
        assert!(workspace_tree.is_null());
        assert!(chrome.is_null());
    }

    #[test]
    fn serialise_state_response_emits_workspace_tree_chrome_and_peers() {
        let resp = Response::State {
            workspace_tree: serde_json::json!({ "workspaces": [] }),
            chrome: serde_json::json!({ "overlays": {} }),
            peers: serde_json::json!({ "pointers": [] }),
        };
        let out = serialise_response(&resp);
        assert!(
            out.contains("\"state\""),
            "tag should be `state`; got {out}"
        );
        assert!(out.contains("\"workspace_tree\""), "got {out}");
        assert!(out.contains("\"chrome\""), "got {out}");
        assert!(out.contains("\"peers\""), "got {out}");
    }

    struct ActionState;

    impl ControlState for ActionState {
        fn widget_registry(&self) -> HashMap<String, Rect> {
            HashMap::new()
        }

        fn enumerate_actions(&self, query: &str) -> Result<serde_json::Value, String> {
            Ok(serde_json::json!({
                "query": query,
                "count": 1,
                "actions": [{
                    "provider": "shell.actions",
                    "id": "settings.open",
                    "kind": "Actions",
                    "title": "Open settings",
                    "subtitle": null,
                    "score": 0,
                }],
            }))
        }

        fn resolve_action(
            &self,
            provider: &str,
            id: &str,
            args: &serde_json::Map<String, serde_json::Value>,
        ) -> Result<serde_json::Value, ActionRefusal> {
            let params: &[crate::shell::search::action_api::ActionParam] = match (provider, id) {
                ("shell.actions", "settings.open") => &[],
                ("shell.actions", "text.insert") => MOCK_INSERT_PARAMS,
                _ => {
                    return Err(ActionRefusal::unknown(format!(
                        "no visible action addressed by ({provider}, {id})"
                    )));
                }
            };
            let values = arg_values_from_json(args)?;
            crate::shell::search::action_api::ActionArgs::bind(params, values)
                .map_err(|e| ActionRefusal::invalid_arguments(e.to_string()))?;
            Ok(serde_json::json!({
                "provider": provider,
                "id": id,
                "kind": "Actions",
                "title": "Open settings",
            }))
        }
    }

    const MOCK_INSERT_PARAMS: &[crate::shell::search::action_api::ActionParam] = &[
        crate::shell::search::action_api::ActionParam::required(
            "text",
            crate::shell::search::action_api::ParamType::Text,
            "what to write",
        ),
        crate::shell::search::action_api::ActionParam::optional(
            "at",
            crate::shell::search::action_api::ParamType::Integer,
            "character offset",
        ),
    ];

    fn invoke_with(id: &str, args: &serde_json::Value) -> Command {
        Command::ActionsInvoke {
            provider: "shell.actions".to_string(),
            id: id.to_string(),
            args: args.as_object().cloned().unwrap_or_default(),
        }
    }

    /// If enumeration could produce a `PlannedAction`, surveying the surface would mutate the session just by looking at it.
    #[test]
    fn enumerate_answers_without_planning_any_side_effect() {
        let (resp, actions) = dispatch_command(
            &Command::ActionsEnumerate {
                query: String::new(),
            },
            &ActionState,
        );

        assert!(
            actions.is_empty(),
            "enumeration must plan no actions; got {actions:?}"
        );
        let Response::Answer(payload) = resp else {
            panic!("expected Response::Answer, got {resp:?}");
        };
        assert_eq!(payload["count"], 1);
        assert_eq!(payload["actions"][0]["provider"], "shell.actions");
        assert_eq!(payload["actions"][0]["id"], "settings.open");
    }

    #[test]
    fn invoke_resolves_the_address_then_plans_one_invocation() {
        let (resp, actions) = dispatch_command(
            &invoke_with("settings.open", &serde_json::json!({})),
            &ActionState,
        );

        let Response::Answer(payload) = resp else {
            panic!("expected Response::Answer, got {resp:?}");
        };
        assert_eq!(payload["title"], "Open settings");
        assert_eq!(
            actions,
            vec![PlannedAction::InvokeAction {
                provider: "shell.actions".to_string(),
                id: "settings.open".to_string(),
                args: serde_json::Map::new(),
            }]
        );
    }

    #[test]
    fn a_nullary_invocation_needs_no_args_key() {
        let cmd = parse_command(
            r#"{"op":"actions_invoke","provider":"shell.actions","id":"settings.open"}"#,
        )
        .expect("an invocation without `args` must still parse");
        let (resp, actions) = dispatch_command(&cmd, &ActionState);
        assert!(matches!(resp, Response::Answer(_)), "got {resp:?}");
        assert_eq!(
            actions.len(),
            1,
            "the nullary path still plans one invocation"
        );
    }

    #[test]
    fn arguments_bind_and_reach_the_planned_invocation() {
        let (resp, actions) = dispatch_command(
            &invoke_with(
                "text.insert",
                &serde_json::json!({"text": "hello", "at": 3}),
            ),
            &ActionState,
        );
        assert!(matches!(resp, Response::Answer(_)), "got {resp:?}");
        let [PlannedAction::InvokeAction { args, .. }] = actions.as_slice() else {
            panic!("expected exactly one planned invocation, got {actions:?}");
        };
        assert_eq!(args["text"], "hello");
        assert_eq!(args["at"], 3);
    }

    /// The refusal code matters: `unknown_action` would send the caller looking for an address that's actually fine — the two mistakes are corrected differently.
    #[test]
    fn a_missing_required_argument_is_refused_under_its_own_code() {
        let (resp, actions) = dispatch_command(
            &invoke_with("text.insert", &serde_json::json!({})),
            &ActionState,
        );
        assert!(
            actions.is_empty(),
            "a refused invocation must plan nothing; got {actions:?}"
        );
        let Response::Err { code, message } = resp else {
            panic!("expected Response::Err, got {resp:?}");
        };
        assert_eq!(code, "invalid_arguments");
        assert!(
            message.contains("text"),
            "the refusal must name the parameter at fault, got {message:?}"
        );
    }

    /// Refused rather than coerced — guessing at intent is how a caller ends up believing an edit happened that didn't.
    #[test]
    fn a_wrong_typed_argument_is_refused_not_coerced() {
        let (resp, actions) = dispatch_command(
            &invoke_with("text.insert", &serde_json::json!({"text": 7})),
            &ActionState,
        );
        assert!(actions.is_empty(), "got {actions:?}");
        let Response::Err { code, message } = resp else {
            panic!("expected Response::Err, got {resp:?}");
        };
        assert_eq!(code, "invalid_arguments");
        assert!(
            message.contains("text") && message.contains("integer"),
            "{message}"
        );
    }

    /// Silently dropping it is worst: the caller believes it asked for something, gets `ok`, and the difference is invisible without eyes on the screen.
    #[test]
    fn an_unknown_argument_is_refused_not_silently_ignored() {
        let (resp, actions) = dispatch_command(
            &invoke_with(
                "text.insert",
                &serde_json::json!({"text": "hi", "colour": "red"}),
            ),
            &ActionState,
        );
        assert!(actions.is_empty(), "got {actions:?}");
        let Response::Err { code, message } = resp else {
            panic!("expected Response::Err, got {resp:?}");
        };
        assert_eq!(code, "invalid_arguments");
        assert!(message.contains("colour"), "{message}");
    }

    #[test]
    fn a_json_shape_with_no_parameter_type_is_refused() {
        for bad in [
            serde_json::json!({"text": null}),
            serde_json::json!({"text": ["a"]}),
            serde_json::json!({"text": "ok", "at": 1.5}),
        ] {
            let (resp, actions) = dispatch_command(&invoke_with("text.insert", &bad), &ActionState);
            assert!(actions.is_empty(), "got {actions:?}");
            let Response::Err { code, .. } = resp else {
                panic!("expected Response::Err, got {resp:?}");
            };
            assert_eq!(code, "invalid_arguments");
        }
    }

    /// Arguments are checked against the action the *address* resolves to, not the one the caller intended.
    #[test]
    fn arguments_meant_for_another_action_are_refused() {
        let (resp, actions) = dispatch_command(
            &invoke_with("settings.open", &serde_json::json!({"text": "hi"})),
            &ActionState,
        );
        assert!(actions.is_empty(), "got {actions:?}");
        let Response::Err { code, .. } = resp else {
            panic!("expected Response::Err, got {resp:?}");
        };
        assert_eq!(code, "invalid_arguments");
    }

    /// Something driving the shell without seeing it can't tell "ran and changed nothing" from "never ran" unless the wire says so.
    #[test]
    fn unknown_address_is_refused_and_plans_nothing() {
        let (resp, actions) = dispatch_command(
            &invoke_with("definitely.not.a.real.action", &serde_json::json!({})),
            &ActionState,
        );

        assert!(
            actions.is_empty(),
            "a refused address must plan no invocation; got {actions:?}"
        );
        let Response::Err { code, .. } = resp else {
            panic!("expected Response::Err, got {resp:?}");
        };
        assert_eq!(code, "unknown_action");
    }

    #[test]
    fn stateless_control_refuses_enumeration_rather_than_reporting_empty() {
        let state = StaticState::new(&[]);
        let (resp, actions) = dispatch_command(
            &Command::ActionsEnumerate {
                query: String::new(),
            },
            &state,
        );

        assert!(actions.is_empty());
        let Response::Err { code, .. } = resp else {
            panic!("a stateless control must refuse, not answer; got {resp:?}");
        };
        assert_eq!(code, "enumerate_failed");
    }

    #[test]
    fn action_ops_round_trip_over_the_wire() {
        let enumerate = parse_command(r#"{"op":"actions_enumerate","query":"settings"}"#)
            .expect("actions_enumerate should parse");
        assert_eq!(
            enumerate,
            Command::ActionsEnumerate {
                query: "settings".to_string()
            }
        );

        let bare = parse_command(r#"{"op":"actions_enumerate"}"#)
            .expect("actions_enumerate should parse without a query");
        assert_eq!(
            bare,
            Command::ActionsEnumerate {
                query: String::new()
            }
        );

        let invoke = parse_command(r#"{"op":"actions_invoke","provider":"p","id":"i"}"#)
            .expect("actions_invoke should parse");
        assert_eq!(
            invoke,
            Command::ActionsInvoke {
                provider: "p".to_string(),
                id: "i".to_string(),
                args: serde_json::Map::new(),
            }
        );

        let with_args = parse_command(
            r#"{"op":"actions_invoke","provider":"p","id":"i","args":{"text":"hi","at":2}}"#,
        )
        .expect("actions_invoke should parse with args");
        let Command::ActionsInvoke { args, .. } = with_args else {
            panic!("expected ActionsInvoke");
        };
        assert_eq!(args["text"], "hi");
        assert_eq!(args["at"], 2);
    }
}
