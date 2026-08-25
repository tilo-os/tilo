//! Real-input e2e test control surface — Phase 1 (pure-logic dispatch).
//!
//! Spec: `docs/superpowers/specs/2026-05-16-real-input-e2e-design.md`.
//!
//! The compositor exposes an opt-in JSON control protocol when launched
//! with `QOS_TEST_CONTROL=/run/user/N/nirios-test-N.sock`. This module
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
//!   click at raw coordinates. The pointer *travels* there from
//!   wherever it was: the last motion is always the target, and any
//!   before it are the trip across the screen.
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
//! - `{"op":"snapshot","path":"/tmp/X.png"}` — re-paint the current
//!   scene into a fresh `OffscreenRenderer` and write the resulting
//!   RGBA buffer as a PNG to `path`. Empty paths are rejected with
//!   `invalid_path`; everything else is left to the OS (the executor
//!   logs file-create / encode errors but doesn't crash the
//!   compositor — the response was already `Ok` from the dispatcher).
//! - `{"op":"state_dump"}` — read-only snapshot of the live
//!   [`shell::state::WorkspaceTree`] + a structured view of the
//!   [`ChromeState`] (overlays, search scope, ws-settings tab,
//!   revision, …). The response shape is `Response::State {
//!   workspace_tree, chrome, peers }`, each `serde_json::Value` so
//!   future fields surface without bumping the wire schema.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use vello::kurbo::Rect;

use crate::shell::search::action_api::ArgValue;

/// Which pointer button a [`Command::PointerButton`] means.
///
/// Spelled out rather than passed as a number: `3` is the right button in
/// X11's numbering, `BTN_RIGHT` is `0x111` in evdev's, and winit calls it
/// `MouseButton::Right`. A harness should not have to know which of those
/// this end speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PointerButtonKind {
    #[default]
    Left,
    Right,
    Middle,
}

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
    /// Synthesise a left-click at raw (x, y) — motion to the point,
    /// then a Left button Pressed + Released. The compositor's chrome
    /// receives the events through the same path winit input would.
    ///
    /// The motion is a path, not a jump, whenever the pointer is
    /// somewhere else already: see [`plan_travel`]. A click with no
    /// pointer movement in front of it is a click nobody watching can
    /// attribute to anything.
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
    /// Per-surface frame counters for every foreign toplevel the
    /// compositor is tracking. Read-only. Lets a harness distinguish an
    /// app that painted from one still showing the first-frame
    /// placeholder without guessing from a screenshot.
    ForeignFrames,
    /// Inject a single key press + release. `key` is a winit-style
    /// name — `"Enter"`, `"Escape"`, `"Tab"`, `"ArrowLeft"/Up/Right/Down`,
    /// `"Backspace"`, `"Delete"`, `"Home"`, `"End"`, `"PageUp"/Down`,
    /// `"Space"`, or a single character like `"a"`, `"1"`. Unknown
    /// names return a `parse_error`.
    Key { key: String },
    /// Press `key` and hold it. Pairs with [`Command::KeyUp`]. Same key
    /// names as [`Command::Key`], plus the bare modifiers `"Shift"`,
    /// `"Control"`, `"Alt"` and `"Super"` — holding a real modifier
    /// keycode across another keystroke is the only way to drive the
    /// seat's xkb modifier state the way hardware does.
    KeyDown { key: String },
    /// Release a key held by [`Command::KeyDown`].
    KeyUp { key: String },
    /// Scroll `(dx, dy)` wheel notches at the current cursor position.
    /// Positive `dy` scrolls up, matching winit's `LineDelta` sign
    /// convention (the compositor inverts it for the Wayland axis
    /// convention when forwarding to a client).
    Scroll { dx: f32, dy: f32 },
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
    /// Re-paint the current scene into a fresh `OffscreenRenderer` and
    /// write the resulting PNG to `path`. The dispatcher only
    /// pre-validates that `path` is non-empty (a clear client bug);
    /// any other I/O error (missing parent dir, permission denied,
    /// disk full, …) is reported via the executor's stderr and the
    /// response from the dispatcher remains `Ok` — proper error
    /// responses are a Phase 5 follow-up.
    Snapshot {
        path: String,
        /// Return as soon as the pixels have been captured, leaving the
        /// PNG encode and write to happen off the compositor's event
        /// loop. The file appears shortly afterwards; a caller that
        /// needs to know when every deferred frame is on disk asks for
        /// [`Command::DrainSnapshots`].
        ///
        /// Defaults to `false`, so a caller that says nothing keeps the
        /// original contract: when the reply arrives, the file is
        /// written. Every existing harness relies on that, and several
        /// assert on the file the moment the call returns.
        ///
        /// A recording is the case this exists for. Encoding is ~79% of
        /// a snapshot and needs nothing from the compositor, so a filmed
        /// session that waits for it spends four fifths of its capture
        /// budget on work the loop should never have been holding.
        #[serde(default)]
        defer: bool,
    },
    /// Block until every deferred snapshot has been written, then
    /// report whether all of them succeeded.
    ///
    /// The other half of `snapshot`'s `defer`. Returns
    /// [`Response::Drained`] with `ok: false` if any deferred write
    /// failed since the compositor started — a caller assembling a
    /// video wants that before it treats the directory as complete.
    DrainSnapshots,
    /// Snapshot the live shell state — the persisted [`WorkspaceTree`]
    /// plus a structured view of [`ChromeState`] (overlays, search
    /// scope, ws-settings tab, revision counter, …). Read-only — no
    /// [`PlannedAction`]s, no mutation. Returns a [`Response::State`]
    /// carrying both blobs as `serde_json::Value` so callers can
    /// `serde_json::from_value` to a typed mirror without the
    /// dispatcher locking the wire schema to a per-field snapshot.
    StateDump,
    /// Grant a capability on the active workspace, on behalf of the
    /// user sitting at this session, through `Workspace::grant_as`.
    ///
    /// The authority check is the production one: `grant_as` refuses a
    /// grantor who neither owns the room nor holds `Admin` over it, and
    /// this op reports that refusal rather than working around it.
    ///
    /// `subject` is `person:<id>` / `app:<id>` / `agent:<id>`; `scope`
    /// is `workspace`, `room`, `app:<id>` or `action:<app>/<action>`;
    /// `verb` is one of read / comment / write / invite / admin.
    Grant {
        subject: String,
        scope: String,
        verb: String,
        /// Stable capability id, so a later `revoke` can name it. One
        /// is minted when omitted.
        #[serde(default)]
        id: Option<String>,
    },
    /// Revoke the capability `id` on the active workspace, through
    /// `Workspace::revoke_as` — same authority check as `grant`.
    Revoke { id: String },
    /// Ask the in-app action gate (`shell::actions::check_action`)
    /// whether `actor` may invoke `app`'s `action`.
    ///
    /// The action's declaration comes from the request because that is
    /// where it comes from in production too: an app declares the verb
    /// its action needs, and the capability model answers about that
    /// verb. The answer is read out of the live workspace.
    CheckAction {
        actor: String,
        app: String,
        action: String,
        verb: String,
        #[serde(default)]
        destructive: bool,
    },
    /// Record that the next window announcing `app_id` should open in its own
    /// window group rather than as a tab in the focused one.
    ///
    /// Goes through `shell::launcher::set_pending_placement`, the same note
    /// the launcher leaves when it opens an app beside another — so this is
    /// the production placement path, not a test-only branch in the tree.
    /// A harness needs it because two client windows can only be visible at
    /// once when they are in separate groups, and side-by-side windows are the
    /// precondition for dragging anything between them.
    PlaceNextWindow { app_id: String },
    /// Move the pointer to `(x, y)` without pressing anything.
    ///
    /// [`Command::ClickAt`] and [`Command::Drag`] bundle press and release
    /// together, which cannot express a drag-and-drop: the source client has
    /// to be *holding* the button when it calls `start_drag`, and the drop
    /// only happens on a release that arrives after the pointer has moved
    /// over another client. These two ops decompose that.
    PointerMove { x: f64, y: f64 },
    /// Give every window group in the active strip the same share of
    /// it, in permille.
    ///
    /// A group takes about three quarters of its strip by default, so
    /// two of them never fit on screen at once and the second is
    /// scrolled off. A harness that needs to see two apps side by side
    /// — a peer's cursor crossing from one to the other is the case —
    /// has no other way to compose that scene. Drives the same
    /// `WindowWidth::Fraction` + doc mirror a width drag does.
    SetGroupWidth { permille: u16 },
    /// Move the window drawing `surface_id` onto the tab bar of the
    /// group already drawing `into_surface_id`, so the two share one
    /// window with a tab each.
    ///
    /// Tabbing here is a frame primitive the compositor owns rather
    /// than something an application does inside itself, so the pair
    /// can be two entirely different applications. The only way to
    /// compose that until now was to drag a tab pill onto another
    /// group's bar — a pointer path a harness has to stage exactly
    /// right, over two clients, and cannot check afterwards. Drives the
    /// same `GroupRow` splice the drag commits.
    MergeTabIntoGroup { surface_id: u64, into_surface_id: u64 },
    /// Press or release a pointer button at the current position.
    ///
    /// `button` defaults to the left one, so every existing caller keeps
    /// working unchanged. The right button exists because a context menu
    /// is opened with it, and a harness that cannot press it cannot drive
    /// the gesture this desktop hangs a good deal on -- select a thing,
    /// right-click, say something about it.
    PointerButton {
        pressed: bool,
        #[serde(default)]
        button: PointerButtonKind,
    },
    /// Copy `text` from **in-process chrome**, through the same
    /// `ui::text_selection::clipboard_copy` every hosted widget calls.
    ///
    /// This is how a harness drives the compositor's own side of a
    /// cross-process copy. It is not a shortcut around the production path:
    /// the call lands on whatever clipboard backend the process resolved,
    /// which in the compositor is the selection mirror, and the mirror is
    /// published as a real Wayland selection on the next tick.
    ClipboardCopy {
        text: String,
        /// `true` targets the primary selection instead of the clipboard.
        #[serde(default)]
        primary: bool,
    },
    /// Paste from in-process chrome, through
    /// `ui::text_selection::clipboard_paste`. Returns
    /// [`Response::Clipboard`] with the text the hosted widgets would
    /// receive, or `null` when the selection is empty.
    ClipboardPaste {
        /// `true` reads the primary selection instead of the clipboard.
        #[serde(default)]
        primary: bool,
    },
    /// Enumerate the actions and resources the shell exports right now,
    /// through the same provider registry that feeds the Ctrl+K palette.
    ///
    /// Read-only by construction: the reply carries typed descriptors and
    /// no invoke handles, so surveying "what can I do here" can never
    /// fire a side-effect. An empty `query` lists every provider's
    /// default set rather than guessing a needle.
    ///
    /// This is the command that makes the shell drivable by something
    /// that cannot see the screen — a descriptor is addressed by a
    /// stable `(provider, id)` pair, never by a screen position.
    ActionsEnumerate {
        #[serde(default)]
        query: String,
        /// Who is asking, when the caller is not the person at the
        /// keyboard.
        ///
        /// Omitted is the person: the rest of this socket types and
        /// clicks *as* them, and a harness driving their own machine is
        /// them. Present, it names an agent, and the surface is gated as
        /// that agent — scoped, revocable, holding no role — the same
        /// principal `apps/agent` announces on its `Hello` and the same
        /// one `agent_opens` decides about. It is an announcement, not
        /// proof: the name is unauthenticated here for exactly the
        /// reason it is there, and narrowing a grant to the participant
        /// it was written for is what it buys.
        #[serde(default)]
        agent: Option<String>,
    },
    /// Invoke one enumerated action by its stable `(provider, id)`
    /// address, with no window, no scene and no synthetic input.
    ///
    /// The address is resolved against a freshly built catalogue before
    /// anything runs, so an id that is unknown *or* hidden by the
    /// visibility gate is refused rather than silently ignored — you
    /// cannot invoke what you were never shown.
    ///
    /// `args` carries the action's parameters, as enumeration advertised
    /// them. It is checked against those parameters before anything
    /// runs: a missing, unknown or wrong-typed argument is an
    /// `invalid_arguments` refusal with the document untouched, which is
    /// deliberately a *different* code from `unknown_action` — "I
    /// addressed something that isn't there" and "I addressed the right
    /// thing wrongly" are different mistakes and a caller fixes them
    /// differently. Omitting `args` is the nullary call, unchanged.
    ActionsInvoke {
        provider: String,
        id: String,
        #[serde(default)]
        args: serde_json::Map<String, serde_json::Value>,
        /// Who is invoking. Same meaning as on
        /// [`Command::ActionsEnumerate`], and it has to be carried on
        /// both: enumeration filtered for one principal and invocation
        /// gated for another is the gap the visibility gate exists to
        /// close.
        #[serde(default)]
        agent: Option<String>,
    },
    /// Put a finished remark in the room, as though one had just been
    /// written in the compose surface and sent.
    ///
    /// **A harness door, not a product path.** The gesture a person makes
    /// is right-click an object and write a sentence beside it; that
    /// produces a [`qos_reference::Remark`] out of
    /// `RemarkCompose::submit`, and the frame loop carries it into the
    /// room. This command joins that path at the same place `submit` does
    /// — the host's outbox — and everything downstream of a finished
    /// remark runs unchanged: the roster is read, mentions are resolved,
    /// the message is minted and encoded as chat's own action, and it
    /// goes out over the broker.
    ///
    /// What it therefore does *not* exercise is the surface: the request
    /// arriving from an application, the panel opening beside the menu,
    /// the destination list being filled from the roster, the text being
    /// typed, and `submit` assembling the value. A run driven through
    /// here proves the room's half and says nothing about those.
    Remark {
        /// What is said. The mention text is resolved against the room's
        /// roster downstream, exactly as a typed one is.
        text: String,
        /// The address of the thing being talked about — `files.entry`,
        /// `word_processor.block`. Same vocabulary an app publishes its
        /// resources under.
        kind: String,
        /// The resource's id in that app's vocabulary.
        id: String,
        /// The chip's human label. Never the address; defaults to the id's
        /// last path segment when it is left out.
        #[serde(default)]
        label: Option<String>,
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
    /// `state_dump` response — the persisted [`WorkspaceTree`] +
    /// structured [`ChromeState`] view, both as opaque
    /// [`serde_json::Value`]s. Callers either inspect specific keys
    /// (`response["workspace_tree"]["workspaces"]`) or
    /// `serde_json::from_value` into a typed mirror.
    ///
    /// `serde_json::Value` is `PartialEq` so the variant participates
    /// in the `PartialEq` derive — same as the rest of the response
    /// envelope.
    /// `foreign_frames` response - one entry per tracked foreign
    /// surface, opaque `serde_json::Value` so the shape can grow.
    ForeignFrames { frames: serde_json::Value },
    State {
        workspace_tree: serde_json::Value,
        chrome: serde_json::Value,
        /// The peers this session would paint, straight off the
        /// `PresenceCtx` the overlay reads.
        peers: serde_json::Value,
    },
    /// Answer to a capability query or administration command
    /// (`grant`, `revoke`, `check_action`). Opaque JSON so the shape
    /// can grow without a wire break.
    Answer(serde_json::Value),
    /// `clipboard_paste` response — what in-process chrome reads off the
    /// selection right now. `None` when nothing has been copied, which is
    /// distinct from `Some("")`: a client may own an empty selection.
    Clipboard { text: Option<String> },
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
    /// JSON view of the persisted [`WorkspaceTree`] — same shape
    /// `NIRIOS_TEST_STATE_DUMP` writes to disk on every frame tick.
    ///
    /// Default impl returns [`serde_json::Value::Null`] so existing
    /// mock `ControlState`s (the unit-test fixtures that only care
    /// about `widget_registry`) don't have to spell out a workspace
    /// tree they never inspect. The compositor's real impl overrides
    /// this with `serde_json::to_value(chrome.tree)`.
    fn workspace_tree_json(&self) -> serde_json::Value {
        serde_json::Value::Null
    }
    /// JSON view of [`ChromeState`]. The compositor's real impl
    /// constructs an object literal carrying `overlays`,
    /// `search_scope`, `ws_settings_tab`, `revision`, and
    /// `editing_workspace_name`; this surface stays additive across
    /// versions because `Response::State::chrome` is opaque
    /// `serde_json::Value`.
    ///
    /// Default impl returns [`serde_json::Value::Null`] so test
    /// fixtures that don't exercise `state_dump` stay terse.
    fn chrome_state_json(&self) -> serde_json::Value {
        serde_json::Value::Null
    }
    /// The peers this session would paint right now, read from the
    /// same `PresenceCtx` the overlay draws from.
    ///
    /// Exposed because a peer cursor is otherwise only assertable by
    /// looking at a picture, and "no peer arrived" and "a peer arrived
    /// and was not drawn" produce the same picture. A harness that
    /// cannot tell those apart cannot prove either.
    ///
    /// Default impl returns [`serde_json::Value::Null`] so mock
    /// fixtures stay terse.
    fn peers_json(&self) -> serde_json::Value {
        serde_json::Value::Null
    }
    /// Where this session's pointer is right now, or `None` if it has
    /// never been anywhere.
    ///
    /// Read by the click ops so a synthesised click **travels** to its
    /// target instead of appearing there. A pointer that teleports is
    /// not a pointer anybody can follow: on a recording the control
    /// activates with nothing having crossed the screen to reach it,
    /// and every interaction reads as the machine operating itself.
    ///
    /// Default `None` — a fixture with no live compositor has no
    /// pointer, and the ops then move once to the target, which is
    /// where the pointer has to end up either way.
    fn pointer(&self) -> Option<(f64, f64)> {
        None
    }

    /// Per-surface frame ground truth: what the compositor itself knows
    /// about whether each foreign toplevel has ever presented.
    ///
    /// A harness that wants to say "this app rendered" must not have to
    /// infer it from pixels. The placeholder tile is the app-id over a
    /// flat card, which is near-identical between any two apps and, for
    /// an app that legitimately starts empty, near-identical to a real
    /// first frame. `frame_seq > 0` is the fact behind that guess.
    ///
    /// Default is an empty list so fixtures that don't exercise it stay
    /// terse.
    fn foreign_frames_json(&self) -> serde_json::Value {
        serde_json::Value::Array(Vec::new())
    }

    /// Grant a capability on the active workspace on behalf of the
    /// local user, via `Workspace::grant_as`.
    ///
    /// Default impl refuses: a `ControlState` that has no live tree has
    /// no room to administer, and answering anything else would make a
    /// harness think it had proved something about authority.
    fn grant_capability(
        &self,
        _subject: &str,
        _scope: &str,
        _verb: &str,
        _id: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        Err("no live workspace on this control state".to_string())
    }

    /// Revoke a capability by id on the active workspace, via
    /// `Workspace::revoke_as`.
    fn revoke_capability(&self, _id: &str) -> Result<serde_json::Value, String> {
        Err("no live workspace on this control state".to_string())
    }

    /// Run `shell::actions::check_action` against the active workspace.
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

    /// Enumerate the exported action surface for `query` as JSON.
    ///
    /// Default impl refuses for the same reason the capability methods
    /// do: a `ControlState` with no live registry and no document has
    /// nothing to enumerate, and returning an empty list would let a
    /// harness read "this shell exports nothing" out of "this fixture
    /// cannot answer". Those are different facts and must not share a
    /// wire shape.
    fn enumerate_actions(
        &self,
        _query: &str,
        _agent: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        Err("no live action registry on this control state".to_string())
    }

    /// Resolve `(provider, id)` against a freshly built catalogue and
    /// report what it addresses, without invoking it.
    ///
    /// This is the validation half of `actions_invoke`: it answers
    /// "is this address real, may this caller see it, and do these
    /// arguments fit it?" so the transport can refuse with a structured
    /// error instead of planning an invocation that silently does
    /// nothing — or, worse, one that runs with an argument the caller
    /// asked for and this side quietly dropped.
    fn resolve_action(
        &self,
        _provider: &str,
        _id: &str,
        _args: &serde_json::Map<String, serde_json::Value>,
        _agent: Option<&str>,
    ) -> Result<serde_json::Value, ActionRefusal> {
        Err(ActionRefusal::unknown(
            "no live action registry on this control state",
        ))
    }
}

/// A refused invocation, carrying the code the wire should report.
///
/// A plain string would collapse "that address does not exist for you"
/// into "you passed the wrong arguments", and those are corrected
/// differently: the first means look again at what you may see, the
/// second means look again at what this action declared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionRefusal {
    pub code: &'static str,
    pub message: String,
}

impl ActionRefusal {
    /// The address names nothing this caller can see. Deliberately the
    /// same answer for "no such action" and "hidden from you", so
    /// probing cannot map what it may not see.
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

    /// The address and arguments are both good, and the action could
    /// still not do its job.
    pub fn failed(message: impl Into<String>) -> Self {
        Self {
            code: "action_failed",
            message: message.into(),
        }
    }
}

/// Translate one JSON argument object into the action vocabulary's
/// values.
///
/// Kept at the transport rather than inside the action API so the
/// catalogue never learns a wire format. JSON shapes with no parameter
/// type — null, arrays, objects, non-integer numbers — are refused here
/// by name rather than coerced: a caller that sent `{"at": 1.5}` asked
/// for something this surface cannot mean, and rounding it silently
/// would be inventing intent.
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
    /// Press or release a pointer button that is not the left one.
    ///
    /// Kept separate from [`Self::MouseLeftButton`] rather than folded
    /// into it: the left button is planned from a dozen call sites here
    /// and in `native.rs`, and widening all of them to carry a value they
    /// would every one of them pass as `Left` is churn that buys nothing.
    MousePointerButton {
        pressed: bool,
        button: PointerButtonKind,
    },
    /// Inject a keyboard press or release for `key`. The transport
    /// translates `KeyTag` to `winit::keyboard::Key` and synthesises
    /// a `WindowEvent::KeyboardInput`.
    KeyboardKey { key: KeyTag, pressed: bool },
    /// Inject a discrete wheel scroll of `(dx, dy)` notches at the
    /// current cursor position. The transport synthesises a
    /// `WindowEvent::MouseWheel` carrying a `LineDelta`, the same shape
    /// winit reports for a mouse wheel.
    MouseWheel { dx: f32, dy: f32 },
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
    /// Re-paint the current scene off-screen and write the RGBA buffer
    /// to `path` as a PNG. Carried as a `PathBuf` rather than a
    /// `String` so the executor doesn't have to re-convert at every
    /// invocation. Failures are logged via `eprintln!` — they don't
    /// propagate to the wire response (Phase 5 will).
    Snapshot { path: PathBuf, defer: bool },
    /// Block until every deferred snapshot has been written. Planned
    /// rather than answered in `dispatch_command` because the encoder
    /// pool lives with the executor, not with the wire.
    DrainSnapshots,
    /// Give every live window group the same share of its strip, in
    /// permille. Applied against the widget tree because that is where
    /// the laid-out width lives.
    SetGroupWidth { permille: u16 },
    /// Hand one finished remark to the host's outbox, where the compose
    /// surface puts the ones a person writes. Planned rather than done in
    /// `dispatch_command` because the outbox lives on the widget tree,
    /// which dispatch only holds immutably.
    Remark { remark: qos_reference::Remark },
    /// Move the tab drawing `surface_id` onto the bar of the group
    /// drawing `into_surface_id`. Planned rather than executed in
    /// `dispatch_command` because it mutates both the widget tree and
    /// the persisted one, and dispatch only holds them immutably.
    MergeTabIntoGroup { surface_id: u64, into_surface_id: u64 },
    /// Run one already-validated action against the live CRDT.
    ///
    /// Carried as a planned action rather than executed inside
    /// `dispatch_command` because invoking mutates the document, and the
    /// dispatch layer only ever holds it immutably. The address was
    /// resolved and permission-checked before this value was produced;
    /// the executor re-resolves against a fresh catalogue because the
    /// document may have moved on between planning and execution.
    ///
    /// `args` travels with it, still as the JSON the caller sent. It was
    /// checked against the action's parameters during dispatch, and is
    /// checked again on the freshly-resolved action before it runs —
    /// the re-resolution can land on a different action, and arguments
    /// validated against the old one would say nothing about the new.
    InvokeAction {
        provider: String,
        id: String,
        args: serde_json::Map<String, serde_json::Value>,
        /// The announced caller, carried through to the re-resolution.
        /// The executor rebuilds the catalogue through the gate, and a
        /// rebuild that forgot who asked would hand an agent the
        /// person's surface at exactly the moment something runs.
        agent: Option<String>,
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
    /// Bare modifier keys. Reachable only through `key_down` / `key_up`
    /// (a `key` press+release pair would release the modifier before the
    /// next key arrives), which is what lets a harness hold a real
    /// modifier across another keystroke — the same shape hardware
    /// produces, and the only way to exercise the seat's xkb modifier
    /// tracking end-to-end.
    Shift,
    Control,
    Alt,
    Super,
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
        "Shift" => Some(KeyTag::Named(NamedKeyTag::Shift)),
        "Control" => Some(KeyTag::Named(NamedKeyTag::Control)),
        "Alt" => Some(KeyTag::Named(NamedKeyTag::Alt)),
        "Super" => Some(KeyTag::Named(NamedKeyTag::Super)),
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

/// Longest step a travelling pointer takes between two frames of its
/// path, in logical pixels.
///
/// Small enough that the path reads as a movement rather than as a
/// handful of jumps, and large enough that crossing a 1920px desktop
/// stays a couple of dozen events rather than hundreds.
const TRAVEL_STEP_PX: f64 = 48.0;

/// Most motions one travel is allowed to plan.
///
/// A corner-to-corner trip on a large desktop is what sets this; past
/// it the step simply grows. The cap is here so the action list a
/// single command produces stays bounded no matter how far apart two
/// targets are.
const TRAVEL_MAX_STEPS: u32 = 24;

/// Below this the pointer is already there and a path would be noise.
const TRAVEL_MIN_PX: f64 = 2.0;

/// Plan the motions that carry the pointer from `from` to `(to_x, to_y)`.
///
/// Always ends with a motion **exactly at the target**, whether or not
/// anything precedes it — that last motion is the one the press needs,
/// and it is the invariant every caller relies on. What comes before it
/// is the trip: a pointer that was somewhere else moves through the
/// intervening space rather than reappearing at the destination.
///
/// Eased rather than linear (a smoothstep on the parameter), because a
/// hand does not start and stop at full speed. It costs nothing here —
/// the same number of points, spaced differently — and it is the
/// difference between a path that reads as somebody moving a mouse and
/// one that reads as a linear interpolation, which is what it is.
fn plan_travel(from: Option<(f64, f64)>, to_x: f64, to_y: f64) -> Vec<PlannedAction> {
    let Some((fx, fy)) = from else {
        return vec![PlannedAction::CursorMoved { x: to_x, y: to_y }];
    };
    let (dx, dy) = (to_x - fx, to_y - fy);
    let distance = dx.hypot(dy);
    if !distance.is_finite() || distance < TRAVEL_MIN_PX {
        return vec![PlannedAction::CursorMoved { x: to_x, y: to_y }];
    }
    let steps = ((distance / TRAVEL_STEP_PX).ceil() as u32)
        .clamp(1, TRAVEL_MAX_STEPS);
    let n = steps as f64;
    (1..=steps)
        .map(|i| {
            let t = i as f64 / n;
            // smoothstep: slow away from the start, slow into the target.
            let e = t * t * (3.0 - 2.0 * t);
            PlannedAction::CursorMoved {
                x: fx + dx * e,
                y: fy + dy * e,
            }
        })
        .collect()
}

/// A click: travel to `(x, y)`, then press and release there.
///
/// The one constructor both click ops go through, so there is no way to
/// plan a click that does not first put the pointer where it is about
/// to happen. That asymmetry is what went wrong before — the easy call
/// was the invisible one — and a shared builder is what stops a future
/// op from re-introducing it.
fn plan_click(from: Option<(f64, f64)>, x: f64, y: f64) -> Vec<PlannedAction> {
    let mut actions = plan_travel(from, x, y);
    actions.push(PlannedAction::MouseLeftButton { pressed: true });
    actions.push(PlannedAction::MouseLeftButton { pressed: false });
    actions
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
            plan_click(state.pointer(), *x, *y),
        ),
        Command::ClickNamed { name } => {
            let registry = state.widget_registry();
            match registry.get(name) {
                Some(rect) if rect.area() > 0.0 => {
                    let c = rect.center();
                    (
                        Response::Ok { frame: 0 },
                        plan_click(state.pointer(), c.x, c.y),
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
            actions.extend(plan_travel(state.pointer(), *x_from, *y_from));
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
        Command::Scroll { dx, dy } => (
            Response::Ok { frame: 0 },
            vec![PlannedAction::MouseWheel { dx: *dx, dy: *dy }],
        ),
        Command::KeyDown { key } | Command::KeyUp { key } => {
            let pressed = matches!(cmd, Command::KeyDown { .. });
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
        Command::Type { text } => {
            // A `KeyTag::Char` reaches a Wayland client as a bare evdev
            // keycode (`client_input::evdev_from_char`, which lowercases
            // before it looks the character up), and a keycode carries no
            // case: 35 is `h` or `H` purely by whether Shift is held at
            // the time. Real hardware holds it, so this must too.
            //
            // Getting this wrong is quiet rather than loud, which is why
            // it survived: the chrome's own widgets read the synthesised
            // event's `text` field and so show the capital regardless,
            // while a focused client — the case that matters, since that
            // is where a document lives — silently receives the lowercase
            // letter. A scripted `type` of "Hello" lands as "hello".
            let mut actions = Vec::with_capacity(text.chars().count() * 2);
            for c in text.chars() {
                let tag = KeyTag::Char(c);
                let shift = KeyTag::Named(NamedKeyTag::Shift);
                // Letters *and* the shifted punctuation marks. Both reach a
                // client as a position on the keyboard rather than as a
                // character, so both need the same bracket — see
                // `client_input::unshifted`, which is the one place that
                // knows which key a character sits on.
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
        Command::DrainSnapshots => (Response::Ok { frame: 0 }, vec![PlannedAction::DrainSnapshots]),
        Command::Snapshot { path, defer } => {
            // Pre-validate empty paths — `File::create("")` would
            // fail with a confusing `ENOENT` message, and the harness
            // wants a stable `invalid_path` code it can branch on.
            // Other path issues (missing parent dir, permission, …)
            // surface from the executor as `eprintln!` logs — Phase 5
            // upgrades those to wire responses.
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
                        defer: *defer,
                    }],
                )
            }
        }
        Command::ForeignFrames => (
            Response::ForeignFrames {
                frames: state.foreign_frames_json(),
            },
            Vec::new(),
        ),
        Command::StateDump => {
            // `state_dump` is read-only: pull both JSON views from the
            // `ControlState` impl and return them verbatim. No
            // `PlannedAction`s — the live binary already sees the
            // post-mutation state via its `FrameBuilderControlState`,
            // and tests inject a fixed `ControlState` snapshot.
            (
                Response::State {
                    workspace_tree: state.workspace_tree_json(),
                    chrome: state.chrome_state_json(),
                    peers: state.peers_json(),
                },
                Vec::new(),
            )
        }
        Command::Grant {
            subject,
            scope,
            verb,
            id,
        } => (
            answer_or_err(
                state.grant_capability(subject, scope, verb, id.as_deref()),
                "grant_refused",
            ),
            Vec::new(),
        ),
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
        } => (
            answer_or_err(
                state.check_action(actor, app, action, verb, *destructive),
                "check_failed",
            ),
            Vec::new(),
        ),
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
        Command::MergeTabIntoGroup {
            surface_id,
            into_surface_id,
        } => (
            Response::Ok { frame: 0 },
            vec![PlannedAction::MergeTabIntoGroup {
                surface_id: *surface_id,
                into_surface_id: *into_surface_id,
            }],
        ),
        Command::PointerButton { pressed, button } => (
            Response::Ok { frame: 0 },
            vec![match button {
                PointerButtonKind::Left => {
                    PlannedAction::MouseLeftButton { pressed: *pressed }
                }
                other => PlannedAction::MousePointerButton {
                    pressed: *pressed,
                    button: *other,
                },
            }],
        ),
        // Both clipboard ops call straight into `ui`, exactly as a hosted
        // widget does, rather than reaching into the compositor's mirror
        // directly. Going through `ui` is the point: it exercises the backend
        // resolution in `ui::clipboard` and the bridge the compositor
        // installed into it, which is the pair that was broken.
        Command::ClipboardCopy { text, primary } => {
            if *primary {
                ui::text_selection::clipboard_copy_primary(text);
            } else {
                ui::text_selection::clipboard_copy(text);
            }
            (Response::Ok { frame: 0 }, Vec::new())
        }
        Command::ClipboardPaste { primary } => {
            let text = if *primary {
                ui::text_selection::clipboard_paste_primary()
            } else {
                ui::text_selection::clipboard_paste()
            };
            (Response::Clipboard { text }, Vec::new())
        }
        Command::ActionsEnumerate { query, agent } => (
            answer_or_err(
                state.enumerate_actions(query, agent.as_deref()),
                "enumerate_failed",
            ),
            Vec::new(),
        ),
        Command::ActionsInvoke {
            provider,
            id,
            args,
            agent,
        } => {
            // Resolve before planning. An unknown or hidden address, and
            // an argument set that does not fit the action, are both
            // structured refusals here rather than an invocation that
            // quietly does nothing — "I ran it and nothing happened",
            // "that action does not exist for you" and "you passed the
            // wrong thing" must not look alike to a caller that cannot
            // see the screen.
            match state.resolve_action(provider, id, args, agent.as_deref()) {
                Ok(resolved) => (
                    Response::Answer(resolved),
                    vec![PlannedAction::InvokeAction {
                        provider: provider.clone(),
                        id: id.clone(),
                        args: args.clone(),
                        agent: agent.clone(),
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
        Command::Remark {
            text,
            kind,
            id,
            label,
        } => {
            let text = text.trim();
            if text.is_empty() || id.is_empty() || kind.is_empty() {
                // The same three refusals `submit` makes by returning, said
                // out loud. A caller that cannot see the screen has no other
                // way to tell "nothing was sent" from "sent and ignored".
                (
                    Response::Err {
                        code: "invalid_remark".to_string(),
                        message: "a remark needs text and a kind:id address"
                            .to_string(),
                    },
                    Vec::new(),
                )
            } else {
                let label = label.clone().unwrap_or_else(|| {
                    id.rsplit(['/', ':'])
                        .next()
                        .unwrap_or(id.as_str())
                        .to_string()
                });
                let reference =
                    qos_reference::ResourceRef::whole(kind.clone(), id.clone(), label);
                // The room's conversation, which is what the surface opens
                // showing and what a remark to an agent has to use anyway:
                // an instruction that will change a shared document must be
                // readable by the people sharing it.
                let destination = ui::widget::remark_compose::room_destination()
                    .destination;
                (
                    Response::Ok { frame: 0 },
                    vec![PlannedAction::Remark {
                        remark: qos_reference::Remark::new(text, reference, destination),
                    }],
                )
            }
        }
    }
}

/// Wrap a capability-op result in the wire envelope.
///
/// A refusal is an [`Response::Err`], not an `Answer` carrying
/// `allowed: false`: "you are not allowed to administer this room" and
/// "the thing you asked about is denied" are different answers, and a
/// harness that conflated them would read a refused *grant* as a
/// successful one that happened to deny.
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

/// Parse a scope string against the room `room_key` stands for.
///
/// `room_key` is the workspace's own id as a string — the same key
/// `Workspace::set_owner` and `Workspace::role_of` use for
/// `Scope::Room`, so a scope written here matches the ones the
/// compositor writes itself.
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
            None => Err(format!("action scope must be action:<app>/<action>, got `{s}`")),
        },
        _ => Err(format!(
            "scope must be workspace, room, app:<id>, fs:<path>, device:<id> \
             or action:<app>/<action>, got `{s}`"
        )),
    }
}

/// Parse a verb name.
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
        /// Where the fixture's pointer is. `None` — never been anywhere
        /// — is the default, which is the state a freshly started
        /// session is really in.
        pointer: Option<(f64, f64)>,
    }

    impl StaticState {
        fn new(entries: &[(&str, Rect)]) -> Self {
            Self {
                registry: entries.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
                pointer: None,
            }
        }

        /// The same fixture with a pointer already resting somewhere.
        fn with_pointer(entries: &[(&str, Rect)], at: (f64, f64)) -> Self {
            Self {
                pointer: Some(at),
                ..Self::new(entries)
            }
        }
    }

    impl ControlState for StaticState {
        fn widget_registry(&self) -> HashMap<String, Rect> {
            self.registry.clone()
        }

        fn pointer(&self) -> Option<(f64, f64)> {
            self.pointer
        }
    }

    /// Every `CursorMoved` in `actions`, in order.
    fn motions(actions: &[PlannedAction]) -> Vec<(f64, f64)> {
        actions
            .iter()
            .filter_map(|a| match a {
                PlannedAction::CursorMoved { x, y } => Some((*x, *y)),
                _ => None,
            })
            .collect()
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

    /// A session whose pointer has never been anywhere has nowhere to
    /// travel *from*, so the click is one motion to the target and the
    /// press pair — the shape this op has always had.
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
        // Centre of (100, 200, 40, 20) is (120, 210).
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

    /// Typing a capital asks the client for a capital.
    ///
    /// `KeyTag::Char` reaches a Wayland client as a bare evdev keycode
    /// (`client_input::evdev_from_char` lowercases before its table
    /// lookup, and `keyboard_input_event` leaves `physical_key`
    /// unidentified so there is no scancode to prefer). A keycode has no
    /// case of its own: keycode 35 is `h` or `H` according to whether
    /// Shift is held. Without the bracket, a scripted `type` of "Hi"
    /// arrives in the focused document as "hi".
    ///
    /// This is invisible from the chrome side — chrome widgets read the
    /// synthesised event's `text` field, which carries the capital — so
    /// only a test on the delivered key stream catches it. Asserting the
    /// exact action sequence, rather than a count, is what makes the
    /// missing Shift show up at all.
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

    /// A colon is a key with Shift on it, and it has to arrive.
    ///
    /// This is the one that was silently wrong. The shifted punctuation marks
    /// had no entry in `evdev_from_char` at all, so a `:` resolved to no
    /// keycode and was never delivered — the letters either side of it
    /// arrived and it did not. Nothing said so: the chrome's own widgets read
    /// the event's `text` field and showed the colon, while the focused
    /// client, which is where a document lives, received the characters
    /// around a hole.
    ///
    /// What it cost is worth naming, because it is the reason this test is
    /// here rather than a nicety. An agent takes its instructions from a
    /// paragraph beginning with its name and a colon
    /// (`plan::Instruction::addressed` matches the literal prefix). A
    /// scripted `Claude: make this square` reached the document as
    /// `Claude make this square`, so no instruction was ever addressed to the
    /// agent, and the agent's report — that it saw the whole document and
    /// nothing in it was for it — was correct about a document nobody had
    /// managed to write.
    #[test]
    fn typing_a_colon_holds_shift_and_names_the_key_it_sits_on() {
        let state = StaticState::new(&[]);
        let (resp, actions) = dispatch_command(
            &Command::Type {
                text: ":".to_string(),
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
            "a colon is Shift held across the semicolon key, the same bracket              a capital gets",
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

    // ===================================================================
    // Phase 4 — snapshot
    // ===================================================================

    /// `Snapshot { path: "/tmp/x.png" }` returns `Ok` and a single
    /// `PlannedAction::Snapshot` carrying the same path as a
    /// `PathBuf`. The executor will turn that into an
    /// `OffscreenRenderer` render + PNG write.
    #[test]
    fn snapshot_returns_ok_and_planned_action() {
        let state = StaticState::new(&[]);
        let (resp, actions) = dispatch_command(
            &Command::Snapshot {
                path: "/tmp/x.png".to_string(),
                defer: false,
            },
            &state,
        );
        assert!(matches!(resp, Response::Ok { .. }));
        assert_eq!(
            actions,
            vec![PlannedAction::Snapshot {
                path: PathBuf::from("/tmp/x.png"),
                defer: false,
            }]
        );
    }

    /// An empty `path` is a client bug — the dispatcher returns the
    /// stable `invalid_path` error code so the harness can branch on
    /// it. No actions are produced; the OS never sees the malformed
    /// request.
    #[test]
    fn snapshot_with_empty_path_returns_error() {
        let state = StaticState::new(&[]);
        let (resp, actions) = dispatch_command(
            &Command::Snapshot {
                path: String::new(),
                defer: false,
            },
            &state,
        );
        assert!(actions.is_empty());
        let Response::Err { code, .. } = resp else {
            panic!("expected Response::Err for empty snapshot path");
        };
        assert_eq!(code, "invalid_path");
    }

    /// Round-trip the wire format — `{"op":"snapshot","path":"..."}`
    /// must deserialise to the `Snapshot` variant. Guards against
    /// `serde(rename_all = "snake_case")` accidentally changing the
    /// field name visible to clients.
    #[test]
    fn parse_snapshot_command_round_trips() {
        let cmd = parse_command(r#"{"op":"snapshot","path":"/tmp/round.png"}"#).unwrap();
        assert_eq!(
            cmd,
            Command::Snapshot {
                path: "/tmp/round.png".to_string(),
                defer: false,
            }
        );
    }

    /// A wire `snapshot` that says nothing about `defer` must keep the
    /// original contract — the file is written before the reply. Every
    /// existing harness depends on that, so the default is asserted on
    /// the parsed command rather than left to the struct definition.
    #[test]
    fn parse_snapshot_defaults_to_waiting_for_the_write() {
        let cmd = parse_command(r#"{"op":"snapshot","path":"/tmp/d.png"}"#).unwrap();
        assert_eq!(
            cmd,
            Command::Snapshot {
                path: "/tmp/d.png".to_string(),
                defer: false,
            }
        );
    }

    /// `defer` reaches the executor. A flag that parsed but was dropped
    /// on the way to `PlannedAction` would look exactly like a working
    /// deferral while still encoding on the event loop.
    #[test]
    fn deferred_snapshot_carries_the_flag_to_the_action() {
        let state = StaticState::new(&[]);
        let cmd = parse_command(r#"{"op":"snapshot","path":"/tmp/x.png","defer":true}"#).unwrap();
        let (resp, actions) = dispatch_command(&cmd, &state);
        assert!(matches!(resp, Response::Ok { .. }));
        assert_eq!(
            actions,
            vec![PlannedAction::Snapshot {
                path: PathBuf::from("/tmp/x.png"),
                defer: true,
            }]
        );
    }

    /// `drain_snapshots` parses and plans the drain the deferred path
    /// needs to be completable.
    #[test]
    fn drain_snapshots_plans_a_drain() {
        let state = StaticState::new(&[]);
        let cmd = parse_command(r#"{"op":"drain_snapshots"}"#).unwrap();
        assert_eq!(cmd, Command::DrainSnapshots);
        let (resp, actions) = dispatch_command(&cmd, &state);
        assert!(matches!(resp, Response::Ok { .. }));
        assert_eq!(actions, vec![PlannedAction::DrainSnapshots]);
    }

    // ===================================================================
    // Phase 5 — state_dump
    // ===================================================================

    /// `ControlState` impl that returns hand-built JSON for both the
    /// workspace tree and the chrome — used to drive the dispatcher
    /// without spinning up a real `FrameBuilder`. Keeps the dispatch
    /// tests pure.
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

    /// `Command::StateDump` round-trips through the wire — the parser
    /// must recognise the `state_dump` tag and emit the unit variant.
    #[test]
    fn parse_state_dump_command_round_trips() {
        let cmd = parse_command(r#"{"op":"state_dump"}"#).unwrap();
        assert_eq!(cmd, Command::StateDump);
    }

    /// Dispatching `Command::StateDump` against a state mock returns a
    /// [`Response::State`] carrying the mock's `workspace_tree` and
    /// `chrome` JSON verbatim, and produces no [`PlannedAction`]s. The
    /// dispatcher is read-only for this command — no side effects, no
    /// re-layout pressure.
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
        // The workspace tree round-trips exactly as the mock returned
        // it — the dispatcher is a pure pipe between `ControlState`
        // and the wire.
        assert!(
            workspace_tree.get("workspaces").is_some(),
            "workspace_tree should carry `workspaces` key; got {workspace_tree:?}"
        );
        let workspaces = workspace_tree["workspaces"].as_array().unwrap();
        assert_eq!(workspaces.len(), 1);
        // Chrome stays an opaque object — only assert the field is
        // present so growing it stays additive on the wire.
        assert!(
            chrome.get("overlays").is_some(),
            "chrome should carry `overlays` key; got {chrome:?}"
        );
    }

    /// Default `ControlState` impl returns `Value::Null` for both
    /// json methods — `state_dump` still dispatches successfully but
    /// carries nulls. This is the path the unit-test fixtures
    /// (`StaticState`) take so they don't have to invent a fake
    /// workspace tree just to compile.
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

    /// Wire format for `Response::State` — the serialiser tags the
    /// variant `state` (snake_case) and the payload is the two JSON
    /// blobs. Guards against accidental rename of `workspace_tree` /
    /// `chrome` / `peers` in the response envelope.
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

    /// A `ControlState` that exports exactly one action, so the
    /// dispatcher can be driven without a compositor.
    struct ActionState;

    impl ControlState for ActionState {
        fn widget_registry(&self) -> HashMap<String, Rect> {
            HashMap::new()
        }

        fn enumerate_actions(
            &self,
            query: &str,
            agent: Option<&str>,
        ) -> Result<serde_json::Value, String> {
            Ok(serde_json::json!({
                "query": query,
                // Echoed so a dispatch test can assert the announced
                // caller *arrived here*, rather than assert that the
                // command struct held it and hope it was passed on.
                "caller": agent,
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
            agent: Option<&str>,
        ) -> Result<serde_json::Value, ActionRefusal> {
            // Same reason as above: the resolve half has to see the
            // caller too, or enumeration and invocation would be gated
            // for two different principals.
            let caller = agent.map(str::to_string);
            // Two addresses: one nullary, one parameterised. The second
            // is what makes the argument paths below testable at the
            // dispatch layer, without a live registry.
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
                "caller": caller,
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

    /// Build an `actions_invoke` command with the given arguments.
    fn invoke_with(id: &str, args: serde_json::Value) -> Command {
        Command::ActionsInvoke {
            provider: "shell.actions".to_string(),
            id: id.to_string(),
            args: args.as_object().cloned().unwrap_or_default(),
            agent: None,
        }
    }

    /// The headline covenant of the enumeration half: asking "what can I
    /// do here" plans **no** action. If enumeration could ever produce a
    /// `PlannedAction`, a caller surveying the surface would be mutating
    /// the session just by looking at it.
    #[test]
    fn enumerate_answers_without_planning_any_side_effect() {
        let (resp, actions) = dispatch_command(
            &Command::ActionsEnumerate {
                query: String::new(),
                agent: None,
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

    /// The address a remark carries has to reach the room whole.
    ///
    /// An app mints a file's address as `files.entry:<path>` — kind, then
    /// the same kind again inside the id — and it is the *id* the agent
    /// resolves against what the room exports. A dispatch that split the
    /// address on its colon and kept the tail would produce a reference
    /// naming nothing, and the agent's answer would be an honest "I
    /// cannot act on that" about an object that is right there. So this
    /// asserts on what is planned, not on what was sent.
    #[test]
    fn a_remark_carries_the_whole_address_it_was_given() {
        let address = "files.entry:/var/tmp/qos/workspace/harbour.png";
        let (resp, actions) = dispatch_command(
            &Command::Remark {
                text: "Claude: make this square".to_string(),
                kind: "files.entry".to_string(),
                id: address.to_string(),
                label: None,
            },
            &ActionState,
        );
        assert!(
            matches!(resp, Response::Ok { .. }),
            "expected the remark to be accepted, got {resp:?}"
        );
        let [PlannedAction::Remark { remark }] = actions.as_slice() else {
            panic!("expected one planned remark, got {actions:?}");
        };
        assert_eq!(remark.reference.resource_id().as_str(), address);
        assert_eq!(remark.reference.kind, "files.entry");
        // The chip's label, derived rather than carried: a person reading
        // it should see the file, never the address.
        assert_eq!(remark.reference.label, "harbour.png");
        assert_eq!(remark.text, "Claude: make this square");
    }

    /// Nothing is planned for a remark with no text or no address.
    ///
    /// `RemarkCompose::submit` refuses both by returning, which is right
    /// and is invisible. Over a socket it has to be said: a caller that
    /// cannot see the screen would otherwise read "sent, and the room
    /// ignored it" off a run where nothing was ever sent.
    #[test]
    fn an_empty_remark_is_refused_out_loud() {
        for (text, kind, id) in [
            ("", "files.entry", "files.entry:/x.png"),
            ("   ", "files.entry", "files.entry:/x.png"),
            ("Claude: crop it", "files.entry", ""),
            ("Claude: crop it", "", "files.entry:/x.png"),
        ] {
            let (resp, actions) = dispatch_command(
                &Command::Remark {
                    text: text.to_string(),
                    kind: kind.to_string(),
                    id: id.to_string(),
                    label: None,
                },
                &ActionState,
            );
            assert!(actions.is_empty(), "nothing may be planned for {text:?}");
            let Response::Err { code, .. } = resp else {
                panic!("expected a refusal for {text:?}/{id:?}, got {resp:?}");
            };
            assert_eq!(code, "invalid_remark");
        }
    }

    /// The announced caller has to survive the whole way from the wire
    /// to the thing that builds the gate — on **both** ops, and on the
    /// planned invocation the executor re-resolves through.
    ///
    /// A caller carried on `actions_enumerate` and dropped on
    /// `actions_invoke` is worse than one carried on neither: the agent
    /// would survey its own surface and then invoke against the
    /// person's, which is exactly the gap the visibility gate exists to
    /// close. The fixture echoes what it was handed, so this asserts on
    /// arrival rather than on the parsed command holding the field.
    #[test]
    fn the_announced_caller_reaches_both_ops_and_the_planned_invocation() {
        let (resp, _) = dispatch_command(
            &Command::ActionsEnumerate {
                query: String::new(),
                agent: Some("Claude".to_string()),
            },
            &ActionState,
        );
        let Response::Answer(payload) = resp else {
            panic!("expected Response::Answer, got {resp:?}");
        };
        assert_eq!(
            payload["caller"], "Claude",
            "enumeration must be answered for the caller that announced itself"
        );

        let (resp, actions) = dispatch_command(
            &Command::ActionsInvoke {
                provider: "shell.actions".to_string(),
                id: "settings.open".to_string(),
                args: serde_json::Map::new(),
                agent: Some("Claude".to_string()),
            },
            &ActionState,
        );
        let Response::Answer(payload) = resp else {
            panic!("expected Response::Answer, got {resp:?}");
        };
        assert_eq!(
            payload["caller"], "Claude",
            "the resolve half must be gated for the same caller enumeration was"
        );
        assert_eq!(
            actions,
            vec![PlannedAction::InvokeAction {
                provider: "shell.actions".to_string(),
                id: "settings.open".to_string(),
                args: serde_json::Map::new(),
                agent: Some("Claude".to_string()),
            }],
            "the executor rebuilds the catalogue before anything runs; a plan \
             that forgot who asked would hand an agent the person's surface at \
             exactly the moment something happens"
        );
    }

    /// The field is optional on the wire, and its absence is the person
    /// — the harnesses that drive this socket as the person at the
    /// keyboard send no `agent` and must keep working unchanged.
    #[test]
    fn the_caller_is_optional_on_the_wire_and_parses_when_present() {
        let named = parse_command(
            r#"{"op":"actions_enumerate","query":"terminal","agent":"Claude"}"#,
        )
        .expect("actions_enumerate should parse with an agent");
        assert_eq!(
            named,
            Command::ActionsEnumerate {
                query: "terminal".to_string(),
                agent: Some("Claude".to_string()),
            }
        );

        let named_invoke = parse_command(
            r#"{"op":"actions_invoke","provider":"p","id":"i","agent":"Claude"}"#,
        )
        .expect("actions_invoke should parse with an agent");
        assert_eq!(
            named_invoke,
            Command::ActionsInvoke {
                provider: "p".to_string(),
                id: "i".to_string(),
                args: serde_json::Map::new(),
                agent: Some("Claude".to_string()),
            }
        );
    }

    /// A known address resolves, reports what it names, and plans
    /// exactly one invocation carrying that same address.
    #[test]
    fn invoke_resolves_the_address_then_plans_one_invocation() {
        let (resp, actions) = dispatch_command(
            &invoke_with("settings.open", serde_json::json!({})),
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
                agent: None,
            }]
        );
    }

    /// A nullary invocation still parses and still plans when the
    /// caller sends no `args` key at all — the shape every existing
    /// client uses.
    #[test]
    fn a_nullary_invocation_needs_no_args_key() {
        let cmd = parse_command(r#"{"op":"actions_invoke","provider":"shell.actions","id":"settings.open"}"#)
            .expect("an invocation without `args` must still parse");
        let (resp, actions) = dispatch_command(&cmd, &ActionState);
        assert!(matches!(resp, Response::Answer(_)), "got {resp:?}");
        assert_eq!(actions.len(), 1, "the nullary path still plans one invocation");
    }

    /// The happy path for a parameterised action: the arguments bind,
    /// and they travel to the executor verbatim.
    #[test]
    fn arguments_bind_and_reach_the_planned_invocation() {
        let (resp, actions) = dispatch_command(
            &invoke_with(
                "text.insert",
                serde_json::json!({"text": "hello", "at": 3}),
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

    /// A missing required argument is refused **before** anything is
    /// planned, and under its own code.
    ///
    /// The code is the point. `unknown_action` would send the caller
    /// looking for an address that is in fact perfectly good; the two
    /// mistakes are corrected in different places, so they must not
    /// share a name.
    #[test]
    fn a_missing_required_argument_is_refused_under_its_own_code() {
        let (resp, actions) =
            dispatch_command(&invoke_with("text.insert", serde_json::json!({})), &ActionState);
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

    /// A wrong-typed argument is refused rather than coerced. An agent
    /// that sent a number where text was declared asked for something
    /// this action cannot mean, and guessing at the intent is how a
    /// caller ends up believing an edit happened that did not.
    #[test]
    fn a_wrong_typed_argument_is_refused_not_coerced() {
        let (resp, actions) = dispatch_command(
            &invoke_with("text.insert", serde_json::json!({"text": 7})),
            &ActionState,
        );
        assert!(actions.is_empty(), "got {actions:?}");
        let Response::Err { code, message } = resp else {
            panic!("expected Response::Err, got {resp:?}");
        };
        assert_eq!(code, "invalid_arguments");
        assert!(message.contains("text") && message.contains("integer"), "{message}");
    }

    /// An argument the action never declared is refused, not dropped.
    ///
    /// Silently ignoring it is the worst available behaviour: the caller
    /// believes it asked for something, the reply says `ok`, and the
    /// difference is invisible to anything that cannot see the screen.
    #[test]
    fn an_unknown_argument_is_refused_not_silently_ignored() {
        let (resp, actions) = dispatch_command(
            &invoke_with(
                "text.insert",
                serde_json::json!({"text": "hi", "colour": "red"}),
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

    /// A JSON shape no parameter type can hold is refused by name
    /// rather than coerced into one that nearly fits.
    #[test]
    fn a_json_shape_with_no_parameter_type_is_refused() {
        for bad in [
            serde_json::json!({"text": null}),
            serde_json::json!({"text": ["a"]}),
            serde_json::json!({"text": "ok", "at": 1.5}),
        ] {
            let (resp, actions) = dispatch_command(&invoke_with("text.insert", bad), &ActionState);
            assert!(actions.is_empty(), "got {actions:?}");
            let Response::Err { code, .. } = resp else {
                panic!("expected Response::Err, got {resp:?}");
            };
            assert_eq!(code, "invalid_arguments");
        }
    }

    /// Arguments are checked against the action the *address* resolves
    /// to. Passing a parameterised action's arguments to a nullary one
    /// is an unknown-argument refusal, not a silent success.
    #[test]
    fn arguments_meant_for_another_action_are_refused() {
        let (resp, actions) = dispatch_command(
            &invoke_with("settings.open", serde_json::json!({"text": "hi"})),
            &ActionState,
        );
        assert!(actions.is_empty(), "got {actions:?}");
        let Response::Err { code, .. } = resp else {
            panic!("expected Response::Err, got {resp:?}");
        };
        assert_eq!(code, "invalid_arguments");
    }

    /// You cannot invoke what you were never shown. An address the
    /// catalogue does not expose is a structured refusal that plans
    /// **nothing** — not an `Ok` whose invocation quietly no-ops.
    ///
    /// The distinction matters most to exactly the caller this API is
    /// for: something driving the shell without being able to see it
    /// cannot tell "it ran and changed nothing" from "it never ran"
    /// unless the wire says so.
    #[test]
    fn unknown_address_is_refused_and_plans_nothing() {
        let (resp, actions) = dispatch_command(
            &invoke_with("definitely.not.a.real.action", serde_json::json!({})),
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

    /// A control state with no live registry **refuses** rather than
    /// reporting an empty surface. "This shell exports nothing" and
    /// "this fixture cannot answer" are different facts, and a harness
    /// that read one as the other would conclude the action model was
    /// missing when it was merely out of reach.
    #[test]
    fn stateless_control_refuses_enumeration_rather_than_reporting_empty() {
        let state = StaticState::new(&[]);
        let (resp, actions) = dispatch_command(
            &Command::ActionsEnumerate {
                query: String::new(),
                agent: None,
            },
            &state,
        );

        assert!(actions.is_empty());
        let Response::Err { code, .. } = resp else {
            panic!("a stateless control must refuse, not answer; got {resp:?}");
        };
        assert_eq!(code, "enumerate_failed");
    }

    /// Wire round-trip for both new ops — guards the `op` tags and field
    /// names the CLI, the demo script and the MCP shim all encode by hand.
    #[test]
    fn action_ops_round_trip_over_the_wire() {
        let enumerate = parse_command(r#"{"op":"actions_enumerate","query":"settings"}"#)
            .expect("actions_enumerate should parse");
        assert_eq!(
            enumerate,
            Command::ActionsEnumerate {
                query: "settings".to_string(),
                agent: None,
            }
        );

        let bare = parse_command(r#"{"op":"actions_enumerate"}"#)
            .expect("actions_enumerate should parse without a query");
        assert_eq!(
            bare,
            Command::ActionsEnumerate {
                query: String::new(),
                agent: None,
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
                agent: None,
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

    /// The production path: a click is preceded by pointer motion that
    /// ends on the target.
    ///
    /// This is the invariant the whole thing exists for. A click that
    /// arrives with no motion in front of it is a control activating
    /// with nothing visible having reached it, which on any recording
    /// of the session reads as the machine operating itself.
    #[test]
    fn click_at_travels_from_where_the_pointer_already_is() {
        let state = StaticState::with_pointer(&[], (40.0, 40.0));
        let (_resp, actions) = dispatch_command(&Command::ClickAt { x: 840.0, y: 500.0 }, &state);

        let path = motions(&actions);
        assert!(
            path.len() > 1,
            "a click across the screen must travel, not teleport: {path:?}"
        );
        assert_eq!(
            path.last().copied(),
            Some((840.0, 500.0)),
            "the last motion before the press has to be the target itself"
        );
        // Nothing but motion may come before the press, and the press
        // must be followed by exactly one release.
        let press = actions
            .iter()
            .position(|a| matches!(a, PlannedAction::MouseLeftButton { pressed: true }))
            .expect("a click presses");
        assert_eq!(press, actions.len() - 2, "the press is second from last");
        assert!(
            actions[..press]
                .iter()
                .all(|a| matches!(a, PlannedAction::CursorMoved { .. })),
            "only motion precedes the press: {actions:?}"
        );
        assert_eq!(
            actions.last(),
            Some(&PlannedAction::MouseLeftButton { pressed: false })
        );
    }

    /// The path starts where the pointer was and stays inside the
    /// segment between the two points — it is a trip, not a detour.
    #[test]
    fn click_travel_starts_near_the_old_position_and_advances_monotonically() {
        let from = (100.0, 700.0);
        let to = (1500.0, 120.0);
        let state = StaticState::with_pointer(&[], from);
        let (_resp, actions) = dispatch_command(&Command::ClickAt { x: to.0, y: to.1 }, &state);
        let path = motions(&actions);

        let span = (to.0 - from.0).hypot(to.1 - from.1);
        let first_hop = (path[0].0 - from.0).hypot(path[0].1 - from.1);
        assert!(
            first_hop < span / 2.0,
            "the first motion continues from where the pointer was, it does \
             not jump most of the way: hop {first_hop} of {span}"
        );
        let mut travelled = 0.0_f64;
        let mut prev = from;
        for p in &path {
            let d = (p.0 - prev.0).hypot(p.1 - prev.1);
            travelled += d;
            prev = *p;
        }
        // Straight line, so the path length is the distance. A tolerance
        // rather than equality because the points are sampled.
        assert!(
            (travelled - span).abs() < 1.0,
            "the pointer goes straight there: walked {travelled}, distance {span}"
        );
    }

    /// A pointer already on the target does not shuffle around before
    /// pressing. It still emits the landing motion, because that is what
    /// the press is aimed with.
    #[test]
    fn click_at_the_current_position_is_one_motion_and_a_press() {
        let state = StaticState::with_pointer(&[], (300.0, 300.0));
        let (_resp, actions) = dispatch_command(&Command::ClickAt { x: 300.0, y: 300.0 }, &state);
        assert_eq!(
            actions,
            vec![
                PlannedAction::CursorMoved { x: 300.0, y: 300.0 },
                PlannedAction::MouseLeftButton { pressed: true },
                PlannedAction::MouseLeftButton { pressed: false },
            ]
        );
    }

    /// `click_named` is the call most beats reach for, so it gets the
    /// approach for free — it must not be a second, invisible door.
    #[test]
    fn click_named_travels_to_the_widget_centre() {
        let state = StaticState::with_pointer(
            &[("collab_invite", Rect::new(1652.0, 154.0, 1770.0, 182.0))],
            (60.0, 900.0),
        );
        let (resp, actions) = dispatch_command(
            &Command::ClickNamed {
                name: "collab_invite".to_string(),
            },
            &state,
        );
        assert!(matches!(resp, Response::Ok { .. }));
        let path = motions(&actions);
        assert!(path.len() > 1, "a named click travels too: {path:?}");
        assert_eq!(path.last().copied(), Some((1711.0, 168.0)));
        assert_eq!(
            actions[actions.len() - 2],
            PlannedAction::MouseLeftButton { pressed: true },
            "the press comes after the pointer has arrived"
        );
    }

    /// A refused `click_named` plans nothing at all — no stray motion
    /// that would move the pointer somewhere the caller never asked for.
    #[test]
    fn click_named_that_cannot_resolve_plans_no_motion() {
        let state = StaticState::with_pointer(&[], (10.0, 10.0));
        let (resp, actions) = dispatch_command(
            &Command::ClickNamed {
                name: "nope".to_string(),
            },
            &state,
        );
        assert!(matches!(resp, Response::Err { .. }));
        assert!(actions.is_empty());
    }

    /// However far apart the two points are, one command plans a bounded
    /// number of motions.
    #[test]
    fn click_travel_is_bounded_however_far_it_has_to_go() {
        let state = StaticState::with_pointer(&[], (0.0, 0.0));
        let (_resp, actions) =
            dispatch_command(&Command::ClickAt { x: 99_000.0, y: 99_000.0 }, &state);
        let path = motions(&actions);
        assert!(
            path.len() <= TRAVEL_MAX_STEPS as usize,
            "{} motions is not bounded",
            path.len()
        );
        assert_eq!(path.last().copied(), Some((99_000.0, 99_000.0)));
    }

    /// A drag reaches its start the same way, so the gesture is watchable
    /// from the moment the pointer sets off rather than from the press.
    #[test]
    fn drag_travels_to_its_start_before_pressing() {
        let state = StaticState::with_pointer(&[], (20.0, 20.0));
        let (_resp, actions) = dispatch_command(
            &Command::Drag {
                x_from: 600.0,
                y_from: 400.0,
                x_to: 900.0,
                y_to: 400.0,
                steps: Some(4),
            },
            &state,
        );
        let press = actions
            .iter()
            .position(|a| matches!(a, PlannedAction::MouseLeftButton { pressed: true }))
            .expect("a drag presses");
        let approach = motions(&actions[..press]);
        assert!(
            approach.len() > 1,
            "the pointer travels to the drag's start: {approach:?}"
        );
        assert_eq!(approach.last().copied(), Some((600.0, 400.0)));
        assert_eq!(
            actions.last(),
            Some(&PlannedAction::MouseLeftButton { pressed: false })
        );
    }

}
