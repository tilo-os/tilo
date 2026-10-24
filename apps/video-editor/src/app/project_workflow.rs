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

    /// Follow the document's heads, which say whether the project changed
    /// since it was saved. The first change after an open is the open
    /// itself: the project as the file holds it, so saved.
    pub(super) fn track_saved_heads(&mut self, heads: Vec<automerge::ChangeHash>) {
        if heads == self.doc_heads {
            return;
        }
        if std::mem::take(&mut self.project_sync.open_submitted) {
            self.saved_heads = Some(heads.clone());
        }
        self.doc_heads = heads;
    }

    /// Whether the project holds nothing a user would mind losing.
    pub(super) fn project_is_empty(&self) -> bool {
        self.project.clips.is_empty()
            && self.project.media.is_empty()
            && self.project.tracks.is_empty()
    }

    /// What the window bar's location says: the file's name, or the project's name
    /// while it has no file yet.
    pub(super) fn project_label(&self) -> String {
        let kept = self
            .project_destination
            .as_ref()
            .map(|kept| Path::new(kept.name()));
        match self.project_path.as_deref().or(kept) {
            Some(p) => crate::project_file::recents::label(p),
            None if self.project.name.trim().is_empty() => "Untitled".to_string(),
            None => self.project.name.clone(),
        }
    }

    /// The one-line answer to "what am I editing, and is it safe?".
    ///
    /// The window bar's location speaks it to a screen reader, alongside
    /// the bar's own edited state.
    pub(super) fn project_status_line(&self) -> String {
        let label = self.project_label();
        match (&self.project_path, self.project_is_dirty()) {
            (Some(p), true) => format!("{label} — unsaved changes, saved at {}", p.display()),
            (Some(p), false) => format!("{label} — saved at {}", p.display()),
            (None, true) if self.project_destination.is_some() => {
                format!("{label}, unsaved changes")
            }
            (None, false) if self.project_destination.is_some() => format!("{label}, saved"),
            // Not a warning: the autosave really does hold it. What the user
            // does not have is a file of their own.
            (None, true) => format!("{label} — not saved to a file yet"),
            (None, false) => format!("{label} — empty project"),
        }
    }

    /// Save straight back to the file the project came from, or to where
    /// it was saved last, or fall through to Save As when it has never been
    /// saved.
    pub(super) fn save_project(&mut self, doc: &AutoCommit) {
        if let Some(path) = self.project_path.clone() {
            self.write_project_to(doc, path);
        } else if self.project_destination.is_some() {
            self.write_project_to_destination(doc);
        } else {
            self.request_save_as();
        }
    }

    /// Write the project where it was last saved with no path here (kept in
    /// [`Self::project_destination`]), without asking.
    fn write_project_to_destination(&mut self, doc: &AutoCommit) {
        if self.project_rx.is_some() {
            return; // one write at a time
        }
        let Some(destination) = &self.project_destination else {
            return;
        };
        match crate::project_file::encode(&self.project) {
            Ok(bytes) => {
                self.project_rx = Some(portal::ProjectChannel::Saving(destination.write(bytes)));
                // What is written is the project as it stands now.
                self.saving_heads = Some(doc.committed_heads());
            }
            Err(e) => {
                self.save_then_open = None;
                self.hold_save_failure(e.to_string());
            }
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
        self.project_notice
            .show_progress(Notice::ok("Choose where to save…"));
        self.project_rx = Some(portal::request_save_project(&suggested));
    }

    /// Ask the host for a project to open.
    pub(super) fn request_open_project(&mut self) {
        if self.project_rx.is_some() {
            return;
        }
        self.show_save_failure_or_nothing();
        self.project_rx = Some(portal::request_open_project());
    }

    /// Write the project to `path` and remember it as the project's home.
    ///
    /// A failure is reported in the words the file layer produced and the
    /// project keeps its old path: claiming a save that did not happen is the
    /// one outcome that could actually cost someone their work.
    pub(super) fn write_project_to(&mut self, doc: &AutoCommit, path: PathBuf) {
        match crate::project_file::save(&path, &self.project) {
            Ok(()) => {
                self.recents = crate::project_file::recents::remember(&self.recents, &path);
                // The location's edited state clears; saving says
                // nothing more.
                self.project_notice.dismiss();
                // Save As can move the project; relative media follows it.
                crate::media::set_project_anchor(path.parent().map(std::path::Path::to_path_buf));
                self.project_path = Some(path);
                self.project_destination = None;
                // The document's heads as they are *now*, not the copy `sync`
                // cached at the start of the frame. Saving from a key press
                // that followed an edit in the same frame would otherwise
                // stamp the pre-edit heads and leave the project reading as
                // unsaved the instant after it was written.
                self.saved_now(doc);
            }
            Err(e) => {
                self.save_then_open = None;
                self.hold_save_failure(e.to_string());
            }
        }
    }

    /// The project as it now stands is saved: the edited state clears and a
    /// save that was only standing in the way of an open lets it through.
    fn saved_now(&mut self, doc: &AutoCommit) {
        // The document's heads as they are *now*, not the copy `sync`
        // cached at the start of the frame. Saving from a key press that
        // followed an edit in the same frame would otherwise stamp the
        // pre-edit heads and leave the project reading as unsaved the
        // instant after it was written. A write that finished later stamps
        // the heads it wrote, so an edit made meanwhile stays unsaved.
        self.doc_heads = doc.committed_heads();
        self.save_failure = None;
        self.saved_heads = Some(
            self.saving_heads
                .take()
                .unwrap_or_else(|| self.doc_heads.clone()),
        );
        if let Some(next) = self.save_then_open.take() {
            self.pending_open = None;
            self.load_project_file(next.path, next.contents.as_deref());
        }
    }

    /// Start opening `path` (its `contents`, where the chooser handed those
    /// over instead), stopping to ask first if that would drop work that
    /// exists nowhere but this session.
    pub(super) fn begin_open(&mut self, path: PathBuf, contents: Option<Vec<u8>>) {
        // Already saved somewhere, or nothing worth keeping: just open it.
        // A project with a file behind it is not at risk — that file is still
        // there afterwards — so only a never-saved project raises the question.
        if self.project_path.is_some() || !self.project_is_dirty() {
            self.load_project_file(path, contents.as_deref());
            return;
        }
        let losing = self.project_label();
        self.pending_open = Some(PendingOpen {
            path,
            contents,
            losing,
        });
    }

    /// Read `path` (or decode `contents`, a file with no path here) and
    /// replace the project with what is in it.
    ///
    /// The replacement goes through the ordinary action pipeline, so opening
    /// a file is one undo boundary and replicates to a peer — see
    /// [`crate::actions::VideoEditorAction::LoadProject`].
    pub(super) fn load_project_file(&mut self, path: PathBuf, contents: Option<&[u8]>) {
        let read = match contents {
            Some(bytes) => crate::project_file::decode(bytes),
            None => crate::project_file::load(&path),
        };
        let mut project = match read {
            Ok(p) => p,
            Err(e) => {
                self.project_notice.show(Notice::failed(e.to_string()));
                return;
            }
        };
        // Relative media in this file means "beside this file" — set before
        // anything asks whether a source can be read, including the notice
        // below, or a project copied somewhere with its footage would report
        // the footage missing. A file with no path here has nothing beside it.
        let located = contents.is_none();
        crate::media::set_project_anchor(
            path.parent()
                .filter(|_| located)
                .map(std::path::Path::to_path_buf),
        );
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
            notice.action = Some(super::notices::NoticeAction::Undo);
        }
        self.project_notice.show(notice);
        // A failed save of the project this replaces is about it, not this one.
        self.save_failure = None;

        self.submit(vec![crate::resource::load_project(&project)]);
        // The host applies the open after this event; the document it leaves
        // is the one the file holds, so that is what reads as saved.
        // Recording today's heads instead would call the opened project
        // unsaved the moment it arrived.
        self.project = project;
        self.clip_edit_bases.clear();
        self.project_sync.open_submitted = true;
        // Wherever the last project was saved is not this one's place.
        self.project_destination = None;
        if located {
            self.recents = crate::project_file::recents::remember(&self.recents, &path);
            self.project_path = Some(path);
        } else {
            // Save asks where again: there is no path to write back to.
            self.project_path = None;
        }
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
    pub(super) fn drain_project_portal(&mut self, doc: &AutoCommit) {
        let mut outcome = None;
        let mut clear = false;
        let saving = self
            .project_rx
            .as_ref()
            .is_some_and(portal::ProjectChannel::is_save);
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
            Some(portal::ProjectOutcome::SaveTo(destination)) => {
                // Kept: the next Save writes here without asking.
                self.project_destination = Some(destination);
                self.show_save_failure_or_nothing();
                self.write_project_to_destination(doc);
            }
            Some(portal::ProjectOutcome::Open(path)) => self.begin_open(path, None),
            Some(portal::ProjectOutcome::OpenContents { name, bytes }) => {
                self.begin_open(PathBuf::from(name), Some(bytes));
            }
            Some(portal::ProjectOutcome::Saved) => {
                self.project_notice.dismiss();
                // The project now lives where it was saved, not at the path
                // of a file it may have been opened from.
                self.project_path = None;
                self.saved_now(doc);
            }
            Some(portal::ProjectOutcome::Cancelled) => {
                self.save_then_open = None;
                self.show_save_failure_or_nothing();
                if self.resource_reply.is_some() {
                    self.settle_resource_job(Err("project file selection was cancelled".into()));
                }
            }
            Some(portal::ProjectOutcome::Failed(reason)) => {
                self.saving_heads = None;
                // A portal that refused is a failure, tinted as one. Its own
                // wording ("the file picker needs the real-desktop build")
                // names no English the old substring test looked for, so this
                // used to paint in the success tint.
                self.save_then_open = None;
                if saving {
                    self.hold_save_failure(reason);
                } else {
                    self.project_notice.show(Notice::failed(reason));
                }
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
    pub(super) fn resolve_pending_open(&mut self, doc: &AutoCommit, choice: DiscardChoice) {
        let Some(pending) = self.pending_open.take() else {
            return;
        };
        match choice {
            DiscardChoice::Cancel => {
                self.save_then_open = None;
            }
            DiscardChoice::Discard => {
                self.load_project_file(pending.path, pending.contents.as_deref());
            }
            DiscardChoice::Save => {
                self.save_then_open = Some(pending);
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
                if self.resource_reply.is_some() {
                    self.settle_resource_job(Err(
                        "export destination selection was cancelled".into()
                    ));
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
