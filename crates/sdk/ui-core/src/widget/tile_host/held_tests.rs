use std::cell::Cell;
use std::rc::Rc;

use super::*;
use crate::offscreen::OffscreenRenderer;
use vello::kurbo::Vec2;
use vello::peniko::Color;

/// An app that fills its window with a colour it may change at any time,
/// and can paint its whole document, twice the window's height.
struct Repaints {
    rect: Rect,
    color: Rc<Cell<Color>>,
}

impl Widget for Repaints {
    crate::provides!(dyn crate::widget::CompleteContentProvider);

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.widget_leaf(Style::default(), node_map, self)
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        _tr: &mut TextRenderer,
        _th: &Theme,
        _p: &PresenceCtx,
    ) {
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

impl crate::widget::CompleteContentProvider for Repaints {
    fn complete_content_size(&self) -> Option<vello::kurbo::Size> {
        Some(vello::kurbo::Size::new(
            self.rect.width(),
            self.rect.height() * 2.0,
        ))
    }

    fn draw_complete_content(
        &mut self,
        scene: &mut Scene,
        bounds: Rect,
        transform: Affine,
        _tr: &mut TextRenderer,
        _theme: &Theme,
        _presence: &PresenceCtx,
    ) -> bool {
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

/// The colour `scene` shows at `at`.
fn shown(scene: &Scene, at: Point) -> (u8, u8, u8) {
    let mut renderer = OffscreenRenderer::new(SIZE.0, SIZE.1);
    let rgba = renderer.render(scene, Color::from_rgb8(0, 0, 0));
    let (r, g, b, _) = OffscreenRenderer::sample(
        &rgba,
        SIZE.0,
        crate::num::u32_of_f64(at.x),
        crate::num::u32_of_f64(at.y),
    );
    (r, g, b)
}

fn draw(host: &mut TileHost, tr: &mut TextRenderer, theme: &Theme) -> Scene {
    let mut scene = Scene::new();
    host.draw(
        &mut scene,
        Affine::IDENTITY,
        tr,
        theme,
        &PresenceCtx::empty(),
    );
    scene
}

// A window of an app the shell runs itself (the terminal, Files, Notes)
// that repaints while the screenshot is open still shows, and is
// captured as, what it showed when the screenshot opened, also once it
// has moved and also when captured whole; the app keeps running, and
// what it drew shows once the hold ends.
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
    host.hold_content(&mut tr, &theme, &PresenceCtx::empty());
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

    // The strip scrolls it: the held window moves with it.
    let moved = rect + Vec2::new(150.0, 0.0);
    host.rect = moved;
    let scene = draw(&mut host, &mut tr, &theme);
    assert_eq!(shown(&scene, moved.center()), OLD, "moved, still held");
    assert_eq!(
        shown(&scene, rect.center()),
        (0, 0, 0),
        "and drawn where the window is now"
    );

    // Captured whole: the complete document as it was when held.
    let complete = host
        .complete_content_rect()
        .expect("the held window has its complete document");
    assert!((complete.height() - rect.height() * 2.0).abs() < 0.5);
    let mut capture = Scene::new();
    let captured = host
        .draw_complete_content_capture(
            &mut capture,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            &PresenceCtx::empty(),
        )
        .expect("the complete capture paints");
    assert_eq!(captured, complete);
    let low = Point::new(complete.center().x, complete.y1 - 10.0);
    assert_eq!(shown(&capture, low), OLD, "the whole document as held");

    // Released: live again, where the app lays itself out.
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
