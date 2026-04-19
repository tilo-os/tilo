//! `WorkspaceSettings` — the three-tabbed modal opened from the TopBar's
//! workspace-name button. This module covers the **General** tab (T15), the
//! **Roles** tab (T16), and the **Files** tab (T17).
//!
//! The modal is a retained-mode shell widget: it owns its `ws_id` + active tab
//! + per-tab sub-state (selected role id, DataTable instances), and mutates a
//! borrowed `&mut Workspace` when the caller invokes one of its mutator
//! methods. Rendering sits on Plan 1's `overlay::Modal` + `FocusTrap`
//! scaffolding so tabbing within the dialog is trapped and the scrim
//! dismisses it.

use quick_ui::overlay::{FocusTrap, Modal};
use quick_ui::widget::data_table::{ColumnDef, ColumnWidth, DataTable};

use crate::shell::state::{AppPerm, FilePerm, Workspace, WorkspaceId};

/// Tab selector for the WorkspaceSettings modal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WsTab {
    General,
    Roles,
    Files,
}

impl WsTab {
    pub fn label(&self) -> &'static str {
        match self {
            WsTab::General => "General",
            WsTab::Roles => "Roles",
            WsTab::Files => "Files",
        }
    }
}

/// One row in the Roles-tab's "apps" `DataTable`: an app name and the
/// permission the currently-selected role has against it. Constructed on
/// demand from `ws.roles[selected].permissions.per_app`; the table is purely
/// a view of that map.
#[derive(Clone, Debug)]
pub struct AppPermRow {
    pub app_name: String,
    pub perm: AppPerm,
}

/// One row in the Files-tab's "files" `DataTable`: a workspace file id/name
/// pair plus the permission the currently-selected role has against it.
/// Constructed on demand from `ws.workspace_files` crossed with
/// `ws.roles[selected].permissions.per_file`; the table is purely a view of
/// that map.
#[derive(Clone, Debug)]
pub struct FilePermRow {
    pub file_id: String,
    pub file_name: String,
    pub perm: FilePerm,
}

/// The WorkspaceSettings modal state.
pub struct WorkspaceSettings {
    pub ws_id: WorkspaceId,
    pub active_tab: WsTab,
    modal: Modal,
    focus_trap: FocusTrap,
    /// Role id currently selected in the Roles tab, if any. Driving the
    /// `apps_table` rows; `None` means no role is selected and the table is
    /// empty.
    selected_role: Option<String>,
    /// Per-app permission table populated from the selected role's
    /// `per_app` map. Typed over [`AppPermRow`]; the table primitive renders
    /// it via the standard Plan 2 [`DataTable`] passes.
    apps_table: DataTable<AppPermRow>,
    /// Per-file permission table populated from the selected role's
    /// `per_file` map crossed with `Workspace.workspace_files`. Typed over
    /// [`FilePermRow`]; rendered via the same [`DataTable`] primitive as the
    /// Roles-tab apps table.
    files_table: DataTable<FilePermRow>,
}

impl std::fmt::Debug for WorkspaceSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkspaceSettings")
            .field("ws_id", &self.ws_id)
            .field("active_tab", &self.active_tab)
            .field("selected_role", &self.selected_role)
            .finish()
    }
}

impl WorkspaceSettings {
    /// Construct a fresh modal targeting `ws_id` with the General tab
    /// selected and a default focus trap spanning the three tab buttons.
    pub fn new(ws_id: WorkspaceId) -> Self {
        // Tab button ids are stable constants so the caller (shell root) can
        // render them without maintaining a separate id registry.
        let members = vec![
            tab_focus_id(WsTab::General),
            tab_focus_id(WsTab::Roles),
            tab_focus_id(WsTab::Files),
        ];
        Self {
            ws_id,
            active_tab: WsTab::General,
            modal: Modal::new("workspace-settings"),
            focus_trap: FocusTrap::new(members),
            selected_role: None,
            apps_table: DataTable::new(apps_table_columns()),
            files_table: DataTable::new(files_table_columns()),
        }
    }

    /// The Plan-1 modal scaffold (scrim + id tag) that owns this dialog.
    pub fn modal(&self) -> &Modal {
        &self.modal
    }

    /// The focus trap bounding tabbing to the three header buttons.
    pub fn focus_trap(&self) -> &FocusTrap {
        &self.focus_trap
    }

    /// Switch the active tab.
    pub fn set_tab(&mut self, tab: WsTab) {
        self.active_tab = tab;
    }

    /// Currently-active tab.
    pub fn active_tab(&self) -> WsTab {
        self.active_tab
    }

    /// Rename the given workspace. Caller is expected to pass the same
    /// `Workspace` the modal was opened against (`ws.id == self.ws_id`);
    /// we do not re-check here so the caller can rename any workspace they
    /// still hold a reference to (simplifies the test harness).
    pub fn rename(&mut self, ws: &mut Workspace, new_name: &str) {
        ws.name = new_name.to_string();
    }

    /// Remove the collaborator with the given id from this workspace. No-op
    /// when the id isn't present.
    pub fn remove_collaborator(&mut self, ws: &mut Workspace, collab_id: &str) {
        ws.collaborators.retain(|c| c.id != collab_id);
    }

    /// Return a `quickdesktop://invite/{id}` link for this workspace. The
    /// TopBar's "Invite" button copies this to the clipboard; full P2P
    /// invite handshake lives in Plan 4.
    pub fn invite_link(&self, ws: &Workspace) -> String {
        format!("quickdesktop://invite/{}", u64::from(ws.id))
    }

    /// Select a role in the Roles tab. The value is stored verbatim (no
    /// validation against a workspace — callers supply the id from a click on
    /// a role list row, so a mismatch would only ever mean an empty apps
    /// table).
    pub fn select_role(&mut self, role_id: &str) {
        self.selected_role = Some(role_id.to_string());
    }

    /// Currently-selected role id, if any.
    pub fn selected_role(&self) -> Option<&str> {
        self.selected_role.as_deref()
    }

    /// Immutable handle to the Roles-tab's apps permission table. Exposed so
    /// the shell root can pass it to its render tree alongside the other
    /// chrome widgets.
    pub fn apps_table(&self) -> &DataTable<AppPermRow> {
        &self.apps_table
    }

    /// Mutable handle to the apps table — used by the shell root when it
    /// needs to forward events (column resize, sort toggle) into the table.
    pub fn apps_table_mut(&mut self) -> &mut DataTable<AppPermRow> {
        &mut self.apps_table
    }

    /// Write `perm` into `ws.roles[role_id].permissions.per_app[app_name]`.
    /// No-op when `role_id` does not exist on the workspace; inserts the
    /// key if not already present. After mutating the workspace the
    /// modal's `apps_table` is refreshed so the visible table matches.
    pub fn set_app_perm(
        &mut self,
        ws: &mut Workspace,
        role_id: &str,
        app_name: &str,
        perm: AppPerm,
    ) {
        if let Some(role) = ws.roles.iter_mut().find(|r| r.id == role_id) {
            role.permissions
                .per_app
                .insert(app_name.to_string(), perm);
        }
        self.refresh_apps_table(ws);
    }

    /// Immutable handle to the Files-tab's files permission table. Mirrors
    /// [`Self::apps_table`] but over the `per_file` map; exposed so the
    /// shell root can pass it to its render tree alongside the other chrome
    /// widgets.
    pub fn files_table(&self) -> &DataTable<FilePermRow> {
        &self.files_table
    }

    /// Mutable handle to the files table — used by the shell root when it
    /// needs to forward events (column resize, sort toggle) into the table.
    pub fn files_table_mut(&mut self) -> &mut DataTable<FilePermRow> {
        &mut self.files_table
    }

    /// Write `perm` into `ws.roles[role_id].permissions.per_file[file_id]`.
    /// No-op when `role_id` does not exist on the workspace; inserts the
    /// key if not already present. After mutating the workspace the
    /// modal's `files_table` is refreshed so the visible table matches.
    /// Mirrors [`Self::set_app_perm`] but over the `per_file` map.
    pub fn set_file_perm(
        &mut self,
        ws: &mut Workspace,
        role_id: &str,
        file_id: &str,
        perm: FilePerm,
    ) {
        if let Some(role) = ws.roles.iter_mut().find(|r| r.id == role_id) {
            role.permissions
                .per_file
                .insert(file_id.to_string(), perm);
        }
        self.refresh_files_table(ws);
    }

    /// Rebuild [`apps_table`] rows from the currently-selected role's
    /// `per_app` map. Safe to call when `selected_role` is `None` (result:
    /// empty table).
    fn refresh_apps_table(&mut self, ws: &Workspace) {
        let rows: Vec<AppPermRow> = match self.selected_role.as_deref() {
            Some(id) => match ws.roles.iter().find(|r| r.id == id) {
                Some(role) => {
                    let mut v: Vec<AppPermRow> = role
                        .permissions
                        .per_app
                        .iter()
                        .map(|(k, v)| AppPermRow {
                            app_name: k.clone(),
                            perm: *v,
                        })
                        .collect();
                    // Stable order so snapshots are deterministic — the table
                    // doesn't sort on insertion.
                    v.sort_by(|a, b| a.app_name.cmp(&b.app_name));
                    v
                }
                None => Vec::new(),
            },
            None => Vec::new(),
        };
        self.apps_table.set_rows(rows);
    }

    /// Rebuild [`files_table`] rows from the currently-selected role's
    /// `per_file` map crossed with `Workspace.workspace_files`. Every file
    /// on the workspace produces a row; files without an entry in `per_file`
    /// default to [`FilePerm::None`] so the UI can cycle them up from there.
    /// Safe to call when `selected_role` is `None` (result: empty table) so
    /// the cycle None → Read → Write → None always has a concrete starting
    /// permission to read for each file.
    fn refresh_files_table(&mut self, ws: &Workspace) {
        let rows: Vec<FilePermRow> = match self.selected_role.as_deref() {
            Some(id) => match ws.roles.iter().find(|r| r.id == id) {
                Some(role) => {
                    let mut v: Vec<FilePermRow> = ws
                        .workspace_files
                        .iter()
                        .map(|f| FilePermRow {
                            file_id: f.id.clone(),
                            file_name: f.name.clone(),
                            perm: role
                                .permissions
                                .per_file
                                .get(&f.id)
                                .copied()
                                .unwrap_or(FilePerm::None),
                        })
                        .collect();
                    // Stable order so snapshots are deterministic — the table
                    // doesn't sort on insertion.
                    v.sort_by(|a, b| a.file_name.cmp(&b.file_name));
                    v
                }
                None => Vec::new(),
            },
            None => Vec::new(),
        };
        self.files_table.set_rows(rows);
    }
}

/// Column layout for the Roles-tab apps table: `App` name (flex), fixed
/// 160px `Permission` picker. The cell renderers paint nothing yet — the
/// shell root forwards theme/transform through its own draw pass in later
/// wiring tasks (T27); the key/header/width metadata is already
/// load-bearing for hit-testing.
fn apps_table_columns() -> Vec<ColumnDef<AppPermRow>> {
    vec![
        ColumnDef::new("app", "App", ColumnWidth::Flex(1.0)).sortable(true),
        ColumnDef::new("permission", "Permission", ColumnWidth::Fixed(160.0)),
    ]
}

/// Column layout for the Files-tab files table: `File` name (flex), fixed
/// 160px `Permission` picker (None/Read/Write). Mirrors
/// [`apps_table_columns`] — cell renderers stay empty for now; T27 wires
/// the draw pass through the shell root.
fn files_table_columns() -> Vec<ColumnDef<FilePermRow>> {
    vec![
        ColumnDef::new("file", "File", ColumnWidth::Flex(1.0)).sortable(true),
        ColumnDef::new("permission", "Permission", ColumnWidth::Fixed(160.0)),
    ]
}

/// Stable focus id for the tab button representing `tab`, used by the
/// dialog's `FocusTrap`. Assigning them via a deterministic function
/// lets tests and the shell agree on the ids without a shared registry.
fn tab_focus_id(tab: WsTab) -> u64 {
    // Reserve a small id range so the modal's trap members can't collide
    // with shell-level focusables.
    const BASE: u64 = 0xFFFF_0000;
    match tab {
        WsTab::General => BASE,
        WsTab::Roles => BASE + 1,
        WsTab::Files => BASE + 2,
    }
}
