//! What a frame paints over its content: the markers of the actions people
//! and agents name on it. A frame a page shows as its composition paints
//! none of it, and keeps the rest of the paint's bookkeeping (headless.rs).

use super::FrameHarness;
use vello::kurbo::Affine;
use vello::kurbo::Rect;

impl FrameHarness {
    /// Paint the place of each action a person's pointer or an agent's turn
    /// names on the frame's scene, into `self.drawn_action_markers`.
    pub(super) fn paint_action_markers(&mut self, interaction_now: crate::time::Instant) {
        if self.presence.has_visible_interactions(interaction_now)
            && let Some((tree, bindings)) = self.retained_action_geometry()
        {
            let viewport = Rect::new(
                0.0,
                0.0,
                f64::from(self.viewport.0),
                f64::from(self.viewport.1),
            );
            let text_loci = crate::presence::capability::agent_text_loci(
                self.root.as_ref(),
                &self.theme,
                &self.presence,
                &self.presence.visible_interactions(interaction_now),
            );
            self.drawn_action_markers = crate::action_locus::draw_action_presence(
                &mut crate::paint::PaintCtx::new(
                    &mut self.scene,
                    Affine::IDENTITY,
                    &mut self.text,
                    &self.theme,
                ),
                &mut self.presence,
                &tree,
                &bindings,
                &text_loci,
                viewport,
                ("retained-primary", &|id| {
                    crate::widget::find_resource(self.root.as_ref(), &self.state.doc, id)
                        .map(|resource| resource.label)
                }),
                interaction_now,
            );
        }
    }
}
