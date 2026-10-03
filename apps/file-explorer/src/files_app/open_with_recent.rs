//! "Open With" and the Recent place, on the Files widget.
//!
//! Both are thin: which applications open a type is the host's
//! [`OpenWithRoute`](crate::open_with::OpenWithRoute) answer, and the Recent
//! list is [`RecentFiles`](crate::recent::RecentFiles). This module wires them
//! into the context menu, the shared app-chooser palette, the resource
//! catalogue and the listing.

use super::{FileExplorerWidget, FileServices, layout_menu_widget};
use crate::command::FileCommand;
use crate::i18n::{arg, t, t_args};
use crate::model::Entry;
use crate::open_with::{OpenWithCandidates, SharedOpenWithRoute};
use crate::recent::RecentUse;
use std::path::{Path, PathBuf};
use ui::tilo_actions::{
    ActionContract, ActionParam, ActionSpec, EncodedAction, OperationKind, ParamType,
    PermissionRequirement, Resource, RetryPolicy,
};
use ui::widget::command_palette::CommandEntry;
use ui::widget::context_menu::MenuItem;
use vello::kurbo::Point;

/// Operation type for every open-with action on a file resource.
pub(super) const OPEN_WITH_OPERATION: &str = "files.open-with";
/// Typed action id: `app` (one of the candidates) and optional `always`.
pub(super) const OPEN_WITH_ACTION: &str = "open-with";
/// Per-application rows the app chooser lists for its target file.
const OPEN_ONCE_PREFIX: &str = "open-with-app:";
const ALWAYS_PREFIX: &str = "always-open-with:";
/// How many applications the submenu lists before "Other Application…".
pub(super) const SUBMENU_APPS: usize = 6;

/// The open-with payload: which published resource, which app, and whether
/// the choice becomes the person's default for the type.
#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct OpenWithPayload {
    pub resource_id: String,
    pub app_id: String,
    pub always: bool,
}

/// The cache key for a file's candidates: its MIME guess and extension, the
/// two things the desktop resolves a type by.
fn type_key(path: &Path) -> String {
    let mime = mime_guess::from_path(path)
        .first_raw()
        .unwrap_or_default()
        .to_string();
    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    format!("{mime}|{ext}")
}

impl FileExplorerWidget {
    fn open_with_route(&self) -> Option<SharedOpenWithRoute> {
        match &self.services {
            FileServices::Legacy => crate::open_with::installed_route(),
            FileServices::Hosted(hosted) => hosted.open_with.clone(),
        }
    }

    /// Whether "Open With" applies: one regular file, and a host to ask.
    pub(super) fn open_with_available(&self) -> bool {
        self.open_with_route().is_some() && self.open_with_subject().is_some()
    }

    /// Applications that open `path`, default first; empty with no route.
    pub(super) fn open_with_candidates(&self, path: &Path) -> OpenWithCandidates {
        let key = type_key(path);
        if let Some(cached) = self.open_with_cache.borrow().get(&key) {
            return cached.clone();
        }
        let Some(route) = self.open_with_route() else {
            return OpenWithCandidates::default();
        };
        let candidates = route.candidates(path);
        self.open_with_cache
            .borrow_mut()
            .insert(key, candidates.clone());
        candidates
    }

    /// Forget cached candidates: an install, uninstall or new default can
    /// change them, and every reload is a natural point to ask again.
    pub(super) fn invalidate_open_with(&self) {
        self.open_with_cache.borrow_mut().clear();
    }

    /// The single selected regular file an "Open With" choice applies to.
    pub(super) fn open_with_subject(&self) -> Option<PathBuf> {
        if crate::archive::location(&self.current_path).is_some() || self.is_trash_view() {
            return None;
        }
        let selected = self.selection.selected();
        if selected.len() != 1 {
            return None;
        }
        let displayed = self.displayed_entries();
        displayed
            .get(*selected.first()?)
            .filter(|entry| !entry.is_dir)
            .map(|entry| entry.path.clone())
    }

    /// `FileCommand::OpenWith` — open `path` with exactly `app_id`, making it
    /// the default for the type first when `always`.
    pub(super) fn apply_open_with(&mut self, path: &Path, app_id: &str, always: bool) -> bool {
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        let Some(route) = self.open_with_route() else {
            self.status = Some(t_args("files-open-with-unavailable", &[arg("name", name)]));
            return true;
        };
        if always {
            if let Err(reason) = route.set_default(path, app_id) {
                self.status = Some(reason);
                return true;
            }
            self.invalidate_open_with();
        }
        match route.open_with(path, app_id) {
            Ok(()) => {
                self.note_recent(path, RecentUse::Opened);
                if always {
                    let app = self
                        .open_with_candidates(path)
                        .find(app_id)
                        .map_or_else(|| app_id.to_string(), |app| app.name.clone());
                    self.status = Some(t_args(
                        "files-open-with-now-default",
                        &[
                            arg("app", app),
                            arg("type", self.open_with_candidates(path).type_label),
                        ],
                    ));
                }
            }
            Err(reason) => self.status = Some(reason),
        }
        true
    }

    /// The "Open With" flyout: the default and other claiming apps, then
    /// "Other Application…" for the full, searchable list.
    pub(super) fn open_open_with_submenu(&mut self, at: Point) {
        let Some(path) = self.open_with_subject() else {
            self.submenu.close();
            self.submenu_commands.clear();
            return;
        };
        let candidates = self.open_with_candidates(&path);
        let mut menu = self.new_overlay_menu();
        let mut commands = Vec::new();
        if candidates.apps.is_empty() {
            menu.push(MenuItem::action(t("files-open-with-none"), || {}).with_enabled(false));
            commands.push(None);
        }
        for app in candidates.apps.iter().take(SUBMENU_APPS) {
            let label = if app.is_default {
                t_args("files-open-with-default", &[arg("app", app.name.clone())])
            } else {
                app.name.clone()
            };
            menu.push(MenuItem::action(label, || {}));
            commands.push(Some(FileCommand::OpenWith {
                path: path.clone(),
                app_id: app.id.clone(),
                always: false,
            }));
        }
        if !candidates.apps.is_empty() {
            menu.push(MenuItem::separator());
            commands.push(None);
            menu.push(MenuItem::action(t("files-open-with-other"), || {}));
            commands.push(Some(FileCommand::OpenWithOther));
        }
        menu.open_at([at.x as f32, at.y as f32]);
        self.submenu = menu;
        self.submenu_commands = commands;
        layout_menu_widget(&mut self.submenu);
    }

    /// "Other Application…": the shared searchable palette, listing every
    /// application that claims the type twice — to open once, and to make it
    /// the default. Its rows are this file's published open-with actions, so
    /// choosing one goes through the same action door automation uses.
    pub(super) fn open_app_chooser(&mut self) {
        let Some(path) = self.open_with_subject() else {
            return;
        };
        let candidates = self.open_with_candidates(&path);
        if candidates.apps.is_empty() {
            self.status = Some(t("files-open-with-none"));
            return;
        }
        self.app_chooser_target = Some(path.clone());
        let Some(entry) = self
            .entries
            .iter()
            .find(|entry| entry.path == path)
            .cloned()
        else {
            return;
        };
        let resource = self.entry_resource(&entry);
        let once_group = t("files-open-with-once-group");
        let always_group = t_args(
            "files-open-with-always-group",
            &[arg("type", candidates.type_label.clone())],
        );
        let flat = ui::tilo_actions::flatten(std::slice::from_ref(&resource));
        let mut once = Vec::new();
        let mut always = Vec::new();
        for action in &flat {
            let id = action.action.id.as_str();
            let (bucket, group, app_id) = if let Some(app_id) = id.strip_prefix(OPEN_ONCE_PREFIX) {
                (&mut once, &once_group, app_id)
            } else if let Some(app_id) = id.strip_prefix(ALWAYS_PREFIX) {
                (&mut always, &always_group, app_id)
            } else {
                continue;
            };
            let Some(app) = candidates.find(app_id) else {
                continue;
            };
            let mut row = CommandEntry::from_flat_action(action).group(group.clone());
            row.label.clone_from(&app.name);
            if app.is_default {
                row = row.badge(t("files-open-with-default-badge"));
            }
            bucket.push(row);
        }
        once.extend(always);
        self.app_chooser.set_entries(once);
        self.app_chooser
            .set_query_placeholder(t_args("files-open-with-search", &[arg("name", entry.name)]));
        self.app_chooser.open();
        self.structure_dirty = true;
    }

    pub(super) fn app_chooser_open(&self) -> bool {
        self.app_chooser_target.is_some() && self.app_chooser.is_open()
    }

    pub(super) fn close_app_chooser(&mut self) {
        self.app_chooser.close();
        self.app_chooser_target = None;
    }

    /// Paint the chooser over a scrim, centred near the top of Files.
    pub(super) fn draw_app_chooser(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &ui::Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        use ui::widget::Widget;
        if !self.app_chooser_open() {
            return;
        }
        ui::chrome::fill_scrim(
            scene,
            transform,
            self.rect,
            ui::widget::modal_frame::MODAL_SCRIM_ALPHA,
            theme,
        );
        let (width, height) = self.app_chooser.desired_size();
        let gutter = f64::from(theme.tokens.spacing.s4);
        let width = width.min((self.rect.width() - 2.0 * gutter).max(0.0));
        let height = height.min((self.rect.height() - 2.0 * gutter).max(0.0));
        let x = self.rect.center().x - width / 2.0;
        let y = self.rect.y0 + (self.rect.height() * 0.12).max(gutter);
        self.app_chooser
            .set_rect(vello::kurbo::Rect::new(x, y, x + width, y + height));
        self.app_chooser
            .draw(scene, transform, text_renderer, theme, presence);
    }

    /// While open, the chooser owns every event; Escape or a pick closes it.
    pub(super) fn route_app_chooser_event(
        &mut self,
        event: &winit::event::WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut automerge::AutoCommit,
        path: &automerge::ObjId,
        focus_target: &mut Option<taffy::NodeId>,
    ) {
        use ui::widget::Widget;
        let mut taffy: taffy::TaffyTree<()> = taffy::TaffyTree::new();
        let Ok(node) = taffy.new_leaf(taffy::Style::default()) else {
            return;
        };
        let mut handled = false;
        self.app_chooser.handle_event(
            event,
            &mut ui::widget::EventCtx {
                scale_factor,
                modifiers: &modifiers,
                state,
                path,
                handled: &mut handled,
                focus_target,
                node_id: node,
                taffy: &taffy,
            },
        );
        self.structure_dirty = true;
    }

    /// Open-with actions for a file resource: the typed door (any claiming
    /// app, optionally made the default) and, while the app chooser is open
    /// on this file, one row action per app for the chooser to list.
    #[allow(
        clippy::expect_used,
        reason = "the payload is two owned strings and a bool; JSON encoding cannot fail"
    )]
    pub(super) fn append_open_with_actions(
        &self,
        entry: &Entry,
        resource_id: &str,
        resource: &mut Resource,
    ) {
        if entry.is_dir
            || crate::archive::location(&self.current_path).is_some()
            || self.is_trash_view()
        {
            return;
        }
        let candidates = self.open_with_candidates(&entry.path);
        if candidates.apps.is_empty() {
            return;
        }
        let typed_resource = resource_id.to_string();
        resource.actions.push(
            ActionSpec::typed_operation(
                OPEN_WITH_ACTION,
                t("files-menu-open-with"),
                t("files-open-with-app-param"),
                OperationKind::Effect,
                OPEN_WITH_OPERATION,
                vec![
                    ActionParam::required_owned(
                        "app",
                        ParamType::Text,
                        t("files-open-with-app-param"),
                    )
                    .with_choices(candidates.apps.iter().map(|app| app.id.clone())),
                    ActionParam::optional_owned(
                        "always",
                        ParamType::Boolean,
                        t_args(
                            "files-open-with-always-param",
                            &[arg("type", candidates.type_label.clone())],
                        ),
                    ),
                ],
                move |args| {
                    EncodedAction::encode_operation(
                        OPEN_WITH_OPERATION,
                        &OpenWithPayload {
                            resource_id: typed_resource.clone(),
                            app_id: args.text("app").unwrap_or_default().to_string(),
                            always: args.boolean("always").unwrap_or(false),
                        },
                    )
                    .expect("open-with payload is serializable")
                },
            )
            .with_contract(open_with_contract()),
        );
        if self.app_chooser_target.as_deref() != Some(entry.path.as_path()) {
            return;
        }
        for app in &candidates.apps {
            for (prefix, always, label) in [
                (
                    OPEN_ONCE_PREFIX,
                    false,
                    t_args("files-open-with-app", &[arg("app", app.name.clone())]),
                ),
                (
                    ALWAYS_PREFIX,
                    true,
                    t_args(
                        "files-open-with-always-app",
                        &[
                            arg("app", app.name.clone()),
                            arg("type", candidates.type_label.clone()),
                        ],
                    ),
                ),
            ] {
                if let Ok(spec) = ActionSpec::operation(
                    format!("{prefix}{}", app.id),
                    label,
                    OperationKind::Effect,
                    OPEN_WITH_OPERATION,
                    &OpenWithPayload {
                        resource_id: resource_id.to_string(),
                        app_id: app.id.clone(),
                        always,
                    },
                ) {
                    resource
                        .actions
                        .push(spec.with_contract(open_with_contract()));
                }
            }
        }
    }

    /// Execute a `files.open-with` operation against the live listing.
    pub(super) fn invoke_open_with(&mut self, payload: &[u8]) -> Result<(), String> {
        let request: OpenWithPayload =
            serde_json::from_slice(payload).map_err(|error| error.to_string())?;
        let path = self
            .visible_open_path(&request.resource_id)
            .ok_or_else(|| "file location is no longer available".to_string())?;
        if path.is_dir() {
            return Err("folders open in Files".into());
        }
        if self
            .open_with_candidates(&path)
            .find(&request.app_id)
            .is_none()
        {
            return Err(t_args(
                "files-open-with-not-a-candidate",
                &[arg("app", request.app_id)],
            ));
        }
        self.close_app_chooser();
        self.apply_open_with(&path, &request.app_id, request.always);
        Ok(())
    }

    // --- Recent ----------------------------------------------------------

    pub(super) fn is_recent_view(&self) -> bool {
        crate::recent::is_location(&self.current_path)
    }

    /// Record a use of `path` and persist the list (best-effort).
    pub(super) fn note_recent(&mut self, path: &Path, last_use: RecentUse) {
        if crate::archive::location(path).is_some() {
            return;
        }
        self.recent.note(path, last_use, crate::recent::now_ms());
        self.save_recent();
    }

    pub(super) fn recent_moved(&mut self, from: &Path, to: &Path) {
        self.recent.moved(from, to);
        self.save_recent();
    }

    fn save_recent(&self) {
        // Unit tests built with `new()` point at the developer's real config
        // directory; they must never rewrite that person's Recent list.
        #[cfg(test)]
        if self.config_dir == crate::config::config_dir() {
            return;
        }
        if let Some(dir) = self.config_dir.as_deref()
            && let Err(error) = self.recent.save(dir)
        {
            tracing::warn!(%error, "could not save recent files");
        }
    }

    /// The Recent listing: every remembered path that still exists, in
    /// recency order.
    pub(super) fn recent_entries(&self) -> Vec<Entry> {
        self.recent
            .items()
            .iter()
            .filter_map(|item| {
                let meta = std::fs::metadata(&item.path).ok()?;
                let is_dir = meta.is_dir();
                Some(Entry {
                    name: item.path.file_name().map_or_else(
                        || item.path.display().to_string(),
                        |name| name.to_string_lossy().into_owned(),
                    ),
                    path: item.path.clone(),
                    is_dir,
                    size: meta.len(),
                    modified: meta.modified().ok().map(chrono::DateTime::from),
                    mime: if is_dir {
                        "inode/directory".into()
                    } else {
                        mime_guess::from_path(&item.path)
                            .first_or_octet_stream()
                            .to_string()
                    },
                    sync_state: None,
                    content_hash: None,
                    record_id: None,
                    availability: None,
                    share: None,
                    history: None,
                    rich_metadata: None,
                })
            })
            .collect()
    }

    /// `FileCommand::RevealInFolder` — go to the item's folder with it selected.
    pub(super) fn apply_reveal_in_folder(&mut self, path: &Path) -> bool {
        let Some(parent) = path.parent() else {
            return false;
        };
        self.navigate(parent.to_path_buf());
        if let Some(index) = self
            .displayed_entries()
            .iter()
            .position(|entry| entry.path == path)
        {
            self.selection.select_only(index);
        }
        true
    }

    /// `FileCommand::ForgetRecent` — drop the item from Recent (the file is
    /// untouched).
    pub(super) fn apply_forget_recent(&mut self, path: &Path) -> bool {
        if self.recent.forget(path) {
            self.save_recent();
        }
        if self.is_recent_view() {
            self.refresh();
        }
        true
    }

    /// Context menu for the Recent place: open, open with, find the file,
    /// and forget it — never an edit, since the rows are from many folders.
    pub(super) fn recent_menu_spec(&self) -> Vec<(String, Option<FileCommand>)> {
        let mut spec = Vec::new();
        let Some(index) = self.selection.primary() else {
            return spec;
        };
        let Some(entry) = self.displayed_entries().get(index).cloned() else {
            return spec;
        };
        spec.push((t("files-menu-open"), Some(FileCommand::Activate(index))));
        if self.open_with_available() {
            spec.push((t("files-menu-open-with"), Some(FileCommand::OpenWithMenu)));
        }
        spec.push((
            t("files-recent-reveal"),
            Some(FileCommand::RevealInFolder(entry.path.clone())),
        ));
        spec.push((String::new(), None));
        spec.push((t("files-menu-copy"), Some(FileCommand::Copy)));
        spec.push((
            t("files-recent-forget"),
            Some(FileCommand::ForgetRecent(entry.path)),
        ));
        spec
    }
}

fn open_with_contract() -> ActionContract {
    ActionContract {
        permission: PermissionRequirement::Read,
        retry: RetryPolicy::AtMostOnce,
        ..ActionContract::default()
    }
}
