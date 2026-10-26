//! Real-input e2e test control protocol (`TILO_TEST_CONTROL=/run/user/N/tilo-test-N.sock`): pure command/response types + dispatcher, kept separate from the socket transport.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use vello::kurbo::Rect;

use crate::shell::pointer_readiness::{NotReady, PressFacts, consume_press};

pub use tilo_control::protocol::{
    ActionRefusal, Agent, DEFAULT_LISTING_LIMIT, ListingDetail, ListingRequest, ListingWindow,
    MAX_AGENT_ID_CHARS, MAX_AGENT_LABEL_CHARS, MAX_LISTING_LIMIT, MAX_WINDOW_CHARS,
    PointerButtonKind, TouchPhaseKind, listing_fingerprint,
};

pub type Command = tilo_control::protocol::Command<
    crate::shell::render::paint::InjectedGpuFault,
    crate::compositor::backend::WakeStage,
>;
pub type Request = tilo_control::protocol::Request<
    crate::shell::render::paint::InjectedGpuFault,
    crate::compositor::backend::WakeStage,
>;

/// Top-level response envelope: an `ok` payload (per-command shape) or a structured `err`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[allow(
    clippy::large_enum_variant,
    reason = "one response is built and serialized per control command, so its size never matters; \
              whether the lint fires depends on serde_json's feature set in the build"
)]
pub enum Response {
    /// `list_targets` response, sorted by key for deterministic output (`HashMap` order would otherwise leak into test assertions).
    Targets(Vec<TargetEntry>),
    /// Successful command with no payload. Carries the post-action frame counter (currently always `0`) so clients can poll against a known frame number.
    Ok { frame: u64 },
    /// Structured error: `code` is stable and machine-readable, `message` is human prose for logs.
    Err { code: String, message: String },
    /// `foreign_frames` response — one opaque entry per tracked foreign surface.
    ForeignFrames { frames: serde_json::Value },
    /// `state_dump` response — persisted `WorkspaceTree` + [`ChromeState`](crate::shell::chrome_state::ChromeState), both opaque.
    State {
        /// Serialised once, straight from the tree, and written into the reply as it is.
        workspace_tree: RawJson,
        chrome: serde_json::Value,
        /// The peers this session would paint, straight off the `PresenceCtx` the overlay reads.
        peers: serde_json::Value,
        /// Compositor-authoritative screen exposure: who can see/control
        /// which surface. This is deliberately separate from presence.
        sharing: serde_json::Value,
    },
    /// Answer to a capability query or admin command (`grant`, `revoke`, `check_action`). Opaque JSON so the shape can grow.
    Answer(serde_json::Value),
    /// `clipboard_paste` response. `None` means nothing has been copied — distinct from `Some("")`, an owned empty selection.
    Clipboard { text: Option<String> },
}

/// JSON text serialised once, straight from a typed value, and written into
/// the reply as it is. Going through a `serde_json::Value` tree instead costs
/// several times the serialisation itself for a whole desktop tree.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RawJson(Box<serde_json::value::RawValue>);

impl RawJson {
    /// JSON `null`.
    #[must_use]
    pub fn null() -> Self {
        Self(serde_json::value::RawValue::NULL.to_owned())
    }

    /// `value` as JSON text, or `null` if it will not serialise. The derived
    /// `Serialize` impls are infallible in practice, and this crate builds
    /// with `panic = "abort"`, so a hypothetical bad value must not take the
    /// compositor down.
    pub fn of<T: Serialize + ?Sized>(value: &T) -> Self {
        serde_json::value::to_raw_value(value).map_or_else(
            |error| {
                tracing::error!(
                    target: "shell::compositor::test_control",
                    %error,
                    "failed to serialise a state dump field; answering null"
                );
                Self::null()
            },
            Self,
        )
    }

    /// The JSON text.
    #[must_use]
    pub fn get(&self) -> &str {
        self.0.get()
    }

    /// Parsed into a [`serde_json::Value`], for a reader that walks it.
    #[must_use]
    pub fn to_value(&self) -> serde_json::Value {
        serde_json::from_str(self.get()).unwrap_or(serde_json::Value::Null)
    }
}

impl PartialEq for RawJson {
    fn eq(&self, other: &Self) -> bool {
        self.get() == other.get()
    }
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
    /// JSON view of the persisted `WorkspaceTree` — same shape `TILO_TEST_STATE_DUMP` writes on every frame tick, less the workspaces' history unless `include_history` (see [`Command::StateDump`]). Defaults to `null` so mock fixtures that only need `widget_registry` don't have to spell out a tree.
    fn workspace_tree_json(&self, _include_history: bool) -> RawJson {
        RawJson::null()
    }
    /// The open windows, one row per tab ([`Command::Windows`]), from the live tree. Default impl refuses: "no tree" must not read as "no windows".
    ///
    /// # Errors
    ///
    /// Returns a human-readable refusal when this control state has no live tree.
    fn windows_json(&self) -> Result<serde_json::Value, String> {
        Err("no live workspace tree on this control state".to_string())
    }
    /// JSON view of [`ChromeState`](crate::shell::chrome_state::ChromeState) (`overlays`, `search_scope`, `ws_settings_tab`, `revision`, `editing_workspace_name`). Defaults to `Null`.
    fn chrome_state_json(&self) -> serde_json::Value {
        serde_json::Value::Null
    }
    /// The peers this session would paint right now, off the same `PresenceCtx` the overlay draws from — exposed because "no peer arrived" and "a peer arrived and wasn't drawn" otherwise look identical. Defaults to `Null`.
    fn peers_json(&self) -> serde_json::Value {
        serde_json::Value::Null
    }
    /// Active stream/input grants from the compositor authority. Defaults to
    /// `Null` for control-state fixtures without a live compositor.
    fn sharing_json(&self) -> serde_json::Value {
        serde_json::Value::Null
    }
    /// Per-surface ground truth of whether each foreign toplevel has ever presented. The placeholder tile is near-identical between apps and, for a legitimately-empty one, near-identical to a real first frame — `frame_seq > 0` is the fact a harness needs instead of guessing from pixels. Defaults to an empty list.
    fn foreign_frames_json(&self) -> serde_json::Value {
        serde_json::Value::Array(Vec::new())
    }

    /// The presentation lens a press is checked through: the widget tree the
    /// registry was walked from, the frame on screen, and whether the desktop
    /// is between two arrangements. Defaults to absent, so a fixture with
    /// canned rects answers presses from those rects alone.
    fn press_witness(&self) -> Option<(&dyn ui::widget::Widget, PressFacts)> {
        None
    }

    /// Grant a sandbox capability on the active workspace. Default impl refuses: a `ControlState` with no live tree has nothing to administer, and answering anything else would let a harness believe it proved something about authority.
    ///
    /// # Errors
    ///
    /// Returns a human-readable refusal reason whenever the grant is not recorded — no live workspace, an unauthorised grantor, or an invalid subject/scope/access.
    fn grant_capability(
        &self,
        _subject: &str,
        _scope: &str,
        _access: &str,
        _id: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        Err("no live workspace on this control state".to_string())
    }

    /// Give a grant on the active workspace through its governance. Default impl refuses, as `grant_capability` does.
    ///
    /// # Errors
    ///
    /// Returns a human-readable refusal reason whenever the grant is not recorded.
    fn allow(
        &self,
        _subject: &str,
        _node: &str,
        _kinds: &[String],
        _answer: &str,
        _id: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        Err("no live workspace on this control state".to_string())
    }

    /// Take back a grant on the active workspace through its governance.
    ///
    /// # Errors
    ///
    /// Returns a human-readable refusal reason whenever nothing is revoked.
    fn disallow(&self, _id: &str) -> Result<serde_json::Value, String> {
        Err("no live workspace on this control state".to_string())
    }

    /// Admit a person to the active workspace with a role, through its governance.
    ///
    /// # Errors
    ///
    /// Returns a human-readable refusal reason whenever nobody is admitted.
    fn admit(&self, _person: &str, _role: &str) -> Result<serde_json::Value, String> {
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

    /// Answer [`Command::CheckAction`] against the active workspace.
    ///
    /// # Errors
    ///
    /// Returns a human-readable refusal reason whenever the check itself cannot be answered — no live workspace on this control state.
    fn check_action(
        &self,
        _actor: &str,
        _app: &str,
        _resource: &str,
        _action: &str,
        _kind: &str,
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

    /// One `page` of the listing for `query`, with the bounded machine
    /// catalogue only when explicitly supported. The default impl pages
    /// [`Self::enumerate_actions`]' whole listing.
    ///
    /// # Errors
    /// `enumerate_failed` if this state does not support programmatic
    /// discovery, or if the underlying action catalogue is unavailable; a
    /// cursor refusal from [`ListingRequest::window`].
    fn enumerate_actions_with_programmatic(
        &self,
        query: &str,
        include_programmatic: bool,
        page: &ListingRequest,
    ) -> Result<serde_json::Value, ActionRefusal> {
        if include_programmatic {
            return Err(ActionRefusal::listing_failed(
                "this control state cannot enumerate programmatic actions",
            ));
        }
        let mut answer = self
            .enumerate_actions(query)
            .map_err(ActionRefusal::listing_failed)?;
        page.apply(&mut answer)?;
        Ok(answer)
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

/// One edge of a synthetic touchpad gesture, as libinput reports it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TouchpadEdge {
    SwipeBegin { fingers: u32 },
    SwipeUpdate { dx: f64, dy: f64 },
    SwipeEnd { cancelled: bool },
    PinchBegin { fingers: u32 },
    PinchUpdate { scale: f64 },
    PinchEnd,
}

/// Side-effect actions the transport must execute after a `dispatch_command` call. Pure values, no I/O — the transport synthesises `winit::WindowEvent`s from these through `host_and_taffy_mut`, the same wire as `WinitBackend`/`ShellDriver`.
#[derive(Debug, Clone, PartialEq)]
pub enum PlannedAction {
    /// One touchpad gesture edge for the shell's gesture tracker.
    Touchpad(TouchpadEdge),
    /// Move the cursor to `(x, y)` in compositor coordinates.
    CursorMoved { x: f64, y: f64 },
    /// Inject a left mouse button event: `pressed=true` is down, `false` is up.
    MouseLeftButton { pressed: bool },
    /// Press or release a pointer button other than left. Kept separate from [`Self::MouseLeftButton`] since call sites here and in `native.rs` only ever plan `Left`, and widening both for an unused case is churn.
    MousePointerButton {
        pressed: bool,
        button: PointerButtonKind,
    },
    /// One raw direct-touch edge, resolved at execution time by the same
    /// ownership arena used by the DRM libinput path.
    Touch {
        device: String,
        slot: u32,
        phase: TouchPhaseKind,
        x: f64,
        y: f64,
    },
    /// One event of a virtual pen at `(x, y)` in compositor coordinates,
    /// with the tip pressed at `pressure` (`0.0..=1.0`). Routed like a real
    /// tablet's: a tablet-aware client gets the tool, anything else a
    /// pointer.
    Pen {
        phase: crate::compositor::backend::TabletPhase,
        x: f64,
        y: f64,
        pressure: f64,
    },
    /// Inject a keyboard press or release for `key`. The transport maps `KeyTag` to `winit::keyboard::Key` and synthesises a `WindowEvent::KeyboardInput`.
    KeyboardKey { key: KeyTag, pressed: bool },
    /// Queue a keyboard press or release for the compositor loop's input funnel, where a
    /// physical keyboard's events arrive ([`Command::SeatKey`]).
    SeatKey { key: KeyTag, pressed: bool },
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
    SetGroupWidth { permille: u32 },
    /// Publish paths through the hosted file manager's production external-drag slot.
    SeedFilesDrag { paths: Vec<PathBuf> },
    /// Run one already-validated action against the live CRDT. Carried as a planned action rather than executed inside `dispatch_command`, since invoking mutates the document and dispatch only ever holds it immutably. The executor re-resolves the address and re-checks `args` against a fresh catalogue, since the document may have moved on since planning.
    InvokeAction {
        provider: String,
        id: String,
        args: serde_json::Map<String, serde_json::Value>,
        /// Enrolled device this invocation is sent to. `None` runs it here.
        device: Option<String>,
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
    F1,
    F4,
    PrintScreen,
    AudioVolumeUp,
    AudioVolumeDown,
    AudioVolumeMute,
    MicrophoneVolumeMute,
    BrightnessUp,
    BrightnessDown,
    MediaPlayPause,
    MediaTrackNext,
    MediaTrackPrevious,
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
        "F1" => Some(KeyTag::Named(NamedKeyTag::F1)),
        "F4" => Some(KeyTag::Named(NamedKeyTag::F4)),
        "PrintScreen" => Some(KeyTag::Named(NamedKeyTag::PrintScreen)),
        "AudioVolumeUp" => Some(KeyTag::Named(NamedKeyTag::AudioVolumeUp)),
        "AudioVolumeDown" => Some(KeyTag::Named(NamedKeyTag::AudioVolumeDown)),
        "AudioVolumeMute" => Some(KeyTag::Named(NamedKeyTag::AudioVolumeMute)),
        "MicrophoneVolumeMute" => Some(KeyTag::Named(NamedKeyTag::MicrophoneVolumeMute)),
        "BrightnessUp" => Some(KeyTag::Named(NamedKeyTag::BrightnessUp)),
        "BrightnessDown" => Some(KeyTag::Named(NamedKeyTag::BrightnessDown)),
        "MediaPlayPause" => Some(KeyTag::Named(NamedKeyTag::MediaPlayPause)),
        "MediaTrackNext" => Some(KeyTag::Named(NamedKeyTag::MediaTrackNext)),
        "MediaTrackPrevious" => Some(KeyTag::Named(NamedKeyTag::MediaTrackPrevious)),
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

/// What a build without development overrides answers to
/// `deadlock_main_loop`: it never hangs its own main loop on request.
#[must_use]
pub fn deadlock_main_loop_refused() -> Response {
    Response::Err {
        code: crate::compositor::dev_test_identity::DEV_IMAGE_ONLY.into(),
        message: "a main-loop deadlock is injected only by development builds".into(),
    }
}

/// `gpu_fault`: a development build has the presenting renderer suffer `fault`; any other
/// build refuses.
#[must_use]
pub fn inject_gpu_fault(fault: crate::shell::render::paint::InjectedGpuFault) -> Response {
    if !crate::compositor::dev_env::DEV_ENV_OVERRIDES {
        return Response::Err {
            code: crate::compositor::dev_test_identity::DEV_IMAGE_ONLY.into(),
            message: "a GPU fault is injected only by development builds".into(),
        };
    }
    crate::shell::render::paint::inject_gpu_fault(fault);
    Response::Ok { frame: 0 }
}

/// Dispatch a single command against the live compositor state.
///
/// Returns `(response, actions)`: read-only commands return no actions; mutating commands return the [`PlannedAction`]s the transport must run before replying — keeps the dispatcher pure (no I/O, no `&mut` state).
#[allow(
    clippy::too_many_lines,
    reason = "the command-to-action mapping is intentionally exhaustive and stays together so the test transport remains auditable"
)]
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
            pressure: None,
        } => drag(*x_from, *y_from, *x_to, *y_to, *steps),
        Command::Drag {
            x_from,
            y_from,
            x_to,
            y_to,
            steps,
            pressure: Some(pressure),
        } => pen_drag((*x_from, *y_from), (*x_to, *y_to), *steps, pressure),
        Command::Key { key } => key_press_release(key),
        Command::SeatKey { key } => seat_key_press_release(key),
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
        Command::ProcessMemory => (
            Response::Answer(crate::shell::memory_report::report()),
            Vec::new(),
        ),
        Command::StateDump { include_history } => state_dump(state, *include_history),
        Command::Windows => (
            answer_or_err(
                state
                    .windows_json()
                    .map(|windows| serde_json::json!({ "windows": windows })),
                "windows_failed",
            ),
            Vec::new(),
        ),
        Command::Grant {
            subject,
            scope,
            access,
            id,
        } => grant(state, subject, scope, access, id.as_deref()),
        Command::Revoke { id } => (
            answer_or_err(state.revoke_capability(id), "revoke_refused"),
            Vec::new(),
        ),
        Command::Allow {
            subject,
            node,
            kinds,
            answer,
            id,
        } => (
            answer_or_err(
                state.allow(subject, node, kinds, answer, id.as_deref()),
                "grant_refused",
            ),
            Vec::new(),
        ),
        Command::Disallow { id } => (
            answer_or_err(state.disallow(id), "revoke_refused"),
            Vec::new(),
        ),
        Command::Admit { person, role } => (
            answer_or_err(state.admit(person, role), "admit_refused"),
            Vec::new(),
        ),
        Command::CheckAction {
            actor,
            app,
            resource,
            action,
            kind,
            destructive,
        } => check_action(state, actor, app, resource, action, kind, *destructive),
        Command::PlaceNextWindow { app_id } => {
            crate::shell::launcher::set_pending_placement(
                app_id,
                crate::shell::tab_binding::Placement::NewGroup,
            );
            (Response::Ok { frame: 0 }, Vec::new())
        }
        Command::MintHostLaunchActivation { .. } => (
            Response::Err {
                code: "test_transport_required".to_string(),
                message: "host activation tokens are minted by the native test transport"
                    .to_string(),
            },
            Vec::new(),
        ),
        Command::DeadlockMainLoop => (
            Response::Err {
                code: "test_transport_required".to_string(),
                message: "a main-loop deadlock is injected by the native test transport"
                    .to_string(),
            },
            Vec::new(),
        ),
        Command::GpuFault { .. } => (
            Response::Err {
                code: "test_transport_required".to_string(),
                message: "a GPU fault is injected by the native test transport".to_string(),
            },
            Vec::new(),
        ),
        Command::DisplayBlank | Command::DisplayWakeFault { .. } => (
            Response::Err {
                code: "test_transport_required".to_string(),
                message: "the display is driven by the native test transport".to_string(),
            },
            Vec::new(),
        ),
        Command::ProvisionTestIdentity => (
            Response::Err {
                code: "test_transport_required".to_string(),
                message: "test identities are provisioned by the native test transport".to_string(),
            },
            Vec::new(),
        ),
        Command::Watch { .. } => (
            Response::Err {
                code: "test_transport_required".to_string(),
                message: "watching is answered by the native compositor transport".to_string(),
            },
            Vec::new(),
        ),
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
        Command::SeedFilesDrag { paths } => (
            Response::Ok { frame: 0 },
            vec![PlannedAction::SeedFilesDrag {
                paths: paths.clone(),
            }],
        ),
        Command::PointerButton { pressed, button } => pointer_button(*pressed, *button),
        Command::Touch {
            device,
            slot,
            phase,
            x,
            y,
        } => (
            Response::Ok { frame: 0 },
            vec![PlannedAction::Touch {
                device: device.clone(),
                slot: *slot,
                phase: *phase,
                x: *x,
                y: *y,
            }],
        ),
        Command::Swipe {
            fingers,
            dx,
            dy,
            steps,
            release,
        } => swipe(*fingers, (*dx, *dy), *steps, *release),
        Command::SwipeEnd { cancelled } => (
            Response::Ok { frame: 0 },
            vec![PlannedAction::Touchpad(TouchpadEdge::SwipeEnd {
                cancelled: *cancelled,
            })],
        ),
        Command::Pinch {
            fingers,
            scale,
            steps,
        } => pinch(*fingers, *scale, *steps),
        Command::ClipboardCopy { text, primary } => clipboard_copy(text, *primary),
        Command::ClipboardCopyImage => clipboard_copy_image(),
        Command::ClipboardPaste { primary } => clipboard_paste(*primary),
        // Whether a listing waits is the transport's: it holds the reply.
        Command::ActionsEnumerate {
            query,
            include_programmatic,
            page,
            ..
        } => (
            match state.enumerate_actions_with_programmatic(query, *include_programmatic, page) {
                Ok(answer) => Response::Answer(answer),
                Err(refusal) => Response::Err {
                    code: refusal.code.to_string(),
                    message: refusal.message,
                },
            },
            Vec::new(),
        ),
        // How long the answer waits is the transport's: it holds the reply.
        Command::ActionsInvoke {
            provider,
            id,
            args,
            device,
            wait_ms: _,
        } => match device {
            Some(device) => plan_enrolled_device_action(device, provider, id, args),
            None => actions_invoke(state, provider, id, args),
        },
        Command::ActionsStatus { invocation_id, .. } => invocation_control(
            crate::shell::search::providers::live_app_actions::job_status_id(*invocation_id),
        ),
        Command::ActionsCancel { invocation_id } => invocation_control(
            crate::shell::search::providers::live_app_actions::job_cancel_id(*invocation_id),
        ),
        Command::AgentSeat | Command::AgentLeave => (
            Response::Err {
                code: AGENT_REQUIRED.to_string(),
                message: "agent_seat and agent_leave name an agent".to_string(),
            },
            Vec::new(),
        ),
    }
}

/// Error code for an agent-only command sent without an `agent`.
pub const AGENT_REQUIRED: &str = "agent_required";

fn invocation_control(id: String) -> (Response, Vec<PlannedAction>) {
    (
        Response::Answer(serde_json::json!({"invocation_id": id})),
        vec![PlannedAction::InvokeAction {
            provider: crate::shell::search::providers::live_app_actions::PROVIDER_ID.into(),
            id,
            args: serde_json::Map::new(),
            device: None,
        }],
    )
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

/// [`Command::ClickNamed`]: resolve `name` against the widget registry, then
/// click its centre — once the presentation the name refers to has drawn.
///
/// Readiness is proven for this target and the widgets it sits in, never for
/// the desktop at large: a widget asking for frames for reasons of its own
/// must not hold every other target back (see
/// [`crate::shell::pointer_readiness`]).
fn click_named<S: ControlState>(state: &S, name: &str) -> (Response, Vec<PlannedAction>) {
    let registry = state.widget_registry();
    let Some(rect) = registry.get(name) else {
        return refuse(NotReady::NoSuchTarget, name);
    };
    if rect.area() <= 0.0 {
        return refuse(NotReady::NotLaidOut, name);
    }
    let centre = match state.press_witness() {
        Some((root, facts)) => {
            match consume_press(root, &registry, &facts, name, facts.aim(name)) {
                Ok(point) => point,
                Err(not_ready) => return refuse(not_ready, name),
            }
        }
        None => rect.center(),
    };
    (
        Response::Ok { frame: 0 },
        vec![
            PlannedAction::CursorMoved {
                x: centre.x,
                y: centre.y,
            },
            PlannedAction::MouseLeftButton { pressed: true },
            PlannedAction::MouseLeftButton { pressed: false },
        ],
    )
}

/// A refused press: the stable code and what the caller should do instead.
fn refuse(not_ready: NotReady, name: &str) -> (Response, Vec<PlannedAction>) {
    (
        Response::Err {
            code: not_ready.code().to_string(),
            message: not_ready.explain(name),
        },
        Vec::new(),
    )
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

/// Most pressures a pen drag may name, one per point at most.
const MAX_PEN_PRESSURES: usize = 1024;

/// [`Command::Drag`] with a pen: into range and down at `from`, `steps`
/// motions to `to` with the pressure interpolated along `pressure`, then up
/// and out of range.
fn pen_drag(
    from: (f64, f64),
    to: (f64, f64),
    steps: Option<u32>,
    pressure: &[f64],
) -> (Response, Vec<PlannedAction>) {
    use crate::compositor::backend::TabletPhase;
    if pressure.is_empty()
        || pressure.len() > MAX_PEN_PRESSURES
        || pressure.iter().any(|p| !(0.0..=1.0).contains(p))
    {
        return (
            Response::Err {
                code: "parse_error".into(),
                message: format!("pressure is 1 to {MAX_PEN_PRESSURES} values, each from 0 to 1"),
            },
            Vec::new(),
        );
    }
    let steps = steps.unwrap_or(8).clamp(1, MAX_GESTURE_STEPS);
    let at = |t| pressure_at(pressure, t);
    let pen = |phase, (x, y): (f64, f64), pressure| PlannedAction::Pen {
        phase,
        x,
        y,
        pressure,
    };
    let mut actions = Vec::with_capacity((steps as usize).saturating_add(4));
    actions.push(pen(TabletPhase::ProximityIn, from, 0.0));
    actions.push(pen(TabletPhase::TipDown, from, at(0.0)));
    for s in 0..steps {
        let t = if steps == 1 {
            0.0
        } else {
            f64::from(s) / f64::from(steps.saturating_sub(1))
        };
        let point = (from.0 + (to.0 - from.0) * t, from.1 + (to.1 - from.1) * t);
        actions.push(pen(TabletPhase::Motion, point, at(t)));
    }
    actions.push(pen(TabletPhase::TipUp, to, 0.0));
    actions.push(pen(TabletPhase::ProximityOut, to, 0.0));
    (Response::Ok { frame: 0 }, actions)
}

/// The pressure `t` (`0.0..=1.0`) of the way along a stroke whose
/// `pressure` values (at least one) are spread evenly over it.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "indices are at most MAX_PEN_PRESSURES, exact as f64, and t is within 0..=1"
)]
fn pressure_at(pressure: &[f64], t: f64) -> f64 {
    let last = pressure.len().saturating_sub(1);
    let position = t.clamp(0.0, 1.0) * last as f64;
    let below = (position.floor() as usize).min(last);
    let value = |index: usize| pressure.get(index.min(last)).copied().unwrap_or(0.0);
    let (start, end) = (value(below), value(below.saturating_add(1)));
    start + (end - start) * (position - below as f64)
}

/// Most updates a synthetic gesture is split into: a 144 Hz touchpad
/// reports about this many over a two-second swipe.
const MAX_GESTURE_STEPS: u32 = 288;

fn client_gesture(fingers: u32) -> (Response, Vec<PlannedAction>) {
    (
        Response::Err {
            code: "client_gesture".into(),
            message: format!(
                "a {fingers}-finger gesture belongs to the focused client; the shell takes {} or more",
                crate::compositor::backend::drm::gestures::SHELL_FINGER_THRESHOLD
            ),
        },
        Vec::new(),
    )
}

/// [`Command::Swipe`]: begin, `steps` equal updates, and an end unless the
/// fingers stay down.
fn swipe(
    fingers: u32,
    (dx, dy): (f64, f64),
    steps: Option<u32>,
    release: bool,
) -> (Response, Vec<PlannedAction>) {
    if !crate::compositor::backend::drm::gestures::claims(fingers) {
        return client_gesture(fingers);
    }
    let steps = steps.unwrap_or(10).clamp(1, MAX_GESTURE_STEPS);
    let n = f64::from(steps);
    let mut actions = vec![PlannedAction::Touchpad(TouchpadEdge::SwipeBegin {
        fingers,
    })];
    actions.extend((0..steps).map(|_| {
        PlannedAction::Touchpad(TouchpadEdge::SwipeUpdate {
            dx: dx / n,
            dy: dy / n,
        })
    }));
    if release {
        actions.push(PlannedAction::Touchpad(TouchpadEdge::SwipeEnd {
            cancelled: false,
        }));
    }
    (Response::Ok { frame: 0 }, actions)
}

/// [`Command::Pinch`]: begin, `steps` updates easing the absolute scale
/// from 1 to `scale`, end.
fn pinch(fingers: u32, scale: f64, steps: Option<u32>) -> (Response, Vec<PlannedAction>) {
    if !crate::compositor::backend::drm::gestures::claims(fingers) {
        return client_gesture(fingers);
    }
    let steps = steps.unwrap_or(10).clamp(1, MAX_GESTURE_STEPS);
    let n = f64::from(steps);
    let mut actions = vec![PlannedAction::Touchpad(TouchpadEdge::PinchBegin {
        fingers,
    })];
    actions.extend((1..=steps).map(|s| {
        PlannedAction::Touchpad(TouchpadEdge::PinchUpdate {
            scale: 1.0 + (scale - 1.0) * f64::from(s) / n,
        })
    }));
    actions.push(PlannedAction::Touchpad(TouchpadEdge::PinchEnd));
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

/// [`Command::SeatKey`]: a press+release pair for the compositor's input funnel.
fn seat_key_press_release(key: &str) -> (Response, Vec<PlannedAction>) {
    let (response, actions) = key_press_release(key);
    let actions = actions
        .into_iter()
        .map(|action| match action {
            PlannedAction::KeyboardKey { key, pressed } => PlannedAction::SeatKey { key, pressed },
            other => other,
        })
        .collect();
    (response, actions)
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
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        // A line break or tab in typed text is the key that makes one, as a
        // person typing it would press; `\r\n` is one line break.
        let control = match c {
            '\r' if chars.peek() == Some(&'\n') => continue,
            '\n' | '\r' => Some(NamedKeyTag::Enter),
            '\t' => Some(NamedKeyTag::Tab),
            _ => None,
        };
        if let Some(named) = control {
            for pressed in [true, false] {
                actions.push(PlannedAction::KeyboardKey {
                    key: KeyTag::Named(named),
                    pressed,
                });
            }
            continue;
        }
        let tag = KeyTag::Char(c);
        let shift = KeyTag::Named(NamedKeyTag::Shift);
        // Shifted punctuation needs the same bracket as letters — both reach the client as a keyboard position, not a character. See `client_input::unshifted`.
        let (_, shifted) = crate::shell::input_policy::unshifted(c);
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
fn state_dump<S: ControlState>(state: &S, include_history: bool) -> (Response, Vec<PlannedAction>) {
    (
        Response::State {
            workspace_tree: state.workspace_tree_json(include_history),
            chrome: state.chrome_state_json(),
            peers: state.peers_json(),
            sharing: state.sharing_json(),
        },
        Vec::new(),
    )
}

/// [`Command::Windows`]: one row per tab of `desktop` — where it is, which
/// app shows it, and whether it is the tab in front of the active
/// workspace's active strip and group. A workspace or strip without a name
/// is named by its id; `kind` is the tile's variant and `app_id` the app it
/// names, if any.
#[must_use]
pub fn window_rows(desktop: &crate::shell::state::DesktopTree) -> Vec<serde_json::Value> {
    fn named(name: &str, id: u64) -> serde_json::Value {
        if name.is_empty() {
            id.into()
        } else {
            name.into()
        }
    }
    let mut rows = Vec::new();
    for workspace in &desktop.workspaces {
        let workspace_active = workspace.id == desktop.active;
        let active_strip = desktop.active_strip.get(&workspace.id);
        for strip in &workspace.strips {
            let strip_active = workspace_active && active_strip == Some(&strip.id);
            let active_group = desktop.active_group.get(&strip.id);
            for group in &strip.window_groups {
                let group_active = strip_active && active_group == Some(&group.id);
                for tab in &group.tabs {
                    let (kind, app_id) = tile_kind(&tab.tile);
                    rows.push(serde_json::json!({
                        "workspace": named(&workspace.name, workspace.id.0),
                        "strip": named(&strip.name, strip.id.0),
                        "group": group.id,
                        "tab": tab.id,
                        "kind": kind,
                        "app_id": app_id,
                        "title": tab.title,
                        "active": group_active && group.active_tab == tab.id,
                    }));
                }
            }
        }
    }
    rows
}

/// A tile's variant name and the `app_id` it carries, as its JSON spells them.
fn tile_kind(tile: &crate::shell::state::TileContent) -> (String, serde_json::Value) {
    match serde_json::to_value(tile).unwrap_or_default() {
        serde_json::Value::String(kind) => (kind, serde_json::Value::Null),
        serde_json::Value::Object(variant) => variant.into_iter().next().map_or_else(
            || (String::new(), serde_json::Value::Null),
            |(kind, body)| (kind, body.get("app_id").cloned().unwrap_or_default()),
        ),
        _ => (String::new(), serde_json::Value::Null),
    }
}

/// [`Command::Grant`]: grant a capability through the authority check.
fn grant<S: ControlState>(
    state: &S,
    subject: &str,
    scope: &str,
    access: &str,
    id: Option<&str>,
) -> (Response, Vec<PlannedAction>) {
    (
        answer_or_err(
            state.grant_capability(subject, scope, access, id),
            "grant_refused",
        ),
        Vec::new(),
    )
}

/// [`Command::CheckAction`]: ask the authority check whether `actor` may invoke the action.
fn check_action<S: ControlState>(
    state: &S,
    actor: &str,
    app: &str,
    resource: &str,
    action: &str,
    kind: &str,
    destructive: bool,
) -> (Response, Vec<PlannedAction>) {
    (
        answer_or_err(
            state.check_action(actor, app, resource, action, kind, destructive),
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

fn clipboard_copy_image() -> (Response, Vec<PlannedAction>) {
    let rgba = [0xff, 0x00, 0x80, 0xff, 0x10, 0x20, 0x30, 0x40];
    if ui::text_selection::clipboard_copy_image(2, 1, &rgba).outcome() == Some(true) {
        (Response::Ok { frame: 0 }, Vec::new())
    } else {
        (
            Response::Err {
                code: "clipboard_image_refused".into(),
                message: "the image clipboard backend refused the probe".into(),
            },
            Vec::new(),
        )
    }
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
                device: None,
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

/// Plan an invocation for an enrolled device. The address is not resolved
/// against this machine: the provider token belongs to the device that will
/// run it.
fn plan_enrolled_device_action(
    device: &str,
    provider: &str,
    id: &str,
    args: &serde_json::Map<String, serde_json::Value>,
) -> (Response, Vec<PlannedAction>) {
    if decode_device_id(device).is_none() {
        return (
            Response::Err {
                code: "invalid_arguments".into(),
                message: "device is not an enrolled device id".into(),
            },
            Vec::new(),
        );
    }
    let Some(address) = remote_action_address(id) else {
        return (
            Response::Err {
                code: "unknown_action".into(),
                message: "action id is not a resource address".into(),
            },
            Vec::new(),
        );
    };
    if provider.is_empty() {
        return (
            Response::Err {
                code: "unknown_action".into(),
                message: "action id is not a resource address".into(),
            },
            Vec::new(),
        );
    }
    let id = address.encode();
    (
        Response::Answer(serde_json::json!({
            "device": device,
            "provider": provider,
            "id": id,
            "queued": true,
        })),
        vec![PlannedAction::InvokeAction {
            provider: provider.to_string(),
            id,
            args: args.clone(),
            device: Some(device.to_string()),
        }],
    )
}

/// A catalogue address, or `resource/action` when the action name itself
/// contains no slash. Resource ids such as `files.entry:/tmp/report` stay on
/// the catalogue separator; a slash there would be ambiguous.
fn remote_action_address(id: &str) -> Option<tilo_actions::ActionAddress> {
    if let Some(address) = tilo_actions::ActionAddress::parse(id) {
        return Some(address);
    }
    let (resource, action) = id.split_once('/')?;
    if resource.is_empty() || action.is_empty() || action.contains('/') {
        return None;
    }
    Some(tilo_actions::ActionAddress::new(resource, action))
}

fn decode_device_id(value: &str) -> Option<[u8; 32]> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return None;
    }
    hex::decode(value).ok()?.try_into().ok()
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

/// Parse a sandbox scope string.
///
/// # Errors
///
/// Returns a human-readable message when `s` matches neither `fs:<path>` nor `device:<id>`.
pub fn parse_scope(s: &str) -> Result<crate::shell::state::Scope, String> {
    use crate::shell::state::Scope;
    match s.split_once(':') {
        Some(("fs", path)) => Ok(Scope::FsPath(path.to_string())),
        Some(("device", d)) => Ok(Scope::Device(d.to_string())),
        _ => Err(format!("scope must be fs:<path> or device:<id>, got `{s}`")),
    }
}

/// Parse a sandbox access name.
///
/// # Errors
///
/// Returns a human-readable message when `s` is not read / write.
pub fn parse_access(s: &str) -> Result<crate::shell::state::Access, String> {
    use crate::shell::state::Access;
    match s {
        "read" => Ok(Access::Read),
        "write" => Ok(Access::Write),
        _ => Err(format!("access must be read / write, got `{s}`")),
    }
}

/// Parse an action kind name.
///
/// # Errors
///
/// Returns a human-readable message when `s` is not read / undoable / outside.
pub fn parse_kind(s: &str) -> Result<tilo_action_policy::ActionKind, String> {
    use tilo_action_policy::ActionKind;
    match s {
        "read" => Ok(ActionKind::Read),
        "undoable" => Ok(ActionKind::Undoable),
        "outside" => Ok(ActionKind::Outside),
        _ => Err(format!("kind must be read / undoable / outside, got `{s}`")),
    }
}

/// Parse a grant subject: `person:<id>`, `app:<id>` or `agent:<id>`.
///
/// # Errors
///
/// Returns a human-readable message for any other spelling.
pub fn parse_policy_subject(s: &str) -> Result<tilo_action_policy::Subject, String> {
    use tilo_action_policy::Subject;
    match s.split_once(':') {
        Some(("person", id)) => Ok(Subject::person(id)),
        Some(("app", id)) => Ok(Subject::app(id)),
        Some(("agent", id)) => Ok(Subject::agent(id)),
        _ => Err(format!(
            "subject must be person:<id>, app:<id> or agent:<id>, got `{s}`"
        )),
    }
}

/// Parse a grant's answer: allow / ask / deny.
///
/// # Errors
///
/// Returns a human-readable message for any other word.
pub fn parse_answer(s: &str) -> Result<tilo_action_policy::Answer, String> {
    use tilo_action_policy::Answer;
    match s {
        "allow" => Ok(Answer::Allow),
        "ask" => Ok(Answer::Ask),
        "deny" => Ok(Answer::Deny),
        _ => Err(format!("answer must be allow / ask / deny, got `{s}`")),
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

/// Parse a request line: the [`Command`] plus the optional `agent` (id) and
/// `agent_label` fields that name who it acts for.
///
/// # Errors
///
/// [`Response::Err`] `parse_error` for a line that is no command, and
/// `invalid_agent` for an empty, overlong or control-character agent id.
#[allow(
    clippy::result_large_err,
    reason = "same wire Response error as parse_command, whose rationale applies unchanged"
)]
pub fn parse_request(line: &str) -> Result<Request, Response> {
    tilo_control::protocol::parse_request(line).map_err(|error| Response::Err {
        code: error.code.to_owned(),
        message: error.message,
    })
}

/// Protocol error for a command an agent-control connection may not send.
pub const AGENT_CONTROL_NOT_PERMITTED: &str =
    "the agent-control socket serves input, screenshots and the watch signal only";

/// Protocol error for a command the device admin's control socket does not
/// serve. It names where the rest of the protocol lives, so a caller that
/// reached for input learns the semantic route rather than retrying.
pub const ADMIN_CONTROL_NOT_PERMITTED: &str = "the admin control socket serves the action model only \
     (actions list/invoke/status/cancel, windows, state-dump, foreign-frames); synthetic input and \
     screenshots exist only on a development session's test-control socket ($TILO_TEST_CONTROL)";

/// Protocol error for an admin control command while first use runs: only
/// the onboarding and authentication actions are served until it ends.
pub const ADMIN_CONTROL_FIRST_USE_ONLY: &str =
    "first use serves only its trusted onboarding and authentication actions";

/// Protocol error for an action or screenshot asked for while the session
/// is locked: nothing ran, and only the person at the device can unlock it.
pub const SESSION_LOCKED: &str = "the session is locked; unlock it at the device, then retry";

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
#[path = "test_control_tests.rs"]
mod tests;

use std::io::BufRead;
use winit::event::{ElementState, WindowEvent};
use winit::keyboard::Key;
pub const TEST_CONTROL_MAX_LINE_BYTES: usize = 64 * 1024;
/// Read up to one `\n`-terminated line from `reader`, capped at
/// `TEST_CONTROL_MAX_LINE_BYTES`. Returns:
///
/// * `Ok(Some(line))` — complete line, trailing newline included (so
///   `parse_command` needs no change).
/// * `Ok(None)` — EOF with no newline seen (peer closed mid-line).
/// * `Err(TooLarge)` — cap fired before a newline; caller must close.
/// * `Err(Io(e))` — I/O failure, including `WouldBlock`.
pub enum ReadLineError {
    TooLarge,
    Io(std::io::Error),
}

/// Read one bounded UTF-8 line, retaining a nonempty final fragment at EOF.
/// # Errors
/// Returns `TooLarge` before exceeding the bound, or an I/O/UTF-8 error.
pub fn read_bounded_line<R: BufRead>(
    reader: &mut R,
    out: &mut String,
    max_bytes: usize,
) -> Result<Option<()>, ReadLineError> {
    BoundedLineBuffer::default().read(reader, out, max_bytes)
}

/// One connection's bounded request prefix. Nonblocking readers retain it
/// across `WouldBlock`, including a UTF-8 character split between reads.
/// Consumed request bytes are zeroized after delivery and on connection drop.
#[derive(Default)]
pub struct BoundedLineBuffer(zeroize::Zeroizing<Vec<u8>>);

impl BoundedLineBuffer {
    /// Read one bounded line, preserving an incomplete prefix until more
    /// bytes arrive. A nonempty final fragment at EOF keeps the existing policy.
    ///
    /// # Errors
    /// Returns `TooLarge` for the cumulative line size, or an I/O/UTF-8 error.
    /// `WouldBlock` preserves the pending bytes for the next call.
    pub fn read<R: BufRead>(
        &mut self,
        reader: &mut R,
        out: &mut String,
        max_bytes: usize,
    ) -> Result<Option<()>, ReadLineError> {
        loop {
            let available = match reader.fill_buf() {
                Ok(bytes) => bytes,
                Err(error) => {
                    if error.kind() != std::io::ErrorKind::WouldBlock {
                        zeroize::Zeroize::zeroize(&mut *self.0);
                    }
                    return Err(ReadLineError::Io(error));
                }
            };
            if available.is_empty() {
                return if self.0.is_empty() {
                    Ok(None)
                } else {
                    self.finish(out)
                };
            }
            let (consumed, found_newline) = available
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or((available.len(), false), |index| {
                    (index.saturating_add(1), true)
                });
            if self.0.len().saturating_add(consumed) > max_bytes {
                reader.consume(max_bytes.saturating_sub(self.0.len()));
                zeroize::Zeroize::zeroize(&mut *self.0);
                return Err(ReadLineError::TooLarge);
            }
            let take = available.get(..consumed).unwrap_or(available);
            let taken = take.len();
            self.0.extend_from_slice(take);
            reader.consume(taken);
            if found_newline {
                return self.finish(out);
            }
        }
    }

    fn finish(&mut self, out: &mut String) -> Result<Option<()>, ReadLineError> {
        let result = std::str::from_utf8(&self.0)
            .map(|line| out.push_str(line))
            .map_err(|error| {
                ReadLineError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, error))
            });
        zeroize::Zeroize::zeroize(&mut *self.0);
        result.map(|()| Some(()))
    }
}

/// Build a `WindowEvent::KeyboardInput` for `key`; `text` is set for
/// `Character` keys so text-input branches see the same glyph real hardware
/// would send.
pub fn keyboard_input_event(key: &Key, pressed: bool) -> WindowEvent {
    ui::synthetic_input::keyboard_input(ui::synthetic_input::key_event(
        key.clone(),
        if let Key::Character(s) = key {
            Some(s.clone())
        } else {
            None
        },
        if pressed {
            ElementState::Pressed
        } else {
            ElementState::Released
        },
    ))
}
#[cfg(test)]
mod framing_tests {
    use super::*;
    #[test]
    fn nonblocking_line_retains_split_utf8_and_the_next_line() {
        use std::io::{BufReader, Write as _};
        use std::os::unix::net::UnixStream;

        let (server, mut client) = UnixStream::pair().unwrap();
        server.set_nonblocking(true).unwrap();
        let mut reader = BufReader::new(server);
        let mut pending = BoundedLineBuffer::default();
        let mut out = String::new();
        client.write_all(&[0xf0, 0x9f]).unwrap();
        assert!(matches!(
            pending.read(&mut reader, &mut out, 64),
            Err(ReadLineError::Io(error)) if error.kind() == std::io::ErrorKind::WouldBlock
        ));
        assert!(out.is_empty(), "a UTF-8 prefix is not yet a request");
        client.write_all(&[0x98, 0x80, b'\n']).unwrap();
        client.write_all(b"next\n").unwrap();
        assert!(matches!(
            pending.read(&mut reader, &mut out, 64),
            Ok(Some(()))
        ));
        assert_eq!(out, "😀\n");
        out.clear();
        assert!(matches!(
            pending.read(&mut reader, &mut out, 64),
            Ok(Some(()))
        ));
        assert_eq!(out, "next\n", "delivery resets only the completed prefix");
    }

    #[test]
    fn nonblocking_line_cap_applies_across_fragments() {
        use std::io::{BufReader, Write as _};
        use std::os::unix::net::UnixStream;

        let (server, mut client) = UnixStream::pair().unwrap();
        server.set_nonblocking(true).unwrap();
        let mut reader = BufReader::new(server);
        let mut pending = BoundedLineBuffer::default();
        let mut out = String::new();
        client.write_all(b"abc").unwrap();
        assert!(matches!(
            pending.read(&mut reader, &mut out, 4),
            Err(ReadLineError::Io(error)) if error.kind() == std::io::ErrorKind::WouldBlock
        ));
        client.write_all(b"def\n").unwrap();
        assert!(matches!(
            pending.read(&mut reader, &mut out, 4),
            Err(ReadLineError::TooLarge)
        ));
        assert!(out.is_empty(), "an oversized line must never be dispatched");
    }

    /// `read_bounded_line` caps a malicious/buggy client's input at
    /// `TEST_CONTROL_MAX_LINE_BYTES`; the caller closes the connection on `TooLarge`.
    #[test]
    fn pump_drops_oversized_lines() {
        use std::io::BufReader;
        let stream: Vec<u8> = vec![b'A'; 100_000];
        let mut reader = BufReader::new(&stream[..]);
        let mut out = String::new();
        let res = read_bounded_line(&mut reader, &mut out, TEST_CONTROL_MAX_LINE_BYTES);
        match res {
            Err(ReadLineError::TooLarge) => {
                // Expected: `out` is intentionally still empty — truncated bytes aren't
                // surfaced to the parser since they'd parse as garbage anyway.
                assert!(
                    out.is_empty(),
                    "read_bounded_line must not hand a truncated line \
                     to the parser; got {} bytes",
                    out.len()
                );
            }
            Err(ReadLineError::Io(e)) => {
                panic!("expected TooLarge on a 100k newline-less stream; got io: {e}")
            }
            Ok(opt) => panic!(
                "expected TooLarge on a 100k newline-less stream; got Ok({:?})",
                opt.is_some()
            ),
        }
    }
}

#[cfg(test)]
mod agent_request_tests {
    use super::*;

    fn code(response: Response) -> String {
        match response {
            Response::Err { code, .. } => code,
            other => panic!("expected an error, got {other:?}"),
        }
    }

    #[test]
    fn a_request_names_the_agent_it_acts_for() {
        let request = parse_request(
            r#"{"op":"key_down","key":"Shift","agent":"mcp:1","agent_label":"Claude\u0007 Code"}"#,
        )
        .unwrap_or_else(|error| panic!("{error:?}"));
        assert_eq!(
            request,
            Request {
                command: Command::KeyDown {
                    key: "Shift".into()
                },
                agent: Some(Agent {
                    id: "mcp:1".into(),
                    label: Some("Claude Code".into()),
                }),
                window: None,
            }
        );
        let plain =
            parse_request(r#"{"op":"list_targets"}"#).unwrap_or_else(|error| panic!("{error:?}"));
        assert_eq!(plain.agent, None, "a line naming nobody acts for nobody");
        let blank_label = parse_request(r#"{"op":"agent_seat","agent":"a","agent_label":"  "}"#)
            .unwrap_or_else(|error| panic!("{error:?}"));
        assert_eq!(blank_label.agent.and_then(|agent| agent.label), None);
    }

    #[test]
    fn a_malformed_agent_is_refused_not_ignored() {
        let long = "x".repeat(MAX_AGENT_ID_CHARS + 1);
        for line in [
            r#"{"op":"key","key":"a","agent":""}"#.to_string(),
            r#"{"op":"key","key":"a","agent":"a\nb"}"#.to_string(),
            r#"{"op":"key","key":"a","agent":7}"#.to_string(),
            r#"{"op":"key","key":"a","agent":"a","agent_label":3}"#.to_string(),
            format!(r#"{{"op":"key","key":"a","agent":"{long}"}}"#),
        ] {
            let error = parse_request(&line)
                .err()
                .unwrap_or_else(|| panic!("{line}"));
            assert_eq!(code(error), "invalid_agent", "{line}");
        }
        let error = parse_request(r#"{"op":"go_fishing","agent":"a"}"#).err();
        assert_eq!(error.map(code).as_deref(), Some("parse_error"));
    }

    #[test]
    fn a_request_can_aim_at_one_apps_window() {
        let request = parse_request(r#"{"op":"click_at","x":1,"y":2,"window":"so.tilo.canvas"}"#)
            .unwrap_or_else(|error| panic!("{error:?}"));
        assert_eq!(request.window.as_deref(), Some("so.tilo.canvas"));
        assert_eq!(request.command, Command::ClickAt { x: 1.0, y: 2.0 });
        for line in [
            r#"{"op":"key","key":"a","window":""}"#,
            r#"{"op":"key","key":"a","window":5}"#,
            r#"{"op":"key","key":"a","window":"a\u0000b"}"#,
        ] {
            let error = parse_request(line)
                .err()
                .unwrap_or_else(|| panic!("{line}"));
            assert_eq!(code(error), "invalid_window", "{line}");
        }
    }

    #[test]
    fn seat_and_leave_need_an_agent_and_pass_the_agent_socket() {
        struct NoState;
        impl ControlState for NoState {
            fn widget_registry(&self) -> HashMap<String, Rect> {
                HashMap::new()
            }
        }
        for command in [Command::AgentSeat, Command::AgentLeave] {
            assert!(command.agent_permitted());
            let (response, actions) = dispatch_command(&command, &NoState);
            assert_eq!(code(response), AGENT_REQUIRED);
            assert!(actions.is_empty());
        }
    }

    #[test]
    fn test_identity_provisioning_is_neither_pure_nor_agent_reachable() {
        struct NoState;
        impl ControlState for NoState {
            fn widget_registry(&self) -> HashMap<String, Rect> {
                HashMap::new()
            }
        }
        let request = parse_request(r#"{"op":"provision_test_identity"}"#)
            .unwrap_or_else(|error| panic!("{error:?}"));
        assert!(matches!(request.command, Command::ProvisionTestIdentity));
        assert!(!Command::ProvisionTestIdentity.agent_permitted());
        let (response, actions) = dispatch_command(&Command::ProvisionTestIdentity, &NoState);
        assert_eq!(code(response), "test_transport_required");
        assert!(actions.is_empty());
    }
}

#[allow(
    unused_imports,
    reason = "The native WorkspaceExt methods are selected by host-specific workspace code and test configurations."
)]
use crate::shell::state::WorkspaceExt as _;
