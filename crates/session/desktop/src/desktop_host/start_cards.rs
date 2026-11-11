//! The start cards on a desk with no window: one pad per display that shows none, centred in that
//! display, so a desk spanning several displays shows the cards on each empty display rather than
//! one grid across them.
//! A pad asks for nothing of its own: what a person chooses on it leaves as the host action its
//! card declares, the same door every other host action comes through.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use taffy::{NodeId, Style, TaffyTree};
use tilo_desktop_model::DesktopTree;
use tilo_desktop_surface::DesktopHandle;
use ui::presence::PresenceCtx;
use ui::search::Scope;
use ui::widget::{AppModel, EventCtx, ModelPath, TaffyTreeExt as _, Widget};
use ui::{TextRenderer, Theme};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use super::{DesktopAsk, desk_is_empty, fill};
use crate::launchpad::{CollaborationLaunchpad, LaunchpadPress};
use ui::widget::DrawChild;

/// Where one display shows the start cards: which display it belongs to,
/// where that display stands in the desk, whether it shows no window, and
/// the launchpad itself.
pub(super) struct DisplayPad {
    /// The display it belongs to; `None` on a desk of one display, where the cards take the whole
    /// desk.
    pub(super) display: Option<u32>,
    /// The display's top-left corner in the desk's coordinates, and its extent: the part of the
    /// desk its cards centre in.
    pub(super) place: Rect,
    /// Whether the display shows no window, and so shows its start cards.
    pub(super) shown: bool,
    pub(super) launchpad: CollaborationLaunchpad,
}

impl DisplayPad {
    /// Centre the cards across this display's part of the desk, so an empty display shows its own
    /// cards rather than a grid spanning the desk.
    pub(super) fn present(&mut self) {
        let rect = self.launchpad.get_rect();
        let area = self.display.map(|_| {
            Rect::new(
                rect.x0 + self.place.x0,
                rect.y0 + self.place.y0,
                rect.x0 + self.place.x1,
                rect.y0 + self.place.y1,
            )
        });
        self.launchpad.set_presentation_area(area);
    }
}

/// What one display's pad shows now.
pub(super) struct PadWant {
    pub(super) display: Option<u32>,
    pub(super) place: Rect,
    pub(super) shown: bool,
}

/// The start cards of every display that shows no window. A desk spanning several displays shows
/// them on each empty display, centred there; a desk of one display shows them once, over the whole
/// desk, while it has no window. What a person chooses on any of them leaves as the host action its
/// card declares ([`LaunchpadAction::host_id`]), the same door every other host action comes
/// through.
pub(super) struct EmptyWorkspace {
    /// What each pad is made from: the words and scope the host gave it.
    pub(super) template: CollaborationLaunchpad,
    pub(super) pads: Vec<DisplayPad>,
    /// Whether the pads were rebuilt, so the desk's layout has to follow.
    pub(super) rebuilt: bool,
    pub(super) handle: DesktopHandle,
    pub(super) asks: Rc<RefCell<Vec<DesktopAsk>>>,
}

impl EmptyWorkspace {
    /// What each display's pad should show now: on a desk spanning several displays, one pad per
    /// display in reading order, shown while the strip that display shows has no window homed
    /// there; on a desk of one display, one pad over the whole desk while it has no window.
    pub(super) fn wants(tree: &DesktopTree) -> Vec<PadWant> {
        if !tree.is_partitioned() {
            return vec![PadWant {
                display: None,
                place: Rect::ZERO,
                shown: desk_is_empty(tree),
            }];
        }
        let Some(workspace) = tree.active_workspace() else {
            return Vec::new();
        };
        tree.outputs
            .reading_order()
            .into_iter()
            .filter_map(|display| {
                let at = tree
                    .outputs
                    .displays
                    .iter()
                    .find(|shown| shown.id == display)?;
                let strip = tree
                    .strip_shown_on(display)
                    .and_then(|id| workspace.strips.iter().find(|strip| strip.id == id));
                let empty =
                    strip.is_none_or(|strip| tree.outputs.groups_on(strip, display).is_empty());
                Some(PadWant {
                    display: Some(display),
                    place: Rect::new(at.x, at.y, at.x + at.width, at.y + at.height),
                    shown: empty,
                })
            })
            .collect()
    }

    /// The pad the person works on: the display in use while it shows no window, else the first one
    /// that does. Its cards are the ones the reader and the keyboard address; the others are the
    /// same cards shown on their own displays.
    fn working_pad(&self) -> Option<usize> {
        let in_use = self.handle.read(|tree| tree.outputs.focused_display());
        self.pads
            .iter()
            .position(|pad| pad.shown && pad.display.is_some() && pad.display == in_use)
            .or_else(|| self.pads.iter().position(|pad| pad.shown))
    }
}

impl Widget for EmptyWorkspace {
    fn update(&mut self, _state: &AppModel, _path: &ModelPath) -> bool {
        let wants = self.handle.read(Self::wants);
        let same_displays = wants.len() == self.pads.len()
            && wants
                .iter()
                .zip(&self.pads)
                .all(|(want, pad)| want.display == pad.display);
        let mut changed = false;
        if !same_displays {
            self.pads = wants
                .iter()
                .map(|want| DisplayPad {
                    display: want.display,
                    place: want.place,
                    shown: want.shown,
                    launchpad: self.template.for_another_display(),
                })
                .collect();
            self.rebuilt = true;
            changed = true;
        }
        for (want, pad) in wants.iter().zip(&mut self.pads) {
            if pad.place != want.place || pad.shown != want.shown {
                pad.place = want.place;
                pad.shown = want.shown;
                changed = true;
            }
        }
        changed
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // Each launchpad keeps the node it is drawn in, so an action addressed to a control of it
        // reaches that launchpad; this workspace is the node around them rather than a second
        // claimant of one.
        let pads: Vec<NodeId> = self
            .pads
            .iter_mut()
            .map(|pad| {
                let node = pad.launchpad.build_layout(taffy, theme, map);
                fill(taffy, node);
                node
            })
            .collect();
        let style = Style {
            display: taffy::Display::Flex,
            size: taffy::Size::from_percent(1.0_f32, 1.0_f32),
            ..Default::default()
        };
        taffy.widget_node(style, &pads, map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let rect = ui::widget::layout_rect(taffy, node, origin);
        let inner = Point::new(rect.x0, rect.y0);
        let children = taffy.children(node).unwrap_or_default();
        for (pad, child) in self.pads.iter_mut().zip(children) {
            pad.launchpad.apply_layout(taffy, child, inner);
            pad.present();
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        for pad in &mut self.pads {
            if pad.shown {
                pad.launchpad
                    .draw_child(scene, transform, text, theme, presence);
            }
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        let children = ctx.taffy.children(ctx.node_id).unwrap_or_default();
        for (pad, node) in self.pads.iter_mut().zip(children).rev() {
            if !pad.shown {
                continue;
            }
            pad.launchpad
                .handle_event(event, &mut ctx.reborrow(ctx.path, node));
            if *ctx.handled {
                break;
            }
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.working_pad()
            .and_then(|at| self.pads.get(at))
            .map(|pad| vec![&pad.launchpad as &dyn Widget])
            .unwrap_or_default()
    }

    /// Every pad is laid out in this node, also the ones whose display shows a window and so draws
    /// and routes nothing: a pad is a place in the retained tree, not only something the person can
    /// reach.
    fn for_each_layout_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) -> bool {
        for pad in &self.pads {
            visitor(&pad.launchpad);
        }
        true
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let at = self.working_pad();
        at.and_then(|at| self.pads.get_mut(at))
            .map(|pad| vec![&mut pad.launchpad as &mut dyn Widget])
            .unwrap_or_default()
    }

    fn take_structure_dirty(&mut self) -> bool {
        std::mem::take(&mut self.rebuilt)
    }

    fn needs_redraw(&self) -> bool {
        self.pads.iter().any(|pad| {
            pad.shown && (pad.launchpad.needs_redraw() || pad.launchpad.needs_animation_frame())
        })
    }

    fn is_layout_dirty(&self) -> bool {
        false
    }

    fn settle(&mut self, _edits: &mut ui::widget::Edits<'_>, _path: &ui::automerge::ObjId) {
        for pad in &self.pads {
            match pad.launchpad.take_press() {
                Some(LaunchpadPress::Card(card)) => self
                    .asks
                    .borrow_mut()
                    .push(DesktopAsk::HostAction(card.host_id())),
                Some(LaunchpadPress::OpenApp) => {
                    self.handle.open_search(Scope::WindowGroup(None), None);
                }
                None => {}
            }
        }
    }

    fn name(&self) -> &'static str {
        "EmptyWorkspace"
    }

    fn get_rect(&self) -> Rect {
        self.pads
            .iter()
            .find(|pad| pad.shown)
            .map_or(Rect::ZERO, |pad| pad.launchpad.get_rect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_desktop_model::{
        Collaborator, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace,
        WorkspaceSlot, partition::Display,
    };

    fn group(id: u64) -> WindowGroup {
        WindowGroup {
            id,
            active_tab: id.saturating_add(1000),
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
            tabs: vec![Tab {
                id: id.saturating_add(1000),
                title: "Notes".into(),
                app_title: None,
                tile: TileContent::AppPlaceholder {
                    kind: tilo_desktop_model::AppKind::Other("so.tilo.notes".into()),
                    label: "Notes".into(),
                },
                view: String::new(),
                pinned: false,
            }],
        }
    }

    fn display(id: u32, monitor: &str, x: f64) -> Display {
        Display {
            id,
            monitor: monitor.into(),
            x,
            y: 0.0,
            width: 1280.0,
            height: 800.0,
            primary: false,
        }
    }

    /// A workspace of two strips: Main holding one window, Games empty.
    fn desk() -> DesktopTree {
        let mut workspace = Workspace::new(WorkspaceSlot(1), "Home");
        workspace.collaborators = Vec::<Collaborator>::new();
        let mut main = Strip::new(StripId(10), "Main");
        main.window_groups = vec![group(1)];
        workspace.strips = vec![main, Strip::new(StripId(11), "Games")];
        let mut tree = DesktopTree {
            active: workspace.slot,
            workspaces: vec![workspace],
            ..DesktopTree::default()
        };
        tree.active_strip.insert(WorkspaceSlot(1), StripId(10));
        tree
    }

    /// On two displays only the display showing no window shows the start cards, and its cards
    /// centre in that display rather than across the desk.
    #[test]
    fn an_empty_display_shows_its_own_start_cards() {
        let mut tree = desk();
        let displays = vec![display(0, "panel", 0.0), display(1, "monitor", 1280.0)];
        tree.outputs.reconcile(&tree.workspaces, &displays);
        tree.outputs.set_focused(0);
        assert!(tree.outputs.set_shown(1, WorkspaceSlot(1), StripId(11)));

        let wants = EmptyWorkspace::wants(&tree);
        assert_eq!(wants.len(), 2, "one pad per display");
        assert_eq!(wants[0].display, Some(0));
        assert!(!wants[0].shown, "the panel shows the window");
        assert_eq!(wants[1].display, Some(1));
        assert!(wants[1].shown, "the monitor shows an empty strip");
        assert_eq!(wants[1].place, Rect::new(1280.0, 0.0, 2560.0, 800.0));
    }

    /// A desk of one display shows the start cards once, over the whole desk, while it has no
    /// window.
    #[test]
    fn a_desk_of_one_display_shows_the_cards_while_it_has_no_window() {
        let mut tree = desk();
        let wants = EmptyWorkspace::wants(&tree);
        assert_eq!(wants.len(), 1);
        assert_eq!(wants[0].display, None);
        assert!(!wants[0].shown, "the strip has a window");

        tree.workspaces[0].strips[0].window_groups.clear();
        let wants = EmptyWorkspace::wants(&tree);
        assert!(wants[0].shown, "nothing on the desk");
    }
}
