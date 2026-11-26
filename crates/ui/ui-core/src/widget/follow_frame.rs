//! `FollowFrame`: the frame round the work while the person follows someone, in the followed
//! person's colour, saying all the way round whose view this is.
//!
//! A shell lays it over the room its windows stand in. The windows are composited over a shell
//! drawn under them, so the frame is drawn as a part over the windows
//! ([`crate::lift::over_windows`]); it takes no input. It comes and goes with the shared appear
//! motion, keeping the last colour while it leaves. It reports what it draws to the host's
//! [`Wells`](super::Wells) whenever that changes, so a host reads the frame from what was drawn.
//! At rest it asks for no frame.

use taffy::{NodeId, Style, TaffyTree};
use vello::kurbo::{Point, Rect};

use tilo_ui_base::component::{Component, Described};
use tilo_ui_base::theme::Theme;
use tilo_ui_base::widget::{TaffyTreeExt, Widget};

/// The frame round the work while following someone.
#[derive(Component, Default)]
#[component(
    name = "FollowFrame",
    role = "Group",
    not_in_gallery = "it frames the windows a shell lays out, which a gallery has none of"
)]
pub struct FollowFrameProps {
    /// The followed person's colour, `rrggbb`; empty while the person follows nobody, and the
    /// frame leaves.
    #[component(required, positional)]
    #[component(data)]
    pub colour: String,
}

/// See the module docs.
pub struct FollowFrame {
    /// The colour it is drawn in: the followed person's, kept while it leaves.
    colour: String,
    presence: tilo_ui_base::motion::Presence,
    /// The motion the theme asks for, as of the last frame drawn.
    motion: Option<tilo_ui_base::tokens::EffectiveMotion>,
    rect: Rect,
    /// The host's wells, as of the last layout: where what was drawn is reported.
    wells: Option<super::Wells>,
    layout_dirty: bool,
    dirty: bool,
}

impl FollowFrame {
    fn take(&mut self, props: FollowFrameProps) {
        let open = !props.colour.trim().is_empty();
        if open && props.colour != self.colour {
            self.colour = props.colour;
            self.dirty = true;
            self.layout_dirty = true;
        }
        if open != self.presence.is_open() {
            let now = tilo_ui_base::motion::frame_now();
            let motion = self.motion.unwrap_or_else(|| {
                tilo_ui_base::tokens::EffectiveMotion::new(
                    tilo_ui_base::tokens::Motion::default(),
                    false,
                )
            });
            let source = tilo_ui_base::motion::recent_input(now)
                .unwrap_or(tilo_ui_base::tokens::MotionSource::Pointer);
            self.presence.show(
                open,
                tilo_ui_base::motion::Appear::SPEC,
                motion,
                source,
                now,
            );
            self.dirty = true;
            self.layout_dirty = true;
        }
        self.report();
    }

    /// Tell the host's wells what is drawn now.
    fn report(&self) {
        if let Some(wells) = &self.wells {
            wells.frame(self.drawn());
        }
    }

    /// What was drawn: the colour and the rectangle the frame runs round, while it is up.
    fn drawn(&self) -> Option<(String, Rect)> {
        (self.presence.is_open() && self.rect.width() > 0.0 && self.rect.height() > 0.0)
            .then(|| (self.colour.clone(), self.rect))
    }
}

impl Drop for FollowFrame {
    fn drop(&mut self) {
        if let Some(wells) = &self.wells {
            wells.frame(None);
        }
    }
}

impl Described for FollowFrame {
    type Props = FollowFrameProps;

    fn from_props(_key: &str, props: FollowFrameProps) -> Self {
        let open = !props.colour.trim().is_empty();
        Self {
            colour: props.colour,
            // A frame that is up when the view is built stands at once: only a change moves.
            presence: if open {
                tilo_ui_base::motion::Presence::opened()
            } else {
                tilo_ui_base::motion::Presence::closed()
            },
            motion: None,
            rect: Rect::ZERO,
            wells: None,
            layout_dirty: true,
            dirty: true,
        }
    }

    fn set_props(&mut self, props: FollowFrameProps) -> bool {
        self.take(props);
        true
    }
}

impl Widget for FollowFrame {
    fn name(&self) -> &'static str {
        "FollowFrame"
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.widget_leaf(
            Style {
                size: taffy::Size::from_percent(1.0, 1.0),
                ..Style::default()
            },
            node_map,
            self,
        )
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = tilo_ui_base::widget::layout_rect(taffy, node, origin);
        if let Some(wells) = super::window_well::current() {
            // Laid out for another part now: the frame it reported for the part before is gone.
            if let Some(before) = self.wells.as_ref().filter(|before| !before.is(&wells)) {
                before.frame(None);
            }
            self.wells = Some(wells);
        }
        self.layout_dirty = false;
        self.report();
    }

    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text, theme, mut env) = ctx.parts();
        self.dirty = false;
        self.motion = Some(theme.motion());
        let now = tilo_ui_base::motion::frame_now();
        #[allow(clippy::cast_possible_truncation, reason = "an opacity")]
        let openness = self.presence.progress(now) as f32;
        if openness <= 0.0 || self.rect.width() <= 0.0 || self.rect.height() <= 0.0 {
            return;
        }
        // The person's own colour, which the theme cannot name.
        let Some(colour) = crate::widget::avatar::colour_of(&self.colour) else {
            return;
        };
        let area = self.rect;
        let rule = 2.0 * f64::from(theme.tokens.metrics.chrome_rule_px);
        crate::lift::over_windows(
            scene,
            transform,
            (
                tilo_ui_base::scene_parts::PartKey::new("follow-frame", ""),
                area.inflate(rule, rule),
            ),
            |scene, transform| {
                tilo_ui_base::presence::paint_follow_frame(
                    scene, transform, area, colour, openness, theme,
                );
            },
        );
    }

    /// A frame at rest asks for no frame: only one coming or going does.
    fn needs_redraw(&self) -> bool {
        self.dirty || self.presence.is_moving(tilo_ui_base::motion::frame_now())
    }

    fn is_layout_dirty(&self) -> bool {
        self.layout_dirty
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    tilo_ui_base::described!();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The frame stands while the person follows someone and leaves when they stop, asking for a
    /// frame only while it moves.
    #[test]
    fn the_frame_comes_and_goes_with_following() {
        let _settled = tilo_ui_base::motion::testing::settled();
        let mut frame = FollowFrame::from_props(
            "",
            FollowFrameProps {
                colour: String::new(),
            },
        );
        frame.rect = Rect::new(0.0, 40.0, 800.0, 600.0);
        assert_eq!(frame.drawn(), None, "nobody followed draws no frame");
        frame.set_props(FollowFrameProps {
            colour: "3366cc".to_owned(),
        });
        assert_eq!(
            frame.drawn(),
            Some(("3366cc".to_owned(), frame.rect)),
            "following draws the frame in their colour"
        );
        frame.dirty = false;
        frame.set_props(FollowFrameProps {
            colour: "3366cc".to_owned(),
        });
        assert!(!frame.needs_redraw(), "a frame at rest draws nothing again");
        frame.set_props(FollowFrameProps {
            colour: String::new(),
        });
        assert_eq!(frame.drawn(), None, "stopping takes the frame away");
    }
}
