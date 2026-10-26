//! "Open with" and the Recent place, on the Files widget.
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
use ui::tilo_actions::{ActionParam, ParamType, Resource};
use ui::tilo_resource::Effect as _;
use ui::widget::command_palette::CommandEntry;
use ui::widget::context_menu::MenuItem;
use vello::kurbo::Point;

/// Typed action id: `app` (one of the candidates) and optional `always`.
pub(super) const OPEN_WITH_ACTION: &str = "open-with";
/// Per-application rows the app chooser lists for its target file.
const OPEN_ONCE_PREFIX: &str = "open-with-app:";
const ALWAYS_PREFIX: &str = "always-open-with:";
/// How many applications the submenu lists before "Other application…".
pub(super) const SUBMENU_APPS: usize = 6;

/// The cache key for a file's candidates: its MIME guess and extension, the
/// two things the desktop resolves a type by.
fn type_key(path: &Path) -> String {
    let mime = tilo_mime::from_path(path).unwrap_or_default().to_string();
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

    pub(super) fn native_selected_discovery(&self) -> bool {
        tilo_files::native::retained_home::contains(&self.current_path)
            && self.open_with_route().is_none()
    }
    pub(super) fn retained_selected_open_available(&self) -> bool {
        tilo_files::native::retained_home::contains(&self.current_path)
            && (self
                .open_with_route()
                .is_some_and(|route| route.retained_selected_open_available())
                || self.native_selected_discovery()
                    && self
                        .entries
                        .iter()
                        .filter(|entry| !entry.is_dir)
                        .any(|entry| self.selected_open.discovery.available(&entry.path)))
    }

    /// Whether "Open with" applies: one regular file, and a host to ask.
    pub(super) fn open_with_available(&self) -> bool {
        self.open_with_subject().is_some_and(|path| {
            self.open_with_route().is_some()
                || self.native_selected_discovery()
                    && !self.open_with_candidates(&path).apps.is_empty()
        })
    }

    /// Applications that open `path`, default first; empty with no route.
    pub(super) fn open_with_candidates(&self, path: &Path) -> OpenWithCandidates {
        if self.native_selected_discovery() && tilo_files::native::retained_home::contains(path) {
            return self.selected_open.discovery.candidates(path);
        }
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

    /// The single selected regular file an "Open with" choice applies to.
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
        if (self.native_selected_discovery()
            || self
                .open_with_route()
                .is_some_and(|route| route.retained_selected_open_available()))
            && tilo_files::native::retained_home::contains(path)
        {
            let Some(_) = self.retained_open_target_access(path, app_id) else {
                self.status = Some(t("files-open-with-launch-unavailable"));
                return true;
            };
            let Some(entry) = self
                .entries
                .iter()
                .find(|entry| entry.path == path && !entry.is_dir)
            else {
                self.status = Some(t("files-open-with-launch-unavailable"));
                return true;
            };
            let resource = Self::entry_resource_id(entry);
            if self.visible_entry_path(&resource).as_deref() != Some(path)
                || self.open_with_candidates(path).find(app_id).is_none()
                || always
            {
                self.status = Some(t("files-open-with-launch-unavailable"));
                return true;
            }
            // The host, not this command bus, mints the original addressed
            // invocation. Both pointer and keyboard submenu activation use this
            // same current published OpenWith action as the full chooser.
            if let Some(address) =
                ui::tilo_actions::ActionAddress::checked(&*resource, OPEN_WITH_ACTION)
            {
                self.selected_open.control = Some(ui::tilo_actions::ControlInvocation {
                    address,
                    arguments: serde_json::Map::from_iter([
                        ("app".into(), app_id.into()),
                        ("always".into(), false.into()),
                        ("access".into(), "read-only".into()),
                    ]),
                });
                return true;
            }
        }
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
            Err(reason) => self.notify_failure(
                t_args("files-open-failed", &[arg("name", name)]),
                &reason,
                None,
            ),
        }
        true
    }

    /// The "Open with" flyout: the default and other claiming apps, then
    /// "Other application…" for the full, searchable list.
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

    /// "Other application…": the shared searchable palette, listing every
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
        let (width, height) = self.app_chooser.desired_size();
        let gutter = f64::from(theme.tokens.spacing.s4);
        let width = width.min((self.rect.width() - 2.0 * gutter).max(0.0));
        let height = height.min((self.rect.height() - 2.0 * gutter).max(0.0));
        let x = self.rect.center().x - width / 2.0;
        let y = (self.rect.y0 + (self.rect.height() * 0.12).max(gutter))
            .min(self.rect.y1 - height - gutter)
            .max(self.rect.y0 + gutter);
        let card = vello::kurbo::Rect::new(x, y, x + width, y + height);
        ui::overlay::scrim::behind_panel(
            scene,
            transform,
            self.rect,
            vello::kurbo::RoundedRect::from_rect(card, ui::widget::floating_surface::radius(theme)),
            theme,
        );
        self.app_chooser.set_rect(card);
        self.app_chooser
            .draw(scene, transform, text_renderer, theme, presence);
    }

    /// While open, the chooser owns every event; Escape or a pick closes it.
    pub(super) fn route_app_chooser_event(
        &mut self,
        event: &winit::event::WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut ui::widget::Edits<'_>,
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
                state: state.reborrow(),
                path,
                handled: &mut handled,
                focus_target,
                node_id: node,
                taffy: &taffy,
            },
        );
        self.structure_dirty = true;
    }

    /// What the open-with actions of `entry`'s row are made from
    /// ([`append_open_with_actions`]); `None` when the row offers none: a
    /// folder, an archive's or the Trash's entry, a type no app claims.
    pub(super) fn open_with_facts(&self, entry: &Entry) -> Option<OpenWithFacts> {
        if entry.is_dir
            || crate::archive::location(&self.current_path).is_some()
            || self.is_trash_view()
        {
            return None;
        }
        let candidates = self.open_with_candidates(&entry.path);
        if candidates.apps.is_empty() {
            return None;
        }
        let access = self.retained_open_default_access(&entry.path);
        let can_edit = candidates.apps.iter().any(|app| {
            self.retained_open_target_access(&entry.path, &app.id)
                == Some(ui::collaboration::open_selected::v2::SelectedAccess::EditInParent)
        });
        let chooser =
            (self.app_chooser_target.as_deref() == Some(entry.path.as_path())).then(|| {
                candidates
                    .apps
                    .iter()
                    .map(|app| {
                        self.retained_open_target_access(&entry.path, &app.id)
                            .is_some()
                    })
                    .collect()
            });
        Some(OpenWithFacts {
            candidates,
            access,
            can_edit,
            chooser,
        })
    }
}

/// What a row's open-with actions are made from: the apps that claim the
/// file's type, the access opening it in place gives, and, while the app
/// chooser is open on the file, which apps open it in place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct OpenWithFacts {
    candidates: OpenWithCandidates,
    access: Option<ui::collaboration::open_selected::v2::SelectedAccess>,
    can_edit: bool,
    /// Per app of `candidates`, whether it opens the file in place.
    chooser: Option<Vec<bool>>,
}

/// Open-with actions for a file resource: the typed door (any claiming app,
/// optionally made the default) and, while the app chooser is open on this
/// file, one row action per app for the chooser to list.
pub(super) fn append_open_with_actions(
    facts: &OpenWithFacts,
    resource_id: &str,
    resource: &mut Resource,
) {
    let candidates = &facts.candidates;
    let access = facts.access;
    let can_edit = facts.can_edit;
    let permission = if can_edit {
        ui::tilo_actions::ActionKind::Undoable
    } else {
        match access {
            Some(ui::collaboration::open_selected::v2::SelectedAccess::EditInParent) => {
                ui::tilo_actions::ActionKind::Undoable
            }
            _ => ui::tilo_actions::ActionKind::Read,
        }
    };
    let mut params = vec![
        ActionParam::required_owned("app", ParamType::Text, t("files-open-with-app-param"))
            .with_choices(candidates.apps.iter().map(|app| app.id.clone())),
        ActionParam::optional_owned(
            "always",
            ParamType::Boolean,
            t_args(
                "files-open-with-always-param",
                &[arg("type", candidates.type_label.clone())],
            ),
        ),
    ];
    if let Some(access) = access {
        let choices = if can_edit {
            vec!["read-only", "edit-in-parent"]
        } else {
            vec!["read-only"]
        };
        params.push(
            ActionParam::optional_owned(
                "access",
                ParamType::Text,
                t("files-open-with-access-param"),
            )
            .with_choices(choices.clone())
            .with_schema(
                serde_json::json!({"type":"string", "enum":choices, "default":access.as_str()}),
            ),
        );
    }
    // This is an actual declared contract for the offered operation, not
    // permission reconstructed later from caller-provided text. The same
    // row is rebuilt from the live dispatcher on every publication.
    let mut open_with = crate::effects::FilesEffect::OpenWith {
        entry: resource_id.to_string(),
        app: String::new(),
        always: None,
        access: access.map(|access| access.as_str().to_owned()),
    }
    .bound_asking_params(params);
    open_with.type_mut().contract.kind = permission;
    open_with.type_mut().id = OPEN_WITH_ACTION.into();
    open_with.type_mut().label = t("files-menu-open-with");
    resource.actions.push(open_with);
    if let Some(chooser) = &facts.chooser {
        append_open_with_chooser_actions(candidates, chooser, resource_id, resource);
    }
}

fn append_open_with_chooser_actions(
    candidates: &OpenWithCandidates,
    in_place: &[bool],
    resource_id: &str,
    resource: &mut Resource,
) {
    for (app, in_place) in candidates.apps.iter().zip(in_place) {
        let access =
            in_place.then_some(ui::collaboration::open_selected::v2::SelectedAccess::ReadOnly);
        let permission =
            if access.is_some_and(ui::collaboration::open_selected::v2::SelectedAccess::editable) {
                ui::tilo_actions::ActionKind::Undoable
            } else {
                ui::tilo_actions::ActionKind::Read
            };
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
            if always && access.is_some() {
                continue;
            }
            let effect = crate::effects::FilesEffect::OpenWith {
                entry: resource_id.to_string(),
                app: app.id.clone(),
                always: Some(always),
                access: if always {
                    None
                } else {
                    access.map(|access| access.as_str().to_owned())
                },
            };
            let spec = if let Some(access) = access.filter(|_| !always) {
                let immediate = effect.bound().encode_immediate();
                let mut spec = effect.bound_asking_params(vec![
                    ActionParam::optional_owned(
                        "access",
                        ParamType::Text,
                        t("files-open-with-access-param"),
                    )
                    .with_choices([access.as_str()])
                    .with_schema(serde_json::json!({
                        "type":"string", "const":access.as_str(), "default":access.as_str(),
                    })),
                ]);
                spec.type_mut().contract.kind = permission;
                // The existing chooser remains one click. Its immutable
                // optional constant also describes the bound intent to the
                // original host custody, without a second permission UI.
                if let Some(immediate) = immediate {
                    spec = spec.with_immediate(std::sync::Arc::unwrap_or_clone(immediate));
                }
                spec.type_mut().id = format!("{prefix}{}", app.id);
                spec.type_mut().label = label;
                spec
            } else {
                crate::effects::row_action(&effect, format!("{prefix}{}", app.id), label)
            };
            resource.actions.push(spec);
        }
    }
}

impl FileExplorerWidget {
    fn retained_open_target_access(
        &self,
        path: &Path,
        app: &str,
    ) -> Option<ui::collaboration::open_selected::v2::SelectedAccess> {
        use ui::collaboration::open_selected::v2::SelectedAccess;
        let source = if self.native_selected_discovery() {
            self.selected_open.discovery.access(path, app)?
        } else {
            self.retained_open_default_access(path)?
        };
        let writes = self.dispatcher.as_ref().is_some_and(|dispatcher| {
            dispatcher.may(&ui::tilo_actions::ActionNeed::of(
                ui::tilo_actions::ActionKind::Undoable,
            ))
        });
        Some(if writes && source.editable() {
            SelectedAccess::EditInParent
        } else {
            SelectedAccess::ReadOnly
        })
    }
    fn retained_open_default_access(
        &self,
        path: &Path,
    ) -> Option<ui::collaboration::open_selected::v2::SelectedAccess> {
        use ui::collaboration::open_selected::v2::SelectedAccess;
        if !self.retained_selected_open_available()
            || !tilo_files::native::retained_home::contains(path)
        {
            return None;
        }
        Some(SelectedAccess::ReadOnly)
    }

    pub(super) fn invoke_open_with(
        &mut self,
        entry: &str,
        app: &str,
        always: bool,
    ) -> Result<(), String> {
        let path = self
            .visible_open_path(entry)
            .ok_or_else(|| "file location is no longer available".to_string())?;
        if path.is_dir() {
            return Err("folders open in Files".into());
        }
        if self.open_with_candidates(&path).find(app).is_none() {
            return Err(t_args(
                "files-open-with-not-a-candidate",
                &[arg("app", app)],
            ));
        }
        self.close_app_chooser();
        self.apply_open_with(&path, app, always);
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
        let at = ui::time::unix_now_ms();
        self.edit_recent(|recent| recent.note(path, last_use, at));
    }

    pub(super) fn recent_moved(&mut self, from: &Path, to: &Path) {
        self.edit_recent(|recent| recent.moved(from, to));
    }

    /// Change the Recent list: this device's setting, where every window
    /// sees it; a window no framework hosts keeps its own.
    fn edit_recent(&mut self, change: impl FnOnce(&mut crate::recent::RecentFiles)) {
        match &self.recent_setting {
            Some(setting) => {
                if let Err(error) = setting.update(change) {
                    tracing::warn!(%error, "could not save recent files");
                }
            }
            None => change(&mut self.recent),
        }
    }

    /// The Recent list as stored.
    fn recent_list(&self) -> crate::recent::RecentFiles {
        self.recent_setting
            .as_ref()
            .map_or_else(|| self.recent.clone(), ui::framework::Setting::get)
    }

    /// The Recent listing: every remembered path that still exists, in
    /// recency order.
    pub(super) fn recent_entries(&self) -> Vec<Entry> {
        self.recent_list()
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
                        tilo_mime::from_path_or_octet_stream(&item.path).to_string()
                    },
                    sync_state: None,
                    content_hash: None,
                    record_id: None,
                    availability: None,
                    share: None,
                    history: None,
                    rich_metadata: None,
                    observed: host_fs::ObservedObject::from_metadata(&meta),
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
            self.retain_shown_selection();
        }
        true
    }

    /// `FileCommand::ForgetRecent` — drop the item from Recent (the file is
    /// untouched).
    pub(super) fn apply_forget_recent(&mut self, path: &Path) -> bool {
        self.edit_recent(|recent| {
            recent.forget(path);
        });
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
