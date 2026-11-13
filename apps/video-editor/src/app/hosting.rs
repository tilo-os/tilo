//! The editor as the window of its bound view: what it takes from the view when built, how
//! it follows the document, and what it leaves for the view's effect handler and
//! publications.

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

/// What finishes after the effect that started it returned, or arrives outside the window,
/// for the window, which an effect handler holds only while the effect starts: it runs on
/// the window's next sync.
#[derive(Clone, Default)]
pub(crate) struct Later(Rc<RefCell<Vec<Work>>>);

type Work = Box<dyn FnOnce(&mut VideoEditorApp)>;

impl Later {
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

/// The playhead and measured mix the edits bind to, and whether a render runs for
/// Cancel export.
#[derive(Default)]
pub(crate) struct Published {
    playhead: f64,
    mix: crate::audio::MixSnapshot,
    exporting: bool,
}

impl Published {
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

    pub(crate) fn published(&self) -> Rc<RefCell<Published>> {
        Rc::clone(&self.published)
    }

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

    /// A drag in flight keeps its live preview; the new project is loaded when it lets go.
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

    pub(super) fn run_later(&mut self) {
        for work in self.later.take() {
            work(self);
        }
    }

    pub(crate) fn publish_shown(&self) {
        let mut published = self.published.borrow_mut();
        published.playhead = self.playhead;
        published.mix = self.mix.clone();
        published.exporting = self.export_job.is_some();
    }

    pub(crate) fn effect(&mut self, effect: VideoEditorEffect, cx: &mut Cx) -> EffectAnswer {
        self.follow_document();
        let answer = crate::agent_timeline::effect(&effect, self.content.as_ref(), cx)
            .unwrap_or_else(|| self.answer_effect(effect, cx));
        // Publications read what the effect changed before the next sync.
        self.publish_shown();
        answer
    }

    #[cfg(feature = "samples")]
    pub(crate) fn stage_later(later: &Later, scenario: Option<&str>) {
        let scenario = scenario.map(str::to_owned);
        later.push(move |app| app.stage_capture(scenario.as_deref()));
    }
}
