//! What an app and the session shell say to each other on the shell-ui connection: the protocol's one
//! version, its messages and the plain types they carry.
#![allow(
    clippy::arithmetic_side_effects,
    reason = "placement math is bounded logical-pixel geometry using kurbo operators"
)]
use serde::{Deserialize, Serialize};

/// What a recipient of a sent message is: someone the search found, or the agent the person is making
/// by sending.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RecipientKind {
    /// An agent this send makes, kept by the person sending.
    NewAgent,
    /// An agent the workspace keeps and the search found.
    Agent,
    /// A person the workspace knows, by their root identity.
    Person,
    /// A conversation, opened where it is and posted in.
    Conversation,
    /// The one place a report about Tilo itself goes, which the session names
    /// when it opens the box there: the search never finds it, and it never
    /// sits beside another recipient.
    Feedback,
}

impl RecipientKind {
    /// The word a row of the box shows for it, when it needs one.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::NewAgent => "New agent",
            Self::Agent => "Agent",
            Self::Person => "Person",
            Self::Conversation => "Conversation",
            Self::Feedback => "Feedback",
        }
    }
}

/// One recipient of a message sent from a window: who it goes to, and what the person reads on its
/// chip.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recipient {
    pub kind: RecipientKind,
    /// The agent's id, the person's root identity or the conversation's id; empty for
    /// [`RecipientKind::NewAgent`], which the shell makes.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
    /// The name the chip shows: the agent's or the person's, the conversation's, or the name the new
    /// agent takes — empty for a new agent nobody named, which the shell names by what it is asked.
    pub name: String,
    /// A line under the name in the search's rows (who keeps an agent, who
    /// is in a conversation); never on the chip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl Recipient {
    /// The agent this send makes, named `name`.
    #[must_use]
    pub fn new_agent(name: impl Into<String>) -> Self {
        Self {
            kind: RecipientKind::NewAgent,
            id: String::new(),
            name: name.into(),
            detail: None,
        }
    }

    /// Someone or something the search found.
    #[must_use]
    pub fn found(kind: RecipientKind, id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            kind,
            id: id.into(),
            name: name.into(),
            detail: None,
        }
    }

    /// The same recipient with a line under its name.
    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        let detail = detail.into();
        self.detail = (!detail.trim().is_empty()).then_some(detail);
        self
    }
}

/// What a model may know about one change it is asked to describe: the
/// change's id as History lists it (32 lowercase hex characters), its
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

/// One change described: its id, the sentence, and which provider and model wrote it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryChangeDescription {
    pub change: String,
    pub text: String,
    pub generator: String,
}

pub const FLOATING_SURFACE_APP_ID_PREFIX: &str = "so.tilo.shell-float.";
#[cfg(unix)]
pub(crate) const MAX_PROTOCOL_FRAME_BYTES: usize = 1024 * 1024;
/// Backpressure bound for a non-reading shell peer.
#[cfg(unix)]
pub(crate) const MAX_OUTGOING_QUEUE_BYTES: usize = 4 * MAX_PROTOCOL_FRAME_BYTES;

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

impl FloatingSurfaceProtocolRole {
    /// Whether interactive content stays rendered by its owning app rather than becoming a
    /// compositor-owned capture mirror.
    #[must_use]
    pub const fn uses_app_owned_host(self) -> bool {
        match self {
            Self::ActiveCall | Self::Navigation | Self::Conversation => true,
            Self::PictureInPicture | Self::LiveActivity => false,
        }
    }
}

/// Starting mode for an app-requested session-shell chooser. The request only
/// opens trusted shell UI; it never grants an app capture permission or pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CaptureStudioProtocolMode {
    Screenshot,
    Recording,
    PictureInPicture,
}

impl From<CaptureStudioProtocolMode> for crate::widget::CaptureMode {
    fn from(mode: CaptureStudioProtocolMode) -> Self {
        match mode {
            CaptureStudioProtocolMode::Screenshot => Self::Screenshot,
            CaptureStudioProtocolMode::Recording => Self::Recording,
            CaptureStudioProtocolMode::PictureInPicture => Self::Pip,
        }
    }
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
    #[serde(default)]
    pub initial_rect: Option<[f64; 4]>,
    /// Full transparent native-window bounds, including app-drawn shadows.
    #[serde(default)]
    pub presentation_rect: Option<[f64; 4]>,
}

/// App → shell lifecycle.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FloatingSurfaceClientMessage {
    /// The first frame on every connection: the shell-ui version this app
    /// speaks ([`FLOATING_SURFACE_PROTOCOL_VERSION`]). Its shape never
    /// changes, so a shell of any version reads it.
    Hello {
        version: u16,
    },
    /// Complete primary-surface marker inventory from a successfully presented SDK frame.
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
    /// Ask the trusted session shell to show its ordinary user-confirmed chooser.
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
    /// The primary window has presented a frame with the transferred pixels removed.
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
    /// This app can stream its window's page (sent once): the shell may ask it to with
    /// [`FloatingSurfaceHostMessage::PaintKeyframe`].
    PaintCapable,
    /// Open another instance of this app as a desktop tab right after this app tab (see
    /// [`crate::shell_tabs`]).
    OpenTabBeside {
        target: Option<String>,
        background: bool,
        instance: Option<u64>,
    },
    /// Close this app tab through the desktop's tab lifecycle.
    CloseTab {
        instance: Option<u64>,
    },
    /// Reopen the workspace's most recently closed tab.
    ReopenClosedTab,
    /// Show the next (`forward`) or previous tab of the window group this app's tab is in: Ctrl+Tab or
    /// Ctrl+Shift+Tab that no widget of the app used.
    CycleTab {
        forward: bool,
        instance: Option<u64>,
    },
    /// The next window this app maps opens as a tab right after this one.
    PlaceNextWindowBeside {
        background: bool,
        instance: Option<u64>,
    },
    /// Open another window of this app as a window of the desk, floating over it where the box of this
    /// window last sent from stood ([`Self::SendTo`]), showing `anchor`: the item that send landed in.
    OpenFloated {
        /// The item the window opens on, as the resource that names it.
        anchor: String,
        instance: Option<u64>,
    },
    /// The slots this app shows in its window's bar ([`crate::window_toolbar`]), or none: sent whenever
    /// they change.
    Toolbar {
        toolbar: Option<crate::window_toolbar::WindowToolbar>,
        instance: Option<u64>,
    },
    /// Turn this app's editable location in its window's bar into a field, as a click on it does (the
    /// app's Ctrl+L).
    EditLocation {
        instance: Option<u64>,
    },
    /// What this app's window offers its bar controls: what the search acts on in place and the open
    /// search's count, and the History control ([`crate::window_toolbar::WindowOffer`]).
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
    /// Open the session's search on this app, the way the app asked ([`crate::search::SearchRequest`]):
    /// Ctrl+K, a search button, or one of its actions whose arguments the search asks for.
    OpenAppSearch {
        instance: Option<u64>,
        request: crate::search::SearchRequest,
    },
    /// Describe these changes of the resource whose History is open: the shell asks the model on this
    /// device and answers with [`FloatingSurfaceHostMessage::HistoryDescriptions`].
    DescribeHistory {
        changes: Vec<HistoryChangeFacts>,
    },
    /// This app answers [`FloatingSurfaceHostMessage::ShareSelection`] and
    /// [`FloatingSurfaceHostMessage::PointAt`] (sent once). An app that never
    /// says so is never asked.
    SelectionCapable,
    /// The person sent what the box beside this app's context menu held: a
    /// message to these recipients about these subjects. The shell makes a
    /// new agent when one recipient is one, opens or makes the one
    /// conversation they share, and posts the message there — as the person.
    /// The app's identity is the launcher's, never the message's.
    SendTo {
        to: Vec<Recipient>,
        /// What the message has besides its words, as the chips named it.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attached: Vec<tilo_reference::Attached>,
        text: String,
    },
    /// The person typed `query` into the box's recipient field: the shell answers with
    /// [`FloatingSurfaceHostMessage::Recipients`], from the one search every part of the desk shares.
    SearchRecipients {
        request: u64,
        query: String,
    },
    /// The resource this window shows (the first it publishes), sent when it changes: the desktop menus
    /// for the window (its window menu) attach it, as the app menus do.
    WindowResource {
        resource: Option<tilo_actions::ActionResource>,
    },
    /// The answer to [`FloatingSurfaceHostMessage::PointAt`] `request`: the control at that point,
    /// `None` where there is none.
    Pointed {
        request: u64,
        control: Option<PointedControl>,
    },
    /// The answer to [`FloatingSurfaceHostMessage::Pick`] `request`, read from this window's retained
    /// tree ([`crate::pick_wire::answer`]).
    PickAnswered {
        request: u64,
        answer: crate::pick_wire::PickAnswer,
    },
    /// The record this window shows, as its route names it (empty for the app record of the workspace),
    /// and the kind of resource it keeps: the desktop lists the window among those a person may share
    /// ([`crate::window_share`]).
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
    /// The windows this app shares (`crate::window_share::publish_shared`), sent whenever they change,
    /// so the desktop offers "Stop sharing" on them.
    SharedWindows {
        shared: Vec<crate::window_share::SharedWindow>,
    },
    /// The controls of this window content, as the app's accessibility
    /// tree exports them (`crate::window_targets`), at
    /// most [`crate::window_targets::MAX_TARGETS`]: what a session, a test or an
    /// agent finds them by name and presses
    /// ([`FloatingSurfaceHostMessage::PressTarget`]). Sent whenever the tree
    /// they come from changes, never per frame. `instance` names the tab, as
    /// for [`Self::Toolbar`].
    WindowTargets {
        targets: Vec<crate::window_targets::WindowTarget>,
        instance: Option<u64>,
    },
    /// The app under the pointer could not scroll sideways with this wheel
    /// event: the shell pans the window strip under it by `travel` logical
    /// pixels, positive toward later windows, by the same rules it applies
    /// to a wheel over its canvas. Sent for every event of a gesture the
    /// app left to the shell, and never for one it took, so a gesture is
    /// never split between app and strip. An app outside a session's shell
    /// has no such connection and keeps every wheel.
    SidewaysWheel {
        travel: f64,
    },
}

/// One control of an app's window as the shell names it when a person
/// points at it: its semantic id, its role and its accessible name. What a
/// field has is never part of it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PointedControl {
    pub id: String,
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// Owner-only operations on Terminal's existing device-shell PTY.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum DeviceShellOperation {
    Select,
    Inspect,
    Run { session: String, command: String },
    Read { session: String },
}

/// Sanitized host network hints.
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
    /// Someone started watching this app's window: send its page as a paint stream, starting from a
    /// whole frame.
    PaintKeyframe,
    /// Nobody watches the window any more: stop capturing the page.
    PaintStop,
    /// The person pressed the desktop's "ask an agent" chord over this app's focused window: answer
    /// with [`FloatingSurfaceClientMessage::SendTo`] for what the box shows, or nothing when it shows
    /// none. Sent only to an app that announced
    /// [`FloatingSurfaceClientMessage::SelectionCapable`].
    ///
    /// `to` is whom the box opens to when the session names it — the people here, the one place a
    /// report goes — and empty when the person names whom it goes to.
    ///
    /// `pointing` asks the window for the thing the box is about first: it lights up what lies under
    /// the pointer in this window, and the box opens about what the person clicks.
    ShareSelection {
        #[serde(default)]
        to: Vec<Recipient>,
        #[serde(default)]
        pointing: bool,
    },
    /// What the one search finds for [`FloatingSurfaceClientMessage::SearchRecipients`] `request`:
    /// agents, people and conversations to send to, the people and conversations this window is already
    /// in first.
    Recipients {
        request: u64,
        rows: Vec<Recipient>,
    },
    /// The person pointed at `point` of this app's focused window, in its logical coordinates, to say
    /// what something is about (feedback on one control): answer with
    /// [`FloatingSurfaceClientMessage::Pointed`].
    PointAt {
        request: u64,
        point: [f64; 2],
    },
    /// The person is picking parts of the screen to report a problem with them, over this app's window:
    /// answer `query` with [`FloatingSurfaceClientMessage::PickAnswered`].
    Pick {
        request: u64,
        query: crate::pick_wire::PickQuery,
    },
    /// A development session's test control activates the control whose
    /// test id is `test_id` in this app's focused window, as an assistive
    /// technology activates a control: by what it is, never by where it is
    /// drawn. Sent to every window of the focused app; only the focused one
    /// acts. Never sent outside a development session's test control.
    ActivateTestId {
        test_id: String,
    },
    /// The person used this app's slots in its window's bar
    /// ([`FloatingSurfaceClientMessage::Toolbar`]), or the bar's search or History control on its
    /// window ([`FloatingSurfaceClientMessage::WindowOffer`]).
    ToolbarEvent {
        event: crate::window_toolbar::ToolbarEvent,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        after_input: Option<u32>,
    },
    /// What the changes of a [`FloatingSurfaceClientMessage::DescribeHistory`] did, as the model on
    /// this device described them.
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
        /// App-local → desktop pointer-coordinate translation. Kept separate from clamped window
        /// geometry so a live drag can rebase without treating compositor clamping as pointer motion.
        coordinate_offset: [f64; 2],
    },
    Adopted {
        id: String,
        rect: [f64; 4],
    },
    /// The shell has hidden, but retained, the authenticated native surface.
    Suspended {
        id: String,
    },
    Reattach {
        id: String,
    },
    Closed {
        id: String,
    },
    /// Where and for whom the app's window is shown ([`crate::theme::WindowAppearance`]): the
    /// temperature of the workspace holding it and the person's colour.
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
        /// Whether the workspace the window is in has synced with its host ([`crate::workspace_sync`]);
        /// absent for a workspace that syncs with no host, and from older shells.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace_sync: Option<crate::workspace_sync::WorkspaceSync>,
        /// How round the shell draws the window's corners, in logical
        /// pixels: its window radius while the window is framed among
        /// others, 0 while it is drawn edge to edge. Absent from older
        /// shells.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        corner_radius: Option<f32>,
    },
    /// This shell draws the views windows embed of other apps' resources
    /// ([`FloatingSurfaceClientMessage::Embeds`]): sent when the app connects, so the app draws no
    /// stand-in for them.
    DrawsEmbeds,
    /// How each embed of this app's window is coming along, every one the shell draws: sent whenever
    /// one changes.
    EmbedStates {
        states: Vec<crate::window_share::KeyedEmbedState>,
    },
    /// The windows the person has open on a record, for the sharing app's picker: sent to the sharing
    /// app alone, whenever they change.
    OpenWindows {
        windows: Vec<crate::window_share::OpenWindow>,
    },
    /// A window's menu asked the sharing app to share it, or to stop sharing it.
    WindowShareAsk {
        ask: crate::window_share::Ask,
    },
    /// This shell does not answer that message: the app is told rather than
    /// left waiting on an answer that never came. `message` names the
    /// variant it sent, `reason` says what the session cannot do of it.
    Unanswered {
        message: String,
        reason: String,
    },
    /// The person, an agent or a test pressed the control `name` of `role` in this window's content,
    /// which the app described in [`FloatingSurfaceClientMessage::WindowTargets`].
    PressTarget {
        /// The control's name, in the person's language.
        name: String,
        /// Its role as the listing named it (`Button`, `Document`).
        role: String,
        /// What it does, when the shell names it by that instead of its name: the identity a window's
        /// app publishes for a control that runs its action or edits its record.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        identity: Option<String>,
        action: crate::window_targets::TargetAction,
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
            workspace_sync: None,
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

    /// What an [`Self::Appearance`] message says, `None` for any other.
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
