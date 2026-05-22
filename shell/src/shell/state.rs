use automerge::{AutoCommit, ChangeHash};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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

pub(crate) fn next_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(1);
    N.fetch_add(1, Ordering::Relaxed)
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
    AppPlaceholder { kind: AppKind, label: String },
    Stub { kind: AppKind, label: String },
    /// Foreign Wayland client adopted by the compositor. `surface_id` is
    /// the wayland-server `wl_surface` numeric protocol id; the
    /// compositor's state.rs maps it back to a smithay `WlSurface` for
    /// rendering. The chrome-side render falls through to a Stub
    /// placeholder until the frame loop blits the real buffer (Phase 4
    /// follow-up).
    Foreign { app_id: String, surface_id: u64 },
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
    /// (see `quick-ui/src/widget/window_group.rs`) — stored here so the
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Role {
    pub id: String,
    pub name: String,
    pub permissions: RolePermissions,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RolePermissions {
    pub can_invite: bool,
    pub can_edit_files: bool,
    /// Keyed by `AppKind::as_str()` (serde-friendly string key).
    pub per_app: HashMap<String, AppPerm>,
    /// Keyed by `WorkspaceFile.id`.
    pub per_file: HashMap<String, FilePerm>,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum AppPerm {
    #[default]
    None,
    View,
    Edit,
    Admin,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum FilePerm {
    #[default]
    None,
    Read,
    Write,
}

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
    /// palette that [`quick_ui::presence::user_color`] would pick for
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
    pub fn with_active_tab(
        id: impl Into<String>,
        name: impl Into<String>,
        tab: u64,
    ) -> Self {
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

/// How long a capability grant lives. The variants match the prompt
/// widget's three buttons (Phase 4 Task 4.2): "Just once", "Until I
/// log out", "Always".
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GrantScope {
    /// One-shot — the grant evaporates after the request finishes.
    Once,
    /// Held for this shell session; cleared on shell restart.
    Session,
    /// Persists across restarts. Owner-only at v1; invited peers
    /// can't escalate to `Always` without the owner's confirmation.
    Always,
}

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
    pub name: String,
    pub strips: Vec<Strip>,
    pub roles: Vec<Role>,
    pub workspace_files: Vec<WorkspaceFile>,
    #[serde(default)]
    pub collaborators: Vec<Collaborator>,
    /// Capability grants per app installed in this workspace. Keyed by
    /// [`AppId`] (the wayland app-id of the requesting client). Each app
    /// can hold multiple capabilities (e.g., a code editor with
    /// filesystem + network grants).
    ///
    /// `#[serde(default)]` so persisted state files written before this
    /// field existed still load — a missing `grants` map deserialises
    /// to empty. Phase 4 Task 4.1 lays down the schema; Tasks 4.2/4.3
    /// (UI prompt + portal backend) populate it.
    #[serde(default)]
    pub grants: HashMap<AppId, Vec<Capability>>,
    /// Surfaces from this workspace that are being streamed to a
    /// specific peer — peer-sync Phase 4 v2. Keys are the wayland
    /// `wl_surface` protocol ids the compositor passes through
    /// `TileContent::Foreign`; values are the libp2p [`PeerId`] (hashed
    /// to `u64`, see [`crate::network::protocol::PeerId`]) the share
    /// targets.
    ///
    /// One entry per (surface, peer) pair; a surface streamed to two
    /// peers shows up once per peer (Phase 4 v2 keeps the map keyed by
    /// surface_id; multi-peer per surface is a v3 follow-up — we add a
    /// `Vec<PeerId>` value then).
    ///
    /// `#[serde(default)]` so persisted state files written before this
    /// field existed still load. Mirrors the `grants` rollout pattern.
    #[serde(default)]
    pub shared_surfaces: HashMap<u64, u64>,
}

impl Workspace {
    /// Build an empty workspace with the given id and name. The workspace
    /// starts with no strips, roles, files, collaborators, or grants —
    /// those are populated by the compositor or the persistence layer.
    pub fn new(id: WorkspaceId, name: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
            strips: Vec::new(),
            roles: Vec::new(),
            workspace_files: Vec::new(),
            collaborators: Vec::new(),
            grants: HashMap::new(),
            shared_surfaces: HashMap::new(),
        }
    }

    /// Mark `surface_id` as being streamed to `peer_id` — peer-sync
    /// Phase 4 v2. The compositor's commit handler consults this map
    /// (indirectly, via [`Workspace::is_surface_shared`]) to decide
    /// whether to stage the buffer for the streaming task.
    ///
    /// Overwrites any prior entry for the same surface — useful when a
    /// share is reassigned from one peer to another without an explicit
    /// `unshare`.
    pub fn share_surface(&mut self, surface_id: u64, peer_id: u64) {
        self.shared_surfaces.insert(surface_id, peer_id);
    }

    /// Drop the share entry for `surface_id`. No-op if the surface is
    /// not shared. Returns the peer the surface was shared with so
    /// callers can fire a `StopShare` event without a second lookup.
    pub fn unshare_surface(&mut self, surface_id: u64) -> Option<u64> {
        self.shared_surfaces.remove(&surface_id)
    }

    /// Whether `surface_id` is currently streamed somewhere. Used by
    /// the compositor's commit hook to short-circuit the readback for
    /// unshared surfaces (~all of them in steady state).
    pub fn is_surface_shared(&self, surface_id: u64) -> bool {
        self.shared_surfaces.contains_key(&surface_id)
    }
}

impl Default for Workspace {
    /// Empty workspace with a zero id and empty name. Useful for
    /// `..Default::default()` in struct-update syntax (mirrors what
    /// [`WorkspaceTree`] does for itself) so adding new fields like
    /// `grants` doesn't force a codebase-wide literal sweep.
    fn default() -> Self {
        Self {
            id: WorkspaceId::default(),
            name: String::new(),
            strips: Vec::new(),
            roles: Vec::new(),
            workspace_files: Vec::new(),
            collaborators: Vec::new(),
            grants: HashMap::new(),
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
        }
    }
}


impl WorkspaceTree {
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

    /// Apply a batch of opaque change-blobs received from a peer. After
    /// applying, the cache fields are rebuilt via
    /// [`Self::rebuild_cache_from_doc`] so subsequent reads observe the
    /// merged state.
    pub fn apply_changes(
        &mut self,
        changes: &[Vec<u8>],
    ) -> Result<(), automerge::AutomergeError> {
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
    fn seed_doc_from_cache(&mut self) {
        use automerge::{
            ObjType, ReadDoc, ROOT, ScalarValue, transaction::Transactable,
        };
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
            let _ = self
                .doc
                .put(&entry, "id", ScalarValue::Uint(ws.id.raw()));
            let _ = self.doc.put(&entry, "name", ws.name.clone());
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
                        let _ = self
                            .doc
                            .put(&tab_entry, "id", ScalarValue::Uint(tab.id));
                        let _ =
                            self.doc.put(&tab_entry, "title", tab.title.clone());
                        if let Ok(blob) = serde_json::to_vec(&tab.tile) {
                            let _ = self
                                .doc
                                .put(&tab_entry, "tile_json", ScalarValue::Bytes(blob));
                        }
                    }
                }
            }
        }
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
        use automerge::{ReadDoc, ROOT, ScalarValue, Value};
        // active
        if let Some((Value::Scalar(s), _)) =
            self.doc.get(&ROOT, "active").ok().flatten()
        {
            if let ScalarValue::Uint(raw) = s.as_ref() {
                self.active = WorkspaceId::from(*raw);
            }
        }
        // workspaces — rebuild while preserving any existing nested
        // state by id-match.
        if let Some((_, workspaces_id)) =
            self.doc.get(&ROOT, "workspaces").ok().flatten()
        {
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
                if let Some(existing_idx) =
                    self.workspaces.iter().position(|w| w.id == ws_id)
                {
                    // Take the existing cached workspace by swap-remove
                    // semantics — but keep ordering intact by drain
                    // -> reinsert. Simplest: remove and rename.
                    let mut existing = self.workspaces.remove(existing_idx);
                    existing.name = name;
                    new_workspaces.push(existing);
                } else {
                    new_workspaces.push(Workspace::new(ws_id, name));
                }
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
        if let Some((_, workspaces_id)) =
            self.doc.get(&ROOT, "workspaces").ok().flatten()
        {
            let ws_len = self.doc.length(&workspaces_id);
            for ws_idx in 0..ws_len {
                let ws_entry =
                    match self.doc.get(&workspaces_id, ws_idx).ok().flatten() {
                        Some((_, id)) => id,
                        None => continue,
                    };
                let ws_id_raw =
                    match self.doc.get(&ws_entry, "id").ok().flatten() {
                        Some((Value::Scalar(s), _)) => match s.as_ref() {
                            ScalarValue::Uint(v) => *v,
                            _ => continue,
                        },
                        _ => continue,
                    };
                let ws_id = WorkspaceId::from(ws_id_raw);
                let strips_id =
                    match self.doc.get(&ws_entry, "strips").ok().flatten() {
                        Some((_, id)) => id,
                        None => continue,
                    };
                let strips_len = self.doc.length(&strips_id);
                for strip_idx in 0..strips_len {
                    let strip_entry =
                        match self.doc.get(&strips_id, strip_idx).ok().flatten() {
                            Some((_, id)) => id,
                            None => continue,
                        };
                    let strip_id_raw = match self
                        .doc
                        .get(&strip_entry, "id")
                        .ok()
                        .flatten()
                    {
                        Some((Value::Scalar(s), _)) => match s.as_ref() {
                            ScalarValue::Uint(v) => *v,
                            _ => continue,
                        },
                        _ => continue,
                    };
                    let strip_id = StripId::from(strip_id_raw);
                    let groups_id = match self
                        .doc
                        .get(&strip_entry, "window_groups")
                        .ok()
                        .flatten()
                    {
                        Some((_, id)) => id,
                        None => continue,
                    };
                    let groups_len = self.doc.length(&groups_id);
                    for group_idx in 0..groups_len {
                        let group_entry = match self
                            .doc
                            .get(&groups_id, group_idx)
                            .ok()
                            .flatten()
                        {
                            Some((_, id)) => id,
                            None => continue,
                        };
                        let group_id = match self
                            .doc
                            .get(&group_entry, "id")
                            .ok()
                            .flatten()
                        {
                            Some((Value::Scalar(s), _)) => match s.as_ref() {
                                ScalarValue::Uint(v) => *v,
                                _ => continue,
                            },
                            _ => continue,
                        };
                        let maximized = match self
                            .doc
                            .get(&group_entry, "maximized")
                            .ok()
                            .flatten()
                        {
                            Some((Value::Scalar(s), _)) => match s.as_ref() {
                                ScalarValue::Boolean(v) => *v,
                                _ => false,
                            },
                            _ => false,
                        };
                        let width_pixels = match self
                            .doc
                            .get(&group_entry, "width_pixels")
                            .ok()
                            .flatten()
                        {
                            Some((Value::Scalar(s), _)) => match s.as_ref() {
                                ScalarValue::Uint(v) => *v as u32,
                                _ => 0,
                            },
                            _ => 0,
                        };

                        // Patch into the cached tree, synthesizing
                        // missing structure as needed.
                        let ws = match self
                            .workspaces
                            .iter_mut()
                            .find(|w| w.id == ws_id)
                        {
                            Some(w) => w,
                            None => continue,
                        };
                        let strip = if let Some(s) =
                            ws.strips.iter_mut().find(|s| s.id == strip_id)
                        {
                            s
                        } else {
                            ws.strips.push(Strip::new(strip_id, "Main"));
                            ws.strips
                                .last_mut()
                                .expect("strip just pushed")
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
                                let id = match self
                                    .doc
                                    .get(&tab_entry, "id")
                                    .ok()
                                    .flatten()
                                {
                                    Some((Value::Scalar(s), _)) => match s
                                        .as_ref()
                                    {
                                        ScalarValue::Uint(v) => *v,
                                        _ => continue,
                                    },
                                    _ => continue,
                                };
                                let title = match self
                                    .doc
                                    .get(&tab_entry, "title")
                                    .ok()
                                    .flatten()
                                {
                                    Some((Value::Scalar(s), _)) => match s
                                        .as_ref()
                                    {
                                        ScalarValue::Str(v) => v.to_string(),
                                        _ => String::new(),
                                    },
                                    _ => String::new(),
                                };
                                let tile_blob: Vec<u8> = match self
                                    .doc
                                    .get(&tab_entry, "tile_json")
                                    .ok()
                                    .flatten()
                                {
                                    Some((Value::Scalar(s), _)) => match s
                                        .as_ref()
                                    {
                                        ScalarValue::Bytes(b) => b.clone(),
                                        _ => Vec::new(),
                                    },
                                    _ => Vec::new(),
                                };
                                doc_tabs.push((id, title, tile_blob));
                            }
                        }

                        if let Some(g) = strip
                            .window_groups
                            .iter_mut()
                            .find(|g| g.id == group_id)
                        {
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
                                    if let Some(existing) =
                                        g.tabs.iter().find(|t| t.id == *tid)
                                    {
                                        new_tabs.push(existing.clone());
                                    } else {
                                        let tile = if !blob.is_empty() {
                                            serde_json::from_slice::<TileContent>(blob)
                                                .unwrap_or(TileContent::Stub {
                                                    kind: AppKind::Other(String::new()),
                                                    label: title.clone(),
                                                })
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
                                    g.active_tab = new_tabs
                                        .first()
                                        .map(|t| t.id)
                                        .unwrap_or(0);
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
                                        serde_json::from_slice::<TileContent>(blob)
                                            .unwrap_or(TileContent::Stub {
                                                kind: AppKind::Other(String::new()),
                                                label: title.clone(),
                                            })
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
                            let active_tab = synthesized_tabs
                                .first()
                                .map(|t| t.id)
                                .unwrap_or(0);
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
    fn locate_group(
        &self,
        group_id: u64,
    ) -> Option<(automerge::ObjId, usize, usize, usize)> {
        use automerge::{ReadDoc, ROOT, ScalarValue, Value};
        let (_, workspaces_id) =
            self.doc.get(&ROOT, "workspaces").ok().flatten()?;
        let ws_len = self.doc.length(&workspaces_id);
        for ws_idx in 0..ws_len {
            let (_, ws_entry) =
                self.doc.get(&workspaces_id, ws_idx).ok().flatten()?;
            let strips_id = match self.doc.get(&ws_entry, "strips").ok().flatten()
            {
                Some((_, id)) => id,
                None => continue,
            };
            let strips_len = self.doc.length(&strips_id);
            for strip_idx in 0..strips_len {
                let (_, strip_entry) =
                    self.doc.get(&strips_id, strip_idx).ok().flatten()?;
                let groups_id = match self
                    .doc
                    .get(&strip_entry, "window_groups")
                    .ok()
                    .flatten()
                {
                    Some((_, id)) => id,
                    None => continue,
                };
                let groups_len = self.doc.length(&groups_id);
                for group_idx in 0..groups_len {
                    let (_, group_entry) =
                        self.doc.get(&groups_id, group_idx).ok().flatten()?;
                    if let Some((Value::Scalar(s), _)) =
                        self.doc.get(&group_entry, "id").ok().flatten()
                    {
                        if let ScalarValue::Uint(id) = s.as_ref() {
                            if *id == group_id {
                                return Some((
                                    group_entry,
                                    ws_idx,
                                    strip_idx,
                                    group_idx,
                                ));
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
            let _ = self.doc.put(
                &entry,
                "maximized",
                ScalarValue::Boolean(new_value),
            );
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
            let _ = self.doc.put(
                &entry,
                "width_pixels",
                ScalarValue::Uint(new_pixels as u64),
            );
        }
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
        use automerge::{ReadDoc, ROOT, ScalarValue, Value};
        let (_, workspaces_id) =
            self.doc.get(&ROOT, "workspaces").ok().flatten()?;
        let ws_len = self.doc.length(&workspaces_id);
        for ws_idx in 0..ws_len {
            let (_, ws_entry) =
                self.doc.get(&workspaces_id, ws_idx).ok().flatten()?;
            let strips_id = match self.doc.get(&ws_entry, "strips").ok().flatten()
            {
                Some((_, id)) => id,
                None => continue,
            };
            let strips_len = self.doc.length(&strips_id);
            for s_idx in 0..strips_len {
                let (_, strip_entry) =
                    self.doc.get(&strips_id, s_idx).ok().flatten()?;
                let strip_id_matches =
                    match self.doc.get(&strip_entry, "id").ok().flatten() {
                        Some((Value::Scalar(s), _)) => match s.as_ref() {
                            ScalarValue::Uint(id) => *id == strip_id.raw(),
                            _ => false,
                        },
                        _ => false,
                    };
                if !strip_id_matches {
                    continue;
                }
                let groups_id = match self
                    .doc
                    .get(&strip_entry, "window_groups")
                    .ok()
                    .flatten()
                {
                    Some((_, id)) => id,
                    None => continue,
                };
                let groups_len = self.doc.length(&groups_id);
                for g_idx in 0..groups_len {
                    let (_, group_entry) =
                        self.doc.get(&groups_id, g_idx).ok().flatten()?;
                    let tabs_id =
                        match self.doc.get(&group_entry, "tabs").ok().flatten() {
                            Some((_, id)) => id,
                            None => continue,
                        };
                    let tabs_len = self.doc.length(&tabs_id);
                    for t_idx in 0..tabs_len {
                        let (_, tab_entry) =
                            self.doc.get(&tabs_id, t_idx).ok().flatten()?;
                        if let Some((Value::Scalar(s), _)) =
                            self.doc.get(&tab_entry, "id").ok().flatten()
                        {
                            if let ScalarValue::Uint(id) = s.as_ref() {
                                if *id == tab_id {
                                    return Some((
                                        group_entry,
                                        tabs_id,
                                        tab_entry,
                                        t_idx,
                                    ));
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
        use automerge::{ObjType, ReadDoc, ROOT, ScalarValue, Value, transaction::Transactable};
        // Capture source.
        let Some((_, src_tabs, src_tab_entry, src_idx)) =
            self.locate_tab(src_strip, tab_id)
        else {
            return false;
        };
        let title = match self
            .doc
            .get(&src_tab_entry, "title")
            .ok()
            .flatten()
        {
            Some((Value::Scalar(s), _)) => match s.as_ref() {
                ScalarValue::Str(v) => v.to_string(),
                _ => String::new(),
            },
            _ => String::new(),
        };
        let tile_blob: Vec<u8> = match self
            .doc
            .get(&src_tab_entry, "tile_json")
            .ok()
            .flatten()
        {
            Some((Value::Scalar(s), _)) => match s.as_ref() {
                ScalarValue::Bytes(b) => b.clone(),
                _ => Vec::new(),
            },
            _ => Vec::new(),
        };

        // Delete from source.
        let _ = self.doc.delete(&src_tabs, src_idx);

        // Locate destination strip and its first window-group's tabs list.
        let Some((_, workspaces_id)) =
            self.doc.get(&ROOT, "workspaces").ok().flatten()
        else {
            return false;
        };
        let ws_len = self.doc.length(&workspaces_id);
        for ws_idx in 0..ws_len {
            let Some((_, ws_entry)) =
                self.doc.get(&workspaces_id, ws_idx).ok().flatten()
            else {
                continue;
            };
            let Some((_, strips_id)) =
                self.doc.get(&ws_entry, "strips").ok().flatten()
            else {
                continue;
            };
            let strips_len = self.doc.length(&strips_id);
            for s_idx in 0..strips_len {
                let Some((_, strip_entry)) =
                    self.doc.get(&strips_id, s_idx).ok().flatten()
                else {
                    continue;
                };
                let dst_matches =
                    matches!(self.doc.get(&strip_entry, "id").ok().flatten(),
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
                let Some((_, first_group)) =
                    self.doc.get(&groups_id, 0_usize).ok().flatten()
                else {
                    return false;
                };
                let tabs_id =
                    match self.doc.get(&first_group, "tabs").ok().flatten() {
                        Some((_, id)) => id,
                        None => match self
                            .doc
                            .put_object(&first_group, "tabs", ObjType::List)
                        {
                            Ok(id) => id,
                            Err(_) => return false,
                        },
                    };
                let new_idx = self.doc.length(&tabs_id);
                let Ok(tab_entry) =
                    self.doc.insert_object(&tabs_id, new_idx, ObjType::Map)
                else {
                    return false;
                };
                let _ =
                    self.doc.put(&tab_entry, "id", ScalarValue::Uint(tab_id));
                let _ = self.doc.put(&tab_entry, "title", title);
                if !tile_blob.is_empty() {
                    let _ = self.doc.put(
                        &tab_entry,
                        "tile_json",
                        ScalarValue::Bytes(tile_blob),
                    );
                }
                return true;
            }
        }
        false
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
        use automerge::{ObjType, ReadDoc, ROOT, ScalarValue, Value, transaction::Transactable};
        let Some((_, workspaces_id)) =
            self.doc.get(&ROOT, "workspaces").ok().flatten()
        else {
            return false;
        };
        let ws_len = self.doc.length(&workspaces_id);
        let active_raw = self.active.raw();
        for ws_idx in 0..ws_len {
            let Some((_, ws_entry)) =
                self.doc.get(&workspaces_id, ws_idx).ok().flatten()
            else {
                continue;
            };
            let active_matches =
                matches!(self.doc.get(&ws_entry, "id").ok().flatten(),
                    Some((Value::Scalar(s), _)) if matches!(s.as_ref(), ScalarValue::Uint(id) if *id == active_raw));
            if !active_matches {
                continue;
            }
            let Some((_, strips_id)) =
                self.doc.get(&ws_entry, "strips").ok().flatten()
            else {
                return false;
            };
            if self.doc.length(&strips_id) == 0 {
                return false;
            }
            let Some((_, strip_entry)) =
                self.doc.get(&strips_id, 0_usize).ok().flatten()
            else {
                return false;
            };
            let Some((_, groups_id)) =
                self.doc.get(&strip_entry, "window_groups").ok().flatten()
            else {
                return false;
            };
            if self.doc.length(&groups_id) == 0 {
                return false;
            }
            let Some((_, group_entry)) =
                self.doc.get(&groups_id, 0_usize).ok().flatten()
            else {
                return false;
            };
            let tabs_id =
                match self.doc.get(&group_entry, "tabs").ok().flatten() {
                    Some((_, id)) => id,
                    None => match self
                        .doc
                        .put_object(&group_entry, "tabs", ObjType::List)
                    {
                        Ok(id) => id,
                        Err(_) => return false,
                    },
                };
            let new_idx = self.doc.length(&tabs_id);
            let Ok(tab_entry) =
                self.doc.insert_object(&tabs_id, new_idx, ObjType::Map)
            else {
                return false;
            };
            let _ = self.doc.put(&tab_entry, "id", ScalarValue::Uint(tab_id));
            let _ = self.doc.put(&tab_entry, "title", title);
            if !tile_blob.is_empty() {
                let _ = self.doc.put(
                    &tab_entry,
                    "tile_json",
                    ScalarValue::Bytes(tile_blob),
                );
            }
            return true;
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
                            tile: TileContent::Stub {
                                kind: AppKind::FileExplorer,
                                label: "Files".into(),
                            },
                        },
                        Tab {
                            id: tab_terminal,
                            title: "Terminal".into(),
                            tile: TileContent::Stub {
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
            grants: HashMap::new(),
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
        use automerge::{ObjType, ReadDoc, ROOT, ScalarValue, transaction::Transactable};
        let ws_id = WorkspaceId::new();
        let strip_id = StripId::new();
        let ws = Workspace {
            id: ws_id,
            name: name.into(),
            strips: vec![Strip {
                id: strip_id,
                name: "Main".into(),
                window_groups: vec![],
            }],
            roles: vec![],
            workspace_files: vec![],
            collaborators: vec![],
            grants: HashMap::new(),
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
        let _ = self
            .doc
            .put(&entry, "id", ScalarValue::Uint(ws_id.raw()));
        let _ = self.doc.put(&entry, "name", name.to_string());
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
                    self.doc_move_tab_across_strips(
                        src_strip_id,
                        dest_strip_id,
                        tab_id,
                    );
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
        let resolved =
            self.resolve_active_group(strip_id, |id| closing_ids.contains(&id));
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
    use automerge::{ReadDoc, ROOT, ScalarValue, transaction::Transactable};

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
            tree_b.workspaces.iter().map(|w| &w.name).collect::<Vec<_>>()
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
        let target_group =
            tree_a.workspaces[0].strips[0].window_groups[0].id;
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
        let target_group =
            tree_a.workspaces[0].strips[0].window_groups[0].id;
        let new_pixels: u32 = 720;

        tree_a.workspaces[0].strips[0].window_groups[0].width =
            WindowWidth::Custom(new_pixels);
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
            name: "Test".into(),
            strips: vec![strip],
            roles: vec![],
            workspace_files: vec![],
            collaborators: vec![],
            grants: HashMap::new(),
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
            name: "Active".into(),
            strips: vec![Strip {
                id: strip_id,
                name: "Main".into(),
                window_groups: vec![],
            }],
            roles: vec![],
            workspace_files: vec![],
            collaborators: collabs,
            grants: HashMap::new(),
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
    fn empty_workspace_has_empty_grants_map() {
        // Both `Workspace::default()` (used by `..Default::default()`
        // sweeps) and `Workspace::new(...)` must seed an empty grants
        // map so reads don't have to special-case `None`.
        let ws = Workspace::default();
        assert!(ws.grants.is_empty());
        let ws_new = Workspace::new(WorkspaceId::from(1), "Demo");
        assert!(ws_new.grants.is_empty());
    }

    #[test]
    fn grants_round_trip_via_serde_json() {
        // Build a workspace with two grants for one app and round-trip
        // through serde_json. Re-serialising the deserialised value
        // must yield the same JSON shape — pinning the on-disk schema
        // for portal-side consumers.
        let mut ws = Workspace::new(WorkspaceId::from(1), "Demo");
        ws.grants.insert(
            "org.gnome.gedit".into(),
            vec![
                Capability::Filesystem {
                    path: PathBuf::from("/home/u/work"),
                    mode: FsMode::ReadWrite,
                    scope: GrantScope::Always,
                },
                Capability::Network {
                    hosts: Some(vec!["github.com".into()]),
                    scope: GrantScope::Session,
                },
            ],
        );

        let json = serde_json::to_string(&ws).expect("serialize");
        let round: Workspace = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(round.grants.len(), 1);
        let caps = round.grants.get("org.gnome.gedit").expect("app present");
        assert_eq!(caps.len(), 2);
        assert_eq!(
            caps[0],
            Capability::Filesystem {
                path: PathBuf::from("/home/u/work"),
                mode: FsMode::ReadWrite,
                scope: GrantScope::Always,
            }
        );
        assert_eq!(
            caps[1],
            Capability::Network {
                hosts: Some(vec!["github.com".into()]),
                scope: GrantScope::Session,
            }
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
        assert!(ws.grants.is_empty());
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
                scope: GrantScope::Once,
            },
            Capability::Camera {
                scope: GrantScope::Session,
            },
            Capability::Microphone {
                scope: GrantScope::Always,
            },
            Capability::Screencast {
                scope: GrantScope::Once,
            },
            Capability::Network {
                hosts: None,
                scope: GrantScope::Always,
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
        // peer for a foreign tab. The map is keyed by `surface_id` —
        // there's one share per surface at v2; multi-peer per surface
        // is a v3 follow-up where the value flips to `Vec<PeerId>`.
        let mut ws = Workspace::new(WorkspaceId::from(1), "Demo");
        assert!(!ws.is_surface_shared(7));
        ws.share_surface(7, 42);
        assert!(ws.is_surface_shared(7));
        assert_eq!(ws.shared_surfaces.get(&7), Some(&42));
    }

    #[test]
    fn share_surface_overwrites_existing_peer() {
        // A share reassignment without an explicit unshare should
        // replace the peer rather than silently keep the old one —
        // matches the `HashMap::insert` semantics so the compositor's
        // UI doesn't have to remember to `unshare` first.
        let mut ws = Workspace::new(WorkspaceId::from(1), "Demo");
        ws.share_surface(7, 42);
        ws.share_surface(7, 99);
        assert_eq!(ws.shared_surfaces.get(&7), Some(&99));
        assert_eq!(ws.shared_surfaces.len(), 1);
    }

    #[test]
    fn unshare_surface_returns_prior_peer_and_clears_entry() {
        // Returning the prior peer matters because the actor needs to
        // fire a `StopShare` event scoped to *that* peer — looking it
        // up again after the removal would be racy.
        let mut ws = Workspace::new(WorkspaceId::from(1), "Demo");
        ws.share_surface(7, 42);
        assert_eq!(ws.unshare_surface(7), Some(42));
        assert!(!ws.is_surface_shared(7));
        // Idempotent: second call returns `None` cleanly.
        assert_eq!(ws.unshare_surface(7), None);
    }

    #[test]
    fn shared_surfaces_round_trip_via_serde_json() {
        let mut ws = Workspace::new(WorkspaceId::from(1), "Demo");
        ws.share_surface(11, 100);
        ws.share_surface(22, 200);

        let json = serde_json::to_string(&ws).expect("serialize");
        let round: Workspace = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(round.shared_surfaces.len(), 2);
        assert_eq!(round.shared_surfaces.get(&11), Some(&100));
        assert_eq!(round.shared_surfaces.get(&22), Some(&200));
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
