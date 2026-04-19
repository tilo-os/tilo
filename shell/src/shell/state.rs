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

fn next_id() -> u64 {
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
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum TileContent {
    AppPlaceholder { kind: AppKind, label: String },
    Stub { kind: AppKind, label: String },
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Workspace {
    pub id: WorkspaceId,
    pub name: String,
    pub strips: Vec<Strip>,
    pub roles: Vec<Role>,
    pub workspace_files: Vec<WorkspaceFile>,
    #[serde(default)]
    pub collaborators: Vec<Collaborator>,
}

impl Workspace {
    /// Build an empty workspace with the given id and name. The workspace
    /// starts with no strips, roles, files, or collaborators — those are
    /// populated by the compositor or the persistence layer.
    pub fn new(id: WorkspaceId, name: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
            strips: Vec::new(),
            roles: Vec::new(),
            workspace_files: Vec::new(),
            collaborators: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkspaceTree {
    pub workspaces: Vec<Workspace>,
    pub active: WorkspaceId,
    pub active_strip: HashMap<WorkspaceId, StripId>,
    pub collaborators: Vec<Collaborator>,
}

impl WorkspaceTree {
    pub fn bootstrap_default() -> Self {
        let ws_id = WorkspaceId::new();
        let strip_id = StripId::new();
        let group_id = next_id();
        let tab_notes = next_id();
        let tab_files = next_id();
        let tab_terminal = next_id();
        let tab_browser = next_id();
        let ws = Workspace {
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
                    width: WindowWidth::Normal,
                }],
            }],
            roles: vec![],
            workspace_files: vec![],
            collaborators: vec![],
        };
        let mut active_strip = HashMap::new();
        active_strip.insert(ws_id, strip_id);
        Self {
            workspaces: vec![ws],
            active: ws_id,
            active_strip,
            collaborators: vec![],
        }
    }

    pub fn append_empty_workspace(&mut self, name: &str) -> WorkspaceId {
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
        };
        self.active_strip.insert(ws_id, strip_id);
        self.workspaces.push(ws);
        ws_id
    }

    pub fn set_active(&mut self, id: WorkspaceId) {
        if self.workspaces.iter().any(|w| w.id == id) {
            self.active = id;
        }
    }

    pub fn active_workspace(&self) -> Option<&Workspace> {
        self.workspaces.iter().find(|w| w.id == self.active)
    }

    pub fn active_workspace_mut(&mut self) -> Option<&mut Workspace> {
        self.workspaces.iter_mut().find(|w| w.id == self.active)
    }

    pub fn switch_up(&mut self) -> bool {
        let idx = self
            .workspaces
            .iter()
            .position(|w| w.id == self.active)
            .unwrap_or(0);
        if idx == 0 {
            return false;
        }
        self.active = self.workspaces[idx - 1].id;
        true
    }

    pub fn switch_down(&mut self) -> bool {
        let idx = self
            .workspaces
            .iter()
            .position(|w| w.id == self.active)
            .unwrap_or(0);
        if idx + 1 >= self.workspaces.len() {
            return false;
        }
        self.active = self.workspaces[idx + 1].id;
        true
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
