//! The compact top bar each display other than the primary carries.
//!
//! The primary display holds the full top bar and the wheel disc. Every
//! other display gets a compact bar along its own top edge — workspace
//! navigation and the centre widget — so a person working on that display
//! can see and change which workspace they are in without travelling to the
//! primary one. Each bar is an ordinary [`TopBarChrome`] in its compact
//! variant, placed by this layer rather than by the workspace column.

use std::collections::HashMap;
use ui::widget::TaffyTreeExt;
use ui::widget::{AppModel, ModelPath};

use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use crate::shell::chrome_state::Chrome;
use crate::shell::topbar_chrome::{TOPBAR_HEIGHT_PX, TopBarChrome};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;

/// One compact bar per display other than the primary.
pub struct DisplayBars {
    chrome: Chrome,
    bars: Vec<(u32, TopBarChrome)>,
    rect: Rect,
    /// Last pointer position, so a button event reaches the bar under it.
    cursor: Option<Point>,
    /// The bar that took the last press, so its release lands there too.
    pressed: Option<u32>,
}

impl DisplayBars {
    pub fn new(chrome: Chrome) -> Self {
        Self {
            chrome,
            bars: Vec::new(),
            rect: Rect::ZERO,
            cursor: None,
            pressed: None,
        }
    }

    /// The displays that get a compact bar, as `(output id, logical rect)`.
    /// Bars follow their display; a display that went away takes its bar.
    pub fn set_displays(&mut self, displays: &[(u32, Rect)]) {
        self.bars
            .retain(|(id, _)| displays.iter().any(|(want, _)| want == id));
        for &(id, rect) in displays {
            let at = Rect::new(
                rect.x0,
                rect.y0,
                rect.x1,
                rect.y0 + f64::from(TOPBAR_HEIGHT_PX),
            );
            if let Some((_, bar)) = self.bars.iter_mut().find(|(have, _)| *have == id) {
                bar.set_placement(Some(at));
                continue;
            }
            let bar = {
                let state = self.chrome.borrow();
                let tree = state.tree();
                let local = state
                    .explicit_local_user()
                    .map_or_else(crate::shell::state::local_user_id, str::to_owned);
                TopBarChrome::from_tree_with_local_user(&tree.desktop, local)
            };
            let mut bar = bar
                .compact()
                .with_chrome(crate::shell::topbar_chrome::native_source(
                    self.chrome.clone(),
                ));
            bar.set_placement(Some(at));
            self.bars.push((id, bar));
        }
    }

    /// Each compact bar's display and where it is drawn.
    pub fn bar_rects(&self) -> Vec<(u32, Rect)> {
        self.bars
            .iter()
            .filter(|(id, _)| !self.suppressed(*id))
            .map(|(id, bar)| (*id, bar.bar_rect()))
            .collect()
    }

    /// A fullscreen owns its display: the bar on that display steps aside,
    /// the bars on the others stay.
    fn suppressed(&self, display: u32) -> bool {
        self.chrome.borrow().fullscreen_covers_display(display)
    }

    fn bar_under(&self, p: Point) -> Option<u32> {
        self.bars
            .iter()
            .filter(|(id, _)| !self.suppressed(*id))
            .find(|(_, bar)| bar.bar_rect().contains(p))
            .map(|(id, _)| *id)
    }
}

impl Widget for DisplayBars {
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn sync(&mut self, state: &AppModel, root: &ModelPath) {
        for (_, bar) in &mut self.bars {
            bar.sync(state, root);
        }
    }

    fn update(&mut self, state: &AppModel, root: &ModelPath) -> bool {
        let mut changed = false;
        for (_, bar) in &mut self.bars {
            changed |= bar.update(state, root);
        }
        changed
    }

    /// The bars hold no children for the default walk, so their clocks'
    /// deadlines are gathered here.
    fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        self.bars
            .iter()
            .filter_map(|(_, bar)| bar.next_wake_deadline())
            .min()
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // The bars are placed on their displays directly; this layer takes
        // no room in the desktop's layout.
        taffy.widget_leaf(Style::default(), node_map, self)
    }

    ui::rect_geometry!();

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let chrome = self.chrome.clone();
        for (id, bar) in &mut self.bars {
            if chrome.borrow().fullscreen_covers_display(*id) {
                continue;
            }
            bar.draw(scene, transform, text_renderer, theme, presence);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        let target = match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = ctx.logical(position);
                self.cursor = Some(p);
                self.bar_under(p)
            }
            WindowEvent::MouseInput {
                state: winit::event::ElementState::Pressed,
                ..
            } => {
                self.pressed = self.cursor.and_then(|p| self.bar_under(p));
                self.pressed
            }
            WindowEvent::MouseInput {
                state: winit::event::ElementState::Released,
                ..
            } => self.pressed.take(),
            _ => None,
        };
        let Some(target) = target else {
            return;
        };
        if let Some((_, bar)) = self.bars.iter_mut().find(|(id, _)| *id == target) {
            bar.handle_event(event, ctx);
        }
    }

    fn needs_redraw(&self) -> bool {
        self.bars.iter().any(|(_, bar)| bar.needs_redraw())
    }

    fn test_rects(&self) -> Vec<(String, Rect)> {
        self.bars
            .iter()
            .filter(|(id, _)| !self.suppressed(*id))
            .flat_map(|(id, bar)| {
                let mut named = vec![(format!("topbar_compact[{id}]"), bar.bar_rect())];
                named.extend(
                    bar.test_rects()
                        .into_iter()
                        .map(|(name, rect)| (format!("{name}[display {id}]"), rect)),
                );
                named
            })
            .collect()
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, Role};
        let children: Vec<AccessNode> = self
            .bars
            .iter()
            .filter(|(id, _)| !self.suppressed(*id))
            .filter_map(|(_, bar)| {
                bar.accessibility()
                    .map(|node| node.with_bounds(bar.bar_rect()))
            })
            .collect();
        if children.is_empty() {
            return None;
        }
        Some(
            AccessNode::new(Role::Group)
                .with_name(crate::shell::i18n::t("status-other-displays-a11y"))
                .with_children(children),
        )
    }

    fn name(&self) -> &'static str {
        "DisplayBars"
    }
}
