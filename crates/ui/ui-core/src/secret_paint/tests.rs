use tilo_ui_base::secret_paint::*;
use tilo_ui_base::text_render::TextRenderer;
use tilo_ui_base::theme::Theme;
use crate::{widget::Column, widget::Row};
use tilo_ui_base::widget::Widget;
use vello::Scene;
use vello::kurbo::Affine;

struct SettledSecret;

impl Widget for SettledSecret {
    crate::testing::inert_widget_methods!(leaf_layout, name = "settled-secret");
    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (_, _, _, _, mut env) = ctx.parts();
        note_secret_painted();
    }
    fn needs_redraw(&self) -> bool {
        false
    }
}

fn frame_shows_secret(widget: &mut dyn Widget) -> bool {
    let presence = tilo_ui_base::presence::PresenceCtx::empty();
    let mut text = TextRenderer::new();
    let secrets = SecretPaint::begin();
    widget.draw(&mut tilo_ui_base::draw::DrawCtx::new(
        &mut Scene::new(),
        Affine::IDENTITY,
        &mut text,
        &Theme::dark(),
        presence.draw_env(),
    ));
    secrets.finish()
}

tilo_test_env::scenario! {
    fn a_secret_painted_in_a_scope_is_reported_there_and_outwards;

    phase a_cached_subtree_still_reports_its_secret;
    {
        let mut column = Column::new().push(Box::new(SettledSecret));
        let mut row = Row::new().push(Box::new(SettledSecret));
        for container in [&mut column as &mut dyn Widget, &mut row] {
            assert!(frame_shows_secret(container), "the first frame draws it");
            assert!(frame_shows_secret(container), "a cached frame replays it");
        }
        let mut plain = Column::new().push(Box::new(Column::new()));
        assert!(!frame_shows_secret(&mut plain));
        assert!(!frame_shows_secret(&mut plain));
    }

    phase a_scope_sees_only_its_own_paint_and_passes_it_outwards;
    {
        let frame = SecretPaint::begin();
        {
            let plain = SecretPaint::begin();
            assert!(!plain.finish());
        }
        {
            let subtree = SecretPaint::begin();
            note_secret_painted();
            assert!(subtree.finish());
        }
        {
            let later = SecretPaint::begin();
            assert!(!later.finish(), "a sibling starts clean");
        }
        assert!(frame.finish(), "the frame saw the subtree's secret");
        let next = SecretPaint::begin();
        assert!(!next.finish(), "the next frame starts clean");
    }
}
