use super::{
    AutoCommit, CodeApp, Conflict, ConflictChoice, EditorDoc, ElementState, Key, KeyEvent,
    PortalIntent, Project, Session, TreeId, WindowEvent, disk, explorer, source_filter,
};

impl CodeApp {
    // ---- saving -----------------------------------------------------------

    /// Write one buffer to its file.
    ///
    /// `force` is the user having answered "overwrite anyway" to a conflict; it
    /// is the only way to skip the freshness check, and it is never set by the
    /// app on its own.
    pub fn save_buffer(&mut self, doc: &AutoCommit, node: TreeId, force: bool) -> bool {
        let Some(idx) = self.session.open.iter().position(|b| b.node == node) else {
            return false;
        };
        let Some(path) = self.session.open.get(idx).and_then(|b| b.path.clone()) else {
            // No file yet: a scratch buffer needs a destination first, which
            // only the user (through the picker) can choose.
            self.request_save_as(node);
            return false;
        };
        let text = EditorDoc::read_buffer_key(doc, &self.buffer_key(node)).unwrap_or_default();
        let expected = if force {
            None
        } else {
            self.session.open.get(idx).and_then(|b| b.stamp.clone())
        };

        match disk::save(&path, &text, expected.as_ref()) {
            Ok(stamp) => {
                let name = self
                    .session
                    .open
                    .get(idx)
                    .map_or_else(String::new, |b| b.name.clone());
                if let Some(buf) = self.session.open.get_mut(idx) {
                    buf.mark_saved(text, Some(stamp));
                }
                self.note(format!("Saved {name}"), false);
                self.reconcile_tabs();
                // Tell the language server the file is on disk now; rustc-based
                // diagnostics only refresh on save.
                if let (Some(c), Some(uri)) = (self.lsp.as_mut(), self.lsp_uri.clone()) {
                    c.did_save(&uri);
                }
                true
            }
            Err(disk::SaveError::Conflict) => {
                if let Some(buf) = self.session.open.get_mut(idx) {
                    buf.stale = true;
                }
                self.conflict = Some(Conflict {
                    node,
                    name: self
                        .session
                        .open
                        .get(idx)
                        .map_or_else(String::new, |b| b.name.clone()),
                });
                false
            }
            Err(e) => {
                let name = self
                    .session
                    .open
                    .get(idx)
                    .map_or_else(String::new, |b| b.name.clone());
                self.note(format!("Could not save {name}: {e}"), true);
                false
            }
        }
    }

    /// Save the active buffer.
    pub(super) fn save_active(&mut self, doc: &AutoCommit) -> bool {
        match self.session.active_buffer().map(|b| b.node) {
            Some(node) => self.save_buffer(doc, node, false),
            None => false,
        }
    }

    /// Save every buffer with unsaved edits. Reports how many were written, and
    /// stops at the first conflict so the user answers one question at a time.
    pub(super) fn save_all(&mut self, doc: &AutoCommit) -> usize {
        let dirty: Vec<TreeId> = self
            .session
            .open
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
        if self.conflict.is_none() {
            match written {
                0 => self.note("Nothing to save", false),
                1 => self.note("Saved 1 file", false),
                n => self.note(format!("Saved {n} files"), false),
            }
        }
        written
    }

    /// Throw away this buffer's edits and take what is on disk.
    pub(super) fn reload_buffer(&mut self, doc: &mut AutoCommit, node: TreeId) {
        let Some(idx) = self.session.open.iter().position(|b| b.node == node) else {
            return;
        };
        match self.session.project.file_seed(node) {
            Ok((text, stamp)) => {
                let name = self
                    .session
                    .open
                    .get(idx)
                    .map_or_else(String::new, |b| b.name.clone());
                // Disk reload is still a visible buffer edit, so it uses the
                // same action-owned splice path as a keystroke or replace.
                let file_id = self.buffer_key(node);
                let base = EditorDoc::read_buffer_key(doc, &file_id).unwrap_or_default();
                if let Err(error) = self.dispatch_text_action(doc, file_id, &name, &base, &text) {
                    self.editor.reconcile_from_doc(doc);
                    self.note(format!("Reload was refused: {error}"), true);
                    return;
                }
                if let Some(buf) = self.session.open.get_mut(idx) {
                    buf.mark_saved(text, stamp);
                }
                self.note(format!("Reloaded {name} from disk"), false);
                self.bound_node = None;
                self.reconcile_tabs();
            }
            Err(e) => {
                let name = self
                    .session
                    .open
                    .get(idx)
                    .map_or_else(String::new, |b| b.name.clone());
                self.note(format!("Could not reload {name}: {e}"), true);
            }
        }
    }

    /// Notice files that changed on disk since we read them, so a tab can warn
    /// before the user tries to save over someone else's work.
    pub(super) fn check_disk_staleness(&mut self) {
        for buf in &mut self.session.open {
            let (Some(path), Some(stamp)) = (buf.path.as_ref(), buf.stamp.as_ref()) else {
                continue;
            };
            if !buf.stale && disk::changed_on_disk(path, stamp) {
                buf.stale = true;
            }
        }
    }

    /// Answer an open conflict.
    pub(super) fn resolve_conflict(&mut self, doc: &mut AutoCommit, choice: ConflictChoice) {
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
                self.note(format!("{} was not saved", conflict.name), false);
            }
        }
    }

    /// The conflict prompt's keyboard shortcuts: Escape cancels, Enter takes
    /// the safe option (reload, which cannot destroy the other writer's work).
    pub(super) fn conflict_key(event: &WindowEvent) -> Option<ConflictChoice> {
        let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key,
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
        else {
            return None;
        };
        match logical_key {
            Key::Named(winit::keyboard::NamedKey::Escape) => Some(ConflictChoice::Cancel),
            Key::Named(winit::keyboard::NamedKey::Enter) => Some(ConflictChoice::Reload),
            _ => None,
        }
    }

    // ---- the file picker --------------------------------------------------

    /// Ask the host desktop for a folder to open as the project.
    ///
    /// This goes through the XDG `FileChooser` portal rather than any in-app
    /// browser: under app-state confinement the editor has exactly one writable
    /// directory of its own and no manifest field can add more, so a path the
    /// user picked in the portal — which is where the sandbox's file-access
    /// grant is made — is the only way it reaches the rest of the filesystem.
    pub(super) fn request_open_folder(&mut self) {
        self.portal_intent = Some(PortalIntent::OpenFolder);
        self.portal = Some(
            ui::portal::PickRequest::open("Open a project folder", "Open")
                .thread_name("code-portal-folder")
                .send(),
        );
    }

    /// Ask the host desktop for a file to open into a tab.
    pub(super) fn request_open_file(&mut self) {
        self.portal_intent = Some(PortalIntent::OpenFile);
        self.portal = Some(
            ui::portal::PickRequest::open("Open a file", "Open")
                .thread_name("code-portal-file")
                .filter(source_filter())
                .all_files()
                .send(),
        );
    }

    /// Ask the host desktop where to write a buffer that has no file yet.
    pub(super) fn request_save_as(&mut self, node: TreeId) {
        let suggested = self
            .session
            .buffer_of(node)
            .map_or_else(|| "untitled.txt".to_string(), |b| b.name.clone());
        self.portal_intent = Some(PortalIntent::SaveAs(node));
        self.portal = Some(
            ui::portal::PickRequest::save("Save file as", "Save", &suggested)
                .thread_name("code-portal-save")
                .filter(source_filter())
                .all_files()
                .send(),
        );
    }

    /// Drain a pending picker, once per frame. Never blocks: the portal's
    /// D-Bus round-trip and its host dialog live entirely on a worker thread.
    pub(super) fn poll_portal(&mut self, doc: &AutoCommit) {
        let Some(rx) = self.portal.as_ref() else {
            return;
        };
        let outcome = match rx.try_recv() {
            Ok(o) => o,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.portal = None;
                self.portal_intent = None;
                self.finish_resource_file(Err("file picker disconnected".into()));
                return;
            }
        };
        self.portal = None;
        let intent = self.portal_intent.take();
        self.apply_portal_outcome(doc, intent, outcome);
    }

    /// Fold a picker result into the app. Separate from the channel plumbing so
    /// a test can drive the exact path a real pick takes.
    pub(crate) fn apply_portal_outcome(
        &mut self,
        doc: &AutoCommit,
        intent: Option<PortalIntent>,
        outcome: ui::portal::PortalOutcome,
    ) {
        use ui::portal::PortalOutcome;
        let path = match outcome {
            PortalOutcome::Opened(p) => p,
            PortalOutcome::Cancelled => {
                if let Some(invocation) = self.resource_invocation.take() {
                    self.resource_result = Some(tilo_actions::InvocationUpdate::Cancelled {
                        invocation,
                        message: Some("file selection cancelled".into()),
                    });
                }
                return;
            }
            PortalOutcome::Failed(e) => {
                self.note(format!("File picker unavailable: {e}"), true);
                self.finish_resource_file(Err(e));
                return;
            }
        };
        let result = match intent {
            Some(PortalIntent::OpenFolder) => self.open_project_dir(&path),
            Some(PortalIntent::OpenFile) => self.open_disk_file(&path),
            Some(PortalIntent::SaveAs(node)) => {
                let name = path.file_name().map_or_else(
                    || "untitled".to_string(),
                    |s| s.to_string_lossy().to_string(),
                );
                self.session.project.set_file_path(node, &path, &name);
                if let Some(buf) = self.session.buffer_of_mut(node) {
                    buf.path = Some(path.clone());
                    buf.name.clone_from(&name);
                    buf.language = crate::model::Language::from_path(&name);
                    // No stamp: this is a destination the user chose, and the
                    // save that follows is the one they asked for.
                    buf.stamp = None;
                }
                explorer::refresh(&mut self.sidebar, &self.session.project);
                self.invalidate_search_projection();
                let snapshot = doc.clone();
                if self.save_buffer(&snapshot, node, true) {
                    Ok(())
                } else {
                    Err("file could not be saved".into())
                }
            }
            None => Ok(()),
        };
        self.finish_resource_file(result);
    }

    pub(super) fn finish_resource_file(&mut self, result: Result<(), String>) {
        let Some(invocation) = self.resource_invocation.take() else {
            return;
        };
        self.resource_result = Some(match result {
            Ok(()) => tilo_actions::InvocationUpdate::Succeeded {
                invocation,
                output: tilo_actions::ActionOutput::default(),
            },
            Err(message) => tilo_actions::InvocationUpdate::Failed {
                invocation,
                error: tilo_actions::ActionFailure {
                    code: "code_editor.io_failed".into(),
                    message,
                    details: None,
                    retryable: false,
                },
            },
        });
    }

    /// Replace the project with the directory at `path`.
    pub(super) fn open_project_dir(&mut self, path: &std::path::Path) -> Result<(), String> {
        match Project::from_dir(path) {
            Ok(project) => {
                let name = project.name.clone();
                self.session = Session::new(project);
                self.invalidate_search_projection();
                explorer::refresh(&mut self.sidebar, &self.session.project);
                explorer::set_current(&mut self.sidebar, None);
                self.bound_node = None;
                self.editor.unbind();
                self.reset_language_server();
                self.reconcile_tabs();
                self.note(format!("Opened {name}"), false);
                Ok(())
            }
            Err(e) => {
                let message = format!("Could not open {}: {e}", path.display());
                self.note(&message, true);
                Err(message)
            }
        }
    }

    /// Open a real file into a tab, revealing it in the tree when it lives
    /// under the open project and adopting it as a one-file project when not.
    pub(super) fn open_disk_file(&mut self, path: &std::path::Path) -> Result<(), String> {
        let path = disk::canonical(path);
        if let Some(node) = self.session.project.reveal_path(&path) {
            explorer::refresh(&mut self.sidebar, &self.session.project);
            self.open_node(node);
            return Ok(());
        }
        match Project::open_single_file(&path) {
            Ok((project, node)) => {
                self.session = Session::new(project);
                self.invalidate_search_projection();
                explorer::refresh(&mut self.sidebar, &self.session.project);
                self.bound_node = None;
                self.reset_language_server();
                self.open_node(node);
                Ok(())
            }
            Err(e) => {
                let message = format!("Could not open {}: {e}", path.display());
                self.note(&message, true);
                Err(message)
            }
        }
    }
}
