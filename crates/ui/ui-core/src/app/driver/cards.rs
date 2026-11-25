//! The cards the app stands over its window. A driver has no compositor to give each card a window
//! of its own, so it stands them as a shell with one window does, through
//! [`crate::shell_cards::ShellCards`]: a card stands, draws and takes input where a page or a
//! nested desktop would put it.

use vello::kurbo::{Point, Rect};
use winit::event::WindowEvent;

use crate::widget::FloatingSurfaceWidgetExt as _;

use super::Driver;

/// The cards standing over the app and where the pointer last was, in logical pixels.
#[derive(Debug, Default)]
pub(super) struct Cards {
    cards: crate::shell_cards::ShellCards,
    pointer: Option<Point>,
}

impl Driver {
    /// Stand the cards the app presents and tell it where they went, after its frame updated and
    /// before the next one draws and takes input. Says whether a card came, went or moved.
    pub(super) fn follow_cards(&mut self) -> bool {
        let (width, height) = self.viewport();
        let work = Rect::new(0.0, 0.0, f64::from(width), f64::from(height));
        let bar = Rect::new(0.0, 0.0, work.width(), 0.0);
        let _entry = self.app.enter_wake_scope();
        self.cards
            .cards
            .follow(self.app.root_widget.as_mut(), work, bar)
    }

    /// Draw each card that stands by the app that owns it, over the app's frame.
    pub(super) fn draw_cards(&mut self) {
        if self.cards.cards.is_empty() {
            return;
        }
        let transform = vello::kurbo::Affine::scale(self.surface.scale.get());
        let app = &mut *self.app;
        self.cards.cards.draw(
            app.root_widget.as_mut(),
            &mut crate::draw::DrawCtx::new(
                &mut self.scene,
                transform,
                &mut app.text_renderer,
                &app.theme,
                app.presence_ctx.draw_env(),
            ),
        );
    }

    /// Hand `event` to the card it is for, ahead of the window under it: the card under the
    /// pointer, or the one last pressed for a key. Answers whether a card took it.
    pub(super) fn route_to_card(&mut self, event: &WindowEvent) -> bool {
        let scale = self.surface.scale.get().max(f64::EPSILON);
        if let Some(at) = crate::motion::pointer_of(event) {
            self.cards.pointer = Some(Point::new(at.x / scale, at.y / scale));
        }
        if self.cards.cards.is_empty() {
            return false;
        }
        let Some(card) = self.cards.cards.target(event, self.cards.pointer) else {
            return false;
        };
        self.app.dispatch_to_card(event, scale, |root, event, ctx| {
            root.floating_surface_event(&card, event, ctx)
        })
    }

    /// The cards as a reader finds them: what each app draws there, at the rectangle it stands
    /// at. The app paints a card, so its controls own no retained widget and are named here.
    pub(super) fn cards_inspection(&self) -> Vec<crate::headless::InspectionNode> {
        self.cards
            .cards
            .accessibility(self.app.root_widget.as_ref())
            .into_iter()
            .map(|(id, window, semantics)| crate::headless::InspectionNode {
                path: format!("floating:{id}"),
                widget: "FloatingSurface".into(),
                test_id: None,
                focusable: false,
                rect: [window.x0, window.y0, window.width(), window.height()],
                child_clip: None,
                page_title: None,
                layout: None,
                paint_key: None,
                semantics: Some(semantics),
                children: Vec::new(),
            })
            .collect()
    }
}
