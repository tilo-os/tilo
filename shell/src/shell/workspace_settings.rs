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

use quick_ui::icons::{Icon, LUCIDE_DEFAULT_STROKE_PX, draw_icon};
use quick_ui::overlay::{FocusTrap, Modal};
use quick_ui::text_render::TextRenderer;
use quick_ui::theme::Theme;
use quick_ui::widget::data_table::{ColumnDef, ColumnWidth, DataTable};

use crate::shell::state::{AppPerm, FilePerm, Workspace, WorkspaceId};

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
}

impl WsTab {
    /// Display label for the sidebar nav row + tab pill. Concept ref
    /// `WorkspaceSettingsModal.tsx:80` (`Roles & Permissions`).
    pub fn label(&self) -> &'static str {
        match self {
            WsTab::General => "General",
            WsTab::Roles => "Roles & Permissions",
            WsTab::Files => "Files",
        }
    }

    /// Iteration order for the sidebar nav (top-to-bottom).
    pub fn all() -> [WsTab; 3] {
        [WsTab::General, WsTab::Roles, WsTab::Files]
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
    pub fn nav_rows(&self, modal_rect: Rect) -> [(WsTab, Rect); 3] {
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
        assert_eq!(WsTab::all(), [WsTab::General, WsTab::Roles, WsTab::Files]);
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
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].0, WsTab::General);
        assert_eq!(rows[1].0, WsTab::Roles);
        assert_eq!(rows[2].0, WsTab::Files);
        // Stacked top-to-bottom, each 40 px tall with a 4 px gap.
        for (_, r) in &rows {
            assert!((r.height() - NAV_ROW_HEIGHT).abs() < f64::EPSILON);
            assert!(r.x1 > r.x0);
        }
        let stride = NAV_ROW_HEIGHT + NAV_ROW_GAP;
        assert!((rows[1].1.y0 - rows[0].1.y0 - stride).abs() < f64::EPSILON);
        assert!((rows[2].1.y0 - rows[1].1.y0 - stride).abs() < f64::EPSILON);
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
