//! One mount's frames: what a frame's scene repaints, and the two ways it
//! reaches the screen. A frame that the GPU lost, or that the GPU could not
//! take at all, is laid out here and drawn on the CPU
//! ([`super::software::Software`]) until the GPU may be tried again
//! ([`crate::gpu_recovery`]); no failure of the GPU's ends the page.

use ui::headless::FrameHarness;
use wasm_bindgen::JsValue;

use super::{FrameChange, Mount, Retained, Viewport, presence, release_gpu, software, unconfigure};

/// Why a frame was not drawn: the GPU could not draw it, and is made again
/// or drawn around ([`crate::gpu_recovery`]), or something else stopped it,
/// which the page says on its failure screen.
pub(crate) enum DrawError {
    Gpu(String),
    Other(String),
}

impl From<String> for DrawError {
    fn from(error: String) -> Self {
        Self::Other(error)
    }
}

impl From<&str> for DrawError {
    fn from(error: &str) -> Self {
        Self::Other(error.to_owned())
    }
}

/// One frame as the retained target's next repaint: what of the frame's scene
/// is drawn, where it is placed, and what of the target it repaints.
pub(super) struct RepaintPlan {
    /// How the target places the frame, and which frame it holds.
    pub(super) key: ui::retained_raster::TargetKey,
    /// What of the target this frame repaints.
    pub(super) repaint: ui::retained_raster::Repaint,
    /// Moved before the damage is repainted.
    copies: Vec<ui::scroll_copy::DeviceCopy>,
    /// The frame's scene placed on the target, cut to what its damage reaches
    /// where that can be proven.
    pub(super) physical: vello::Scene,
    /// Whether `physical` was cut: the check compares against the frame whole.
    pub(super) culled: bool,
    /// This target draws no backdrops.
    backdrop: ui::backdrop::BackdropFrame,
}

impl RepaintPlan {
    pub(super) fn content(&self) -> ui::retained_raster::FrameContent<'_> {
        let (damage, full_redraw) = match &self.repaint {
            ui::retained_raster::Repaint::Full(_) => (&[][..], true),
            ui::retained_raster::Repaint::Regions(regions) => (regions.as_slice(), false),
        };
        ui::retained_raster::FrameContent {
            scene: &self.physical,
            width: self.key.width,
            height: self.key.height,
            base_color: self.key.base_color,
            damage_regions: damage,
            scroll_copies: &self.copies,
            full_redraw,
            backdrop: &self.backdrop,
        }
    }
}

/// The frame `harness` laid out, as `retained`'s target's next repaint:
/// `on_cpu` says the frames rasterize on the CPU until the renderer's
/// pipelines are compiled, which forgets what the target held. Returns the
/// frame's parts, which a presented frame keeps.
pub(super) fn repaint_plan<'a>(
    retained: &mut Retained,
    harness: &'a FrameHarness,
    viewport: Viewport,
    on_cpu: bool,
) -> (Option<&'a ui::scene_parts::SectionParts>, RepaintPlan) {
    retained.switch_raster(on_cpu);
    let frame = harness.frame_render_input();
    let key = ui::retained_raster::TargetKey {
        width: viewport.width,
        height: viewport.height,
        to_device: ui::scale::composite_transform(viewport.dpr),
        base_color: frame.background,
        generation: retained.generation,
    };
    let parts = harness.scene_parts();
    // Compared with the frame the target shows, command by command: a
    // change no part proves is bounded (an unknown one) repaints it all.
    let (repaint, copies) = retained.frames.repaint_copying(frame.scene, parts, key);
    let damage = match &repaint {
        ui::retained_raster::Repaint::Full(_) => &[][..],
        ui::retained_raster::Repaint::Regions(regions) => regions.as_slice(),
    };
    // A small damage renders only the parts that can reach it.
    let culled = ui::retained_raster::culled_for_damage(frame.scene, parts, key, damage);
    let mut physical = vello::Scene::new();
    physical.append(culled.as_ref().unwrap_or(frame.scene), Some(key.to_device));
    (
        parts,
        RepaintPlan {
            key,
            repaint,
            copies,
            physical,
            culled: culled.is_some(),
            backdrop: ui::backdrop::BackdropFrame::default(),
        },
    )
}

impl Mount {
    /// The frame is on screen: what every presentation counts, and says.
    pub(super) fn presented(&mut self, change: FrameChange, reasons: Option<Vec<String>>) {
        self.viewport.did_present();
        self.pending = false;
        self.frames = self.frames.saturating_add(1);
        if !self.hidden
            && let Some(desktop) = &self.desktop
        {
            desktop.shared.presented.set(true);
        }
        if let Some(reasons) = reasons {
            self.metrics.borrow_mut().presented(reasons);
        }
        let stage = self.metric_start();
        self.presentation.set(true);
        self.publish_frame(0, change, true);
        self.presentation.set(false);
        self.metric_end("publish", stage);
    }

    /// A frame failed with `error`: the GPU's failures are made good
    /// ([`Self::gpu_failed`]), and anything else stops the desktop, which the
    /// page says on its failure screen.
    pub(super) fn frame_failed(&mut self, id: u64, error: DrawError) {
        match error {
            DrawError::Gpu(error) => self.gpu_failed(id, &error),
            DrawError::Other(error) => {
                self.error = Some(error);
                self.publish(id);
            }
        }
    }

    /// The GPU could not draw a frame, `error` saying how: the device, surface
    /// and renderer are made again and the shell redraws on them, keeping
    /// every document. A GPU that keeps failing is not chased for ever: the
    /// mount draws its frames on the CPU, on a canvas of its own over the
    /// mount's, and tries the GPU again when
    /// [`crate::gpu_recovery::GpuRecovery::retry`] says. Neither stops the
    /// desktop.
    pub(super) fn gpu_failed(&mut self, id: u64, error: &str) {
        let lived = self
            .gpu
            .as_ref()
            .and_then(|gpu| gpu.shared.try_borrow().ok().map(|shared| shared.made))
            .map_or(ui::time::Duration::ZERO, |made| {
                ui::time::now().saturating_duration_since(made)
            });
        if self.gpu_recovery.failed(ui::time::now(), lived) == crate::gpu_recovery::Next::Remake {
            let other = crate::gpu_recovery::device_lost(lived);
            let on = if other {
                "on the other GPU"
            } else {
                "on the same GPU"
            };
            web_sys::console::warn_1(&JsValue::from_str(&format!(
                "Browser runtime: the GPU failed ({error}); making it again {on} and drawing on"
            )));
            self.remake_gpu(id);
            return;
        }
        web_sys::console::warn_1(&JsValue::from_str(&format!(
            "Browser runtime: the GPU cannot draw ({error}); drawing on the CPU until it is tried again"
        )));
        // The canvas would keep showing the last frame the lost device
        // presented, which may be half drawn: the overlay covers it until a
        // GPU draws there again.
        unconfigure(&self.canvas);
        if let Some(gpu) = self.gpu.take() {
            release_gpu(id, gpu);
        }
        if self.soft.is_none() {
            match software::Software::new(&self.canvas) {
                Ok(soft) => self.soft = Some(soft),
                // The page has no second canvas to draw on: this is not the
                // GPU's failure, and the person is told.
                Err(error) => {
                    self.error = Some(format!("the page cannot draw the desktop: {error}"));
                    self.publish(id);
                    return;
                }
            }
        }
        self.pending = true;
        self.window.request_redraw();
    }

    /// When this mount, drawing on the CPU, tries its GPU again.
    pub(super) fn soft_retry(&self) -> Option<ui::time::Instant> {
        if self.gpu.is_some() || self.soft.is_none() {
            return None;
        }
        self.gpu_recovery.retry()
    }

    /// Draw the frame on the CPU and put its pixels on the page
    /// ([`software::Software`]): what a mount does while its GPU cannot draw.
    pub(super) fn draw_software(&mut self) -> Result<(), DrawError> {
        let mut reasons = self.metrics.borrow().enabled().then(|| {
            let mut reasons = Vec::new();
            if self.pending {
                reasons.push("input or a wake".to_owned());
            }
            reasons
        });
        self.frame_due(&mut reasons);
        if !self.pending {
            if !self.skipped_frame {
                self.held = "nothing to draw";
            }
            return Ok(());
        }
        // The GPU may be tried again: its device is made behind the overlay,
        // which stays until a frame is drawn on it.
        if self.gpu_recovery.try_due(ui::time::now()) {
            self.remake_gpu(self.id);
            return Ok(());
        }
        let viewport = self.viewport.requested();
        let mut unsettled = None;
        if self.viewport.needs_apply() {
            self.frame.set_input_scale(viewport.dpr)?;
            unsettled = self.frame.resize(viewport.logical_size()).err();
            self.viewport.did_apply();
        }
        let (change, unsettled) = self.lay_out(unsettled, &mut reasons);
        if let Some(error) = unsettled {
            if let Some(line) = self.frame.skip_frame(&error) {
                web_sys::console::error_1(&JsValue::from_str(&line));
            }
            self.pending = !self.skipped_frame;
            self.skipped_frame = true;
            self.held = "a layout that did not settle";
            return Ok(());
        }
        self.skipped_frame = false;
        let stage = self.metric_start();
        let Some(soft) = self.soft.as_mut() else {
            self.held = "no surface";
            return Ok(());
        };
        let (parts, plan) = repaint_plan(&mut soft.retained, &self.frame, viewport, true);
        if let Err(error) = soft.retained.raster.rasterize_to_cpu(plan.content()) {
            soft.retained.forget();
            return Err(DrawError::Gpu(error));
        }
        soft.retained
            .frames
            .presented(self.frame.frame_render_input().scene, parts, plan.key);
        soft.present().map_err(DrawError::from)?;
        self.metric_end("render:cpu", stage);
        self.presented(change, reasons);
        Ok(())
    }

    /// Whether a frame is due, and why (when frame metrics are on): a wake
    /// from the page, the viewport, the frame's own work and schedule, and who is here.
    /// What only one presentation adds — a frame the GPU lost, pixels an app
    /// asked for — is its own.
    pub(super) fn frame_due(&mut self, reasons: &mut Option<Vec<String>>) {
        // Host wakes are consumed only after acquiring a surface. A skipped
        // acquisition must retain them for the next redraw.
        self.pending |= Self::desktop_wants_frame(self.desktop.as_ref());
        if let Some(peer) = self.peer {
            crate::replicated::follow_answers(peer);
        }
        self.pending |=
            self.replica_changed.get() || self.theme_seen != crate::page_theme::generation();
        let frame = self.frame.frame_schedule();
        let due = frame
            .next_wake_deadline
            .is_some_and(|wake| wake <= ui::time::now());
        if let Some(reasons) = reasons {
            if self.viewport.needs_present() {
                reasons.push("viewport".into());
            }
            if frame.queued_work {
                let sources = self.frame.queued_work_sources();
                if sources.is_empty() {
                    reasons.push("queued work".into());
                }
                reasons.extend(
                    sources
                        .into_iter()
                        .map(|source| format!("queued: {source}")),
                );
            }
            if frame.needs_animation_frame || due {
                reasons.extend(
                    self.frame
                        .frame_requesters()
                        .into_iter()
                        .map(|(path, demand)| match demand {
                            ui::headless::FrameDemand::Animation => format!("animation: {path}"),
                            ui::headless::FrameDemand::Deadline => format!("deadline: {path}"),
                        }),
                );
            }
        }
        self.pending |= self.viewport.needs_present()
            || frame.queued_work
            || frame.needs_animation_frame
            || due;
        // Who is here changed on the board: the frame puts them on the desktop, whose people
        // lists read them, even when nothing else on the page moved.
        if !self.pending && presence::moved(self.peer, &self.workspace, self.presence.revision) {
            if let Some(reasons) = reasons {
                reasons.push("presence".into());
            }
            self.pending = true;
        }
    }

    /// Lay out the frame at the viewport just applied: who is here, the theme,
    /// a peer's change and the widgets. `unsettled` is why the viewport could
    /// not be laid out. Returns what the frame changed, and why it did not
    /// settle: the last frame stays on screen then, and the next lays out
    /// again at once.
    pub(super) fn lay_out(
        &mut self,
        mut unsettled: Option<ui::widget::WidgetLayoutError>,
        reasons: &mut Option<Vec<String>>,
    ) -> (FrameChange, Option<ui::widget::WidgetLayoutError>) {
        let viewport = self.viewport.requested();
        let passes_before = self.frame.relayout_frames();
        let mut host_changed =
            self.follow_desktop(f64::from(viewport.logical_size().0), reasons.as_mut());
        // The person's settings or their browser's preferences changed
        // what the theme is: every window draws in the new one.
        if self.theme_seen != crate::page_theme::generation() {
            self.theme_seen = crate::page_theme::generation();
            self.frame.set_theme(crate::page_theme::current());
            host_changed = true;
        }
        // A peer's change merged into an open record.
        if self.replica_changed.replace(false) {
            if let Some(reasons) = &mut *reasons {
                reasons.push("replica change".into());
            }
            host_changed = true;
        }
        // Who is here, placed in the windows this frame lays out.
        self.place_here();
        self.place_presence();
        // Whom this page's person follows, told to the windows' apps as a native shell's apps
        // hear it: the window of theirs shows where they work.
        if let Some(desktop) = &self.desktop {
            crate::follow::tell(
                &mut desktop.shared.follow_to_tell.borrow_mut(),
                &mut self.frame,
            );
        }
        // A frame whose semantics will be built draws every window: one it
        // replayed instead (a pan frame may) would have to be drawn again
        // before its tree could be read.
        let tree_due = host_changed
            || self.entries != self.tree_entries.get()
            || !self.semantics.borrow().would_defer_unbuilt(ui::time::now());
        self.frame.set_pan_replay(!tree_due);
        // Poll only after applying the new viewport, avoiding an old-size
        // layout merely to discover work.
        let passes_resized = self.frame.relayout_frames();
        let stage = self.metric_start();
        let tooltip_before = self.frame.drawn_tooltip().map(|(text, _)| text.to_owned());
        let polled = match self.frame.poll_frame() {
            Ok(polled) => polled,
            Err(error) => {
                unsettled = Some(error);
                false
            }
        };
        let stats = self.frame.last_relayout();
        // A tooltip that came or went is in the tree a reader is told.
        let tooltip_changed =
            self.frame.drawn_tooltip().map(|(text, _)| text) != tooltip_before.as_deref();
        self.metric_end(
            match (polled, stats.rebuilt, stats.measured) {
                (false, ..) => "poll_frame:idle",
                (true, true, _) => "poll_frame:rebuilt",
                (true, false, true) => "poll_frame:measured",
                (true, false, false) => "poll_frame:applied",
            },
            stage,
        );
        if polled {
            self.note_measured();
        }
        // A frame that laid nothing out shows a reader what the last one
        // did; one that only re-applied positions moved laid-out content
        // and nothing else, so its tree's structure and focus are the last
        // one's. Either needs no host change, and the tree also no call
        // since it was built that the frame cannot see (`publish_frame`).
        let change = if host_changed || tooltip_changed || passes_resized != passes_before {
            FrameChange::Other
        } else if !polled && self.frame.relayout_frames() == passes_resized {
            FrameChange::Nothing
        } else if polled && !stats.rebuilt && !stats.measured {
            FrameChange::Moved
        } else {
            FrameChange::Other
        };
        if host_changed && !polled && unsettled.is_none() {
            let stage = self.metric_start();
            unsettled = self.frame.relayout_at(viewport.logical_size()).err();
            self.metric_end("wake_relayout", stage);
        }
        let stage = self.metric_start();
        self.save_desktop();
        self.metric_end("save_desktop", stage);
        if let Some(peer) = self.peer {
            let stage = self.metric_start();
            crate::replicated::publish(peer);
            self.metric_end("replicate", stage);
        }
        // A frame input or an idle pass ran and skipped reports here.
        if let Some(line) = self.frame.take_skip_report() {
            web_sys::console::error_1(&JsValue::from_str(&line));
        }
        (change, unsettled)
    }
}
