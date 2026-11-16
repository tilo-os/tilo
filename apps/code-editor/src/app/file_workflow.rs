use super::{
    AutoCommit, CodeApp, CodeEditorEffect, Conflict, ConflictChoice, EditorDoc, Project, Session,
    TreeId, disk, source_filter,
};
use ui::framework::{EffectAnswer, EffectCx, Effected};
use ui::widget::toast::ToastKind;

pub(crate) enum AfterSave {
    Nothing,
    Close(Option<TreeId>),
    Effect,
}

impl AfterSave {
    fn landed(self, app: &mut CodeApp, node: TreeId) {
        match self {
            Self::Nothing => {}
            Self::Close(then_open) => app.close_node(node, then_open),
            Self::Effect => app.finish_file_effect(Ok(())),
        }
    }

    fn failed(self, app: &mut CodeApp) {
        if matches!(self, Self::Effect) {
            app.finish_file_effect(Err("file could not be saved".into()));
        }
    }
}

impl CodeApp {
    /// Read a disk-backed file into its buffer the first time this session opens it. Text the document
    /// already holds (a collaborator's, a previous session's unsaved edits) is kept, and the file's bytes
    /// and stamp become the comparison a save is checked against.
    ///
    /// A read failure closes the tab again: a buffer that opened empty would be written back empty by the
    /// next Ctrl+S.
    pub(super) fn seed_buffer(&mut self, node: TreeId) {
        let Some(buffer) = self.session.buffer_of(node) else {
            return;
        };
        let Some(path) = self.session.project.disk_path_of(node) else {
            return;
        };
        if buffer.stamp.is_some() || !self.disk_work.seeding.insert(node) {
            return;
        }
        let name = buffer.name.clone();
        self.on_disk(
            "code-read-file",
            move || {
                let loaded = crate::disk::load(&path);
                (path, loaded)
            },
            move |app, (path, loaded)| app.seeded(node, &name, path, loaded),
        );
    }

    pub(super) fn read_folder(
        &mut self,
        id: TreeId,
        listing: std::io::Result<crate::model::Listing>,
    ) {
        match listing {
            Ok(listing) => self.session.project.adopt_listing(id, listing),
            Err(e) => {
                let name = self
                    .session
                    .project
                    .get(id)
                    .map(|n| n.name.clone())
                    .unwrap_or_default();
                Self::note(format!("Could not read {name}: {e}"), ToastKind::Error);
            }
        }
        self.invalidate_search_projection();
        self.explorer.show(&self.session.project);
    }

    fn seeded(
        &mut self,
        node: TreeId,
        name: &str,
        path: std::path::PathBuf,
        loaded: Result<crate::disk::Loaded, crate::disk::LoadError>,
    ) {
        self.disk_work.seeding.remove(&node);
        match loaded {
            Ok(loaded) => {
                if let Some(buffer) = self.session.buffer_of_mut(node) {
                    buffer.path = Some(path);
                    buffer.mark_saved(loaded.text, Some(loaded.stamp));
                    self.bound_node = None;
                }
            }
            Err(error) => {
                if self.session.buffer_of(node).is_some() {
                    self.session.close_file(node);
                    self.invalidate_search_projection();
                }
                Self::note(format!("Could not open {name}: {error}"), ToastKind::Error);
                self.bound_node = None;
            }
        }
    }

    /// Write one buffer to its file, and say whether it landed: on the frame loop the write runs on a
    /// background thread and its outcome is folded in later (`Self::save_buffer_then`).
    ///
    /// `force` is the person having answered "overwrite anyway"; it is the only way past the freshness
    /// check, and the app never sets it itself.
    pub fn save_buffer(&mut self, doc: &AutoCommit, node: TreeId, force: bool) -> bool {
        self.save_buffer_then(doc, node, force, AfterSave::Nothing)
    }

    pub(super) fn save_buffer_then(
        &mut self,
        doc: &AutoCommit,
        node: TreeId,
        force: bool,
        then: AfterSave,
    ) -> bool {
        if self.session.buffer_of(node).is_none() {
            then.failed(self);
            return false;
        }
        let Some(path) = self.session.buffer_of(node).and_then(|b| b.path.clone()) else {
            // No file yet: a scratch buffer needs a destination first, which
            // only the user (through the picker) can choose.
            self.ask_where_to_save(node);
            return false;
        };
        let text = EditorDoc::read_buffer_key(doc, &self.buffer_key(node)).unwrap_or_default();
        let expected = if force {
            None
        } else {
            self.session.buffer_of(node).and_then(|b| b.stamp.clone())
        };
        let landed = std::rc::Rc::new(std::cell::Cell::new(false));
        let noted = std::rc::Rc::clone(&landed);
        self.on_disk(
            "code-save-file",
            move || {
                let saved = disk::save(&path, &text, expected.as_ref());
                (text, saved)
            },
            move |app, (text, saved)| noted.set(app.saved(node, text, saved, then)),
        );
        landed.get()
    }

    fn saved(
        &mut self,
        node: TreeId,
        text: String,
        saved: Result<disk::Stamp, disk::SaveError>,
        then: AfterSave,
    ) -> bool {
        match saved {
            Ok(stamp) => {
                if let Some(buf) = self.session.buffer_of_mut(node) {
                    buf.mark_saved(text, Some(stamp));
                }
                // Tell the language server the file is on disk now; rustc-based
                // diagnostics only refresh on save.
                if let (Some(c), Some(uri)) = (self.lsp.as_mut(), self.lsp_uri.clone()) {
                    c.did_save(&uri);
                }
                then.landed(self, node);
                true
            }
            Err(disk::SaveError::Conflict) => {
                if let Some(buf) = self.session.buffer_of_mut(node) {
                    buf.stale = true;
                }
                if self.conflict.is_none() {
                    self.conflict = Some(Conflict {
                        node,
                        name: self
                            .session
                            .buffer_of(node)
                            .map_or_else(String::new, |b| b.name.clone()),
                    });
                }
                then.failed(self);
                false
            }
            Err(e) => {
                let name = self
                    .session
                    .buffer_of(node)
                    .map_or_else(String::new, |b| b.name.clone());
                Self::note(format!("Could not save {name}: {e}"), ToastKind::Error);
                then.failed(self);
                false
            }
        }
    }

    pub(super) fn save_all(&mut self, doc: &AutoCommit) -> usize {
        let dirty: Vec<TreeId> = self
            .session
            .buffer
            .iter()
            .filter(|b| b.dirty && b.path.is_some())
            .map(|b| b.node)
            .collect();
        let mut written = 0;
        for node in dirty {
            if self.conflict.is_some() {
                break;
            }
            if self.save_buffer(doc, node, false) {
                written += 1;
            }
        }
        written
    }

    pub(super) fn reload_buffer(&mut self, doc: &mut ui::widget::Edits<'_>, node: TreeId) {
        if self.session.buffer_of(node).is_none() {
            return;
        }
        if let Some(path) = self.session.project.disk_path_of(node) {
            self.on_disk_editing(
                "code-reload-file",
                doc,
                move || disk::load(&path).map(|loaded| (loaded.text, Some(loaded.stamp))),
                move |app, doc, seed| app.reloaded(doc, node, seed),
            );
        } else {
            let seed = self.session.project.file_seed(node);
            self.reloaded(doc, node, seed);
        }
    }

    fn reloaded(
        &mut self,
        doc: &mut ui::widget::Edits<'_>,
        node: TreeId,
        seed: Result<(String, Option<disk::Stamp>), disk::LoadError>,
    ) {
        if self.session.buffer_of(node).is_none() {
            return;
        }
        match seed {
            Ok((text, stamp)) => {
                let name = self
                    .session
                    .buffer_of(node)
                    .map_or_else(String::new, |b| b.name.clone());
                let file_id = self.buffer_key(node);
                let base = EditorDoc::read_buffer_key(doc, &file_id).unwrap_or_default();
                if let Err(error) = self.dispatch_text_action(doc, file_id, &base, &text) {
                    self.editor.reconcile_from_doc(doc);
                    Self::note(format!("Reload was refused: {error}"), ToastKind::Error);
                    return;
                }
                if let Some(buf) = self.session.buffer_of_mut(node) {
                    buf.mark_saved(text, stamp);
                }
                Self::note(format!("Reloaded {name} from disk"), ToastKind::Success);
                self.bound_node = None;
            }
            Err(e) => {
                let name = self
                    .session
                    .buffer_of(node)
                    .map_or_else(String::new, |b| b.name.clone());
                Self::note(format!("Could not reload {name}: {e}"), ToastKind::Error);
            }
        }
    }

    /// Notice files that changed on disk since their last read, so a tab warns before a save over
    /// someone else's work. One look at a time, off the frame loop, waking the window only on a change.
    pub(super) fn check_disk_staleness(&mut self) {
        if self.staleness_looking {
            return;
        }
        let Some(buf) = self.session.buffer.as_ref() else {
            return;
        };
        let (Some(path), Some(stamp)) = (buf.path.clone(), buf.stamp.clone()) else {
            return;
        };
        if buf.stale {
            return;
        }
        let node = buf.node;
        self.staleness_looking = true;
        let looked = stamp.clone();
        self.on_disk_waking(
            "code-file-changed",
            |changed| *changed,
            move || disk::changed_on_disk(&path, &stamp),
            move |app, changed| {
                app.staleness_looking = false;
                if changed
                    && let Some(buf) = app.session.buffer_of_mut(node)
                    && buf.stamp.as_ref() == Some(&looked)
                {
                    buf.stale = true;
                }
            },
        );
    }

    pub(super) fn resolve_conflict(
        &mut self,
        doc: &mut ui::widget::Edits<'_>,
        choice: ConflictChoice,
    ) {
        let Some(conflict) = self.conflict.take() else {
            return;
        };
        match choice {
            ConflictChoice::Overwrite => {
                let snapshot = doc.clone();
                self.save_buffer(&snapshot, conflict.node, true);
            }
            ConflictChoice::Reload => self.reload_buffer(doc, conflict.node),
            ConflictChoice::Cancel => {
                Self::note(format!("{} was not saved", conflict.name), ToastKind::Info);
            }
        }
    }

    /// Ask the host desktop for a folder to open as the project: under app-state confinement the chooser
    /// is the only way out of the editor's one writable directory, and it is where the sandbox's
    /// file-access grant is made.
    pub(super) fn choose_folder(
        &self,
        cx: &mut EffectCx<crate::project::Project, Self>,
    ) -> Effected {
        let request = ui::portal::PickRequest::writable_directory("Open a project folder", "Open")
            .thread_name("code-portal-folder");
        self.choose(cx, request, |app, path| app.open_project_dir(&path))
    }

    pub(super) fn choose_file(&self, cx: &mut EffectCx<crate::project::Project, Self>) -> Effected {
        let request = ui::portal::PickRequest::open("Open a file", "Open")
            .thread_name("code-portal-file")
            .filter(source_filter())
            .all_files();
        self.choose(cx, request, |app, path| app.open_disk_file(&path))
    }

    /// Ask where `node` is saved, then save it there: the buffer is that file from then on.
    pub(super) fn choose_save_as(
        &self,
        node: TreeId,
        cx: &mut EffectCx<crate::project::Project, Self>,
    ) -> EffectAnswer {
        let suggested = self
            .session
            .buffer_of(node)
            .ok_or("no file is open")?
            .name
            .clone();
        let request = ui::portal::PickRequest::save("Save file as", "Save", &suggested)
            .thread_name("code-portal-save")
            .filter(source_filter())
            .all_files();
        Ok(self.choose(cx, request, move |app, path| {
            app.pending_save = Some((node, Some(path)));
            None
        }))
    }

    /// Ask for a place to save `node`, as Save As does, when it has none yet.
    pub(super) fn ask_where_to_save(&mut self, node: TreeId) {
        if self
            .session
            .active_buffer()
            .is_none_or(|buffer| buffer.node != node)
        {
            self.show_node(node);
        }
        self.run_effect(&CodeEditorEffect::SaveAs);
    }

    /// Run `effect` as a person asking for it does, so a chooser it opens answers through the effect.
    pub(super) fn run_effect(&self, effect: &CodeEditorEffect) {
        if let Some(dispatch) = &self.dispatch {
            dispatch.effect(effect);
        }
    }

    /// Whether a file chooser is open.
    pub(super) fn choosing(&self) -> bool {
        std::rc::Rc::strong_count(&self.choosing) > 1
    }

    /// Open the host's chooser for `request`, then do what `chosen` does with the path: its answer, or
    /// `None` when it answers later through [`Self::finish_file_effect`].
    fn choose(
        &self,
        cx: &mut EffectCx<crate::project::Project, Self>,
        request: ui::portal::PickRequest,
        chosen: impl FnOnce(&mut Self, std::path::PathBuf) -> Option<Result<(), String>> + 'static,
    ) -> Effected {
        let choosing = std::rc::Rc::clone(&self.choosing);
        cx.choose_path(request, move |app, path, cx| {
            drop(choosing);
            let path = path.ok_or("file selection cancelled")?;
            if let Some(answer) = chosen(app, path) {
                return answer.map(|()| Effected::done());
            }
            app.file_reply = Some(cx.reply());
            Ok(Effected::Pending)
        })
    }

    /// Write `node`'s text to `path` and make that its file. The buffer keeps its file and conflict
    /// stamp until the new file exists: a failed Save As must not redirect Save or Reload away from the
    /// file the person was editing.
    pub(super) fn save_as(
        &mut self,
        doc: &AutoCommit,
        node: TreeId,
        path: std::path::PathBuf,
        then: AfterSave,
    ) {
        let text = self
            .session
            .buffer_of(node)
            .and_then(|_| EditorDoc::read_buffer_key(doc, &self.buffer_key(node)));
        let Some(text) = text else {
            then.failed(self);
            return;
        };
        self.on_disk(
            "code-save-file-as",
            move || {
                let saved = disk::save(&path, &text, None);
                (path, text, saved)
            },
            move |app, (path, text, saved)| match app.saved_as(node, &path, text, saved) {
                Ok(()) => then.landed(app, node),
                Err(_) => then.failed(app),
            },
        );
    }

    fn saved_as(
        &mut self,
        node: TreeId,
        path: &std::path::Path,
        text: String,
        saved: Result<disk::Stamp, disk::SaveError>,
    ) -> Result<(), String> {
        match saved {
            Ok(stamp) => {
                let name = path.file_name().map_or_else(
                    || "untitled".to_string(),
                    |s| s.to_string_lossy().to_string(),
                );
                self.session.project.set_file_path(node, path, &name);
                if let Some(buf) = self.session.buffer_of_mut(node) {
                    buf.path = Some(path.to_path_buf());
                    buf.name.clone_from(&name);
                    buf.language = crate::model::Language::from_path(&name);
                    buf.mark_saved(text, Some(stamp));
                }
                self.explorer.show(&self.session.project);
                self.invalidate_search_projection();
                self.bound_node = None;
                self.reset_language_server();
                Ok(())
            }
            Err(error) => {
                let message = format!("Could not save {}: {error}", path.display());
                Self::note(&message, ToastKind::Error);
                Err(message)
            }
        }
    }

    pub(super) fn finish_file_effect(&mut self, result: Result<(), String>) {
        if let Some(reply) = self.file_reply.take() {
            reply.finish(result.map(|()| tilo_actions::ActionOutput::default()));
        }
    }

    pub(super) fn open_project_dir(
        &mut self,
        path: &std::path::Path,
    ) -> Option<Result<(), String>> {
        let path = path.to_path_buf();
        self.on_disk_answering(
            "code-open-folder",
            move || {
                let project = Project::from_dir(&path);
                (path, project)
            },
            |app, (path, project)| app.adopt_project_dir(&path, project),
        )
    }

    fn adopt_project_dir(
        &mut self,
        path: &std::path::Path,
        project: std::io::Result<Project>,
    ) -> Result<(), String> {
        match project {
            Ok(project) => {
                self.session = Session::new(project);
                self.adopt_presented();
                self.invalidate_search_projection();
                self.explorer.show(&self.session.project);
                self.explorer.set_current(None);
                self.bound_node = None;
                self.editor.unbind();
                self.reset_language_server();
                self.clear_search();
                self.search_symbols.clear();
                self.symbol_projection_revision = None;
                Ok(())
            }
            Err(e) => {
                let message = could_not_open(path, &e);
                Self::note(&message, ToastKind::Error);
                Err(message)
            }
        }
    }

    pub(super) fn open_disk_file(&mut self, path: &std::path::Path) -> Option<Result<(), String>> {
        let path = path.to_path_buf();
        let root = self.session.project.root_path();
        self.on_disk_answering(
            "code-open-file",
            move || {
                let path = disk::canonical(&path);
                let along = root
                    .filter(|root| path.starts_with(root))
                    .map(|root| crate::model::read_folders_along(&root, &path));
                let single = Project::open_single_file(&path);
                (path, along, single)
            },
            |app, (path, along, single)| {
                if let Some(along) = along {
                    app.session.project.adopt_folders(along);
                    if let Some(node) = app.session.project.reveal_read(&path) {
                        app.explorer.show(&app.session.project);
                        app.open_node(node);
                        return Ok(());
                    }
                }
                app.adopt_single_file(&path, single)
            },
        )
    }

    fn adopt_single_file(
        &mut self,
        path: &std::path::Path,
        single: std::io::Result<(Project, TreeId)>,
    ) -> Result<(), String> {
        match single {
            Ok((project, node)) => {
                self.session = Session::new(project);
                self.adopt_presented();
                self.invalidate_search_projection();
                self.explorer.show(&self.session.project);
                self.bound_node = None;
                self.reset_language_server();
                self.open_node(node);
                Ok(())
            }
            Err(e) => {
                let message = could_not_open(path, &e);
                Self::note(&message, ToastKind::Error);
                Err(message)
            }
        }
    }

    pub(super) fn reveal_on_disk(
        &mut self,
        path: std::path::PathBuf,
        then: impl FnOnce(&mut Self, Option<TreeId>) + 'static,
    ) {
        if let Some(node) = self.session.project.node_for_path(&path) {
            then(self, Some(node));
            return;
        }
        let Some(root) = self.session.project.root_path() else {
            then(self, None);
            return;
        };
        self.on_disk(
            "code-reveal-file",
            move || {
                let along = crate::model::read_folders_along(&root, &path);
                (path, along)
            },
            move |app, (path, along)| {
                app.session.project.adopt_folders(along);
                let node = app.session.project.reveal_read(&path);
                app.invalidate_search_projection();
                then(app, node);
            },
        );
    }
}

/// Why `path` did not open: the editor's sandbox reaches the files the desktop gave it (from Files,
/// dropped on it, picked in the file chooser), not every path on the machine.
pub(super) fn could_not_open(path: &std::path::Path, error: &std::io::Error) -> String {
    let unreachable = matches!(
        error.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
    );
    if unreachable {
        format!(
            "Could not open {}: {error}. Code opens the files it is handed: open it from \
             Files (Open, or Open with Code Editor), which gives Code the file",
            path.display()
        )
    } else {
        format!("Could not open {}: {error}", path.display())
    }
}
