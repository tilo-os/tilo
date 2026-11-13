//! "Open with" and the Recent place on the Files window.

use super::{FilesState, MenuChange, layout_menu_widget};
use crate::command::FileCommand;
use crate::i18n::{arg, t, t_args};
use crate::model::Entry;
use crate::recent::{RecentRecord, RecentUse};
use runtime_portal::tools::DocumentApp;
use std::path::{Path, PathBuf};
use ui::tilo_actions::{ActionParam, ParamType, ResourceSpec};
use ui::tilo_resource::{AbsolutePath, Effect as _, Invoke};
use ui::widget::context_menu::MenuItem;
use vello::kurbo::Point;

pub(super) const OPEN_WITH_ACTION: &str = "open-with";
const OPEN_ONCE_PREFIX: &str = "open-with-app:";
pub(super) const SUBMENU_APPS: usize = 6;

fn type_key(path: &Path) -> String {
    let mime = tilo_mime::from_path(path).unwrap_or_default().to_string();
    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    format!("{mime}|{ext}")
}

impl FilesState {
    pub(super) fn open_with_candidates(&self, path: &Path) -> Vec<DocumentApp> {
        let key = type_key(path);
        if let Some(cached) = self.open_with_cache.borrow().get(&key) {
            return cached.clone();
        }
        let apps = match (self.document_candidates)(path) {
            Ok(apps) => apps,
            Err(reason) => {
                tracing::debug!(%reason, "the session did not list the apps that open this file");
                Vec::new()
            }
        };
        self.open_with_cache.borrow_mut().insert(key, apps.clone());
        apps
    }

    pub(super) fn invalidate_open_with(&self) {
        self.open_with_cache.borrow_mut().clear();
    }

    pub(super) fn open_with_available(&self) -> bool {
        self.open_with_subject().is_some()
    }

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

    pub(super) fn apply_open_with(&mut self, path: &Path, app_id: &str) -> bool {
        self.ask_open(path, Some(app_id.to_owned()));
        true
    }

    pub(super) fn open_open_with_submenu(&mut self, at: Point) {
        let Some(path) = self.open_with_subject() else {
            self.menus.push(MenuChange::CloseSub);
            return;
        };
        let candidates = self.open_with_candidates(&path);
        let mut menu = self.new_overlay_menu();
        let mut commands = Vec::new();
        if candidates.is_empty() {
            menu.push(MenuItem::action(t("files-open-with-none"), || {}).with_enabled(false));
            commands.push(None);
        }
        for app in candidates.iter().take(SUBMENU_APPS) {
            let label = if app.is_default {
                t_args("files-open-with-default", &[arg("app", app.name.clone())])
            } else {
                app.name.clone()
            };
            menu.push(MenuItem::action(label, || {}));
            commands.push(Some(FileCommand::OpenWith {
                path: path.clone(),
                app_id: app.id.clone(),
            }));
        }
        if !candidates.is_empty() {
            menu.push(MenuItem::separator());
            commands.push(None);
            menu.push(MenuItem::action(t("files-open-with-other"), || {}));
            commands.push(Some(FileCommand::OpenWithOther));
        }
        menu.open_at([at.x as f32, at.y as f32]);
        layout_menu_widget(&mut menu);
        self.menus.push(MenuChange::Sub(menu, commands));
    }

    pub(super) fn open_app_chooser(&mut self) {
        let Some(path) = self.open_with_subject() else {
            return;
        };
        if self.open_with_candidates(&path).is_empty() {
            self.status = Some(t("files-open-with-none"));
            return;
        }
        let Some(entry) = self
            .entries
            .iter()
            .find(|entry| entry.path == path)
            .cloned()
        else {
            return;
        };
        self.app_chooser_target = Some(path);
        let resource = self.entry_resource(&entry);
        self.app_chooser.open(ui::search::SearchRequest::within(
            ui::search::ScopeDecl::new(resource.id.0, entry.name),
        ));
        self.structure_dirty = true;
    }

    pub(super) fn close_app_chooser(&mut self) {
        self.app_chooser_target = None;
    }

    pub(super) fn open_with_facts(&self, entry: &Entry) -> Option<OpenWithFacts> {
        if entry.is_dir
            || crate::archive::location(&self.current_path).is_some()
            || self.is_trash_view()
        {
            return None;
        }
        let candidates = self.open_with_candidates(&entry.path);
        if candidates.is_empty() {
            return None;
        }
        Some(OpenWithFacts {
            candidates,
            chooser: self.app_chooser_target.as_deref() == Some(entry.path.as_path()),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct OpenWithFacts {
    candidates: Vec<DocumentApp>,
    chooser: bool,
}

pub(super) fn append_open_with_actions(
    facts: &OpenWithFacts,
    resource_id: &str,
    resource: &mut ResourceSpec,
) {
    let params = vec![
        ActionParam::required_owned("app", ParamType::Text, t("files-open-with-app-param"))
            .with_choices(facts.candidates.iter().map(|app| app.id.clone())),
    ];
    // This is an actual declared contract for the offered operation, not
    // permission reconstructed later from caller-provided text. The same row
    // is rebuilt from the live dispatcher on every publication.
    let mut open_with = crate::effects::FilesEffect::OpenWith {
        entry: resource_id.to_string(),
        app: String::new(),
    }
    .bound_asking_params(params);
    open_with.type_mut().id = OPEN_WITH_ACTION.into();
    open_with.type_mut().label = t("files-menu-open-with");
    resource.actions.push(open_with);
    if facts.chooser {
        for app in &facts.candidates {
            let name = if app.is_default {
                t_args("files-open-with-default", &[arg("app", app.name.clone())])
            } else {
                app.name.clone()
            };
            let effect = crate::effects::FilesEffect::OpenWith {
                entry: resource_id.to_string(),
                app: app.id.clone(),
            };
            resource.actions.push(crate::effects::row_action(
                &effect,
                format!("{OPEN_ONCE_PREFIX}{}", app.id),
                t_args("files-open-with-app", &[arg("app", name)]),
            ));
        }
    }
}

impl FilesState {
    pub(super) fn invoke_open_with(&mut self, entry: &str, app: &str) -> Result<(), String> {
        let path = self
            .visible_open_path(entry)
            .ok_or_else(|| "file location is no longer available".to_string())?;
        if path.is_dir() {
            return Err("folders open in Files".into());
        }
        if !self
            .open_with_candidates(&path)
            .iter()
            .any(|candidate| candidate.id == app)
        {
            return Err(t_args(
                "files-open-with-not-a-candidate",
                &[arg("app", app)],
            ));
        }
        self.close_app_chooser();
        self.apply_open_with(&path, app);
        Ok(())
    }

    pub(super) fn is_recent_view(&self) -> bool {
        crate::recent::is_location(&self.current_path)
    }

    pub(super) fn note_recent(&mut self, path: &Path, last_use: RecentUse) {
        if crate::archive::location(path).is_some() {
            return;
        }
        let at = ui::time::unix_now_ms();
        let shown = AbsolutePath::new(path).ok();
        self.edit_recent(
            |device| shown.map(|path| RecentRecord::NOTE.with((device, path, last_use, at))),
            |recent| recent.note(path, last_use, at),
        );
    }

    pub(super) fn recent_moved(&mut self, from: &Path, to: &Path) {
        let shown = AbsolutePath::new(from).ok().zip(AbsolutePath::new(to).ok());
        self.edit_recent(
            |device| shown.map(|(from, to)| RecentRecord::MOVED.with((device, from, to))),
            |recent| recent.moved(from, to),
        );
    }

    fn recent_record(&self) -> Option<(&ui::framework::PersonRecord<RecentRecord>, &str)> {
        self.recent_record.as_ref().zip(self.device.as_deref())
    }

    fn edit_recent(
        &mut self,
        edit: impl FnOnce(String) -> Option<Invoke<RecentRecord>>,
        change: impl FnOnce(&mut crate::recent::RecentFiles),
    ) {
        match self.recent_record() {
            Some((record, device)) => {
                if let Some(edit) = edit(device.to_owned())
                    && let Err(error) = record.invoke(&edit)
                {
                    tracing::warn!(%error, "could not save recent files");
                }
            }
            None => change(&mut self.recent),
        }
    }

    pub(super) fn recent_list(&self) -> crate::recent::RecentFiles {
        self.recent_record().map_or_else(
            || self.recent.clone(),
            |(record, device)| record.with(|recent| recent.on(device)),
        )
    }

    #[cfg(test)]
    pub(super) fn recent_entries(&self) -> Vec<Entry> {
        let paths: Vec<PathBuf> = self
            .recent_list()
            .items()
            .iter()
            .map(|item| item.path.clone())
            .collect();
        recent_entries_of(&paths)
    }

    pub(super) fn apply_reveal_in_folder(&mut self, path: &Path) -> bool {
        let Some(parent) = path.parent() else {
            return false;
        };
        let parent = parent.to_path_buf();
        if parent != self.current_path {
            self.history.push(self.current_path.clone());
            self.forward.clear();
        }
        self.load_then(parent, super::Then::Select(path.to_path_buf()));
        true
    }

    pub(super) fn apply_forget_recent(&mut self, path: &Path) -> bool {
        let shown = AbsolutePath::new(path).ok();
        self.edit_recent(
            |device| shown.map(|path| RecentRecord::FORGET.with((device, path))),
            |recent| {
                recent.forget(path);
            },
        );
        if self.is_recent_view() {
            self.refresh();
        }
        true
    }

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

pub(super) fn recent_entries_of(paths: &[PathBuf]) -> Vec<Entry> {
    paths
        .iter()
        .filter_map(|path| {
            let meta = std::fs::metadata(path).ok()?;
            let is_dir = meta.is_dir();
            Some(Entry {
                name: path.file_name().map_or_else(
                    || path.display().to_string(),
                    |name| name.to_string_lossy().into_owned(),
                ),
                path: path.clone(),
                is_dir,
                size: meta.len(),
                modified: meta.modified().ok().map(chrono::DateTime::from),
                mime: if is_dir {
                    "inode/directory".into()
                } else {
                    tilo_mime::from_path_or_octet_stream(path).to_string()
                },
                sync_state: None,
                content_hash: None,
                record_id: None,
                availability: None,
                history: None,
                app: None,
                rich_metadata: None,
                observed: host_fs::ObservedObject::from_metadata(&meta),
            })
        })
        .collect()
}
