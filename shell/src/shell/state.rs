use automerge::{AutoCommit, ChangeHash};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub use collab_types::caps::{
    Actor, CapDecision, CapOverride, Effect, Expiry, Principal, ResourceRef, Scope, Subject, Verb,
    decide_capability,
};
pub use collab_types::role::Role as CapRole;

/// Monotonic workspace identifier. Wrapped as a newtype so call-sites can
/// mint fresh ids via `WorkspaceId::new()`; integer literals still flow in
/// ergonomically through `From<u64>`.
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

/// Durable, globally-unique identity of a workspace **as a
/// collaboration room**.
///
/// [`WorkspaceId`] is a *local handle*: a counter that indexes this
/// process's tree. Two machines that each have a "workspace 7" have two
/// unrelated workspaces, and the *same* shared workspace generally
/// carries a different local id on each of them. Deriving the
/// collaboration cluster from that counter therefore got both halves
/// wrong at once — it addressed two strangers' workspaces as one room,
/// and it split one shared room into two.
///
/// A `RoomId` is a random v4 uuid minted once, when the workspace is
/// created. Random rather than derived: the identity has to be unique
/// between machines that have never met and have no channel to
/// negotiate over, and every derivation available at mint time (local
/// counter, hostname, user name, wall clock) is either not unique or
/// needs the coordination we are trying to avoid. 122 random bits put
/// the collision probability far below every other failure mode in the
/// stack, and the cost is that the value must be *carried* rather than
/// recomputed — which is what [`Workspace::room`] persisting it and the
/// invite token shipping it are for.
///
/// [`Self::nil`] is the "not minted yet" sentinel. It is what a
/// workspace deserialised from a state file written before rooms
/// existed comes back with, and
/// [`WorkspaceTree::adopt_roomless_workspaces`] is what replaces it
/// with a real identity at boot. A nil room addresses nobody — see
/// [`Workspace::collab_cluster_id`].
#[derive(
    Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord,
)]
#[serde(transparent)]
pub struct RoomId(uuid::Uuid);

impl RoomId {
    /// Mint a fresh room identity. Called exactly once per workspace,
    /// at creation; every later reference carries the minted value.
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

    /// Parse a room id off the wire (an invite token, the automerge
    /// doc, `QOS_WORKSPACE_ID`). `None` on anything that is not a
    /// uuid, so a malformed value cannot become a room nobody else is
    /// in.
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

/// Monotonic strip identifier. Mirrors [`WorkspaceId`] so callers can mint
/// fresh ids via `StripId::new()` without threading a counter through; literal
/// integers still flow in through `From<u64>` for tests and persistence.
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

/// Lightweight snapshot of the currently focused tab. Cloned out of the
/// [`WorkspaceTree`] read guard so callers can drop the guard before
/// looking at the values — see [`crate::shell::chrome_state::ChromeState::active_tab_id`].
#[derive(Clone, Debug)]
pub struct TabRef {
    pub id: TabId,
    pub title: String,
}

use std::sync::atomic::{AtomicU16, AtomicU64, Ordering as AtomicOrdering};

/// Process-wide monotonic counter shared by [`WorkspaceId::new`],
/// [`StripId::new`], and ad-hoc `next_id()` consumers (group / tab /
/// file ids).
///
/// Hoisted out of `next_id()` so [`WorkspaceTree::seed_next_id_from_self`]
/// can advance it past the highest persisted id at load time — without
/// that seeding, a fresh process restarts the counter at `1` and the
/// next allocation collides with the lowest-id entry already in the
/// loaded cache (D-9).
///
/// C-1: only the **low 48 bits** of each minted id come from this
/// counter; the high 16 bits are stamped with [`NEXT_ID_PEER_SALT`] so
/// two peers running in parallel can never mint the same id. Without
/// the salt, both peers' counters tick `1, 2, 3, …` from `1`, then
/// `apply_changes` folds the two distinct workspaces with id `2` into a
/// single row (strips collide, names overwrite, grants merge).
pub(crate) static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Process-wide peer salt occupying the **high 16 bits** of every id
/// minted by [`next_id`]. Default `0` keeps tests and pre-peer-sync
/// boots producing the familiar `0x0001, 0x0002, …` sequence; the
/// hosted entry point overwrites this at startup with a deterministic
/// hash of the local libp2p public key (see
/// [`crate::network::behaviour::peer_salt_from_keypair`]).
///
/// `AtomicU16` because the salt is a single 16-bit field and writes
/// happen exactly once at boot. Reads happen on every id allocation —
/// `Relaxed` is enough (no other state piggy-backs on the salt's
/// visibility).
pub(crate) static NEXT_ID_PEER_SALT: AtomicU16 = AtomicU16::new(0);

/// Mask that selects the low-48-bit counter portion of a salted id.
pub(crate) const PEER_ID_COUNTER_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;

/// Shift amount that lifts the 16-bit peer salt into the high bits.
pub(crate) const PEER_ID_SALT_SHIFT: u32 = 48;

pub(crate) fn next_id() -> u64 {
    let counter = NEXT_ID.fetch_add(1, AtomicOrdering::Relaxed);
    let counter = counter & PEER_ID_COUNTER_MASK;
    let salt = NEXT_ID_PEER_SALT.load(AtomicOrdering::Relaxed) as u64;
    (salt << PEER_ID_SALT_SHIFT) | counter
}

/// Extract the 16-bit peer salt encoded in the high bits of a salted id.
pub(crate) fn peer_salt_of(id: u64) -> u16 {
    (id >> PEER_ID_SALT_SHIFT) as u16
}

/// Extract the low-48-bit counter component of a salted id.
pub(crate) fn counter_of(id: u64) -> u64 {
    id & PEER_ID_COUNTER_MASK
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
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum WindowWidth {
    Narrow,
    Normal,
    Wide,
    Custom(u32),
}

impl WindowWidth {
    pub fn pixels(&self) -> u32 {
        match self {
            WindowWidth::Narrow => 400,
            WindowWidth::Normal => 600,
            WindowWidth::Wide => 800,
            WindowWidth::Custom(p) => (*p).clamp(300, 1200),
        }
    }
}

/// Interim tile content for v1. Plan 3 replaces `App` with a live `Box<dyn AppWidget>`
/// handle; v1 JSON persistence only carries Stub and a symbolic `AppPlaceholder` for
/// native apps whose concrete widgets are constructed at shell boot.
///
/// Phase 6 of `plans/2026-05-02-real-wayland-compositor.md` adds
/// [`TileContent::Foreign`] for adopted xdg_toplevel clients.
#[derive(Clone, Debug, Serialize, Deserialize)]
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
    /// the wayland-server `wl_surface` numeric protocol id; the
    /// compositor's state.rs maps it back to a smithay `WlSurface` for
    /// rendering. The chrome-side render falls through to a Stub
    /// placeholder until the frame loop blits the real buffer (Phase 4
    /// follow-up).
    Foreign {
        app_id: String,
        surface_id: u64,
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
    /// Whether the user has toggled this group into the concept's
    /// "maximized" mode. Mirrors `WindowGroup::maximized` on the widget
    /// (see `ui/src/widget/window_group.rs`) — stored here so the
    /// state survives a shell restart and replicates across collaborators.
    /// Audit task #105.
    ///
    /// `#[serde(default)]` (defaults to `false`) so persisted JSON written
    /// before this field existed still loads — old workspaces simply boot
    /// un-maximized, matching prior runtime behaviour.
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
    /// Build an empty strip with the given id and name. Window groups start
    /// empty; callers (the compositor or persistence layer) populate them.
    pub fn new(id: StripId, name: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
            window_groups: Vec::new(),
        }
    }
}

/// **Displaced vocabulary — presentation only, enforces nothing.**
///
/// [`Role`], [`RolePermissions`], [`AppPerm`], [`FilePerm`] and
/// [`WorkspaceFile`] are the compositor's original permission model.
/// They are superseded by [`Workspace::capabilities`] and are retained
/// solely because the Workspace Settings "Roles" tab still renders
/// tables over them.
///
/// Nothing in this vocabulary is consulted by any enforcement point.
/// No decision anywhere reads `RolePermissions`; `Workspace::roles` and
/// `Workspace::workspace_files` are `vec![]` in every production
/// construction, so the tab is a UI over two empty vectors.
///
/// The replacements, all in the one model:
/// - a member's role → the verb on their membership capability,
///   [`Workspace::role_of`]
/// - `per_app` → a capability scoped to [`Scope::App`]
/// - `per_file` → a capability scoped to [`Scope::FsPath`], which
///   inherits down the tree and is enforced by both the compositor and
///   the filesystem
///
/// Do not add call sites. Porting the Roles tab onto
/// [`Workspace::roster`] + [`Workspace::shares_of`] retires these
/// types entirely.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Role {
    pub id: String,
    pub name: String,
    pub permissions: RolePermissions,
}

/// Displaced vocabulary — see [`Role`]. Read by no enforcement point.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RolePermissions {
    pub can_invite: bool,
    pub can_edit_files: bool,
    /// Keyed by `AppKind::as_str()` (serde-friendly string key).
    pub per_app: HashMap<String, AppPerm>,
    /// Keyed by `WorkspaceFile.id`.
    pub per_file: HashMap<String, FilePerm>,
}

/// Displaced vocabulary — see [`Role`]. Superseded by a capability
/// scoped to [`Scope::App`].
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum AppPerm {
    #[default]
    None,
    View,
    Edit,
    Admin,
}

/// Displaced vocabulary — see [`Role`]. Superseded by a capability
/// scoped to [`Scope::FsPath`], which inherits down the tree and is
/// enforced by the compositor and the filesystem alike.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum FilePerm {
    #[default]
    None,
    Read,
    Write,
}

/// Displaced vocabulary — see [`Role`]. `Workspace::workspace_files`
/// is `vec![]` in every production construction.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkspaceFile {
    pub id: String,
    pub name: String,
    pub mime: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct Collaborator {
    pub id: String,
    pub name: String,
    /// Tab id the collaborator is currently focused on, if any. Drives
    /// per-tab presence dots (`TabBar` reads `PresenceKey::Tab{ hex(id) }`
    /// from `PresenceCtx`; the shell seeds that mapping from this field).
    /// `None` means "not attributed to a specific tab" — e.g. the peer is
    /// in the workspace chrome but hasn't focused content yet. Optional
    /// via `#[serde(default)]` so pre-existing persisted workspaces (with
    /// no `active_tab` column) still load.
    #[serde(default)]
    pub active_tab: Option<u64>,
    /// Optional hex string (`#rrggbb` or `#rrggbbaa`) overriding the
    /// palette that [`ui::presence::user_color`] would pick for
    /// this peer. Mirrors `nirios-concept/src/data.ts`'s per-peer
    /// `cursorColor`. `None` falls through to the palette — the common
    /// case for the four canonical ids (`me`/`alice`/`bob`/`charlie`).
    #[serde(default)]
    pub cursor_color: Option<String>,
    /// Optional avatar URL or initials fragment. Concept feature parity
    /// only — the runtime renders initials today, so this is stored but
    /// not yet wired to a bitmap loader.
    #[serde(default)]
    pub avatar: Option<String>,
    /// Role this peer plays in the workspace. References
    /// [`Role::id`](Role) in the owning workspace's role table.
    /// Permission checks read from `WorkspaceFile`; this field drives
    /// the top-bar role chip + concept's roster dropdown.
    #[serde(default)]
    pub role_id: Option<String>,
}

impl Collaborator {
    /// Build a collaborator with no tab attribution — the common case for
    /// "peer entered the workspace but hasn't focused content yet". Existing
    /// call sites that used the struct literal `Collaborator { id, name }`
    /// migrate to this constructor so adding future fields (cursor colour,
    /// avatar, role) doesn't force another codebase-wide literal sweep.
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            active_tab: None,
            cursor_color: None,
            avatar: None,
            role_id: None,
        }
    }

    /// Same as [`Collaborator::new`] but also attributes the peer to a
    /// specific tab, turning on the low-alpha presence dot beneath that
    /// tab's pill.
    pub fn with_active_tab(id: impl Into<String>, name: impl Into<String>, tab: u64) -> Self {
        let mut c = Self::new(id, name);
        c.active_tab = Some(tab);
        c
    }

    /// Attach an explicit `#rrggbb` / `#rrggbbaa` override for this peer.
    /// Takes precedence over the palette lookup when the top-bar /
    /// sidebar / tab dots read a colour.
    pub fn with_cursor_color(mut self, hex: impl Into<String>) -> Self {
        self.cursor_color = Some(hex.into());
        self
    }

    /// Attach an avatar reference (URL or initials fragment).
    pub fn with_avatar(mut self, avatar: impl Into<String>) -> Self {
        self.avatar = Some(avatar.into());
        self
    }

    /// Attach a role id pointing into the workspace's role table.
    pub fn with_role(mut self, role_id: impl Into<String>) -> Self {
        self.role_id = Some(role_id.into());
        self
    }
}

/// Identifier for an installed app — opaque to the state layer; the
/// portal layer fills it in with the caller's wayland app-id.
pub type AppId = String;

/// Filesystem-grant access mode. Mirrors the read / read-write split
/// the portal D-Bus backend (Phase 4 Task 4.3) will surface to apps.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FsMode {
    Read,
    ReadWrite,
}

/// How long a capability grant lives.
///
/// This is [`collab_types::caps::Expiry`] under the name the prompt UI
/// already uses. It is an alias, not a parallel enum: the compositor's
/// "Just once / Until I log out / Always" buttons are
/// `AfterUse` / `EndOfSession` / `Never`, and the same three values
/// travel to the filesystem, the launcher sandbox and every peer.
pub type GrantScope = Expiry;

/// "Just once" — the prompt's one-shot button.
pub const GRANT_ONCE: Expiry = Expiry::AfterUse;
/// "Until I log out" — held for this shell session, never written to disk.
pub const GRANT_SESSION: Expiry = Expiry::EndOfSession;
/// "Always" — survives a restart.
pub const GRANT_ALWAYS: Expiry = Expiry::Never;

/// Per-app capability grant stored on a [`Workspace`]. Phase 5 wires
/// each variant to its enforcement layer (Bubblewrap allowlist for
/// filesystem, Pipewire ACL for camera/mic, nftables for network);
/// at this phase we only model the data so the prompt widget and
/// portal backend can read/write into the CRDT.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Capability {
    /// Read or read-write access to a specific filesystem path.
    Filesystem {
        path: std::path::PathBuf,
        mode: FsMode,
        scope: GrantScope,
    },
    /// Camera (`/dev/video*`) access.
    Camera { scope: GrantScope },
    /// Microphone (Pipewire socket) access.
    Microphone { scope: GrantScope },
    /// Screen capture via portal `ScreenCast`.
    Screencast { scope: GrantScope },
    /// Outbound network access. `hosts: None` = "any host"; `hosts:
    /// Some(allowlist)` = "only these hosts".
    ///
    /// **Enforcement (Phase 5 Task 5.2):** v1 toggles the per-workspace
    /// network namespace isolation (`unshare(CLONE_NEWNET)` /
    /// bwrap's `--unshare-net`) — any `Always`-scoped grant lifts the
    /// default-deny. The `hosts` allowlist is recorded by the data
    /// model but **not yet enforced**: per-namespace nftables rule
    /// generation is deferred to a Phase 5.2 follow-up unit. v1
    /// therefore treats `Some([...])` and `None` identically.
    Network {
        hosts: Option<Vec<String>>,
        scope: GrantScope,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Workspace {
    pub id: WorkspaceId,
    /// Durable identity of this workspace as a collaboration room.
    ///
    /// [`Self::id`] indexes the local tree and is meaningless off this
    /// machine; `room` is the value two hosts must agree on for their
    /// apps to exchange anything. Minted once at creation, written to
    /// the state file with the rest of the workspace, mirrored into the
    /// automerge doc so it replicates, and carried in the invite a peer
    /// accepts.
    ///
    /// `#[serde(default)]` yields [`RoomId::nil`] for state files
    /// written before rooms existed;
    /// [`WorkspaceTree::adopt_roomless_workspaces`] mints a real one at
    /// boot and flushes it, so the sentinel survives exactly one load.
    #[serde(default)]
    pub room: RoomId,
    pub name: String,
    pub strips: Vec<Strip>,
    pub roles: Vec<Role>,
    pub workspace_files: Vec<WorkspaceFile>,
    #[serde(default)]
    pub collaborators: Vec<Collaborator>,
    /// Who owns this room.
    ///
    /// Ownership is the root of trust and the answer to the
    /// bootstrapping question every capability system has: someone must
    /// be able to issue the first capability. The owner is the one
    /// principal whose authority is not itself a capability; everything
    /// else in [`Self::capabilities`] descends from them.
    ///
    /// `None` means "no owner recorded" — a workspace restored from a
    /// state file written before ownership existed. Such a workspace is
    /// adopted by the local user on first load
    /// ([`WorkspaceTree::adopt_ownerless_workspaces`]) rather than being
    /// treated as owned by everybody.
    #[serde(default)]
    pub owner: Option<UserId>,
    /// **Every** permission in this room, as capabilities.
    ///
    /// One list, one vocabulary. Room membership lives here (scope
    /// [`Scope::Room`]); a file shared with one person lives here
    /// (scope [`Scope::FsPath`]); an app's portal grant lives here
    /// (subject [`Principal::App`]). There is deliberately no second
    /// list for "shares" and no per-app map — those were the shapes
    /// that let the two ideas drift apart.
    ///
    /// `#[serde(default)]` so state files written before this field
    /// existed still load; they come back with no capabilities, which
    /// is the safe direction.
    #[serde(default)]
    pub capabilities: Vec<CapOverride>,
    /// Surfaces from this workspace that are being streamed to one or
    /// more peers — peer-sync Phase 4 v2. Keys are the wayland
    /// `wl_surface` protocol ids the compositor passes through
    /// `TileContent::Foreign`; values are the set of libp2p
    /// [`PeerId`]s (hashed to `u64`, see
    /// [`crate::network::protocol::PeerId`]) the share targets.
    ///
    /// D-7 fix: the prior shape was `HashMap<u64, u64>` — a single
    /// peer per surface. The second `share_surface(s, peer_b)` call
    /// silently overwrote the first `share_surface(s, peer_a)`, so a
    /// surface shared with two peers dropped one of them without
    /// surfacing an error. The value is now a `HashSet<u64>` so
    /// multi-recipient streaming works as expected; `share_surface`
    /// inserts into the set, `unshare_surface` clears it.
    ///
    /// `#[serde(default)]` so persisted state files written before this
    /// field existed still load. Mirrors the `grants` rollout pattern.
    #[serde(default)]
    pub shared_surfaces: HashMap<u64, std::collections::HashSet<u64>>,
}

/// Stable identifier for a person. The compositor's collaborator table
/// keys on this; a peer's Ed25519 key hex is a valid value, and so is
/// the local user's account name.
pub type UserId = String;

/// The id of the person sitting at this machine.
///
/// Derived from the device identity when one is provisioned, so a
/// capability granted to this user is the same subject a peer sees, and
/// falls back to the OS account name otherwise. Deliberately stable
/// across restarts: an owner whose id changed every boot would lose
/// their own workspaces.
pub fn local_user_id() -> UserId {
    if let Ok(v) = std::env::var("QOS_USER_ID")
        && !v.is_empty()
    {
        return v;
    }
    if let Ok(bytes) = std::fs::read("/var/lib/qos/user-identity/pubkey.bin")
        && bytes.len() == 32
    {
        let mut key = [0u8; 32];
        key.copy_from_slice(&bytes);
        if let Principal::Person(id) =
            Principal::person_from_key(collab_types::crypto::Ed25519PublicKey::from_bytes(key))
        {
            return id;
        }
    }
    std::env::var("USER").unwrap_or_else(|_| "local".to_string())
}

/// Seconds since the Unix epoch, for capability expiry.
///
/// A clock that has not been set yet reads as 0, which makes every
/// `AtUnix` capability look live. That is the safe direction for a
/// *grant* and the unsafe one for a *restriction*, so restrictions
/// should be written without an expiry.
pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl Capability {
    /// The `(scope, verb)` this request descriptor asks for.
    ///
    /// [`Capability`] is what an app *asks* for — a path, a device, a
    /// set of hosts. It is not a storage vocabulary: granting one
    /// writes [`CapOverride`]s, and this is the translation. A network
    /// request names one scope per host, so an allowlist of three hosts
    /// becomes three capabilities and dropping one of them is a
    /// meaningful, expressible act.
    pub fn requested_scopes(&self) -> Vec<(Scope, Verb)> {
        match self {
            Capability::Filesystem { path, mode, .. } => {
                let verb = match mode {
                    FsMode::Read => Verb::Read,
                    FsMode::ReadWrite => Verb::Write,
                };
                vec![(Scope::FsPath(path.to_string_lossy().into_owned()), verb)]
            }
            Capability::Camera { .. } => vec![(Scope::Device("camera".into()), Verb::Read)],
            Capability::Microphone { .. } => {
                vec![(Scope::Device("microphone".into()), Verb::Read)]
            }
            Capability::Screencast { .. } => {
                vec![(Scope::Device("screencast".into()), Verb::Read)]
            }
            Capability::Network { hosts, .. } => match hosts {
                // An allowlist means exactly those hosts. `None` is the
                // explicit "any host" request and is the only thing
                // that produces a wildcard scope — recording an
                // allowlist and then behaving as if it said `None` is
                // the bug this shape exists to prevent.
                Some(list) => list
                    .iter()
                    .map(|h| (Scope::NetHost(h.clone()), Verb::Write))
                    .collect(),
                None => vec![(Scope::NetHost("*".into()), Verb::Write)],
            },
        }
    }

    /// The lifetime the requester asked for.
    pub fn requested_expiry(&self) -> Expiry {
        match self {
            Capability::Filesystem { scope, .. }
            | Capability::Camera { scope }
            | Capability::Microphone { scope }
            | Capability::Screencast { scope }
            | Capability::Network { scope, .. } => *scope,
        }
    }

    /// Rebuild a descriptor from a stored capability, for the
    /// Permissions tab. `None` when the capability is not an app grant
    /// this UI knows how to render.
    pub fn from_capability(cap: &CapOverride) -> Option<Self> {
        let scope = cap.expiry;
        match &cap.scope {
            Scope::FsPath(p) => Some(Capability::Filesystem {
                path: std::path::PathBuf::from(p),
                mode: if cap.verb.implies(Verb::Write) {
                    FsMode::ReadWrite
                } else {
                    FsMode::Read
                },
                scope,
            }),
            Scope::Device(d) if d == "camera" => Some(Capability::Camera { scope }),
            Scope::Device(d) if d == "microphone" => Some(Capability::Microphone { scope }),
            Scope::Device(d) if d == "screencast" => Some(Capability::Screencast { scope }),
            Scope::NetHost(h) => Some(Capability::Network {
                hosts: if h == "*" {
                    None
                } else {
                    Some(vec![h.clone()])
                },
                scope,
            }),
            _ => None,
        }
    }
}

impl Workspace {
    // ---------------------------------------------------------------
    // App grants — the portal's view of the one capability list.
    //
    // These are not a second store. They read and write
    // `self.capabilities`, filtered to subjects that are apps.
    // ---------------------------------------------------------------

    /// Record an app's grant. Returns the ids of the capabilities
    /// written (one per requested scope).
    pub fn grant_app(&mut self, app_id: &str, cap: &Capability) -> Vec<String> {
        let expiry = cap.requested_expiry();
        cap.requested_scopes()
            .into_iter()
            .map(|(scope, verb)| {
                self.grant_capability(
                    CapOverride::grant(Subject::app(app_id), scope, verb).expiring(expiry),
                )
            })
            .collect()
    }

    /// Every capability whose subject is the app `app_id`.
    pub fn app_capabilities(&self, app_id: &str) -> Vec<&CapOverride> {
        let want = Subject::app(app_id);
        let now = Some(now_unix());
        self.capabilities
            .iter()
            .filter(|c| c.subject == want && c.effect == Effect::Grant && c.is_live_at(now))
            .collect()
    }

    /// The portal-shaped view of app grants, for the Permissions tab.
    ///
    /// Derived on every call from [`Self::capabilities`] — there is no
    /// cached copy to fall out of sync.
    pub fn grants(&self) -> HashMap<AppId, Vec<Capability>> {
        let now = Some(now_unix());
        let mut out: HashMap<AppId, Vec<Capability>> = HashMap::new();
        for c in &self.capabilities {
            if c.effect != Effect::Grant || !c.is_live_at(now) {
                continue;
            }
            let Subject::Principal(Principal::App(app)) = &c.subject else {
                continue;
            };
            if let Some(desc) = Capability::from_capability(c) {
                out.entry(app.clone()).or_default().push(desc);
            }
        }
        out
    }

    /// Does any app hold a grant in this room? Drives the Permissions
    /// tab's visibility.
    pub fn has_app_grants(&self) -> bool {
        let now = Some(now_unix());
        self.capabilities.iter().any(|c| {
            c.effect == Effect::Grant
                && c.is_live_at(now)
                && matches!(&c.subject, Subject::Principal(Principal::App(_)))
                && Capability::from_capability(c).is_some()
        })
    }

    /// Ids of the capabilities backing app `app_id`'s grants, in the
    /// same order [`Self::grants`] renders them, so a Revoke button
    /// that knows a row index can name the capability it removes.
    pub fn app_grant_ids(&self, app_id: &str) -> Vec<String> {
        let now = Some(now_unix());
        let want = Subject::app(app_id);
        self.capabilities
            .iter()
            .filter(|c| {
                c.subject == want
                    && c.effect == Effect::Grant
                    && c.is_live_at(now)
                    && Capability::from_capability(c).is_some()
            })
            .map(|c| c.id.clone())
            .collect()
    }

    // ---------------------------------------------------------------
    // The capability store. Every permission question in the
    // compositor bottoms out in `decide` / `allows` below; nothing
    // else is authorised to answer one.
    // ---------------------------------------------------------------

    /// Decide whether `actor` may apply `verb` to `resource` in this
    /// room, at the current wall-clock time.
    ///
    /// Fail-closed: this is `collab_types::decide_capability`, which
    /// denies unless something explicitly permits.
    pub fn decide(&self, actor: &Actor, resource: &ResourceRef, verb: Verb) -> CapDecision {
        decide_capability(actor, resource, verb, &self.capabilities, Some(now_unix()))
    }

    /// [`Self::decide`] reduced to a bool, for call sites that only
    /// gate an action rather than explaining a denial.
    pub fn allows(&self, actor: &Actor, resource: &ResourceRef, verb: Verb) -> bool {
        self.decide(actor, resource, verb) == CapDecision::Allow
    }

    /// Record a capability. Returns the capability's id, minting one if
    /// the caller did not supply it so the grant can always be revoked
    /// by name later.
    ///
    /// Authority to *make* a grant is checked by the caller
    /// ([`Self::grant_as`]), not here — this is the storage primitive.
    pub fn grant_capability(&mut self, mut cap: CapOverride) -> String {
        if cap.id.is_empty() {
            cap.id = format!("cap-{}", next_id());
        }
        // A re-grant of the same (subject, scope, effect, verb) merges
        // rather than stacks, so revoking once is enough.
        //
        // Merging keeps the **longer-lived** expiry. A grant is
        // monotonic: granting something for this session when it is
        // already granted permanently must not quietly demote it to the
        // session. That is not a hypothetical — an app that holds
        // "Always" for the camera and then makes a fresh request would
        // have had its permanent grant replaced by the one-shot the
        // prompt defaults to, and the user would never be told. Reducing
        // authority is what revoke and `Effect::Restrict` are for, and
        // both are explicit acts.
        if let Some(slot) = self.capabilities.iter_mut().find(|c| {
            c.subject == cap.subject
                && c.scope == cap.scope
                && c.effect == cap.effect
                && c.verb == cap.verb
        }) {
            slot.expiry = Expiry::longest(slot.expiry, cap.expiry);
            if !cap.id.is_empty() {
                slot.id = cap.id;
            }
            if cap.granted_by.is_some() {
                slot.granted_by = cap.granted_by;
            }
            return slot.id.clone();
        }
        if cap.id.is_empty() {
            cap.id = format!("cap-{}", next_id());
        }
        let id = cap.id.clone();
        self.capabilities.push(cap);
        id
    }

    /// Grant a capability **on behalf of** `grantor`, enforcing that
    /// the grantor actually holds authority to delegate it.
    ///
    /// This is the check that makes ownership mean something: a peer
    /// who is not the owner and holds no `Admin` over the scope cannot
    /// widen anyone's access, including their own. Returns `None` when
    /// the grantor lacks that authority and nothing was recorded.
    pub fn grant_as(&mut self, grantor: &UserId, cap: CapOverride) -> Option<String> {
        if !self.may_administer(grantor) {
            return None;
        }
        let cap = cap.granted_by(Principal::Person(grantor.clone()));
        Some(self.grant_capability(cap))
    }

    /// Drop the capability with this id. Returns whether anything was
    /// removed.
    pub fn revoke_capability(&mut self, id: &str) -> bool {
        let before = self.capabilities.len();
        self.capabilities.retain(|c| c.id != id);
        self.capabilities.len() != before
    }

    /// Revoke on behalf of `revoker`, enforcing the same authority as
    /// [`Self::grant_as`].
    pub fn revoke_as(&mut self, revoker: &UserId, id: &str) -> bool {
        self.may_administer(revoker) && self.revoke_capability(id)
    }

    /// Drop every capability matching `pred`. Returns the ids removed,
    /// so the caller can mirror each removal into the replicated doc.
    pub fn revoke_matching(&mut self, pred: impl Fn(&CapOverride) -> bool) -> Vec<String> {
        let mut removed = Vec::new();
        self.capabilities.retain(|c| {
            if pred(c) {
                removed.push(c.id.clone());
                false
            } else {
                true
            }
        });
        removed
    }

    // ---------------------------------------------------------------
    // Ownership and membership.
    // ---------------------------------------------------------------

    /// Is `user` the owner of this room?
    ///
    /// True for the recorded owner, and for anyone the owner has given
    /// `Admin` over the room — a delegated co-owner. Everyone else is
    /// false, including when no owner is recorded at all: an ownerless
    /// workspace has no administrators, rather than only administrators.
    pub fn may_administer(&self, user: &UserId) -> bool {
        if self.owner.as_deref() == Some(user.as_str()) {
            return true;
        }
        self.allows(
            &self.actor_for_person(user),
            &ResourceRef::Workspace,
            Verb::Admin,
        )
    }

    /// Install `user` as owner, and record the membership capability
    /// that makes them an `Admin` of the room.
    ///
    /// Both halves matter: `owner` is the root of trust that survives
    /// an empty capability list, and the capability is what replicates
    /// to peers and what the decision function actually reads.
    pub fn set_owner(&mut self, user: impl Into<UserId>) {
        let user = user.into();
        self.owner = Some(user.clone());
        self.grant_capability(
            CapOverride::membership(
                Subject::Principal(Principal::Person(user)),
                self.id.raw().to_string(),
                Verb::Admin,
            )
            .with_id(format!("owner-{}", self.id.raw())),
        );
    }

    /// The role `user` holds in this room, derived from their
    /// membership capability.
    ///
    /// A role is not stored separately — it *is* the verb on the
    /// membership capability, which is what keeps "being in a room" and
    /// "being allowed to do something" from drifting apart. `None`
    /// means "not a member", which is different from `Viewer`.
    pub fn role_of(&self, user: &UserId) -> Option<CapRole> {
        if self.owner.as_deref() == Some(user.as_str()) {
            return Some(CapRole::Owner);
        }
        let room = self.id.raw().to_string();
        let subject_is_user = |c: &CapOverride| match &c.subject {
            Subject::Principal(Principal::Person(p)) => p == user,
            _ => false,
        };
        let now = Some(now_unix());
        let mut best: Option<Verb> = None;
        let mut blocked = false;
        for c in &self.capabilities {
            if !subject_is_user(c) || !c.is_live_at(now) {
                continue;
            }
            let covers_room = matches!(&c.scope, Scope::Room(r) if *r == room)
                || matches!(c.scope, Scope::Workspace);
            if !covers_room {
                continue;
            }
            match c.effect {
                Effect::Grant => {
                    best = Some(match best {
                        Some(prev) if prev.implies(c.verb) => prev,
                        _ => c.verb,
                    })
                }
                // A room-wide Restrict below Read is how a member is
                // blocked without being erased from history.
                Effect::Restrict if !c.verb.implies(Verb::Read) => blocked = true,
                Effect::Restrict => {}
            }
        }
        if blocked {
            return Some(CapRole::Denied);
        }
        best.map(|v| match v {
            Verb::Admin => CapRole::Owner,
            Verb::Write => CapRole::Editor,
            Verb::Comment => CapRole::Commenter,
            Verb::Read | Verb::Invite => CapRole::Viewer,
        })
    }

    /// Build the [`Actor`] for a person acting in this room.
    pub fn actor_for_person(&self, user: &UserId) -> Actor {
        Actor::person(self.role_of(user), user.clone()).in_room(self.id.raw().to_string())
    }

    /// Build the [`Actor`] for an app running in this room.
    ///
    /// The app carries the room as a principal so room-wide capabilities
    /// reach it, but **no role** — so it has no base authority and can
    /// only act through a capability granted to it by name. That is
    /// precisely the property that makes the portal prompt the one
    /// user-facing permission dialog.
    pub fn actor_for_app(&self, app_id: &str) -> Actor {
        Actor::app(app_id).in_room(self.id.raw().to_string())
    }

    /// Build the [`Actor`] for an agent acting for the user. Same
    /// authority shape as an app: scoped, revocable, no role.
    pub fn actor_for_agent(&self, agent_id: &str) -> Actor {
        Actor::agent(agent_id).in_room(self.id.raw().to_string())
    }

    /// The room roster: the people who are *members*.
    ///
    /// Deliberately built from room-scoped capabilities only. Someone
    /// who holds a capability on one file and nothing else is **not**
    /// here — presenting them as a collaborator is the exact confusion
    /// the model exists to prevent. See [`Self::shares_of`] for where
    /// they do appear.
    pub fn roster(&self) -> Vec<(UserId, CapRole)> {
        let room = self.id.raw().to_string();
        let now = Some(now_unix());
        let mut seen: Vec<UserId> = Vec::new();
        for c in &self.capabilities {
            if !c.is_live_at(now) || c.effect != Effect::Grant {
                continue;
            }
            let covers_room = matches!(&c.scope, Scope::Room(r) if *r == room)
                || matches!(c.scope, Scope::Workspace);
            if !covers_room {
                continue;
            }
            if let Subject::Principal(Principal::Person(p)) = &c.subject
                && !seen.contains(p)
            {
                seen.push(p.clone());
            }
        }
        if let Some(owner) = &self.owner
            && !seen.contains(owner)
        {
            seen.insert(0, owner.clone());
        }
        seen.into_iter()
            .filter_map(|u| self.role_of(&u).map(|r| (u, r)))
            .collect()
    }

    /// The share list for one resource: the people who hold a
    /// capability naming *this* resource specifically.
    ///
    /// Room-scoped capabilities are excluded — those are membership and
    /// belong in [`Self::roster`]. The two lists are disjoint by
    /// construction, which is how the UI is stopped from ever
    /// presenting "I shared one file with Bob" as "Bob is a
    /// collaborator".
    pub fn shares_of(&self, resource: &ResourceRef) -> Vec<&CapOverride> {
        let now = Some(now_unix());
        self.capabilities
            .iter()
            .filter(|c| {
                c.is_live_at(now)
                    && c.effect == Effect::Grant
                    && c.subject_is_person()
                    && !c.is_room_membership()
                    && c.scope.matches_resource(resource)
            })
            .collect()
    }

    /// Build an empty workspace with the given id and name. The workspace
    /// starts with no strips, roles, files, collaborators, or grants —
    /// those are populated by the compositor or the persistence layer.
    pub fn new(id: WorkspaceId, name: impl Into<String>) -> Self {
        Self {
            id,
            // A workspace that exists is a room that exists. Minting
            // here rather than lazily on first use is what makes the
            // identity a property of the workspace instead of a
            // property of whoever asked for it first.
            room: RoomId::new(),
            name: name.into(),
            strips: Vec::new(),
            roles: Vec::new(),
            workspace_files: Vec::new(),
            collaborators: Vec::new(),
            owner: None,
            capabilities: Vec::new(),
            shared_surfaces: HashMap::new(),
        }
    }

    /// A workspace with `id`, `name`, and a freshly-minted room, and
    /// every other field empty.
    ///
    /// The builder the fixtures want. Hand-written `Workspace { … }`
    /// literals break on every field this struct gains; going through
    /// here means a fixture only names what it cares about — and, for
    /// the room specifically, means a fixture never accidentally sits
    /// in the nil room alongside every other fixture in the suite.
    pub fn test_default(id: WorkspaceId, name: impl Into<String>) -> Self {
        Self::new(id, name)
    }

    /// This workspace's collaboration cluster id.
    ///
    /// A workspace *is* the collaboration room: apps opened into it
    /// exchange CRDT deltas with each other and with the peers invited
    /// to it, and apps in another workspace are a different room. The
    /// wire speaks `Uuid` for that room, and this is the value it
    /// carries — [`Self::room`], not a function of the local
    /// [`Self::id`]. That is the whole point: a value two machines can
    /// both hold, rather than one each derives from a counter that
    /// means something different on each of them.
    ///
    /// A workspace whose room has not been minted yet returns the nil
    /// uuid. `crate::shell::collab_broker::CrdtRoute::delivers_to`
    /// refuses to deliver anything to or from it, so the window
    /// between loading a pre-rooms state file and
    /// [`WorkspaceTree::adopt_roomless_workspaces`] running cannot
    /// become one giant room shared by every unminted workspace on
    /// every machine.
    pub fn collab_cluster_id(&self) -> uuid::Uuid {
        self.room.as_uuid()
    }

    /// Mark `surface_id` as being streamed to `peer_id` — peer-sync
    /// Phase 4 v2. The compositor's commit handler consults this map
    /// (indirectly, via [`Workspace::is_surface_shared`]) to decide
    /// whether to stage the buffer for the streaming task.
    ///
    /// D-7 fix: this call is now additive. Two
    /// `share_surface(s, peer_a)` / `share_surface(s, peer_b)` calls
    /// in sequence record BOTH peers in the per-surface set. The
    /// previous shape (single-peer-per-surface) overwrote, dropping
    /// peer A silently — callers that wired up multi-recipient
    /// streaming lost subscribers without any error signal.
    pub fn share_surface(&mut self, surface_id: u64, peer_id: u64) {
        self.shared_surfaces
            .entry(surface_id)
            .or_default()
            .insert(peer_id);
    }

    /// Drop the share entry for `surface_id`. No-op if the surface is
    /// not shared. Returns the set of peers the surface was shared
    /// with so callers can fire `StopShare` events for each without a
    /// second lookup.
    pub fn unshare_surface(&mut self, surface_id: u64) -> Option<std::collections::HashSet<u64>> {
        self.shared_surfaces.remove(&surface_id)
    }

    /// Stop streaming `surface_id` to one specific peer, leaving any
    /// other peers' shares intact. Returns `true` if a share was
    /// removed; `false` if the (surface, peer) pair wasn't shared.
    /// When the last peer is removed the surface key itself is also
    /// dropped so [`Self::is_surface_shared`] reports `false` again.
    pub fn unshare_surface_from_peer(&mut self, surface_id: u64, peer_id: u64) -> bool {
        let removed = match self.shared_surfaces.get_mut(&surface_id) {
            Some(peers) => peers.remove(&peer_id),
            None => false,
        };
        if removed
            && self
                .shared_surfaces
                .get(&surface_id)
                .map(|p| p.is_empty())
                .unwrap_or(false)
        {
            self.shared_surfaces.remove(&surface_id);
        }
        removed
    }

    /// Whether `surface_id` is currently streamed somewhere. Used by
    /// the compositor's commit hook to short-circuit the readback for
    /// unshared surfaces (~all of them in steady state).
    pub fn is_surface_shared(&self, surface_id: u64) -> bool {
        self.shared_surfaces
            .get(&surface_id)
            .map(|peers| !peers.is_empty())
            .unwrap_or(false)
    }
}

impl Default for Workspace {
    /// Empty workspace with a zero id, an empty name, and a freshly
    /// minted room. Useful for `..Default::default()` in struct-update
    /// syntax (mirrors what [`WorkspaceTree`] does for itself) so
    /// adding new fields like `grants` doesn't force a codebase-wide
    /// literal sweep.
    ///
    /// The room is minted rather than left at [`RoomId::nil`] on
    /// purpose: a workspace that exists is a room that exists, and two
    /// workspaces built this way must not silently turn out to be the
    /// same room. The nil sentinel is reachable only through
    /// deserialisation of a state file written before rooms existed —
    /// `#[serde(default)]` on the field, not this impl — which is
    /// exactly the case
    /// [`WorkspaceTree::adopt_roomless_workspaces`] repairs.
    fn default() -> Self {
        Self {
            id: WorkspaceId::default(),
            room: RoomId::new(),
            name: String::new(),
            strips: Vec::new(),
            roles: Vec::new(),
            workspace_files: Vec::new(),
            collaborators: Vec::new(),
            owner: None,
            capabilities: Vec::new(),
            shared_surfaces: HashMap::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkspaceTree {
    pub workspaces: Vec<Workspace>,
    #[serde(default)]
    pub active: WorkspaceId,
    pub active_strip: HashMap<WorkspaceId, StripId>,
    /// Per-strip "active group" pointer — the [`GroupId`] inside each strip
    /// that currently owns focus / keyboard input / presence broadcasts.
    /// Parallels [`Self::active_strip`] (per-workspace) but at one level
    /// deeper: each strip carries its own active group independently so
    /// switching strips doesn't drag focus back to a stale group.
    ///
    /// Optional per strip — `None`/missing means "no explicit selection;
    /// resolvers should fall back to the first non-closing group" (see
    /// [`Self::resolve_active_group`]).
    ///
    /// `#[serde(default)]` so persisted state files written before this
    /// field existed still load — they boot with an empty map and the
    /// shell seeds entries lazily on first focus.
    #[serde(default)]
    pub active_group: HashMap<StripId, GroupId>,
    pub collaborators: Vec<Collaborator>,
    /// Automerge backing doc for peer-sync. Lazily seeded — empty until
    /// a peer feeds changes via [`Self::apply_changes`] or a test
    /// directly mutates this field. The cache (other fields) is the
    /// source of truth for reads; the doc is the source of truth for
    /// peer change export. Mutators that should mirror to both sides
    /// land in subsequent units (B–D) of the migration.
    ///
    /// `#[serde(skip)]` so existing JSON persistence ignores it; the
    /// doc rebuilds empty on load and gets populated as peer-sync runs.
    ///
    /// Public for two reasons: (1) struct-literal sites scattered across
    /// integration tests need `..Default::default()` to compile against
    /// a non-exhaustive struct, and Rust's update-syntax requires every
    /// unspecified field to be visible at the use-site; (2) the
    /// foundation tests for unit A poke values directly into the doc to
    /// synthesise change-blobs since no mutator wires through yet.
    /// Real consumers must use [`Self::current_heads`],
    /// [`Self::export_changes_since`], and [`Self::apply_changes`] —
    /// touching `doc` directly bypasses the cache rebuild and is only
    /// safe for peer-sync internals or tests.
    #[serde(skip, default = "AutoCommit::new")]
    pub doc: AutoCommit,
    /// Where to write this tree when a capability changes.
    ///
    /// `None` disables the write, which is what tests and the offscreen
    /// renderer want. The compositor sets it at boot, and that is what
    /// makes an `Always` grant actually survive a restart: before this,
    /// nothing in the compositor called `save_tree_to` at all, so
    /// `Always` behaved exactly like `EndOfSession`.
    #[serde(skip)]
    pub persist_path: Option<std::path::PathBuf>,
    /// Where to export the capability list the FUSE mount enforces.
    ///
    /// `None` disables the export. The compositor sets it at boot to the
    /// same path it passes to `qos-fs --acl`, so the filesystem and the
    /// compositor decide from one list rather than two.
    #[serde(skip)]
    pub fs_acl_path: Option<std::path::PathBuf>,
}

impl Default for WorkspaceTree {
    fn default() -> Self {
        Self {
            workspaces: Vec::new(),
            active: WorkspaceId::default(),
            active_strip: HashMap::new(),
            active_group: HashMap::new(),
            collaborators: Vec::new(),
            doc: AutoCommit::new(),
            persist_path: None,
            fs_acl_path: None,
        }
    }
}

/// Shared handle to a single [`WorkspaceTree`] instance.
///
/// Closes C-3 (`docs/superpowers/specs/2026-05-22-workspace-tree-arc-mutex-design.md`):
/// the compositor (`ChromeState`), the portal (`GrantStoreHandle`), and
/// peer-sync (`WorkspaceTreeReplica`) all hold a clone of this handle so
/// every mutation lands on the same underlying tree. Previously each
/// subsystem owned an independent `WorkspaceTree` clone — three trees,
/// three identities, diverging on the first write.
///
/// `parking_lot::RwLock` is chosen over `std::sync::Mutex` because the
/// shell repaints at ~60 Hz reading the tree on every layout pass; many
/// readers (paint + grant lookup + peer-sync export) outnumber the
/// occasional writer (chrome mutations + portal `Allow` / peer-sync
/// `apply_changes`). Uncontended `parking_lot` read locks are ~10 ns —
/// imperceptible at frame-rate.
///
/// Use [`WorkspaceTree::shared`] to wrap an owned tree, or
/// [`WorkspaceTreeHandleExt::test_default`] for a fresh handle with a
/// default tree.
pub type WorkspaceTreeHandle = std::sync::Arc<parking_lot::RwLock<WorkspaceTree>>;

/// Extension trait providing constructors on the [`WorkspaceTreeHandle`]
/// type alias. Rust doesn't let us add inherent methods to a type alias,
/// so the constructors live as associated functions on this trait.
///
/// Imported as `use WorkspaceTreeHandleExt as _;` at use-sites; the
/// trait is in the same module as `WorkspaceTreeHandle` so the natural
/// import (`use crate::shell::state::WorkspaceTreeHandle;`) already
/// brings the trait into scope when its name is.
pub trait WorkspaceTreeHandleExt: Sized {
    /// Build a handle pre-wrapped around a default (empty) tree. Used by
    /// tests that don't care about the initial workspace set — and by
    /// the C-3 integration tests in particular, which build a handle,
    /// hand a clone to each subsystem, then mutate one and assert
    /// the others observe it.
    fn test_default() -> Self;
}

impl WorkspaceTreeHandleExt for WorkspaceTreeHandle {
    fn test_default() -> Self {
        std::sync::Arc::new(parking_lot::RwLock::new(WorkspaceTree::default()))
    }
}

impl WorkspaceTree {
    /// Wrap this tree in a [`WorkspaceTreeHandle`]. Consumes the tree
    /// (there should only ever be one instance) and returns the shared
    /// handle every subsystem clones from. Mirrors the C-3 RFC's
    /// "Threading the handle" section.
    pub fn shared(self) -> WorkspaceTreeHandle {
        std::sync::Arc::new(parking_lot::RwLock::new(self))
    }
}

impl WorkspaceTree {
    /// Set the process-wide peer salt that [`next_id`] stamps into the
    /// high 16 bits of every freshly-allocated id (C-1). Called once
    /// from the hosted entry point after the libp2p keypair is loaded —
    /// see [`crate::network::behaviour::peer_salt_from_keypair`].
    ///
    /// Default salt is `0`, which keeps every test that asserts on
    /// `WorkspaceId(1)` / `StripId(2)` style literals working without
    /// touching anything. Two peers minting ids under distinct salts
    /// can never collide.
    ///
    /// Takes `&mut self` even though the salt is process-global so the
    /// API surface invites the caller to set it on a tree they own — a
    /// future per-tree-salt refactor (e.g. for in-process e2e tests that
    /// simulate two peers) is a one-line change without breaking
    /// callers.
    pub fn set_peer_salt(&mut self, salt: u16) {
        NEXT_ID_PEER_SALT.store(salt, AtomicOrdering::Relaxed);
    }

    /// Read the current process-wide peer salt. Mostly used by tests to
    /// assert on the salt without poking the static directly.
    pub fn peer_salt(&self) -> u16 {
        NEXT_ID_PEER_SALT.load(AtomicOrdering::Relaxed)
    }

    /// Walk every id in the tree (workspace, strip, group, tab) and
    /// advance the process-wide [`NEXT_ID`] counter past the maximum
    /// **counter** stamped by the local peer salt. Idempotent: a second
    /// call with an unchanged tree is a no-op against the already-
    /// advanced counter.
    ///
    /// D-9 regression: the counter starts at `1` on every fresh
    /// process. On a restart where the persisted state already uses
    /// ids `[1, 50]`, the next [`WorkspaceId::new`] allocation
    /// collides with the existing id `1` — the cache rebuild then
    /// folds two distinct entries into one.
    ///
    /// C-1 follow-up: only ids whose high 16 bits match the local salt
    /// count toward the seed. Ids minted by a remote peer (different
    /// salt) live in a disjoint range and must NOT advance our counter,
    /// or we'd skip over our own legitimate `next_id` slots for no
    /// reason. The 0-salt case (tests, pre-peer-sync) walks every id
    /// the way it did before C-1.
    ///
    /// Call this from [`persist::load_or_default`] (and any other
    /// load path) after deserialising so subsequent allocations are
    /// guaranteed to be fresh.
    pub fn seed_next_id_from_self(&self) {
        let local_salt = NEXT_ID_PEER_SALT.load(AtomicOrdering::Relaxed);
        let mut max_counter: u64 = 0;
        let mut consider = |id: u64| {
            if peer_salt_of(id) == local_salt {
                max_counter = max_counter.max(counter_of(id));
            }
        };
        for ws in &self.workspaces {
            consider(ws.id.raw());
            for strip in &ws.strips {
                consider(strip.id.raw());
                for group in &strip.window_groups {
                    consider(group.id);
                    for tab in &group.tabs {
                        consider(tab.id);
                    }
                }
            }
        }
        // Advance the counter past `max_counter`. `fetch_max` keeps the
        // higher of the two so a parallel allocator that already
        // pushed past max_counter isn't reset backwards.
        NEXT_ID.fetch_max(max_counter.saturating_add(1), AtomicOrdering::Relaxed);
    }

    /// Close the room document's open transaction, recording when it
    /// happened.
    ///
    /// `AutoCommit` closes a transaction implicitly on the next read and
    /// leaves the change's advisory timestamp at zero. Automerge stores that
    /// field verbatim, so a room whose changes all went through the implicit
    /// path has a change graph that reads as 1 January 1970 from end to end —
    /// every entry in an activity view lands on the same day, and any
    /// date-range filter over it either matches everything or nothing. Ending
    /// each `doc_*` mirror here is what makes a room's history datable.
    ///
    /// It also gives each mirrored mutation its own change rather than letting
    /// an arbitrary run of them coalesce into whatever the next reader
    /// happened to flush, which is what makes one row mean one thing that
    /// happened.
    ///
    /// Cheap to call when nothing is pending — automerge returns without
    /// writing a change — so the compositor also calls it once a frame to
    /// catch the mirrors whose success paths are too branchy to end here.
    pub fn commit_room_change(&mut self) {
        let _ = qdoc::history::commit_stamped(&mut self.doc);
    }

    /// Snapshot of the current automerge change-graph heads. Peer-sync
    /// uses this as the baseline when computing what to send to a
    /// remote replica.
    ///
    /// Takes `&mut self` because automerge `0.7`'s `AutoCommit` flushes
    /// any pending transaction before returning heads — that's an
    /// internal mutation. Callers in peer-sync hold a `&mut` handle on
    /// the tree anyway.
    pub fn current_heads(&mut self) -> Vec<ChangeHash> {
        self.doc.get_heads()
    }

    /// Export every change applied since `since` as opaque change-blobs
    /// suitable for shipping to a peer. Returns an empty vec when the
    /// caller is already up-to-date with the current heads.
    ///
    /// Takes `&mut self` for the same reason as
    /// [`Self::current_heads`] — the underlying `AutoCommit::get_changes`
    /// commits any pending transaction first.
    pub fn export_changes_since(&mut self, since: &[ChangeHash]) -> Vec<Vec<u8>> {
        self.doc
            .get_changes(since)
            .into_iter()
            .map(|c| c.raw_bytes().to_vec())
            .collect()
    }

    /// Bring the public cache fields back in line with the room document
    /// after a *local* rewrite of it.
    ///
    /// [`Self::apply_changes`] already does this for changes arriving from a
    /// peer. A restore-from-history writes into the same document without
    /// going through that path, so it needs the same reconciliation — without
    /// it the tree would still be showing the pre-restore state it cached.
    pub fn resync_cache_from_doc(&mut self) {
        self.rebuild_cache_from_doc();
    }

    /// Apply a batch of opaque change-blobs received from a peer. After
    /// applying, the cache fields are rebuilt via
    /// [`Self::rebuild_cache_from_doc`] so subsequent reads observe the
    /// merged state.
    pub fn apply_changes(&mut self, changes: &[Vec<u8>]) -> Result<(), automerge::AutomergeError> {
        let parsed: Result<Vec<automerge::Change>, _> = changes
            .iter()
            .map(|b| automerge::Change::try_from(b.as_slice()))
            .collect();
        self.doc.apply_changes(parsed?)?;
        self.rebuild_cache_from_doc();
        Ok(())
    }

    /// Seed the automerge doc from the cache. Idempotent: a second call
    /// with the same cache state ends up writing the same values back
    /// (truncates the `workspaces` list and re-fills it). Called from
    /// [`Self::bootstrap_default`] after the cache is populated; tests
    /// that build literal trees should also call this if they need
    /// doc-side changes flowing out via [`Self::export_changes_since`].
    ///
    /// Units B + C mirror these shapes:
    /// - `active: ScalarValue::Uint` at `ROOT` (unit B),
    /// - `workspaces: List<Map>` at `ROOT`, each entry
    ///   `{ id: Uint, name: String, strips: List<Map> }` (unit B + C),
    /// - each strip entry: `{ id: Uint, window_groups: List<Map> }` (unit C),
    /// - each window-group entry:
    ///   `{ id: Uint, maximized: Bool, width_pixels: Uint }` (unit C).
    ///
    /// Tabs, `active_strip`, `active_group`, and collaborators stay
    /// cache-only — unit D wires those.
    pub fn seed_doc_from_cache(&mut self) {
        use automerge::{ObjType, ROOT, ReadDoc, ScalarValue, transaction::Transactable};
        // active
        let _ = self
            .doc
            .put(&ROOT, "active", ScalarValue::Uint(self.active.raw()));
        // workspaces — create the list if missing, then truncate +
        // re-fill from the cache. Simplest unit-B approach; future
        // units get diff-aware so concurrent appends don't clobber.
        let workspaces_id = match self.doc.get(&ROOT, "workspaces").ok().flatten() {
            Some((_, id)) => id,
            None => self
                .doc
                .put_object(&ROOT, "workspaces", ObjType::List)
                .expect("create workspaces list"),
        };
        let current_len = self.doc.length(&workspaces_id);
        for i in (0..current_len).rev() {
            let _ = self.doc.delete(&workspaces_id, i);
        }
        for (i, ws) in self.workspaces.iter().enumerate() {
            let entry = self
                .doc
                .insert_object(&workspaces_id, i, ObjType::Map)
                .expect("insert workspace map");
            let _ = self.doc.put(&entry, "id", ScalarValue::Uint(ws.id.raw()));
            let _ = self.doc.put(&entry, "name", ws.name.clone());
            // The room identity travels with the workspace. A peer that
            // hydrates a workspace out of this doc must land in *our*
            // room, not mint one of its own — the local `id` above is
            // meaningless to it, and a workspace it cannot address is a
            // workspace it cannot collaborate in.
            let _ = self.doc.put(&entry, "room", ws.room.to_string());
            // Ownership + capabilities travel with the workspace, so a
            // peer that hydrates from this doc learns who may do what
            // at the same moment it learns the room exists.
            if let Some(owner) = &ws.owner {
                let _ = self.doc.put(&entry, "owner", owner.clone());
            }
            let caps_id = self
                .doc
                .put_object(&entry, "capabilities", ObjType::Map)
                .expect("create capabilities map");
            for cap in &ws.capabilities {
                if let Ok(blob) = serde_json::to_vec(cap) {
                    let _ = self.doc.put(&caps_id, &cap.id, ScalarValue::Bytes(blob));
                }
            }
            // Unit C: nested strips + window_groups so per-group
            // maximized + width_pixels writes have a doc-side path
            // to land in.
            let strips_id = self
                .doc
                .put_object(&entry, "strips", ObjType::List)
                .expect("create strips list");
            for (j, strip) in ws.strips.iter().enumerate() {
                let strip_entry = self
                    .doc
                    .insert_object(&strips_id, j, ObjType::Map)
                    .expect("insert strip map");
                let _ = self
                    .doc
                    .put(&strip_entry, "id", ScalarValue::Uint(strip.id.raw()));
                let groups_id = self
                    .doc
                    .put_object(&strip_entry, "window_groups", ObjType::List)
                    .expect("create window_groups list");
                for (k, group) in strip.window_groups.iter().enumerate() {
                    let group_entry = self
                        .doc
                        .insert_object(&groups_id, k, ObjType::Map)
                        .expect("insert window_group map");
                    let _ = self
                        .doc
                        .put(&group_entry, "id", ScalarValue::Uint(group.id));
                    let _ = self.doc.put(
                        &group_entry,
                        "maximized",
                        ScalarValue::Boolean(group.maximized),
                    );
                    let _ = self.doc.put(
                        &group_entry,
                        "width_pixels",
                        ScalarValue::Uint(group.width.pixels() as u64),
                    );
                    // Unit D: tabs as a List of Maps with the tab's
                    // identifying scalars + a `tile_json` blob carrying
                    // the `TileContent` enum. JSON-blob shim is the
                    // pragmatic v1 (per spec § "Encoding shape") — full
                    // per-variant CRDT structure is a follow-up.
                    let tabs_id = self
                        .doc
                        .put_object(&group_entry, "tabs", ObjType::List)
                        .expect("create tabs list");
                    for (l, tab) in group.tabs.iter().enumerate() {
                        let tab_entry = self
                            .doc
                            .insert_object(&tabs_id, l, ObjType::Map)
                            .expect("insert tab map");
                        let _ = self.doc.put(&tab_entry, "id", ScalarValue::Uint(tab.id));
                        let _ = self.doc.put(&tab_entry, "title", tab.title.clone());
                        if let Ok(blob) = serde_json::to_vec(&tab.tile) {
                            let _ = self
                                .doc
                                .put(&tab_entry, "tile_json", ScalarValue::Bytes(blob));
                        }
                    }
                }
            }
        }
        self.commit_room_change();
    }

    /// Walk the automerge doc and refresh the public cache fields.
    ///
    /// Unit B mirrors `active` + the top-level `workspaces` list shape
    /// (id + name). Strips, window-groups, tabs, `active_strip`,
    /// `active_group`, and collaborators stay whatever the cache had —
    /// a peer that never observes structural ops just sees the
    /// workspace-list shape change. Units C/D extend this to nested
    /// state.
    ///
    /// To preserve strips that the cache built locally (e.g. tests that
    /// construct a literal tree, then receive a doc update from a
    /// peer), each rebuilt workspace tries to inherit `strips`,
    /// `roles`, `workspace_files`, and `collaborators` from the
    /// existing cache entry that shares its id. A workspace that
    /// arrived purely from the doc starts with empty nested vecs.
    fn rebuild_cache_from_doc(&mut self) {
        use automerge::{ROOT, ReadDoc, ScalarValue, Value};
        // active
        if let Some((Value::Scalar(s), _)) = self.doc.get(&ROOT, "active").ok().flatten() {
            if let ScalarValue::Uint(raw) = s.as_ref() {
                self.active = WorkspaceId::from(*raw);
            }
        }
        // workspaces — rebuild while preserving any existing nested
        // state by id-match.
        if let Some((_, workspaces_id)) = self.doc.get(&ROOT, "workspaces").ok().flatten() {
            let len = self.doc.length(&workspaces_id);
            let mut new_workspaces: Vec<Workspace> = Vec::with_capacity(len);
            for i in 0..len {
                // Each entry is a Map; pull id + name out of it.
                let entry_id = match self.doc.get(&workspaces_id, i).ok().flatten() {
                    Some((_, id)) => id,
                    None => continue,
                };
                let id_raw = match self.doc.get(&entry_id, "id").ok().flatten() {
                    Some((Value::Scalar(s), _)) => match s.as_ref() {
                        ScalarValue::Uint(v) => *v,
                        _ => continue,
                    },
                    _ => continue,
                };
                let name = match self.doc.get(&entry_id, "name").ok().flatten() {
                    Some((Value::Scalar(s), _)) => match s.as_ref() {
                        ScalarValue::Str(v) => v.to_string(),
                        _ => String::new(),
                    },
                    _ => String::new(),
                };
                let ws_id = WorkspaceId::from(id_raw);
                // The room identity replicates too. A peer that shipped
                // us this workspace named the room it collaborates in;
                // adopting it is the difference between hydrating a
                // workspace we can talk in and hydrating a
                // same-looking one we cannot. Absent or unparseable
                // (a doc written before rooms existed) leaves whatever
                // the cache already had, which for a locally-created
                // workspace is its own minted id.
                let replicated_room = match self.doc.get(&entry_id, "room").ok().flatten() {
                    Some((Value::Scalar(s), _)) => match s.as_ref() {
                        ScalarValue::Str(v) => RoomId::parse(v.as_str()),
                        _ => None,
                    },
                    _ => None,
                }
                .filter(|r| !r.is_nil());
                // Capabilities and ownership replicate through the doc,
                // so they are rebuilt from it here rather than being
                // carried over from the local cache. This is the half
                // that makes a *revocation* take effect: a capability a
                // peer deleted is simply absent from the doc, so it is
                // absent from the cache after this rebuild. Carrying the
                // cached list over instead would let a revoked
                // capability survive indefinitely on every peer that
                // already had it.
                let replicated = self.doc_read_capabilities(&entry_id);
                let mut ws = if let Some(existing_idx) =
                    self.workspaces.iter().position(|w| w.id == ws_id)
                {
                    // Take the existing cached workspace by swap-remove
                    // semantics — but keep ordering intact by drain
                    // -> reinsert. Simplest: remove and rename.
                    let mut existing = self.workspaces.remove(existing_idx);
                    existing.name = name;
                    existing
                } else {
                    Workspace::new(ws_id, name)
                };
                if let Some(room) = replicated_room {
                    ws.room = room;
                }
                if let Some((owner, caps)) = replicated {
                    if owner.is_some() {
                        ws.owner = owner;
                    }
                    ws.capabilities = caps;
                }
                new_workspaces.push(ws);
            }
            self.workspaces = new_workspaces;
        }

        // Unit C: reconstruct nested per-window-group `maximized` +
        // `width_pixels` from the doc. Walks each workspace's
        // `strips` list, then each strip's `window_groups` list, and
        // for every group entry tries to find the cached counterpart
        // by id. If the cache has the group, the doc values overwrite
        // the cache. If the cache lacks the strip or group entirely
        // (e.g. peer B started from `default()` and is hydrating from
        // a peer A snapshot), a fresh `Strip` / `WindowGroup` is
        // synthesized so the receiver's tree gains the structure.
        //
        // The synthesized groups carry no tabs — unit C only mirrors
        // maximized/width. Tab content propagation is unit D.
        if let Some((_, workspaces_id)) = self.doc.get(&ROOT, "workspaces").ok().flatten() {
            let ws_len = self.doc.length(&workspaces_id);
            for ws_idx in 0..ws_len {
                let ws_entry = match self.doc.get(&workspaces_id, ws_idx).ok().flatten() {
                    Some((_, id)) => id,
                    None => continue,
                };
                let ws_id_raw = match self.doc.get(&ws_entry, "id").ok().flatten() {
                    Some((Value::Scalar(s), _)) => match s.as_ref() {
                        ScalarValue::Uint(v) => *v,
                        _ => continue,
                    },
                    _ => continue,
                };
                let ws_id = WorkspaceId::from(ws_id_raw);
                let strips_id = match self.doc.get(&ws_entry, "strips").ok().flatten() {
                    Some((_, id)) => id,
                    None => continue,
                };
                let strips_len = self.doc.length(&strips_id);
                for strip_idx in 0..strips_len {
                    let strip_entry = match self.doc.get(&strips_id, strip_idx).ok().flatten() {
                        Some((_, id)) => id,
                        None => continue,
                    };
                    let strip_id_raw = match self.doc.get(&strip_entry, "id").ok().flatten() {
                        Some((Value::Scalar(s), _)) => match s.as_ref() {
                            ScalarValue::Uint(v) => *v,
                            _ => continue,
                        },
                        _ => continue,
                    };
                    let strip_id = StripId::from(strip_id_raw);
                    let groups_id = match self.doc.get(&strip_entry, "window_groups").ok().flatten()
                    {
                        Some((_, id)) => id,
                        None => continue,
                    };
                    let groups_len = self.doc.length(&groups_id);
                    for group_idx in 0..groups_len {
                        let group_entry = match self.doc.get(&groups_id, group_idx).ok().flatten() {
                            Some((_, id)) => id,
                            None => continue,
                        };
                        let group_id = match self.doc.get(&group_entry, "id").ok().flatten() {
                            Some((Value::Scalar(s), _)) => match s.as_ref() {
                                ScalarValue::Uint(v) => *v,
                                _ => continue,
                            },
                            _ => continue,
                        };
                        let maximized = match self.doc.get(&group_entry, "maximized").ok().flatten()
                        {
                            Some((Value::Scalar(s), _)) => match s.as_ref() {
                                ScalarValue::Boolean(v) => *v,
                                _ => false,
                            },
                            _ => false,
                        };
                        let width_pixels =
                            match self.doc.get(&group_entry, "width_pixels").ok().flatten() {
                                Some((Value::Scalar(s), _)) => match s.as_ref() {
                                    ScalarValue::Uint(v) => *v as u32,
                                    _ => 0,
                                },
                                _ => 0,
                            };

                        // Patch into the cached tree, synthesizing
                        // missing structure as needed.
                        let ws = match self.workspaces.iter_mut().find(|w| w.id == ws_id) {
                            Some(w) => w,
                            None => continue,
                        };
                        let strip = if let Some(s) = ws.strips.iter_mut().find(|s| s.id == strip_id)
                        {
                            s
                        } else {
                            ws.strips.push(Strip::new(strip_id, "Main"));
                            ws.strips.last_mut().expect("strip just pushed")
                        };
                        // Unit D: read the tabs list (if present) so we
                        // can rebuild the group's `tabs` vec. Tabs missing
                        // from the doc fall back to whatever the cache
                        // already had; new ids get freshly synthesized
                        // entries with the doc-side title + tile blob.
                        let mut doc_tabs: Vec<(u64, String, Vec<u8>)> = Vec::new();
                        if let Some((_, tabs_id)) =
                            self.doc.get(&group_entry, "tabs").ok().flatten()
                        {
                            let tabs_len = self.doc.length(&tabs_id);
                            for t_idx in 0..tabs_len {
                                let Some((_, tab_entry)) =
                                    self.doc.get(&tabs_id, t_idx).ok().flatten()
                                else {
                                    continue;
                                };
                                let id = match self.doc.get(&tab_entry, "id").ok().flatten() {
                                    Some((Value::Scalar(s), _)) => match s.as_ref() {
                                        ScalarValue::Uint(v) => *v,
                                        _ => continue,
                                    },
                                    _ => continue,
                                };
                                let title = match self.doc.get(&tab_entry, "title").ok().flatten() {
                                    Some((Value::Scalar(s), _)) => match s.as_ref() {
                                        ScalarValue::Str(v) => v.to_string(),
                                        _ => String::new(),
                                    },
                                    _ => String::new(),
                                };
                                let tile_blob: Vec<u8> =
                                    match self.doc.get(&tab_entry, "tile_json").ok().flatten() {
                                        Some((Value::Scalar(s), _)) => match s.as_ref() {
                                            ScalarValue::Bytes(b) => b.clone(),
                                            _ => Vec::new(),
                                        },
                                        _ => Vec::new(),
                                    };
                                doc_tabs.push((id, title, tile_blob));
                            }
                        }

                        if let Some(g) = strip.window_groups.iter_mut().find(|g| g.id == group_id) {
                            g.maximized = maximized;
                            // Round-tripping `WindowWidth::Pixels` is
                            // lossy — Narrow/Normal/Wide map to fixed
                            // pixel values, but the inverse mapping
                            // would silently coerce a user's custom
                            // resize back to one of the presets. Pick
                            // `Custom` always; it clamps 300..=1200
                            // internally.
                            if width_pixels > 0 {
                                g.width = WindowWidth::Custom(width_pixels);
                            }
                            if !doc_tabs.is_empty() {
                                let mut new_tabs: Vec<Tab> = Vec::with_capacity(doc_tabs.len());
                                for (tid, title, blob) in &doc_tabs {
                                    if let Some(existing) = g.tabs.iter().find(|t| t.id == *tid) {
                                        new_tabs.push(existing.clone());
                                    } else {
                                        let tile = if !blob.is_empty() {
                                            serde_json::from_slice::<TileContent>(blob).unwrap_or(
                                                TileContent::Stub {
                                                    kind: AppKind::Other(String::new()),
                                                    label: title.clone(),
                                                },
                                            )
                                        } else {
                                            TileContent::Stub {
                                                kind: AppKind::Other(String::new()),
                                                label: title.clone(),
                                            }
                                        };
                                        new_tabs.push(Tab {
                                            id: *tid,
                                            title: title.clone(),
                                            tile,
                                        });
                                    }
                                }
                                if g.tabs
                                    .iter()
                                    .all(|t| !doc_tabs.iter().any(|(id, _, _)| *id == t.id))
                                    || g.tabs.is_empty()
                                {
                                    g.active_tab = new_tabs.first().map(|t| t.id).unwrap_or(0);
                                }
                                g.tabs = new_tabs;
                            }
                        } else {
                            // Synthesize a placeholder group — it
                            // carries tabs from the doc when present.
                            let synthesized_tabs: Vec<Tab> = doc_tabs
                                .iter()
                                .map(|(tid, title, blob)| {
                                    let tile = if !blob.is_empty() {
                                        serde_json::from_slice::<TileContent>(blob).unwrap_or(
                                            TileContent::Stub {
                                                kind: AppKind::Other(String::new()),
                                                label: title.clone(),
                                            },
                                        )
                                    } else {
                                        TileContent::Stub {
                                            kind: AppKind::Other(String::new()),
                                            label: title.clone(),
                                        }
                                    };
                                    Tab {
                                        id: *tid,
                                        title: title.clone(),
                                        tile,
                                    }
                                })
                                .collect();
                            let active_tab = synthesized_tabs.first().map(|t| t.id).unwrap_or(0);
                            strip.window_groups.push(WindowGroup {
                                id: group_id,
                                tabs: synthesized_tabs,
                                active_tab,
                                width: if width_pixels > 0 {
                                    WindowWidth::Custom(width_pixels)
                                } else {
                                    WindowWidth::Normal
                                },
                                maximized,
                            });
                        }
                    }
                }
            }
        }
    }

    /// Walk the doc to the [`WindowGroup`] map for `group_id`. Returns
    /// the `ObjId` of the group entry alongside the indices
    /// `(ws_idx, strip_idx, group_idx)` so callers can mutate by id
    /// once they have a handle. Returns `None` if the group isn't yet
    /// in the doc — usually because [`Self::seed_doc_from_cache`]
    /// hasn't run for this group (e.g. a literal-construction test or
    /// a brand-new group that the harvester saw before the seed pass).
    /// Callers tolerate the doc being behind by skipping the doc-side
    /// write — the cache write already happened.
    fn locate_group(&self, group_id: u64) -> Option<(automerge::ObjId, usize, usize, usize)> {
        use automerge::{ROOT, ReadDoc, ScalarValue, Value};
        let (_, workspaces_id) = self.doc.get(&ROOT, "workspaces").ok().flatten()?;
        let ws_len = self.doc.length(&workspaces_id);
        for ws_idx in 0..ws_len {
            let (_, ws_entry) = self.doc.get(&workspaces_id, ws_idx).ok().flatten()?;
            let strips_id = match self.doc.get(&ws_entry, "strips").ok().flatten() {
                Some((_, id)) => id,
                None => continue,
            };
            let strips_len = self.doc.length(&strips_id);
            for strip_idx in 0..strips_len {
                let (_, strip_entry) = self.doc.get(&strips_id, strip_idx).ok().flatten()?;
                let groups_id = match self.doc.get(&strip_entry, "window_groups").ok().flatten() {
                    Some((_, id)) => id,
                    None => continue,
                };
                let groups_len = self.doc.length(&groups_id);
                for group_idx in 0..groups_len {
                    let (_, group_entry) = self.doc.get(&groups_id, group_idx).ok().flatten()?;
                    if let Some((Value::Scalar(s), _)) =
                        self.doc.get(&group_entry, "id").ok().flatten()
                    {
                        if let ScalarValue::Uint(id) = s.as_ref() {
                            if *id == group_id {
                                return Some((group_entry, ws_idx, strip_idx, group_idx));
                            }
                        }
                    }
                }
            }
        }
        None
    }

    /// Mirror a `WindowGroup.maximized` flip into the doc. Called by
    /// the overlay-host harvester after the cache write so peer-sync
    /// observes the change. No-op if the group isn't yet seeded into
    /// the doc — see [`Self::locate_group`] for the tolerance
    /// rationale.
    pub fn doc_set_maximized(&mut self, group_id: u64, new_value: bool) {
        use automerge::{ScalarValue, transaction::Transactable};
        if let Some((entry, _, _, _)) = self.locate_group(group_id) {
            let _ = self
                .doc
                .put(&entry, "maximized", ScalarValue::Boolean(new_value));
        }
    }

    /// Mirror a `WindowGroup.width` resize into the doc as a raw pixel
    /// scalar. Stored as `width_pixels: Uint` because the cache uses
    /// the `WindowWidth::Custom(p)` variant after a manual resize and
    /// peers don't need to round-trip the preset (`Narrow`/`Normal`/
    /// `Wide`) — they receive the resolved pixel value and clamp it
    /// on the cache side. Same null-tolerance as
    /// [`Self::doc_set_maximized`].
    pub fn doc_set_width(&mut self, group_id: u64, new_pixels: u32) {
        use automerge::{ScalarValue, transaction::Transactable};
        if let Some((entry, _, _, _)) = self.locate_group(group_id) {
            let _ = self
                .doc
                .put(&entry, "width_pixels", ScalarValue::Uint(new_pixels as u64));
        }
    }

    /// Walk the doc to the workspace map for `workspace_id` and return
    /// its `ObjId`. None on miss — used by the grant mirror helpers
    /// below, same null-tolerance as [`Self::locate_group`].
    fn locate_workspace(&self, workspace_id: WorkspaceId) -> Option<automerge::ObjId> {
        use automerge::{ROOT, ReadDoc, ScalarValue, Value};
        let (_, workspaces_id) = self.doc.get(&ROOT, "workspaces").ok().flatten()?;
        let len = self.doc.length(&workspaces_id);
        for i in 0..len {
            let (_, ws_entry) = self.doc.get(&workspaces_id, i).ok().flatten()?;
            if let Some((Value::Scalar(s), _)) = self.doc.get(&ws_entry, "id").ok().flatten() {
                if let ScalarValue::Uint(raw) = s.as_ref() {
                    if *raw == workspace_id.raw() {
                        return Some(ws_entry);
                    }
                }
            }
        }
        None
    }

    /// Write the tree to [`Self::persist_path`], if one is set.
    ///
    /// Called on every capability mutation. Persistence is deliberately
    /// synchronous and eager rather than batched on shutdown: a
    /// compositor that is killed rather than asked to exit must not
    /// forget which permissions the user granted, and the alternative —
    /// silently losing an `Always` grant on a crash — is the failure
    /// mode we are fixing.
    pub fn flush_to_disk(&self) {
        if let Some(path) = &self.persist_path
            && let Err(e) = crate::shell::persist::save_tree_to(self, path)
        {
            tracing::warn!("capability persistence failed: {e}");
        }
        self.export_fs_capabilities();
    }

    /// Write the capability list the filesystem enforces.
    ///
    /// The FUSE mount is a separate process with its own address space,
    /// so it cannot read `self`. It reads a JSON array of capabilities
    /// from the path its `--acl` flag names, and this is what writes it.
    /// Without this the filesystem's enforcement is real but always
    /// enforcing an empty list.
    ///
    /// Every workspace's capabilities are exported together, because
    /// `/var/workspace` is one mount for the whole session. Path scopes
    /// are absolute, so workspaces with disjoint subtrees stay
    /// disjoint — but two workspaces granting the *same* path to
    /// different subjects would union, which is a known limitation of
    /// having one mount rather than one per room.
    ///
    /// Written atomically via a temp file + rename, so the filesystem
    /// never reads a half-written list and concludes the user has no
    /// permissions.
    ///
    /// Two gaps this does **not** close, recorded here rather than left
    /// to be discovered:
    ///
    /// 1. `qos-fs` reads this file once, when the mount is established
    ///    (`fs::main::resolve_acl`). A grant or revoke made afterwards
    ///    is written here but does not reach an already-mounted
    ///    filesystem without a remount. Only expiry is evaluated live.
    /// 2. `os/usr/lib/systemd/system/fs.service` does not pass `--acl`,
    ///    so the mount in the shipped OS image runs unenforced. Wiring
    ///    it needs an ordering fix first: the unit starts before any
    ///    session exists, and `resolve_acl` refuses to mount when the
    ///    file it was pointed at is missing — correctly, but that would
    ///    turn "no session yet" into a failed boot.
    pub fn export_fs_capabilities(&self) {
        let Some(path) = &self.fs_acl_path else {
            return;
        };
        let caps: Vec<&CapOverride> = self
            .workspaces
            .iter()
            .flat_map(|w| w.capabilities.iter())
            .filter(|c| matches!(c.scope, Scope::FsPath(_)))
            .collect();
        let Ok(json) = serde_json::to_vec_pretty(&caps) else {
            return;
        };
        let tmp = path.with_extension("json.tmp");
        if let Some(parent) = path.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            tracing::warn!("filesystem capability export failed: {e}");
            return;
        }
        if let Err(e) = std::fs::write(&tmp, &json).and_then(|()| std::fs::rename(&tmp, path)) {
            tracing::warn!("filesystem capability export failed: {e}");
        }
    }

    /// Give every workspace that has no recorded owner to `user`.
    ///
    /// State files written before ownership existed have `owner: None`.
    /// Treating them as unowned forever would leave the user unable to
    /// grant anything in their own workspaces; treating unowned as
    /// "owned by anyone" is what the old
    /// `is_active_workspace_owner` did and is exactly the hole we are
    /// closing. Adopting them for the local user at load is the only
    /// answer that is both usable and closed: it names a specific
    /// owner, and it names the person sitting at the machine.
    ///
    /// Returns the ids adopted, so the caller can mirror them.
    pub fn adopt_ownerless_workspaces(&mut self, user: &UserId) -> Vec<WorkspaceId> {
        let mut adopted = Vec::new();
        for ws in &mut self.workspaces {
            if ws.owner.is_none() {
                ws.set_owner(user.clone());
                adopted.push(ws.id);
            }
        }
        for id in &adopted {
            self.doc_set_owner(*id, user);
            let caps: Vec<CapOverride> = self
                .workspaces
                .iter()
                .find(|w| w.id == *id)
                .map(|w| w.capabilities.clone())
                .unwrap_or_default();
            for cap in caps {
                self.doc_insert_capability(*id, &cap);
            }
        }
        if !adopted.is_empty() {
            self.flush_to_disk();
        }
        adopted
    }

    /// Mint a durable [`RoomId`] for every workspace that has none, and
    /// write it down.
    ///
    /// State files written before rooms existed deserialise with
    /// [`RoomId::nil`]. Leaving them there would leave those workspaces
    /// unable to collaborate at all (the nil cluster addresses nobody);
    /// minting one *without persisting it* would be worse, because the
    /// next boot would mint a different one and a workspace's room
    /// would change every restart — exactly the instability this whole
    /// identity exists to remove.
    ///
    /// So the mint and the flush are one operation, and the caller is
    /// responsible for having set [`Self::persist_path`] first. The
    /// compositor calls this right after it does, next to
    /// [`Self::adopt_ownerless_workspaces`], which has the same shape
    /// and the same reason.
    ///
    /// Returns the ids that were minted for, so the caller can log
    /// them; an empty vec means every workspace already had a room and
    /// nothing was written.
    pub fn adopt_roomless_workspaces(&mut self) -> Vec<WorkspaceId> {
        let mut adopted = Vec::new();
        for ws in &mut self.workspaces {
            if ws.room.is_nil() {
                ws.room = RoomId::new();
                adopted.push(ws.id);
            }
        }
        for id in &adopted {
            let room = self
                .workspaces
                .iter()
                .find(|w| w.id == *id)
                .map(|w| w.room)
                .unwrap_or_default();
            self.doc_set_room(*id, room);
        }
        if !adopted.is_empty() {
            self.flush_to_disk();
        }
        adopted
    }

    /// The durable room identity of `workspace_id`, if the tree has
    /// that workspace.
    ///
    /// The lookup the launch path needs: callers hold a
    /// [`WorkspaceId`] (the local handle) and have to turn it into the
    /// value the wire speaks.
    pub fn room_of(&self, workspace_id: WorkspaceId) -> Option<RoomId> {
        self.workspaces
            .iter()
            .find(|w| w.id == workspace_id)
            .map(|w| w.room)
    }

    /// Point `workspace_id` at `room`, mirroring the change to peers
    /// and to disk.
    ///
    /// This is how an invitee stops being in their own room and starts
    /// being in the inviter's: accepting an invite replaces the local
    /// workspace's freshly-minted identity with the one the token
    /// carried. Both ends then address the same cluster, which is the
    /// only way any delta crosses between them.
    ///
    /// Refuses a nil `room` — "join the unminted room" is never a
    /// meaningful request, and honouring it would drop a workspace out
    /// of collaboration silently. Returns whether the tree changed.
    pub fn set_room(&mut self, workspace_id: WorkspaceId, room: RoomId) -> bool {
        if room.is_nil() {
            return false;
        }
        let Some(ws) = self.workspaces.iter_mut().find(|w| w.id == workspace_id) else {
            return false;
        };
        if ws.room == room {
            return false;
        }
        ws.room = room;
        self.doc_set_room(workspace_id, room);
        self.flush_to_disk();
        true
    }

    /// Mirror a workspace's room into the automerge doc.
    ///
    /// Stored as a string rather than 16 bytes because that is what
    /// every other consumer of the value speaks (`QOS_WORKSPACE_ID`,
    /// the invite token, `CrdtRoute::cluster_id`), and a doc field
    /// that needs re-encoding at every read is a field that will
    /// eventually be re-encoded wrongly.
    ///
    /// Null-tolerant like the other `doc_*` mirrors: a doc that has
    /// not been seeded is left alone and the cache stays authoritative
    /// until the next seed.
    pub fn doc_set_room(&mut self, workspace_id: WorkspaceId, room: RoomId) {
        use automerge::transaction::Transactable;
        let Some(ws_entry) = self.locate_workspace(workspace_id) else {
            return;
        };
        let _ = self.doc.put(&ws_entry, "room", room.to_string());
        self.commit_room_change();
    }

    /// Grant a capability in `workspace_id` on behalf of `grantor`,
    /// mirroring it to peers and to disk.
    ///
    /// The single production entry point for handing out authority.
    /// Returns the new capability's id, or `None` when the grantor
    /// lacked the authority to delegate it — the check that makes
    /// ownership mean something.
    pub fn grant_in_workspace(
        &mut self,
        workspace_id: WorkspaceId,
        grantor: &UserId,
        cap: CapOverride,
    ) -> Option<String> {
        let ws = self.workspaces.iter_mut().find(|w| w.id == workspace_id)?;
        let id = ws.grant_as(grantor, cap)?;
        let stored = ws.capabilities.iter().find(|c| c.id == id).cloned()?;
        self.doc_insert_capability(workspace_id, &stored);
        self.flush_to_disk();
        Some(id)
    }

    /// Revoke a capability in `workspace_id` on behalf of `revoker`,
    /// mirroring the removal to peers and to disk.
    ///
    /// The mirror is the point. A revoke that only touched the local
    /// cache left every peer still holding the capability and let a
    /// restart bring it back.
    pub fn revoke_in_workspace(
        &mut self,
        workspace_id: WorkspaceId,
        revoker: &UserId,
        cap_id: &str,
    ) -> bool {
        let Some(ws) = self.workspaces.iter_mut().find(|w| w.id == workspace_id) else {
            return false;
        };
        if !ws.revoke_as(revoker, cap_id) {
            return false;
        }
        self.doc_revoke_capability(workspace_id, cap_id);
        self.flush_to_disk();
        true
    }

    /// Mirror a capability grant into the automerge doc so peer-sync
    /// ships it on the next tick and a restart reloads it.
    ///
    /// Capabilities live at `workspaces[i]/capabilities` as a **map
    /// keyed by capability id**, not a list. That choice is what makes
    /// revocation replicate: deleting a map key is a well-defined
    /// concurrent operation that merges, whereas deleting list index 3
    /// on two peers at once removes two different capabilities.
    ///
    /// Null-tolerant: a doc that has not been seeded yet is left alone
    /// rather than panicking, and the cache remains authoritative until
    /// the next seed.
    pub fn doc_insert_capability(&mut self, workspace_id: WorkspaceId, cap: &CapOverride) {
        use automerge::{ObjType, ReadDoc, ScalarValue, transaction::Transactable};
        let Some(ws_entry) = self.locate_workspace(workspace_id) else {
            return;
        };
        let caps_id = match self.doc.get(&ws_entry, "capabilities").ok().flatten() {
            Some((_, id)) => id,
            None => match self.doc.put_object(&ws_entry, "capabilities", ObjType::Map) {
                Ok(id) => id,
                Err(_) => return,
            },
        };
        let Ok(blob) = serde_json::to_vec(cap) else {
            return;
        };
        let _ = self.doc.put(&caps_id, &cap.id, ScalarValue::Bytes(blob));
        self.commit_room_change();
    }

    /// Mirror a revocation into the automerge doc.
    ///
    /// This is the half that used to be missing: without it a revoke
    /// mutated only the local cache, so a peer kept using a capability
    /// the owner had taken away, and a restart resurrected it.
    pub fn doc_revoke_capability(&mut self, workspace_id: WorkspaceId, cap_id: &str) {
        use automerge::{ReadDoc, transaction::Transactable};
        let Some(ws_entry) = self.locate_workspace(workspace_id) else {
            return;
        };
        let Some((_, caps_id)) = self.doc.get(&ws_entry, "capabilities").ok().flatten() else {
            return;
        };
        let _ = self.doc.delete(&caps_id, cap_id);
        self.commit_room_change();
    }

    /// Mirror the room's owner into the doc.
    pub fn doc_set_owner(&mut self, workspace_id: WorkspaceId, owner: &str) {
        use automerge::transaction::Transactable;
        let Some(ws_entry) = self.locate_workspace(workspace_id) else {
            return;
        };
        let _ = self.doc.put(&ws_entry, "owner", owner);
        self.commit_room_change();
    }

    /// Read a workspace's capabilities and owner back out of the doc.
    ///
    /// Called by [`Self::rebuild_cache_from_doc`] after applying peer
    /// changes. Capabilities absent from the doc are absent from the
    /// cache — that is what makes a peer's revocation take effect
    /// locally rather than merely being recorded somewhere.
    fn doc_read_capabilities(
        &self,
        ws_entry: &automerge::ObjId,
    ) -> Option<(Option<UserId>, Vec<CapOverride>)> {
        use automerge::{ReadDoc, ScalarValue, Value};
        let owner = match self.doc.get(ws_entry, "owner").ok().flatten() {
            Some((Value::Scalar(s), _)) => match s.as_ref() {
                ScalarValue::Str(v) => Some(v.to_string()),
                _ => None,
            },
            _ => None,
        };
        let (_, caps_id) = self.doc.get(ws_entry, "capabilities").ok().flatten()?;
        let mut caps = Vec::new();
        for key in self.doc.keys(&caps_id) {
            if let Some((Value::Scalar(s), _)) = self.doc.get(&caps_id, &key).ok().flatten()
                && let ScalarValue::Bytes(blob) = s.as_ref()
                && let Ok(cap) = serde_json::from_slice::<CapOverride>(blob)
            {
                caps.push(cap);
            }
        }
        Some((owner, caps))
    }

    /// Walk the doc to the `Tab` map for `(strip_id, tab_id)`. Returns
    /// the parent group entry, the tabs list, the tab entry, and the
    /// tab's index in the list. Strip id disambiguates if a tab id ever
    /// collided across strips. None on miss for the same reasons as
    /// [`Self::locate_group`].
    fn locate_tab(
        &self,
        strip_id: StripId,
        tab_id: u64,
    ) -> Option<(automerge::ObjId, automerge::ObjId, automerge::ObjId, usize)> {
        use automerge::{ROOT, ReadDoc, ScalarValue, Value};
        let (_, workspaces_id) = self.doc.get(&ROOT, "workspaces").ok().flatten()?;
        let ws_len = self.doc.length(&workspaces_id);
        for ws_idx in 0..ws_len {
            let (_, ws_entry) = self.doc.get(&workspaces_id, ws_idx).ok().flatten()?;
            let strips_id = match self.doc.get(&ws_entry, "strips").ok().flatten() {
                Some((_, id)) => id,
                None => continue,
            };
            let strips_len = self.doc.length(&strips_id);
            for s_idx in 0..strips_len {
                let (_, strip_entry) = self.doc.get(&strips_id, s_idx).ok().flatten()?;
                let strip_id_matches = match self.doc.get(&strip_entry, "id").ok().flatten() {
                    Some((Value::Scalar(s), _)) => match s.as_ref() {
                        ScalarValue::Uint(id) => *id == strip_id.raw(),
                        _ => false,
                    },
                    _ => false,
                };
                if !strip_id_matches {
                    continue;
                }
                let groups_id = match self.doc.get(&strip_entry, "window_groups").ok().flatten() {
                    Some((_, id)) => id,
                    None => continue,
                };
                let groups_len = self.doc.length(&groups_id);
                for g_idx in 0..groups_len {
                    let (_, group_entry) = self.doc.get(&groups_id, g_idx).ok().flatten()?;
                    let tabs_id = match self.doc.get(&group_entry, "tabs").ok().flatten() {
                        Some((_, id)) => id,
                        None => continue,
                    };
                    let tabs_len = self.doc.length(&tabs_id);
                    for t_idx in 0..tabs_len {
                        let (_, tab_entry) = self.doc.get(&tabs_id, t_idx).ok().flatten()?;
                        if let Some((Value::Scalar(s), _)) =
                            self.doc.get(&tab_entry, "id").ok().flatten()
                        {
                            if let ScalarValue::Uint(id) = s.as_ref() {
                                if *id == tab_id {
                                    return Some((group_entry, tabs_id, tab_entry, t_idx));
                                }
                            }
                        }
                    }
                }
            }
        }
        None
    }

    /// Mirror a `move_tab_across_strips` mutation into the doc. v1
    /// captures the tab's identifying scalars from the source, deletes
    /// it from the source list, and appends a fresh map (with the same
    /// scalars) to the destination strip's first window-group's `tabs`
    /// list. Returns `false` and is a no-op if the source tab or
    /// destination strip aren't seeded into the doc.
    pub fn doc_move_tab_across_strips(
        &mut self,
        src_strip: StripId,
        dst_strip: StripId,
        tab_id: u64,
    ) -> bool {
        use automerge::{ObjType, ROOT, ReadDoc, ScalarValue, Value, transaction::Transactable};
        // Capture source.
        let Some((_, src_tabs, src_tab_entry, src_idx)) = self.locate_tab(src_strip, tab_id) else {
            return false;
        };
        let title = match self.doc.get(&src_tab_entry, "title").ok().flatten() {
            Some((Value::Scalar(s), _)) => match s.as_ref() {
                ScalarValue::Str(v) => v.to_string(),
                _ => String::new(),
            },
            _ => String::new(),
        };
        let tile_blob: Vec<u8> = match self.doc.get(&src_tab_entry, "tile_json").ok().flatten() {
            Some((Value::Scalar(s), _)) => match s.as_ref() {
                ScalarValue::Bytes(b) => b.clone(),
                _ => Vec::new(),
            },
            _ => Vec::new(),
        };

        // Delete from source.
        let _ = self.doc.delete(&src_tabs, src_idx);

        // Locate destination strip and its first window-group's tabs list.
        let Some((_, workspaces_id)) = self.doc.get(&ROOT, "workspaces").ok().flatten() else {
            return false;
        };
        let ws_len = self.doc.length(&workspaces_id);
        for ws_idx in 0..ws_len {
            let Some((_, ws_entry)) = self.doc.get(&workspaces_id, ws_idx).ok().flatten() else {
                continue;
            };
            let Some((_, strips_id)) = self.doc.get(&ws_entry, "strips").ok().flatten() else {
                continue;
            };
            let strips_len = self.doc.length(&strips_id);
            for s_idx in 0..strips_len {
                let Some((_, strip_entry)) = self.doc.get(&strips_id, s_idx).ok().flatten() else {
                    continue;
                };
                let dst_matches = matches!(self.doc.get(&strip_entry, "id").ok().flatten(),
                        Some((Value::Scalar(s), _)) if matches!(s.as_ref(), ScalarValue::Uint(id) if *id == dst_strip.raw()));
                if !dst_matches {
                    continue;
                }
                let Some((_, groups_id)) =
                    self.doc.get(&strip_entry, "window_groups").ok().flatten()
                else {
                    return false;
                };
                if self.doc.length(&groups_id) == 0 {
                    return false;
                }
                let Some((_, first_group)) = self.doc.get(&groups_id, 0_usize).ok().flatten()
                else {
                    return false;
                };
                let tabs_id = match self.doc.get(&first_group, "tabs").ok().flatten() {
                    Some((_, id)) => id,
                    None => match self.doc.put_object(&first_group, "tabs", ObjType::List) {
                        Ok(id) => id,
                        Err(_) => return false,
                    },
                };
                let new_idx = self.doc.length(&tabs_id);
                let Ok(tab_entry) = self.doc.insert_object(&tabs_id, new_idx, ObjType::Map) else {
                    return false;
                };
                let _ = self.doc.put(&tab_entry, "id", ScalarValue::Uint(tab_id));
                let _ = self.doc.put(&tab_entry, "title", title);
                if !tile_blob.is_empty() {
                    let _ = self
                        .doc
                        .put(&tab_entry, "tile_json", ScalarValue::Bytes(tile_blob));
                }
                return true;
            }
        }
        false
    }

    /// Mirror a [`Self::reorder_group_within_strip`] cache move into the
    /// automerge doc: delete the group's existing entry from the strip's
    /// `window_groups` list and re-insert a fresh map carrying the group's
    /// full content (id, `maximized`, `width_pixels`, and its `tabs`) at
    /// `new_index`. Rebuilt from the passed-in cache [`WindowGroup`] rather
    /// than read back out of the doc, so it can't drift from what the cache
    /// just committed.
    ///
    /// Tolerant of an unseeded doc: if the strip / group isn't present it
    /// no-ops (the cache write already happened; a later
    /// [`Self::seed_doc_from_cache`] picks up the new order). Same shape
    /// as [`Self::doc_move_tab_across_strips`].
    pub fn doc_reorder_group_within_strip(
        &mut self,
        strip_id: StripId,
        group: &WindowGroup,
        new_index: usize,
    ) {
        use automerge::{ObjType, ROOT, ReadDoc, ScalarValue, Value, transaction::Transactable};
        // Locate the strip's window_groups list.
        let Some((_, workspaces_id)) = self.doc.get(&ROOT, "workspaces").ok().flatten() else {
            return;
        };
        let ws_len = self.doc.length(&workspaces_id);
        for ws_idx in 0..ws_len {
            let Some((_, ws_entry)) = self.doc.get(&workspaces_id, ws_idx).ok().flatten() else {
                continue;
            };
            let Some((_, strips_id)) = self.doc.get(&ws_entry, "strips").ok().flatten() else {
                continue;
            };
            let strips_len = self.doc.length(&strips_id);
            for s_idx in 0..strips_len {
                let Some((_, strip_entry)) = self.doc.get(&strips_id, s_idx).ok().flatten() else {
                    continue;
                };
                let matches = matches!(
                    self.doc.get(&strip_entry, "id").ok().flatten(),
                    Some((Value::Scalar(s), _))
                        if matches!(s.as_ref(), ScalarValue::Uint(id) if *id == strip_id.raw())
                );
                if !matches {
                    continue;
                }
                let Some((_, groups_id)) =
                    self.doc.get(&strip_entry, "window_groups").ok().flatten()
                else {
                    return;
                };
                // Find the group's current doc index by id.
                let glen = self.doc.length(&groups_id);
                let mut cur: Option<usize> = None;
                for gi in 0..glen {
                    if let Some((_, g_entry)) = self.doc.get(&groups_id, gi).ok().flatten() {
                        if matches!(
                            self.doc.get(&g_entry, "id").ok().flatten(),
                            Some((Value::Scalar(s), _))
                                if matches!(s.as_ref(), ScalarValue::Uint(id) if *id == group.id)
                        ) {
                            cur = Some(gi);
                            break;
                        }
                    }
                }
                let Some(cur) = cur else { return };
                let _ = self.doc.delete(&groups_id, cur);
                let dest = new_index.min(self.doc.length(&groups_id));
                let Ok(group_entry) = self.doc.insert_object(&groups_id, dest, ObjType::Map) else {
                    return;
                };
                let _ = self
                    .doc
                    .put(&group_entry, "id", ScalarValue::Uint(group.id));
                let _ = self.doc.put(
                    &group_entry,
                    "maximized",
                    ScalarValue::Boolean(group.maximized),
                );
                let _ = self.doc.put(
                    &group_entry,
                    "width_pixels",
                    ScalarValue::Uint(group.width.pixels() as u64),
                );
                if let Ok(tabs_id) = self.doc.put_object(&group_entry, "tabs", ObjType::List) {
                    for (l, tab) in group.tabs.iter().enumerate() {
                        if let Ok(tab_entry) = self.doc.insert_object(&tabs_id, l, ObjType::Map) {
                            let _ = self.doc.put(&tab_entry, "id", ScalarValue::Uint(tab.id));
                            let _ = self.doc.put(&tab_entry, "title", tab.title.clone());
                            if let Ok(blob) = serde_json::to_vec(&tab.tile) {
                                let _ =
                                    self.doc
                                        .put(&tab_entry, "tile_json", ScalarValue::Bytes(blob));
                            }
                        }
                    }
                }
                return;
            }
        }
    }

    /// Mirror an `apply_drop`-style insert: append a tab map to the
    /// active workspace's first strip's first window-group's `tabs`
    /// list. v1 hardcodes "first/first" because that's what
    /// `apply_drop` itself targets when no specific group is named.
    /// No-op if the doc isn't seeded for the active workspace.
    pub fn doc_append_tab_to_first_group_of_active_workspace(
        &mut self,
        tab_id: u64,
        title: String,
        tile_blob: Vec<u8>,
    ) -> bool {
        use automerge::{ObjType, ROOT, ReadDoc, ScalarValue, Value, transaction::Transactable};
        let Some((_, workspaces_id)) = self.doc.get(&ROOT, "workspaces").ok().flatten() else {
            return false;
        };
        let ws_len = self.doc.length(&workspaces_id);
        let active_raw = self.active.raw();
        for ws_idx in 0..ws_len {
            let Some((_, ws_entry)) = self.doc.get(&workspaces_id, ws_idx).ok().flatten() else {
                continue;
            };
            let active_matches = matches!(self.doc.get(&ws_entry, "id").ok().flatten(),
                    Some((Value::Scalar(s), _)) if matches!(s.as_ref(), ScalarValue::Uint(id) if *id == active_raw));
            if !active_matches {
                continue;
            }
            let Some((_, strips_id)) = self.doc.get(&ws_entry, "strips").ok().flatten() else {
                return false;
            };
            if self.doc.length(&strips_id) == 0 {
                return false;
            }
            let Some((_, strip_entry)) = self.doc.get(&strips_id, 0_usize).ok().flatten() else {
                return false;
            };
            let Some((_, groups_id)) = self.doc.get(&strip_entry, "window_groups").ok().flatten()
            else {
                return false;
            };
            if self.doc.length(&groups_id) == 0 {
                return false;
            }
            let Some((_, group_entry)) = self.doc.get(&groups_id, 0_usize).ok().flatten() else {
                return false;
            };
            let tabs_id = match self.doc.get(&group_entry, "tabs").ok().flatten() {
                Some((_, id)) => id,
                None => match self.doc.put_object(&group_entry, "tabs", ObjType::List) {
                    Ok(id) => id,
                    Err(_) => return false,
                },
            };
            let new_idx = self.doc.length(&tabs_id);
            let Ok(tab_entry) = self.doc.insert_object(&tabs_id, new_idx, ObjType::Map) else {
                return false;
            };
            let _ = self.doc.put(&tab_entry, "id", ScalarValue::Uint(tab_id));
            let _ = self.doc.put(&tab_entry, "title", title);
            if !tile_blob.is_empty() {
                let _ = self
                    .doc
                    .put(&tab_entry, "tile_json", ScalarValue::Bytes(tile_blob));
            }
            return true;
        }
        false
    }

    /// Cursor-targeted counterpart of
    /// [`Self::doc_append_tab_to_first_group_of_active_workspace`]: mirror
    /// a tab append into the automerge doc under the active workspace's
    /// group whose `id` matches `group_id`, scanning every strip. Used by
    /// the file-drop router so a drop onto a non-first group replicates to
    /// peers on the same object path the cache append wrote to.
    ///
    /// Returns `true` if the group was found and the tab written, `false`
    /// otherwise (doc not seeded, active workspace absent, or no group
    /// with that id) — the caller treats a `false` return as "cache-only
    /// mirror; peer-sync will backfill on the next full snapshot".
    pub fn doc_append_tab_to_group_of_active_workspace(
        &mut self,
        group_id: u64,
        tab_id: u64,
        title: String,
        tile_blob: Vec<u8>,
    ) -> bool {
        use automerge::{ObjType, ROOT, ReadDoc, ScalarValue, Value, transaction::Transactable};
        let Some((_, workspaces_id)) = self.doc.get(&ROOT, "workspaces").ok().flatten() else {
            return false;
        };
        let ws_len = self.doc.length(&workspaces_id);
        let active_raw = self.active.raw();
        for ws_idx in 0..ws_len {
            let Some((_, ws_entry)) = self.doc.get(&workspaces_id, ws_idx).ok().flatten() else {
                continue;
            };
            let active_matches = matches!(
                self.doc.get(&ws_entry, "id").ok().flatten(),
                Some((Value::Scalar(s), _)) if matches!(s.as_ref(), ScalarValue::Uint(id) if *id == active_raw));
            if !active_matches {
                continue;
            }
            let Some((_, strips_id)) = self.doc.get(&ws_entry, "strips").ok().flatten() else {
                return false;
            };
            let strips_len = self.doc.length(&strips_id);
            for strip_idx in 0..strips_len {
                let Some((_, strip_entry)) = self.doc.get(&strips_id, strip_idx).ok().flatten()
                else {
                    continue;
                };
                let Some((_, groups_id)) =
                    self.doc.get(&strip_entry, "window_groups").ok().flatten()
                else {
                    continue;
                };
                let groups_len = self.doc.length(&groups_id);
                for group_idx in 0..groups_len {
                    let Some((_, group_entry)) = self.doc.get(&groups_id, group_idx).ok().flatten()
                    else {
                        continue;
                    };
                    let id_matches = matches!(
                        self.doc.get(&group_entry, "id").ok().flatten(),
                        Some((Value::Scalar(s), _)) if matches!(s.as_ref(), ScalarValue::Uint(id) if *id == group_id));
                    if !id_matches {
                        continue;
                    }
                    let tabs_id = match self.doc.get(&group_entry, "tabs").ok().flatten() {
                        Some((_, id)) => id,
                        None => match self.doc.put_object(&group_entry, "tabs", ObjType::List) {
                            Ok(id) => id,
                            Err(_) => return false,
                        },
                    };
                    let new_idx = self.doc.length(&tabs_id);
                    let Ok(tab_entry) = self.doc.insert_object(&tabs_id, new_idx, ObjType::Map)
                    else {
                        return false;
                    };
                    let _ = self.doc.put(&tab_entry, "id", ScalarValue::Uint(tab_id));
                    let _ = self.doc.put(&tab_entry, "title", title);
                    if !tile_blob.is_empty() {
                        let _ =
                            self.doc
                                .put(&tab_entry, "tile_json", ScalarValue::Bytes(tile_blob));
                    }
                    return true;
                }
            }
            return false;
        }
        false
    }

    pub fn bootstrap_default() -> Self {
        let ws_id = WorkspaceId::new();
        let strip_id = StripId::new();
        let group_id = next_id();
        let tab_notes = next_id();
        let tab_files = next_id();
        let tab_terminal = next_id();
        let tab_browser = next_id();
        // First (Home) workspace seeds the demo Notes/Files/Terminal/Browser
        // tabs at `WindowWidth::Wide` so the rendered white card fills more
        // than half the viewport on a 1600 px-wide first-run desktop —
        // matches `WindowFrame.tsx`'s "fill the container" behaviour.
        let home = Workspace {
            id: ws_id,
            room: RoomId::new(),
            name: "Home".into(),
            strips: vec![Strip {
                id: strip_id,
                name: "Main".into(),
                window_groups: vec![WindowGroup {
                    id: group_id,
                    tabs: vec![
                        Tab {
                            id: tab_notes,
                            title: "Notes".into(),
                            tile: TileContent::AppPlaceholder {
                                kind: AppKind::Other("Notes".into()),
                                label: "Notes".into(),
                            },
                        },
                        Tab {
                            id: tab_files,
                            title: "Files".into(),
                            tile: TileContent::AppPlaceholder {
                                kind: AppKind::FileExplorer,
                                label: "Files".into(),
                            },
                        },
                        Tab {
                            id: tab_terminal,
                            title: "Terminal".into(),
                            tile: TileContent::AppPlaceholder {
                                kind: AppKind::Terminal,
                                label: "Terminal".into(),
                            },
                        },
                        Tab {
                            id: tab_browser,
                            title: "Browser".into(),
                            tile: TileContent::Stub {
                                kind: AppKind::Browser,
                                label: "Browser".into(),
                            },
                        },
                    ],
                    active_tab: tab_notes,
                    width: WindowWidth::Wide,
                    maximized: false,
                }],
            }],
            roles: vec![],
            workspace_files: vec![],
            // Home is active on first boot; seed one demo peer so
            // the top-bar avatar stack paints immediately.
            // `TopBarChrome::sync_from_tree` prefers the active
            // workspace's roster (with a tree-level fallback), so
            // without this seed the right cluster would be empty
            // until the user navigates to Research. The local user
            // ("me") is deliberately not seeded here so
            // `draw_ws_settings` synthesizes its "You" tile from the
            // fallback path — keeping the General-tab fill count
            // stable for the Roles-tab parity-spec contract. Hex
            // matches `nirios-concept/src/data.ts`'s `cursorColor`.
            collaborators: vec![
                Collaborator::new("alice", "Alice")
                    .with_cursor_color("#ec4899")
                    .with_role("editor"),
            ],
            owner: None,
            capabilities: Vec::new(),
            shared_surfaces: HashMap::new(),
        };

        let mut active_strip = HashMap::new();
        active_strip.insert(ws_id, strip_id);
        let mut active_group = HashMap::new();
        active_group.insert(strip_id, group_id);
        let mut tree = Self {
            workspaces: vec![home],
            active: ws_id,
            active_strip,
            active_group,
            collaborators: vec![],
            ..Default::default()
        };

        // Research and Design seed empty strips so the overview overlay
        // paints a meaningful 3-card grid on first run, matching the
        // committed `e2e-open-close-overview-settled.png` golden. Research
        // also seeds two demo collaborators so the top-bar avatar stack +
        // per-tab presence dots have something to render — the canonical
        // hex colours mirror `nirios-concept/src/data.ts`'s `cursorColor`.
        let research_id = tree.append_empty_workspace("Research");
        if let Some(ws) = tree.workspaces.iter_mut().find(|w| w.id == research_id) {
            ws.collaborators.push(
                Collaborator::new("alice", "Alice")
                    .with_cursor_color("#ec4899")
                    .with_role("editor"),
            );
            ws.collaborators.push(
                Collaborator::new("bob", "Bob")
                    .with_cursor_color("#f97316")
                    .with_role("viewer"),
            );
        }
        tree.append_empty_workspace("Design");

        // Home stays active — `append_empty_workspace` doesn't change
        // `tree.active`, so the user lands on the populated workspace
        // rather than an empty one on first boot.
        //
        // Seed the automerge backing doc from the now-populated cache
        // so peer-sync exports (`current_heads`, `export_changes_since`)
        // reflect the bootstrap state. `append_empty_workspace` already
        // mirrors each appended workspace into the doc; this call
        // additionally writes the initial Home workspace + the active
        // pointer that the cache literal seeded directly.
        tree.seed_doc_from_cache();
        tree
    }

    pub fn append_empty_workspace(&mut self, name: &str) -> WorkspaceId {
        use automerge::{ObjType, ROOT, ReadDoc, ScalarValue, transaction::Transactable};
        let ws_id = WorkspaceId::new();
        let strip_id = StripId::new();
        // A workspace the user just created is a room that now exists;
        // it gets its identity at the same moment it gets its id, not
        // the first time someone tries to share it.
        let room = RoomId::new();
        let ws = Workspace {
            id: ws_id,
            room,
            name: name.into(),
            strips: vec![Strip {
                id: strip_id,
                name: "Main".into(),
                window_groups: vec![],
            }],
            roles: vec![],
            workspace_files: vec![],
            collaborators: vec![],
            owner: None,
            capabilities: Vec::new(),
            shared_surfaces: HashMap::new(),
        };
        self.active_strip.insert(ws_id, strip_id);
        self.workspaces.push(ws);

        // Mirror the append into the doc-side `workspaces: List<Map>`
        // so peer-sync sees the new workspace as a discrete change.
        // Creates the list lazily — `bootstrap_default` calls
        // `seed_doc_from_cache` after construction, but tests / future
        // call sites may invoke this on a tree that hasn't been seeded
        // yet.
        let workspaces_id = match self.doc.get(&ROOT, "workspaces").ok().flatten() {
            Some((_, id)) => id,
            None => self
                .doc
                .put_object(&ROOT, "workspaces", ObjType::List)
                .expect("create workspaces list"),
        };
        let idx = self.doc.length(&workspaces_id);
        let entry = self
            .doc
            .insert_object(&workspaces_id, idx, ObjType::Map)
            .expect("insert workspace map");
        let _ = self.doc.put(&entry, "id", ScalarValue::Uint(ws_id.raw()));
        let _ = self.doc.put(&entry, "name", name.to_string());
        // The room goes into the doc with the workspace, not after it:
        // a peer that receives this change has to learn the identity in
        // the same change that tells it the workspace exists, or it
        // hydrates a workspace it cannot address.
        let _ = self.doc.put(&entry, "room", room.to_string());
        ws_id
    }

    pub fn set_active(&mut self, id: WorkspaceId) {
        use automerge::{ROOT, ScalarValue, transaction::Transactable};
        if self.workspaces.iter().any(|w| w.id == id) {
            self.active = id;
            let _ = self
                .doc
                .put(&ROOT, "active", ScalarValue::Uint(self.active.raw()));
        }
    }

    pub fn active_workspace(&self) -> Option<&Workspace> {
        self.workspaces.iter().find(|w| w.id == self.active)
    }

    pub fn active_workspace_mut(&mut self) -> Option<&mut Workspace> {
        self.workspaces.iter_mut().find(|w| w.id == self.active)
    }

    pub fn switch_up(&mut self) -> bool {
        use automerge::{ROOT, ScalarValue, transaction::Transactable};
        let idx = self
            .workspaces
            .iter()
            .position(|w| w.id == self.active)
            .unwrap_or(0);
        if idx == 0 {
            return false;
        }
        self.active = self.workspaces[idx - 1].id;
        let _ = self
            .doc
            .put(&ROOT, "active", ScalarValue::Uint(self.active.raw()));
        true
    }

    pub fn switch_down(&mut self) -> bool {
        use automerge::{ROOT, ScalarValue, transaction::Transactable};
        let idx = self
            .workspaces
            .iter()
            .position(|w| w.id == self.active)
            .unwrap_or(0);
        if idx + 1 >= self.workspaces.len() {
            return false;
        }
        self.active = self.workspaces[idx + 1].id;
        let _ = self
            .doc
            .put(&ROOT, "active", ScalarValue::Uint(self.active.raw()));
        true
    }

    // -----------------------------------------------------------------
    // Active-group pointer
    // -----------------------------------------------------------------
    //
    // The shell picks one [`GroupId`] per [`StripId`] as the "active"
    // group — the receiver of focus, keyboard input, and presence
    // broadcasts. The pointer lives here (in the persisted state tree)
    // so multiple chrome surfaces can read it without threading a
    // separate handle through, and so it survives reload.
    //
    // The interesting case is the close fade: when the user closes the
    // last tab of the active group, the widget tree latches a 180 ms
    // alpha+scale shrink before pruning (see `WindowGroup::start_closing`
    // in `quick-ui`). Without intervention the active-group pointer
    // continues to resolve to the closing group for the duration of the
    // animation, sending focus and keyboard input to a fading corpse.
    // [`Self::nudge_active_group_off_closing`] is the reconciler that
    // advances the pointer to the next surviving sibling at fade-start
    // time; the shell is expected to call it from its per-tick loop with
    // the set of group ids whose widgets currently report
    // `is_closing()`.

    /// Look up the currently-active [`GroupId`] for the given strip, or
    /// `None` if no entry has been seeded. Callers that want a default
    /// (the leftmost group, say) compose this with their own fallback.
    pub fn active_group_for(&self, strip_id: StripId) -> Option<GroupId> {
        self.active_group.get(&strip_id).copied()
    }

    /// Mark `group_id` as the active group of `strip_id`. The id is not
    /// validated against the strip's actual `window_groups` membership —
    /// callers that care can check first; the shell's reconciler is
    /// expected to be the sole writer.
    pub fn set_active_group(&mut self, strip_id: StripId, group_id: GroupId) {
        self.active_group.insert(strip_id, group_id);
    }

    /// Resolve the active group on `strip_id`, *as observed under the
    /// given `is_closing` predicate*. If the recorded active group is
    /// reported closing, this returns the next non-closing sibling to
    /// the right; if there isn't one, it falls back to the previous
    /// non-closing sibling. Pure read — does not mutate the map.
    /// Returns `None` if the strip has no surviving (i.e. non-closing)
    /// groups at all, or if the strip itself doesn't exist.
    pub fn resolve_active_group(
        &self,
        strip_id: StripId,
        is_closing: impl Fn(GroupId) -> bool,
    ) -> Option<GroupId> {
        let strip = self
            .workspaces
            .iter()
            .flat_map(|w| w.strips.iter())
            .find(|s| s.id == strip_id)?;
        let recorded = self.active_group.get(&strip_id).copied();
        // Helper: first non-closing group in the strip starting at
        // `idx`, walking right (`step = 1`) or left (`step = -1`).
        let scan_from = |start: isize, step: isize| -> Option<GroupId> {
            let n = strip.window_groups.len() as isize;
            let mut i = start;
            while i >= 0 && i < n {
                let g = &strip.window_groups[i as usize];
                if !is_closing(g.id) {
                    return Some(g.id);
                }
                i += step;
            }
            None
        };
        match recorded {
            Some(id) => {
                if !is_closing(id) {
                    return Some(id);
                }
                // Recorded group is closing — find its index, then
                // scan right for a survivor; if none, scan left.
                let idx = strip
                    .window_groups
                    .iter()
                    .position(|g| g.id == id)
                    .map(|i| i as isize)
                    .unwrap_or(-1);
                if idx < 0 {
                    // Recorded group no longer in the strip; fall
                    // through to "first non-closing" as a default.
                    return scan_from(0, 1);
                }
                scan_from(idx + 1, 1).or_else(|| scan_from(idx - 1, -1))
            }
            None => scan_from(0, 1),
        }
    }

    /// Mirror a cross-strip tab drag-drop into the persisted tree (#151).
    ///
    /// The widget side of the cross-strip drag (`GroupRow::accept_cross_strip_drop`)
    /// only mutates the widget tree — without this writeback the tab would
    /// pop back to its source strip on the next shell restart because the
    /// persisted state still records it under the source.
    ///
    /// Walks both strips in the active workspace, removes the matching
    /// `Tab` from the source's groups (pruning any group it leaves empty,
    /// matching the widget's "source group is removed if last tab moved out"
    /// pattern), then appends a brand-new single-tab `WindowGroup` to the
    /// destination strip carrying the moved tab. This keeps the persisted
    /// shape aligned with `accept_cross_strip_drop`'s "splice into a new
    /// group" semantics.
    ///
    /// Idempotent / forgiving: returns `false` if `tab_id` isn't found on
    /// the source strip (the harvest may run on a frame where the tab
    /// already moved due to a different code path), or if either strip
    /// id doesn't resolve. Returns `true` on a successful move.
    ///
    /// Searches across every workspace because cross-strip drops can in
    /// principle land on a strip outside the currently-active workspace
    /// (e.g. a future "drag tab into another workspace's strip via the
    /// overview" flow). For the v1 within-active-workspace case the same
    /// loop matches both strips on the active workspace's `strips` vec.
    pub fn move_tab_across_strips(
        &mut self,
        src_strip_id: StripId,
        dest_strip_id: StripId,
        tab_id: TabId,
    ) -> bool {
        if src_strip_id == dest_strip_id {
            // Return-to-source path on the widget side is a re-splice into
            // a new group on the same strip. The persisted tree's
            // structure stays the same: the tab is still on this strip,
            // just (potentially) in a different group. We don't try to
            // mirror the widget's "now in a brand new group at gap_idx"
            // shape because the tree's group ids are widget-side ids and
            // we'd need to invent one. Skipping the write is safe because
            // the tab is still attributed to this strip — a shell restart
            // simply rebuilds the widget tree from the unchanged persisted
            // groups, which is the same outcome as a "drop in same strip
            // creates new group" interaction restoring after restart.
            return false;
        }
        // Phase 1: remove the tab from the source strip. We have to scan
        // every workspace's strips because the StripId is workspace-
        // agnostic by design.
        let mut moved_tab: Option<Tab> = None;
        let mut prune_group_at: Option<(usize, usize, usize)> = None; // (ws_idx, strip_idx, group_idx)
        'outer: for (ws_idx, ws) in self.workspaces.iter_mut().enumerate() {
            for (strip_idx, strip) in ws.strips.iter_mut().enumerate() {
                if strip.id != src_strip_id {
                    continue;
                }
                for (group_idx, group) in strip.window_groups.iter_mut().enumerate() {
                    if let Some(pos) = group.tabs.iter().position(|t| t.id == tab_id) {
                        let removed = group.tabs.remove(pos);
                        // If the moved tab was active, promote the
                        // first remaining sibling so a future re-render
                        // doesn't dereference a stale active id.
                        if group.active_tab == tab_id {
                            if let Some(first) = group.tabs.first() {
                                group.active_tab = first.id;
                            }
                        }
                        if group.tabs.is_empty() {
                            prune_group_at = Some((ws_idx, strip_idx, group_idx));
                        }
                        moved_tab = Some(removed);
                        break 'outer;
                    }
                }
            }
        }
        let Some(moved) = moved_tab else { return false };
        if let Some((ws_idx, strip_idx, group_idx)) = prune_group_at {
            self.workspaces[ws_idx].strips[strip_idx]
                .window_groups
                .remove(group_idx);
        }
        // Phase 2: append to destination strip as a brand-new group with
        // the moved tab as its sole member. Mirrors the widget side's
        // `accept_cross_strip_drop` (splice into a brand-new
        // `WindowGroup`).
        let new_group_id = next_id();
        let active_tab = moved.id;
        let new_group = WindowGroup {
            id: new_group_id,
            tabs: vec![moved],
            active_tab,
            width: WindowWidth::Normal,
            maximized: false,
        };
        for ws in self.workspaces.iter_mut() {
            for strip in ws.strips.iter_mut() {
                if strip.id == dest_strip_id {
                    strip.window_groups.push(new_group);
                    // Unit D: mirror the cache mutation into the doc.
                    // The widget side has already accepted the drop;
                    // the doc-side write keeps peer-sync coherent.
                    self.doc_move_tab_across_strips(src_strip_id, dest_strip_id, tab_id);
                    return true;
                }
            }
        }
        // Destination strip not found — we already mutated the source.
        // Returning `false` here would lose the tab, so re-attach it to
        // the source strip's first group as a recovery (rare edge case;
        // dest strip vanished between drag start and harvest tick).
        false
    }

    /// Reorder the window group `group_id` within `strip_id` so it lands
    /// at `new_index` in the strip's `window_groups` vector. Neighbours
    /// shift to fill the vacated slot — the whole group (all its tabs,
    /// its width, its maximized flag) moves as a unit, unlike
    /// [`Self::move_tab_across_strips`] which relocates a single tab.
    ///
    /// `new_index` is clamped to `[0, window_groups.len() - 1]` (the
    /// index space *after* the group is lifted out), so a caller can pass
    /// a raw gap index without worrying about the off-by-one that a
    /// removal introduces. Returns `true` when the order actually changed;
    /// `false` when the strip / group wasn't found or the group was
    /// already at the requested slot (so callers can skip a redundant
    /// doc write / revision bump).
    ///
    /// Mirrors the reorder into the automerge doc via
    /// [`Self::doc_reorder_group_within_strip`] so peer-sync ships the new
    /// order on the next tick — same cache-then-doc shape as
    /// [`Self::move_tab_across_strips`].
    pub fn reorder_group_within_strip(
        &mut self,
        strip_id: StripId,
        group_id: u64,
        new_index: usize,
    ) -> bool {
        // Locate the strip (StripId is workspace-agnostic, so scan all).
        let mut located: Option<(usize, usize, usize)> = None; // (ws_idx, strip_idx, cur_idx)
        'scan: for (ws_idx, ws) in self.workspaces.iter().enumerate() {
            for (strip_idx, strip) in ws.strips.iter().enumerate() {
                if strip.id != strip_id {
                    continue;
                }
                if let Some(cur_idx) = strip.window_groups.iter().position(|g| g.id == group_id) {
                    located = Some((ws_idx, strip_idx, cur_idx));
                }
                break 'scan;
            }
        }
        let Some((ws_idx, strip_idx, cur_idx)) = located else {
            return false;
        };
        let groups = &mut self.workspaces[ws_idx].strips[strip_idx].window_groups;
        // Clamp into the post-removal index space so a raw gap index
        // (which can equal `len`, "append") lands on the last slot.
        let dest = new_index.min(groups.len().saturating_sub(1));
        if dest == cur_idx {
            return false;
        }
        let moved = groups.remove(cur_idx);
        groups.insert(dest, moved.clone());
        // Mirror into the doc from the cache copy we just placed, so the
        // doc's group entry carries the group's full content (tabs +
        // width + maximized) at the new slot. Tolerant of an unseeded
        // doc — see [`Self::locate_group`].
        self.doc_reorder_group_within_strip(strip_id, &moved, dest);
        true
    }

    /// Move the tab at `from` to index `to` inside `group_id`'s tab list.
    ///
    /// This is the persisted half of an intra-group tab drag: the widget tree
    /// reorders its own pills + hosts immediately, and the shell mirrors the
    /// new order here so it survives a restart and reaches peers. Both indices
    /// are in the group's own tab-index space; `to` is clamped to the last
    /// slot. Returns `true` only when the order actually changed, so callers
    /// can skip a redundant revision bump.
    pub fn reorder_tab_within_group(&mut self, group_id: u64, from: usize, to: usize) -> bool {
        // Locate the group in the cache (ids are workspace-agnostic).
        let mut located: Option<(usize, usize, usize)> = None;
        'scan: for (ws_idx, ws) in self.workspaces.iter().enumerate() {
            for (strip_idx, strip) in ws.strips.iter().enumerate() {
                if let Some(g_idx) = strip.window_groups.iter().position(|g| g.id == group_id) {
                    located = Some((ws_idx, strip_idx, g_idx));
                    break 'scan;
                }
            }
        }
        let Some((ws_idx, strip_idx, g_idx)) = located else {
            return false;
        };
        let group = &mut self.workspaces[ws_idx].strips[strip_idx].window_groups[g_idx];
        if from >= group.tabs.len() {
            return false;
        }
        let dest = to.min(group.tabs.len().saturating_sub(1));
        if dest == from {
            return false;
        }
        let moved = group.tabs.remove(from);
        group.tabs.insert(dest, moved);
        // Mirror the reordered list into the doc so peer-sync ships the new
        // order. Cloned first because the doc write borrows `self` mutably.
        let tabs = group.tabs.clone();
        self.doc_rewrite_group_tabs(group_id, &tabs);
        true
    }

    /// Replace `group_id`'s doc-side `tabs` list with `tabs`, in order.
    ///
    /// Used by [`Self::reorder_tab_within_group`]. Rewriting the whole list
    /// (rather than splicing one entry) keeps this in step with
    /// [`Self::doc_reorder_group_within_strip`], which also re-emits a
    /// group's tabs wholesale after a move. No-op when the doc isn't seeded
    /// for this group yet — the cache write already happened, and the next
    /// [`Self::seed_doc_from_cache`] carries the order over.
    fn doc_rewrite_group_tabs(&mut self, group_id: u64, tabs: &[Tab]) {
        use automerge::{ObjType, ScalarValue, transaction::Transactable};
        let Some((group_entry, _, _, _)) = self.locate_group(group_id) else {
            return;
        };
        let Ok(tabs_id) = self.doc.put_object(&group_entry, "tabs", ObjType::List) else {
            return;
        };
        for (i, tab) in tabs.iter().enumerate() {
            if let Ok(tab_entry) = self.doc.insert_object(&tabs_id, i, ObjType::Map) {
                let _ = self.doc.put(&tab_entry, "id", ScalarValue::Uint(tab.id));
                let _ = self.doc.put(&tab_entry, "title", tab.title.clone());
                if let Ok(blob) = serde_json::to_vec(&tab.tile) {
                    let _ = self
                        .doc
                        .put(&tab_entry, "tile_json", ScalarValue::Bytes(blob));
                }
            }
        }
    }

    /// Reconcile the active-group pointer against the set of currently-
    /// closing groups in `strip_id`. If the recorded active group is in
    /// `closing_ids`, advance the pointer to the next non-closing
    /// sibling using the same right-then-left search as
    /// [`Self::resolve_active_group`]. Returns the group id the pointer
    /// landed on, or `None` if every group in the strip is closing
    /// (in which case the entry is removed from the map — "no active
    /// group" is the explicit chosen behaviour for an emptying strip,
    /// not a clamp to a closing-group corpse).
    ///
    /// Idempotent: a no-op when the recorded pointer is not in the
    /// closing set, or when `strip_id` has no recorded pointer at all.
    pub fn nudge_active_group_off_closing(
        &mut self,
        strip_id: StripId,
        closing_ids: &[GroupId],
    ) -> Option<GroupId> {
        let recorded = self.active_group.get(&strip_id).copied()?;
        if !closing_ids.contains(&recorded) {
            return Some(recorded);
        }
        let resolved = self.resolve_active_group(strip_id, |id| closing_ids.contains(&id));
        match resolved {
            Some(next) => {
                self.active_group.insert(strip_id, next);
                Some(next)
            }
            None => {
                // No surviving sibling — the whole strip is fading
                // away. Drop the entry rather than leave a dangling
                // pointer; the shell can re-seed when fresh groups
                // land in the strip.
                self.active_group.remove(&strip_id);
                None
            }
        }
    }
}

/// Shared peer-count helper for the active workspace.
///
/// Counts collaborators on the active workspace excluding the local user
/// (`id == "me"`); falls back to the tree-level roster (also `me`-filtered)
/// when the active workspace has no peers of its own. This matches the
/// precedence already applied by `TopBarChrome::sync_from_tree` and lets
/// `SidebarWheelChrome::sync_from_tree` reuse the same arithmetic for the
/// active card so the two chromes never disagree (audit `B-10`).
///
/// Note: callers that need per-workspace counts for *non-active* workspaces
/// must keep their own filtered count — the fallback only fires for the
/// active workspace.
pub fn active_workspace_peer_count(tree: &WorkspaceTree) -> usize {
    let active = tree.active_workspace();
    let per_ws = active
        .map(|w| w.collaborators.iter().filter(|c| c.id != "me").count())
        .unwrap_or(0);
    if per_ws > 0 {
        return per_ws;
    }
    tree.collaborators.iter().filter(|c| c.id != "me").count()
}

#[cfg(test)]
mod bootstrap_default_tests {
    //! Coverage for [`WorkspaceTree::bootstrap_default`]'s seeded fixture.
    //!
    //! The compositor boots straight into this tree on first run, so what
    //! lives here is what an end user sees when they install QuickDesktop
    //! and log in for the first time. The asserts below pin the
    //! concept-driven contract:
    //!
    //!   - At least three workspaces (Home / Research / Design) so the
    //!     overview overlay paints a meaningful grid rather than a single
    //!     lonely card.
    //!   - At least one workspace seeded with collaborators so the top-bar
    //!     avatar stack and per-tab presence dots have data to render.
    //!   - The first group's [`WindowWidth`] is `Wide` (or wider) so the
    //!     default tile fills more than half the viewport — matching
    //!     `WindowFrame.tsx`'s "fill the container" behaviour.
    //!
    //! Updates here are intentional fixture changes — when the tests are
    //! re-pinned the e2e goldens may also need re-seeding (see
    //! `tests/e2e_shell_flows.rs` for the `e2e-open-close-overview-settled`
    //! golden which is a downstream consumer of this fixture).

    use super::*;

    #[test]
    fn bootstrap_default_seeds_three_demo_workspaces() {
        let tree = WorkspaceTree::bootstrap_default();
        assert!(
            tree.workspaces.len() >= 3,
            "default tree should seed at least 3 workspaces (Home / Research / \
             Design) so the overview has meaningful content; got {}",
            tree.workspaces.len()
        );
        // At least one workspace should have collaborators so the top-bar
        // avatar stack has something to render.
        assert!(
            tree.workspaces.iter().any(|w| !w.collaborators.is_empty()),
            "at least one default workspace should seed >=1 collaborator"
        );
    }

    #[test]
    fn bootstrap_default_first_group_uses_wide_or_larger() {
        let tree = WorkspaceTree::bootstrap_default();
        let first_ws = tree.workspaces.first().expect("at least one workspace");
        let first_strip = first_ws.strips.first().expect("at least one strip");
        let first_group = first_strip
            .window_groups
            .first()
            .expect("at least one group");
        let wide_enough = match first_group.width {
            WindowWidth::Wide => true,
            WindowWidth::Custom(w) => w >= 900,
            _ => false,
        };
        assert!(
            wide_enough,
            "default group should be Wide+ so the white card fills more than \
             half the viewport; got {:?}",
            first_group.width
        );
    }
}

#[cfg(test)]
mod automerge_foundation_tests {
    //! Foundation tests for unit A of the WorkspaceTree → automerge
    //! migration. These exercise the new `current_heads`,
    //! `export_changes_since`, `apply_changes` API surface against a
    //! manually-poked doc — no mutator wires through to the doc yet, so
    //! the tests synthesise changes by writing directly into
    //! `tree.doc`. Subsequent units (B–D) replace the manual writes
    //! with real mutator calls (`set_active`, `append_empty_workspace`,
    //! `harvest_pending_*`, etc.).

    use super::*;
    use automerge::{ROOT, ReadDoc, ScalarValue, transaction::Transactable};

    #[test]
    fn empty_tree_has_no_heads_and_no_changes() {
        let mut t = WorkspaceTree::default();
        assert!(t.current_heads().is_empty());
        assert!(t.export_changes_since(&[]).is_empty());
    }

    #[test]
    fn apply_changes_round_trips_a_manual_doc_mutation() {
        let mut tree_a = WorkspaceTree::default();
        // Directly poke a value into the doc — unit A doesn't have any
        // mutators wired through, so we synthesise a change manually.
        tree_a
            .doc
            .put(&ROOT, "demo", ScalarValue::Uint(42))
            .unwrap();

        let heads_after = tree_a.current_heads();
        assert!(!heads_after.is_empty());
        let changes = tree_a.export_changes_since(&[]);
        assert!(!changes.is_empty());

        let mut tree_b = WorkspaceTree::default();
        tree_b.apply_changes(&changes).unwrap();

        // Tree B now sees the same demo value.
        let val = tree_b.doc.get(&ROOT, "demo").unwrap();
        assert!(val.is_some());
    }

    #[test]
    fn export_changes_since_current_heads_yields_empty() {
        let mut t = WorkspaceTree::default();
        t.doc.put(&ROOT, "k", ScalarValue::Uint(1)).unwrap();
        let heads = t.current_heads();
        let changes = t.export_changes_since(&heads);
        assert!(
            changes.is_empty(),
            "exporting since current heads must yield no changes"
        );
    }

    #[test]
    fn appending_a_workspace_propagates_via_apply_changes() {
        // Unit-B convergence: peer A appends a workspace, ships the
        // resulting change-blobs to a fresh peer B, and B's cache
        // reflects the append after apply_changes.
        //
        // Tree B starts from `default()` rather than `bootstrap_default`
        // so its empty doc cleanly absorbs A's full change-graph without
        // both peers concurrently creating the `workspaces` list at
        // ROOT. Two peers running `bootstrap_default` independently is
        // explicitly out of scope for unit B — that's the
        // concurrent-appends test in unit-C/D where structural ops
        // need diff-aware seeding.
        let mut tree_a = WorkspaceTree::bootstrap_default();
        let initial_count = tree_a.workspaces.len();

        tree_a.append_empty_workspace("Project X");
        assert_eq!(tree_a.workspaces.len(), initial_count + 1);

        let changes = tree_a.export_changes_since(&[]);
        assert!(
            !changes.is_empty(),
            "append_empty_workspace must produce doc-side changes"
        );

        let mut tree_b = WorkspaceTree::default();
        tree_b.apply_changes(&changes).expect("apply changes");

        assert!(
            tree_b.workspaces.iter().any(|w| w.name == "Project X"),
            "tree B should observe the appended workspace after sync; \
             got names: {:?}",
            tree_b
                .workspaces
                .iter()
                .map(|w| &w.name)
                .collect::<Vec<_>>()
        );
    }

    /// A peer that hydrates a workspace out of another peer's doc must
    /// end up in *that peer's* room. Minting a local one instead would
    /// leave it holding a workspace it can see but cannot collaborate
    /// in — the same room-of-one failure the invite path exists to
    /// prevent, arriving by the other route.
    #[test]
    fn a_workspaces_room_propagates_via_apply_changes() {
        let mut tree_a = WorkspaceTree::bootstrap_default();
        let new_id = tree_a.append_empty_workspace("Project X");
        let room = tree_a.room_of(new_id).expect("the append mints a room");
        assert!(!room.is_nil());

        let changes = tree_a.export_changes_since(&[]);
        let mut tree_b = WorkspaceTree::default();
        tree_b.apply_changes(&changes).expect("apply changes");

        let hydrated = tree_b
            .workspaces
            .iter()
            .find(|w| w.name == "Project X")
            .expect("peer B should observe the appended workspace");
        assert_eq!(
            hydrated.room, room,
            "peer B must adopt the room peer A named, not mint one of its \
             own — otherwise the two hold the same workspace and address \
             different clusters"
        );
        assert_eq!(
            hydrated.collab_cluster_id(),
            tree_a
                .workspaces
                .iter()
                .find(|w| w.id == new_id)
                .expect("peer A's workspace")
                .collab_cluster_id(),
            "and the cluster their apps are launched into must match"
        );
    }

    /// `set_room` is the mutation an accepted invite performs. It has to
    /// reach the doc as well as the cache, or the invitee's peers learn
    /// about a room change that never replicates.
    #[test]
    fn set_room_propagates_via_apply_changes() {
        let mut tree_a = WorkspaceTree::bootstrap_default();
        let id = tree_a.workspaces[0].id;
        let inviters_room = RoomId::new();
        assert!(tree_a.set_room(id, inviters_room), "set_room must apply");

        let changes = tree_a.export_changes_since(&[]);
        let mut tree_b = WorkspaceTree::default();
        tree_b.apply_changes(&changes).expect("apply changes");

        assert_eq!(
            tree_b.room_of(id),
            Some(inviters_room),
            "a workspace moved into a new room must carry that room to its \
             peers"
        );
    }

    /// "Join the unminted room" is never a meaningful request, and
    /// honouring it would drop a workspace out of collaboration with no
    /// signal at all.
    #[test]
    fn set_room_refuses_the_nil_room() {
        let mut tree = WorkspaceTree::bootstrap_default();
        let id = tree.workspaces[0].id;
        let before = tree.room_of(id).expect("bootstrap mints a room");
        assert!(
            !tree.set_room(id, RoomId::nil()),
            "setting the nil room must be refused"
        );
        assert_eq!(
            tree.room_of(id),
            Some(before),
            "and must leave the workspace's real room in place"
        );
    }

    #[test]
    fn set_active_propagates_via_apply_changes() {
        // Unit-B convergence: peer A flips the active pointer; a fresh
        // peer B observes the new active id after apply_changes. Tree
        // B uses `default()` for the same reason as the append test:
        // two `bootstrap_default` calls would race on the doc-side
        // workspaces list.
        let mut tree_a = WorkspaceTree::bootstrap_default();
        let target = tree_a
            .workspaces
            .iter()
            .find(|w| w.id != tree_a.active)
            .map(|w| w.id)
            .expect("bootstrap seeds >=2 workspaces");
        tree_a.set_active(target);
        assert_eq!(tree_a.active, target);

        let changes = tree_a.export_changes_since(&[]);
        let mut tree_b = WorkspaceTree::default();
        tree_b.apply_changes(&changes).expect("apply changes");
        assert_eq!(
            tree_b.active, target,
            "tree B's active should match peer A's after sync"
        );
    }

    #[test]
    fn maximize_toggle_propagates_via_apply_changes() {
        // Unit-C convergence: peer A flips a window-group's
        // `maximized` flag through the doc, ships the resulting
        // change-blobs to a fresh peer B, and B's cache reflects the
        // flip after `apply_changes` (driven by
        // `rebuild_cache_from_doc`'s nested-state pass).
        //
        // Tree B starts from `default()` for the same reason as the
        // unit-B tests: two `bootstrap_default` calls would race on
        // the doc-side workspaces list. Default-receiver pattern keeps
        // the convergence assertion clean.
        let mut tree_a = WorkspaceTree::bootstrap_default();
        let target_group = tree_a.workspaces[0].strips[0].window_groups[0].id;
        assert!(
            !tree_a.workspaces[0].strips[0].window_groups[0].maximized,
            "bootstrap_default seeds the first group un-maximized"
        );

        // Mirror to both sides — production calls
        // `harvest_pending_maximize_changes`, which writes cache then
        // doc; in the test we replicate that pattern directly.
        tree_a.workspaces[0].strips[0].window_groups[0].maximized = true;
        tree_a.doc_set_maximized(target_group, true);

        let changes = tree_a.export_changes_since(&[]);
        let mut tree_b = WorkspaceTree::default();
        tree_b.apply_changes(&changes).expect("apply changes");

        let target = tree_b
            .workspaces
            .iter()
            .flat_map(|w| w.strips.iter())
            .flat_map(|s| s.window_groups.iter())
            .find(|g| g.id == target_group);
        assert!(
            target.is_some(),
            "tree B should observe the window group after sync"
        );
        assert!(
            target.unwrap().maximized,
            "maximize flip should propagate; got {:?}",
            target
        );
    }

    #[test]
    fn width_change_propagates_via_apply_changes() {
        // Unit-C convergence: peer A drags a resize handle, the
        // harvester writes `WindowWidth::Custom(p)` to the cache and
        // mirrors `width_pixels: Uint(p)` to the doc; peer B observes
        // the new pixel value after `apply_changes`.
        //
        // The `Custom` branch clamps to 300..=1200 inside `pixels()`,
        // so we pick a value already inside that range to avoid the
        // clamp masking a propagation bug.
        let mut tree_a = WorkspaceTree::bootstrap_default();
        let target_group = tree_a.workspaces[0].strips[0].window_groups[0].id;
        let new_pixels: u32 = 720;

        tree_a.workspaces[0].strips[0].window_groups[0].width = WindowWidth::Custom(new_pixels);
        tree_a.doc_set_width(target_group, new_pixels);

        let changes = tree_a.export_changes_since(&[]);
        let mut tree_b = WorkspaceTree::default();
        tree_b.apply_changes(&changes).expect("apply changes");

        let target = tree_b
            .workspaces
            .iter()
            .flat_map(|w| w.strips.iter())
            .flat_map(|s| s.window_groups.iter())
            .find(|g| g.id == target_group);
        assert!(
            target.is_some(),
            "tree B should observe the window group after sync"
        );
        assert_eq!(
            target.unwrap().width.pixels(),
            new_pixels,
            "width pixels should round-trip via doc"
        );
    }

    #[test]
    fn moving_a_tab_across_strips_propagates_via_apply_changes() {
        // Unit-D convergence: peer A moves a tab from one strip to
        // another. After `move_tab_across_strips` mirrors the move
        // into the doc, peer B sees the tab in the destination strip
        // after `apply_changes`.
        let mut tree_a = WorkspaceTree::bootstrap_default();
        // bootstrap_default has one strip in the Home workspace. Add a
        // second strip with one empty group so the move has somewhere
        // to land. Mirror through the seed so the doc carries the new
        // strip too.
        let dst_strip_id = StripId::new();
        let dst_group_id = next_id();
        tree_a.workspaces[0].strips.push(Strip {
            id: dst_strip_id,
            name: "Dest".into(),
            window_groups: vec![WindowGroup {
                id: dst_group_id,
                tabs: Vec::new(),
                active_tab: 0,
                width: WindowWidth::Normal,
                maximized: false,
            }],
        });
        tree_a.seed_doc_from_cache();

        let src_strip_id = tree_a.workspaces[0].strips[0].id;
        let target_tab = tree_a.workspaces[0].strips[0].window_groups[0].tabs[0].id;

        assert!(tree_a.move_tab_across_strips(src_strip_id, dst_strip_id, target_tab));

        let changes = tree_a.export_changes_since(&[]);
        let mut tree_b = WorkspaceTree::default();
        tree_b.apply_changes(&changes).expect("apply changes");

        let dst_strip_in_b = tree_b
            .workspaces
            .iter()
            .flat_map(|w| w.strips.iter())
            .find(|s| s.id == dst_strip_id);
        assert!(
            dst_strip_in_b.is_some(),
            "destination strip must be observable in tree B"
        );
        let landed_in_dst = dst_strip_in_b
            .unwrap()
            .window_groups
            .iter()
            .flat_map(|g| g.tabs.iter())
            .any(|t| t.id == target_tab);
        assert!(
            landed_in_dst,
            "moved tab must appear in destination strip after sync"
        );
    }
}

#[cfg(test)]
mod collaborator_tests {
    use super::*;

    #[test]
    fn new_initializes_all_concept_fields_to_none() {
        let c = Collaborator::new("alice", "Alice");
        assert_eq!(c.id, "alice");
        assert_eq!(c.name, "Alice");
        assert_eq!(c.active_tab, None);
        assert_eq!(c.cursor_color, None);
        assert_eq!(c.avatar, None);
        assert_eq!(c.role_id, None);
    }

    #[test]
    fn with_active_tab_delegates_to_new_and_sets_tab() {
        let c = Collaborator::with_active_tab("bob", "Bob", 42);
        assert_eq!(c.active_tab, Some(42));
        // Other concept fields stay at their defaults.
        assert_eq!(c.cursor_color, None);
        assert_eq!(c.avatar, None);
        assert_eq!(c.role_id, None);
    }

    #[test]
    fn with_cursor_color_sets_hex_override() {
        let c = Collaborator::new("alice", "Alice").with_cursor_color("#ff00aa");
        assert_eq!(c.cursor_color.as_deref(), Some("#ff00aa"));
    }

    #[test]
    fn with_avatar_sets_reference() {
        let c = Collaborator::new("charlie", "Charlie").with_avatar("CC");
        assert_eq!(c.avatar.as_deref(), Some("CC"));
    }

    #[test]
    fn with_role_sets_role_id() {
        let c = Collaborator::new("bob", "Bob").with_role("editor");
        assert_eq!(c.role_id.as_deref(), Some("editor"));
    }

    #[test]
    fn builder_methods_can_be_chained() {
        let c = Collaborator::with_active_tab("me", "Me", 7)
            .with_cursor_color("#112233")
            .with_avatar("ME")
            .with_role("owner");
        assert_eq!(c.active_tab, Some(7));
        assert_eq!(c.cursor_color.as_deref(), Some("#112233"));
        assert_eq!(c.avatar.as_deref(), Some("ME"));
        assert_eq!(c.role_id.as_deref(), Some("owner"));
    }

    #[test]
    fn legacy_json_without_concept_fields_still_deserializes() {
        // Workspaces persisted before the struct grew concept fields omit
        // `cursor_color`, `avatar`, and `role_id`. The `#[serde(default)]`
        // on each field keeps those payloads loadable, so upgrading doesn't
        // wipe saved state.
        let legacy = r#"{"id":"alice","name":"Alice","active_tab":null}"#;
        let c: Collaborator = serde_json::from_str(legacy).unwrap();
        assert_eq!(c.id, "alice");
        assert_eq!(c.name, "Alice");
        assert_eq!(c.active_tab, None);
        assert_eq!(c.cursor_color, None);
        assert_eq!(c.avatar, None);
        assert_eq!(c.role_id, None);
    }

    #[test]
    fn fully_populated_round_trip() {
        let c = Collaborator::with_active_tab("alice", "Alice", 3)
            .with_cursor_color("#aabbcc")
            .with_avatar("A")
            .with_role("editor");
        let json = serde_json::to_string(&c).unwrap();
        let back: Collaborator = serde_json::from_str(&json).unwrap();
        assert_eq!(back, c);
    }
}

#[cfg(test)]
mod active_group_tests {
    //! Coverage for the active-group pointer + close-fade reconciler.
    //!
    //! The shell layer keeps a per-strip "active group" pointer so
    //! focus / keyboard input / presence broadcasts have a single
    //! authoritative target. When the user closes the last tab of the
    //! active group the widget tree latches a 180 ms fade before
    //! pruning; the reconciler here advances the pointer to a
    //! surviving sibling at fade-start so input doesn't dwell on a
    //! corpse for the duration of the animation.
    //!
    //! Tests build a minimal `WorkspaceTree` via the same field
    //! literals already used elsewhere in the file, then drive the
    //! reconciler with the closing-group set the shell would harvest
    //! from the widget tree (`g.id` filtered by `g.is_closing()`).
    //! Calling the reconciler directly (rather than spinning up a
    //! whole widget tree) keeps the test focused on the data-layer
    //! contract — coupling the data layer to live widgets would
    //! reintroduce the GPU dependency that quick-shell's other unit
    //! tests deliberately avoid.

    use super::*;

    /// Build a single-workspace, single-strip tree carrying `n` empty
    /// window groups with monotonic ids `1..=n`. Groups have no tabs;
    /// the tests only exercise the active-group pointer logic, not
    /// rendering. Returns the strip id alongside the tree so callers
    /// can drive the reconciler without re-walking the structure.
    fn tree_with_n_groups(n: u64) -> (WorkspaceTree, StripId, Vec<GroupId>) {
        let ws_id = WorkspaceId::new();
        let strip_id = StripId::new();
        let group_ids: Vec<GroupId> = (1..=n).collect();
        let strip = Strip {
            id: strip_id,
            name: "Main".into(),
            window_groups: group_ids
                .iter()
                .map(|gid| WindowGroup {
                    id: *gid,
                    tabs: vec![],
                    active_tab: 0,
                    width: WindowWidth::Normal,
                    maximized: false,
                })
                .collect(),
        };
        let ws = Workspace {
            id: ws_id,
            room: RoomId::new(),
            name: "Test".into(),
            strips: vec![strip],
            roles: vec![],
            workspace_files: vec![],
            collaborators: vec![],
            owner: None,
            capabilities: Vec::new(),
            shared_surfaces: HashMap::new(),
        };
        let mut active_strip = HashMap::new();
        active_strip.insert(ws_id, strip_id);
        let tree = WorkspaceTree {
            workspaces: vec![ws],
            active: ws_id,
            active_strip,
            active_group: HashMap::new(),
            collaborators: vec![],
            ..Default::default()
        };
        (tree, strip_id, group_ids)
    }

    #[test]
    fn active_group_advances_when_active_group_starts_closing() {
        // Two groups [A, B] with A active. When A starts closing the
        // pointer must immediately advance to B — the surviving
        // right-hand sibling — so focus / keyboard / presence stop
        // targeting the fading-away group.
        let (mut tree, strip_id, ids) = tree_with_n_groups(2);
        let (a, b) = (ids[0], ids[1]);
        tree.set_active_group(strip_id, a);
        assert_eq!(tree.active_group_for(strip_id), Some(a));

        let landed = tree.nudge_active_group_off_closing(strip_id, &[a]);

        assert_eq!(landed, Some(b));
        assert_eq!(
            tree.active_group_for(strip_id),
            Some(b),
            "active-group pointer must advance to the next surviving sibling at fade-start, not after the 180 ms fade",
        );
    }

    #[test]
    fn active_group_advances_to_previous_when_last_group_starts_closing() {
        // Two groups [A, B] with B active (i.e. B is the rightmost
        // group and is the one closing). With no surviving sibling
        // to the right, the reconciler falls back to A on the left.
        // The chosen behaviour: prefer right-hand survivor first,
        // then walk left. Documented here so the next reader doesn't
        // assume a clamp-to-index-0 semantic.
        let (mut tree, strip_id, ids) = tree_with_n_groups(2);
        let (a, b) = (ids[0], ids[1]);
        tree.set_active_group(strip_id, b);

        let landed = tree.nudge_active_group_off_closing(strip_id, &[b]);

        assert_eq!(landed, Some(a));
        assert_eq!(tree.active_group_for(strip_id), Some(a));
    }

    #[test]
    fn active_group_doesnt_advance_when_non_active_group_closes() {
        // Two groups [A, B] with A active. B closes — the pointer
        // must stay on A. Idempotency: a stray reconcile pass when
        // the active group itself isn't in the closing set is a
        // no-op so the shell can call it freely from a per-tick
        // loop.
        let (mut tree, strip_id, ids) = tree_with_n_groups(2);
        let (a, b) = (ids[0], ids[1]);
        tree.set_active_group(strip_id, a);

        let landed = tree.nudge_active_group_off_closing(strip_id, &[b]);

        assert_eq!(landed, Some(a));
        assert_eq!(tree.active_group_for(strip_id), Some(a));
    }

    #[test]
    fn active_group_clears_when_every_group_is_closing() {
        // Pathological case: every group in the strip is closing
        // (e.g. the user nuked them in quick succession). The
        // pointer can't land anywhere, so the entry is dropped —
        // documented behaviour, not a clamp to a closing-group id.
        // The shell's "no active group" state is the absence of an
        // entry in `active_group`, which `active_group_for` already
        // returns as `None`.
        let (mut tree, strip_id, ids) = tree_with_n_groups(2);
        let (a, b) = (ids[0], ids[1]);
        tree.set_active_group(strip_id, a);

        let landed = tree.nudge_active_group_off_closing(strip_id, &[a, b]);

        assert_eq!(landed, None);
        assert_eq!(tree.active_group_for(strip_id), None);
    }

    #[test]
    fn nudge_is_noop_when_strip_has_no_recorded_pointer() {
        // No call to `set_active_group` first. The reconciler must
        // return `None` and not insert a phantom entry — otherwise a
        // shell that never seeded the pointer would see one
        // suddenly appear after the first close.
        let (mut tree, strip_id, ids) = tree_with_n_groups(2);
        let (a, _b) = (ids[0], ids[1]);
        let landed = tree.nudge_active_group_off_closing(strip_id, &[a]);
        assert_eq!(landed, None);
        assert_eq!(tree.active_group_for(strip_id), None);
    }

    #[test]
    fn resolve_active_group_returns_recorded_when_not_closing() {
        // Pure read path: with the recorded pointer not in the
        // closing set, `resolve_active_group` must return it
        // unchanged. Different from `nudge_*` in that it doesn't
        // mutate the map.
        let (mut tree, strip_id, ids) = tree_with_n_groups(3);
        let (a, b, c) = (ids[0], ids[1], ids[2]);
        tree.set_active_group(strip_id, b);
        let resolved = tree.resolve_active_group(strip_id, |id| id == a || id == c);
        assert_eq!(resolved, Some(b));
        // Map is unchanged.
        assert_eq!(tree.active_group_for(strip_id), Some(b));
    }

    #[test]
    fn resolve_active_group_walks_right_then_left() {
        // Three groups [A, B, C] with B active and B closing. The
        // reconciler must prefer C (right) over A (left) so focus
        // moves "forward" through the strip rather than ping-ponging
        // back to the start.
        let (mut tree, strip_id, ids) = tree_with_n_groups(3);
        let (_a, b, c) = (ids[0], ids[1], ids[2]);
        tree.set_active_group(strip_id, b);

        let landed = tree.nudge_active_group_off_closing(strip_id, &[b]);

        assert_eq!(landed, Some(c));
    }

    #[test]
    fn legacy_json_without_active_group_field_still_deserializes() {
        // Persisted trees written before this field existed must
        // continue to load — `#[serde(default)]` keeps the upgrade
        // path lossless. We write a minimal JSON payload omitting
        // `active_group` and assert the field defaults to an empty
        // map, mirroring the parallel test for `Collaborator`'s
        // concept fields.
        let legacy = r#"{
            "workspaces": [],
            "active": 0,
            "active_strip": {},
            "collaborators": []
        }"#;
        let tree: WorkspaceTree = serde_json::from_str(legacy).unwrap();
        assert!(tree.active_group.is_empty());
    }

    #[test]
    fn window_group_maximized_round_trips_through_json() {
        // Audit task #105: window state must survive restart. A
        // bootstrap_default tree starts un-maximized; flipping
        // `maximized = true` on one group must serialize and
        // deserialize cleanly.
        let mut tree = WorkspaceTree::bootstrap_default();
        let ws = tree.workspaces.first_mut().expect("seed has workspaces");
        let strip = ws.strips.first_mut().expect("seed has strips");
        let group = strip.window_groups.first_mut().expect("seed has groups");
        assert!(!group.maximized);
        group.maximized = true;

        let json = serde_json::to_string(&tree).unwrap();
        let round: WorkspaceTree = serde_json::from_str(&json).unwrap();
        assert!(round.workspaces[0].strips[0].window_groups[0].maximized);
    }

    #[test]
    fn legacy_json_without_maximized_field_defaults_to_false() {
        // `#[serde(default)]` covers persisted trees written before the
        // `maximized` field existed (audit task #105). Stripped-down
        // window-group payload omitting the field must load.
        let legacy = r#"{
            "id": 1,
            "tabs": [],
            "active_tab": 0,
            "width": "Normal"
        }"#;
        let g: WindowGroup = serde_json::from_str(legacy).unwrap();
        assert!(!g.maximized);
    }

    fn group_order(tree: &WorkspaceTree, strip_id: StripId) -> Vec<u64> {
        tree.workspaces[0]
            .strips
            .iter()
            .find(|s| s.id == strip_id)
            .unwrap()
            .window_groups
            .iter()
            .map(|g| g.id)
            .collect()
    }

    /// Dragging a tab to a new slot inside its own window group must persist
    /// into the tree (and be mirrored into the doc), or the reorder is
    /// in-memory only and resets on the next shell restart — the same class of
    /// gap that `pending_maximize_change` / `pending_width_change` exist to
    /// close for maximize and width.
    #[test]
    #[serial_test::serial(next_id_statics)]
    fn reorder_tab_within_group_persists_and_mirrors_into_the_doc() {
        // One group (id 1) carrying three tabs: [10, 11, 12].
        let (mut tree, _strip_id, ids) = tree_with_n_groups(1);
        let group_id = ids[0];
        let mk = |id: u64, name: &str| Tab {
            id,
            title: name.into(),
            tile: TileContent::Stub {
                kind: AppKind::Terminal,
                label: name.into(),
            },
        };
        {
            let g = &mut tree.workspaces[0].strips[0].window_groups[0];
            g.tabs = vec![mk(10, "A"), mk(11, "B"), mk(12, "C")];
            g.active_tab = 10;
        }
        tree.seed_doc_from_cache();

        let tab_order = |t: &WorkspaceTree| -> Vec<u64> {
            t.workspaces[0].strips[0].window_groups[0]
                .tabs
                .iter()
                .map(|tb| tb.id)
                .collect()
        };
        assert_eq!(tab_order(&tree), vec![10, 11, 12]);

        // Drag the first tab to the last slot → [11, 12, 10].
        let changed = tree.reorder_tab_within_group(group_id, 0, 2);
        assert!(changed, "moving a tab to a new slot reports a change");
        assert_eq!(tab_order(&tree), vec![11, 12, 10], "cache order updated");

        // Same slot is a no-op; unknown group is a no-op.
        assert!(!tree.reorder_tab_within_group(group_id, 1, 1));
        assert!(!tree.reorder_tab_within_group(999, 0, 1));
        assert_eq!(tab_order(&tree), vec![11, 12, 10]);

        // The doc must carry the new order too, so peers/persistence agree.
        use automerge::{ROOT, ReadDoc, ScalarValue, Value};
        let (_, workspaces_id) = tree.doc.get(&ROOT, "workspaces").unwrap().unwrap();
        let (_, ws0) = tree.doc.get(&workspaces_id, 0_usize).unwrap().unwrap();
        let (_, strips_id) = tree.doc.get(&ws0, "strips").unwrap().unwrap();
        let (_, strip0) = tree.doc.get(&strips_id, 0_usize).unwrap().unwrap();
        let (_, groups_id) = tree.doc.get(&strip0, "window_groups").unwrap().unwrap();
        let (_, g0) = tree.doc.get(&groups_id, 0_usize).unwrap().unwrap();
        let (_, tabs_id) = tree.doc.get(&g0, "tabs").unwrap().unwrap();
        let mut doc_order = Vec::new();
        for i in 0..tree.doc.length(&tabs_id) {
            let (_, t) = tree.doc.get(&tabs_id, i).unwrap().unwrap();
            if let Some((Value::Scalar(s), _)) = tree.doc.get(&t, "id").unwrap() {
                if let ScalarValue::Uint(id) = s.as_ref() {
                    doc_order.push(*id);
                }
            }
        }
        assert_eq!(doc_order, vec![11, 12, 10], "doc mirrors the new tab order");
    }

    #[test]
    fn reorder_group_moves_group_to_dropped_index() {
        // Strip [1, 2, 3]. Drag group 1 to index 2 → [2, 3, 1].
        let (mut tree, strip_id, ids) = tree_with_n_groups(3);
        assert_eq!(group_order(&tree, strip_id), vec![1, 2, 3]);
        let changed = tree.reorder_group_within_strip(strip_id, ids[0], 2);
        assert!(changed, "moving group 1 to a new slot must report a change");
        assert_eq!(
            group_order(&tree, strip_id),
            vec![2, 3, 1],
            "group 1 must land at the dropped index, neighbours shift left",
        );
    }

    #[test]
    fn reorder_group_forward_and_backward() {
        let (mut tree, strip_id, ids) = tree_with_n_groups(4); // [1,2,3,4]
        // Move last group (4) to the front.
        assert!(tree.reorder_group_within_strip(strip_id, ids[3], 0));
        assert_eq!(group_order(&tree, strip_id), vec![4, 1, 2, 3]);
        // Move it back to the end (index >= len clamps to last slot).
        assert!(tree.reorder_group_within_strip(strip_id, 4, 99));
        assert_eq!(group_order(&tree, strip_id), vec![1, 2, 3, 4]);
    }

    #[test]
    fn reorder_group_to_same_index_is_a_noop() {
        let (mut tree, strip_id, ids) = tree_with_n_groups(3);
        let changed = tree.reorder_group_within_strip(strip_id, ids[1], 1);
        assert!(!changed, "moving a group to its own slot reports no change");
        assert_eq!(group_order(&tree, strip_id), vec![1, 2, 3]);
    }

    #[test]
    fn reorder_group_unknown_group_is_a_noop() {
        let (mut tree, strip_id, _ids) = tree_with_n_groups(2);
        assert!(!tree.reorder_group_within_strip(strip_id, 999, 0));
        assert_eq!(group_order(&tree, strip_id), vec![1, 2]);
    }

    #[test]
    fn reorder_group_mirrors_into_the_seeded_doc() {
        // With the doc seeded from the cache, a reorder must also reshuffle
        // the doc's `window_groups` list so peer-sync ships the new order.
        let (mut tree, strip_id, ids) = tree_with_n_groups(3);
        tree.seed_doc_from_cache();
        tree.reorder_group_within_strip(strip_id, ids[0], 2); // [2,3,1]

        use automerge::{ROOT, ReadDoc, ScalarValue, Value};
        let (_, workspaces_id) = tree.doc.get(&ROOT, "workspaces").unwrap().unwrap();
        let (_, ws0) = tree.doc.get(&workspaces_id, 0_usize).unwrap().unwrap();
        let (_, strips_id) = tree.doc.get(&ws0, "strips").unwrap().unwrap();
        let (_, strip0) = tree.doc.get(&strips_id, 0_usize).unwrap().unwrap();
        let (_, groups_id) = tree.doc.get(&strip0, "window_groups").unwrap().unwrap();
        let mut doc_order = Vec::new();
        for i in 0..tree.doc.length(&groups_id) {
            let (_, g) = tree.doc.get(&groups_id, i).unwrap().unwrap();
            if let Some((Value::Scalar(s), _)) = tree.doc.get(&g, "id").unwrap() {
                if let ScalarValue::Uint(id) = s.as_ref() {
                    doc_order.push(*id);
                }
            }
        }
        assert_eq!(
            doc_order,
            vec![2, 3, 1],
            "the doc's window_groups list must mirror the cache reorder",
        );
    }
}

#[cfg(test)]
mod active_workspace_peer_count_tests {
    //! Coverage for [`active_workspace_peer_count`] — the shared helper
    //! that keeps the top-bar avatar stack and the wheel-disc active-card
    //! collaborator-dot count in lock-step (audit `B-10`).

    use super::*;
    use crate::shell::sidebar_wheel_chrome::SidebarWheelChrome;
    use crate::shell::topbar_chrome::TopBarChrome;
    use std::collections::HashMap;

    /// Build a single-workspace tree with the given collaborators on the
    /// active workspace. The tree-level roster is left empty so this
    /// isolates the per-workspace path from the fallback path.
    fn tree_with_active_collaborators(collabs: Vec<Collaborator>) -> WorkspaceTree {
        let ws_id = WorkspaceId::new();
        let strip_id = StripId::new();
        let ws = Workspace {
            id: ws_id,
            room: RoomId::new(),
            name: "Active".into(),
            strips: vec![Strip {
                id: strip_id,
                name: "Main".into(),
                window_groups: vec![],
            }],
            roles: vec![],
            workspace_files: vec![],
            collaborators: collabs,
            owner: None,
            capabilities: Vec::new(),
            shared_surfaces: HashMap::new(),
        };
        let mut active_strip = HashMap::new();
        active_strip.insert(ws_id, strip_id);
        WorkspaceTree {
            workspaces: vec![ws],
            active: ws_id,
            active_strip,
            active_group: HashMap::new(),
            collaborators: vec![],
            ..Default::default()
        }
    }

    #[test]
    fn topbar_and_wheel_disc_agree_on_active_collab_count() {
        // Two explicit collaborators on the active workspace — one is the
        // local user ("me"). Both chromes must agree that the *peer* count
        // (excluding "me") is 1.
        let tree = tree_with_active_collaborators(vec![
            Collaborator::new("me", "Me"),
            Collaborator::new("alice", "Alice"),
        ]);

        let topbar = TopBarChrome::from_tree(&tree);
        let wheel = SidebarWheelChrome::from_tree(&tree);
        let active_idx = tree
            .workspaces
            .iter()
            .position(|w| w.id == tree.active)
            .expect("active workspace present");

        assert_eq!(
            topbar.collaborator_initials().len(),
            wheel.card_collab_count(active_idx),
            "top-bar avatar stack and wheel-disc active card must agree on \
             the peer count (audit B-10); helper is the single source of truth"
        );
        // And both should equal the helper's value.
        assert_eq!(active_workspace_peer_count(&tree), 1);
        assert_eq!(topbar.collaborator_initials().len(), 1);
    }
}

#[cfg(test)]
mod capability_tests {
    //! Coverage for the Phase 4 Task 4.1 data-model addition:
    //! [`Capability`] / [`GrantScope`] / [`FsMode`] and the
    //! [`Workspace::grants`] field. Pure schema tests — no portal
    //! plumbing is exercised here (that lands in Phase 4 Task 4.3).
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn a_new_workspace_holds_no_capabilities_and_grants_nothing() {
        // Both `Workspace::default()` (used by `..Default::default()`
        // sweeps) and `Workspace::new(...)` must start empty, and an
        // empty room must deny rather than default-allow.
        for ws in [
            Workspace::default(),
            Workspace::new(WorkspaceId::from(1), "Demo"),
        ] {
            assert!(ws.capabilities.is_empty());
            assert!(ws.grants().is_empty());
            assert!(!ws.has_app_grants());
            assert!(
                !ws.may_administer(&"anyone".to_string()),
                "a room with no owner has no administrators"
            );
            assert!(
                !ws.allows(
                    &ws.actor_for_app("org.gnome.gedit"),
                    &ResourceRef::FsPath("/home/u/work".into()),
                    Verb::Read
                ),
                "an app with no capability is denied"
            );
        }
    }

    #[test]
    fn app_grants_round_trip_via_serde_json() {
        // Grants are stored as capabilities and re-derived for the
        // portal view. Round-tripping the workspace must preserve both
        // the stored form and the derived one.
        let mut ws = Workspace::new(WorkspaceId::from(1), "Demo");
        ws.grant_app(
            "org.gnome.gedit",
            &Capability::Filesystem {
                path: PathBuf::from("/home/u/work"),
                mode: FsMode::ReadWrite,
                scope: GRANT_ALWAYS,
            },
        );
        ws.grant_app(
            "org.gnome.gedit",
            &Capability::Network {
                hosts: Some(vec!["github.com".into()]),
                scope: GRANT_SESSION,
            },
        );

        let json = serde_json::to_string(&ws).expect("serialize");
        let round: Workspace = serde_json::from_str(&json).expect("deserialize");

        let view = round.grants();
        assert_eq!(view.len(), 1);
        let caps = view.get("org.gnome.gedit").expect("app present");
        assert_eq!(caps.len(), 2);
        assert!(caps.contains(&Capability::Filesystem {
            path: PathBuf::from("/home/u/work"),
            mode: FsMode::ReadWrite,
            scope: GRANT_ALWAYS,
        }));
        assert!(caps.contains(&Capability::Network {
            hosts: Some(vec!["github.com".into()]),
            scope: GRANT_SESSION,
        }));

        // And the decision function agrees after the round-trip.
        let app = round.actor_for_app("org.gnome.gedit");
        assert!(round.allows(
            &app,
            &ResourceRef::FsPath("/home/u/work/notes.txt".into()),
            Verb::Write
        ));
        assert!(
            !round.allows(
                &app,
                &ResourceRef::FsPath("/home/u/secrets".into()),
                Verb::Read
            ),
            "a path outside the grant stays denied across serialisation"
        );
        assert!(round.allows(
            &app,
            &ResourceRef::NetHost("github.com".into()),
            Verb::Write
        ));
        assert!(
            !round.allows(
                &app,
                &ResourceRef::NetHost("evil.example".into()),
                Verb::Write
            ),
            "and a host outside the allowlist stays denied"
        );
    }

    #[test]
    fn legacy_workspace_without_grants_field_deserialises() {
        // Persisted workspaces written before the `grants` field
        // existed must continue to load — `#[serde(default)]` keeps
        // the upgrade path lossless. Mirrors the parallel test for
        // `WindowGroup::maximized` (audit task #105).
        let legacy = r#"{
            "id": 1,
            "name": "Old",
            "strips": [],
            "roles": [],
            "workspace_files": []
        }"#;
        let ws: Workspace = serde_json::from_str(legacy).expect("legacy compat");
        assert!(ws.capabilities.is_empty());
        assert!(ws.grants().is_empty());
        assert_eq!(ws.owner, None, "a legacy workspace records no owner");
        assert_eq!(ws.id, WorkspaceId::from(1));
        assert_eq!(ws.name, "Old");
    }

    #[test]
    fn all_capability_variants_round_trip_through_serde() {
        // Pin the wire shape of every variant so a downstream change
        // (e.g. renaming a field on `Camera` or flipping `hosts` from
        // `Option<Vec<_>>` to a sentinel) breaks loudly here rather
        // than silently in Phase 4 Task 4.3's portal backend.
        let caps = vec![
            Capability::Filesystem {
                path: PathBuf::from("/tmp/ws"),
                mode: FsMode::Read,
                scope: GRANT_ONCE,
            },
            Capability::Camera {
                scope: GRANT_SESSION,
            },
            Capability::Microphone {
                scope: GRANT_ALWAYS,
            },
            Capability::Screencast { scope: GRANT_ONCE },
            Capability::Network {
                hosts: None,
                scope: GRANT_ALWAYS,
            },
        ];
        let json = serde_json::to_string(&caps).expect("serialize caps");
        let round: Vec<Capability> = serde_json::from_str(&json).expect("deserialize caps");
        assert_eq!(round, caps);
    }

    #[test]
    fn empty_workspace_has_empty_shared_surfaces_map() {
        // Phase 4 v2: parallel guarantee to `grants` — both
        // `Workspace::default()` and `Workspace::new(...)` seed an
        // empty `shared_surfaces` map so reads don't have to special-
        // case `None`.
        let ws = Workspace::default();
        assert!(ws.shared_surfaces.is_empty());
        let ws_new = Workspace::new(WorkspaceId::from(1), "Demo");
        assert!(ws_new.shared_surfaces.is_empty());
    }

    #[test]
    fn share_surface_records_peer_target() {
        // `share_surface` is the entry point the compositor's share-UI
        // (Phase 4 v3, not yet wired) will call once the user picks a
        // peer for a foreign tab. D-7 fix: the map values are
        // `HashSet<u64>` so a surface can be shared with multiple peers.
        let mut ws = Workspace::new(WorkspaceId::from(1), "Demo");
        assert!(!ws.is_surface_shared(7));
        ws.share_surface(7, 42);
        assert!(ws.is_surface_shared(7));
        let peers = ws.shared_surfaces.get(&7).expect("peer set present");
        assert_eq!(peers.len(), 1);
        assert!(peers.contains(&42));
    }

    /// D-7 regression: `share_surface(s, peer_a)` followed by
    /// `share_surface(s, peer_b)` must record BOTH peers. The
    /// previous code's `HashMap<u64, u64>` shape silently overwrote
    /// peer A, dropping it from the share set with no error signal.
    #[test]
    fn shared_surfaces_supports_multiple_peers() {
        let mut ws = Workspace::new(WorkspaceId::from(1), "Demo");
        ws.share_surface(7, 42);
        ws.share_surface(7, 99);

        // BOTH peers must persist; neither has been silently dropped.
        let peers = ws.shared_surfaces.get(&7).expect("peer set present");
        assert_eq!(
            peers.len(),
            2,
            "D-7 regression: two share_surface() calls on the same \
             surface must record BOTH peers, got {peers:?}"
        );
        assert!(peers.contains(&42), "peer 42 (first share) missing");
        assert!(peers.contains(&99), "peer 99 (second share) missing");
        assert!(ws.is_surface_shared(7));

        // Idempotent re-share is also handled.
        ws.share_surface(7, 42);
        assert_eq!(
            ws.shared_surfaces.get(&7).map(|p| p.len()).unwrap_or(0),
            2,
            "re-sharing with an already-recorded peer must be idempotent"
        );

        // Per-peer unshare narrows the set without nuking the entry.
        assert!(ws.unshare_surface_from_peer(7, 42));
        let peers_after = ws.shared_surfaces.get(&7).expect("entry remains");
        assert_eq!(peers_after.len(), 1);
        assert!(peers_after.contains(&99));
        assert!(ws.is_surface_shared(7), "still shared with peer 99");
    }

    #[test]
    fn unshare_surface_returns_prior_peers_and_clears_entry() {
        // Returning the prior peer set matters because the actor needs
        // to fire one `StopShare` event scoped to each peer — looking
        // them up again after the removal would be racy.
        let mut ws = Workspace::new(WorkspaceId::from(1), "Demo");
        ws.share_surface(7, 42);
        ws.share_surface(7, 99);
        let removed = ws.unshare_surface(7).expect("share was present");
        assert_eq!(removed.len(), 2);
        assert!(removed.contains(&42));
        assert!(removed.contains(&99));
        assert!(!ws.is_surface_shared(7));
        // Idempotent: second call returns `None` cleanly.
        assert!(ws.unshare_surface(7).is_none());
    }

    #[test]
    fn shared_surfaces_round_trip_via_serde_json() {
        let mut ws = Workspace::new(WorkspaceId::from(1), "Demo");
        ws.share_surface(11, 100);
        ws.share_surface(22, 200);
        ws.share_surface(22, 201);

        let json = serde_json::to_string(&ws).expect("serialize");
        let round: Workspace = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(round.shared_surfaces.len(), 2);
        let peers_11 = round.shared_surfaces.get(&11).expect("11 present");
        assert_eq!(peers_11.len(), 1);
        assert!(peers_11.contains(&100));
        let peers_22 = round.shared_surfaces.get(&22).expect("22 present");
        assert_eq!(peers_22.len(), 2);
        assert!(peers_22.contains(&200));
        assert!(peers_22.contains(&201));
    }

    #[test]
    fn legacy_workspace_without_shared_surfaces_field_deserialises() {
        // Persisted workspaces written before Phase 4 v2 lack the
        // `shared_surfaces` field; `#[serde(default)]` keeps the
        // upgrade path lossless. Mirrors the parallel guarantee for
        // `grants` (Phase 4 Task 4.1).
        let legacy = r#"{
            "id": 1,
            "name": "Old",
            "strips": [],
            "roles": [],
            "workspace_files": [],
            "grants": {}
        }"#;
        let ws: Workspace = serde_json::from_str(legacy).expect("legacy compat");
        assert!(ws.shared_surfaces.is_empty());
        assert_eq!(ws.id, WorkspaceId::from(1));
    }
}

#[cfg(test)]
mod next_id_seed_tests {
    //! D-9 regression: after loading a persisted tree whose ids climb
    //! past the static counter, [`WorkspaceTree::seed_next_id_from_self`]
    //! must advance the counter so subsequent [`WorkspaceId::new`]
    //! allocations don't collide with persisted entries.
    //!
    //! The counter is process-wide so this test must observe a value
    //! `>= max+1` rather than asserting an exact `max+1` (other tests
    //! in the same binary may have already pushed it further).
    use super::*;

    /// Restore the process-wide peer salt to `0` at scope-exit so a
    /// test that flips the salt doesn't leak the new value into other
    /// tests sharing the same binary (`cargo test` runs them in the
    /// same process). RAII: storing the previous salt in a struct + a
    /// `Drop` impl makes the restore unconditional even on panic.
    struct SaltGuard(u16);
    impl SaltGuard {
        fn new() -> Self {
            Self(NEXT_ID_PEER_SALT.load(AtomicOrdering::Relaxed))
        }
    }
    impl Drop for SaltGuard {
        fn drop(&mut self) {
            NEXT_ID_PEER_SALT.store(self.0, AtomicOrdering::Relaxed);
        }
    }

    #[test]
    #[serial_test::serial(next_id_statics)]
    fn next_id_seeds_above_max_used_id_on_load() {
        let _guard = SaltGuard::new();
        // The default tree path runs with salt=0 (back-compat); the
        // existing assertion still holds in that range.
        NEXT_ID_PEER_SALT.store(0, AtomicOrdering::Relaxed);
        // Build a tree whose ids include a deliberately-high value
        // (50). A fresh process counter starts at 1; without seeding,
        // the next allocation would be `2` and collide with id 1.
        let mut tree = WorkspaceTree::default();
        tree.workspaces
            .push(Workspace::new(WorkspaceId::from(1), "First"));
        tree.workspaces
            .push(Workspace::new(WorkspaceId::from(50), "Fiftieth"));

        tree.seed_next_id_from_self();

        // Mint a fresh id — it must be strictly greater than every
        // persisted id, including the gap-jumping `50`.
        let allocated = next_id();
        assert!(
            allocated > 50,
            "after seeding, next_id() must skip past every persisted \
             id (max was 50); got {} which would collide with the \
             existing tree (D-9 regression)",
            allocated
        );
    }

    /// C-1: a salt of `0` (the default) reproduces the exact id shape
    /// every existing test asserts on. Without this, the C-1 patch
    /// would silently break every fixture that expects `WorkspaceId(1)`
    /// to round-trip through serde.
    #[test]
    #[serial_test::serial(next_id_statics)]
    fn peer_salt_zero_default_keeps_old_ids() {
        let _guard = SaltGuard::new();
        // Reset both the salt and the counter so a parallel test that
        // bumped them doesn't leak.
        NEXT_ID_PEER_SALT.store(0, AtomicOrdering::Relaxed);
        NEXT_ID.store(1, AtomicOrdering::Relaxed);

        let id = next_id();
        assert_eq!(
            id, 1,
            "salt-0 default must mint the bare counter; got 0x{:016x}",
            id
        );
        assert_eq!(peer_salt_of(id), 0);
        assert_eq!(counter_of(id), 1);
    }

    /// C-1: after `set_peer_salt(0xABCD)` the top 16 bits of every
    /// freshly-minted id must equal `0xABCD`. Sanity check on the
    /// bit-packing.
    #[test]
    #[serial_test::serial(next_id_statics)]
    fn peer_salt_high_bits_are_the_salt() {
        let _guard = SaltGuard::new();
        let mut tree = WorkspaceTree::default();
        tree.set_peer_salt(0xABCD);

        // Reset the counter so the assertion below sees a small value
        // it can compare cleanly.
        NEXT_ID.store(1, AtomicOrdering::Relaxed);
        let id = next_id();

        assert_eq!(
            peer_salt_of(id),
            0xABCD,
            "set_peer_salt(0xABCD) must stamp 0xABCD into the high 16 \
             bits of next_id(); got id=0x{:016x} (salt=0x{:04x})",
            id,
            peer_salt_of(id)
        );
        // Counter portion must remain monotonic and small.
        assert_eq!(counter_of(id), 1);
    }

    /// C-1 (the headline scenario from
    /// `docs/superpowers/audits/2026-05-22-nirios-fresh-bugs.md`):
    /// two peers run independent `next_id` counters in parallel. With
    /// distinct peer salts, their ids live in disjoint high-bit ranges
    /// so CRDT sync surfaces both workspaces side-by-side instead of
    /// folding them into a single row.
    ///
    /// To isolate the salt's contribution from an unrelated bug (two
    /// peers concurrently `put_object`'ing the `workspaces` list at
    /// ROOT — separately tracked, not in C-1's scope), we share a
    /// common base doc between the two peers before each appends. Then
    /// the only thing left to differ between the two `append` calls is
    /// the id their respective `next_id` counter mints.
    #[test]
    #[serial_test::serial(next_id_statics)]
    fn two_peers_with_independent_next_id_collide_under_sync() {
        let _guard = SaltGuard::new();

        // Establish a shared base doc: peer A creates the workspaces
        // list, peer B applies that change. After this, both peers
        // share the same `workspaces` list object id at ROOT —
        // subsequent appends become concurrent edits to the *same*
        // list, not concurrent creates of separate lists.
        NEXT_ID_PEER_SALT.store(0x1111, AtomicOrdering::Relaxed);
        NEXT_ID.store(1, AtomicOrdering::Relaxed);
        let mut tree_a = WorkspaceTree::default();
        tree_a.seed_doc_from_cache();
        let base_changes = tree_a.export_changes_since(&[]);
        let mut tree_b = WorkspaceTree::default();
        tree_b.apply_changes(&base_changes).expect("seed B from A");
        // Snapshot the heads after the base sync so the post-fork
        // export only ships the *incremental* appends.
        let base_heads = tree_a.current_heads();

        // Peer B switches to its own salt and appends a workspace.
        NEXT_ID_PEER_SALT.store(0x2222, AtomicOrdering::Relaxed);
        NEXT_ID.store(1, AtomicOrdering::Relaxed);
        let id_b = tree_b.append_empty_workspace("PeerB");

        // Peer A appends concurrently under its own salt. Without the
        // salt, both peers would mint id `1` here and collide.
        NEXT_ID_PEER_SALT.store(0x1111, AtomicOrdering::Relaxed);
        NEXT_ID.store(1, AtomicOrdering::Relaxed);
        let id_a = tree_a.append_empty_workspace("PeerA");

        assert_ne!(
            id_a,
            id_b,
            "two peers with distinct salts must mint distinct ids; \
             got A=0x{:016x} B=0x{:016x} (C-1 regression — pre-patch \
             both peers would have minted id 1)",
            id_a.raw(),
            id_b.raw()
        );
        assert_eq!(peer_salt_of(id_a.raw()), 0x1111);
        assert_eq!(peer_salt_of(id_b.raw()), 0x2222);

        // Sync the incremental appends in both directions and rebuild
        // each peer's cache from the merged doc.
        let changes_a = tree_a.export_changes_since(&base_heads);
        let changes_b = tree_b.export_changes_since(&base_heads);
        tree_b.apply_changes(&changes_a).expect("apply A→B");
        tree_a.apply_changes(&changes_b).expect("apply B→A");

        // Both peers must observe BOTH workspaces — no collision-fold.
        // Pre-patch, the shared id `1` would cause `rebuild_cache_from_doc`
        // to merge the two distinct rows into one (strips collide,
        // names overwrite, grants merge per the audit).
        for (label, tree) in [("A", &tree_a), ("B", &tree_b)] {
            let has_a = tree.workspaces.iter().any(|w| w.id == id_a);
            let has_b = tree.workspaces.iter().any(|w| w.id == id_b);
            assert!(
                has_a && has_b,
                "peer {label} must see both workspaces after sync; \
                 has_a={has_a} has_b={has_b}, workspaces={:?}",
                tree.workspaces
                    .iter()
                    .map(|w| (w.id.raw(), &w.name))
                    .collect::<Vec<_>>()
            );
        }
    }

    /// C-1 follow-up on `seed_next_id_from_self`: the counter advances
    /// based ONLY on ids minted under the local salt. Ids minted by a
    /// remote peer (different salt) live in a disjoint range and must
    /// not drag the local counter forward — that would burn legitimate
    /// local slots for no reason.
    #[test]
    #[serial_test::serial(next_id_statics)]
    fn seed_next_id_from_self_respects_peer_salt() {
        let _guard = SaltGuard::new();
        NEXT_ID_PEER_SALT.store(0x1111, AtomicOrdering::Relaxed);
        NEXT_ID.store(1, AtomicOrdering::Relaxed);

        // A tree containing one id under the local salt (counter=5)
        // and a much larger id under a remote salt (counter=1000).
        let local_id_raw = ((0x1111u64) << PEER_ID_SALT_SHIFT) | 5;
        let remote_id_raw = ((0x2222u64) << PEER_ID_SALT_SHIFT) | 1000;
        let mut tree = WorkspaceTree::default();
        tree.workspaces
            .push(Workspace::new(WorkspaceId::from(local_id_raw), "Local"));
        tree.workspaces
            .push(Workspace::new(WorkspaceId::from(remote_id_raw), "Remote"));

        tree.seed_next_id_from_self();

        // The counter must have advanced to 6 (= local max + 1), NOT
        // to 1001 — remote ids do not consume our counter space.
        let next = NEXT_ID.load(AtomicOrdering::Relaxed);
        assert_eq!(
            next, 6,
            "seed_next_id_from_self must only consider ids under the \
             local salt; got counter={} but the only local-salt id had \
             counter=5 (remote-salt id counter=1000 is in a disjoint \
             range and must be ignored)",
            next
        );
    }
}
