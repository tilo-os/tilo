#[allow(
    clippy::wildcard_imports,
    reason = "this file is one part of the `VideoEditorApp` impl split out of `app.rs`; it shares the parent's whole scope"
)]
use super::*;

type Cx = ui::framework::EffectCx<Project>;

/// The export's job: its completions get the window.
type ExportCx = ui::framework::EffectCx<Project, VideoEditorApp>;

/// The one export the window runs at a time: choosing where, rendering, and writing.
const EXPORTING: &str = "video-editor-export";

impl VideoEditorApp {
    // ---- project files ---------------------------------------------------
    //
    // Every edit is kept, versioned and synced as it happens, so there is nothing to save.
    // A project file is a projection: Export writes one wherever the person chooses, and
    // Open brings one in as the project, one edit Undo takes back.

    pub(super) fn project_label(&self) -> String {
        if self.project.name.trim().is_empty() {
            crate::resource::CATALOG.t("video-editor-untitled")
        } else {
            self.project.name.clone()
        }
    }

    /// As the palette or an agent does: a project file is a job only the effect's handler
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

    /// How much arrived, and — if any of it cannot be read here — which sources, in the words
    /// the bin and the timeline use for the same states.
    pub(super) fn opened_notice(path: &Path, project: &crate::model::Project) -> Notice {
        let name = crate::project_file::label(path);
        let missing = crate::project_file::missing_sources(project);
        let named = crate::resource::CATALOG.t_args(
            "video-editor-opened",
            &[("name", ui::i18n::ArgValue::Text(name.clone()))],
        );
        if missing.is_empty() {
            return Notice::ok(named);
        }
        let absent = missing.len();
        let sources: Vec<&str> = missing.iter().take(3).map(|(n, _)| n.as_str()).collect();
        let listed = sources.join(", ");
        let tail = if absent > sources.len() {
            crate::resource::CATALOG.t_args(
                "video-editor-import-more",
                &[
                    ("why", ui::i18n::ArgValue::Text(listed)),
                    (
                        "hidden",
                        ui::i18n::ArgValue::Number((absent - sources.len()) as f64),
                    ),
                ],
            )
        } else {
            listed
        };
        // Opened, but not everything came with it: a warning, not a success
        // and not a failure.
        let state = crate::resource::CATALOG.t(MEDIA_NOT_LOCAL);
        Notice::warning(crate::resource::CATALOG.t_args(
            "video-editor-opened-partly",
            &[
                ("name", ui::i18n::ArgValue::Text(name)),
                ("state", ui::i18n::ArgValue::Text(state)),
                ("sources", ui::i18n::ArgValue::Text(tail)),
            ],
        ))
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
            let suggested = crate::project_file::with_extension(
                &self.project_label(),
                &crate::resource::CATALOG.t("video-editor-untitled"),
            );
            let later = self.later();
            return Ok(cx.save_file(
                portal::export_project_request(&suggested),
                bytes,
                move |(), saved, cx| {
                    if !saved {
                        return Err("the project file export was cancelled".into());
                    }
                    later.run(cx, |app| {
                        app.project_notice.show(Notice::ok(
                            crate::resource::CATALOG.t("video-editor-exported-project"),
                        ));
                    });
                    Ok(ui::framework::Effected::done())
                },
            ));
        };
        let path = path.into_path_buf();
        // A plain write rather than write-temp-then-rename: under a strict sandbox the grant
        // is for the file chosen, not for siblings beside it. The header's length catches a
        // write cut short.
        if let Err(e) = crate::files::save(&path, &bytes) {
            let reason = crate::resource::CATALOG.t_args(
                "video-editor-export-failed",
                &[("reason", ui::i18n::ArgValue::Text(e.to_string()))],
            );
            self.project_notice.show(Notice::failed(reason.clone()));
            return Err(reason);
        }
        self.project_notice
            .show(Notice::ok(crate::resource::CATALOG.t_args(
                "video-editor-exported-name",
                &[(
                    "name",
                    ui::i18n::ArgValue::Text(crate::project_file::label(&path)),
                )],
            )));
        Ok(ui::framework::Effected::done())
    }

    /// The picker is the grant: under a strict sandbox it is what earns the write to somewhere
    /// like `~/Videos`, and without it the file lands in a temp directory the user cannot
    /// find. The chooser opens inside the press, which a page's Save dialog needs; the
    /// render runs once the person chose.
    pub(super) fn request_export(&mut self) {
        if self.export_job.is_some() || self.choosing_export() {
            return; // one render at a time
        }
        // The same api the button is painted from: a render that cannot run
        // says why in the dialog, and never asks for an output file first.
        if let Some(reason) = self.export_blocker() {
            self.export_status = ExportStatus::Failed { reason };
            return;
        }
        let Some(jobs) = self.jobs.clone() else {
            return;
        };
        let request = portal::export_request(
            &self.export.suggested_filename(&self.project.name),
            self.export.container.extension(),
        );
        self.export_status = ExportStatus::Idle;
        self.export_notice = Some(crate::resource::CATALOG.t("video-editor-save-where"));
        jobs.start::<Self>(EXPORTING, move |cx| {
            Ok(cx.choose_destination(request, |app, chosen, cx| {
                app.export_notice = None;
                match chosen {
                    Some(destination) => Ok(app.export_to(destination, cx)),
                    None => Ok(ui::framework::Effected::done()),
                }
            }))
        });
    }

    /// Whether the person is choosing where an export goes, or a render
    /// waits to be written there.
    pub(super) fn choosing_export(&self) -> bool {
        self.jobs
            .as_ref()
            .is_some_and(|jobs| jobs.is_running(EXPORTING))
    }

    /// Stop choosing where an export goes; a render already running goes on.
    pub(super) fn stop_choosing_export(&self) {
        if let Some(jobs) = &self.jobs {
            jobs.cancel(EXPORTING);
        }
    }

    /// Render to the file the person chose, on a worker thread.
    #[cfg(any(feature = "gst", feature = "decode"))]
    #[expect(
        clippy::needless_pass_by_value,
        reason = "a render by steps keeps the destination to write it later"
    )]
    fn export_to(
        &mut self,
        destination: ui::portal::SaveDestination,
        _cx: &mut ExportCx,
    ) -> ui::framework::Effected {
        let path = destination.location().map_or_else(
            || crate::export::output_path(&self.export, &self.project),
            Path::to_path_buf,
        );
        self.begin_export_to(path);
        ui::framework::Effected::done()
    }

    /// Render a step at a time each frame, then write the file where the
    /// person chose: the job waits for both.
    #[cfg(not(any(feature = "gst", feature = "decode")))]
    fn export_to(
        &mut self,
        destination: ui::portal::SaveDestination,
        cx: &mut ExportCx,
    ) -> ui::framework::Effected {
        let (job, rendered) = crate::export::ExportJob::stepped(&self.project, self.export);
        self.export_status = ExportStatus::Rendering { progress: 0.0 };
        self.export_job = Some(job);
        cx.wait_for(rendered, move |_, rendered, cx| {
            // Without a file the render stopped, and its status says why.
            let Some(rendered) = rendered else {
                return Ok(ui::framework::Effected::done());
            };
            let name = if rendered.without_sound {
                format!("{} (picture only, without sound)", destination.name())
            } else {
                destination.name().to_owned()
            };
            Ok(
                cx.write_destination(&destination, rendered.bytes, move |app, written, _| {
                    app.export_job = None;
                    app.export_status = match written {
                        Ok(()) => ExportStatus::Done { path: name },
                        Err(why) => ExportStatus::Failed {
                            reason: format!("The video could not be saved: {why}"),
                        },
                    };
                    Ok(ui::framework::Effected::done())
                }),
            )
        })
    }

    pub(super) fn begin_export_to(&mut self, destination: PathBuf) {
        self.export_notice = None;
        self.export_status = ExportStatus::Rendering { progress: 0.0 };
        self.export_job = Some(crate::export::start_export(
            &self.project,
            &self.export,
            destination,
        ));
    }

    /// Every frame: `needs_animation_frame` keeps frames coming while a job runs, so the
    /// progress bar moves.
    pub(super) fn poll_export(&mut self) {
        // A chooser that failed ended the job and said why on the window's bar.
        if !self.choosing_export() {
            self.export_notice = None;
        }
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

    /// Whether a render is in flight (so Cancel means "stop it", not "close").
    pub(super) fn export_running(&self) -> bool {
        self.export_job.is_some()
    }

    /// The dialog paints the button from this and the press path waits for it, so the offer and
    /// the behaviour cannot disagree, as they did when the dialog said "Export needs the
    /// `GStreamer` build" over an enabled Export button.
    ///
    /// Footage that is not on this device refuses for the same reason: the render would stop
    /// before its first frame, and nobody needs to choose an output file to learn that.
    pub(super) fn export_is_offered(&self) -> bool {
        crate::export::unavailable_reason().is_none()
            && !self.export_running()
            && self.export_blocker().is_none()
    }

    /// Shown while idle, and the Export button is withdrawn, so the two cannot disagree.
    pub(super) fn export_blocker(&self) -> Option<String> {
        if let Some(reason) = crate::export::unavailable_reason() {
            return Some(reason.to_string());
        }
        let missing = crate::export::unreadable_sources(&self.project);
        if missing.is_empty() {
            return None;
        }
        let names: Vec<&str> = missing.iter().map(|(name, _)| name.as_str()).collect();
        Some(crate::resource::CATALOG.t_args(
            "video-editor-export-missing-sources",
            &[("sources", ui::i18n::ArgValue::Text(names.join(", ")))],
        ))
    }
}

fn holds_nothing(project: &crate::model::Project) -> bool {
    project.clips.is_empty() && project.media.is_empty() && project.tracks.is_empty()
}

/// The project and what to say about it, or why it could not be read.
///
/// The replacement is one edit of the resource, so it is one undo step and replicates like
/// any other.
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
    // The name follows the file unless the file's project carries a real one: a project still
    // called "Untitled" opened from `Holiday.qvp` should read as Holiday.
    let untitled = crate::resource::CATALOG.t("video-editor-untitled");
    if (project.name.trim().is_empty() || project.name == untitled || project.name == "Untitled")
        && let Some(n) = crate::project_file::name_from_path(path)
    {
        project.name = n;
    }
    // An open that throws a timeline away says the way back, because an
    // accidental one otherwise feels final when it is not.
    let replaced = !holds_nothing(&cx.live().get());
    let mut notice = VideoEditorApp::opened_notice(path, &project);
    if replaced {
        // The notice carries the way back as its Undo action; saying so in words would tell a
        // person how to use the button in front of them.
        notice.action = Some(super::notices::NoticeAction::Undo);
    }
    cx.submit(crate::resource::load_project(&project));
    Ok((project, notice))
}
