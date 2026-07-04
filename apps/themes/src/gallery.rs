//! The preset gallery — a left-hand column of theme cards. Selection and
//! all card colours are pushed in from [`crate::app::ThemesApp`] via
//! [`Gallery::set_catalog`]; clicks are emitted through the command bus
//! by the individual [`ThemeCard`]s.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::style_helpers::length;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::Widget;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use crate::draw;
use crate::model::ThemeSpec;
use crate::widgets::ThemeCard;

const HEADER_H: f64 = 52.0;
const PAD: f32 = 14.0;

pub struct Gallery {
    cards: Vec<ThemeCard>,
    rect: Rect,
    node: Option<NodeId>,
    count: usize,
    structure_dirty: bool,
}

impl Gallery {
    pub fn new() -> Self {
        Self { cards: Vec::new(), rect: Rect::ZERO, node: None, count: 0, structure_dirty: false }
    }

    pub fn set_catalog(&mut self, specs: &[ThemeSpec], selected: &str) {
        self.count = specs.len();
        let ids_match = self.cards.len() == specs.len()
            && self.cards.iter().zip(specs).all(|(c, s)| c.id() == s.id);
        if ids_match {
            for (c, s) in self.cards.iter_mut().zip(specs) {
                c.update(s, s.id == selected);
            }
        } else {
            self.cards = specs.iter().map(|s| ThemeCard::from_spec(s, s.id == selected)).collect();
            self.structure_dirty = true;
        }
    }
}

impl Widget for Gallery {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(&mut self, taffy: &mut TaffyTree<()>, theme: &Theme, node_map: &mut HashMap<NodeId, *mut dyn Widget>) -> NodeId {
        let child_nodes: Vec<NodeId> = self.cards.iter_mut().map(|c| c.build_layout(taffy, theme, node_map)).collect();
        let node = taffy
            .new_with_children(
                Style {
                    display: taffy::Display::Flex,
                    flex_direction: taffy::FlexDirection::Column,
                    gap: Size { width: length(0.0), height: length(8.0) },
                    padding: taffy::Rect {
                        left: length(PAD),
                        right: length(PAD),
                        top: length(HEADER_H as f32),
                        bottom: length(PAD),
                    },
                    size: Size { width: Dimension::percent(1.0), height: Dimension::percent(1.0) },
                    ..Default::default()
                },
                &child_nodes,
            )
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        self.node = Some(node);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + l.location.x as f64,
            origin.y + l.location.y as f64,
            origin.x + l.location.x as f64 + l.size.width as f64,
            origin.y + l.location.y as f64 + l.size.height as f64,
        );
        let new_origin = Point::new(self.rect.x0, self.rect.y0);
        if let Ok(children) = taffy.children(node) {
            for (card, child) in self.cards.iter_mut().zip(children.iter()) {
                card.apply_layout(taffy, *child, new_origin);
            }
        }
    }

    fn draw(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, p: &PresenceCtx) {
        // Panel background + right divider.
        draw::fill_rect(scene, t, self.rect, theme.bg_app);
        draw::line(scene, t, Point::new(self.rect.x1, self.rect.y0), Point::new(self.rect.x1, self.rect.y1), theme.border_standard, 1.0);
        // Header.
        draw::text_w(tr, scene, t, "Themes", 16.0, theme.text_primary, self.rect.x0 + PAD as f64, self.rect.y0 + 16.0, TextWeight::Bold);
        draw::text(tr, scene, t, &format!("{} themes", self.count), 11.0, theme.text_dim, self.rect.x0 + PAD as f64, self.rect.y0 + 36.0);
        for c in &mut self.cards {
            c.draw(scene, t, tr, theme, p);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_event(&mut self, event: &WindowEvent, scale: f64, m: &winit::event::Modifiers, state: &mut AutoCommit, path: &ObjId, handled: &mut bool, focus_target: &mut Option<NodeId>, node: NodeId, taffy: &TaffyTree<()>) {
        if let Ok(children) = taffy.children(node) {
            for (card, child) in self.cards.iter_mut().zip(children.iter()) {
                card.handle_event(event, scale, m, state, path, handled, focus_target, *child, taffy);
                if *handled {
                    break;
                }
            }
        }
    }

    fn name(&self) -> &str { "Gallery" }
    fn get_rect(&self) -> Rect { self.rect }

    fn children(&self) -> Vec<&dyn Widget> {
        self.cards.iter().map(|c| c as &dyn Widget).collect()
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.cards.iter_mut().map(|c| c as &mut (dyn Widget + '_)).collect()
    }

    fn take_structure_dirty(&mut self) -> bool {
        let d = self.structure_dirty;
        self.structure_dirty = false;
        d
    }
}

use winit::event::WindowEvent;
