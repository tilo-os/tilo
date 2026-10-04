#[allow(
    clippy::wildcard_imports,
    reason = "this file is one part of the `VideoEditorApp` impl split out of `app.rs`; it shares the parent's whole scope"
)]
use super::*;

impl VideoEditorApp {
    // ---- project files ---------------------------------------------------
    //
    // ## What happens to the implicit autosave
    //
    // The autosave document does not go away and is not demoted. It remains
    // the thing that makes work impossible to lose: it holds whatever is open
    // right now, saved or not, and it is written back on a debounce whether or
    // not the project has ever been given a file. An unsaved project therefore
    // survives a crash, a reboot and a close, exactly as it did before files
    // existed — which is why closing the window is not made to interrogate the
    // user about saving.
    //
    // What files add is a *second*, explicit copy the user names, can put
    // where they like, can have several of, and can hand to someone. The two
    // never race: a Save writes the file from the same project the autosave
    // holds, and an Open replaces the document — so the autosave then holds
    // the opened project, and the file that was open before is untouched on
    // disk.
    //
    // The one place that could destroy work is opening a file over a project
    // that has never been saved anywhere, because the Open replaces the
    // document the autosave is tracking. That is the only case
    // [`Self::begin_open`] stops to ask about, and Ctrl+Z undoes it even if
    // the user says yes.

    /// Whether the document has changed since the last save.
    ///
    /// A project that has never been saved counts as unsaved as soon as it
    /// has any content — an empty project nobody has touched is not
    /// "unsaved work" and must not raise a question about discarding it.
    pub(super) fn project_is_dirty(&self) -> bool {
        match &self.saved_heads {
            Some(heads) => *heads != self.doc_heads,
            None => !self.project_is_empty(),
        }
    }

    /// Whether the project holds nothing a user would mind losing.
    pub(super) fn project_is_empty(&self) -> bool {
        self.project.clips.is_empty()
            && self.project.media.is_empty()
            && self.project.tracks.is_empty()
    }

    /// What the project button says: the file's name, or the project's name
    /// while it has no file yet.
    pub(super) fn project_label(&self) -> String {
        match &self.project_path {
            Some(p) => crate::project_file::recents::label(p),
            None if self.project.name.trim().is_empty() => "Untitled".to_string(),
            None => self.project.name.clone(),
        }
    }

    /// The one-line answer to "what am I editing, and is it safe?".
    ///
    /// Spoken to a screen reader and used as the project button's tooltip
    /// text, so both say the same thing the dot on the button means.
    pub(super) fn project_status_line(&self) -> String {
        let label = self.project_label();
        match (&self.project_path, self.project_is_dirty()) {
            (Some(p), true) => format!("{label} — unsaved changes, saved at {}", p.display()),
            (Some(p), false) => format!("{label} — saved at {}", p.display()),
            // Not a warning: the autosave really does hold it. What the user
            // does not have is a file of their own.
            (None, true) => format!("{label} — not saved to a file yet"),
            (None, false) => format!("{label} — empty project"),
        }
    }

    /// Save straight back to the file the project came from, or fall through
    /// to Save As when it has never had one.
    pub(super) fn save_project(&mut self, doc: &mut AutoCommit) {
        match self.project_path.clone() {
            Some(path) => self.write_project_to(doc, path),
            None => self.request_save_as(),
        }
    }

    pub(super) fn queue_save_project(&mut self) {
        self.project_sync.save_requested = true;
    }

    /// Ask the host where to write the project. Under a strict sandbox this
    /// round-trip is what earns the write at all.
    pub(super) fn request_save_as(&mut self) {
        if self.project_rx.is_some() {
            return; // one chooser at a time
        }
        let suggested = crate::project_file::with_extension(&self.project_label());
        self.project_notice = Some(Notice::ok("Choose where to save…"));
        self.project_rx = Some(portal::request_save_project(&suggested));
    }

    /// Ask the host for a project to open.
    pub(super) fn request_open_project(&mut self) {
        if self.project_rx.is_some() {
            return;
        }
        self.project_notice = None;
        self.project_rx = Some(portal::request_open_project());
    }

    /// Write the project to `path` and remember it as the project's home.
    ///
    /// A failure is reported in the words the file layer produced and the
    /// project keeps its old path: claiming a save that did not happen is the
    /// one outcome that could actually cost someone their work.
    pub(super) fn write_project_to(&mut self, doc: &mut AutoCommit, path: PathBuf) {
        match crate::project_file::save(&path, &self.project) {
            Ok(()) => {
                self.recents = crate::project_file::recents::remember(&self.recents, &path);
                self.project_notice = Some(Notice::ok(saved_words(&path)));
                // Save As can move the project; relative media follows it.
                crate::media::set_project_anchor(path.parent().map(std::path::Path::to_path_buf));
                self.project_path = Some(path);
                // The document's heads as they are *now*, not the copy `sync`
                // cached at the start of the frame. Saving from a key press
                // that followed an edit in the same frame would otherwise
                // stamp the pre-edit heads and leave the project reading as
                // unsaved the instant after it was written.
                self.doc_heads = doc.get_heads();
                self.saved_heads = Some(self.doc_heads.clone());
                // A save that was only standing in the way of an open now
                // lets it through.
                if let Some(next) = self.save_then_open.take() {
                    self.pending_open = None;
                    self.load_project_file(doc, next);
                }
            }
            Err(e) => {
                self.save_then_open = None;
                self.project_notice = Some(Notice::failed(e.to_string()));
            }
        }
    }

    /// Start opening `path`, stopping to ask first if that would drop work
    /// that exists nowhere but this session.
    pub(super) fn begin_open(&mut self, doc: &mut AutoCommit, path: PathBuf) {
        // Already saved somewhere, or nothing worth keeping: just open it.
        // A project with a file behind it is not at risk — that file is still
        // there afterwards — so only a never-saved project raises the question.
        if self.project_path.is_some() || !self.project_is_dirty() {
            self.load_project_file(doc, path);
            return;
        }
        let losing = self.project_label();
        self.pending_open = Some(PendingOpen { path, losing });
    }

    /// Read `path` and replace the project with what is in it.
    ///
    /// The replacement goes through the ordinary action pipeline, so opening
    /// a file is one undo boundary and replicates to a peer — see
    /// [`crate::actions::VideoEditorAction::LoadProject`].
    pub(super) fn load_project_file(&mut self, doc: &mut AutoCommit, path: PathBuf) {
        let mut project = match crate::project_file::load(&path) {
            Ok(p) => p,
            Err(e) => {
                self.project_notice = Some(Notice::failed(e.to_string()));
                return;
            }
        };
        // Relative media in this file means "beside this file" — set before
        // anything asks whether a source can be read, including the notice
        // below, or a project copied somewhere with its footage would report
        // the footage missing.
        crate::media::set_project_anchor(path.parent().map(std::path::Path::to_path_buf));
        // The name follows the file unless the file's own project carries a
        // real one — a project still called "Untitled" opened from
        // `Holiday.qvp` should read as Holiday.
        if (project.name.trim().is_empty() || project.name == "Untitled")
            && let Some(n) = crate::project_file::name_from_path(&path)
        {
            project.name = n;
        }
        // Whether this Open is throwing a timeline away. If it is, the notice
        // says the way back, because an accidental Open otherwise feels final
        // when it is not. It now names the control rather than only the key:
        // the transport strip carries a real Undo button, and this dispatch
        // records the boundary it reverts.
        let replaced = !self.project_is_empty();
        let mut notice = Self::opened_notice(&path, &project);
        if replaced {
            notice.text.push_str(". Undo to go back");
        }
        self.project_notice = Some(notice);

        self.actions.dispatch(
            doc,
            crate::actions::VIDEO_EDITOR_ACTION_TYPE,
            &crate::actions::load_project_action(&project),
        );
        self.project = VideoDoc::load_project(doc);
        self.clip_edit_bases.clear();
        self.doc_heads = doc.get_heads();
        self.saved_heads = Some(self.doc_heads.clone());
        self.recents = crate::project_file::recents::remember(&self.recents, &path);
        self.project_path = Some(path);
        // The view belongs to the project that was open, not to this one.
        self.selection = Selection::default();
        self.selected_media = None;
        self.playhead = 0.0;
        self.scroll = 0.0;
        self.playback.playing = false;
        self.seed.selection_initialised = false;
        self.ensure_initial_selection();
        #[cfg(any(feature = "gst", feature = "decode"))]
        {
            self.media_pipeline.monitor_stale = true;
        }
    }

    /// What to say after an open: how much arrived, and — if any of it cannot
    /// be read here — which sources, in the same words the bin and the
    /// timeline use for the same states.
    pub(super) fn opened_notice(path: &Path, project: &crate::model::Project) -> Notice {
        let name = crate::project_file::recents::label(path);
        let missing = crate::project_file::missing_sources(project);
        if missing.is_empty() {
            return Notice::ok(format!("Opened {name}"));
        }
        // Split on the distinction the app already draws everywhere else:
        // bytes that are not here yet, versus bytes that are here and are not
        // media. Blurring them would tell a user to re-fetch a file that will
        // never decode.
        let absent = missing.len();
        let names: Vec<&str> = missing.iter().take(3).map(|(n, _)| n.as_str()).collect();
        let listed = names.join(", ");
        let tail = if absent > names.len() {
            format!("{listed} and {} more", absent - names.len())
        } else {
            listed
        };
        // Opened, but not everything came with it: a warning, not a success
        // and not a failure.
        Notice::warning(format!("Opened {name} — {MEDIA_NOT_LOCAL}: {tail}"))
    }

    /// Drain an in-flight project chooser. Mirrors [`Self::poll_export`].
    pub(super) fn drain_project_portal(&mut self, doc: &mut AutoCommit) {
        let mut outcome = None;
        let mut clear = false;
        if let Some(rx) = self.project_rx.as_ref() {
            match rx.try_recv() {
                Ok(o) => {
                    outcome = Some(o);
                    clear = true;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => clear = true,
            }
        }
        if clear {
            self.project_rx = None;
        }
        match outcome {
            Some(portal::ProjectOutcome::Save(path)) => self.write_project_to(doc, path),
            Some(portal::ProjectOutcome::Open(path)) => self.begin_open(doc, path),
            Some(portal::ProjectOutcome::Cancelled) => {
                self.save_then_open = None;
                self.project_notice = None;
                if let Some(invocation) = self.resource_invocation.take() {
                    self.resource_operation = None;
                    self.resource_result = Some(tilo_actions::InvocationUpdate::Cancelled {
                        invocation,
                        message: Some("project file selection was cancelled".into()),
                    });
                }
            }
            Some(portal::ProjectOutcome::Failed(reason)) => {
                // A portal that refused is a failure, tinted as one. Its own
                // wording ("the file picker needs the real-desktop build")
                // names no English the old substring test looked for, so this
                // used to paint in the success tint.
                self.save_then_open = None;
                self.project_notice = Some(Notice::failed(reason));
            }
            None => {}
        }
    }

    /// Answer the "this project has never been saved" question.
    ///
    /// `Discard` opens anyway — recoverable, because the open is one action
    /// and Ctrl+Z puts the old project back. `Save` writes first and then
    /// continues into the open on its own, so the user does not have to
    /// remember what they were doing.
    pub(super) fn resolve_pending_open(&mut self, doc: &mut AutoCommit, choice: DiscardChoice) {
        let Some(pending) = self.pending_open.take() else {
            return;
        };
        match choice {
            DiscardChoice::Cancel => {
                self.save_then_open = None;
            }
            DiscardChoice::Discard => self.load_project_file(doc, pending.path),
            DiscardChoice::Save => {
                self.save_then_open = Some(pending.path);
                self.save_project(doc);
            }
        }
    }

    /// The rows the project menu offers right now.
    pub(super) fn project_menu_rows(&self) -> Vec<(ProjectMenuItem, String)> {
        let mut rows = vec![
            (ProjectMenuItem::Open, "Open…".to_string()),
            (
                ProjectMenuItem::Save,
                match &self.project_path {
                    Some(_) => "Save".to_string(),
                    // Naming what Save will actually do, rather than letting
                    // the user press it and be surprised by a file chooser.
                    None => "Save…".to_string(),
                },
            ),
            (ProjectMenuItem::SaveAs, "Save as…".to_string()),
        ];
        // Recents, minus the project already open — an entry that reopens
        // what you are looking at is a row that does nothing.
        let recent: Vec<&PathBuf> = self
            .recents
            .iter()
            .filter(|p| Some(*p) != self.project_path.as_ref())
            .collect();
        if !recent.is_empty() {
            for p in recent {
                rows.push((
                    ProjectMenuItem::Recent(p.clone()),
                    crate::project_file::recents::label(p),
                ));
            }
        }
        rows
    }

    /// Open the project menu under the project button.
    pub(super) fn open_project_menu(&mut self, at: Point) {
        let rows = self.project_menu_rows();
        self.project_menu = ContextMenu::new();
        self.project_menu_items.clear();
        let mut first_recent = true;
        for (item, label) in rows {
            if matches!(item, ProjectMenuItem::Recent(_)) && first_recent {
                self.project_menu.push(ui::widget::MenuItem::separator());
                first_recent = false;
            }
            self.project_menu
                .push(ui::widget::MenuItem::action(label, || {}));
            self.project_menu_items.push(item);
        }
        self.project_menu.open_at([at.x as f32, at.y as f32]);
        self.project_menu.layout_at_position();
    }

    /// Resolve a click on the open project menu. Separators occupy a row in
    /// the menu but not in [`Self::project_menu_items`], so the index is
    /// mapped through the same skip the build used rather than assumed equal.
    pub(super) fn project_menu_intent(&self, idx: usize) -> Option<ProjectMenuItem> {
        let mut actionable = 0usize;
        for (i, row) in self.project_menu.items().iter().enumerate() {
            if matches!(row, ui::widget::MenuItem::Separator) {
                continue;
            }
            if i == idx {
                return self.project_menu_items.get(actionable).cloned();
            }
            actionable += 1;
        }
        None
    }

    /// Act on a chosen project-menu row.
    pub(super) fn run_project_menu_item(&mut self, doc: &mut AutoCommit, item: ProjectMenuItem) {
        match item {
            ProjectMenuItem::Open => self.request_open_project(),
            ProjectMenuItem::Save => self.queue_save_project(),
            ProjectMenuItem::SaveAs => self.request_save_as(),
            ProjectMenuItem::Recent(p) => self.begin_open(doc, p),
        }
    }

    // ---- export ----------------------------------------------------------
    /// Ask the user where the render should land, then encode there. The
    /// picker is the grant: under a strict sandbox it is what earns the write
    /// to somewhere like `~/Videos`, and without it the file lands in a temp
    /// directory the user was never told about and cannot find.
    pub(super) fn request_export(&mut self) {
        if self.export_job.is_some() {
            return; // one render at a time
        }
        if let Some(reason) = crate::export::unavailable_reason() {
            self.export_status = ExportStatus::Failed {
                reason: reason.to_string(),
            };
            return;
        }
        let name = self.export.suggested_filename(&self.project.name);
        let ext = self.export.container.extension();
        self.export_notice = Some("Choose where to save…".to_string());
        self.export_dest_rx = Some(portal::request_export_destination(&name, ext));
    }

    /// Begin the render at `destination` on a worker thread.
    pub(super) fn begin_export_to(&mut self, destination: PathBuf) {
        self.export_notice = None;
        self.export_status = ExportStatus::Rendering { progress: 0.0 };
        self.export_job = Some(crate::export::start_export(
            &self.project,
            &self.export,
            destination,
        ));
    }

    /// Drain the destination picker and poll the running render. Called every
    /// frame; `needs_animation_frame` keeps frames coming while a job runs, so
    /// the progress bar actually moves.
    pub(super) fn poll_export(&mut self) {
        // Destination round-trip.
        let mut outcome = None;
        let mut clear = false;
        if let Some(rx) = self.export_dest_rx.as_ref() {
            match rx.try_recv() {
                Ok(o) => {
                    outcome = Some(o);
                    clear = true;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => clear = true,
            }
        }
        if clear {
            self.export_dest_rx = None;
        }
        match outcome {
            Some(portal::ExportDestOutcome::Opened(path)) => self.begin_export_to(path),
            Some(portal::ExportDestOutcome::Cancelled) => {
                self.export_notice = None;
                if let Some(invocation) = self.resource_invocation.take() {
                    self.resource_operation = None;
                    self.resource_result = Some(tilo_actions::InvocationUpdate::Cancelled {
                        invocation,
                        message: Some("export destination selection was cancelled".into()),
                    });
                }
            }
            Some(portal::ExportDestOutcome::Failed(reason)) => {
                // No portal on this box. Say where the file will go rather than
                // silently dropping it somewhere the user will never look.
                let fallback = crate::export::output_path(&self.export, &self.project);
                self.export_notice = Some(format!("{reason} — saving to {}", fallback.display()));
                self.begin_export_to(fallback);
            }
            None => {}
        }
        // Running render.
        if let Some(job) = self.export_job.as_ref() {
            let status = job.status();
            let finished = !matches!(status, ExportStatus::Rendering { .. });
            self.export_status = status;
            if finished {
                self.export_job = None;
            }
        }
    }

    /// Whether a render is in flight (so Cancel means "stop it", not "close").
    pub(super) fn export_running(&self) -> bool {
        self.export_job.is_some()
    }

    /// Whether the Export action is actually available. The dialog paints the
    /// button from this and the press path gates on it, so the offer and the
    /// behaviour cannot disagree — which they did: the dialog said "Export
    /// needs the `GStreamer` build" and put an enabled Export button under it.
    pub(super) fn export_is_offered(&self) -> bool {
        crate::export::unavailable_reason().is_none() && !self.export_running()
    }
}

/// What the header row says after a save: the folder the file went to, by
/// name. The project button already names the file, and a whole path in a
/// pill is a line nobody reads.
pub(super) fn saved_words(path: &Path) -> String {
    path.parent().and_then(Path::file_name).map_or_else(
        || "Saved".to_string(),
        |folder| format!("Saved to {}", folder.to_string_lossy()),
    )
}
