#[allow(
    clippy::wildcard_imports,
    reason = "this file is one part of the `VideoEditorApp` impl split out of `app.rs`; it shares the parent's whole scope"
)]
use super::*;

type Cx = ui::framework::EffectCx<Project>;

impl VideoEditorApp {
    // ---- project files ---------------------------------------------------
    //
    // The project is this app's resource: every edit is kept, versioned and
    // synced as it happens, so there is nothing to save and no unsaved state
    // to ask about. A project file is a projection of it: Export project
    // file writes the project as one wherever the person chooses, and Open
    // project file brings one in as the project, one edit Undo takes back.

    /// The project's name, as the window bar's location says it.
    pub(super) fn project_label(&self) -> String {
        if self.project.name.trim().is_empty() {
            "Untitled".to_string()
        } else {
            self.project.name.clone()
        }
    }

    /// Invoke one of the editor's effects through its door, as the palette
    /// or an agent does: a project file is a job only the effect's handler
    /// can run.
    pub(super) fn invoke_effect(&self, effect: &crate::actions::VideoEditorEffect) {
        if let Some(dispatch) = &self.actions {
            dispatch.effect(effect);
        }
    }

    /// Answer Open project file: bring in the file `path` names, or the one
    /// the person chooses.
    pub(super) fn open_project_file(
        &mut self,
        path: Option<ui::tilo_resource::AbsolutePath>,
        cx: &mut Cx,
    ) -> ui::framework::EffectAnswer {
        if let Some(path) = path {
            let path = path.into_path_buf();
            let read = crate::project_file::load(&path);
            let brought = bring_in_project(read, &path, path.parent(), cx);
            return self.show_brought_in(brought);
        }
        let request = portal::open_project_request();
        // The person answers the chooser after this effect returned: what
        // the window shows of the answer reaches it on its next sync.
        let later = self.later();
        // Where files have paths, relative media resolves beside the file;
        // elsewhere (a browser page) the chooser hands over its contents.
        if tilo_files::available(tilo_files::Feature::NativePaths) {
            return Ok(cx.choose_path(request, move |(), chosen, cx| {
                let Some(path) = chosen else {
                    return Err("project file selection was cancelled".into());
                };
                let read = crate::project_file::load(&path);
                let brought = bring_in_project(read, &path, path.parent(), cx);
                Self::show_later(&later, cx, brought)
            }));
        }
        Ok(cx.choose_file(request, move |(), chosen, cx| {
            let Some(file) = chosen else {
                return Err("project file selection was cancelled".into());
            };
            let read = crate::project_file::decode(&file.bytes);
            let brought = bring_in_project(read, Path::new(&file.name), None, cx);
            Self::show_later(&later, cx, brought)
        }))
    }

    /// What [`bring_in_project`] answered, shown on the window at its next
    /// sync.
    fn show_later(
        later: &super::hosting::Later,
        cx: &Cx,
        brought: Result<(crate::model::Project, Notice), String>,
    ) -> ui::framework::EffectAnswer {
        let answer = match &brought {
            Ok(_) => Ok(ui::framework::Effected::done()),
            Err(reason) => Err(reason.clone()),
        };
        later.run(cx, move |app| {
            // The effect was answered above; this only shows it.
            let _shown = app.show_brought_in(brought);
        });
        answer
    }

    /// Show the project a file brought in (or why it could not), and answer
    /// the effect with it.
    fn show_brought_in(
        &mut self,
        brought: Result<(crate::model::Project, Notice), String>,
    ) -> ui::framework::EffectAnswer {
        let (project, notice) = match brought {
            Ok(brought) => brought,
            Err(reason) => {
                self.project_notice.show(Notice::failed(reason.clone()));
                return Err(reason);
            }
        };
        self.project_notice.show(notice);
        self.project = project;
        self.clip_edit_bases.clear();
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
        Ok(ui::framework::Effected::done())
    }

    /// What to say after an open: how much arrived, and — if any of it cannot
    /// be read here — which sources, in the same words the bin and the
    /// timeline use for the same states.
    pub(super) fn opened_notice(path: &Path, project: &crate::model::Project) -> Notice {
        let name = crate::project_file::label(path);
        let missing = crate::project_file::missing_sources(project);
        if missing.is_empty() {
            return Notice::ok(format!("Opened {name}"));
        }
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

    /// Answer Export project file: write the project as a project file to
    /// `path`, or where the person chooses.
    pub(super) fn export_project_file(
        &mut self,
        path: Option<ui::tilo_resource::AbsolutePath>,
        cx: &mut Cx,
    ) -> ui::framework::EffectAnswer {
        let bytes = crate::project_file::encode(&self.project).map_err(|e| e.to_string())?;
        let Some(path) = path else {
            let suggested = crate::project_file::with_extension(&self.project_label());
            let later = self.later();
            return Ok(cx.save_file(
                portal::export_project_request(&suggested),
                bytes,
                move |(), saved, cx| {
                    if !saved {
                        return Err("the project file export was cancelled".into());
                    }
                    later.run(cx, |app| {
                        app.project_notice
                            .show(Notice::ok("Exported the project file"));
                    });
                    Ok(ui::framework::Effected::done())
                },
            ));
        };
        let path = path.into_path_buf();
        // A plain write rather than write-temp-then-rename: under a strict
        // sandbox the grant is for the file chosen, not for siblings beside
        // it. The header's length is what catches a write cut short.
        if let Err(e) = crate::files::save(&path, &bytes) {
            let reason = format!("Could not export: {e}");
            self.project_notice.show(Notice::failed(reason.clone()));
            return Err(reason);
        }
        self.project_notice.show(Notice::ok(format!(
            "Exported {}",
            crate::project_file::label(&path)
        )));
        Ok(ui::framework::Effected::done())
    }

    /// Ask the user where the render should land, then encode there. The
    /// picker is the grant: under a strict sandbox it is what earns the write
    /// to somewhere like `~/Videos`, and without it the file lands in a temp
    /// directory the user was never told about and cannot find.
    pub(super) fn request_export(&mut self) {
        if self.export_job.is_some() {
            return; // one render at a time
        }
        // The same door the button is painted from: a render that cannot run
        // says why in the dialog, and never asks for an output file first.
        if let Some(reason) = self.export_blocker() {
            self.export_status = ExportStatus::Failed { reason };
            return;
        }
        let name = self.export.suggested_filename(&self.project.name);
        let ext = self.export.container.extension();
        self.export_notice = Some("Choose where to save…".to_string());
        if crate::export::renders_by_steps() {
            // A page saves by contents: the file is chosen now, while the
            // press that asked for it still lets the page open its Save
            // dialog, and written when the render finishes.
            self.export_save_rx = Some(portal::request_export_save(&name, ext));
            return;
        }
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
        // A destination chosen for a render run a step at a time.
        let chosen = match self
            .export_save_rx
            .as_ref()
            .map(ui::portal::DestinationChannel::try_recv)
        {
            None | Some(Err(std::sync::mpsc::TryRecvError::Empty)) => None,
            Some(answer) => {
                self.export_save_rx = None;
                Some(answer)
            }
        };
        match chosen {
            Some(Ok(ui::portal::DestinationOutcome::Chosen(destination))) => {
                self.export_notice = None;
                self.export_status = ExportStatus::Rendering { progress: 0.0 };
                self.export_job = Some(Self::stepped_export(
                    &self.project,
                    self.export,
                    destination,
                ));
            }
            Some(Ok(ui::portal::DestinationOutcome::Cancelled)) => {
                self.export_notice = None;
                if self.resource_reply.is_some() {
                    self.settle_resource_job(Err(
                        "export destination selection was cancelled".into()
                    ));
                }
            }
            Some(Ok(ui::portal::DestinationOutcome::Failed(reason))) => {
                self.export_notice = None;
                self.export_status = ExportStatus::Failed { reason };
            }
            Some(Err(_)) => {
                self.export_notice = None;
                self.export_status = ExportStatus::Failed {
                    reason: "the file chooser closed without a result".into(),
                };
            }
            None => {}
        }
        // Running render.
        if let Some(job) = self.export_job.as_mut() {
            job.pump();
            let status = job.status();
            let finished = !matches!(status, ExportStatus::Rendering { .. });
            self.export_status = status;
            if finished {
                self.export_job = None;
            }
        }
    }

    /// A render run a step at a time, saved to `destination`.
    #[cfg(not(any(feature = "gst", feature = "decode")))]
    fn stepped_export(
        project: &crate::model::Project,
        settings: crate::export::ExportSettings,
        destination: ui::portal::SaveDestination,
    ) -> crate::export::ExportJob {
        crate::export::ExportJob::stepped(project, settings, destination)
    }

    /// A `GStreamer` build renders on a worker thread and never asks for a
    /// destination this way; one that somehow did writes where it was told.
    #[cfg(any(feature = "gst", feature = "decode"))]
    #[expect(
        clippy::needless_pass_by_value,
        reason = "the stepped build's signature, which keeps the destination"
    )]
    fn stepped_export(
        project: &crate::model::Project,
        settings: crate::export::ExportSettings,
        destination: ui::portal::SaveDestination,
    ) -> crate::export::ExportJob {
        let path = destination.location().map_or_else(
            || crate::export::output_path(&settings, project),
            Into::into,
        );
        crate::export::start_export(project, &settings, path)
    }

    /// Whether a render is in flight (so Cancel means "stop it", not "close").
    pub(super) fn export_running(&self) -> bool {
        self.export_job.is_some()
    }

    /// Whether the Export action is actually available. The dialog paints the
    /// button from this and the press path gates on it, so the offer and the
    /// behaviour cannot disagree — which they did: the dialog said "Export
    /// needs the `GStreamer` build" and put an enabled Export button under it.
    ///
    /// Footage that is not on this device refuses the offer for the same
    /// reason: the render would stop before its first frame, naming the source
    /// it never read, and a person does not need to choose an output file to
    /// learn that a clip is still arriving.
    pub(super) fn export_is_offered(&self) -> bool {
        crate::export::unavailable_reason().is_none()
            && !self.export_running()
            && self.export_blocker().is_none()
    }

    /// What stands between this project and a render, in the person's words,
    /// or `None` when Export can run. The dialog's status line shows it while
    /// idle and the Export button is withdrawn, so the two cannot disagree.
    pub(super) fn export_blocker(&self) -> Option<String> {
        if let Some(reason) = crate::export::unavailable_reason() {
            return Some(reason.to_string());
        }
        let missing = crate::export::unreadable_sources(&self.project);
        if missing.is_empty() {
            return None;
        }
        let names: Vec<&str> = missing.iter().map(|(name, _)| name.as_str()).collect();
        let it = if names.len() == 1 { "it" } else { "them" };
        Some(format!(
            "This cut refers to {}. Bring {it} onto this device first.",
            names.join(", "),
        ))
    }
}

/// Whether `project` holds nothing a user would mind losing.
fn holds_nothing(project: &crate::model::Project) -> bool {
    project.clips.is_empty() && project.media.is_empty() && project.tracks.is_empty()
}

/// Replace the project with what the file at `path` held (`read`), its
/// relative media found in `dir` when the file has one here: the project
/// and what to say about it, or why it could not be read.
///
/// The replacement is one edit of the resource, so it is one undo step and
/// replicates to a peer like any other.
fn bring_in_project(
    read: Result<crate::model::Project, crate::project_file::ProjectFileError>,
    path: &Path,
    dir: Option<&Path>,
    cx: &Cx,
) -> Result<(crate::model::Project, Notice), String> {
    let mut project = read.map_err(|e| e.to_string())?;
    if let Some(dir) = dir {
        crate::project_file::anchor_media(&mut project, dir);
    }
    // The name follows the file unless the file's own project carries a
    // real one — a project still called "Untitled" opened from
    // `Holiday.qvp` should read as Holiday.
    if (project.name.trim().is_empty() || project.name == "Untitled")
        && let Some(n) = crate::project_file::name_from_path(path)
    {
        project.name = n;
    }
    // An open that throws a timeline away says the way back, because an
    // accidental one otherwise feels final when it is not.
    let replaced = !holds_nothing(&cx.live().get());
    let mut notice = VideoEditorApp::opened_notice(path, &project);
    if replaced {
        notice.text.push_str(". Undo to go back");
        notice.action = Some(super::notices::NoticeAction::Undo);
    }
    cx.submit(crate::resource::load_project(&project));
    Ok((project, notice))
}
