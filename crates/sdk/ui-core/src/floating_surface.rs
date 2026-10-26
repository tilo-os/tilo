//! Shared placement contract for app-bound overlays and shell-owned `PiP`.
#![allow(
    clippy::arithmetic_side_effects,
    reason = "placement math is bounded logical-pixel geometry using kurbo operators"
)]
//!
//! Apps describe *what* should float; this module owns *how* it moves.  The
//! same state machine is used while a surface is attached to its app and after
//! the shell adopts it as desktop `PiP`, avoiding a second set of drag/snap rules.

use vello::kurbo::Affine;
use vello::kurbo::{Point, Rect, Size, Vec2};

use serde::{Deserialize, Serialize};
#[cfg(unix)]
use std::collections::VecDeque;
#[cfg(unix)]
use std::io::{Read, Write};
#[cfg(all(unix, feature = "local-daemons"))]
use std::os::fd::{FromRawFd, OwnedFd};
#[cfg(unix)]
use std::os::unix::net::UnixStream;

/// A surface let go settles on its place (`motion::Snap`); the stretch a
/// change of owner gives it relaxes on the same motion.
fn settle_progress(elapsed: crate::time::Duration) -> f64 {
    crate::motion::Snap::SPEC.progress_after(
        crate::motion::Direction::Enter,
        crate::tokens::EffectiveMotion::default(),
        crate::tokens::MotionSource::Gesture,
        elapsed,
    )
}

/// How long [`settle_progress`] runs.
fn settle_time() -> crate::time::Duration {
    crate::motion::Snap::SPEC.settle_time(
        crate::motion::Direction::Enter,
        crate::tokens::EffectiveMotion::default(),
        crate::tokens::MotionSource::Gesture,
    )
}

/// Stable identity of content that can survive an app → desktop hand-off.
#[derive(Clone, Debug, PartialEq)]
pub enum FloatingSource {
    /// A semantically stable app element plus its current app-local fallback
    /// box. Identity lets the app keep updating the same source as its layout
    /// changes; the region lets the compositor present the first frame
    /// immediately without querying private app internals.
    AppElement {
        app_id: String,
        element_id: String,
        region: Rect,
    },
    AppRegion {
        app_id: String,
        region: Rect,
    },
    Surface {
        surface_id: u64,
    },
    Composite(Vec<FloatingSource>),
}

/// Presentation hints understood by both app and shell hosts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FloatingRole {
    PictureInPicture,
    ActiveCall,
    Navigation,
    LiveActivity,
    /// Interactive conversation content retained by its original app.
    Conversation,
}

impl FloatingRole {
    /// Whether interactive content stays rendered by its owning app rather
    /// than becoming a compositor-owned capture mirror.
    pub const fn uses_app_owned_host(self) -> bool {
        match self {
            Self::ActiveCall | Self::Navigation | Self::Conversation => true,
            Self::PictureInPicture | Self::LiveActivity => false,
        }
    }
}

/// What a floating surface's body is made of. A surface names its material;
/// the toolkit and compositor decide how it renders — the blur radius, the
/// tint and the opaque fallback are never the caller's
/// (`docs/design-language.md`, "Material and transparency").
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Material {
    /// Floating chrome — palette, menus, popovers, quick settings, sheets,
    /// notifications, and chrome over media: the blurred backdrop tinted
    /// with `surfaces.overlay`. Opaque `surfaces.overlay` where the renderer
    /// cannot blur, and under reduced transparency or forced colours.
    #[default]
    Floating,
    /// Always opaque `surfaces.overlay`: a surface whose backdrop must not
    /// show through, such as a prompt for a secret over content the user
    /// does not own.
    Opaque,
    /// `surfaces.overlay` at `surfaces.see_through_opacity` (half), over an
    /// unblurred backdrop: chrome that must leave the exact pixels under it
    /// visible, such as the capture bar over what is being chosen. Opaque
    /// under reduced transparency, forced colours and high contrast.
    SeeThrough,
}

/// Paint a floating surface's body: `shape` filled with `material`. Edges,
/// shadows and content are drawn by the caller on top, exactly as on an
/// opaque surface.
pub fn paint_material(
    scene: &mut vello::Scene,
    transform: Affine,
    shape: &impl vello::kurbo::Shape,
    material: Material,
    theme: &crate::theme::Theme,
) {
    crate::backdrop::fill(scene, transform, shape, material, theme);
}

/// The native allocation around retained interactive content, including the
/// floating card's frame, its edge and the themed elevation. Content
/// geometry stays unchanged: the extra pixels are a transparent frame gutter,
/// not padding inserted into the app's layout or input coordinates.
#[must_use]
pub fn app_owned_content_window_rect(
    content_rect: Rect,
    shadow: &crate::tokens::Shadow,
    rim_width: f64,
) -> Rect {
    let frame = content_rect.inflate(rim_width / 2.0, rim_width / 2.0);
    crate::surface::shadow_bounds(content_rect, shadow).map_or(frame, |bounds| frame.union(bounds))
}

/// Paint the framework-owned frame around one interactive app-owned surface
/// (an app's call or navigation card).
///
/// The frame is the one floating card
/// ([`crate::widget::floating_surface::paint_card`]): its shape, shadow and
/// edges, the same as a banner's or the palette's. Retained app content
/// stands on the ordinary opaque Object ground inside it, not a translucent
/// Panel material. The card's shape clips every app paint, and its edge is
/// drawn last so square app backgrounds cannot erase the frame. The native
/// allocation remains transparent outside the card and its shadow; hosts
/// allocate it with [`app_owned_content_window_rect`].
pub fn paint_app_owned_content(
    scene: &mut vello::Scene,
    transform: Affine,
    content_rect: Rect,
    theme: &crate::theme::Theme,
    draw: impl FnOnce(&mut vello::Scene) -> bool,
) -> bool {
    use crate::widget::floating_surface as card;
    use vello::kurbo::RoundedRect;
    use vello::peniko::{BlendMode, Fill};

    card::paint_card_shadow(scene, transform, content_rect, 1.0, theme);
    card::paint_card_body_with(
        scene,
        transform,
        content_rect,
        theme.tokens.colors.surfaces.content,
        theme,
    );
    let body = RoundedRect::from_rect(content_rect, card::card_radius(content_rect, theme));
    scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &body);
    let drawn = draw(scene);
    scene.pop_layer();
    card::paint_card_edge(scene, transform, content_rect, theme);
    drawn
}

/// The app ↔ shell interface's one version: everything an app and the
/// session shell say on the shell-ui connection (floating-surface
/// lifecycle, the window's appearance, tabs, selection). The app's first
/// frame is [`FloatingSurfaceClientMessage::Hello`] with this number; a shell
/// at another number answers [`FloatingSurfaceHostMessage::VersionMismatch`]
/// and closes, and updates whichever side is behind
/// (docs/architecture/compatibility.md). There is no negotiation and nothing
/// kept for an older number. `interface.toml` beside this crate holds the
/// schema, and the repository policy fails a change to it that keeps the
/// number.
///
/// Geometry is encoded as plain logical-pixel arrays rather than renderer
/// types so sandbox services, Wayland protocol shims, and test transports
/// can all carry the same messages.
pub const FLOATING_SURFACE_PROTOCOL_VERSION: u16 = 17;
pub const FLOATING_SURFACE_SERVICE: &str = "shell-ui";

/// What a model may know about one change it is asked to describe: the
/// change's id as History lists it (32 lowercase hex characters), its own
/// summary, the action and resource it named, and what it wrote. Never who
/// made it: History shows that beside the description.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryChangeFacts {
    pub change: String,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub wrote: String,
}

/// One change described: its id, the sentence, and which provider and model
/// wrote it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryChangeDescription {
    pub change: String,
    pub text: String,
    pub generator: String,
}

pub const FLOATING_SURFACE_APP_ID_PREFIX: &str = "so.tilo.shell-float.";
#[cfg(unix)]
const MAX_PROTOCOL_FRAME_BYTES: usize = 1024 * 1024;
/// Backpressure bound for a non-reading shell peer. Frames are normally
/// flushed immediately; this only limits the exceptional `WouldBlock` queue.
#[cfg(unix)]
const MAX_OUTGOING_QUEUE_BYTES: usize = 4 * MAX_PROTOCOL_FRAME_BYTES;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FloatingSurfaceProtocolSource {
    AppElement {
        app_id: String,
        element_id: String,
        region: [f64; 4],
    },
    AppRegion {
        app_id: String,
        region: [f64; 4],
    },
    Surface {
        surface_id: u64,
    },
    Composite {
        sources: Vec<FloatingSurfaceProtocolSource>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// Roles are additive wire values: an older peer refuses an unknown role;
/// it must never reinterpret interactive content as a capture mirror. Adding
/// a role preserves the existing framing, geometry, and legacy role values.
pub enum FloatingSurfaceProtocolRole {
    PictureInPicture,
    ActiveCall,
    Navigation,
    LiveActivity,
    Conversation,
}

/// Starting mode for an app-requested session-shell chooser. The request only
/// opens trusted shell UI; it never grants an app capture permission or pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureStudioProtocolMode {
    Screenshot,
    Recording,
    PictureInPicture,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FloatingSurfaceProtocolRequest {
    pub id: String,
    pub title: String,
    pub role: FloatingSurfaceProtocolRole,
    pub source: FloatingSurfaceProtocolSource,
    pub preferred_size: [f64; 2],
    pub aspect_ratio: Option<f64>,
    /// Desktop-space transfer rectangle for an app-owned native surface.
    /// Absent for ordinary media `PiP`.
    #[serde(default)]
    pub initial_rect: Option<[f64; 4]>,
    /// Full transparent native-window bounds, including app-drawn shadows.
    #[serde(default)]
    pub presentation_rect: Option<[f64; 4]>,
}

/// App → shell lifecycle. `Suspend` is the reversible half of an attached
/// ↔ desktop handoff: the authenticated native surface and its renderer stay
/// warm while its pixels return to the app. `Dismiss` is deliberately
/// permanent and reserved for hang-up, navigation-end, or an explicit close.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FloatingSurfaceClientMessage {
    /// The first frame on every connection: the shell-ui version this app
    /// speaks ([`FLOATING_SURFACE_PROTOCOL_VERSION`]). Its shape never
    /// changes, so a shell of any version reads it.
    Hello {
        version: u16,
    },
    /// Complete primary-surface marker inventory from a successfully presented
    /// SDK frame. The host resolves surface and room from launch custody.
    ActionMarkers {
        markers: Vec<crate::PaintedActionMarker>,
    },
    /// A typed Settings request on the existing authenticated app connection.
    MoveDeviceDisplay {
        request: u64,
        placement: settings_store::device_displays::DeviceDisplayMove,
    },
    /// Bounded response to a trusted owner operation on this same connection.
    DeviceShellOutcome {
        request: u64,
        result: Result<serde_json::Value, String>,
    },
    /// Terminal asks for the existing host-shell permission prompt.
    AllowHostShell,
    /// Ask the trusted session shell to show its ordinary user-confirmed
    /// chooser. This shares the same authenticated channel as floating
    /// surfaces so a separately hosted app and an embedded app have identical
    /// `PiP` UX.
    OpenCaptureStudio {
        mode: CaptureStudioProtocolMode,
    },
    Present {
        request: FloatingSurfaceProtocolRequest,
    },
    UpdateRegion {
        id: String,
        region: [f64; 4],
    },
    /// Live desktop placement of an app-owned secondary surface. The shell
    /// clamps it to the combined output before presentation.
    SurfaceGeometry {
        id: String,
        /// Full native-window bounds.
        rect: [f64; 4],
        /// Shared placement/hit geometry inside that transparent window.
        content_rect: [f64; 4],
        /// Interactive frame excluding shadow; absent means legacy window input.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input_rect: Option<[f64; 4]>,
    },
    /// The primary window has presented a frame with the transferred pixels
    /// removed. The shell may now move its proxy/native surface away from the
    /// source rect without ever exposing two copies.
    SourceCleared {
        id: String,
    },
    /// The primary app window has presented the first frame containing the
    /// reattached surface. Until this arrives, the compositor keeps a frozen
    /// copy of the last desktop-owned pixels in exactly the same place, so
    /// hiding the native secondary can never expose a blank transfer frame.
    DestinationPresented {
        id: String,
    },
    Suspend {
        id: String,
        rect: [f64; 4],
        content_rect: [f64; 4],
    },
    Dismiss {
        id: String,
    },
    /// Withhold every window of this app from captures — screenshots,
    /// recordings, screen sharing, remote viewers — or stop doing so. The
    /// local display is unaffected. The shell applies it to windows mapped
    /// later too, and drops it when this connection closes.
    SetCaptureProtection {
        protected: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        generation: Option<u64>,
    },
    /// One frame of the page this app's window shows, as a paint stream
    /// (`tilo_paint_stream::wire::Payload`, base64): the session shares it
    /// with web members as a display list they draw, instead of as video.
    /// `page` is where the page sits in the window, in logical pixels, and
    /// `scale` the window's buffer scale, so the session can place input.
    /// Sent only while the shell has asked for it ([`FloatingSurfaceHostMessage::PaintKeyframe`]).
    PaintStream {
        payload: String,
        page: [f64; 4],
        scale: f64,
    },
    /// This app can stream its window's page (sent once): the shell may ask
    /// it to with [`FloatingSurfaceHostMessage::PaintKeyframe`].
    PaintCapable,
    /// Open another instance of this app as a desktop tab right after this
    /// app's own tab (see [`crate::shell_tabs`]). `target` is a URI the app's
    /// manifest declares, or `None` for an empty instance. `instance` is the
    /// record instance the shell launched this process for; the shell only
    /// uses it to find a tab that already shows this same app.
    OpenTabBeside {
        target: Option<String>,
        background: bool,
        instance: Option<u64>,
    },
    /// Close this app's own tab through the desktop's tab lifecycle.
    CloseTab {
        instance: Option<u64>,
    },
    /// Reopen the workspace's most recently closed tab.
    ReopenClosedTab,
    /// Show the next (`forward`) or previous tab of the window group this
    /// app's tab is in: Ctrl+Tab or Ctrl+Shift+Tab that no widget of the app
    /// used. The shell steps only while that tab is the focused one.
    CycleTab {
        forward: bool,
        instance: Option<u64>,
    },
    /// The next window this app maps lands as a tab right after this one.
    PlaceNextWindowBeside {
        background: bool,
        instance: Option<u64>,
    },
    /// The slots this app shows in its window's bar
    /// ([`crate::window_toolbar`]), or none: sent whenever they change.
    /// `instance` is the record instance the shell launched this process
    /// for, which names the tab, as for [`Self::OpenTabBeside`].
    Toolbar {
        toolbar: Option<crate::window_toolbar::WindowToolbar>,
        instance: Option<u64>,
    },
    /// Turn this app's editable location in its window's bar into a field,
    /// as a click on it does (the app's Ctrl+L).
    EditLocation {
        instance: Option<u64>,
    },
    /// What this app's window offers its bar's own controls: what the
    /// search acts on in place and the open search's count, and the History
    /// control ([`crate::window_toolbar::WindowOffer`]). Sent whenever it
    /// changes; the bar answers with [`FloatingSurfaceHostMessage::ToolbarEvent`].
    WindowOffer {
        offer: crate::window_toolbar::WindowOffer,
        instance: Option<u64>,
    },
    /// Open the search in this app's window's bar, as a click on its control
    /// does (the app's Ctrl+F), or with its replacement field holding the
    /// keyboard (the app's Ctrl+H).
    OpenSearch {
        instance: Option<u64>,
        #[serde(default)]
        opening: crate::window_toolbar::SearchOpening,
    },
    /// Describe these changes of the resource whose History is open: the
    /// shell asks the model on this device and answers with
    /// [`FloatingSurfaceHostMessage::HistoryDescriptions`]. The app holds no
    /// model and needs none.
    DescribeHistory {
        changes: Vec<HistoryChangeFacts>,
    },
    /// This app answers [`FloatingSurfaceHostMessage::ShareSelection`] and
    /// [`FloatingSurfaceHostMessage::PointAt`] (sent once). An app that never
    /// says so is never asked.
    SelectionCapable,
    /// The person asked an agent about what they selected in this app's
    /// window (an "Tell an agent…" entry, or the shell's chord answered through
    /// [`FloatingSurfaceHostMessage::ShareSelection`]); `None` when nothing
    /// is selected. The shell opens its agent composer seeded with it. The
    /// app's identity is the launcher's, never the message's.
    AskAgent {
        selection: Option<crate::shared_selection::SharedSelection>,
        /// Where the selection is drawn in this window, in its logical
        /// coordinates (`[x0, y0, x1, y1]`), so the composer opens beside it
        /// rather than over it. Absent when the app does not know.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bounds: Option<[f64; 4]>,
    },
    /// The resource this window shows (the first it publishes), sent when it
    /// changes: the desktop's own menus for the window (its window menu)
    /// attach it, as the app's own menus do. `None` when it publishes none.
    WindowResource {
        resource: Option<tilo_actions::ActionResource>,
    },
    /// The answer to [`FloatingSurfaceHostMessage::PointAt`] `request`: the
    /// control at that point, `None` where there is none.
    Pointed {
        request: u64,
        control: Option<PointedControl>,
    },
    /// The answer to [`FloatingSurfaceHostMessage::Pick`] `request`, read
    /// from this window's retained tree ([`crate::pick_wire::answer`]).
    PickAnswered {
        request: u64,
        answer: crate::pick_wire::PickAnswer,
    },
    /// The record this window shows, as its route names it (empty for the
    /// app's own record of the workspace), and the kind of resource it
    /// keeps: the desktop lists the window among those a person may share
    /// ([`crate::window_share`]). Sent once per connection.
    WindowRecord {
        record: String,
        kind: String,
        instance: Option<u64>,
    },
    /// Where this window wants other apps' views of resources drawn, at most
    /// [`crate::window_share::MAX_EMBEDS`] (`crate::window_share::declare`):
    /// sent whenever they change. The shell answers with
    /// [`FloatingSurfaceHostMessage::EmbedStates`]. Only the sharing app's
    /// embeds are drawn.
    Embeds {
        embeds: Vec<crate::window_share::DeclaredEmbed>,
        instance: Option<u64>,
    },
    /// The windows this app shares (`crate::window_share::publish_shared`),
    /// sent whenever they change, so the desktop offers "Stop sharing" on
    /// them. Only the sharing app's are taken.
    SharedWindows {
        shared: Vec<crate::window_share::SharedWindow>,
    },
}

/// One control of an app's window as the shell names it when a person
/// points at it: its semantic id, its role and its accessible name. What a
/// field holds is never part of it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PointedControl {
    pub id: String,
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// Owner-only operations on Terminal's existing device-shell PTY.
/// Every input/read operation is bound to the exact currently open local session.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum DeviceShellOperation {
    Select,
    Inspect,
    Run { session: String, command: String },
    Read { session: String },
}

/// Sanitized host network hints. Unknown is distinct from known unmetered;
/// this carries no connection name, device, address or network permission.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostNetworkStatus {
    pub available: Option<bool>,
    pub metered: Option<bool>,
}

/// Shell → app lifecycle. The shell supplies one authoritative combined
/// desktop coordinate space and explicitly acknowledges ownership changes;
/// transports must not infer either from timing.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FloatingSurfaceHostMessage {
    /// The shell speaks shell-ui version `shell`, not the app's: its answer to
    /// a [`FloatingSurfaceClientMessage::Hello`] at another version, after
    /// which it closes the connection and updates whichever side is behind.
    /// Its shape never changes, so an app of any version reads it.
    VersionMismatch {
        shell: u16,
    },
    /// Admission/delivery result. The owner's published topology confirms application.
    DeviceDisplayMoveResult {
        request: u64,
        result: Result<(), String>,
    },
    /// The authenticated device owner selected Terminal's device-shell target.
    /// This requests its ordinary permission flow; it is not a grant.
    DeviceShellOperation {
        request: u64,
        operation: DeviceShellOperation,
    },
    /// Sent only after the compositor has installed the requested policy.
    CaptureProtectionApplied {
        generation: u64,
        protected: bool,
    },
    /// The trusted desktop's answer, after supervisor admission.
    HostShellAllowed {
        allowed: bool,
    },
    DesktopBounds {
        bounds: [f64; 4],
    },
    /// Someone started watching this app's window: send its page as a paint
    /// stream, starting from a whole frame.
    PaintKeyframe,
    /// Nobody watches the window any more: stop capturing the page.
    PaintStop,
    /// The person pressed the desktop's "ask an agent" chord over this
    /// app's focused window: answer with
    /// [`FloatingSurfaceClientMessage::AskAgent`] carrying the current
    /// selection. Sent only to an app that announced
    /// [`FloatingSurfaceClientMessage::SelectionCapable`].
    ShareSelection,
    /// The person pointed at `point` of this app's focused window, in its
    /// logical coordinates, to say what something is about (feedback on one
    /// control): answer with [`FloatingSurfaceClientMessage::Pointed`].
    /// Sent only to an app that announced
    /// [`FloatingSurfaceClientMessage::SelectionCapable`].
    PointAt {
        request: u64,
        point: [f64; 2],
    },
    /// The person is picking parts of the screen to report a problem with
    /// them, over this app's window: answer `query` with
    /// [`FloatingSurfaceClientMessage::PickAnswered`]. Sent only to an app
    /// that announced [`FloatingSurfaceClientMessage::SelectionCapable`].
    Pick {
        request: u64,
        query: crate::pick_wire::PickQuery,
    },
    /// The person used this app's slots in its window's bar
    /// ([`FloatingSurfaceClientMessage::Toolbar`]), or the bar's search or
    /// History control on its window ([`FloatingSurfaceClientMessage::WindowOffer`]).
    ToolbarEvent {
        event: crate::window_toolbar::ToolbarEvent,
    },
    /// What the changes of a [`FloatingSurfaceClientMessage::DescribeHistory`]
    /// did, as the model on this device described them. A change it could
    /// not describe is left out.
    HistoryDescriptions {
        described: Vec<HistoryChangeDescription>,
    },
    /// Authorises creation of one secondary toplevel. `token` is random and
    /// scoped to this authenticated connection; merely copying an app id can
    /// never gain overlay authority.
    PrepareSurface {
        id: String,
        token: String,
        rect: [f64; 4],
        content_rect: [f64; 4],
        /// True when the shell has already installed a frozen, pixel-exact
        /// transfer frame for this surface. The app can hide its in-window
        /// copy immediately in that case; if capture failed it keeps drawing
        /// until the native surface acknowledges its first buffer.
        transfer_covered: bool,
        /// App-local → desktop pointer-coordinate translation. Kept
        /// separate from clamped window geometry so a live drag can rebase
        /// without treating compositor clamping as pointer motion.
        coordinate_offset: [f64; 2],
    },
    Adopted {
        id: String,
        rect: [f64; 4],
    },
    /// The shell has hidden, but retained, the authenticated native surface.
    /// A later `Present` for the same connection/id resumes that exact
    /// surface instead of issuing a new capability or renderer.
    Suspended {
        id: String,
    },
    Reattach {
        id: String,
    },
    Closed {
        id: String,
    },
    /// Where and for whom the app's window is shown
    /// ([`crate::theme::WindowAppearance`]): the temperature of the
    /// workspace holding it and the person's colour. Sent when the app
    /// connects and again whenever either changes — a workspace switch, a
    /// new wallpaper, a window moved to another workspace, a colour chosen
    /// in Settings.
    Appearance {
        /// The host's configured palette; apps cannot read its settings file
        /// through their private sandbox configuration directory.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        theme: Option<crate::theme_document::ThemeDocument>,
        /// Absent on older hosts or launches without network access.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        network_status: Option<HostNetworkStatus>,
        /// OKLCH degrees; absent while the workspace has no temperature.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace_hue: Option<f32>,
        /// A presence hue by its stored name (`PresenceHue::id`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        your_colour: Option<String>,
        /// Whether the room the window is in has synced with its host
        /// ([`crate::room_sync`]); absent for a workspace that syncs with no
        /// host, and from older shells.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        room_sync: Option<crate::room_sync::RoomSync>,
        /// How round the shell draws the window's corners, in logical
        /// pixels: its window radius while the window is framed among
        /// others, 0 while it is drawn edge to edge. Absent from older
        /// shells.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        corner_radius: Option<f32>,
    },
    /// This shell draws the views windows embed of other apps' resources
    /// ([`FloatingSurfaceClientMessage::Embeds`]): sent when the app
    /// connects, so the app draws no stand-in for them.
    DrawsEmbeds,
    /// How each embed of this app's window is coming along, every one the
    /// shell draws: sent whenever one changes.
    EmbedStates {
        states: Vec<crate::window_share::KeyedEmbedState>,
    },
    /// The windows the person has open on a record, for the sharing app's
    /// picker: sent to the sharing app alone, whenever they change.
    OpenWindows {
        windows: Vec<crate::window_share::OpenWindow>,
    },
    /// A window's menu asked the sharing app to share it, or to stop
    /// sharing it.
    WindowShareAsk {
        ask: crate::window_share::Ask,
    },
}

impl FloatingSurfaceHostMessage {
    #[must_use]
    pub fn appearance(appearance: &crate::theme::WindowAppearance) -> Self {
        Self::Appearance {
            theme: None,
            network_status: None,
            workspace_hue: appearance.workspace_hue.filter(|hue| hue.is_finite()),
            your_colour: appearance.your_colour.map(|hue| hue.id().to_owned()),
            room_sync: None,
            corner_radius: appearance.corner_radius.filter(|radius| radius.is_finite()),
        }
    }

    /// The host's configured theme and the window's workspace appearance.
    #[must_use]
    pub fn appearance_with_theme(
        appearance: &crate::theme::WindowAppearance,
        theme: &crate::Theme,
    ) -> Self {
        let mut message = Self::appearance(appearance);
        if let Self::Appearance {
            theme: document, ..
        } = &mut message
        {
            *document = Some(theme.document.clone());
        }
        message
    }

    /// Validate the host's authored palette before applying it.
    #[must_use]
    pub fn configured_theme(&self) -> Option<crate::Theme> {
        let Self::Appearance {
            theme: Some(document),
            ..
        } = self
        else {
            return None;
        };
        crate::Theme::from_document(document)
    }

    /// What an [`Self::Appearance`] message says, `None` for any other. A
    /// colour this build does not name, or a hue that is not a number,
    /// counts as unsaid.
    #[must_use]
    pub fn window_appearance(&self) -> Option<crate::theme::WindowAppearance> {
        let Self::Appearance {
            workspace_hue,
            your_colour,
            corner_radius,
            ..
        } = self
        else {
            return None;
        };
        Some(crate::theme::WindowAppearance {
            corner_radius: corner_radius.filter(|radius| radius.is_finite() && *radius >= 0.0),
            workspace_hue: workspace_hue
                .filter(|hue| hue.is_finite())
                .map(|hue| hue.rem_euclid(360.0)),
            your_colour: your_colour
                .as_deref()
                .and_then(crate::tokens::PresenceHue::from_id),
        })
    }
}

/// Non-blocking app-side connection to the session shell. The launcher hands
/// the already-connected socket into the sandbox as
/// `TILO_SVC_SHELL-UI_FD`; newline-delimited JSON keeps the tiny lifecycle
/// protocol inspectable and lets the compositor service evolve independently
/// of app/widget releases.
#[cfg(unix)]
pub struct FloatingSurfaceServiceClient {
    stream: UnixStream,
    incoming: Vec<u8>,
    outgoing: VecDeque<Vec<u8>>,
    outgoing_offset: usize,
    outgoing_bytes: usize,
    /// Tells the readable watcher ([`Self::from_stream_waking`]) the app
    /// has read; dropping it stops the watcher.
    #[cfg(any(test, feature = "local-daemons"))]
    rearm_wake: Option<std::sync::mpsc::Sender<()>>,
    /// The last appearance the shell told: the shell says it once per
    /// change, so a window this connection is handed to next
    /// ([`Self::hand_to_next_window`]) hears it from here.
    appearance: Option<FloatingSurfaceHostMessage>,
    /// Messages the next [`Self::pump`] answers before reading.
    replay: Vec<FloatingSurfaceHostMessage>,
}

#[cfg(all(unix, feature = "local-daemons"))]
thread_local! {
    /// The connection of a window that gave its place to the next one,
    /// until that window takes it ([`FloatingSurfaceServiceClient::for_window`]).
    static HANDED_OVER: std::cell::RefCell<Option<FloatingSurfaceServiceClient>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(unix)]
impl FloatingSurfaceServiceClient {
    /// Discover the optional session service. Outside Tilo (or on an older
    /// shell) this returns `None`; apps retain their ordinary attached UI.
    /// The inherited descriptor belongs to one app launch. Transport loss is
    /// terminal for this client; only a fresh launcher-mediated launch supplies
    /// a new connection. Discovery never opens a privileged host socket.
    #[cfg(feature = "local-daemons")]
    pub fn discover() -> Option<Self> {
        let fd = runtime_portal::service_fd(FLOATING_SURFACE_SERVICE)?;
        // The inherited service descriptor belongs to this one client. Lift
        // it exactly once; dropping the client closes it cleanly.
        let owned = unsafe { OwnedFd::from_raw_fd(fd) };
        // The shell writes unprompted (a workspace switch changes the
        // window's appearance), so an idle app must wake to read it.
        let wake = crate::wake::WakeHandle::new();
        Self::from_stream_waking(UnixStream::from(owned), move || wake.wake()).ok()
    }

    /// The connection a new window takes: the one the window before it
    /// handed over ([`Self::hand_over`]), else the one the launcher handed
    /// this process ([`Self::discover`]).
    pub(crate) fn for_window() -> Option<Self> {
        // A window the shell opened in this running process has a
        // connection of its own (`crate::window_door`).
        #[cfg(feature = "local-daemons")]
        if crate::window_door::is_building() {
            let connection = crate::window_door::take_shell_ui()?;
            let wake = crate::wake::WakeHandle::new();
            return Self::from_stream_waking(UnixStream::from(connection), move || wake.wake())
                .ok();
        }
        #[cfg(feature = "local-daemons")]
        if let Some(mut client) = HANDED_OVER.with(|slot| slot.borrow_mut().take()) {
            client.hand_to_next_window();
            return Some(client);
        }
        Self::discover()
    }

    /// Keep this connection for the next window the process opens, instead
    /// of closing it with this one: the launcher hands each process one
    /// connection, and a window that gives its place to the next
    /// (`crate::workspace_identity`) gives it its connection too.
    #[cfg(feature = "local-daemons")]
    #[cfg_attr(
        not(feature = "native"),
        expect(dead_code, reason = "only a native window gives its place to the next")
    )]
    pub(crate) fn hand_over(self) {
        HANDED_OVER.with(|slot| *slot.borrow_mut() = Some(self));
    }

    /// Hand this connection to the next window of the process: the window a
    /// launch opens in place of one that waited for its workspace's identity
    /// (`crate::workspace_identity`). The connection, and the version it
    /// said, carry over; the shell's unprompted messages wake the window in
    /// front from now on, and it hears the appearance the last one was told.
    #[cfg(feature = "local-daemons")]
    fn hand_to_next_window(&mut self) {
        let wake = crate::wake::WakeHandle::new();
        match spawn_readable_watcher(&self.stream, move || wake.wake(), || {}) {
            Ok(rearm) => self.rearm_wake = Some(rearm),
            Err(error) => {
                tracing::warn!(%error, "the next window does not wake for the shell's messages");
            }
        }
        self.replay.extend(self.appearance.clone());
    }

    /// [`Self::from_stream`], calling `wake` whenever the shell has written
    /// something the app has not read yet: one wake, then nothing until the
    /// app has read ([`Self::pump`] rearms it), so unread bytes never spin
    /// it. Dropping the client stops the watcher within one poll interval.
    #[cfg(any(test, feature = "local-daemons"))]
    fn from_stream_waking(
        stream: UnixStream,
        wake: impl Fn() + Send + 'static,
    ) -> std::io::Result<Self> {
        Self::from_stream_watched(stream, wake, || {})
    }

    /// [`Self::from_stream_waking`], also calling `parked` each time the
    /// watcher, having woken the app, waits for it to read.
    #[cfg(any(test, feature = "local-daemons"))]
    fn from_stream_watched(
        stream: UnixStream,
        wake: impl Fn() + Send + 'static,
        parked: impl Fn() + Send + 'static,
    ) -> std::io::Result<Self> {
        let rearm = spawn_readable_watcher(&stream, wake, parked)?;
        let mut client = Self::from_stream(stream)?;
        client.rearm_wake = Some(rearm);
        Ok(client)
    }

    /// Without the local daemons there is no inherited session service.
    #[cfg(not(feature = "local-daemons"))]
    pub fn discover() -> Option<Self> {
        None
    }

    /// Construct an explicit fixture connection through the production stream adapter.
    #[cfg(feature = "test-support")]
    pub fn from_test_stream(stream: UnixStream) -> std::io::Result<Self> {
        Self::from_stream(stream)
    }

    /// The client of `stream`, which says the app's shell-ui version
    /// ([`FloatingSurfaceClientMessage::Hello`]) before anything else.
    #[cfg(any(test, feature = "local-daemons", feature = "test-support"))]
    pub(crate) fn from_stream(stream: UnixStream) -> std::io::Result<Self> {
        stream.set_nonblocking(true)?;
        let mut client = Self {
            stream,
            incoming: Vec::new(),
            outgoing: VecDeque::new(),
            outgoing_offset: 0,
            outgoing_bytes: 0,
            #[cfg(any(test, feature = "local-daemons"))]
            rearm_wake: None,
            appearance: None,
            replay: Vec::new(),
        };
        client
            .send(&FloatingSurfaceClientMessage::Hello {
                version: FLOATING_SURFACE_PROTOCOL_VERSION,
            })
            .map_err(std::io::Error::other)?;
        Ok(client)
    }

    fn enqueue_frame(&mut self, frame: Vec<u8>) -> Result<(), String> {
        let queued = self
            .outgoing_bytes
            .checked_add(frame.len())
            .ok_or_else(|| "floating-surface outgoing queue size overflow".to_string())?;
        if queued > MAX_OUTGOING_QUEUE_BYTES {
            return Err("floating-surface outgoing queue exceeds 4 MiB".into());
        }
        self.outgoing_bytes = queued;
        self.outgoing.push_back(frame);
        Ok(())
    }

    /// Queue and flush one client message.
    ///
    /// # Errors
    ///
    /// Returns an error when the message cannot be encoded, exceeds the
    /// protocol limit, or cannot be written to the service stream.
    pub fn send(&mut self, message: &FloatingSurfaceClientMessage) -> Result<(), String> {
        let mut frame = serde_json::to_vec(message)
            .map_err(|error| format!("encode floating-surface message: {error}"))?;
        if frame.len() > MAX_PROTOCOL_FRAME_BYTES {
            return Err("floating-surface message exceeds 1 MiB".into());
        }
        frame.push(b'\n');
        self.enqueue_frame(frame)?;
        self.flush_outgoing()
            .map_err(|error| format!("send floating-surface message: {error}"))
    }

    /// Flush queued writes and collect every complete host message currently
    /// available. `WouldBlock` is ordinary idle state, never a disconnect.
    pub fn pump(&mut self) -> Result<Vec<FloatingSurfaceHostMessage>, String> {
        self.flush_outgoing()
            .map_err(|error| format!("send floating-surface message: {error}"))?;
        let mut chunk = [0_u8; 4096];
        loop {
            match self.stream.read(&mut chunk) {
                Ok(0) => return Err("floating-surface service disconnected".into()),
                Ok(count) => {
                    let bytes = chunk
                        .get(..count)
                        .ok_or_else(|| "floating-surface read exceeded its buffer".to_string())?;
                    self.incoming.extend_from_slice(bytes);
                    if self.incoming.len() > MAX_PROTOCOL_FRAME_BYTES {
                        return Err("floating-surface host message exceeds 1 MiB".into());
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) => return Err(format!("read floating-surface service: {error}")),
            }
        }
        #[cfg(any(test, feature = "local-daemons"))]
        if let Some(rearm) = &self.rearm_wake {
            let _ = rearm.send(());
        }

        let mut messages = std::mem::take(&mut self.replay);
        while let Some(newline) = self.incoming.iter().position(|byte| *byte == b'\n') {
            let frame: Vec<u8> = self.incoming.drain(..=newline).collect();
            let payload = frame.get(..newline).unwrap_or_default();
            if payload.is_empty() {
                continue;
            }
            let message = serde_json::from_slice(payload)
                .map_err(|error| format!("decode floating-surface host message: {error}"))?;
            if let FloatingSurfaceHostMessage::VersionMismatch { shell } = message {
                return Err(format!(
                    "the shell speaks shell-ui version {shell}, this app \
                     {FLOATING_SURFACE_PROTOCOL_VERSION}; the shell updates the one behind"
                ));
            }
            if matches!(message, FloatingSurfaceHostMessage::Appearance { .. }) {
                self.appearance = Some(message.clone());
            }
            messages.push(message);
        }
        Ok(messages)
    }

    fn flush_outgoing(&mut self) -> std::io::Result<()> {
        while let Some(frame) = self.outgoing.front() {
            let remaining = frame.get(self.outgoing_offset..).unwrap_or_default();
            match self.stream.write(remaining) {
                Ok(0) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::WriteZero,
                        "floating-surface service accepted zero bytes",
                    ));
                }
                Ok(count) => {
                    self.outgoing_offset = self.outgoing_offset.saturating_add(count);
                    if self.outgoing_offset >= frame.len() {
                        self.outgoing_bytes = self.outgoing_bytes.saturating_sub(frame.len());
                        self.outgoing.pop_front();
                        self.outgoing_offset = 0;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
}

/// Watch `stream` for what the shell writes, calling `wake` once each time
/// it has written something the app has not read yet; the app rearms the
/// watch through the returned sender after reading, and dropping it stops
/// the watcher within one poll interval. `parked` is called each time the
/// watcher, having woken the app, starts waiting for that rearm.
#[cfg(all(unix, any(test, feature = "local-daemons")))]
fn spawn_readable_watcher(
    stream: &UnixStream,
    wake: impl Fn() + Send + 'static,
    parked: impl Fn() + Send + 'static,
) -> std::io::Result<std::sync::mpsc::Sender<()>> {
    use std::os::fd::AsRawFd as _;
    use std::sync::mpsc::{RecvTimeoutError, TryRecvError};
    const POLL: std::time::Duration = std::time::Duration::from_millis(500);
    // Its own descriptor for the same socket: closing the client's never
    // leaves the watcher polling a number the process reused.
    let watched = stream.try_clone()?;
    let (rearm, armed) = std::sync::mpsc::channel::<()>();
    tilo_rt::spawn_thread("tilo-shell-ui-wake", move || {
        loop {
            let mut readable = libc::pollfd {
                fd: watched.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let timeout = libc::c_int::try_from(POLL.as_millis()).unwrap_or(500);
            // SAFETY: one valid pollfd for a descriptor this thread owns.
            let ready = unsafe { libc::poll(&raw mut readable, 1, timeout) };
            if ready < 0 {
                if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return;
            }
            if ready == 0 {
                // Idle: stop once the client is gone. A rearm that
                // arrives while armed means nothing.
                match armed.try_recv() {
                    Err(TryRecvError::Disconnected) => return,
                    Ok(()) | Err(TryRecvError::Empty) => continue,
                }
            }
            wake();
            if readable.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
                return;
            }
            parked();
            loop {
                match armed.recv_timeout(POLL) {
                    Ok(()) => break,
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            }
            while armed.try_recv().is_ok() {}
        }
    })?;
    Ok(rearm)
}

/// Non-Unix hosts cannot receive Tilo session-service file descriptors. Keep
/// the optional client API available so applications compile and naturally
/// retain their attached in-window surface.
#[cfg(not(unix))]
pub struct FloatingSurfaceServiceClient;

#[cfg(not(unix))]
impl FloatingSurfaceServiceClient {
    pub fn discover() -> Option<Self> {
        None
    }

    pub(crate) fn for_window() -> Option<Self> {
        None
    }

    /// No connection exists to keep for the next window.
    #[cfg_attr(
        not(feature = "native"),
        expect(dead_code, reason = "only a native window gives its place to the next")
    )]
    pub(crate) fn hand_over(self) {}

    /// # Errors
    /// Always returns an error because this host has no session-service transport.
    pub fn send(&mut self, _message: &FloatingSurfaceClientMessage) -> Result<(), String> {
        Err("floating-surface service is unavailable on this platform".into())
    }

    /// # Errors
    /// Always returns an error because this host has no session-service transport.
    pub fn pump(&mut self) -> Result<Vec<FloatingSurfaceHostMessage>, String> {
        Err("floating-surface service is unavailable on this platform".into())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct FloatingSurfaceRequest {
    pub id: String,
    pub title: String,
    pub role: FloatingRole,
    pub source: FloatingSource,
    pub preferred_size: Size,
    pub aspect_ratio: Option<f64>,
    pub initial_rect: Option<Rect>,
    pub presentation_rect: Option<Rect>,
    /// Shell-internal source binding retained in app-local coordinates. It is
    /// deliberately absent from the wire representation: the authenticated
    /// compositor installs it while translating an external request, then
    /// re-resolves it against the app's live surface on every mirror frame.
    pub tracking_source: Option<FloatingSource>,
}

/// App-owned presentation currently living in desktop coordinates. The UI
/// runtime uses this one read-only description both to render the secondary
/// window and to stream placement changes to the compositor.
#[derive(Clone, Debug, PartialEq)]
pub struct FloatingSurfacePresentation {
    pub id: String,
    pub content_rect: Rect,
    pub window_rect: Rect,
    /// Includes interactive chrome, not just media; excludes shadow.
    /// `None` preserves legacy whole-window hit testing.
    pub input_rect: Option<Rect>,
    /// The pointer crossed back into the parent app, but the shell still owns
    /// the visible pixels until it acknowledges suspension. Keeping this
    /// presentation alive for that interval supplies the exact final geometry
    /// and prevents app + proxy from painting simultaneously.
    pub returning_to_app: bool,
}

impl From<&FloatingSurfaceRequest> for FloatingSurfaceProtocolRequest {
    fn from(request: &FloatingSurfaceRequest) -> Self {
        Self {
            id: request.id.clone(),
            title: request.title.clone(),
            role: request.role.into(),
            source: (&request.source).into(),
            preferred_size: [request.preferred_size.width, request.preferred_size.height],
            aspect_ratio: request.aspect_ratio,
            initial_rect: request.initial_rect.map(protocol_rect_array),
            presentation_rect: request.presentation_rect.map(protocol_rect_array),
        }
    }
}

impl TryFrom<FloatingSurfaceProtocolRequest> for FloatingSurfaceRequest {
    type Error = String;

    fn try_from(request: FloatingSurfaceProtocolRequest) -> Result<Self, Self::Error> {
        if request.id.trim().is_empty() {
            return Err("floating-surface id is empty".into());
        }
        let [width, height] = request.preferred_size;
        if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
            return Err("floating-surface preferred size must be finite and positive".into());
        }
        if request
            .aspect_ratio
            .is_some_and(|ratio| !ratio.is_finite() || ratio <= 0.0)
        {
            return Err("floating-surface aspect ratio must be finite and positive".into());
        }
        Ok(Self {
            id: request.id,
            title: request.title,
            role: request.role.into(),
            source: request.source.try_into()?,
            preferred_size: Size::new(width, height),
            aspect_ratio: request.aspect_ratio,
            initial_rect: request.initial_rect.map(protocol_rect).transpose()?,
            presentation_rect: request.presentation_rect.map(protocol_rect).transpose()?,
            tracking_source: None,
        })
    }
}

pub fn protocol_rect_array(rect: Rect) -> [f64; 4] {
    [rect.x0, rect.y0, rect.x1, rect.y1]
}

impl From<FloatingRole> for FloatingSurfaceProtocolRole {
    fn from(role: FloatingRole) -> Self {
        match role {
            FloatingRole::PictureInPicture => Self::PictureInPicture,
            FloatingRole::ActiveCall => Self::ActiveCall,
            FloatingRole::Navigation => Self::Navigation,
            FloatingRole::LiveActivity => Self::LiveActivity,
            FloatingRole::Conversation => Self::Conversation,
        }
    }
}

impl From<FloatingSurfaceProtocolRole> for FloatingRole {
    fn from(role: FloatingSurfaceProtocolRole) -> Self {
        match role {
            FloatingSurfaceProtocolRole::PictureInPicture => Self::PictureInPicture,
            FloatingSurfaceProtocolRole::ActiveCall => Self::ActiveCall,
            FloatingSurfaceProtocolRole::Navigation => Self::Navigation,
            FloatingSurfaceProtocolRole::LiveActivity => Self::LiveActivity,
            FloatingSurfaceProtocolRole::Conversation => Self::Conversation,
        }
    }
}

impl From<&FloatingSource> for FloatingSurfaceProtocolSource {
    fn from(source: &FloatingSource) -> Self {
        match source {
            FloatingSource::AppElement {
                app_id,
                element_id,
                region,
            } => Self::AppElement {
                app_id: app_id.clone(),
                element_id: element_id.clone(),
                region: protocol_rect_array(*region),
            },
            FloatingSource::AppRegion { app_id, region } => Self::AppRegion {
                app_id: app_id.clone(),
                region: [region.x0, region.y0, region.x1, region.y1],
            },
            FloatingSource::Surface { surface_id } => Self::Surface {
                surface_id: *surface_id,
            },
            FloatingSource::Composite(sources) => Self::Composite {
                sources: sources.iter().map(Self::from).collect(),
            },
        }
    }
}

impl TryFrom<FloatingSurfaceProtocolSource> for FloatingSource {
    type Error = String;

    fn try_from(source: FloatingSurfaceProtocolSource) -> Result<Self, Self::Error> {
        Ok(match source {
            FloatingSurfaceProtocolSource::AppElement {
                app_id,
                element_id,
                region,
            } => {
                if app_id.trim().is_empty() || element_id.trim().is_empty() {
                    return Err("floating app-element source ids must not be empty".into());
                }
                Self::AppElement {
                    app_id,
                    element_id,
                    region: protocol_rect(region)?,
                }
            }
            FloatingSurfaceProtocolSource::AppRegion { app_id, region } => {
                if app_id.trim().is_empty() {
                    return Err("floating app-region source app id is empty".into());
                }
                Self::AppRegion {
                    app_id,
                    region: protocol_rect(region)?,
                }
            }
            FloatingSurfaceProtocolSource::Surface { surface_id } => Self::Surface { surface_id },
            FloatingSurfaceProtocolSource::Composite { sources } => {
                if sources.is_empty() {
                    return Err("floating composite source is empty".into());
                }
                Self::Composite(
                    sources
                        .into_iter()
                        .map(Self::try_from)
                        .collect::<Result<Vec<_>, _>>()?,
                )
            }
        })
    }
}

/// Convert protocol coordinates into a validated rectangle.
pub fn protocol_rect(coords: [f64; 4]) -> Result<Rect, String> {
    let [x0, y0, x1, y1] = coords;
    if coords.iter().any(|value| !value.is_finite()) || x1 <= x0 || y1 <= y0 {
        return Err("floating-surface rect must be finite with positive area".into());
    }
    Ok(Rect::new(x0, y0, x1, y1))
}

/// Validate optional interactive geometry without enlarging the native window.
pub fn protocol_input_rect(window: Rect, coords: Option<[f64; 4]>) -> Result<Option<Rect>, String> {
    coords
        .map(|coords| {
            let rect = protocol_rect(coords)?;
            if !rect.width().is_finite()
                || !rect.height().is_finite()
                || rect.intersect(window) != rect
            {
                return Err("floating input rect must be contained in its window".into());
            }
            Ok(rect)
        })
        .transpose()
}

/// Move a declared interactive region with its host geometry.
#[must_use]
pub fn translate_input_rect(input: Option<Rect>, from: Point, to: Point) -> Option<Rect> {
    input.map(|rect| rect + (to - from))
}

/// Where the floating surface currently belongs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Attachment {
    /// Moves with its application. `offset` is from the app's top-right
    /// floating inset, so resizing/moving the app retains the relationship.
    App { offset: Vec2 },
    /// Independent desktop coordinates, owned by shell chrome.
    Desktop { rect: Rect },
}

/// Result of updating a drag. Hosts use the edge to animate the hand-off.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handoff {
    None,
    Detached,
    Reattached,
}

#[derive(Clone, Copy, Debug)]
struct Drag {
    pointer_origin: Point,
    rect_origin: Rect,
    last_pointer: Point,
    last_sample: crate::time::Instant,
    /// Low-pass pointer velocity in logical pixels/second. Presentation uses
    /// it only for the tiny handoff deformation; placement remains exactly
    /// under the pointer and never acquires imprecise inertial drift.
    velocity: Vec2,
}

#[derive(Clone, Copy, Debug)]
struct HandoffMotion {
    started: crate::time::Instant,
    axis: Vec2,
    strength: f64,
}

#[derive(Clone, Debug)]
struct SettleMotion {
    /// Armed by the first geometry sample, not by the ownership event. A
    /// delayed renderer must still present the exact transfer rect once
    /// before easing toward its destination.
    started: std::cell::Cell<Option<crate::time::Instant>>,
    from: Rect,
    to: Rect,
}

/// Placement + drag state shared by call, navigation, browser `PiP` and
/// arbitrary captured regions.
#[derive(Clone, Debug)]
pub struct FloatingPlacement {
    attachment: Attachment,
    /// The shell has presented the app-owned secondary surface at least once.
    /// Until this flips, the app may keep painting its clipped in-window copy
    /// so a detach never contains a transparent handoff frame.
    desktop_adopted: bool,
    /// The shell is already drawing either the frozen transfer frame or the
    /// live native surface. Kept separate from `desktop_adopted` so a valid
    /// proxy can remove the clipped app copy without pretending the client
    /// window has committed yet.
    desktop_covered: bool,
    /// Shell → app is a handshake, not an instantaneous enum flip. While
    /// true, app-local drawing stays suppressed and the runtime publishes one
    /// final geometry before requesting suspension.
    return_pending: bool,
    /// App-local rect published by the exact boundary-crossing event. A
    /// shell Prepare reply refers to this snapshot; later pointer motion must
    /// be carried forward, not mistaken for compositor clamping.
    handoff_rect: Option<Rect>,
    drag: Option<Drag>,
    preferred_size: Size,
    /// Inline app elements may fill their ordinary layout region before
    /// detaching; activity overlays leave this unset and use the inset home.
    attached_region: Option<Rect>,
    /// Visual chrome surrounding the content rect while it belongs to an app.
    /// Keeping this here makes the *complete* soft surface stay inside its
    /// parent, while desktop geometry can continue to identify the stable
    /// content region used by the native-surface protocol.
    attached_chrome: [f64; 4],
    /// Bands of the app the surface keeps clear of while attached (the
    /// window's header row), as `[left, top, right, bottom]`.
    safe_insets: [f64; 4],
    /// Translation between the app's local widget space and the shell's
    /// desktop space while a native surface owns presentation.
    desktop_coordinate_offset: Vec2,
    handoff_motion: Option<HandoffMotion>,
    /// Exact render-only handoff phase used by deterministic visual review.
    /// Production interaction always leaves this unset and follows the clock.
    handoff_progress_override: Option<f64>,
    settle_motion: Option<SettleMotion>,
    /// Reduced motion: a return lands home at once and the ownership
    /// flourish is not played ([`Self::set_reduce_motion`]).
    reduce_motion: bool,
}

impl FloatingPlacement {
    /// Whether an attached surface has crossed the shared detach boundary.
    ///
    /// Hosts can use this before constructing a retained surface so the
    /// pre-creation gesture follows the same threshold as [`Self::update_drag`].
    pub fn is_beyond_detach_boundary(pointer: Point, app: Rect) -> bool {
        !app.inflate(28.0, 28.0).contains(pointer)
    }

    pub fn attached(preferred_size: Size) -> Self {
        Self {
            attachment: Attachment::App { offset: Vec2::ZERO },
            desktop_adopted: false,
            desktop_covered: false,
            return_pending: false,
            handoff_rect: None,
            drag: None,
            preferred_size,
            attached_region: None,
            attached_chrome: [0.0; 4],
            safe_insets: [0.0; 4],
            desktop_coordinate_offset: Vec2::ZERO,
            handoff_motion: None,
            handoff_progress_override: None,
            settle_motion: None,
            reduce_motion: false,
        }
    }

    /// Apply the person's reduced-motion setting (`Theme::motion`). The
    /// owner forwards it from `Widget::set_motion_reduced`; under it a
    /// return to the app lands home at once and any motion in flight
    /// settles now.
    pub fn set_reduce_motion(&mut self, reduced: bool) {
        self.reduce_motion = reduced;
        if reduced {
            self.settle_motion = None;
            self.handoff_motion = None;
        }
    }

    pub fn attachment(&self) -> Attachment {
        self.attachment
    }

    pub fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    pub fn is_desktop_adopted(&self) -> bool {
        self.desktop_adopted
    }

    pub fn is_desktop_covered(&self) -> bool {
        self.desktop_covered
    }

    pub fn is_return_pending(&self) -> bool {
        self.return_pending
    }

    /// Ask the host to return an adopted surface without painting a second
    /// source copy before its `Suspended` acknowledgement.
    pub fn request_app_return(&mut self) {
        if self.desktop_adopted && matches!(self.attachment, Attachment::Desktop { .. }) {
            self.return_pending = true;
            self.drag = None;
        }
    }

    pub fn set_preferred_size(&mut self, size: Size) {
        self.preferred_size = size;
    }

    /// Bind placement to the exact current app-local region of an inline
    /// element. This is lifecycle geometry, not an independent layout system.
    pub fn set_attached_region(&mut self, region: Option<Rect>) {
        match region {
            None => self.attached_region = None,
            Some(rect) if protocol_rect([rect.x0, rect.y0, rect.x1, rect.y1]).is_ok() => {
                self.attached_region = Some(rect);
            }
            Some(_) => {}
        }
    }

    /// Declare app-owned decoration around the placement's content rect as
    /// `[left, top, right, bottom]` logical pixels.
    pub fn set_attached_chrome(&mut self, chrome: [f64; 4]) {
        self.attached_chrome = chrome.map(|value| value.max(0.0));
    }

    /// Keep the attached surface clear of bands of the app as `[left, top,
    /// right, bottom]` logical pixels: its home sits inside them and a drag
    /// inside the app does not slide it under them. A host with a header
    /// row passes `[0.0, header_height, 0.0, 0.0]`. Separate from
    /// [`Self::set_attached_chrome`], which is the surface's own decoration.
    pub fn set_safe_insets(&mut self, insets: [f64; 4]) {
        self.safe_insets = insets.map(|value| value.max(0.0));
    }

    /// The part of `app` an attached surface may stand in.
    fn safe_area(&self, app: Rect) -> Rect {
        let [left, top, right, bottom] = self.safe_insets;
        let x0 = (app.x0 + left).min(app.x1);
        let y0 = (app.y0 + top).min(app.y1);
        Rect::new(x0, y0, (app.x1 - right).max(x0), (app.y1 - bottom).max(y0))
    }

    /// Restore only app-local presentation; never restore desktop surface authority.
    pub fn restore_local_offset(&mut self, offset: Vec2) {
        self.reset_attached();
        if offset.x.is_finite() && offset.y.is_finite() {
            self.attachment = Attachment::App { offset };
        }
    }

    pub fn reset_attached(&mut self) {
        self.attachment = Attachment::App { offset: Vec2::ZERO };
        self.desktop_adopted = false;
        self.desktop_covered = false;
        self.return_pending = false;
        self.handoff_rect = None;
        self.drag = None;
        self.handoff_motion = None;
        self.handoff_progress_override = None;
        self.settle_motion = None;
        self.desktop_coordinate_offset = Vec2::ZERO;
    }

    /// Host-declined/programmatic shell → app return. The surface keeps its
    /// current desktop rect on the first frame and eases back to the app home,
    /// so an unavailable native-surface path never makes it vanish or jump.
    pub fn reattach_to_app(&mut self, app: Rect, desktop: Rect) {
        let from = self.rect(app, desktop) - self.desktop_coordinate_offset;
        let home = self.attached_home_rect(app);
        self.attachment = Attachment::App { offset: Vec2::ZERO };
        self.desktop_adopted = false;
        self.desktop_covered = false;
        self.return_pending = false;
        self.handoff_rect = None;
        self.drag = None;
        self.desktop_coordinate_offset = Vec2::ZERO;
        self.handoff_progress_override = None;
        if self.reduce_motion {
            self.settle_motion = None;
            self.handoff_motion = None;
            return;
        }
        let started = crate::time::now();
        self.settle_motion = (from != home).then_some(SettleMotion {
            started: std::cell::Cell::new(None),
            from,
            to: home,
        });
        self.handoff_motion = Some(HandoffMotion {
            started,
            axis: home.center() - from.center(),
            strength: 0.55,
        });
    }

    /// Programmatic counterpart to crossing the app edge (for a “Float” menu
    /// action). The supplied rect is already in desktop coordinates.
    pub fn detach_to_desktop(&mut self, rect: Rect) {
        self.attachment = Attachment::Desktop { rect };
        self.desktop_adopted = false;
        self.desktop_covered = false;
        self.return_pending = false;
        self.handoff_rect = None;
        self.drag = None;
        self.settle_motion = None;
        self.handoff_progress_override = None;
        self.handoff_motion = Some(HandoffMotion {
            started: crate::time::now(),
            axis: Vec2::new(1.0, 0.0),
            strength: 0.55,
        });
    }

    /// Apply the compositor's clamped transfer geometry before the native
    /// surface's first draw, without claiming that pixels are visible yet.
    pub fn prepare_desktop_rect(&mut self, rect: Rect) {
        let inferred = match self.attachment {
            Attachment::Desktop { rect: previous } => rect.origin() - previous.origin(),
            Attachment::App { .. } => Vec2::ZERO,
        };
        self.prepare_desktop_rect_with_offset(rect, inferred);
    }

    /// Prepare desktop geometry while rebasing a still-held pointer grab from
    /// app-local into desktop coordinates. The correction term keeps the
    /// prepared (possibly compositor-clamped) rect exact on this frame and
    /// on the first native pointer event.
    pub fn prepare_desktop_rect_with_offset(&mut self, rect: Rect, coordinate_offset: Vec2) {
        let returning_to_app = self.return_pending;
        let previous = match self.attachment {
            Attachment::Desktop { rect } => Some(rect),
            Attachment::App { .. } => None,
        };
        if let (Some(previous), Some(drag)) = (previous, self.drag.as_mut()) {
            let referenced = self.handoff_rect.unwrap_or(previous);
            let clamp_correction = rect.origin() - (referenced + coordinate_offset).origin();
            drag.pointer_origin += coordinate_offset;
            drag.last_pointer += coordinate_offset;
            drag.rect_origin = drag.rect_origin + coordinate_offset + clamp_correction;
            self.attachment = Attachment::Desktop {
                rect: previous + coordinate_offset + clamp_correction,
            };
        } else {
            self.attachment = Attachment::Desktop { rect };
        }
        self.handoff_rect = None;
        self.return_pending = returning_to_app;
        self.desktop_coordinate_offset = coordinate_offset;
        self.settle_motion = None;
    }

    /// Tell placement whether the shell can already cover the old app-local
    /// pixels during native-surface startup.
    pub fn set_desktop_transfer_covered(&mut self, covered: bool) {
        self.desktop_covered = covered;
    }

    /// Confirm that the native surface has committed a visible buffer. This
    /// deliberately does not replay the acknowledgement's geometry: while
    /// the surface was preparing, pointer motion may already have advanced
    /// beyond that earlier rectangle.
    pub fn confirm_desktop_adoption(&mut self) {
        if matches!(self.attachment, Attachment::Desktop { .. }) {
            self.desktop_adopted = true;
            self.desktop_covered = true;
        }
    }

    /// Complete the shell → app half of the ownership handshake. The shell
    /// has hidden the native/proxy surface, so the attached copy may now be
    /// included in the next primary frame.
    pub fn confirm_app_adoption(&mut self, app: Rect) {
        let Attachment::Desktop { rect } = self.attachment else {
            return;
        };
        let local_rect = rect - self.desktop_coordinate_offset;
        let home = self.attached_home_rect(app);
        self.attachment = Attachment::App {
            offset: local_rect.origin() - home.origin(),
        };
        if let Some(drag) = self.drag.as_mut() {
            drag.pointer_origin -= self.desktop_coordinate_offset;
            drag.last_pointer -= self.desktop_coordinate_offset;
            drag.rect_origin = drag.rect_origin - self.desktop_coordinate_offset;
        }
        self.desktop_coordinate_offset = Vec2::ZERO;
        self.return_pending = false;
        self.desktop_adopted = false;
        self.desktop_covered = false;
    }

    pub fn is_handoff_animating(&self) -> bool {
        if self.reduce_motion {
            return false;
        }
        (self.handoff_progress_override.is_none()
            && self
                .handoff_motion
                .is_some_and(|motion| motion.started.elapsed() < settle_time()))
            || self.settle_motion.as_ref().is_some_and(|motion| {
                motion
                    .started
                    .get()
                    .is_none_or(|started| started.elapsed() < settle_time())
            })
    }

    /// Pin the subtle ownership-transfer deformation to an exact phase for
    /// screenshot review. This does not alter geometry, hit testing, or the
    /// production animation timeline.
    #[doc(hidden)]
    pub fn pin_handoff_for_visual_review(&mut self, permille: u16) {
        if self.handoff_motion.is_some() {
            self.handoff_progress_override = Some(f64::from(permille.min(1_000)) / 1_000.0);
        }
    }

    /// A tiny, render-only stretch along the drag direction when ownership
    /// changes. Geometry and hit testing stay on the exact drag rect, so the
    /// handoff cannot jump; reduced-motion disables the flourish entirely.
    pub fn presentation_transform(
        &self,
        transform: Affine,
        rect: Rect,
        reduce_motion: bool,
    ) -> Affine {
        let Some(motion) = self.handoff_motion else {
            return transform;
        };
        if reduce_motion || self.reduce_motion {
            return transform;
        }
        let progress = self.handoff_progress_override.unwrap_or_else(|| {
            settle_progress(crate::motion::frame_now().saturating_duration_since(motion.started))
        });
        let amount = (1.0 - progress).powi(2) * motion.strength;
        if amount <= f64::EPSILON {
            return transform;
        }
        let horizontal = motion.axis.x.abs() >= motion.axis.y.abs();
        let (sx, sy) = if horizontal {
            (1.0 + 0.018 * amount, 1.0 - 0.008 * amount)
        } else {
            (1.0 - 0.008 * amount, 1.0 + 0.018 * amount)
        };
        let center = rect.center();
        transform
            .pre_translate((center.x, center.y).into())
            .pre_scale_non_uniform(sx, sy)
            .pre_translate((-center.x, -center.y).into())
    }

    /// Resolve the live rect. Attached overlays follow `app`; detached ones
    /// retain screen coordinates and only clamp to the desktop.
    pub fn rect(&self, app: Rect, desktop: Rect) -> Rect {
        let resolved = match self.attachment {
            Attachment::App { offset } => self.attached_home_rect(app) + offset,
            Attachment::Desktop { rect } => clamp_rect(rect, desktop, 64.0),
        };
        if self.reduce_motion {
            return resolved;
        }
        self.settle_motion.as_ref().map_or(resolved, |motion| {
            let Some(started) = motion.started.get() else {
                motion.started.set(Some(crate::time::now()));
                return motion.from;
            };
            let eased =
                settle_progress(crate::motion::frame_now().saturating_duration_since(started));
            Rect::new(
                motion.from.x0 + (motion.to.x0 - motion.from.x0) * eased,
                motion.from.y0 + (motion.to.y0 - motion.from.y0) * eased,
                motion.from.x1 + (motion.to.x1 - motion.from.x1) * eased,
                motion.from.y1 + (motion.to.y1 - motion.from.y1) * eased,
            )
        })
    }

    fn attached_home_rect(&self, app: Rect) -> Rect {
        if let Some(region) = self.attached_region {
            return region;
        }
        let app = self.safe_area(app);
        let [left, top, right, bottom] = self.attached_chrome;
        let w = self
            .preferred_size
            .width
            .min((app.width() - left - right - 32.0).max(1.0));
        let h = self
            .preferred_size
            .height
            .min((app.height() - top - bottom - 32.0).max(1.0));
        Rect::from_origin_size(
            Point::new(app.x1 - right - w - 16.0, app.y0 + top + 16.0),
            Size::new(w, h),
        )
    }

    pub fn begin_drag(&mut self, pointer: Point, app: Rect, desktop: Rect) {
        let rect_origin = self.rect(app, desktop);
        self.settle_motion = None;
        self.handoff_progress_override = None;
        self.drag = Some(Drag {
            pointer_origin: pointer,
            rect_origin,
            last_pointer: pointer,
            last_sample: crate::time::now(),
            velocity: Vec2::ZERO,
        });
    }

    /// Detaches after crossing the app by a deliberate 28px, and reattaches
    /// when the pointer returns well inside it. The asymmetric thresholds stop
    /// the surface flickering between owners along the border.
    pub fn update_drag(&mut self, pointer: Point, app: Rect, desktop: Rect) -> Handoff {
        let Some(mut drag) = self.drag else {
            return Handoff::None;
        };
        self.settle_motion = None;
        let delta = pointer - drag.pointer_origin;
        let moved = clamp_rect(drag.rect_origin + delta, desktop, 64.0);
        let now = crate::time::now();
        let sample_delta = pointer - drag.last_pointer;
        let elapsed = now.duration_since(drag.last_sample).as_secs_f64();
        let sample = if (0.001..=0.250).contains(&elapsed) {
            sample_delta * (1.0 / elapsed)
        } else {
            sample_delta
        };
        drag.velocity = if drag.velocity.hypot() <= f64::EPSILON {
            sample
        } else {
            drag.velocity * 0.35 + sample * 0.65
        };
        drag.last_pointer = pointer;
        drag.last_sample = now;
        self.drag = Some(drag);
        let motion_axis = if drag.velocity.hypot() > 1.0 {
            drag.velocity
        } else {
            delta
        };
        // Slow, deliberate boundary crossings barely breathe; a quick throw
        // gets the full (still restrained) 1.8% stretch.
        let motion_strength = (drag.velocity.hypot() / 1_200.0).clamp(0.45, 1.0);
        let app_in_pointer_space = match self.attachment {
            Attachment::Desktop { .. } => app + self.desktop_coordinate_offset,
            Attachment::App { .. } => app,
        };
        match self.attachment {
            Attachment::App { .. } if Self::is_beyond_detach_boundary(pointer, app) => {
                self.attachment = Attachment::Desktop { rect: moved };
                self.handoff_rect = Some(moved);
                self.handoff_progress_override = None;
                self.handoff_motion = Some(HandoffMotion {
                    started: now,
                    axis: motion_axis,
                    strength: motion_strength,
                });
                Handoff::Detached
            }
            Attachment::Desktop { .. }
                if !self.return_pending && app_in_pointer_space.inset(36.0).contains(pointer) =>
            {
                self.attachment = Attachment::Desktop { rect: moved };
                // Keep the primary copy hidden until the shell has consumed
                // the final geometry and acknowledged that its proxy/native
                // surface is no longer visible.
                self.return_pending = true;
                self.handoff_progress_override = None;
                self.handoff_motion = Some(HandoffMotion {
                    started: now,
                    axis: motion_axis,
                    strength: motion_strength,
                });
                Handoff::Reattached
            }
            Attachment::Desktop { .. } => {
                self.attachment = Attachment::Desktop { rect: moved };
                Handoff::None
            }
            Attachment::App { .. } => {
                let home = self.attached_home_rect(app);
                // Within the app, the surface never slides under the bands
                // its host keeps clear (the header row).
                let top = self.safe_area(app).y0 + self.attached_chrome[1];
                let y0 = if self.safe_insets[1] > 0.0 {
                    moved.y0.max(top)
                } else {
                    moved.y0
                };
                self.attachment = Attachment::App {
                    offset: Vec2::new(moved.x0 - home.x0, y0 - home.y0),
                };
                Handoff::None
            }
        }
    }

    pub fn end_drag(&mut self, desktop: Rect) {
        let released = self.drag.take();
        let release_velocity = released.map_or(Vec2::ZERO, |drag| drag.velocity);
        if let Attachment::Desktop { rect } = self.attachment {
            if self.return_pending {
                return;
            }
            let from = clamp_rect(rect, desktop, 64.0);
            let to = snap_to_edges(from, desktop, 16.0);
            self.attachment = Attachment::Desktop { rect: to };
            if from != to {
                let axis = to.center() - from.center();
                let started = crate::time::now();
                self.settle_motion = Some(SettleMotion {
                    started: std::cell::Cell::new(None),
                    from,
                    to,
                });
                self.handoff_progress_override = None;
                let motion_axis = if release_velocity.hypot() > 1.0 {
                    release_velocity
                } else {
                    axis
                };
                let strength = (release_velocity.hypot() / 1_200.0).clamp(0.45, 1.0);
                self.handoff_motion = Some(HandoffMotion {
                    started,
                    axis: motion_axis,
                    strength,
                });
            }
        }
    }
}

fn clamp_rect(rect: Rect, bounds: Rect, reachable: f64) -> Rect {
    let mut dx = 0.0;
    let mut dy = 0.0;
    if rect.x1 < bounds.x0 + reachable {
        dx = bounds.x0 + reachable - rect.x1;
    }
    if rect.x0 > bounds.x1 - reachable {
        dx = bounds.x1 - reachable - rect.x0;
    }
    if rect.y1 < bounds.y0 + reachable {
        dy = bounds.y0 + reachable - rect.y1;
    }
    if rect.y0 > bounds.y1 - reachable {
        dy = bounds.y1 - reachable - rect.y0;
    }
    rect + Vec2::new(dx, dy)
}

fn snap_to_edges(rect: Rect, bounds: Rect, gap: f64) -> Rect {
    let mut out = rect;
    let threshold = 32.0;
    if (rect.x0 - bounds.x0).abs() < threshold {
        out = out + Vec2::new(bounds.x0 + gap - rect.x0, 0.0);
    }
    if (bounds.x1 - rect.x1).abs() < threshold {
        out = out + Vec2::new(bounds.x1 - gap - out.x1, 0.0);
    }
    if (rect.y0 - bounds.y0).abs() < threshold {
        out = out + Vec2::new(0.0, bounds.y0 + gap - rect.y0);
    }
    if (bounds.y1 - rect.y1).abs() < threshold {
        out = out + Vec2::new(0.0, bounds.y1 - gap - out.y1);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn optional_input_geometry_never_expands_the_host_window() {
        let window = Rect::new(100.0, 80.0, 500.0, 400.0);
        assert_eq!(protocol_input_rect(window, None).unwrap(), None);
        let input = [108.0, 88.0, 492.0, 392.0];
        assert_eq!(
            protocol_input_rect(window, Some(input)).unwrap(),
            Some(Rect::new(108.0, 88.0, 492.0, 392.0))
        );
        for invalid in [
            [99.0, 88.0, 492.0, 392.0],
            [108.0, 88.0, 501.0, 392.0],
            [108.0, 88.0, 108.0, 392.0],
            [108.0, 88.0, f64::NAN, 392.0],
            [108.0, 88.0, 492.0, f64::INFINITY],
        ] {
            assert!(protocol_input_rect(window, Some(invalid)).is_err());
        }
        let translated =
            translate_input_rect(Some(window), window.origin(), Point::new(200.0, 180.0));
        assert_eq!(translated, Some(window + Vec2::new(100.0, 100.0)));
    }

    fn instant_ago(duration: crate::time::Duration) -> crate::time::Instant {
        crate::time::now()
            .checked_sub(duration)
            .unwrap_or_else(crate::time::now)
    }

    #[test]
    fn repeated_float_return_keeps_independent_conversation_ownership() {
        let app = Rect::new(100.0, 80.0, 900.0, 700.0);
        let desktop = Rect::new(-800.0, 0.0, 1600.0, 900.0);
        let inline = Rect::new(300.0, 120.0, 840.0, 660.0);
        let detached = Rect::new(-700.0, 150.0, -160.0, 690.0);
        let mut conversation = FloatingPlacement::attached(inline.size());
        conversation.set_attached_region(Some(inline));
        let mut sibling = FloatingPlacement::attached(inline.size());
        sibling.detach_to_desktop(Rect::new(940.0, 120.0, 1480.0, 660.0));
        sibling.confirm_desktop_adoption();
        let sibling_rect = sibling.rect(app, desktop);
        for _ in 0..3 {
            conversation.detach_to_desktop(detached);
            assert!(!conversation.is_desktop_adopted());
            conversation.prepare_desktop_rect_with_offset(detached, Vec2::new(-1000.0, 30.0));
            assert_eq!(conversation.rect(app, desktop), detached);
            conversation.confirm_desktop_adoption();
            conversation.request_app_return();
            assert!(conversation.is_return_pending());
            assert!(matches!(
                conversation.attachment(),
                Attachment::Desktop { .. }
            ));
            conversation.confirm_app_adoption(app);
            assert!(matches!(conversation.attachment(), Attachment::App { .. }));
            assert!(!conversation.is_desktop_adopted());
            assert!(!conversation.is_return_pending());
            assert!(sibling.is_desktop_adopted());
            assert_eq!(sibling.rect(app, desktop), sibling_rect);
        }
    }

    #[test]
    fn attached_and_detached_surfaces_stay_inside_their_owner() {
        {
            let p = FloatingPlacement::attached(Size::new(320.0, 180.0));
            let desk = Rect::new(0.0, 0.0, 1400.0, 900.0);
            let a = p.rect(Rect::new(100.0, 80.0, 900.0, 700.0), desk);
            let b = p.rect(Rect::new(240.0, 120.0, 1040.0, 740.0), desk);
            assert_eq!(b.x0 - a.x0, 140.0);
            assert_eq!(b.y0 - a.y0, 40.0);
        }

        {
            let app = Rect::new(100.0, 80.0, 900.0, 700.0);
            let desk = Rect::new(0.0, 0.0, 1_400.0, 900.0);
            let inline = Rect::new(310.0, 120.0, 840.0, 660.0);
            let mut p = FloatingPlacement::attached(inline.size());
            p.set_attached_region(Some(inline));
            assert_eq!(p.rect(app, desk), inline);
            p.set_attached_region(Some(Rect::new(f64::NAN, 0.0, 2.0, 2.0)));
            assert_eq!(p.rect(app, desk), inline, "invalid geometry is ignored");
        }

        {
            let app = Rect::new(100.0, 80.0, 900.0, 700.0);
            let desk = Rect::new(0.0, 0.0, 1_400.0, 900.0);
            let mut p = FloatingPlacement::attached(Size::new(320.0, 180.0));
            p.request_app_return();
            assert!(!p.is_return_pending());
            p.detach_to_desktop(Rect::new(940.0, 120.0, 1_260.0, 300.0));
            p.request_app_return();
            assert!(
                !p.is_return_pending(),
                "a request is not proof that desktop pixels were adopted"
            );
            p.prepare_desktop_rect(Rect::new(940.0, 120.0, 1_260.0, 300.0));
            p.confirm_desktop_adoption();
            p.request_app_return();
            assert!(p.is_return_pending());
            p.reattach_to_app(app, desk);
            assert!(!p.is_return_pending());
        }

        // Case `safe_insets_keep_the_home_and_a_drag_below_the_header_row`.
        {
            let app = Rect::new(100.0, 80.0, 900.0, 700.0);
            let desk = Rect::new(0.0, 0.0, 1400.0, 900.0);
            let mut p = FloatingPlacement::attached(Size::new(320.0, 180.0));
            let plain = p.rect(app, desk);
            p.set_safe_insets([0.0, 56.0, 0.0, 0.0]);
            let home = p.rect(app, desk);
            assert!(
                (home.y0 - (plain.y0 + 56.0)).abs() < 1e-9,
                "home clears the header row"
            );
            p.begin_drag(home.center(), app, desk);
            p.update_drag(home.center() - Vec2::new(0.0, 100.0), app, desk);
            let dragged = p.rect(app, desk);
            assert!(
                dragged.y0 >= app.y0 + 56.0 - 1e-9,
                "a drag stays below it: {dragged:?}"
            );
        }

        {
            let app = Rect::new(100.0, 80.0, 900.0, 700.0);
            let desk = Rect::new(0.0, 0.0, 1400.0, 900.0);
            let mut p = FloatingPlacement::attached(Size::new(320.0, 180.0));
            p.set_attached_chrome([12.0, 40.0, 12.0, 156.0]);
            let content = p.rect(app, desk);
            let complete = Rect::new(
                content.x0 - 12.0,
                content.y0 - 40.0,
                content.x1 + 12.0,
                content.y1 + 156.0,
            );
            assert!(app.contains(complete.origin()));
            assert!(app.contains(Point::new(complete.x1, complete.y1)));
            assert_eq!(complete.y0, app.y0 + 16.0);
            assert_eq!(complete.x1, app.x1 - 16.0);
        }

        {
            let app = Rect::new(200.0, 100.0, 900.0, 700.0);
            let desk = Rect::new(0.0, 0.0, 1400.0, 900.0);
            let mut p = FloatingPlacement::attached(Size::new(320.0, 180.0));
            let pointer = Point::new(600.0, 150.0);
            let before = p.rect(app, desk);
            p.begin_drag(pointer, app, desk);
            let outside = Point::new(940.0, 150.0);
            assert!(!FloatingPlacement::is_beyond_detach_boundary(
                Point::new(app.x1 + 27.0, app.y0),
                app
            ));
            assert!(FloatingPlacement::is_beyond_detach_boundary(
                Point::new(app.x1 + 28.0, app.y0),
                app
            ));
            assert!(FloatingPlacement::is_beyond_detach_boundary(outside, app));
            assert_eq!(p.update_drag(outside, app, desk), Handoff::Detached);
            assert!(matches!(p.attachment(), Attachment::Desktop { .. }));
            assert_eq!(
                p.rect(app, desk),
                before + (outside - pointer),
                "ownership transfer must not move the surface by even one logical pixel"
            );
        }

        {
            let app = Rect::new(100.0, 100.0, 700.0, 700.0);
            let desk = Rect::new(0.0, 0.0, 1400.0, 900.0);
            let mut p = FloatingPlacement::attached(Size::new(320.0, 180.0));
            p.begin_drag(Point::new(600.0, 150.0), app, desk);
            assert_eq!(
                p.update_drag(Point::new(6_000.0, 150.0), app, desk),
                Handoff::Detached
            );
            let r = p.rect(app, desk);
            assert_eq!(r.x0, desk.x1 - 64.0);
            assert_eq!(r.x1, desk.x1 - 64.0 + 320.0);
        }

        {
            let app = Rect::new(100.0, 100.0, 700.0, 700.0);
            let desk = Rect::new(0.0, 0.0, 1400.0, 900.0);
            let released = Rect::new(1055.0, 300.0, 1375.0, 480.0);
            let target = Rect::new(1064.0, 300.0, 1384.0, 480.0);
            let mut p = FloatingPlacement::attached(released.size());
            p.attachment = Attachment::Desktop { rect: released };
            p.end_drag(desk);
            assert!(p.is_handoff_animating());
            assert!((p.rect(app, desk).x0 - released.x0).abs() < 1.0);

            p.settle_motion
                .as_mut()
                .unwrap()
                .started
                .set(Some(instant_ago(
                    settle_time().saturating_add(crate::time::Duration::from_millis(1)),
                )));
            assert_eq!(p.rect(app, desk), target);
        }
    }

    #[test]
    fn handoff_motion_is_continuous_across_owners() {
        {
            let app = Rect::new(200.0, 100.0, 900.0, 700.0);
            let desk = Rect::new(0.0, 0.0, 1400.0, 900.0);
            let mut p = FloatingPlacement::attached(Size::new(320.0, 180.0));
            p.begin_drag(Point::new(600.0, 150.0), app, desk);
            let _ = p.update_drag(Point::new(940.0, 150.0), app, desk);
            let rect = p.rect(app, desk);
            let transform = p.presentation_transform(Affine::IDENTITY, rect, false);
            assert_eq!(transform * rect.center(), rect.center());
            assert_eq!(
                p.presentation_transform(Affine::IDENTITY, rect, true),
                Affine::IDENTITY,
                "reduced motion must disable the handoff flourish"
            );
        }

        {
            let app = Rect::new(200.0, 100.0, 900.0, 700.0);
            let desk = Rect::new(0.0, 0.0, 1400.0, 900.0);
            let mut p = FloatingPlacement::attached(Size::new(320.0, 180.0));
            p.detach_to_desktop(Rect::new(500.0, 180.0, 820.0, 360.0));
            p.pin_handoff_for_visual_review(500);
            let rect = p.rect(app, desk);
            let desktop_transform = p.presentation_transform(Affine::IDENTITY, rect, false);

            p.handoff_motion.as_mut().unwrap().started =
                instant_ago(settle_time().saturating_add(crate::time::Duration::from_secs(1)));
            assert_eq!(
                p.presentation_transform(Affine::IDENTITY, rect, false),
                desktop_transform,
                "a pinned review frame must not drift with wall-clock time"
            );

            p.confirm_app_adoption(app);
            assert_eq!(p.rect(app, desk), rect);
            assert_eq!(
                p.presentation_transform(Affine::IDENTITY, rect, false),
                desktop_transform,
                "switching owners must preserve the exact visual handoff phase"
            );
            assert!(
                !p.is_handoff_animating(),
                "a pinned review frame must not keep the headless renderer alive"
            );
        }

        {
            let app = Rect::new(200.0, 100.0, 900.0, 700.0);
            let desk = Rect::new(0.0, 0.0, 1400.0, 900.0);
            let mut p = FloatingPlacement::attached(Size::new(320.0, 180.0));
            p.begin_drag(Point::new(600.0, 150.0), app, desk);
            p.drag.as_mut().unwrap().last_sample =
                instant_ago(crate::time::Duration::from_millis(16));
            assert_eq!(
                p.update_drag(Point::new(600.0, 230.0), app, desk),
                Handoff::None
            );
            p.drag.as_mut().unwrap().last_sample =
                instant_ago(crate::time::Duration::from_millis(16));
            assert_eq!(
                p.update_drag(Point::new(950.0, 220.0), app, desk),
                Handoff::Detached
            );
            let motion = p.handoff_motion.unwrap();
            assert!(
                motion.axis.x.abs() > motion.axis.y.abs(),
                "the boundary flourish should follow the latest fast rightward motion"
            );
            assert!(motion.strength > 0.9, "a quick throw gets the full cue");
        }

        {
            let app = Rect::new(200.0, 100.0, 900.0, 700.0);
            let desk = Rect::new(0.0, 0.0, 1400.0, 900.0);
            let mut p = FloatingPlacement::attached(Size::new(320.0, 180.0));
            let origin = Point::new(600.0, 150.0);
            let initial = p.rect(app, desk);
            p.begin_drag(origin, app, desk);
            assert_eq!(
                p.update_drag(Point::new(950.0, 150.0), app, desk),
                Handoff::Detached
            );
            let returned = Point::new(500.0, 300.0);
            assert_eq!(p.update_drag(returned, app, desk), Handoff::Reattached);
            assert!(p.is_return_pending());
            assert!(matches!(p.attachment(), Attachment::Desktop { .. }));
            assert_eq!(
                p.rect(app, desk),
                initial + (returned - origin),
                "shell → app ownership transfer must preserve geometry too"
            );
            p.confirm_app_adoption(app);
            assert!(!p.is_return_pending());
            assert!(matches!(p.attachment(), Attachment::App { .. }));
            assert_eq!(p.rect(app, desk), initial + (returned - origin));
        }

        {
            let app = Rect::new(0.0, 0.0, 700.0, 600.0);
            let desktop = Rect::new(0.0, 0.0, 1600.0, 900.0);
            let app_to_desktop = Vec2::new(240.0, 100.0);
            let mut p = FloatingPlacement::attached(Size::new(320.0, 180.0));
            let origin = Point::new(600.0, 120.0);
            p.begin_drag(origin, app, desktop);
            p.drag.as_mut().unwrap().last_sample =
                instant_ago(crate::time::Duration::from_millis(16));
            assert_eq!(
                p.update_drag(Point::new(900.0, 150.0), app, desktop),
                Handoff::Detached
            );

            let prepared = p.rect(app, desktop) + app_to_desktop;
            p.prepare_desktop_rect_with_offset(prepared, app_to_desktop);
            p.confirm_desktop_adoption();
            p.drag.as_mut().unwrap().last_sample =
                instant_ago(crate::time::Duration::from_millis(16));
            let returned_desktop_pointer = Point::new(620.0, 310.0);
            assert_eq!(
                p.update_drag(returned_desktop_pointer, app, desktop),
                Handoff::Reattached
            );
            let last_shell_rect = p.rect(app, desktop);
            let shell_velocity = p.drag.unwrap().velocity;
            let shell_motion = p.handoff_motion.unwrap();

            p.confirm_app_adoption(app);
            assert!(p.is_dragging(), "the held drag survives the ownership edge");
            assert_eq!(
                p.rect(app, desktop) + app_to_desktop,
                last_shell_rect,
                "the first app-owned pixels cover the exact final shell rect"
            );
            assert_eq!(
                p.drag.unwrap().velocity,
                shell_velocity,
                "the soft handoff flourish must retain its velocity vector"
            );
            let app_motion = p.handoff_motion.unwrap();
            assert_eq!(app_motion.started, shell_motion.started);
            assert_eq!(app_motion.axis, shell_motion.axis);
            assert_eq!(app_motion.strength, shell_motion.strength);

            let next_delta = Vec2::new(12.0, -8.0);
            let next_local_pointer = returned_desktop_pointer - app_to_desktop + next_delta;
            assert_eq!(
                p.update_drag(next_local_pointer, app, desktop),
                Handoff::None
            );
            assert_eq!(
                p.rect(app, desktop) + app_to_desktop,
                last_shell_rect + next_delta,
                "the first primary-window pointer event continues the same drag"
            );
            p.end_drag(desktop);
            assert!(!p.is_dragging());
        }

        {
            let app = Rect::new(200.0, 100.0, 900.0, 700.0);
            let desk = Rect::new(0.0, 0.0, 1400.0, 900.0);
            let mut p = FloatingPlacement::attached(Size::new(320.0, 180.0));
            let home = p.rect(app, desk);
            p.set_reduce_motion(true);
            p.begin_drag(Point::new(600.0, 150.0), app, desk);
            let _ = p.update_drag(Point::new(1_050.0, 200.0), app, desk);
            p.reattach_to_app(app, desk);
            assert_eq!(p.rect(app, desk), home, "no snap-back under reduced motion");
            assert!(!p.is_handoff_animating(), "nothing asks for motion frames");
            let rect = Rect::new(0.0, 0.0, 100.0, 50.0);
            p.detach_to_desktop(Rect::new(500.0, 180.0, 820.0, 360.0));
            assert_eq!(
                p.presentation_transform(Affine::IDENTITY, rect, false),
                Affine::IDENTITY,
                "the ownership flourish is not played"
            );
        }

        {
            let app = Rect::new(200.0, 100.0, 900.0, 700.0);
            let desk = Rect::new(0.0, 0.0, 1400.0, 900.0);
            let mut p = FloatingPlacement::attached(Size::new(320.0, 180.0));
            p.begin_drag(Point::new(600.0, 150.0), app, desk);
            let _ = p.update_drag(Point::new(1_050.0, 200.0), app, desk);
            let detached = p.rect(app, desk);
            p.reattach_to_app(app, desk);
            assert!(matches!(p.attachment(), Attachment::App { .. }));
            assert!(
                (p.rect(app, desk).x0 - detached.x0).abs() < 1.0,
                "the decline frame stays where the pointer left it"
            );
            p.settle_motion
                .as_mut()
                .unwrap()
                .started
                .set(Some(instant_ago(
                    settle_time().saturating_add(crate::time::Duration::from_millis(1)),
                )));
            assert_eq!(p.rect(app, desk), p.attached_home_rect(app));
        }
    }

    #[test]
    fn capture_protection_request_has_a_stable_wire_form() {
        for protected in [true, false] {
            let message = FloatingSurfaceClientMessage::SetCaptureProtection {
                protected,
                generation: None,
            };
            let json = serde_json::to_string(&message).unwrap();
            assert_eq!(
                json,
                format!(r#"{{"type":"set_capture_protection","protected":{protected}}}"#)
            );
            assert_eq!(
                serde_json::from_str::<FloatingSurfaceClientMessage>(&json).unwrap(),
                message
            );
        }
    }

    #[test]
    fn surface_protocol_carries_explicit_geometry_and_rejects_invalid_frames() {
        {
            let req = FloatingSurfaceRequest {
                id: "pair".into(),
                title: "Compare".into(),
                role: FloatingRole::PictureInPicture,
                source: FloatingSource::Composite(vec![
                    FloatingSource::Surface { surface_id: 1 },
                    FloatingSource::Surface { surface_id: 2 },
                ]),
                preferred_size: Size::new(480.0, 270.0),
                aspect_ratio: Some(16.0 / 9.0),
                initial_rect: None,
                presentation_rect: None,
                tracking_source: None,
            };
            assert!(matches!(req.source, FloatingSource::Composite(ref v) if v.len() == 2));
        }

        {
            let request = FloatingSurfaceRequest {
                id: "comparison".into(),
                title: "Live comparison".into(),
                role: FloatingRole::PictureInPicture,
                source: FloatingSource::Composite(vec![
                    FloatingSource::AppRegion {
                        app_id: "so.tilo.editor".into(),
                        region: Rect::new(12.0, 30.0, 412.0, 330.0),
                    },
                    FloatingSource::AppElement {
                        app_id: "so.tilo.browser".into(),
                        element_id: "video:main".into(),
                        region: Rect::new(20.0, 40.0, 620.0, 378.0),
                    },
                    FloatingSource::Surface { surface_id: 42 },
                ]),
                preferred_size: Size::new(520.0, 320.0),
                aspect_ratio: Some(1.625),
                initial_rect: Some(Rect::new(600.0, 80.0, 1_120.0, 400.0)),
                presentation_rect: Some(Rect::new(590.0, 70.0, 1_130.0, 410.0)),
                tracking_source: None,
            };
            let message = FloatingSurfaceClientMessage::Present {
                request: (&request).into(),
            };
            let json = serde_json::to_string(&message).unwrap();
            let decoded: FloatingSurfaceClientMessage = serde_json::from_str(&json).unwrap();
            let FloatingSurfaceClientMessage::Present { request: wire } = decoded else {
                panic!("present must remain present across the transport");
            };
            assert_eq!(FloatingSurfaceRequest::try_from(wire).unwrap(), request);
        }

        {
            let messages = [
                FloatingSurfaceHostMessage::PrepareSurface {
                    id: "call".into(),
                    token: "0123456789abcdef0123456789abcdef".into(),
                    rect: [92.0, 32.0, 488.0, 288.0],
                    content_rect: [100.0, 40.0, 480.0, 280.0],
                    transfer_covered: true,
                    coordinate_offset: [220.0, 90.0],
                },
                FloatingSurfaceHostMessage::Adopted {
                    id: "call".into(),
                    rect: [100.0, 40.0, 480.0, 280.0],
                },
            ];
            for message in messages {
                let json = serde_json::to_string(&message).unwrap();
                assert_eq!(
                    serde_json::from_str::<FloatingSurfaceHostMessage>(&json).unwrap(),
                    message
                );
            }
            let geometry = FloatingSurfaceClientMessage::SurfaceGeometry {
                id: "call".into(),
                rect: [192.0, 72.0, 588.0, 328.0],
                content_rect: [200.0, 80.0, 580.0, 320.0],
                input_rect: Some([200.0, 80.0, 580.0, 320.0]),
            };
            let json = serde_json::to_string(&geometry).unwrap();
            assert_eq!(
                serde_json::from_str::<FloatingSurfaceClientMessage>(&json).unwrap(),
                geometry
            );

            for message in [
                FloatingSurfaceClientMessage::OpenCaptureStudio {
                    mode: CaptureStudioProtocolMode::PictureInPicture,
                },
                FloatingSurfaceClientMessage::SourceCleared { id: "call".into() },
                FloatingSurfaceClientMessage::DestinationPresented { id: "call".into() },
                FloatingSurfaceClientMessage::Suspend {
                    id: "call".into(),
                    rect: [192.0, 72.0, 588.0, 328.0],
                    content_rect: [200.0, 80.0, 580.0, 320.0],
                },
                FloatingSurfaceClientMessage::Dismiss { id: "call".into() },
            ] {
                let json = serde_json::to_string(&message).unwrap();
                assert_eq!(
                    serde_json::from_str::<FloatingSurfaceClientMessage>(&json).unwrap(),
                    message
                );
            }
            let suspended = FloatingSurfaceHostMessage::Suspended { id: "call".into() };
            let json = serde_json::to_string(&suspended).unwrap();
            assert_eq!(
                serde_json::from_str::<FloatingSurfaceHostMessage>(&json).unwrap(),
                suspended
            );
        }

        {
            let base = FloatingSurfaceProtocolRequest {
                id: "call".into(),
                title: "Call".into(),
                role: FloatingSurfaceProtocolRole::ActiveCall,
                source: FloatingSurfaceProtocolSource::AppRegion {
                    app_id: "so.tilo.chat".into(),
                    region: [10.0, 20.0, 210.0, 140.0],
                },
                preferred_size: [360.0, 220.0],
                aspect_ratio: None,
                initial_rect: Some([900.0, 40.0, 1_260.0, 260.0]),
                presentation_rect: Some([892.0, 32.0, 1_268.0, 268.0]),
            };
            let mut inverted = base;
            inverted.source = FloatingSurfaceProtocolSource::AppRegion {
                app_id: "so.tilo.chat".into(),
                region: [210.0, 20.0, 10.0, 140.0],
            };
            assert!(FloatingSurfaceRequest::try_from(inverted).is_err());
        }

        {
            let bounds = FloatingSurfaceHostMessage::DesktopBounds {
                bounds: [0.0, 0.0, 3200.0, 1080.0],
            };
            let json = serde_json::to_string(&bounds).unwrap();
            assert!(json.contains("desktop_bounds"));
            assert_eq!(
                serde_json::from_str::<FloatingSurfaceHostMessage>(&json).unwrap(),
                bounds
            );
            assert_ne!(
                FloatingSurfaceHostMessage::Adopted {
                    id: "call".into(),
                    rect: [100.0, 100.0, 460.0, 320.0],
                },
                FloatingSurfaceHostMessage::Reattach { id: "call".into() }
            );
        }
    }

    #[test]
    fn prepare_rebases_the_live_handoff_without_frame_jumps() {
        {
            let app = Rect::new(200.0, 100.0, 900.0, 700.0);
            let desk = Rect::new(0.0, 0.0, 1_400.0, 900.0);
            let mut placement = FloatingPlacement::attached(Size::new(320.0, 180.0));
            placement.begin_drag(Point::new(600.0, 150.0), app, desk);
            assert_eq!(
                placement.update_drag(Point::new(950.0, 150.0), app, desk),
                Handoff::Detached
            );
            let prepared = Rect::new(1_000.0, 120.0, 1_320.0, 300.0);
            placement.prepare_desktop_rect(prepared);
            assert_eq!(placement.rect(app, desk), prepared);
            assert!(placement.is_dragging(), "the pointer grab survives prepare");
            assert!(
                !placement.is_desktop_adopted(),
                "creation is not evidence that pixels are visible"
            );

            placement.confirm_desktop_adoption();
            assert!(placement.is_desktop_adopted());
            assert_eq!(
                placement.rect(app, desk),
                prepared,
                "ownership acknowledgement cannot replay stale geometry"
            );
        }

        {
            let app = Rect::new(0.0, 0.0, 700.0, 600.0);
            let desk = Rect::new(0.0, 0.0, 1_400.0, 900.0);
            let mut placement = FloatingPlacement::attached(Size::new(320.0, 180.0));
            let press = Point::new(600.0, 40.0);
            placement.begin_drag(press, app, desk);
            let outside = Point::new(750.0, 40.0);
            assert_eq!(placement.update_drag(outside, app, desk), Handoff::Detached);
            let local_detached = placement.rect(app, desk);
            let coordinate_offset = Vec2::new(240.0, 110.0);
            let prepared = local_detached + coordinate_offset;
            placement.prepare_desktop_rect_with_offset(prepared, coordinate_offset);
            assert_eq!(placement.rect(app, desk), prepared);

            // The compositor may deliver a native cursor event without the mouse
            // moving at all. Its coordinates are desktop-space; it must not pull
            // the surface by the app's origin.
            assert_eq!(
                placement.update_drag(outside + coordinate_offset, app, desk),
                Handoff::None
            );
            assert_eq!(placement.rect(app, desk), prepared);
        }

        {
            let app = Rect::new(0.0, 0.0, 1_310.0, 768.0);
            let desk = Rect::new(0.0, 0.0, 1_600.0, 900.0);
            let app_origin = Vec2::new(262.0, 124.0);
            let mut placement = FloatingPlacement::attached(Size::new(360.0, 156.0));
            let press = Point::new(1_114.0, 24.0);
            placement.begin_drag(press, app, desk);
            let crossing = Point::new(1_114.0, -64.0);
            assert_eq!(
                placement.update_drag(crossing, app, desk),
                Handoff::Detached
            );
            let request_rect = placement.rect(app, desk);

            // A fast lateral move lands before the shell replies to Present.
            let pre_prepare_pointer = Point::new(738.0, -64.0);
            assert_eq!(
                placement.update_drag(pre_prepare_pointer, app, desk),
                Handoff::None
            );
            let live_before_prepare = placement.rect(app, desk);

            placement.prepare_desktop_rect_with_offset(request_rect + app_origin, app_origin);
            assert_eq!(
                placement.rect(app, desk),
                live_before_prepare + app_origin,
                "Prepare references the crossing frame; newer live motion must survive it"
            );
            assert_eq!(
                placement.update_drag(pre_prepare_pointer + app_origin, app, desk),
                Handoff::None
            );
            assert_eq!(placement.rect(app, desk), live_before_prepare + app_origin);
        }

        {
            let app = Rect::new(0.0, 0.0, 1_310.0, 768.0);
            let desktop = Rect::new(0.0, 0.0, 1_600.0, 900.0);
            let app_origin = Vec2::new(262.0, 124.0);
            let mut placement = FloatingPlacement::attached(Size::new(360.0, 156.0));
            let press_local = Point::new(1_114.0, 24.0);
            let initial_local = placement.rect(app, desktop);
            placement.begin_drag(press_local, app, desktop);
            assert_eq!(
                placement.update_drag(Point::new(1_114.0, -104.0), app, desktop),
                Handoff::Detached
            );
            let local_detached = placement.rect(app, desktop);
            placement.prepare_desktop_rect_with_offset(local_detached + app_origin, app_origin);

            // The native renderer is still warming, so compositor pointer focus
            // remains on the primary window and reports app-local coordinates.
            // The runtime translates that event before the shared placement sees
            // it; ownership can then return without losing the app origin.
            let returned_desktop = Point::new(1_376.0, 180.0);
            let returned_primary_local = returned_desktop - app_origin;
            assert_eq!(
                placement.update_drag(returned_primary_local + app_origin, app, desktop),
                Handoff::Reattached
            );
            assert!(placement.is_return_pending());
            let returned_desktop_rect = placement.rect(app, desktop);
            let expected_desktop =
                initial_local + app_origin + (returned_desktop - (press_local + app_origin));
            assert_eq!(returned_desktop_rect, expected_desktop);
            placement.confirm_app_adoption(app);
            assert_eq!(placement.rect(app, desktop) + app_origin, expected_desktop);
        }
    }

    #[cfg(unix)]
    #[test]
    fn nonblocking_service_client_bounds_output_and_completes_lifecycles() {
        {
            let (app_side, _shell_side) = UnixStream::pair().unwrap();
            let mut client = FloatingSurfaceServiceClient::from_stream(app_side).unwrap();
            let chunk = MAX_PROTOCOL_FRAME_BYTES / 2;
            for _ in 0..8 {
                client.enqueue_frame(vec![0; chunk]).unwrap();
            }
            assert_eq!(client.outgoing_bytes, MAX_OUTGOING_QUEUE_BYTES);
            assert_eq!(client.outgoing.len(), 8);
            assert!(
                client.enqueue_frame(vec![0]).is_err(),
                "a stalled peer must exert bounded backpressure"
            );
        }

        {
            use std::io::{BufRead, BufReader};

            let (app_side, mut shell_side) = UnixStream::pair().unwrap();
            shell_side
                .set_read_timeout(Some(tilo_test_env::hang_bound()))
                .unwrap();
            let mut client = FloatingSurfaceServiceClient::from_stream(app_side).unwrap();
            let outbound = FloatingSurfaceClientMessage::Dismiss { id: "call".into() };
            client.send(&outbound).unwrap();

            let mut reader = BufReader::new(shell_side.try_clone().unwrap());
            let mut read = || {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                serde_json::from_str::<FloatingSurfaceClientMessage>(line.trim()).unwrap()
            };
            assert_eq!(
                read(),
                FloatingSurfaceClientMessage::Hello {
                    version: FLOATING_SURFACE_PROTOCOL_VERSION
                },
                "the app says its version before anything else"
            );
            assert_eq!(read(), outbound);

            let inbound = FloatingSurfaceHostMessage::DesktopBounds {
                bounds: [0.0, 0.0, 2560.0, 1440.0],
            };
            let mut bytes = serde_json::to_vec(&inbound).unwrap();
            bytes.push(b'\n');
            shell_side.write_all(&bytes).unwrap();
            let mut received = Vec::new();
            tilo_test_env::wait_until("the inbound surface message", || {
                received.extend(client.pump().unwrap());
                !received.is_empty()
            });
            assert_eq!(received, vec![inbound]);

            // A shell at another version says so, and the connection is over.
            let mut bytes = serde_json::to_vec(&FloatingSurfaceHostMessage::VersionMismatch {
                shell: FLOATING_SURFACE_PROTOCOL_VERSION + 1,
            })
            .unwrap();
            bytes.push(b'\n');
            shell_side.write_all(&bytes).unwrap();
            let mut ended = None;
            tilo_test_env::wait_until("the shell's version mismatch", || {
                match client.pump() {
                    Ok(messages) => assert!(messages.is_empty(), "{messages:?}"),
                    Err(error) => ended = Some(error),
                }
                ended.is_some()
            });
            assert!(
                ended.is_some_and(|error| error.contains("shell-ui version")),
                "a mismatch ends the client"
            );
        }
    }

    #[test]
    fn a_window_appearance_round_trips_and_unknown_values_are_unsaid() {
        use crate::theme::WindowAppearance;
        use crate::tokens::PresenceHue;

        let told = WindowAppearance {
            workspace_hue: Some(40.0),
            your_colour: Some(PresenceHue::Violet),
            corner_radius: None,
        };
        let message = FloatingSurfaceHostMessage::appearance(&told);
        let wire = serde_json::to_value(&message).unwrap();
        assert_eq!(
            wire,
            serde_json::json!({"type": "appearance", "workspace_hue": 40.0, "your_colour": "violet"})
        );
        let back: FloatingSurfaceHostMessage = serde_json::from_value(wire).unwrap();
        assert_eq!(back.window_appearance(), Some(told));

        let neutral: FloatingSurfaceHostMessage =
            serde_json::from_value(serde_json::json!({"type": "appearance"})).unwrap();
        assert_eq!(
            neutral.window_appearance(),
            Some(WindowAppearance::default()),
            "no temperature and no colour said"
        );
        let unknown: FloatingSurfaceHostMessage = serde_json::from_value(
            serde_json::json!({"type": "appearance", "workspace_hue": 400.0, "your_colour": "chartreuse"}),
        )
        .unwrap();
        assert_eq!(
            unknown.window_appearance(),
            Some(WindowAppearance {
                workspace_hue: Some(40.0),
                your_colour: None,
                corner_radius: None,
            })
        );
        assert_eq!(
            FloatingSurfaceHostMessage::Closed { id: "pip".into() }.window_appearance(),
            None
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_idle_client_wakes_once_per_arrival() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let (app_side, mut shell_side) = UnixStream::pair().unwrap();
        let wakes = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&wakes);
        let parks = Arc::new(AtomicUsize::new(0));
        let parked = Arc::clone(&parks);
        let mut client = FloatingSurfaceServiceClient::from_stream_watched(
            app_side,
            move || {
                counted.fetch_add(1, Ordering::SeqCst);
            },
            move || {
                parked.fetch_add(1, Ordering::SeqCst);
            },
        )
        .unwrap();
        let send = |shell_side: &mut UnixStream| {
            let mut bytes = serde_json::to_vec(&FloatingSurfaceHostMessage::appearance(
                &crate::theme::WindowAppearance {
                    workspace_hue: Some(40.0),
                    your_colour: None,
                    corner_radius: None,
                },
            ))
            .unwrap();
            bytes.push(b'\n');
            shell_side.write_all(&bytes).unwrap();
        };
        send(&mut shell_side);
        tilo_test_env::wait_until("the watcher waits for the app to read", || {
            parks.load(Ordering::SeqCst) >= 1
        });
        assert_eq!(parks.load(Ordering::SeqCst), 1);
        assert_eq!(
            wakes.load(Ordering::SeqCst),
            1,
            "unread bytes wake the app once, not in a loop"
        );
        assert_eq!(client.pump().unwrap().len(), 1);
        send(&mut shell_side);
        tilo_test_env::wait_until("the next arrival wakes it again", || {
            wakes.load(Ordering::SeqCst) >= 2
        });
        assert_eq!(client.pump().unwrap().len(), 1);
    }
}
