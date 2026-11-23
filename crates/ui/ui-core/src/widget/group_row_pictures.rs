//! Pictures of a strip's windows that the frame did not draw whole: a window panned out of the
//! row, or only partly in it, drawn once for its picture alone ([`crate::window_pictures`]).

use vello::Scene;
use vello::kurbo::Affine;

use super::GroupRow;
use crate::text_render::TextRenderer;
use crate::theme::Theme;

impl GroupRow {
    /// Group `i`, drawn whole for its picture alone when one is wanted and the frame left
    /// it none ([`crate::window_pictures::owed`]): a window panned out of the row, or only partly
    /// in it, whose drawing is not the whole window. A surface showing every window small (the
    /// workspace overview) then shows it as it looks, not as its app's mark. Nothing of this
    /// drawing reaches the frame.
    pub(super) fn picture_unseen_group(
        &mut self,
        i: usize,
        transform: Affine,
        (tr, theme, env): (&mut TextRenderer, &Theme, &crate::draw::DrawEnv<'_>),
    ) {
        let id = self.groups[i].id;
        if !crate::window_pictures::owed(id)
            || crate::window_pictures::with_picture(id, |_| ()).is_some()
            || self.floats.contains_key(&id)
        {
            return;
        }
        // Drawn whole: what a window draws is culled to the part of it on screen, which is none.
        let mut whole = env.clone();
        whole.viewport = None;
        let drawing = self.record_group(i, Scene::new(), transform, (tr, theme, &whole));
        drawing.offer_picture(0);
    }
}

#[cfg(test)]
mod tests {
    use crate::widget::Widget as _;

    /// A wanted window panned out of the row is drawn once for its picture, which keeps its rect,
    /// and nothing of it reaches the frame; once pictured it is not drawn again off screen.
    #[test]
    fn a_wanted_window_off_screen_is_pictured_once_and_not_drawn() {
        crate::time::testing::with_monotonic_time(crate::time::now(), || {
            let (plain, plain_scene) = super::super::tests::drawn_strip(40);
            let unseen = *plain
                .culled_groups
                .last()
                .expect("the fixture culls windows");
            crate::window_pictures::want(&[unseen]);
            assert!(crate::window_pictures::owed(unseen));
            let (row, scene) = super::super::tests::drawn_strip(40);
            assert!(row.culled_groups.contains(&unseen), "it stays off screen");
            assert_eq!(
                scene.encoding().n_paths,
                plain_scene.encoding().n_paths,
                "nothing of the window off screen reaches the frame"
            );
            let rect = crate::window_pictures::with_picture(unseen, |picture| {
                assert!(!picture.scene.encoding().is_empty(), "it is drawn whole");
                picture.rect
            });
            let index = row.groups.iter().position(|group| group.id == unseen);
            assert_eq!(rect, index.map(|i| row.groups[i].get_rect()));
            assert!(
                !crate::window_pictures::owes_any(),
                "pictured, nothing is owed"
            );
            crate::window_pictures::want(&[]);
        });
    }
}
