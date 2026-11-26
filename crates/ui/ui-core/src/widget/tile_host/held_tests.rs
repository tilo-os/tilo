use std::cell::Cell;
use std::rc::Rc;

use super::*;
use tilo_ui_base::offscreen::OffscreenRenderer;
use vello::kurbo::Vec2;
use vello::peniko::Color;

struct Repaints {
    rect: Rect,
    color: Rc<Cell<Color>>,
}

impl Widget for Repaints {
    tilo_ui_base::provides!(dyn tilo_ui_base::widget::CompleteContentProvider);

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.widget_leaf(Style::default(), node_map, self)
    }

    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, _tr, _th, mut env) = ctx.parts();
        let rect = self.rect;
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            self.color.get(),
            None,
            &rect,
        );
    }

    fn name(&self) -> &'static str {
        "Repaints"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

impl tilo_ui_base::widget::CompleteContentProvider for Repaints {
    fn complete_content_size(&self) -> Option<vello::kurbo::Size> {
        Some(vello::kurbo::Size::new(
            self.rect.width(),
            self.rect.height() * 2.0,
        ))
    }

    fn draw_complete_content(&mut self, bounds: Rect, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) -> bool {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, _tr, _theme, mut env) = ctx.parts();
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            self.color.get(),
            None,
            &bounds,
        );
        true
    }
}

impl crate::widget::app_widget::AppWidget for Repaints {
    fn app_kind(&self) -> AppKind {
        AppKind::Editor
    }
    fn app_title(&self) -> &'static str {
        "Repaints"
    }
    fn app_icon(&self) -> crate::widget::app_widget::IconDescriptor {
        crate::widget::app_widget::IconDescriptor::Glyph("R")
    }
}

const OLD: (u8, u8, u8) = (0xE0, 0x18, 0xA0);
const NEW: (u8, u8, u8) = (0x18, 0xC0, 0x40);
const SIZE: (u32, u32) = (400, 400);

fn rgb((r, g, b): (u8, u8, u8)) -> Color {
    Color::from_rgb8(r, g, b)
}

fn shown(scene: &Scene, at: Point) -> (u8, u8, u8) {
    let mut renderer = OffscreenRenderer::new(SIZE.0, SIZE.1);
    let rgba = renderer.render(scene, Color::from_rgb8(0, 0, 0));
    let (r, g, b, _) = OffscreenRenderer::sample(
        &rgba,
        SIZE.0,
        tilo_ui_base::num::u32_of_f64(at.x),
        tilo_ui_base::num::u32_of_f64(at.y),
    );
    (r, g, b)
}

fn draw(host: &mut TileHost, tr: &mut TextRenderer, theme: &Theme) -> Scene {
    let mut scene = Scene::new();
    host.draw(&mut tilo_ui_base::draw::DrawCtx::new(
        &mut scene,
        Affine::IDENTITY,
        tr,
        theme,
        tilo_ui_base::presence::PresenceCtx::empty().draw_env(),
    ));
    scene
}

#[test]
fn an_in_process_window_that_repaints_while_held_shows_what_it_showed_when_held() {
    let theme = Theme::default();
    let mut tr = TextRenderer::new();
    let rect = Rect::new(40.0, 40.0, 160.0, 160.0);
    let color = Rc::new(Cell::new(rgb(OLD)));
    let mut host = TileHost::new(TileContent::App(Box::new(Repaints {
        rect,
        color: color.clone(),
    })));
    host.rect = rect;
    assert_eq!(shown(&draw(&mut host, &mut tr, &theme), rect.center()), OLD);

    assert!(hold_windows());
    host.hold_content(&mut tr, &theme, &tilo_ui_base::presence::PresenceCtx::empty());
    assert!(host.content_held());
    assert!(
        !host.needs_redraw(),
        "what the app draws while held is not drawn"
    );
    color.set(rgb(NEW));
    assert_eq!(
        shown(&draw(&mut host, &mut tr, &theme), rect.center()),
        OLD,
        "the app repainted; the window still shows what it showed when held"
    );

    let moved = rect + Vec2::new(150.0, 0.0);
    host.rect = moved;
    let scene = draw(&mut host, &mut tr, &theme);
    assert_eq!(shown(&scene, moved.center()), OLD, "moved, still held");
    assert_eq!(
        shown(&scene, rect.center()),
        (0, 0, 0),
        "and drawn where the window is now"
    );

    let complete = host
        .complete_content_rect()
        .expect("the held window has its complete document");
    assert!((complete.height() - rect.height() * 2.0).abs() < 0.5);
    let mut capture = Scene::new();
    let captured = host
        .draw_complete_content_capture(&mut tilo_ui_base::draw::DrawCtx::new(
            &mut capture,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            tilo_ui_base::presence::PresenceCtx::empty().draw_env(),
        ))
        .expect("the complete capture paints");
    assert_eq!(captured, complete);
    let low = Point::new(complete.center().x, complete.y1 - 10.0);
    assert_eq!(shown(&capture, low), OLD, "the whole document as held");

    host.rect = rect;
    assert!(release_windows());
    assert!(!host.content_held());
    assert!(
        host.needs_redraw(),
        "the hold ended: the live window replaces the recording"
    );
    assert_eq!(
        shown(&draw(&mut host, &mut tr, &theme), rect.center()),
        NEW,
        "the hold ended; the window shows what the app drew"
    );
    let mut damage = Vec::new();
    host.collect_damage(&mut damage);
    assert!(
        damage.iter().any(|region| region.contains(rect.center())),
        "and the frame that drew it repaints the window, even when the app \
         has nothing new to say: {damage:?}"
    );
    host.mark_drawn();
    assert!(host.held.is_none(), "the recording goes once drawn over");
}
