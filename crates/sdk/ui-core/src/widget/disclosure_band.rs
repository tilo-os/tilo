//! [`DisclosureBand`]: any content that opens and closes in place in a
//! column on the disclosure motion (`motion::Disclosure`), as a banner
//! does: it unrolls from its top while what follows moves down with its
//! edge, and when its view stops building it the column rolls it up.

use std::collections::HashMap;

use taffy::style_helpers::{auto, length, percent};
use taffy::{NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::floating_surface::DetachedContent;
use crate::widget::{BandMotion, Element, TaffyTreeExt, Widget, layout_rect};

#[path = "disclosure_band_component.rs"]
mod component;
pub use component::DisclosureBandProps;

/// Content a view builds only while it is open (a tile's details under
/// it, a row's section), standing in a column as a band
/// ([`crate::widget::Disclosing::Band`]).
///
/// Built after its window has shown, it arrives: its box opens from the
/// top over its content while the widgets after it in its column move down
/// with its edge. When its view stops building it, the column draws it
/// leaving from its last paint. A key's result lands at once, and under
/// reduced motion its height lands while its content fades.
///
/// Its `key` is its identity across rebuilds: a band built again with the
/// same key goes on from the one it replaces (at rest stays at rest,
/// opening goes on opening), while one with another key arrives as the
/// other leaves, so switching from one section to another reads as one
/// closing and the other opening.
///
/// While it moves it stands out of its column's layout and takes no input;
/// its content is laid out across its slot in a tree of its own. At rest
/// the column lays it out like any other child.
pub struct DisclosureBand {
    child: Element,
    key: String,
    /// The child's node in its host's tree, while the host lays it out
    /// (the band rests in the layout).
    child_node: Option<NodeId>,
    /// The child laid out across the band's slot, while the band moves out
    /// of the layout, and the width it was laid out at.
    detached: Option<(DetachedContent, f64)>,
    /// A host has built its layout.
    built: bool,
    rect: Rect,
    layout_dirty: bool,
    dirty: bool,
    band: BandMotion,
}

impl DisclosureBand {
    /// `child` as a band known across rebuilds by `key`.
    pub fn new(key: impl Into<String>, child: Element) -> Self {
        Self {
            child,
            key: key.into(),
            child_node: None,
            detached: None,
            built: false,
            rect: Rect::ZERO,
            layout_dirty: true,
            dirty: true,
            band: BandMotion::new(true),
        }
    }

    /// The content it shows.
    #[must_use]
    pub fn child(&self) -> &dyn Widget {
        self.child.as_ref()
    }

    /// Its identity among the motions a rebuild leaves: never one a
    /// banner's text could name.
    fn carry_key(&self) -> String {
        format!("disclosure band\u{1f}{}", self.key)
    }

    /// Whether the host's tree holds the child, against whether it should:
    /// it holds it exactly while the band rests in the layout.
    fn hosting_changed(&self) -> bool {
        self.built && self.child_node.is_some() != self.band.in_flow(true)
    }

    fn style(&self) -> Style {
        Style {
            display: if self.band.in_flow(true) {
                taffy::Display::Flex
            } else {
                taffy::Display::None
            },
            flex_direction: taffy::FlexDirection::Column,
            flex_shrink: 0.0,
            size: Size {
                width: percent(1.0_f32),
                height: auto(),
            },
            min_size: Size {
                width: length(0.0_f32),
                height: auto(),
            },
            ..Default::default()
        }
    }

    /// On its way, out of the layout: the child laid out across its slot
    /// (again when the slot's width changed or the child asks), placed at
    /// the slot's top, and the band as tall as the child is there.
    fn place_detached(&mut self, text: &mut TextRenderer, theme: &Theme) {
        let slot = self.band.laid_rect;
        let width = slot.width();
        let stale = match &self.detached {
            Some((_, laid)) => (laid - width).abs() > 0.25 || self.child.is_layout_dirty(),
            None => true,
        };
        if stale {
            let content = DetachedContent::lay_out_across(self.child.as_mut(), theme, width, text);
            self.detached = Some((content, width));
        }
        let Some((content, _)) = &self.detached else {
            return;
        };
        self.child
            .apply_layout(&content.taffy, content.node, slot.origin());
        let height = content.size().1.ceil();
        self.band.laid_rect = Rect::new(slot.x0, slot.y0, slot.x1, slot.y0 + height);
    }
}

impl Widget for DisclosureBand {
    fn update(&mut self, state: &crate::widget::AppModel, path: &crate::widget::ModelPath) -> bool {
        let changed = self.child.update(state, path);
        // A band that has come to rest takes its place in the layout.
        if self.band.layout_moved(true) {
            crate::widget::note_layout_moved();
        }
        changed
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.band.motion = theme.motion();
        // Built again by its view: go on from the band it replaces, else
        // arrive.
        let key = self.carry_key();
        self.band.build_keyed(&key);
        self.band.laid_in_flow = self.band.in_flow(true);
        self.built = true;
        // At rest the host lays the child out; on its way it is laid out
        // across its slot when drawn.
        let children = if self.band.laid_in_flow {
            self.detached = None;
            let child = self.child.build_layout(taffy, theme, node_map);
            self.child_node = Some(child);
            vec![child]
        } else {
            self.child_node = None;
            Vec::new()
        };
        let node = taffy.parent_node(self.style(), &children);
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let rect = if self.band.laid_in_flow {
            layout_rect(taffy, node, origin)
        } else {
            Rect::ZERO
        };
        if rect != self.rect {
            self.rect = rect;
            self.dirty = true;
        }
        self.layout_dirty = false;
        if !self.band.laid_in_flow {
            // On its way: its container puts it in its slot, and its
            // content is placed there when it is drawn.
            self.band.unplaced = true;
            return;
        }
        self.band.laid_rect = self.rect;
        if let Some(child) = self.child_node {
            self.child.apply_layout(taffy, child, self.rect.origin());
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
        self.band.motion = theme.motion();
        if !self.band.in_motion() {
            if self.rect.width() <= 0.0 {
                return;
            }
            // Kept, so its column can draw it leaving when its view stops
            // building it.
            let mut painted = self.band.take_painted();
            self.child
                .draw(&mut painted, Affine::IDENTITY, text, theme, presence);
            self.band.show_at_rest(painted, scene, transform);
            return;
        }
        if self.band.land_unplaced(true) {
            // Out of the layout with no container to put it in its slot:
            // it lands, and the next layout takes it in.
            self.layout_dirty = true;
            return;
        }
        if !self.band.laid_in_flow && self.band.laid_rect.width() > 0.0 {
            self.place_detached(text, theme);
        }
        if self.band.laid_rect.width() <= 0.0 {
            return;
        }
        let mut painted = self.band.take_painted();
        self.child
            .draw(&mut painted, Affine::IDENTITY, text, theme, presence);
        self.band.show_moving(painted, scene, transform);
    }

    fn needs_animation_frame(&self) -> bool {
        self.band.needs_frame(true) || self.child.needs_animation_frame()
    }

    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        if self.band.in_motion() {
            regions.push(self.band.laid_rect);
        } else if self.needs_redraw() {
            regions.push(self.rect);
        }
    }

    fn is_band(&self) -> bool {
        true
    }

    fn note_placement(&mut self, placement: crate::widget::Placement) {
        self.band.note_placement(placement);
    }

    fn place_disclosing(&mut self, slot: Rect) {
        self.band.place(slot);
    }

    fn paint_bounds(&self) -> Rect {
        self.band.paint_bounds(self.rect)
    }

    fn disclosing(&self) -> Option<crate::widget::Disclosing> {
        self.band.disclosing()
    }

    /// At rest only: on its way it takes no input, as the band its column
    /// draws leaving takes none.
    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) {
        let Some(node) = self.child_node.filter(|_| self.band.laid_in_flow) else {
            return;
        };
        crate::widget::route_to_children(
            std::slice::from_mut(&mut self.child),
            &[node],
            event,
            ctx,
        );
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![self.child.as_ref()]
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![self.child.as_mut()]
    }

    /// Dirty when it should join or leave the layout; its content counts
    /// only while the layout places it.
    fn is_layout_dirty(&self) -> bool {
        self.layout_dirty
            || self.band.layout_moved(true)
            || (self.band.laid_in_flow && self.child.is_layout_dirty())
    }

    fn layout_needs_measure(&self) -> bool {
        self.is_layout_dirty()
    }

    /// Joining or leaving the layout adds or removes the child's nodes in
    /// its host's tree.
    fn take_structure_dirty(&mut self) -> bool {
        let own = std::mem::take(&mut self.band.structure_dirty) || self.hosting_changed();
        super::defaults::take_structure_dirty(self.children_mut()) || own
    }

    fn needs_redraw(&self) -> bool {
        self.dirty || self.band.in_motion() || self.child.needs_redraw()
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        self.child.mark_drawn();
    }

    fn name(&self) -> &'static str {
        "DisclosureBand"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

/// A band a rebuild drops leaves its motion for the one put in its place,
/// or for its column to draw it leaving.
impl Drop for DisclosureBand {
    fn drop(&mut self) {
        let key = self.carry_key();
        self.band.leave(key, self.rect, true);
    }
}

#[cfg(test)]
mod tests;
