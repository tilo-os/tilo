//! The cards the shell's apps present over it, for a shell with one window:
//! the shell stands them through [`ui::shell_cards::ShellCards`], as a page's
//! frame does, and this layer gives the column its area under the bar and
//! hands the card on top of its windows the input that lands on it.

use ui::presence::PresenceCtx;
use ui::widget::{EventCtx, Widget};
use ui::{TextRenderer, Theme};
use vello::Scene;
use vello::kurbo::{Affine, Rect};
use winit::event::WindowEvent;

use super::BackgroundLayer;

impl BackgroundLayer {
    /// The area the windows stand in, under the bar: a card the shell places
    /// stands inside it.
    #[must_use]
    pub fn area_rect(&self) -> Rect {
        self.area.get_rect()
    }

    /// Stand each card where the shell's floating column puts it, tell the app
    /// what it did, and drop the cards the app no longer presents. Once a
    /// frame, after the surface updated and before it draws. Says whether the
    /// shell's cards changed, so a frame that moved one is drawn.
    pub fn follow_cards(&mut self) -> bool {
        let work = self.area_rect();
        let bar = self.band.get_rect();
        let Some(surface) = self.area.children_mut().pop() else {
            return false;
        };
        self.cards.follow(surface, work, bar)
    }

    /// Draw the cards the shell stands over its windows, by the apps that own
    /// them, once the surface has drawn.
    pub fn draw_cards(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let Some(surface) = self.area.children_mut().pop() else {
            return;
        };
        self.cards
            .draw(surface, scene, transform, text, theme, presence);
    }

    /// Hand `event` to the card it is for, in the shell's coordinates, through
    /// the app that draws it. Answers whether a card took it.
    pub fn card_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) -> bool {
        if self.cards.is_empty() {
            return false;
        }
        let path = ctx.path.clone();
        // The shell's third layer is the area, and the area's top layer is the
        // surface: the card's input is forwarded with the node it draws at.
        let Some(area) = ctx
            .taffy
            .children(ctx.node_id)
            .ok()
            .and_then(|nodes| nodes.get(2).copied())
        else {
            return false;
        };
        let Some(node) = ctx
            .taffy
            .children(area)
            .ok()
            .and_then(|nodes| nodes.last().copied())
        else {
            return false;
        };
        let Some(surface) = self.area.children_mut().pop() else {
            return false;
        };
        self.cards
            .route(surface, event, self.cursor, &mut ctx.reborrow(&path, node))
    }

    /// The cards as assistive technology reads them: each the reading and the
    /// controls its app draws, where the shell stands it.
    #[must_use]
    pub fn cards_accessibility(&self) -> Vec<ui::a11y::AccessNode> {
        let Some(surface) = self.area.children().pop() else {
            return Vec::new();
        };
        self.cards
            .accessibility(surface)
            .into_iter()
            .map(|(_, _, node)| node)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use taffy::{NodeId, Style, TaffyTree};

    use super::*;
    use ui::floating_surface::{FloatingSurfaceHostMessage, FloatingSurfaceRequest};
    use ui::widget::TaffyTreeExt;
    use vello::kurbo::{Point, Size};

    /// A surface with one card up, which keeps what the shell told it.
    struct CardSurface {
        told: Rc<RefCell<Vec<FloatingSurfaceHostMessage>>>,
        seen: Rc<RefCell<Vec<&'static str>>>,
        /// Whether the card is still to be asked for.
        to_ask: bool,
    }

    impl ui::widget::FloatingSurfaceProvider for CardSurface {
        fn on_floating_surface_host_message(&mut self, message: &FloatingSurfaceHostMessage) {
            self.told.borrow_mut().push(message.clone());
        }

        fn take_floating_surface_request(&mut self) -> Option<FloatingSurfaceRequest> {
            std::mem::take(&mut self.to_ask).then(request)
        }

        fn active_floating_surface_ids(&self) -> Vec<String> {
            vec![request().id]
        }

        fn floating_surface_event(
            &mut self,
            id: &str,
            event: &WindowEvent,
            _: &mut EventCtx<'_>,
        ) -> bool {
            if id != request().id {
                return false;
            }
            match event {
                WindowEvent::MouseInput { .. } => self.seen.borrow_mut().push("press"),
                WindowEvent::CursorMoved { .. } => self.seen.borrow_mut().push("move"),
                _ => return false,
            }
            matches!(event, WindowEvent::MouseInput { .. })
        }
    }

    /// The windows of the shell the press test lays out: none is placed.
    struct Tiles;

    impl tilo_desktop_surface::TileSource for Tiles {
        fn tile(
            &mut self,
            _: &tilo_desktop_model::Workspace,
            tab: &tilo_desktop_model::Tab,
        ) -> ui::widget::TileHost {
            ui::widget::TileHost::new(ui::widget::TileContent::Notice {
                title: tab.title.clone(),
                detail: String::new(),
            })
        }
    }

    impl Widget for CardSurface {
        ui::provides!(dyn ui::widget::FloatingSurfaceProvider);

        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &Theme,
            map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            taffy.widget_leaf(Style::default(), map, self)
        }

        fn name(&self) -> &'static str {
            "CardSurface"
        }
    }

    fn request() -> FloatingSurfaceRequest {
        FloatingSurfaceRequest {
            id: "chat-call:general".into(),
            title: "Just you".into(),
            role: ui::floating_surface::FloatingRole::ActiveCall,
            source: ui::floating_surface::FloatingSource::AppRegion {
                app_id: "chat".into(),
                region: Rect::new(40.0, 60.0, 640.0, 460.0),
            },
            preferred_size: Size::new(320.0, 236.0),
            aspect_ratio: None,
            initial_rect: None,
            presentation_rect: Some(Rect::new(0.0, 0.0, 336.0, 252.0)),
            tracking_source: None,
        }
    }

    /// The shell a page has: one window, the cards of its apps over
    /// it. A press on a card's control is the app's that draws the card, and
    /// the shell keeps the press from what lies under the card.
    #[test]
    fn a_press_on_a_card_the_shell_stands_reaches_the_app_that_draws_it() {
        use tilo_desktop_model::IdMinter;
        use ui::synthetic_input::{cursor_moved, mouse_press};

        let told = Rc::new(RefCell::new(Vec::new()));
        let seen = Rc::new(RefCell::new(Vec::new()));
        let surface = CardSurface {
            told: Rc::clone(&told),
            seen: Rc::clone(&seen),
            to_ask: true,
        };
        let mut minter = IdMinter::new(7);
        let tree = tilo_session_core::desktop::seed_tree(&mut minter, "Shell", Vec::new());
        let handle = tilo_desktop_surface::DesktopSurface::new(tree, 7, Box::new(Tiles)).1;
        let mut layer = BackgroundLayer::new(
            Box::new(surface),
            handle,
            crate::launchpad::CollaborationLaunchpad::new(),
            Rc::new(Vec::new),
            Rc::default(),
            Vec::new(),
        )
        .with_cards();
        let mut taffy = TaffyTree::new();
        let mut map = std::collections::HashMap::new();
        let node = layer.build_layout(&mut taffy, &Theme::default(), &mut map);
        let available = taffy::Size {
            width: taffy::AvailableSpace::Definite(1024.0),
            height: taffy::AvailableSpace::Definite(640.0),
        };
        taffy
            .compute_layout_with_measure(node, available, |_, _, _, _, _| taffy::Size::ZERO)
            .expect("a layout");
        layer.apply_layout(&taffy, node, Point::ZERO);
        layer.follow_cards();
        let (_, window) = layer
            .cards
            .windows()
            .next()
            .expect("the shell stands the card its app presents");
        assert!(
            window.area() > 0.0,
            "the card the app drew has a place on the shell: {window:?}"
        );
        // The card is drawn by the app, over the shell, once a frame.
        ui::widget::DrawChild::draw_child(
            &mut layer,
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &Theme::default(),
            &PresenceCtx::default(),
        );
        let at = window.center();
        let mut dispatch = ui::testing::Dispatch::new().layout(&taffy, node);
        dispatch.send(&mut layer, &cursor_moved(at.x, at.y));
        assert!(
            dispatch.send(&mut layer, &mouse_press()).handled,
            "the shell takes a press on the card it stands: {:?}",
            seen.borrow()
        );
        assert!(
            seen.borrow().contains(&"press"),
            "the app that draws the card is handed the press: {:?}",
            seen.borrow()
        );
    }
}
