//! Only the credential part — the password prompt and its unlock action —
//! may change over a retained lock image. The complete scene remains
//! authoritative; an unproven difference (the clock, artwork, media card,
//! keyboard, an alert, the theme) always repaints it all.

use super::{LockOutput, LockView};
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
    /// When the prompt's answer in flight (a verification or a refusal
    /// arriving) comes to rest, as of the last frame drawn.
    answer_deadline: Option<std::time::Instant>,
}

impl RetainedLockScene {
    /// Both lock edges are boundaries, even if no unlocked frame was painted.
    pub(crate) fn clear(&mut self) {
        self.previous = None;
        self.answer_deadline = None;
    }

    /// Whether the answer in flight needs another frame, including
    /// the frame that paints it at rest.
    ///
    /// The lock's frames are otherwise filtered to the clock and the hardware
    /// readout, so the host must ask this (and [`Self::next_feedback_deadline`])
    /// where it asks those.
    pub(crate) fn feedback_frame_due(&self) -> bool {
        // Moving until the deadline, then the resting frame at it.
        self.answer_deadline.is_some()
    }

    /// When the answer in flight comes to rest; `None` once its resting frame
    /// has been painted.
    pub(crate) fn next_feedback_deadline(&self) -> Option<std::time::Instant> {
        self.answer_deadline
    }

    /// The live producer and pixel oracles record the same complete scene.
    pub(crate) fn draw(
        &mut self,
        builder: &mut FrameBuilder,
        composition: &LockComposition<'_>,
    ) -> FramePaint {
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
                composition.now,
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
        // The prompt timestamps its answer as it paints, and asks for the
        // answer's frames on the motion clock; the resting frame is the last.
        self.answer_deadline = composition
            .view
            .prompt
            .and_then(|prompt| prompt.borrow().answer_deadline());
        self.finish(scene, parts, composition.outputs, builder)
    }

    /// Called after the complete composition, including OSD. Changes outside
    /// the credential parts (wallpaper, clock, media, keyboard, an alert,
    /// theme) cannot pass the byte-for-byte scene-parts proof; a change inside
    /// the prompt damages just that part's old and new
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
