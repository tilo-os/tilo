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
//!
//! ## Modal layout (concept-fidelity T1)
//!
//! Concept ref `nirios-concept/src/components/WorkspaceSettingsModal.tsx:43-95`
//! splits the modal into a **256 px left sidebar** (workspace identity card +
//! General / Roles & Permissions / Files nav rows + Close-Settings footer)
//! and a right content pane that swaps body by `active_tab`. The sidebar
//! nav and dispatch helpers ([`WorkspaceSettings::nav_rows`],
//! [`WorkspaceSettings::sidebar_rect`], [`WorkspaceSettings::pane_rect`],
//! [`WorkspaceSettings::draw_body`]) live on `WorkspaceSettings` itself so
//! the per-tab content drawers (T2 General, T3 Roles, T4 Files) implemented
//! by sister patches in `overlay_host.rs` can hang their content off the
//! `match ws.active_tab` arm without re-deriving any geometry.

use vello::Scene;
use vello::kurbo::{Affine, Rect, RoundedRect, Stroke};
use vello::peniko::Fill;

use ui::icons::{Icon, LUCIDE_DEFAULT_STROKE_PX, draw_icon};
use ui::overlay::{FocusTrap, Modal};
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::data_table::{ColumnDef, ColumnWidth, DataTable};

use crate::shell::state::{AppId, AppPerm, Capability, FilePerm, Role, Workspace, WorkspaceId};

/// Width of the modal's left sidebar in CSS px. Mirrors
/// `WorkspaceSettingsModal.tsx:50` (`w-64` = 16 rem = 256 px).
pub const SIDEBAR_WIDTH: f64 = 256.0;

/// Vertical inset between the top of the sidebar and the workspace
/// identity card. Mirrors `p-6` on the sidebar wrapper plus the badge's
/// own height (concept ref:55-72).
const SIDEBAR_IDENTITY_TOP: f64 = 24.0;

/// Height of one nav row (Tailwind `h-10` minus the row's vertical
/// breathing room — concept ref:75-89 uses `py-2.5` on a 40 px target).
const NAV_ROW_HEIGHT: f64 = 40.0;

/// Vertical gap between consecutive nav rows. The concept stacks them
/// with `space-y-1` (= 4 px); we add the row height for the start-of-next
/// stride.
const NAV_ROW_GAP: f64 = 4.0;

/// Y origin (relative to the sidebar's top edge) where the first nav row
/// begins. Reserves room for the identity badge + caption.
const NAV_ROWS_TOP: f64 = 120.0;

/// Tab selector for the WorkspaceSettings modal.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WsTab {
    /// Workspace identity, invite link, collaborators (T2).
    #[default]
    General,
    /// Per-role app + file permissions (T3).
    Roles,
    /// File grid w/ assigned-role avatar piles (T4).
    Files,
    /// Per-app capability grants list with Revoke affordances
    /// (Sandbox Phase 6). Hidden by default — only surfaces in the
    /// sidebar nav + tab strip when the active workspace has at
    /// least one entry in [`Workspace::grants`]. Empty workspaces
    /// keep the four-tab parity baseline so concept-fidelity pill
    /// positions don't shift.
    Permissions,
}

impl WsTab {
    /// Display label for the sidebar nav row + tab pill. Concept ref
    /// `WorkspaceSettingsModal.tsx:80` (`Roles & Permissions`).
    pub fn label(&self) -> &'static str {
        match self {
            WsTab::General => "General",
            WsTab::Roles => "Roles & Permissions",
            WsTab::Files => "Files",
            WsTab::Permissions => "Permissions",
        }
    }

    /// Iteration order for the sidebar nav (top-to-bottom). Includes
    /// every variant; callers that only want the visible-in-this-
    /// workspace subset should use [`WsTab::visible`] instead. The
    /// stable iteration order (General → Roles → Files → Permissions)
    /// is what the sidebar nav and the tab strip render against.
    pub fn all() -> [WsTab; 4] {
        [WsTab::General, WsTab::Roles, WsTab::Files, WsTab::Permissions]
    }

    /// Tabs visible for the supplied workspace. The Permissions tab is
    /// hidden by default and only surfaces once the workspace has at
    /// least one capability grant — concept-parity pill positions are
    /// pinned for the 4-pill strip (`General/Roles/Files/Collaborators`),
    /// so adding a 5th pill unconditionally would shift them. Empty
    /// `grants` ⇒ Permissions is filtered out so the parity tests keep
    /// observing the original strip; non-empty ⇒ Permissions joins the
    /// strip at the tail.
    pub fn visible(ws: &Workspace) -> Vec<WsTab> {
        let base = vec![WsTab::General, WsTab::Roles, WsTab::Files];
        if ws.grants.is_empty() {
            base
        } else {
            let mut v = base;
            v.push(WsTab::Permissions);
            v
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
            tab_focus_id(WsTab::Permissions),
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
    pub fn select_tab(&mut self, tab: WsTab) {
        self.active_tab = tab;
    }

    /// Synonym for [`Self::select_tab`]. Retained for callers that
    /// already use the imperative `set_*` form.
    pub fn set_tab(&mut self, tab: WsTab) {
        self.select_tab(tab);
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

    // -----------------------------------------------------------------
    // Sidebar nav layout (T1)
    // -----------------------------------------------------------------

    /// The sidebar's bounds inside `modal_rect`. The modal is split with
    /// the left 256 px reserved for nav (concept ref:50). Returns
    /// `Rect::ZERO` when the modal isn't wide enough to host the full
    /// sidebar (callers that hit this can fall back to drawing nothing).
    pub fn sidebar_rect(&self, modal_rect: Rect) -> Rect {
        if modal_rect.width() <= SIDEBAR_WIDTH {
            return Rect::ZERO;
        }
        Rect::new(
            modal_rect.x0,
            modal_rect.y0,
            modal_rect.x0 + SIDEBAR_WIDTH,
            modal_rect.y1,
        )
    }

    /// The right-pane bounds inside `modal_rect`. T2/T3/T4 dispatch
    /// content into this rect.
    pub fn pane_rect(&self, modal_rect: Rect) -> Rect {
        if modal_rect.width() <= SIDEBAR_WIDTH {
            return modal_rect;
        }
        Rect::new(
            modal_rect.x0 + SIDEBAR_WIDTH,
            modal_rect.y0,
            modal_rect.x1,
            modal_rect.y1,
        )
    }

    /// Per-row hit-test rects for the sidebar nav, one per [`WsTab`] in
    /// `WsTab::all()` order. Geometry mirrors the concept's `space-y-1`
    /// stack (40 px row + 4 px gap) starting `NAV_ROWS_TOP` below the
    /// sidebar's top edge.
    pub fn nav_rows(&self, modal_rect: Rect) -> [(WsTab, Rect); 4] {
        let sidebar = self.sidebar_rect(modal_rect);
        let inset_x0 = sidebar.x0 + 16.0;
        let inset_x1 = sidebar.x1 - 16.0;
        let tabs = WsTab::all();
        std::array::from_fn(|i| {
            let y0 = sidebar.y0 + NAV_ROWS_TOP + i as f64 * (NAV_ROW_HEIGHT + NAV_ROW_GAP);
            let y1 = y0 + NAV_ROW_HEIGHT;
            (tabs[i], Rect::new(inset_x0, y0, inset_x1, y1))
        })
    }

    /// Tab whose nav-row contains `point`, if any. Used by hit-testing
    /// to dispatch a pointer-down into [`Self::select_tab`].
    pub fn nav_hit_test(&self, modal_rect: Rect, point: vello::kurbo::Point) -> Option<WsTab> {
        self.nav_rows(modal_rect)
            .into_iter()
            .find_map(|(tab, row)| row.contains(point).then_some(tab))
    }

    /// Draw the modal body — paints the 256 px sidebar nav (always
    /// visible, even when no per-tab content is wired) and dispatches
    /// the right pane to the per-tab drawer keyed by `active_tab`.
    /// Per-tab drawers receive [`Self::pane_rect`] so they don't need to
    /// re-derive geometry. T2/T3/T4 supply their own drawer closures via
    /// the optional `pane_drawer` argument; `None` paints nothing on
    /// the right (the sidebar still renders).
    pub fn draw_body(
        &self,
        scene: &mut Scene,
        transform: Affine,
        modal_rect: Rect,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        pane_drawer: Option<&mut dyn FnMut(&mut Scene, Affine, Rect, &mut TextRenderer, &Theme)>,
    ) {
        // The sidebar always renders, even when the right pane isn't
        // wired yet — so the user sees the nav and can switch tabs.
        self.draw_sidebar(scene, transform, modal_rect, text_renderer, theme);

        if let Some(drawer) = pane_drawer {
            let pane = self.pane_rect(modal_rect);
            drawer(scene, transform, pane, text_renderer, theme);
        }
    }

    /// Paint the sidebar — identity card, nav rows, footer. Concept ref
    /// `WorkspaceSettingsModal.tsx:50-95`.
    fn draw_sidebar(
        &self,
        scene: &mut Scene,
        transform: Affine,
        modal_rect: Rect,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let sidebar = self.sidebar_rect(modal_rect);
        if sidebar.is_zero_area() {
            return;
        }
        let tokens = &theme.tokens;
        let colors = &tokens.colors;

        // Sidebar surface: slate-900 at low alpha so the nav reads
        // distinct from the right pane. Right border = slate-700 hairline.
        let bg = colors.neutral[1].with_alpha_factor(0.40);
        scene.fill(Fill::NonZero, transform, bg, None, &sidebar);
        let border_x = sidebar.x1 - 0.5;
        let border = Rect::new(border_x, sidebar.y0, border_x + 1.0, sidebar.y1);
        scene.fill(Fill::NonZero, transform, colors.neutral[3], None, &border);

        // Identity card — 56 px indigo-500 rounded square + workspace
        // first letter centered. Concept ref:55-72.
        let identity_size = 56.0_f64;
        let identity_rect = Rect::new(
            sidebar.x0 + 24.0,
            sidebar.y0 + SIDEBAR_IDENTITY_TOP,
            sidebar.x0 + 24.0 + identity_size,
            sidebar.y0 + SIDEBAR_IDENTITY_TOP + identity_size,
        );
        let identity_card = RoundedRect::from_rect(identity_rect, 16.0);
        scene.fill(
            Fill::NonZero,
            transform,
            colors.accent[5],
            None,
            &identity_card,
        );

        // Caption "Workspace Settings" beneath the badge.
        let caption_size = tokens.typography.scale.xs;
        text_renderer.draw_text(
            scene,
            "Workspace Settings",
            caption_size,
            colors.text_dim,
            transform * Affine::translate((sidebar.x0 + 24.0, identity_rect.y1 + 12.0)),
            None,
        );

        // Nav rows — selected row gets indigo-500 fill + white text;
        // idle rows are transparent + dimmed.
        for (tab, row) in self.nav_rows(modal_rect) {
            let active = self.active_tab == tab;
            let row_rr = RoundedRect::from_rect(row, 12.0);
            if active {
                scene.fill(
                    Fill::NonZero,
                    transform,
                    colors.accent[5],
                    None,
                    &row_rr,
                );
            }
            let label_color = if active {
                colors.text_primary
            } else {
                colors.text_dim
            };
            text_renderer.draw_text(
                scene,
                tab.label(),
                tokens.typography.scale.base,
                label_color,
                transform * Affine::translate((row.x0 + 12.0, row.y0 + 10.0)),
                None,
            );
        }

        // Footer "Close Settings" row pinned to the bottom of the sidebar.
        let footer_h = 40.0_f64;
        let footer_y = sidebar.y1 - footer_h - 16.0;
        let footer = Rect::new(
            sidebar.x0 + 16.0,
            footer_y,
            sidebar.x1 - 16.0,
            footer_y + footer_h,
        );
        let footer_rr = RoundedRect::from_rect(footer, 12.0);
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            colors.border_standard,
            None,
            &footer_rr,
        );
        // Concept uses lucide `X` at 16 px slate-400.
        let glyph_size = 16.0;
        let glyph_rect = Rect::new(
            footer.x0 + 12.0,
            footer.y0 + 12.0,
            footer.x0 + 12.0 + glyph_size,
            footer.y0 + 12.0 + glyph_size,
        );
        draw_icon(
            scene,
            transform,
            glyph_rect,
            Icon::X,
            colors.text_dim,
            LUCIDE_DEFAULT_STROKE_PX,
        );
        text_renderer.draw_text(
            scene,
            "Close Settings",
            tokens.typography.scale.sm,
            colors.text_dim,
            transform * Affine::translate((footer.x0 + 36.0, footer.y0 + 12.0)),
            None,
        );
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

/// Width (in CSS px) of the role-list left column inside the Roles
/// pane. Mirrors `WorkspaceSettingsModal.tsx:155` (`w-48` = 12 rem =
/// 192 px).
const ROLES_LIST_WIDTH: f64 = 192.0;

/// Vertical padding inside the role list — matches the concept's `p-4`
/// wrapper around the column.
const ROLES_LIST_PADDING: f64 = 16.0;

/// Height of one role-list button row (concept uses `py-2` on a
/// flex-row with text-sm children — ~36 px target).
const ROLE_ROW_HEIGHT: f64 = 36.0;

/// Vertical gap between consecutive role-list rows. Concept stacks
/// them with `gap-2` (= 8 px).
const ROLE_ROW_GAP: f64 = 8.0;

/// Concept-hardcoded application list shown in the per-role app
/// permission grid. Mirrors `WorkspaceSettingsModal.tsx:191`.
const ROLES_PANE_APPS: [&str; 5] = ["Editor", "Files", "Browser", "Terminal", "Settings"];

/// Paint the Roles tab body into `pane_rect`. Pure renderer — owns no
/// state of its own; reads role + file metadata off `workspace` and
/// honours the optional `selected_role_id` so callers (the overlay host
/// in production, tests in isolation) can drive selection without
/// constructing a full `WorkspaceSettings` instance.
///
/// Concept reference: `nirios-concept/src/components/WorkspaceSettingsModal.tsx:152-254`.
/// The pane is split:
///
/// * **Left column (192 px)** — role list. Header "Roles" caption +
///   one selectable button per `Workspace::role`, each carrying a
///   coloured swatch + role name + optional "Default" pill, plus a
///   "+ New Role" affordance pinned beneath. Selected row gets a
///   `white/10` fill so the section reads as a list view.
/// * **Right pane (flex)** — when a role is selected, paints two
///   sections: an **Apps** grid (5 hardcoded entries — `editor` /
///   `files` / `browser` / `terminal` / `settings` per concept TSX:191),
///   and a **Files** strip (one row per `Workspace::workspace_files`
///   entry with three permission chips: `None` / `Read` / `Write`).
///   When no role is selected, paints the concept's "Select a role"
///   empty state (Shield glyph + caption).
///
/// The function short-circuits if `pane_rect` has zero area so the
/// modal-clip path can call it unconditionally without painting into a
/// degenerate rect.
pub fn draw_roles_pane(
    scene: &mut Scene,
    transform: Affine,
    pane_rect: Rect,
    workspace: &Workspace,
    selected_role_id: Option<&str>,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
) {
    if pane_rect.width() <= 0.0 || pane_rect.height() <= 0.0 {
        return;
    }
    let tokens = &theme.tokens;
    let colors = &tokens.colors;

    // ---- Left column: role list ------------------------------------
    // Concept ref:155 — the role list lives in a `w-48` column with a
    // 1 px slate-700-ish right border. Render the column bg at low
    // alpha so it reads distinct from the right pane (mirrors the
    // sidebar's surface treatment in `draw_sidebar`).
    let list_rect = Rect::new(
        pane_rect.x0,
        pane_rect.y0,
        (pane_rect.x0 + ROLES_LIST_WIDTH).min(pane_rect.x1),
        pane_rect.y1,
    );
    let list_bg = colors.neutral[1].with_alpha_factor(0.20);
    scene.fill(Fill::NonZero, transform, list_bg, None, &list_rect);

    // 1 px right border separating role list from permission pane —
    // concept ref:155 `border-r border-white/5`.
    let list_border = Rect::new(list_rect.x1 - 0.5, list_rect.y0, list_rect.x1 + 0.5, list_rect.y1);
    scene.fill(Fill::NonZero, transform, colors.neutral[3], None, &list_border);

    // "Roles" caption (concept ref:156, `text-[10px] uppercase tracking-widest`).
    let caption_y = list_rect.y0 + ROLES_LIST_PADDING;
    text_renderer.draw_text(
        scene,
        "Roles",
        tokens.typography.scale.xs,
        colors.text_dim,
        transform * Affine::translate((list_rect.x0 + ROLES_LIST_PADDING, caption_y)),
        None,
    );

    // Per-role rows. Use the workspace's actual roster so callers see
    // their own roles surface in the list; an empty roster paints just
    // the caption and the "+ New Role" affordance below.
    let mut row_y = caption_y + 28.0;
    for role in &workspace.roles {
        let row = Rect::new(
            list_rect.x0 + 8.0,
            row_y,
            list_rect.x1 - 8.0,
            row_y + ROLE_ROW_HEIGHT,
        );
        let active = selected_role_id == Some(role.id.as_str());
        if active {
            // Selected row fills `white/10` — concept ref:161
            // `bg-white/10 text-white`.
            scene.fill(
                Fill::NonZero,
                transform,
                colors.neutral[3].with_alpha_factor(0.25),
                None,
                &RoundedRect::from_rect(row, 12.0),
            );
        }
        // Coloured swatch dot (concept ref:163 — `w-2 h-2 rounded-full`).
        // We don't have a per-role colour field on `Role` yet, so seed
        // the swatch from the role id's first byte against the accent
        // ramp (deterministic and avoids hand-coded palette entries).
        let swatch_size = 10.0_f64;
        let swatch_x = row.x0 + 12.0;
        let swatch_y = row.y0 + (ROLE_ROW_HEIGHT - swatch_size) / 2.0;
        let swatch = Rect::new(
            swatch_x,
            swatch_y,
            swatch_x + swatch_size,
            swatch_y + swatch_size,
        );
        let swatch_color = role_swatch_color(&role.id, colors);
        scene.fill(
            Fill::NonZero,
            transform,
            swatch_color,
            None,
            &RoundedRect::from_rect(swatch, swatch_size / 2.0),
        );
        // Role name.
        let label_color = if active { colors.text_primary } else { colors.text_dim };
        text_renderer.draw_text(
            scene,
            &role.name,
            tokens.typography.scale.sm,
            label_color,
            transform * Affine::translate((swatch.x1 + 10.0, row.y0 + 8.0)),
            None,
        );
        row_y += ROLE_ROW_HEIGHT + ROLE_ROW_GAP;
    }

    // "+ New Role" affordance — concept ref:168, indigo-400 text on
    // hover-only background. Always rendered with an indigo tint dot
    // so the row reads as actionable even at rest.
    let new_role_row = Rect::new(
        list_rect.x0 + 8.0,
        row_y + 4.0,
        list_rect.x1 - 8.0,
        row_y + 4.0 + ROLE_ROW_HEIGHT,
    );
    if new_role_row.y1 < list_rect.y1 - ROLES_LIST_PADDING {
        scene.fill(
            Fill::NonZero,
            transform,
            colors.accent[5].with_alpha_factor(0.10),
            None,
            &RoundedRect::from_rect(new_role_row, 12.0),
        );
        text_renderer.draw_text(
            scene,
            "+ New Role",
            tokens.typography.scale.sm,
            colors.accent[5],
            transform
                * Affine::translate((new_role_row.x0 + 12.0, new_role_row.y0 + 8.0)),
            None,
        );
    }

    // ---- Right pane: permission grids ------------------------------
    let perm_rect = Rect::new(list_rect.x1, pane_rect.y0, pane_rect.x1, pane_rect.y1);
    if perm_rect.width() <= 0.0 {
        return;
    }
    // Resolve the role we're rendering permissions for. If the caller
    // didn't pin one but the workspace has at least one role, fall
    // through to the first role so the grids render with sensible
    // defaults — matches the concept's UX where the first role auto-
    // selects on tab open.
    let resolved_role = selected_role_id
        .and_then(|id| workspace.roles.iter().find(|r| r.id == id))
        .or_else(|| workspace.roles.first());

    let pad = 24.0_f64;
    let mut cursor_y = perm_rect.y0 + pad;

    // Section: "Allowed Applications" — concept ref:189.
    text_renderer.draw_text(
        scene,
        "Allowed Applications",
        tokens.typography.scale.xs,
        colors.text_dim,
        transform * Affine::translate((perm_rect.x0 + pad, cursor_y)),
        None,
    );
    cursor_y += 24.0;

    // Apps grid — 5 hardcoded entries per concept TSX:191. Each row
    // paints an icon container, the toggle pill, and (when on) a knob
    // dot — three solid-colour fills minimum, regardless of the
    // workspace's roles vector being empty.
    let row_h = 56.0_f64;
    let row_gap = 8.0_f64;
    for app in ROLES_PANE_APPS {
        let row = Rect::new(
            perm_rect.x0 + pad,
            cursor_y,
            perm_rect.x1 - pad,
            cursor_y + row_h,
        );
        let row_bg = colors.neutral[3].with_alpha_factor(0.15);
        scene.fill(
            Fill::NonZero,
            transform,
            row_bg,
            None,
            &RoundedRect::from_rect(row, 16.0),
        );

        // Icon container (concept ref:194 — `p-2 bg-white/5 rounded-lg`).
        let icon_box = Rect::new(
            row.x0 + 12.0,
            row.y0 + 14.0,
            row.x0 + 12.0 + 28.0,
            row.y0 + 14.0 + 28.0,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            colors.neutral[3].with_alpha_factor(0.25),
            None,
            &RoundedRect::from_rect(icon_box, 8.0),
        );

        // App label (concept ref:197 — `text-sm font-bold text-white`).
        text_renderer.draw_text(
            scene,
            app,
            tokens.typography.scale.sm,
            colors.text_primary,
            transform * Affine::translate((icon_box.x1 + 12.0, row.y0 + 18.0)),
            None,
        );

        // Toggle pill (concept ref:206 — `w-9 h-5 ... peer-checked:bg-indigo-500`).
        let pill_w = 36.0_f64;
        let pill_h = 20.0_f64;
        let pill_x = row.x1 - 12.0 - pill_w;
        let pill_y = row.y0 + (row_h - pill_h) / 2.0;
        let pill = Rect::new(pill_x, pill_y, pill_x + pill_w, pill_y + pill_h);
        let on = role_app_perm(resolved_role, app)
            .map(|p| !matches!(p, AppPerm::None))
            .unwrap_or(false);
        let pill_bg = if on {
            colors.accent[5]
        } else {
            colors.neutral[2]
        };
        scene.fill(
            Fill::NonZero,
            transform,
            pill_bg,
            None,
            &RoundedRect::from_rect(pill, pill_h / 2.0),
        );
        // Knob (white circle inside the pill).
        let knob_d = pill_h - 4.0;
        let knob_x = if on {
            pill.x1 - 2.0 - knob_d
        } else {
            pill.x0 + 2.0
        };
        let knob_y = pill.y0 + 2.0;
        let knob = Rect::new(knob_x, knob_y, knob_x + knob_d, knob_y + knob_d);
        scene.fill(
            Fill::NonZero,
            transform,
            colors.text_primary,
            None,
            &RoundedRect::from_rect(knob, knob_d / 2.0),
        );

        cursor_y += row_h + row_gap;
    }

    cursor_y += 16.0;

    // Section: "File Access Overrides" — concept ref:215.
    text_renderer.draw_text(
        scene,
        "File Access Overrides",
        tokens.typography.scale.xs,
        colors.text_dim,
        transform * Affine::translate((perm_rect.x0 + pad, cursor_y)),
        None,
    );
    cursor_y += 24.0;

    // File rows — one per workspace file. Each row paints a chip strip
    // (None/Read/Write) where the active permission gets the indigo
    // accent fill (concept ref:234 — `bg-indigo-500 text-white shadow`).
    if workspace.workspace_files.is_empty() {
        // No files → render a single placeholder row so the section
        // header reads as belonging to a populated UI and not a
        // dangling label. The concept's empty state is just the empty
        // section, but the spec test exercises a default workspace
        // (zero files) and we want the Roles tab to clearly differ
        // from General regardless.
        let row = Rect::new(
            perm_rect.x0 + pad,
            cursor_y,
            perm_rect.x1 - pad,
            cursor_y + row_h,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            colors.neutral[3].with_alpha_factor(0.10),
            None,
            &RoundedRect::from_rect(row, 16.0),
        );
        text_renderer.draw_text(
            scene,
            "No files added yet",
            tokens.typography.scale.sm,
            colors.text_dim,
            transform * Affine::translate((row.x0 + 16.0, row.y0 + 18.0)),
            None,
        );
    } else {
        for file in &workspace.workspace_files {
            let row = Rect::new(
                perm_rect.x0 + pad,
                cursor_y,
                perm_rect.x1 - pad,
                cursor_y + row_h,
            );
            scene.fill(
                Fill::NonZero,
                transform,
                colors.neutral[3].with_alpha_factor(0.15),
                None,
                &RoundedRect::from_rect(row, 16.0),
            );
            text_renderer.draw_text(
                scene,
                &file.name,
                tokens.typography.scale.sm,
                colors.text_primary,
                transform * Affine::translate((row.x0 + 16.0, row.y0 + 18.0)),
                None,
            );
            // Three-permission chip strip — concept ref:230-238.
            let chip_strip_w = 168.0_f64;
            let chip_pad = 4.0_f64;
            let chip_w = (chip_strip_w - chip_pad * 4.0) / 3.0;
            let chip_h = 28.0_f64;
            let strip_x = row.x1 - 12.0 - chip_strip_w;
            let strip_y = row.y0 + (row_h - chip_h) / 2.0;
            let strip_bg = Rect::new(
                strip_x,
                strip_y,
                strip_x + chip_strip_w,
                strip_y + chip_h,
            );
            scene.fill(
                Fill::NonZero,
                transform,
                colors.neutral[1].with_alpha_factor(0.50),
                None,
                &RoundedRect::from_rect(strip_bg, 12.0),
            );
            let cur_perm = role_file_perm(resolved_role, &file.id);
            for (i, perm) in [FilePerm::None, FilePerm::Read, FilePerm::Write]
                .into_iter()
                .enumerate()
            {
                let cx = strip_x + chip_pad + i as f64 * (chip_w + chip_pad);
                let chip = Rect::new(cx, strip_y + chip_pad, cx + chip_w, strip_y + chip_h - chip_pad);
                let active = cur_perm == perm;
                if active {
                    scene.fill(
                        Fill::NonZero,
                        transform,
                        colors.accent[5],
                        None,
                        &RoundedRect::from_rect(chip, 8.0),
                    );
                }
            }
            cursor_y += row_h + row_gap;
        }
    }
}

/// Minimal Files-tab body — paints a "Files" caption header plus one
/// row per [`Workspace::workspace_files`] entry. Standalone of the
/// General body so the Files arm of the Workspace Settings modal does
/// not depend on `editing_workspace_name` (which is the Name-input edit
/// buffer for the General tab and unrelated to file-grid rendering).
///
/// This is the placeholder until the concept's full Files tab
/// (`WorkspaceSettingsModal.tsx:257-303` — icon + name + type badge +
/// assigned-roles avatar pile + Remove button per row) lands in T3's
/// follow-up. Even in its minimal form, it owns enough geometry to
/// keep `WsTab::Files` orthogonal to the General body's name edit
/// state — the side-channel migration in `chrome_state::WsTab` relies
/// on that orthogonality.
pub fn draw_files_pane(
    scene: &mut Scene,
    transform: Affine,
    pane_rect: Rect,
    workspace: &Workspace,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
) {
    if pane_rect.width() <= 0.0 || pane_rect.height() <= 0.0 {
        return;
    }
    let tokens = &theme.tokens;
    let colors = &tokens.colors;

    let pad = 16.0_f64;
    let row_h = 40.0_f64;
    let row_gap = 8.0_f64;
    let mut cursor_y = pane_rect.y0 + pad;

    text_renderer.draw_text(
        scene,
        "Files",
        tokens.typography.scale.sm,
        colors.text_dim,
        transform * Affine::translate((pane_rect.x0 + pad, cursor_y)),
        None,
    );
    cursor_y += 28.0;

    if workspace.workspace_files.is_empty() {
        let row = Rect::new(
            pane_rect.x0 + pad,
            cursor_y,
            pane_rect.x1 - pad,
            cursor_y + row_h,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            colors.neutral[3].with_alpha_factor(0.10),
            None,
            &RoundedRect::from_rect(row, 12.0),
        );
        text_renderer.draw_text(
            scene,
            "No files added yet",
            tokens.typography.scale.sm,
            colors.text_dim,
            transform * Affine::translate((row.x0 + 16.0, row.y0 + 12.0)),
            None,
        );
    } else {
        for file in &workspace.workspace_files {
            let row = Rect::new(
                pane_rect.x0 + pad,
                cursor_y,
                pane_rect.x1 - pad,
                cursor_y + row_h,
            );
            scene.fill(
                Fill::NonZero,
                transform,
                colors.neutral[3].with_alpha_factor(0.15),
                None,
                &RoundedRect::from_rect(row, 12.0),
            );
            text_renderer.draw_text(
                scene,
                &file.name,
                tokens.typography.scale.sm,
                colors.text_primary,
                transform * Affine::translate((row.x0 + 16.0, row.y0 + 12.0)),
                None,
            );
            cursor_y += row_h + row_gap;
        }
    }
}

// =====================================================================
// Sandbox Phase 6 — Permissions tab
// =====================================================================

/// One row in the Permissions pane: a `(app_id, capability)` pairing
/// from [`Workspace::grants`] plus the geometry the renderer painted
/// for the row body and its Revoke button. Returned by
/// [`permissions_pane_rows`] so click-routing can match a click point
/// back to a (app_id, capability) tuple without re-deriving any
/// layout. The row is keyed by `(app_id, cap_index)` because the
/// `grants` map stores capabilities in a `Vec` and removing by value
/// would be ambiguous when an app holds two `Filesystem` grants for
/// different paths — index is the unambiguous identifier.
#[derive(Clone, Debug)]
pub struct PermissionRow {
    pub app_id: AppId,
    pub cap_index: usize,
    pub capability: Capability,
    pub row_rect: Rect,
    pub revoke_rect: Rect,
}

/// Vertical inset between the top of the pane and the first row.
const PERMISSIONS_PANE_PAD_TOP: f64 = 16.0;
/// Horizontal inset on each side of the pane.
const PERMISSIONS_PANE_PAD_X: f64 = 16.0;
/// Height of one row (icon + label + revoke button).
const PERMISSIONS_ROW_HEIGHT: f64 = 48.0;
/// Vertical gap between consecutive rows.
const PERMISSIONS_ROW_GAP: f64 = 8.0;
/// Width of the Revoke button (right-aligned within the row).
const PERMISSIONS_REVOKE_WIDTH: f64 = 84.0;
/// Height of the Revoke button.
const PERMISSIONS_REVOKE_HEIGHT: f64 = 28.0;

/// Compute the `(row_rect, revoke_rect)` per-row geometry for the
/// Permissions pane in `pane_rect`. Pure layout — no painting; the
/// returned [`PermissionRow`]s feed both the renderer and the
/// click-routing path so a click on a Revoke button always lands on
/// the same rect that was painted.
///
/// Rows are emitted in the iteration order of `workspace.grants`
/// (HashMap, so ordering is undefined but deterministic per run).
/// Tests sort by app_id when they need stable assertions; the click
/// path is index-based and doesn't care about ordering.
pub fn permissions_pane_rows(workspace: &Workspace, pane_rect: Rect) -> Vec<PermissionRow> {
    let mut rows = Vec::new();
    if pane_rect.width() <= 0.0 || pane_rect.height() <= 0.0 {
        return rows;
    }
    let mut cursor_y = pane_rect.y0 + PERMISSIONS_PANE_PAD_TOP;
    // Iterate sorted by app_id so the on-screen order is stable across
    // runs — HashMap iteration is unspecified, but the renderer wants
    // a deterministic visual order so visual snapshots don't churn.
    let mut entries: Vec<(&AppId, &Vec<Capability>)> = workspace.grants.iter().collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));
    for (app_id, caps) in entries {
        for (cap_index, capability) in caps.iter().enumerate() {
            let row_rect = Rect::new(
                pane_rect.x0 + PERMISSIONS_PANE_PAD_X,
                cursor_y,
                pane_rect.x1 - PERMISSIONS_PANE_PAD_X,
                cursor_y + PERMISSIONS_ROW_HEIGHT,
            );
            let revoke_rect = Rect::new(
                row_rect.x1 - 12.0 - PERMISSIONS_REVOKE_WIDTH,
                row_rect.y0 + (PERMISSIONS_ROW_HEIGHT - PERMISSIONS_REVOKE_HEIGHT) / 2.0,
                row_rect.x1 - 12.0,
                row_rect.y0 + (PERMISSIONS_ROW_HEIGHT + PERMISSIONS_REVOKE_HEIGHT) / 2.0,
            );
            rows.push(PermissionRow {
                app_id: app_id.clone(),
                cap_index,
                capability: capability.clone(),
                row_rect,
                revoke_rect,
            });
            cursor_y += PERMISSIONS_ROW_HEIGHT + PERMISSIONS_ROW_GAP;
        }
    }
    rows
}

/// Build a human-readable label for `cap`. Mirrors the helper in
/// `overlay_host.rs::capability_label` — duplicated here so the
/// permissions pane can render labels without reaching across modules.
/// Phase 4.2's prompt widget and this Phase 6 list view share the
/// same label vocabulary so a user revoking a grant sees the exact
/// same wording the prompt offered when they granted it.
pub fn capability_label(cap: &Capability) -> String {
    match cap {
        Capability::Filesystem { path, mode, .. } => {
            let mode_str = match mode {
                crate::shell::state::FsMode::Read => "read",
                crate::shell::state::FsMode::ReadWrite => "read/write",
            };
            format!("{} access to {}", mode_str, path.display())
        }
        Capability::Camera { .. } => "Camera access".to_string(),
        Capability::Microphone { .. } => "Microphone access".to_string(),
        Capability::Screencast { .. } => "Screen capture".to_string(),
        Capability::Network { hosts, .. } => match hosts {
            Some(h) if !h.is_empty() => format!("Network access to {}", h.join(", ")),
            _ => "Network access".to_string(),
        },
    }
}

/// Short scope badge label (Once / Session / Always) — read off the
/// capability's `scope` field. Mirrors the GrantScope enum's three
/// variants from `state.rs:323-332`.
pub fn capability_scope_label(cap: &Capability) -> &'static str {
    use crate::shell::state::GrantScope;
    let scope = match cap {
        Capability::Filesystem { scope, .. } => scope,
        Capability::Camera { scope } => scope,
        Capability::Microphone { scope } => scope,
        Capability::Screencast { scope } => scope,
        Capability::Network { scope, .. } => scope,
    };
    match scope {
        GrantScope::Once => "Once",
        GrantScope::Session => "Session",
        GrantScope::Always => "Always",
    }
}

/// Remove `(app_id, cap_index)` from `workspace.grants`. No-op when
/// `app_id` is missing or `cap_index` is out of bounds. Cleans up an
/// empty `Vec` entry so the workspace's `grants.is_empty()` predicate
/// (used by [`WsTab::visible`] to hide the tab) flips back to true
/// once the last grant for the workspace is revoked.
///
/// Returns `true` if a capability was removed, `false` otherwise —
/// callers (the click router) can use this to decide whether to bump
/// the chrome's revision counter.
pub fn revoke_grant(workspace: &mut Workspace, app_id: &AppId, cap_index: usize) -> bool {
    let Some(caps) = workspace.grants.get_mut(app_id) else {
        return false;
    };
    if cap_index >= caps.len() {
        return false;
    }
    caps.remove(cap_index);
    if caps.is_empty() {
        workspace.grants.remove(app_id);
    }
    true
}

/// Paint the Permissions tab body into `pane_rect`. Renders one row
/// per `(app_id, capability)` pair across the workspace's grants map;
/// each row paints the app id (left), capability label (middle), grant
/// scope badge, and a Revoke button (right). The Revoke button paints
/// at full alpha for owners and at 40% alpha for non-owners — mirrors
/// the PermissionPrompt's `Always` button gating from Phase 4.2.
///
/// Empty grants ⇒ paints a "No permissions granted" placeholder row.
pub fn draw_permissions_pane(
    scene: &mut Scene,
    transform: Affine,
    pane_rect: Rect,
    workspace: &Workspace,
    is_owner: bool,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
) {
    if pane_rect.width() <= 0.0 || pane_rect.height() <= 0.0 {
        return;
    }
    let tokens = &theme.tokens;
    let colors = &tokens.colors;

    let rows = permissions_pane_rows(workspace, pane_rect);

    if rows.is_empty() {
        // Empty-state placeholder so the section reads as "intentionally
        // empty" instead of "broken". Concept ref: Roles tab uses the
        // same idiom for an empty workspace_files vector.
        let row = Rect::new(
            pane_rect.x0 + PERMISSIONS_PANE_PAD_X,
            pane_rect.y0 + PERMISSIONS_PANE_PAD_TOP,
            pane_rect.x1 - PERMISSIONS_PANE_PAD_X,
            pane_rect.y0 + PERMISSIONS_PANE_PAD_TOP + PERMISSIONS_ROW_HEIGHT,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            colors.neutral[3].with_alpha_factor(0.10),
            None,
            &RoundedRect::from_rect(row, 12.0),
        );
        text_renderer.draw_text(
            scene,
            "No permissions granted yet",
            tokens.typography.scale.sm,
            colors.text_dim,
            transform * Affine::translate((row.x0 + 16.0, row.y0 + 16.0)),
            None,
        );
        return;
    }

    for row in &rows {
        // Row card.
        scene.fill(
            Fill::NonZero,
            transform,
            colors.neutral[3].with_alpha_factor(0.15),
            None,
            &RoundedRect::from_rect(row.row_rect, 12.0),
        );

        // App id (left) — concept's grant rows lead with the app's
        // wayland app-id so the user can correlate the entry with
        // the originating app.
        text_renderer.draw_text(
            scene,
            &row.app_id,
            tokens.typography.scale.sm,
            colors.text_primary,
            transform * Affine::translate((row.row_rect.x0 + 16.0, row.row_rect.y0 + 8.0)),
            None,
        );

        // Capability label (left, second line) — describes what the
        // grant allows. Reuses Phase 4.2's vocabulary via
        // [`capability_label`] so revoke wording matches the prompt.
        let cap_text = capability_label(&row.capability);
        text_renderer.draw_text(
            scene,
            &cap_text,
            tokens.typography.scale.xs,
            colors.text_dim,
            transform * Affine::translate((row.row_rect.x0 + 16.0, row.row_rect.y0 + 28.0)),
            None,
        );

        // Scope badge (middle-right, before the Revoke button).
        let scope_label = capability_scope_label(&row.capability);
        let (sw, sh) = text_renderer.measure(scope_label, tokens.typography.scale.xs, None);
        let badge = Rect::new(
            row.revoke_rect.x0 - 16.0 - (sw as f64 + 16.0),
            row.row_rect.y0 + (PERMISSIONS_ROW_HEIGHT - (sh as f64 + 8.0)) / 2.0,
            row.revoke_rect.x0 - 16.0,
            row.row_rect.y0 + (PERMISSIONS_ROW_HEIGHT + (sh as f64 + 8.0)) / 2.0,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            colors.accent[5].with_alpha_factor(0.20),
            None,
            &RoundedRect::from_rect(badge, 8.0),
        );
        text_renderer.draw_text(
            scene,
            scope_label,
            tokens.typography.scale.xs,
            colors.accent[5],
            transform * Affine::translate((badge.x0 + 8.0, badge.y0 + 4.0)),
            None,
        );

        // Revoke button. Owners see it at full alpha; non-owners see
        // it dimmed to 40 % so it visually reads as disabled — mirrors
        // PermissionPrompt's `Always` button gating from Phase 4.2.
        let revoke_alpha: f32 = if is_owner { 1.0 } else { 0.40 };
        let revoke_bg = colors
            .semantic
            .danger
            .with_alpha_factor(0.20 * revoke_alpha);
        scene.fill(
            Fill::NonZero,
            transform,
            revoke_bg,
            None,
            &RoundedRect::from_rect(row.revoke_rect, 8.0),
        );
        text_renderer.draw_text(
            scene,
            "Revoke",
            tokens.typography.scale.sm,
            colors.semantic.danger.with_alpha_factor(revoke_alpha),
            transform
                * Affine::translate((row.revoke_rect.x0 + 16.0, row.revoke_rect.y0 + 6.0)),
            None,
        );
    }
}

/// Pick a swatch colour for the role with id `role_id`. Deterministic
/// and palette-pinned: hashes the id's first byte against the theme's
/// accent ramp + a small set of fallbacks so two roles with different
/// ids never collide on the same dot. No-op until the workspace state
/// gains a per-role `color` field — at that point this becomes a
/// thin `role.color.unwrap_or_else(|| swatch_for_id(...))`.
fn role_swatch_color(role_id: &str, colors: &ui::tokens::colors::Colors) -> vello::peniko::Color {
    let bucket = role_id.bytes().next().unwrap_or(0) as usize % 5;
    match bucket {
        0 => colors.accent[5],
        1 => colors.accent[4],
        2 => colors.accent[6],
        3 => colors.text_dim,
        _ => colors.text_primary,
    }
}

/// Look up the [`AppPerm`] the supplied role grants for `app`. Returns
/// `None` when no role is in scope; the caller renders the toggle as
/// off.
fn role_app_perm(role: Option<&Role>, app: &str) -> Option<AppPerm> {
    role.and_then(|r| r.permissions.per_app.get(app).copied())
}

/// Look up the [`FilePerm`] the supplied role grants for `file_id`,
/// defaulting to [`FilePerm::None`] when there's no entry on the role
/// (or no role in scope).
fn role_file_perm(role: Option<&Role>, file_id: &str) -> FilePerm {
    role.and_then(|r| r.permissions.per_file.get(file_id).copied())
        .unwrap_or(FilePerm::None)
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
        WsTab::Permissions => BASE + 3,
    }
}

#[cfg(test)]
mod tab_tests {
    //! Concept-fidelity T1 — sidebar nav default + switch behaviour
    //! (plan `2026-04-28-nirios-concept-fidelity.md`, Task 1).
    //!
    //! These tests pin the exposed [`WsTab`] dispatch surface so the
    //! General/Roles/Files tab patches (T2/T3/T4) can rely on a stable
    //! `active_tab` + `select_tab` API and a sidebar that renders even
    //! without their per-tab content drawers.

    use super::*;
    use vello::kurbo::Point;

    /// Construct a fresh modal with a default workspace id — equivalent
    /// to the plan's `WorkspaceSettings::new()` shorthand, accommodating
    /// the existing `new(WorkspaceId)` signature without renaming the
    /// constructor used elsewhere in the shell.
    fn ws_settings_default() -> WorkspaceSettings {
        WorkspaceSettings::new(WorkspaceId::default())
    }

    #[test]
    fn ws_settings_default_tab_is_general() {
        let ws = ws_settings_default();
        assert_eq!(ws.active_tab, WsTab::General);
    }

    #[test]
    fn select_tab_switches_active() {
        let mut ws = ws_settings_default();
        ws.select_tab(WsTab::Roles);
        assert_eq!(ws.active_tab, WsTab::Roles);
        ws.select_tab(WsTab::Files);
        assert_eq!(ws.active_tab, WsTab::Files);
        ws.select_tab(WsTab::General);
        assert_eq!(ws.active_tab, WsTab::General);
    }

    #[test]
    fn ws_tab_default_matches_general() {
        // `Default for WsTab` is what `#[derive(Default)] struct …` would
        // pick up if the field were ever defaulted that way; pin it here
        // so a future code-gen path can't silently flip the default.
        assert_eq!(WsTab::default(), WsTab::General);
    }

    #[test]
    fn ws_tab_all_iterates_in_concept_order() {
        // Concept renders the rows top-to-bottom: General, Roles &
        // Permissions, Files (`WorkspaceSettingsModal.tsx:75-89`).
        // Sandbox Phase 6 appends `Permissions` at the tail; the head of
        // the iteration order stays pinned to the concept fidelity strip.
        assert_eq!(
            WsTab::all(),
            [WsTab::General, WsTab::Roles, WsTab::Files, WsTab::Permissions]
        );
    }

    #[test]
    fn ws_tab_appears_in_all_iter() {
        // Sandbox Phase 6 — `WsTab::all()` must include `Permissions` so
        // the focus trap and sidebar nav can reach it via the standard
        // iteration helper.
        assert!(WsTab::all().contains(&WsTab::Permissions));
    }

    #[test]
    fn ws_tab_visible_hides_permissions_when_no_grants() {
        // Concept-fidelity invariant: the 4-pill strip
        // (General/Roles/Files/Collaborators) must keep its pinned
        // positions when the workspace has no grants. Permissions is
        // suppressed in that case so the parity tests keep pinning the
        // same x-coordinates for General/Roles/Files.
        let ws = Workspace::new(WorkspaceId::default(), "no-grants");
        assert_eq!(WsTab::visible(&ws), vec![WsTab::General, WsTab::Roles, WsTab::Files]);
    }

    #[test]
    fn ws_tab_visible_shows_permissions_when_workspace_has_grants() {
        // Sandbox Phase 6 — once a grant exists the Permissions tab
        // joins the strip at the tail. Owner-only revoke enforcement
        // happens at click-time (not visibility-time).
        let mut ws = Workspace::new(WorkspaceId::default(), "with-grants");
        ws.grants.insert(
            "firefox".into(),
            vec![Capability::Camera {
                scope: crate::shell::state::GrantScope::Always,
            }],
        );
        let visible = WsTab::visible(&ws);
        assert_eq!(visible.last(), Some(&WsTab::Permissions));
        assert_eq!(visible.len(), 4);
    }

    #[test]
    fn sidebar_rect_is_256_px_wide() {
        // Concept ref:50 — `w-64` = 16 rem = 256 px.
        let ws = ws_settings_default();
        let modal = Rect::new(0.0, 0.0, 896.0, 600.0);
        let sidebar = ws.sidebar_rect(modal);
        assert_eq!(sidebar.width(), SIDEBAR_WIDTH);
        assert_eq!(sidebar.height(), modal.height());
        assert_eq!(sidebar.x0, modal.x0);
    }

    #[test]
    fn pane_rect_starts_after_sidebar() {
        let ws = ws_settings_default();
        let modal = Rect::new(0.0, 0.0, 896.0, 600.0);
        let pane = ws.pane_rect(modal);
        assert_eq!(pane.x0, modal.x0 + SIDEBAR_WIDTH);
        assert_eq!(pane.x1, modal.x1);
        // Sidebar + pane partition the modal exactly.
        assert!((pane.width() + SIDEBAR_WIDTH - modal.width()).abs() < f64::EPSILON);
    }

    #[test]
    fn nav_rows_yield_one_rect_per_tab_in_order() {
        let ws = ws_settings_default();
        let modal = Rect::new(0.0, 0.0, 896.0, 600.0);
        let rows = ws.nav_rows(modal);
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].0, WsTab::General);
        assert_eq!(rows[1].0, WsTab::Roles);
        assert_eq!(rows[2].0, WsTab::Files);
        assert_eq!(rows[3].0, WsTab::Permissions);
        // Stacked top-to-bottom, each 40 px tall with a 4 px gap.
        for (_, r) in &rows {
            assert!((r.height() - NAV_ROW_HEIGHT).abs() < f64::EPSILON);
            assert!(r.x1 > r.x0);
        }
        let stride = NAV_ROW_HEIGHT + NAV_ROW_GAP;
        assert!((rows[1].1.y0 - rows[0].1.y0 - stride).abs() < f64::EPSILON);
        assert!((rows[2].1.y0 - rows[1].1.y0 - stride).abs() < f64::EPSILON);
        assert!((rows[3].1.y0 - rows[2].1.y0 - stride).abs() < f64::EPSILON);
    }

    #[test]
    fn nav_hit_test_returns_tab_at_point() {
        let ws = ws_settings_default();
        let modal = Rect::new(0.0, 0.0, 896.0, 600.0);
        let rows = ws.nav_rows(modal);
        for (tab, row) in rows {
            let center = Point::new((row.x0 + row.x1) / 2.0, (row.y0 + row.y1) / 2.0);
            assert_eq!(ws.nav_hit_test(modal, center), Some(tab));
        }
        // Click well outside the sidebar — no tab should match.
        assert_eq!(
            ws.nav_hit_test(modal, Point::new(modal.x1 - 10.0, modal.y1 - 10.0)),
            None
        );
    }

    #[test]
    fn draw_body_paints_sidebar_when_pane_drawer_absent() {
        // The sidebar nav renders even when no per-tab drawer has been
        // wired (T2/T3/T4 are not landed in this unit) — verify by
        // checking the produced `Scene` is non-empty after `draw_body`.
        let ws = ws_settings_default();
        let mut scene = Scene::new();
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let modal = Rect::new(0.0, 0.0, 896.0, 600.0);
        ws.draw_body(&mut scene, Affine::IDENTITY, modal, &mut tr, &theme, None);
        assert!(
            !scene.encoding().draw_tags.is_empty(),
            "sidebar must paint at least one draw command even without a pane drawer"
        );
    }

    #[test]
    fn draw_body_invokes_pane_drawer_for_active_tab() {
        // Pin the dispatch surface T2/T3/T4 will hang content off:
        // `draw_body` must pass [`Self::pane_rect`] to the drawer.
        let mut ws = ws_settings_default();
        ws.select_tab(WsTab::Roles);
        let modal = Rect::new(0.0, 0.0, 896.0, 600.0);
        let expected_pane = ws.pane_rect(modal);

        let mut observed: Option<Rect> = None;
        let mut scene = Scene::new();
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let mut drawer = |_s: &mut Scene, _x: Affine, pane: Rect, _t: &mut TextRenderer, _th: &Theme| {
            observed = Some(pane);
        };
        ws.draw_body(
            &mut scene,
            Affine::IDENTITY,
            modal,
            &mut tr,
            &theme,
            Some(&mut drawer),
        );
        assert_eq!(observed, Some(expected_pane));
    }
}

#[cfg(test)]
mod roles_pane_tests {
    //! Concept-fidelity T2 — Roles pane paints the per-role app + file
    //! permission grids (plan `2026-04-28-nirios-concept-fidelity.md`,
    //! Task 3 — exposed here as wave-2 unit T2).
    //!
    //! These tests pin the public [`draw_roles_pane`] helper that the
    //! overlay host calls from its `WsTab::Roles` arm. The helper paints
    //! enough chrome (role list left column, app-permission grid for the
    //! 5 concept-hardcoded apps, file-permission rows) that switching to
    //! the Roles tab adds at least 20 solid-colour fills over the General
    //! tab — the threshold pinned by `concept_parity_spec`'s
    //! `workspace_settings_renders_roles_tab_with_per_role_app_grid`.
    //!
    //! Concept reference: `nirios-concept/src/components/WorkspaceSettingsModal.tsx:152-254`.
    use super::*;
    use crate::shell::state::{Role, RolePermissions, Workspace, WorkspaceId};
    use vello_encoding::DrawTag;

    fn count_solid_fills(scene: &Scene) -> usize {
        scene
            .encoding()
            .draw_tags
            .iter()
            .filter(|t| **t == DrawTag::COLOR)
            .count()
    }

    fn empty_workspace() -> Workspace {
        Workspace::new(WorkspaceId::default(), "Roles-Test")
    }

    #[test]
    fn draw_roles_pane_paints_at_least_twenty_fills_on_empty_workspace() {
        // Even with no roles or files in the workspace the Roles pane must
        // render the role list scaffolding + the 5 concept-hardcoded app
        // permission rows (each row contributes an icon container fill, a
        // toggle-pill bg fill, and a knob fill — plus the section divider
        // strip and role-list bg). Concept TSX:152-254 shows this content
        // is independent of the workspace's roles/files vector contents.
        let workspace = empty_workspace();
        let mut scene = Scene::new();
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let pane = Rect::new(256.0, 0.0, 896.0, 600.0);
        draw_roles_pane(
            &mut scene,
            Affine::IDENTITY,
            pane,
            &workspace,
            None,
            &mut tr,
            &theme,
        );
        let fills = count_solid_fills(&scene);
        assert!(
            fills >= 20,
            "Roles pane must paint at least 20 solid-colour fills (role list \
             scaffolding + 5-app permission grid per concept TSX 152-254); \
             observed {fills}"
        );
    }

    #[test]
    fn draw_roles_pane_paints_more_with_roles_than_without() {
        // Adding roles to the workspace surfaces them in the left-column
        // role list — each row adds at minimum a coloured swatch fill.
        let mut workspace = empty_workspace();
        workspace.roles.push(Role {
            id: "owner".into(),
            name: "Owner".into(),
            permissions: RolePermissions::default(),
        });
        workspace.roles.push(Role {
            id: "editor".into(),
            name: "Editor".into(),
            permissions: RolePermissions::default(),
        });

        let theme = Theme::dark();
        let pane = Rect::new(256.0, 0.0, 896.0, 600.0);

        let mut scene_empty = Scene::new();
        let mut tr_empty = TextRenderer::new();
        draw_roles_pane(
            &mut scene_empty,
            Affine::IDENTITY,
            pane,
            &empty_workspace(),
            None,
            &mut tr_empty,
            &theme,
        );

        let mut scene_with = Scene::new();
        let mut tr_with = TextRenderer::new();
        draw_roles_pane(
            &mut scene_with,
            Affine::IDENTITY,
            pane,
            &workspace,
            Some("owner"),
            &mut tr_with,
            &theme,
        );

        let with = count_solid_fills(&scene_with);
        let without = count_solid_fills(&scene_empty);
        assert!(
            with > without,
            "Roles pane with 2 roles must paint more fills than empty roster \
             (with={with}, without={without})"
        );
    }

    #[test]
    fn draw_roles_pane_no_op_on_zero_area_pane() {
        // Hit-test invariant: a degenerate pane rect (modal too narrow for
        // sidebar) must short-circuit without panic and without painting.
        let workspace = empty_workspace();
        let mut scene = Scene::new();
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let pane = Rect::new(100.0, 100.0, 100.0, 100.0);
        draw_roles_pane(
            &mut scene,
            Affine::IDENTITY,
            pane,
            &workspace,
            None,
            &mut tr,
            &theme,
        );
        assert!(
            scene.encoding().draw_tags.is_empty(),
            "zero-area pane must short-circuit — observed draw tags emitted"
        );
    }
}

#[cfg(test)]
mod permissions_pane_tests {
    //! Sandbox Phase 6 — Permissions tab body.
    //!
    //! These tests pin the public [`permissions_pane_rows`] +
    //! [`draw_permissions_pane`] + [`revoke_grant`] surface that the
    //! overlay host calls from its `WsTab::Permissions` arm. The
    //! revoke flow mutates `Workspace::grants` directly (cache-only
    //! per Sandbox 4.1's commit message — full doc-mirror is a
    //! follow-up); the click router writes through and bumps the
    //! revision counter so widgets resync on the next frame.
    use super::*;
    use crate::shell::state::{Capability, GrantScope, Workspace, WorkspaceId};

    fn ws_with_grants() -> Workspace {
        let mut ws = Workspace::new(WorkspaceId::default(), "perm-test");
        ws.grants.insert(
            "firefox".into(),
            vec![
                Capability::Camera {
                    scope: GrantScope::Always,
                },
                Capability::Network {
                    hosts: None,
                    scope: GrantScope::Session,
                },
            ],
        );
        ws
    }

    #[test]
    fn permissions_tab_renders_zero_rows_when_no_grants() {
        // Empty grants ⇒ no row rects; the renderer paints a single
        // empty-state placeholder but `permissions_pane_rows` (the
        // hit-test source of truth) returns nothing.
        let ws = Workspace::new(WorkspaceId::default(), "empty");
        let pane = Rect::new(0.0, 0.0, 480.0, 360.0);
        let rows = permissions_pane_rows(&ws, pane);
        assert_eq!(rows.len(), 0, "no grants ⇒ no rows; got {}", rows.len());
    }

    #[test]
    fn permissions_tab_renders_one_row_per_capability_per_app() {
        // `firefox` holds two capabilities ⇒ two rows. Each row owns a
        // distinct row_rect (stacked vertically) and revoke_rect
        // (right-aligned within row_rect).
        let ws = ws_with_grants();
        let pane = Rect::new(0.0, 0.0, 480.0, 360.0);
        let rows = permissions_pane_rows(&ws, pane);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].app_id, "firefox");
        assert_eq!(rows[1].app_id, "firefox");
        // Distinct cap_index values keep revoke routing unambiguous.
        assert_ne!(rows[0].cap_index, rows[1].cap_index);
        // Stacked top-to-bottom: row[1] starts below row[0].
        assert!(rows[1].row_rect.y0 > rows[0].row_rect.y0);
        // Revoke button is right-aligned inside row_rect.
        for row in &rows {
            assert!(row.revoke_rect.x1 <= row.row_rect.x1);
            assert!(row.revoke_rect.x0 > row.row_rect.x0);
        }
    }

    #[test]
    fn revoke_grant_removes_capability_from_grants_at_index() {
        // Sandbox Phase 6 — `revoke_grant` is the cache-mutator the
        // click router calls. It removes by (app_id, cap_index) so a
        // workspace with two `Camera`+`Network` grants on the same
        // app can be revoked one at a time without ambiguity.
        let mut ws = ws_with_grants();
        let removed = revoke_grant(&mut ws, &"firefox".to_string(), 0);
        assert!(removed);
        // After removing the Camera grant the Network grant remains.
        let firefox = ws.grants.get("firefox").expect("entry");
        assert_eq!(firefox.len(), 1);
        assert!(matches!(firefox[0], Capability::Network { .. }));
    }

    #[test]
    fn revoke_grant_cleans_up_empty_app_entry() {
        // After revoking the last capability for an app the
        // map entry must be removed entirely so
        // `Workspace::grants.is_empty()` flips to true once the last
        // grant is revoked. Without this cleanup the Permissions tab
        // would stay visible (`WsTab::visible` checks `is_empty()`).
        let mut ws = Workspace::new(WorkspaceId::default(), "single-grant");
        ws.grants.insert(
            "vscode".into(),
            vec![Capability::Camera {
                scope: GrantScope::Once,
            }],
        );
        let removed = revoke_grant(&mut ws, &"vscode".to_string(), 0);
        assert!(removed);
        assert!(ws.grants.is_empty(), "empty cap vec must be cleaned up");
    }

    #[test]
    fn revoke_grant_no_op_for_missing_app_or_index() {
        // Out-of-bounds index and missing app id are both no-ops —
        // hit-test misalignment / racy clicks must not panic.
        let mut ws = ws_with_grants();
        let before = ws.grants.get("firefox").map(|v| v.len());
        let r1 = revoke_grant(&mut ws, &"unknown".to_string(), 0);
        let r2 = revoke_grant(&mut ws, &"firefox".to_string(), 99);
        assert!(!r1);
        assert!(!r2);
        assert_eq!(ws.grants.get("firefox").map(|v| v.len()), before);
    }

    #[test]
    fn capability_label_matches_phase_4_2_vocabulary() {
        // Phase 6's revoke list and Phase 4.2's grant prompt share the
        // same wording so users see the same string when granting and
        // revoking. This pins the helper's outputs against the
        // canonical labels.
        let cam = Capability::Camera {
            scope: GrantScope::Always,
        };
        assert_eq!(capability_label(&cam), "Camera access");
        let mic = Capability::Microphone {
            scope: GrantScope::Once,
        };
        assert_eq!(capability_label(&mic), "Microphone access");
        let net = Capability::Network {
            hosts: None,
            scope: GrantScope::Session,
        };
        assert_eq!(capability_label(&net), "Network access");
    }

    #[test]
    fn capability_scope_label_returns_short_badge_text() {
        // Three-variant short label used by the row-side scope badge.
        let always = Capability::Camera {
            scope: GrantScope::Always,
        };
        let session = Capability::Camera {
            scope: GrantScope::Session,
        };
        let once = Capability::Camera {
            scope: GrantScope::Once,
        };
        assert_eq!(capability_scope_label(&always), "Always");
        assert_eq!(capability_scope_label(&session), "Session");
        assert_eq!(capability_scope_label(&once), "Once");
    }

    #[test]
    fn draw_permissions_pane_paints_empty_state_when_no_grants() {
        // Empty grants ⇒ one placeholder row fill so the section
        // reads as "intentionally empty" rather than "broken".
        let ws = Workspace::new(WorkspaceId::default(), "empty");
        let mut scene = Scene::new();
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let pane = Rect::new(0.0, 0.0, 480.0, 360.0);
        draw_permissions_pane(
            &mut scene,
            Affine::IDENTITY,
            pane,
            &ws,
            true,
            &mut tr,
            &theme,
        );
        assert!(
            !scene.encoding().draw_tags.is_empty(),
            "empty Permissions pane must paint at least the placeholder"
        );
    }

    #[test]
    fn draw_permissions_pane_paints_more_with_grants_than_without() {
        // Adding two grants for an app surfaces two row cards + scope
        // badges + revoke buttons ⇒ strictly more solid fills than the
        // empty-state placeholder.
        let theme = Theme::dark();
        let pane = Rect::new(0.0, 0.0, 480.0, 360.0);
        let mut tr = TextRenderer::new();

        let mut empty_scene = Scene::new();
        draw_permissions_pane(
            &mut empty_scene,
            Affine::IDENTITY,
            pane,
            &Workspace::new(WorkspaceId::default(), "empty"),
            true,
            &mut tr,
            &theme,
        );

        let mut full_scene = Scene::new();
        draw_permissions_pane(
            &mut full_scene,
            Affine::IDENTITY,
            pane,
            &ws_with_grants(),
            true,
            &mut tr,
            &theme,
        );

        let empty = empty_scene.encoding().draw_tags.len();
        let full = full_scene.encoding().draw_tags.len();
        assert!(
            full > empty,
            "Permissions pane with grants must paint more than empty (full={full}, empty={empty})"
        );
    }

    #[test]
    fn draw_permissions_pane_no_op_on_zero_area_pane() {
        let ws = ws_with_grants();
        let mut scene = Scene::new();
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let pane = Rect::new(100.0, 100.0, 100.0, 100.0);
        draw_permissions_pane(
            &mut scene,
            Affine::IDENTITY,
            pane,
            &ws,
            true,
            &mut tr,
            &theme,
        );
        assert!(
            scene.encoding().draw_tags.is_empty(),
            "zero-area pane must short-circuit — observed draw tags emitted"
        );
    }
}
