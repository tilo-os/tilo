//! The editor as the window of its bound view: what it takes from the view
//! when it is built, how it follows the document, and what it leaves for the
//! view's effect handler and publications to read.

#[allow(
    clippy::wildcard_imports,
    reason = "this file is one part of the `VideoEditorApp` impl split out of `app.rs`; it shares the parent's whole scope"
)]
use super::*;

use std::cell::RefCell;
use std::rc::Rc;

use ui::framework::{EffectAnswer, EffectCx, EffectStatus, RunMode, ViewCx};

use crate::actions::VideoEditorEffect;

type Cx = EffectCx<Project>;

/// What finishes after the effect that started it returned (a chooser the
/// person answered) or arrives outside the window (a capture's stage), for
/// the window, which an effect handler holds only while the effect starts:
/// it runs on the window's next sync.
#[derive(Clone, Default)]
pub(crate) struct Later(Rc<RefCell<Vec<Work>>>);

/// One piece of work [`Later`] holds for the window.
type Work = Box<dyn FnOnce(&mut VideoEditorApp)>;

impl Later {
    /// Run `work` on the window at its next sync, which `cx` wakes it for.
    pub(super) fn run(&self, cx: &Cx, work: impl FnOnce(&mut VideoEditorApp) + 'static) {
        self.push(work);
        cx.waker().wake();
    }

    fn push(&self, work: impl FnOnce(&mut VideoEditorApp) + 'static) {
        self.0.borrow_mut().push(Box::new(work));
    }

    fn take(&self) -> Vec<Work> {
        std::mem::take(&mut *self.0.borrow_mut())
    }
}

/// What the view publishes beside the project, as the window last showed
/// it: the playhead and measured mix the edits bind to, and whether a
/// render runs for Cancel export.
#[derive(Default)]
pub(crate) struct Published {
    playhead: f64,
    mix: crate::audio::MixSnapshot,
    exporting: bool,
}

impl Published {
    /// The edits bound to the playhead and the measured mix over `project`.
    pub(crate) fn declarations(
        &self,
        project: &Project,
    ) -> Vec<ui::tilo_resource::ActionDecl<Project>> {
        crate::actions::surface_declarations(&VideoResourceCtx {
            project: project.clone(),
            playhead: self.playhead,
            mix: self.mix.clone(),
        })
    }

    /// Export needs an encoder in this build; cancelling needs a render
    /// running.
    pub(crate) fn effect_statuses(&self) -> Vec<EffectStatus> {
        let mut statuses = Vec::new();
        if let Some(reason) = crate::export::unavailable_reason() {
            statuses.push(EffectStatus::unavailable(
                VideoEditorEffect::EXPORT,
                "export_unavailable",
                reason,
            ));
            statuses.push(EffectStatus::unavailable(
                VideoEditorEffect::RENDER_TIMELINE,
                "export_unavailable",
                reason,
            ));
        }
        if !self.exporting {
            statuses.push(EffectStatus::unavailable(
                VideoEditorEffect::CANCEL_EXPORT,
                "no_export_running",
                "No export is running",
            ));
        }
        statuses
    }
}

impl VideoEditorApp {
    /// The editor `cx`'s view lays out. A launch naming files imports them,
    /// the way a file chooser's pick does, and places the first on the
    /// timeline. A screenshot or a past version of anything else is
    /// hermetic.
    pub(crate) fn hosted(cx: &ViewCx<Project>) -> Self {
        let mut app = Self::new();
        app.actions = Some(cx.dispatcher());
        app.content = cx.content();
        app.live = Some(cx.live().clone());
        let mode = cx.mode();
        app.seed.capture = mode == RunMode::Capture;
        let launched: Vec<PathBuf> = cx.launch_arguments().iter().map(PathBuf::from).collect();
        if launched.is_empty() {
            app.seed.hermetic = !mode.is_live();
            app.thumbnails.set_decoding(!app.seed.hermetic);
            // A still render cannot wait on a background waveform compute.
            app.render_pipeline.warm_waveforms_sync = app.seed.capture;
        } else {
            app.pending_imports = launched;
            app.media_pipeline.auto_place_imports = true;
        }
        app.follow_document();
        app
    }

    /// What the view's publications read of this window.
    pub(crate) fn published(&self) -> Rc<RefCell<Published>> {
        Rc::clone(&self.published)
    }

    /// Where work for this window goes from outside it.
    pub(crate) fn later(&self) -> Later {
        self.later.clone()
    }

    /// Show the project the document holds when it changed.
    pub(super) fn follow_document(&mut self) {
        let Some(project) = self
            .live
            .as_ref()
            .and_then(|live| live.changed_since(&mut self.presented))
        else {
            return;
        };
        self.present(&project);
    }

    /// The stored project changed: a local edit, a collaborator's, an undo
    /// — or, in History mode, a past version. A drag in flight keeps its
    /// live preview; the new project is loaded when it lets go, exactly as
    /// for the standalone editor.
    pub(crate) fn present(&mut self, project: &Rc<Project>) {
        self.stored = Rc::clone(project);
        if self.drag_is_editing() {
            self.project_sync.reload_pending = true;
            return;
        }
        self.project = Project::clone(project);
        self.clip_edit_bases.clear();
        self.project_sync.reload_pending = false;
        #[cfg(any(feature = "gst", feature = "decode"))]
        {
            self.media_pipeline.monitor_stale = true;
        }
    }

    /// Run what was left for the window since its last sync.
    pub(super) fn run_later(&mut self) {
        for work in self.later.take() {
            work(self);
        }
    }

    /// Bring what the view publishes in step with the window.
    pub(crate) fn publish_shown(&self) {
        let mut published = self.published.borrow_mut();
        published.playhead = self.playhead;
        // The copy `draw` reads, so the palette cannot report a level the
        // meter on screen disagrees with.
        published.mix = self.mix.clone();
        published.exporting = self.export_job.is_some();
    }

    /// Answer `effect` on the window as it shows the document now.
    pub(crate) fn effect(&mut self, effect: VideoEditorEffect, cx: &mut Cx) -> EffectAnswer {
        self.follow_document();
        let answer = crate::agent_timeline::effect(&effect, self.content.as_ref(), cx)
            .unwrap_or_else(|| self.answer_effect(effect, cx));
        // Publications read what the effect changed before the next sync.
        self.publish_shown();
        answer
    }

    /// A `--shots` capture: the scenario's project is loaded; arrange the
    /// editor around it on its next sync. See [`Self::stage_capture`].
    #[cfg(feature = "samples")]
    pub(crate) fn stage_later(later: &Later, scenario: Option<&str>) {
        let scenario = scenario.map(str::to_owned);
        later.push(move |app| app.stage_capture(scenario.as_deref()));
    }
}
