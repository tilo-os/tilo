//! Shared control-wire vocabulary for desktop, console and tooling endpoints.
//!
//! Endpoint authorization stays with the host. Test-only commands are parsed by
//! this vocabulary but never become available merely because a client can name
//! them. The two fault payloads are supplied by the compositor; other clients
//! can preserve their JSON without depending on rendering or hardware crates.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Version of the shared JSON grammar recorded by the interface policy check.
pub const CONTROL_INTERFACE_VERSION: u32 = 1;

/// Which pointer button a [`Command::PointerButton`] means — spelled out because X11, evdev and winit each number buttons differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PointerButtonKind {
    #[default]
    Left,
    Right,
    Middle,
}

/// Phase of a direct touchscreen contact injected through the explicit
/// test-control socket. Kept separate from winit's type so the wire remains
/// stable and serde-friendly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TouchPhaseKind {
    Down,
    Motion,
    Up,
    Cancel,
    Frame,
}

/// Top-level command envelope. Tag is `op`; remaining fields are per-variant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Command<GpuFault = serde_json::Value, WakeFault = serde_json::Value> {
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
        /// Draw the drag with a pen rather than the mouse: the tip's
        /// pressure (`0.0..=1.0`), one value for the whole stroke or several
        /// spread evenly from its start to its end. The pen comes into range
        /// at the start, touches down, moves, lifts and leaves range at the
        /// end, so an app sees exactly what a tablet sends.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pressure: Option<Vec<f64>>,
    },
    /// Per-surface frame counters for every tracked foreign toplevel. Read-only; lets a harness tell a painted app from one still showing the first-frame placeholder.
    ForeignFrames,
    /// The compositor process's own memory by kind of mapping (heap, stacks, GPU driver memory, fonts, files), glibc's heap split and its threads by name, in KiB (the host's memory report). Read-only; vm-acceptance R01 records it because Tilo's kernel has no smaps.
    ProcessMemory,
    /// Inject a key press + release. `key` is a winit-style name or a single character; unknown names return `parse_error`.
    Key { key: String },
    /// Inject a key press + release the way a keyboard attached to the machine delivers it: the
    /// compositor loop takes it from its input funnel with the backend's own events, so routing,
    /// shortcuts and whether it asks for a frame are decided exactly as for a physical key.
    /// `key` is named as for [`Command::Key`]. Test-only.
    SeatKey { key: String },
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
    /// Say whether a viewer is looking at a headless session's frames. Unwatched, the headless backend lays frames out and hands clients their frame callbacks but does not rasterize them; attaching a viewer paints a fresh frame, and shared legacy windows are encoded only while watched. `snapshot` renders on demand either way. Other backends always paint. Answered by the native transport; the agent-control socket accepts it, so a partner's `POST /watch` lease reaches it.
    Watch { watching: bool },
    /// Read-only snapshot of the live `shell::state::WorkspaceTree` plus chrome state, both opaque JSON so future fields don't break the wire.
    ///
    /// The tree is the desktop as it is now. A workspace's append-only history — effect receipts, task audit trails, ownership-transfer records — grows with every action ever taken, so it is left out unless `include_history` asks for it (`tilo_desktop_model::history`).
    StateDump {
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        include_history: bool,
    },
    /// The open windows, one row per tab: `workspace`, `strip`, `group`, `tab`, `kind`, `app_id`, `title`, and `active` for the tab in front of the active workspace's active group. Read straight off the live tree, so it costs the windows, not a state dump.
    Windows,
    /// Grant a sandbox capability on the active workspace on behalf of the user at this session, through the tree's governance entry point — the production authority check, so a grantor who may not grant in the room is refused, not worked around.
    ///
    /// `subject` is `person:<id>` / `app:<id>` / `agent:<id>`; `scope` is `workspace`, `room`, `fs:<path>` or `device:<id>`; `access` is read / write.
    Grant {
        subject: String,
        scope: String,
        access: String,
        /// Stable capability id so a later `revoke` can name it; minted when omitted.
        #[serde(default)]
        id: Option<String>,
    },
    /// Revoke sandbox capability `id` on the active workspace — same authority check as `grant`.
    Revoke { id: String },
    /// Give `subject` (`person:<id>` / `app:<id>` / `agent:<id>`) `answer` (allow / ask / deny) for actions of `kinds` (read / undoable / outside) at `node`, a path below the room such as `app:so.tilo.notes`, on the active workspace, as the user at this session — through the room's governance, so a grantor who may not grant there is refused.
    Allow {
        subject: String,
        node: String,
        kinds: Vec<String>,
        answer: String,
        /// Stable grant id so a later `disallow` can name it; minted when omitted.
        #[serde(default)]
        id: Option<String>,
    },
    /// Take back grant `id` on the active workspace — same authority check as `allow`.
    Disallow { id: String },
    /// Admit `person` to the active workspace with `role` (viewer / commenter / editor), as the user at this session.
    Admit { person: String, role: String },
    /// Ask the room's policy what `actor` gets for `action` on `app`'s `resource`, an action of `kind` (read / undoable / outside), against the live workspace.
    CheckAction {
        actor: String,
        app: String,
        resource: String,
        action: String,
        kind: String,
        #[serde(default)]
        destructive: bool,
    },
    /// Route the next window announcing `app_id` into its own group rather than a tab, via `shell::launcher::set_pending_placement` — needed because dragging between two client windows requires them in separate groups.
    PlaceNextWindow { app_id: String },
    /// Mint the compositor-owned, single-use activation token a user-selected
    /// host launch would receive.  The native test transport handles this
    /// directly because token issuance deliberately requires `&mut State`.
    /// Only available on the explicit test-control socket.
    MintHostLaunchActivation {
        desktop_id: String,
        display_name: String,
    },
    /// Mint a throwaway collaboration identity for the session's user.
    /// Answered by the native test transport, and only by a dev build
    /// (`compositor::dev_test_identity`); never on the agent-control socket.
    ProvisionTestIdentity,
    /// Deadlock the compositor thread on a lock it holds, as a compositor
    /// that read-locks its workspace tree under its own write once did. The
    /// main-loop watchdog's end-to-end test uses it to prove a hung desktop
    /// is recorded and ended (`compositor::watchdog`). Answered by the native
    /// test transport of a development build only, and never: the reply is
    /// the compositor exiting.
    DeadlockMainLoop,
    /// Have the presenting renderer's GPU fail under its next frame, as the
    /// GPU of a laptop whose firmware died once did (`paint::InjectedGpuFault`).
    /// Answered by the native test transport of a development build only.
    GpuFault { fault: GpuFault },
    /// Turn the display off now, as the idle timeout does; input wakes it
    /// again. Answered by the native test transport.
    DisplayBlank,
    /// Take the wake stage `stage` (`commit`, `link`, `crtc` or `backlight`)
    /// as failed in every output's next wake, whatever it observes, so a test
    /// proves on hardware that never fails that the wake check sees it and
    /// redoes the modeset. Answered by the native test transport.
    DisplayWakeFault { stage: WakeFault },
    /// Move the pointer to `(x, y)` without pressing anything. [`Command::ClickAt`]/[`Command::Drag`] bundle press+release, which can't express drag-and-drop: the source must be *holding* the button when it calls `start_drag`, with the drop on a release after the pointer has moved over another client. These ops decompose that.
    PointerMove { x: f64, y: f64 },
    /// Give every window group in the active strip the same share of it, in permille. A group defaults to about three quarters of its strip, so two never fit together without this.
    SetGroupWidth { permille: u32 },
    /// Seed the production hand-off slot used by the hosted file manager. Files' own widget tests prove pointer-to-drag arming; compositor integration tests use this to isolate the Wayland hand-off boundary.
    SeedFilesDrag { paths: Vec<PathBuf> },
    /// Press or release a pointer button at the current position. `button` defaults to the left one; the right button drives the "select, right-click" gesture this desktop leans on.
    PointerButton {
        pressed: bool,
        #[serde(default)]
        button: PointerButtonKind,
    },
    /// Inject one direct-touch hardware edge through the shared DRM touch
    /// ownership state. Only available on the explicitly opted-in test socket;
    /// it exists so E2E clients can assert real `wl_touch` protocol delivery.
    Touch {
        device: String,
        slot: u32,
        phase: TouchPhaseKind,
        x: f64,
        y: f64,
    },
    /// A touchpad swipe of `fingers` fingers travelling `(dx, dy)` logical
    /// pixels over `steps` updates (default 10), fed to the gesture tracker
    /// the DRM runtime feeds from libinput, under the default direction
    /// preference. Fewer than three fingers are the focused client's and
    /// are refused. `release: false` keeps the fingers down, to inspect a
    /// gesture in flight; [`Self::SwipeEnd`] lifts them.
    Swipe {
        fingers: u32,
        dx: f64,
        dy: f64,
        #[serde(default)]
        steps: Option<u32>,
        #[serde(default = "default_release")]
        release: bool,
    },
    /// Lift the fingers of a swipe left in flight; `cancelled` as libinput
    /// reports a cancelled gesture.
    SwipeEnd {
        #[serde(default)]
        cancelled: bool,
    },
    /// A touchpad pinch of `fingers` fingers to absolute `scale` over `steps`
    /// updates (default 10), through the same tracker as [`Self::Swipe`].
    Pinch {
        fingers: u32,
        scale: f64,
        #[serde(default)]
        steps: Option<u32>,
    },
    /// Copy `text` from in-process chrome, through `ui::text_selection::clipboard_copy` — lands on the real clipboard backend and is published as a Wayland selection on the next tick.
    ClipboardCopy {
        text: String,
        /// `true` targets the primary selection instead of the clipboard.
        #[serde(default)]
        primary: bool,
    },
    /// Copy a deterministic RGBA probe from in-process chrome. This exercises
    /// the same image clipboard route used by screenshot capture.
    ClipboardCopyImage,
    /// Paste from in-process chrome, through `ui::text_selection::clipboard_paste`. Returns a clipboard response with the text, or `null` when empty.
    ClipboardPaste {
        /// `true` reads the primary selection instead of the clipboard.
        #[serde(default)]
        primary: bool,
    },
    /// Enumerate the actions/resources the shell exports now, through the same provider registry that feeds the Ctrl+K palette. Read-only — no invoke handles in the reply. An empty `query` lists every provider's defaults, and adds `launcher`: the sections global search shows before anything is typed, in order (Happening now, Waiting on you, Continue, Apps), each with its groups of rows and every row's reason, for the workspace in front or, with `every_workspace`, all of them.
    ///
    /// The answer is one page of the listing ([`ListingRequest`]); a request without paging fields gets the first.
    ActionsEnumerate {
        #[serde(default, deserialize_with = "default_string")]
        query: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        include_programmatic: bool,
        #[serde(flatten)]
        page: ListingRequest,
        /// Hold the answer until the listing is `complete` — no index build
        /// or app directory page that would change it still owed — for at
        /// most this many milliseconds (capped at 30 s). The answer says
        /// whether it `settled`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        wait_ms: Option<u64>,
        /// With `wait_ms`: a listing with this `revision` was already seen,
        /// so only a different one settles the wait — "answer when this
        /// changes", e.g. when an app just opened publishes its actions.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        after_revision: Option<String>,
    },
    /// Invoke one enumerated action by its `(provider, id)` address, with no window, scene or synthetic input.
    ///
    /// The address is resolved against a fresh catalogue first, so an unknown or hidden id is `unknown_action`, not silently ignored. `args` is checked against the resolved parameters; a bad argument is `invalid_arguments`, deliberately distinct — "address is wrong" vs. "arguments are wrong" are fixed differently. Omitting `args` is the nullary call.
    ActionsInvoke {
        #[serde(deserialize_with = "nonempty_string")]
        provider: String,
        #[serde(deserialize_with = "nonempty_string")]
        id: String,
        #[serde(default, deserialize_with = "arguments")]
        args: serde_json::Map<String, serde_json::Value>,
        /// Enrolled device id. When set, the action is sent to that device
        /// instead of being resolved against this machine's catalogue.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device: Option<String>,
        /// Hold the answer until the invocation settles — succeeds, fails or
        /// is cancelled — for at most this many milliseconds (capped at 30
        /// s), and answer its latest receipt then. Absent, the answer is the
        /// admission receipt. Ignored with `device`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        wait_ms: Option<u64>,
    },
    /// Read the latest broker lifecycle state for an invocation receipt id.
    ActionsStatus {
        invocation_id: u64,
        /// Hold the answer until the invocation settles, as
        /// `actions_invoke`'s `wait_ms` does.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        wait_ms: Option<u64>,
    },
    /// Request cooperative cancellation for an invocation receipt id.
    ActionsCancel { invocation_id: u64 },
    /// Seat the request's [`Agent`] without injecting input: its strip takes
    /// focus, so a window launched next lands there. Answers the strip and its
    /// viewport `[x, y, w, h]` in screen pixels.
    AgentSeat,
    /// End the request's [`Agent`]: release every key and button it holds and
    /// drop its seat. Its strip stays while it holds windows.
    AgentLeave,
}

/// Longest agent id a request may name.
pub const MAX_AGENT_ID_CHARS: usize = 128;
/// Longest agent label; longer labels are cut, not refused.
pub const MAX_AGENT_LABEL_CHARS: usize = 80;

/// The participant a request acts for. Every agent has its own seat and strip:
/// input it sends is attributed to it, its held keys never reach another
/// agent's window, and its coordinates and screenshots are its strip's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Agent {
    pub id: String,
    /// Human name for the agent's strip; the id when absent.
    pub label: Option<String>,
}

/// One parsed request line: a [`Command`], optionally on behalf of an agent
/// named by the line's `agent` (and `agent_label`) fields, and optionally
/// aimed at one app's window by its `window` field (the window's app id).
#[derive(Debug, Clone, PartialEq)]
pub struct Request<GpuFault = serde_json::Value, WakeFault = serde_json::Value> {
    pub command: Command<GpuFault, WakeFault>,
    pub agent: Option<Agent>,
    /// The app id of the window an agent acts in: its window takes focus, the
    /// command's coordinates are relative to the window's top-left and its
    /// screenshots show the window alone.
    pub window: Option<String>,
}

/// Longest `window` app id a request may name.
pub const MAX_WINDOW_CHARS: usize = 255;

/// Rows an `actions_enumerate` page holds when the request names no `limit`.
/// A stock desktop's whole surface fits, so a client that predates paging
/// still sees all of it there, and `truncated` says when it does not.
pub const DEFAULT_LISTING_LIMIT: usize = 200;
/// Most rows one `actions_enumerate` page holds, whatever `limit` asks.
pub const MAX_LISTING_LIMIT: usize = 2_000;

/// Which page of an `actions_enumerate` listing to answer, and how much of
/// each row. Every field is optional on the wire.
///
/// The answer carries `count` (rows in this page), `total` (rows in the
/// listing), `truncated` and `next_cursor`: while `truncated`, send the same
/// query again with `cursor` set to `next_cursor` for the next page. A
/// cursor names a position in one listing; if the listing has changed since,
/// the request is refused with `stale_cursor` and the caller starts again.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListingRequest {
    /// Rows in the page: [`DEFAULT_LISTING_LIMIT`] when absent, at least one
    /// and at most [`MAX_LISTING_LIMIT`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    /// The previous page's `next_cursor`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    /// How much of each row: `full` (the default) or `compact`, which leaves
    /// out each row's `contract`, most of a row's size. Every other field,
    /// including `params`, stays, so a compact row can still be invoked.
    #[serde(default, skip_serializing_if = "ListingDetail::is_full")]
    pub detail: ListingDetail,
    /// Only rows of these kinds (`application`, `action`, `note`, `file`, …;
    /// singular or plural, any case). Absent: every kind but files and
    /// folders.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kinds: Vec<String>,
    /// Also list files and folders. They stay out of a listing unless asked
    /// (here or by naming their kind): a search for an app's action is
    /// answered by hundreds of same-named files otherwise, ahead of it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub files: bool,
    /// With an empty `query`: the answer's `launcher` (what global search
    /// shows before anything is typed) covers every workspace instead of
    /// the one in front.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub every_workspace: bool,
}

/// How much of each row a listing carries; see [`ListingRequest::detail`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ListingDetail {
    #[default]
    Full,
    Compact,
}

impl ListingDetail {
    #[must_use]
    pub const fn is_full(&self) -> bool {
        matches!(self, Self::Full)
    }
}

/// The rows of a listing one page holds, as [`ListingRequest::window`] cut it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListingWindow {
    /// Indices of this page's rows in the whole listing.
    pub rows: std::ops::Range<usize>,
    /// Rows in the whole listing.
    pub total: usize,
    /// Where the next page starts; `None` on the last page.
    pub next_cursor: Option<String>,
}

impl ListingWindow {
    /// Record this page's place in `answer`: `count`, `total`, `truncated`
    /// and `next_cursor`.
    pub fn describe(&self, answer: &mut serde_json::Map<String, serde_json::Value>) {
        answer.insert("count".into(), self.rows.len().into());
        answer.insert("total".into(), self.total.into());
        answer.insert("truncated".into(), self.next_cursor.is_some().into());
        answer.insert(
            "next_cursor".into(),
            self.next_cursor
                .clone()
                .map_or(serde_json::Value::Null, Into::into),
        );
    }
}

/// An identity for one listing: its rows' addresses, in order. A cursor cut
/// from one listing is stale against any other.
pub fn listing_fingerprint<'a>(addresses: impl IntoIterator<Item = (&'a str, &'a str)>) -> u64 {
    use std::hash::{Hash as _, Hasher as _};
    let mut hasher = std::hash::DefaultHasher::new();
    for (provider, id) in addresses {
        provider.hash(&mut hasher);
        id.hash(&mut hasher);
    }
    hasher.finish()
}

impl ListingRequest {
    /// The rows of a listing of `total` rows, identified by `fingerprint`
    /// ([`listing_fingerprint`]), that this page holds.
    ///
    /// # Errors
    ///
    /// `invalid_arguments` for a cursor this socket never handed out, and
    /// `stale_cursor` for one cut from a listing that has changed since.
    pub fn window(&self, total: usize, fingerprint: u64) -> Result<ListingWindow, ActionRefusal> {
        let start = match self.cursor.as_deref() {
            None => 0,
            Some(cursor) => {
                let (offset, listing) = cursor
                    .split_once('.')
                    .and_then(|(offset, listing)| {
                        Some((
                            offset.parse::<usize>().ok()?,
                            u64::from_str_radix(listing, 16).ok()?,
                        ))
                    })
                    .ok_or_else(|| {
                        ActionRefusal::invalid_arguments(
                            "cursor is not a next_cursor this listing handed out",
                        )
                    })?;
                if listing != fingerprint || offset > total {
                    return Err(ActionRefusal::stale_cursor());
                }
                offset
            }
        };
        let limit = self
            .limit
            .unwrap_or(DEFAULT_LISTING_LIMIT)
            .clamp(1, MAX_LISTING_LIMIT);
        let end = start.saturating_add(limit).min(total);
        Ok(ListingWindow {
            rows: start..end,
            total,
            next_cursor: (end < total).then(|| format!("{end}.{fingerprint:016x}")),
        })
    }

    /// Cut this page out of a whole listing already in `answer` — `actions`,
    /// one row per action — and describe it there ([`ListingWindow::describe`]).
    ///
    /// # Errors
    ///
    /// As [`Self::window`].
    pub fn apply(&self, answer: &mut serde_json::Value) -> Result<(), ActionRefusal> {
        let Some(answer) = answer.as_object_mut() else {
            return Ok(());
        };
        let mut rows = match answer.remove("actions") {
            Some(serde_json::Value::Array(rows)) => rows,
            _ => Vec::new(),
        };
        let field = |row: &serde_json::Value, name: &str| {
            row.get(name)
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        let addresses: Vec<(String, String)> = rows
            .iter()
            .map(|row| (field(row, "provider"), field(row, "id")))
            .collect();
        let window = self.window(
            rows.len(),
            listing_fingerprint(
                addresses
                    .iter()
                    .map(|(provider, id)| (provider.as_str(), id.as_str())),
            ),
        )?;
        rows.truncate(window.rows.end);
        let mut page: Vec<serde_json::Value> = rows.drain(window.rows.clone()).collect();
        if !self.detail.is_full() {
            for row in &mut page {
                if let Some(row) = row.as_object_mut() {
                    row.remove("contract");
                }
            }
        }
        answer.insert("actions".into(), page.into());
        window.describe(answer);
        Ok(())
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

    /// The listing could not be enumerated at all.
    pub fn listing_failed(message: impl Into<String>) -> Self {
        Self {
            code: "enumerate_failed",
            message: message.into(),
        }
    }

    /// A listing cursor names a place in a listing that has changed since it
    /// was handed out; list again from the start.
    #[must_use]
    pub fn stale_cursor() -> Self {
        Self {
            code: "stale_cursor",
            message:
                "the listing changed since this cursor was handed out; list again without a cursor"
                    .into(),
        }
    }
}

impl<GpuFault, WakeFault> Command<GpuFault, WakeFault> {
    /// Whether the device admin's control socket serves this command: the
    /// action model — enumerate, invoke, status, cancel — and read-only state.
    /// Synthetic input, screenshots, grants, fixtures and agent seats are not:
    /// the admin drives the session the way an assistive tool or an agent
    /// does, through typed actions, and a person's screen is not something a
    /// socket client reads or clicks.
    #[must_use]
    pub const fn admin_permitted(&self) -> bool {
        matches!(
            self,
            Self::ActionsEnumerate { .. }
                | Self::ActionsInvoke { .. }
                | Self::ActionsStatus { .. }
                | Self::ActionsCancel { .. }
                | Self::StateDump { .. }
                | Self::Windows
                | Self::ForeignFrames
        )
    }

    /// Whether an agent-control connection may send this command: pointer,
    /// keyboard and clipboard input, screenshots, read-only frame and target
    /// inventories, and the partner watch signal, which only decides whether
    /// frames are painted. Everything that grants, invokes, seeds fixtures or
    /// reaches around the input path stays test-only.
    #[must_use]
    pub const fn agent_permitted(&self) -> bool {
        matches!(
            self,
            Self::ListTargets
                | Self::ClickAt { .. }
                | Self::Drag { .. }
                | Self::ForeignFrames
                | Self::Key { .. }
                | Self::KeyDown { .. }
                | Self::KeyUp { .. }
                | Self::Scroll { .. }
                | Self::Type { .. }
                | Self::Modifiers { .. }
                | Self::Snapshot { .. }
                | Self::PointerMove { .. }
                | Self::PointerButton { .. }
                | Self::ClipboardCopy { .. }
                | Self::ClipboardPaste { .. }
                | Self::Watch { .. }
                | Self::AgentSeat
                | Self::AgentLeave
        )
    }
}

/// Parse command and participant metadata using the same rules at every endpoint.
///
/// # Errors
/// A malformed command, participant id or window id is refused before dispatch.
pub fn parse_request<
    GpuFault: serde::de::DeserializeOwned,
    WakeFault: serde::de::DeserializeOwned,
>(
    line: &str,
) -> Result<Request<GpuFault, WakeFault>, ActionRefusal> {
    let parse_error = |message: String| ActionRefusal {
        code: "parse_error",
        message,
    };
    let mut value = serde_json::from_str::<serde_json::Value>(line.trim())
        .map_err(|e| parse_error(format!("invalid JSON command: {e}")))?;
    let (agent, label, window) = match value.as_object_mut() {
        Some(fields) => (
            fields.remove("agent"),
            fields.remove("agent_label"),
            fields.remove("window"),
        ),
        None => (None, None, None),
    };
    let command = serde_json::from_value::<Command<GpuFault, WakeFault>>(value)
        .map_err(|e| parse_error(format!("invalid JSON command: {e}")))?;
    let invalid = |message: &str| ActionRefusal {
        code: "invalid_agent",
        message: message.to_string(),
    };
    let agent = match agent {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(id)) => {
            if id.is_empty()
                || id.chars().count() > MAX_AGENT_ID_CHARS
                || id.chars().any(char::is_control)
            {
                return Err(invalid(
                    "agent must be 1 to 128 characters without control characters",
                ));
            }
            let label = match label {
                Some(serde_json::Value::String(label)) => Some(label),
                None | Some(serde_json::Value::Null) => None,
                Some(_) => return Err(invalid("agent_label must be a string")),
            }
            .map(|label| {
                label
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(MAX_AGENT_LABEL_CHARS)
                    .collect::<String>()
            })
            .filter(|label| !label.trim().is_empty());
            Some(Agent { id, label })
        }
        Some(_) => return Err(invalid("agent must be a string")),
    };
    let window = match window {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(app_id))
            if !app_id.is_empty()
                && app_id.chars().count() <= MAX_WINDOW_CHARS
                && !app_id.chars().any(char::is_control) =>
        {
            Some(app_id)
        }
        Some(_) => {
            return Err(ActionRefusal {
                code: "invalid_window",
                message: "window must be an app id: 1 to 255 characters without control characters"
                    .to_string(),
            });
        }
    };
    Ok(Request {
        command,
        agent,
        window,
    })
}

fn default_release() -> bool {
    true
}

fn default_string<'de, D: serde::Deserializer<'de>>(decoder: D) -> Result<String, D::Error> {
    Ok(Option::<String>::deserialize(decoder)?.unwrap_or_default())
}

fn nonempty_string<'de, D: serde::Deserializer<'de>>(decoder: D) -> Result<String, D::Error> {
    let value = String::deserialize(decoder)?;
    if value.is_empty() {
        return Err(serde::de::Error::custom("must be a non-empty string"));
    }
    Ok(value)
}

fn arguments<'de, D: serde::Deserializer<'de>>(
    decoder: D,
) -> Result<serde_json::Map<String, serde_json::Value>, D::Error> {
    Ok(
        Option::<serde_json::Map<String, serde_json::Value>>::deserialize(decoder)?
            .unwrap_or_default(),
    )
}
