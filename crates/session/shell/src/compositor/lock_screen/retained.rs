//! Only the credential parts — the password field and the status row — may
//! change over a retained lock image. The complete scene remains
//! authoritative; an unproven difference (the clock, artwork, media card,
//! keyboard, an alert, the theme) always repaints it all.

use super::{LockFeedback, LockOutput, LockView};
use crate::shell::frame::{FrameBuilder, FramePaint};
use crate::shell::osd::Osd;
use ui::scene_parts::SectionParts;
use vello::Scene;

struct Previous {
    scene: Scene,
    parts: SectionParts,
    revision: u64,
    outputs: Vec<LockOutput>,
}

pub(crate) struct LockComposition<'a> {
    pub outputs: &'a [LockOutput],
    pub theme: &'a ui::theme::Theme,
    pub view: &'a LockView<'a>,
    pub wallpaper: Option<&'a vello::peniko::ImageData>,
    pub osd: Option<Osd>,
    pub now: std::time::Instant,
}

#[derive(Default)]
pub(crate) struct RetainedLockScene {
    previous: Option<Previous>,
    /// The answer transitions of this lock session: when a verification or a
    /// refusal arrived, so the frame that shows it can paint the next phase.
    feedback: LockFeedback,
}

impl RetainedLockScene {
    /// Both lock edges are boundaries, even if no unlocked frame was painted.
    pub(crate) fn clear(&mut self) {
        self.previous = None;
        self.feedback.reset();
    }

    /// Whether the answer in flight needs its next frame at `now`, including
    /// the frame that paints it at rest.
    ///
    /// The lock's frames are otherwise filtered to the clock and the hardware
    /// readout, so the host must ask this (and [`Self::next_feedback_deadline`])
    /// where it asks those.
    pub(crate) fn feedback_frame_due(&self, now: std::time::Instant) -> bool {
        self.feedback.moving(now) || self.feedback.next_deadline().is_some_and(|end| end <= now)
    }

    /// When the answer in flight comes to rest; `None` once its resting frame
    /// has been painted.
    pub(crate) fn next_feedback_deadline(&self) -> Option<std::time::Instant> {
        self.feedback.next_deadline()
    }

    /// The live producer and pixel oracles record the same complete scene.
    pub(crate) fn draw(
        &mut self,
        builder: &mut FrameBuilder,
        composition: &LockComposition<'_>,
    ) -> FramePaint {
        // Timestamp the answer before painting it: the paint is pure and reads
        // only this snapshot, so authentication state and the accessibility
        // tree never move with an animation frame.
        self.feedback.observe(
            composition.view,
            composition.now,
            composition.theme.motion(),
        );
        let status = self.feedback.paint(composition.now);
        if self.feedback_frame_due(composition.now)
            && let Some(until) = self.next_feedback_deadline()
        {
            // The answer moves until `until`. The lock draws outside the
            // widget tree, so say it on the motion clock too: a host whose
            // frame pump follows that clock asks for these frames without the
            // compositor having to know about this scene.
            ui::motion::want_frames_until(until);
        }
        let mut scene = Scene::new();
        let ((), parts) = ui::scene_parts::record(&mut scene, |scene| {
            super::draw_lock_scene_into(
                scene,
                composition.outputs,
                composition.theme,
                builder.text_renderer_mut(),
                composition.view,
                composition.wallpaper,
                ui::auth_backdrop::STILL_SECONDS,
                &status,
            );
            if let Some(osd) = composition.osd {
                // An OSD can declare bounded parts of its own. They are not
                // credential parts and cannot authorize retaining a lock frame.
                // Append its complete image without importing those part keys.
                let mut overlay = Scene::new();
                super::draw_lock_osd(
                    &mut overlay,
                    composition.outputs,
                    composition.theme,
                    builder.text_renderer_mut(),
                    osd,
                    composition.now,
                );
                scene.append(&overlay, None);
            }
        });
        // The resting frame is the last one the answer asks for.
        self.feedback.rest_if_settled(composition.now);
        self.finish(scene, parts, composition.outputs, builder)
    }

    /// Called after the complete composition, including OSD. Changes outside
    /// the credential parts (wallpaper, clock, media, keyboard, an alert,
    /// theme) cannot pass the byte-for-byte scene-parts proof; a change inside
    /// the field or the status row damages just that part's old and new
    /// bounds.
    fn finish(
        &mut self,
        scene: Scene,
        parts: Option<SectionParts>,
        outputs: &[LockOutput],
        builder: &mut FrameBuilder,
    ) -> FramePaint {
        let revision = builder.reserve_scene_revision();
        let previous_revision = self.previous.as_ref().map(|old| old.revision);
        let damage_regions = self.previous.as_ref().and_then(|old| {
            if revision == 0 || old.outputs != outputs {
                return None;
            }
            let rects =
                ui::scene_parts::damage(&old.scene, &old.parts, &scene, parts.as_ref()?).ok()?;
            Some(
                rects
                    .into_iter()
                    .filter_map(|rect| builder.scene_tile_rect(rect))
                    .collect(),
            )
        });
        self.previous = parts.filter(|_| revision != 0).map(|parts| Previous {
            scene: scene.clone(),
            parts,
            revision,
            outputs: outputs.to_vec(),
        });
        FramePaint {
            revision,
            previous_revision,
            damage_regions,
            ..FramePaint::full(scene)
        }
    }
}

#[cfg(test)]
mod tests;
