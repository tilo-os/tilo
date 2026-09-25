use crate::SharedSurfaces;
use crate::identity::next_id;
use collab_types::PermissionProfile;
use collab_types::caps::CapOverride;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Monotonic workspace identifier, newtype-wrapped over `u64`.
#[derive(
    Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord,
)]
#[serde(transparent)]
pub struct WorkspaceId(pub u64);

impl WorkspaceId {
    /// Allocate a fresh, process-wide unique id.
    pub fn new() -> Self {
        Self(next_id())
    }

    pub fn raw(&self) -> u64 {
        self.0
    }
}

/// Durable, globally-unique identity of a workspace as a collaboration
/// room. Unlike [`WorkspaceId`] (a local counter) it is minted once as a
/// random v4 uuid and carried rather than recomputed, so machines that
/// have never met still agree on it. [`Self::nil`] is the unminted
/// sentinel a pre-room state file loads as.
#[derive(
    Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord,
)]
#[serde(transparent)]
pub struct RoomId(uuid::Uuid);

impl RoomId {
    /// Mint a fresh room identity, once per workspace at creation.
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }

    /// The "no identity yet" sentinel. Distinguishable from every
    /// minted id because `Uuid::new_v4` never produces the nil uuid.
    pub const fn nil() -> Self {
        Self(uuid::Uuid::nil())
    }

    /// Whether this is the unminted sentinel.
    pub fn is_nil(&self) -> bool {
        self.0.is_nil()
    }

    /// The underlying uuid, for the wire formats that speak `Uuid`.
    pub fn as_uuid(&self) -> uuid::Uuid {
        self.0
    }

    /// Parse a room id off the wire (invite token, automerge doc,
    /// `QOS_WORKSPACE_ID`). `None` on anything that isn't a valid uuid.
    pub fn parse(s: &str) -> Option<Self> {
        uuid::Uuid::parse_str(s).ok().map(Self)
    }
}

impl std::fmt::Display for RoomId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<uuid::Uuid> for RoomId {
    fn from(v: uuid::Uuid) -> Self {
        Self(v)
    }
}

impl From<u64> for WorkspaceId {
    fn from(v: u64) -> Self {
        Self(v)
    }
}

impl From<WorkspaceId> for u64 {
    fn from(v: WorkspaceId) -> Self {
        v.0
    }
}

/// Monotonic strip identifier, newtype-wrapped like [`WorkspaceId`].
#[derive(
    Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord,
)]
#[serde(transparent)]
pub struct StripId(pub u64);

impl StripId {
    /// Allocate a fresh, process-wide unique id.
    pub fn new() -> Self {
        Self(next_id())
    }

    pub fn raw(&self) -> u64 {
        self.0
    }
}

impl From<u64> for StripId {
    fn from(v: u64) -> Self {
        Self(v)
    }
}

impl From<StripId> for u64 {
    fn from(v: StripId) -> Self {
        v.0
    }
}

pub type GroupId = u64;
pub type TabId = u64;
pub type FileId = u64;

/// Snapshot of the focused tab, cloned out of the [`WorkspaceTree`] read
/// guard so callers can drop the guard before using the values.
#[derive(Clone, Debug)]
pub struct TabRef {
    pub id: TabId,
    pub title: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum AppKind {
    Terminal,
    FileExplorer,
    Browser,
    Settings,
    Other(String),
}

impl AppKind {
    pub fn as_str(&self) -> &str {
        match self {
            AppKind::Terminal => "Terminal",
            AppKind::FileExplorer => "Files",
            AppKind::Browser => "Browser",
            AppKind::Settings => "Settings",
            AppKind::Other(s) => s,
        }
    }

    /// The key into the per-workspace [`Workspace::grants`] map — distinct
    /// from [`Self::as_str`]'s display label. Foreign clients carry their
    /// real `xdg_toplevel::set_app_id` (`org.gimp.GIMP`, …) straight in;
    /// built-ins get namespaced under the compositor's reverse-DNS prefix
    /// so every grant key lives in one namespace.
    pub fn app_id(&self) -> AppId {
        match self {
            AppKind::Terminal => format!("{APP_ID_PREFIX}.Terminal"),
            AppKind::FileExplorer => format!("{APP_ID_PREFIX}.Files"),
            AppKind::Browser => format!("{APP_ID_PREFIX}.Browser"),
            AppKind::Settings => format!("{APP_ID_PREFIX}.Settings"),
            AppKind::Other(s) if s.contains('.') => s.clone(),
            AppKind::Other(s) => format!("{APP_ID_PREFIX}.{s}"),
        }
    }
}

/// Reverse-DNS prefix the compositor stamps on built-in apps' ids,
/// matching its own portal backend name
/// (`org.freedesktop.impl.portal.desktop.nirios`).
pub const APP_ID_PREFIX: &str = "org.nirios";

/// Reference width the legacy pixel model was scaled against, so a
/// migrated `Custom(600)` preserves the *proportion* the user saw rather
/// than snapping to whichever tier is now "Normal".
const LEGACY_PIXEL_REFERENCE_WIDTH: u32 = 1200;

/// Persisted width of a [`WindowGroup`], as a fraction of the strip's
/// usable width — a stored pixel count would mean a different screen
/// share on every monitor. Kept separate from [`ui::widget::WindowWidth`]
/// (serde/CRDT shape vs. layout shape).
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
pub enum WindowWidth {
    Narrow,
    Normal,
    Wide,
    /// An arbitrary fraction of the strip's usable width, in per-mille.
    /// What a manual resize persists.
    Fraction(u16),
}

impl WindowWidth {
    /// The fraction this width represents, in per-mille of the strip's
    /// usable width. Delegates to the toolkit so the shell's persisted
    /// tiers and the laid-out ones cannot drift apart.
    pub fn permille(&self) -> u16 {
        self.to_ui().permille()
    }

    /// Resolve to logical pixels against a strip of `strip_usable_px`.
    pub fn resolve(&self, strip_usable_px: f64) -> f64 {
        self.to_ui().resolve(strip_usable_px)
    }

    /// The toolkit width this persisted width maps to.
    pub fn to_ui(self) -> ui::widget::WindowWidth {
        match self {
            WindowWidth::Narrow => ui::widget::WindowWidth::Narrow,
            WindowWidth::Normal => ui::widget::WindowWidth::Normal,
            WindowWidth::Wide => ui::widget::WindowWidth::Wide,
            WindowWidth::Fraction(p) => ui::widget::WindowWidth::Fraction(p),
        }
    }

    /// The persisted width for a toolkit width.
    pub fn from_ui(w: ui::widget::WindowWidth) -> Self {
        match w {
            ui::widget::WindowWidth::Narrow => WindowWidth::Narrow,
            ui::widget::WindowWidth::Normal => WindowWidth::Normal,
            ui::widget::WindowWidth::Wide => WindowWidth::Wide,
            ui::widget::WindowWidth::Fraction(p) => WindowWidth::Fraction(p),
        }
    }

    /// Migrate a width persisted by the pixel model — see
    /// [`LEGACY_PIXEL_REFERENCE_WIDTH`].
    pub fn from_legacy_pixels(px: u32) -> Self {
        WindowWidth::Fraction(ui::widget::permille_for_px(
            f64::from(px),
            f64::from(LEGACY_PIXEL_REFERENCE_WIDTH),
        ))
    }
}

impl<'de> Deserialize<'de> for WindowWidth {
    /// Accepts both the current shape and the legacy pixel shape, so an
    /// existing `~/.local/state` workspace still loads.
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        enum Raw {
            Narrow,
            Normal,
            Wide,
            Fraction(u16),
            /// Legacy: an absolute pixel count.
            Custom(u32),
        }
        Ok(match Raw::deserialize(d)? {
            Raw::Narrow => WindowWidth::Narrow,
            Raw::Normal => WindowWidth::Normal,
            Raw::Wide => WindowWidth::Wide,
            Raw::Fraction(p) => WindowWidth::Fraction(p.clamp(
                ui::widget::WIDTH_MIN_PERMILLE,
                ui::widget::WIDTH_MAX_PERMILLE,
            )),
            Raw::Custom(px) => WindowWidth::from_legacy_pixels(px),
        })
    }
}

/// What a tile holds. Persistence only carries `Stub` and a symbolic
/// `AppPlaceholder` for native apps, whose concrete widgets are
/// constructed at shell boot.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum TileContent {
    AppPlaceholder {
        kind: AppKind,
        label: String,
    },
    Stub {
        kind: AppKind,
        label: String,
    },
    /// Foreign Wayland client adopted by the compositor. `surface_id` is
    /// the `wl_surface` protocol id the compositor maps back to a smithay
    /// `WlSurface`; renders as a Stub until the frame loop blits it.
    Foreign {
        app_id: String,
        surface_id: u64,
    },
    /// Durable local application window awaiting a fresh compositor surface.
    /// Unlike `Foreign`, this stores no process-local Wayland id. At session
    /// startup the normal launcher opens `app_id` and binds its first
    /// toplevel back into this exact tab, preserving layout without reviving
    /// a dead surface handle.
    LocalApp {
        app_id: String,
        label: String,
        #[serde(default)]
        note: String,
    },
    /// A window a peer has open, before this machine has bound a local
    /// instance to the tab. The peer's `surface_id` only resolves on
    /// their compositor so it is dropped, keeping only the app id. A
    /// waypoint: `shell::peer_windows` binds a local instance and the
    /// tile becomes `Foreign`. Never projected into a room document,
    /// since a `PeerWindow` is by construction somebody else's.
    PeerWindow {
        /// The wayland app-id the peer's window announces.
        app_id: String,
        /// The peer's own resolved tab title.
        label: String,
        /// What this machine is doing about it, for the card. Empty means
        /// nobody has looked yet; writing the note is how
        /// `shell::peer_windows` records that the decision was made, so a
        /// refusal or launch only ever starts once.
        #[serde(default)]
        note: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Tab {
    pub id: TabId,
    pub title: String,
    pub tile: TileContent,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WindowGroup {
    pub id: GroupId,
    pub tabs: Vec<Tab>,
    pub active_tab: TabId,
    pub width: WindowWidth,
    /// Mirrors the widget's maximized flag. This is presentation state for
    /// this device: it survives a local restart but is never sent to room
    /// peers. `#[serde(default)]` keeps pre-existing local JSON loading
    /// un-maximized.
    #[serde(default)]
    pub maximized: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Strip {
    pub id: StripId,
    pub name: String,
    pub window_groups: Vec<WindowGroup>,
}

impl Strip {
    pub fn new(id: StripId, name: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
            window_groups: Vec::new(),
        }
    }
}

/// [`put_str_if_changed`] for a boolean.
#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct Collaborator {
    pub id: String,
    pub name: String,
    /// Tab id the collaborator is focused on, driving per-tab presence
    /// dots. `None` means present in the workspace but not on content.
    #[serde(default)]
    pub active_tab: Option<u64>,
    /// Optional `#rrggbb`/`#rrggbbaa` override for the palette
    /// [`ui::presence::user_color`] would otherwise pick.
    #[serde(default)]
    pub cursor_color: Option<String>,
    /// Optional avatar URL or initials fragment; not yet wired to a
    /// bitmap loader (the runtime renders initials today).
    #[serde(default)]
    pub avatar: Option<String>,
    /// Display-only role chip. Authority comes from
    /// [`Workspace::role_of`], never from this field.
    #[serde(default)]
    pub role_id: Option<String>,
    /// Named by the room but has not taken part yet.
    #[serde(default)]
    pub invited: bool,
    /// The publishing peer's Iroh handle, so a departure (which names
    /// only a peer id) finds this row again after it has been renamed
    /// from the handle to the person. `None` for a persisted row.
    #[serde(default)]
    pub session: Option<u64>,
}

impl Collaborator {
    /// Build a collaborator with no tab attribution.
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            active_tab: None,
            cursor_color: None,
            avatar: None,
            role_id: None,
            invited: false,
            session: None,
        }
    }

    /// Record the live session this person is here through.
    #[must_use]
    pub fn with_session(mut self, peer: u64) -> Self {
        self.session = Some(peer);
        self
    }

    /// Like [`Collaborator::new`] but attributed to a specific tab.
    #[must_use]
    pub fn with_active_tab(id: impl Into<String>, name: impl Into<String>, tab: u64) -> Self {
        let mut c = Self::new(id, name);
        c.active_tab = Some(tab);
        c
    }

    /// Attach an explicit `#rrggbb` / `#rrggbbaa` override for this peer.
    #[must_use]
    pub fn with_cursor_color(mut self, hex: impl Into<String>) -> Self {
        self.cursor_color = Some(hex.into());
        self
    }

    /// Attach an avatar reference (URL or initials fragment).
    #[must_use]
    pub fn with_avatar(mut self, avatar: impl Into<String>) -> Self {
        self.avatar = Some(avatar.into());
        self
    }

    /// Attach a role id pointing into the workspace's role table.
    #[must_use]
    pub fn with_role(mut self, role_id: impl Into<String>) -> Self {
        self.role_id = Some(role_id.into());
        self
    }

    /// Mark this collaborator as invited rather than arrived.
    #[must_use]
    pub fn as_invited(mut self) -> Self {
        self.invited = true;
        self
    }
}

pub type AppId = String;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceTaskStatus {
    Queued,
    InProgress,
    Waiting,
    Done,
    Cancelled,
}

impl WorkspaceTaskStatus {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Queued => "Queued",
            Self::InProgress => "In progress",
            Self::Waiting => "Waiting",
            Self::Done => "Done",
            Self::Cancelled => "Cancelled",
        }
    }

    pub fn is_finished(&self) -> bool {
        matches!(self, Self::Done | Self::Cancelled)
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceTaskRoute {
    /// A person advances the task from the native task surface. No model is
    /// contacted, which keeps Task Center useful on an offline desktop.
    Manual,
    /// An agent may use exported actions and deterministic matching only.
    RulesOnly,
    /// An agent may ask its configured model before dispatching exported
    /// actions. The resulting action trace remains the source of truth.
    Model,
}

impl WorkspaceTaskRoute {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Manual => "Manual · no model",
            Self::RulesOnly => "Agent · rules only",
            Self::Model => "Agent · model assisted",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceTaskResource {
    pub app_id: Option<String>,
    pub app_label: Option<String>,
    pub id: String,
    pub kind: String,
    pub label: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceTaskAuditEvent {
    pub at: i64,
    pub actor: String,
    pub summary: String,
}

/// Append-only, broker-authenticated record of an external side effect.
/// Arguments, output values, produced resource addresses, failure messages,
/// and failure details are deliberately absent from this durable shape.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceEffectReceipt {
    pub invocation: tilo_actions::InvocationId,
    pub actor: String,
    pub context: tilo_actions::ActionContext,
    pub status: tilo_actions::EffectReceiptStatus,
    pub completed_at: i64,
}

/// A durable unit of work in a collaboration room.
///
/// Tasks are deliberately shell-owned rather than chat messages: every app
/// can attach a resource, people can run them without a language model, and
/// the workspace room CRDT carries the same record to every desktop.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceTask {
    pub id: String,
    pub title: String,
    pub created_by: String,
    pub assigned_agent: Option<String>,
    pub route: WorkspaceTaskRoute,
    pub status: WorkspaceTaskStatus,
    pub resource: Option<WorkspaceTaskResource>,
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(default)]
    pub audit: Vec<WorkspaceTaskAuditEvent>,
}

impl WorkspaceTask {
    pub fn action_context(&self) -> tilo_actions::ActionContext {
        let mut context = tilo_actions::ActionContext::default()
            .with_purpose(self.title.clone())
            .with_group(self.id.clone(), self.title.clone(), "task");
        if let Some(resource) = &self.resource {
            context.resource = Some(tilo_actions::ActionResource {
                id: resource.id.clone(),
                kind: resource.kind.clone(),
                label: resource.label.clone(),
            });
            if let (Some(id), Some(label)) = (&resource.app_id, &resource.app_label) {
                context = context.with_app(id.clone(), label.clone());
            }
        }
        context
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Workspace {
    pub id: WorkspaceId,
    /// Durable identity of this workspace as a collaboration room — the
    /// value two hosts must agree on, unlike the local-only [`Self::id`].
    /// `#[serde(default)]` yields [`RoomId::nil`] for pre-room state
    /// files until `adopt_roomless_workspaces` mints a real one at boot.
    #[serde(default)]
    pub room: RoomId,
    pub name: String,
    pub strips: Vec<Strip>,
    /// Who is in this workspace *right now*, asserted present by a
    /// verified invite or presence frame — not durable membership, which
    /// is [`Workspace::owner`] / [`Workspace::capabilities`].
    /// `skip_deserializing` rather than `skip`: reading is the direction
    /// that can fabricate a stale person, but `Serialize` still feeds the
    /// control socket's state dump.
    #[serde(skip_deserializing)]
    pub collaborators: Vec<Collaborator>,
    /// Who owns this room — the one principal whose authority is not
    /// itself a capability. `None` means a pre-ownership state file,
    /// adopted by the local user on first load.
    #[serde(default)]
    pub owner: Option<UserId>,
    /// One exact, owner-signed offer awaiting the named recipient's consent.
    /// Persisted with the workspace so restart cannot erase, extend, or retarget
    /// the ceremony.
    #[serde(default)]
    pub pending_ownership_transfer: Option<collab_types::OwnershipTransferProposalV1>,
    /// Completed transfers retaining the outgoing owner's signed authorization
    /// and the exact recipient's signed acceptance. This is append-only audit
    /// evidence, not a second source of current ownership.
    #[serde(default)]
    pub ownership_transfer_audit: Vec<collab_types::OwnershipTransferAuditV1>,
    /// Owner-signed cancellations retained so a cancelled proposal cannot be
    /// replayed after restart.
    #[serde(default)]
    pub ownership_transfer_cancellations: Vec<collab_types::OwnershipTransferCancellationV1>,
    /// **Every** permission in this room, as capabilities — one list, one
    /// vocabulary (room membership, file shares, portal grants), so it
    /// can't drift the way a second per-app map would.
    #[serde(default)]
    pub capabilities: Vec<CapOverride>,
    /// Reusable least-privilege bundles. Assignments still materialize into
    /// `capabilities`, which remains the enforcement vocabulary.
    #[serde(default)]
    pub permission_profiles: Vec<PermissionProfile>,
    /// Default-application policy belongs to the workspace, while each
    /// person's ordinary defaults remain in their personal settings. All
    /// mutation goes through the authority-checked methods below.
    #[serde(default)]
    pub default_apps: desktop_policy::WorkspaceDefaults,
    /// Work shared by the room independently of any one chat thread or app.
    /// Values are projected as a CRDT map keyed by task id.
    #[serde(default)]
    pub tasks: Vec<WorkspaceTask>,
    /// Consequential effects are activity, but never pretend to be edits to
    /// an app's collaborative document.
    #[serde(default)]
    pub effect_receipts: Vec<WorkspaceEffectReceipt>,
    /// Surfaces from this workspace being streamed to peers. Keys are
    /// wayland `wl_surface` ids; values are the target Iroh peers
    /// (hashed to `u64`) — a `HashSet` for multi-recipient streaming.
    #[serde(default)]
    pub shared_surfaces: SharedSurfaces,
}

/// Stable identifier for a person. The compositor's collaborator table
/// keys on this; a peer's Ed25519 key hex is a valid value, and so is
/// the local user's account name.
pub type UserId = String;

impl Workspace {
    pub fn new(id: WorkspaceId, name: impl Into<String>) -> Self {
        Self {
            id,
            // A workspace that exists is a room that exists: minted
            // here rather than lazily.
            room: RoomId::new(),
            name: name.into(),
            strips: Vec::new(),
            collaborators: Vec::new(),
            owner: None,
            pending_ownership_transfer: None,
            ownership_transfer_audit: Vec::new(),
            ownership_transfer_cancellations: Vec::new(),
            capabilities: Vec::new(),
            permission_profiles: Vec::new(),
            default_apps: desktop_policy::WorkspaceDefaults::default(),
            tasks: Vec::new(),
            effect_receipts: Vec::new(),
            shared_surfaces: SharedSurfaces::default(),
        }
    }

    /// What fixtures want instead of a hand-written `Workspace { … }`
    /// literal (which breaks on every new field); each fixture also gets
    /// its own room rather than sharing the nil one.
    pub fn test_default(id: WorkspaceId, name: impl Into<String>) -> Self {
        Self::new(id, name)
    }

    /// This workspace's collaboration cluster id — [`Self::room`], not
    /// the local [`Self::id`]. Unminted returns the nil uuid, which
    /// `CrdtRoute::delivers_to` refuses to deliver to.
    pub fn collab_cluster_id(&self) -> uuid::Uuid {
        self.room.as_uuid()
    }

    /// Mark `surface_id` as being streamed to `peer_id`. Additive — two
    /// calls record both peers in the per-surface set.
    pub fn share_surface(&mut self, surface_id: u64, peer_id: u64) {
        self.shared_surfaces.share(surface_id, peer_id);
    }

    /// Drop the share entry for `surface_id` (no-op if unshared).
    /// Returns the peers it was shared with, so callers can fire
    /// `StopShare` for each without a second lookup.
    pub fn unshare_surface(&mut self, surface_id: u64) -> Option<std::collections::HashSet<u64>> {
        self.shared_surfaces.unshare(surface_id)
    }

    /// Stop streaming `surface_id` to one peer, leaving others intact.
    /// Drops the surface key entirely once the last peer is removed, so
    /// [`Self::is_surface_shared`] reports `false` again.
    pub fn unshare_surface_from_peer(&mut self, surface_id: u64, peer_id: u64) -> bool {
        self.shared_surfaces.unshare_peer(surface_id, peer_id)
    }

    /// Whether `surface_id` is currently streamed somewhere. Used by
    /// the compositor's commit hook to short-circuit the readback for
    /// unshared surfaces (~all of them in steady state).
    pub fn is_surface_shared(&self, surface_id: u64) -> bool {
        self.shared_surfaces.contains(surface_id)
    }
}

impl Default for Workspace {
    /// Room is minted, not left at [`RoomId::nil`] — two workspaces
    /// built this way must not silently become the same room. Nil is
    /// reachable only via deserializing a pre-rooms state file.
    fn default() -> Self {
        Self {
            id: WorkspaceId::default(),
            room: RoomId::new(),
            name: String::new(),
            strips: Vec::new(),
            collaborators: Vec::new(),
            owner: None,
            pending_ownership_transfer: None,
            ownership_transfer_audit: Vec::new(),
            ownership_transfer_cancellations: Vec::new(),
            capabilities: Vec::new(),
            permission_profiles: Vec::new(),
            default_apps: desktop_policy::WorkspaceDefaults::default(),
            tasks: Vec::new(),
            effect_receipts: Vec::new(),
            shared_surfaces: SharedSurfaces::default(),
        }
    }
}

/// The shape of `state.json` this build writes and understands. Bump
/// only when a change makes a file unreadable to an older build in a way
/// that loses meaning (field removed/repurposed, id space renumbered) —
/// a new `#[serde(default)]` field does not qualify. Guards the one
/// direction with no safe default: older reading newer.
pub const STATE_SCHEMA_VERSION: u32 = 1;

/// A file with no `schema_version` key is version 1 — the key was added
/// alongside the check, not a format change, and defaulting to "unknown"
/// would strand every existing session on upgrade.
fn state_schema_version_of_a_file_without_one() -> u32 {
    1
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkspaceTree {
    #[serde(default = "state_schema_version_of_a_file_without_one")]
    pub schema_version: u32,
    #[serde(skip)]
    pub room_epoch: u64,
    /// Mutations made outside retained chrome increment this process-local
    /// epoch so hosts invalidate their cached desktop exactly once.
    #[serde(skip)]
    pub external_revision: u64,
    pub workspaces: Vec<Workspace>,
    /// Peer-local workspace selection; never room topology.
    #[serde(default)]
    pub active: WorkspaceId,
    #[serde(deserialize_with = "crate::persistence::deserialize_id_map")]
    pub active_strip: HashMap<WorkspaceId, StripId>,
    /// Peer-local focus; never room topology.
    #[serde(default, deserialize_with = "crate::persistence::deserialize_id_map")]
    pub active_group: HashMap<StripId, GroupId>,
    #[serde(skip_deserializing)]
    pub collaborators: Vec<Collaborator>,
    /// Mirror of each room's record index (record id → kind), pushed by
    /// [`Self::set_room_records`] — a plain-data copy since the real
    /// index lives behind an `Rc<RefCell<_>>` peer-sync's
    /// `Arc<RwLock<_>>` can't reach. Not persisted.
    #[serde(skip, default)]
    pub room_records_held: HashMap<RoomId, std::collections::BTreeMap<String, String>>,
    /// References that arrived from a peer and have not been folded into
    /// the local index yet. Drained by the shell — see
    /// [`Self::take_inbound_room_records`].
    #[serde(skip, default)]
    pub room_records_inbound: Vec<(RoomId, String, String)>,
    /// This tree's own peer salt, overriding the process-wide one — the
    /// static is wrong when one process holds **two** trees (e.g. two
    /// simulated peers). [`Self::local_salt`] prefers this field;
    /// production never sets it.
    #[serde(skip, default)]
    pub peer_salt_override: Option<u16>,
}

impl Default for WorkspaceTree {
    fn default() -> Self {
        Self {
            schema_version: STATE_SCHEMA_VERSION,
            room_epoch: 0,
            external_revision: 0,
            workspaces: Vec::new(),
            active: WorkspaceId::default(),
            active_strip: HashMap::new(),
            active_group: HashMap::new(),
            collaborators: Vec::new(),
            room_records_held: HashMap::new(),
            room_records_inbound: Vec::new(),
            peer_salt_override: None,
        }
    }
}

pub type WorkspaceTreeHandle = std::sync::Arc<parking_lot::RwLock<WorkspaceTree>>;
impl WorkspaceTree {
    pub fn shared(self) -> WorkspaceTreeHandle {
        std::sync::Arc::new(parking_lot::RwLock::new(self))
    }

    /// Move the active workspace's focused strip one step in `dir`, clamping
    /// at both ends. Returns the resulting index, or `None` when the active
    /// workspace has no strips.
    pub fn move_active_strip(&mut self, dir: i32) -> Option<usize> {
        let workspace = self
            .workspaces
            .iter()
            .find(|workspace| workspace.id == self.active)?;
        let last = workspace.strips.len().checked_sub(1)?;
        let current = self
            .active_strip
            .get(&self.active)
            .and_then(|id| workspace.strips.iter().position(|strip| strip.id == *id))
            .unwrap_or(0)
            .min(last);
        let next = match dir.signum() {
            -1 => current.saturating_sub(1),
            1 => current.saturating_add(1).min(last),
            _ => current,
        };
        self.active_strip
            .insert(self.active, workspace.strips.get(next)?.id);
        Some(next)
    }
}
