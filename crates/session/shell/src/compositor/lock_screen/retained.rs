//! Only the password part may change over a retained lock image. The complete
//! scene remains authoritative; an unproven difference always repaints it all.

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
}

impl RetainedLockScene {
    /// Both lock edges are boundaries, even if no unlocked frame was painted.
    pub(crate) fn clear(&mut self) {
        self.previous = None;
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
            );
            if let Some(osd) = composition.osd {
                super::draw_lock_osd(
                    scene,
                    composition.outputs,
                    composition.theme,
                    builder.text_renderer_mut(),
                    osd,
                    composition.now,
                );
            }
        });
        self.finish(scene, parts, composition.outputs, builder)
    }

    /// Called after the complete composition, including OSD. Changes outside
    /// the password part (wallpaper, clock, status, media, keyboard, theme)
    /// cannot pass the byte-for-byte scene-parts proof.
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
