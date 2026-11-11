//! [`PageSlide`] — the page a panel shows, and the page it replaced sliding
//! out beside it when the view swaps one for another.
//!
//! A view that shows one page of a stack in one place builds the page in
//! this wrapper and gives it the depth it stands at (0 the first page, 1
//! the page it opens). On the [`Drill`](crate::motion::Drill) motion the
//! page arrives from the side the depth says, while the page it replaced
//! leaves toward the other one; Back runs it the other way. The wrapper's
//! identity is the key its view gives it ([`PageSlideProps`]), so a rebuild
//! that changes the page hands the page it showed to the one put in its
//! place, which draws it leaving and goes on from the depth the motion had
//! reached. Nothing is left when the wrapper drops with nothing put in its
//! place.

use std::collections::HashMap;

use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{BlendMode, Fill};

use crate::component::{Component, Described, SlotViews};
use crate::motion::{Drill, DrillDepth, Pose};
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::MotionSource;
use crate::widget::DrawChild;
use crate::widget::decorator::concrete::Decorator;
use crate::widget::{Element, Placement, layout_rect};

/// The identity a slide built by [`PageSlide::new`] leaves its motion under: one page slot in
/// a window.
const KEY: &str = "page slide";

/// The slot the page stands in.
const PAGE_SLOT: &str = "page";

/// One page of a stack, in the place a view shows it, with the page it replaced leaving while
/// the view swaps the two: a panel that opens a deeper page of its own, and Back that slides it
/// away again.
///
/// A page that goes deeper arrives from the side its level names, and the page it replaced
/// leaves toward the other; a page that goes shallower runs the motion the other way. A view
/// that puts a different page at the same level (a list's item opened in the page the list
/// showed) changes it where it stands, and nothing moves.
#[derive(Component, Default)]
#[component(name = "PageSlide", role = "Group")]
pub struct PageSlideProps {
    /// Its stable key.
    #[component(key)]
    pub key: String,
    /// How deep the page stands: 0 the first page, 1 the page it opens.
    pub level: i64,
    /// The page it shows.
    #[component(slot(default))]
    pub page: Option<Element>,
}

/// One page of a stack, drawn in place, with the page it replaced leaving.
pub struct PageSlide {
    /// The identity it leaves its motion under, so the slide a rebuild puts in its place takes
    /// it up.
    kept_as: String,
    page: Element,
    /// How deep the page stands: 0 the first page, 1 the one it opens.
    level: usize,
    depth: DrillDepth,
    /// The level the motion was last aimed at.
    aimed: Option<usize>,
    /// The page this one replaced, drawn leaving until the motion rests.
    leaving: Option<Leaving>,
    rect: Rect,
    dirty: bool,
    layout_dirty: bool,
}

/// A page on its way out, with the layout it was drawn at.
struct Leaving {
    page: Element,
    level: usize,
    rect: Rect,
}

/// What a page slot a rebuild dropped leaves for the one put in its place.
struct Left {
    depth: DrillDepth,
    page: Element,
    level: usize,
    rect: Rect,
}

impl PageSlide {
    /// `page`, standing `level` deep in its stack.
    pub fn new(level: usize, page: Element) -> Self {
        Self {
            kept_as: KEY.to_owned(),
            page,
            level,
            depth: DrillDepth::default(),
            aimed: None,
            leaving: None,
            rect: Rect::ZERO,
            dirty: true,
            layout_dirty: true,
        }
    }

    /// The page it shows.
    #[must_use]
    pub fn page(&self) -> &dyn crate::widget::Widget {
        self.page.as_ref()
    }

    /// The page an empty slot stands on.
    fn no_page() -> Element {
        Box::new(crate::widget::Spacer::new())
    }

    /// Stand a page where this one stands.
    fn put(&mut self, page: Element) {
        self.page = page;
        self.dirty = true;
        self.layout_dirty = true;
    }

    /// Whether it is drawing a page it replaced on its way out.
    #[cfg(test)]
    pub(crate) fn is_leaving(&self) -> bool {
        self.leaving.is_some()
    }

    /// Draw `page` under `pose`, clipped to `clip`.
    fn draw_posed(
        page: &mut Element,
        pose: Pose,
        clip: Rect,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if pose.opacity <= 0.0 || clip.width() <= 0.0 {
            return;
        }
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            pose.opacity,
            transform,
            &clip,
        );
        page.draw_child(scene, transform * pose.transform, text, theme, presence);
        scene.pop_layer();
    }
}

fn level_of(level: usize) -> f64 {
    f64::from(u32::try_from(level).unwrap_or(u32::MAX))
}

/// The depth a page's level names; a negative level is the first page.
fn depth_of(level: i64) -> usize {
    usize::try_from(level).unwrap_or(0)
}

impl Described for PageSlide {
    type Props = PageSlideProps;

    fn from_props(key: &str, props: PageSlideProps) -> Self {
        let PageSlideProps {
            key: _,
            level,
            page,
        } = props;
        let mut slide = Self::new(depth_of(level), page.unwrap_or_else(Self::no_page));
        key.clone_into(&mut slide.kept_as);
        slide
    }

    /// A page at the level it stood at takes its changed props where it stands: the page its
    /// slot swapped for another came through [`Self::slot_views`]. One that goes deeper or
    /// shallower is built again, so the slide the rebuild puts in its place takes up the page
    /// this one showed and runs the drill motion with it.
    fn set_props(&mut self, props: PageSlideProps) -> bool {
        depth_of(props.level) == self.level
    }

    fn slot_views(&mut self, slot: &str) -> Option<&mut dyn SlotViews> {
        (slot == PAGE_SLOT).then_some(self as &mut dyn SlotViews)
    }
}

/// The page, the slot's one view, which changes where it stands.
impl SlotViews for PageSlide {
    fn len(&self) -> usize {
        1
    }

    fn view_mut(&mut self, index: usize) -> Option<&mut Element> {
        (index == 0).then_some(&mut self.page)
    }

    fn insert(&mut self, _index: usize, view: Element) {
        self.put(view);
    }

    fn remove(&mut self, _index: usize) {
        self.put(Self::no_page());
    }

    fn move_view(&mut self, _from: usize, _to: usize) {}

    fn replace(&mut self, _index: usize, view: Element) {
        self.put(view);
    }
}

impl Decorator for PageSlide {
    type Inner = dyn crate::widget::Widget;

    fn inner(&self) -> &(dyn crate::widget::Widget + 'static) {
        self.page.as_ref()
    }

    fn inner_mut(&mut self) -> &mut (dyn crate::widget::Widget + 'static) {
        self.page.as_mut()
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn crate::widget::Widget>,
    ) -> NodeId {
        // Built again where a page stood: go on from the page it dropped
        // and the depth its motion had reached, and draw that page leaving.
        if self.aimed.is_none()
            && let Some(left) = crate::motion::take_kept::<Left>(&self.kept_as)
        {
            self.depth = left.depth;
            self.leaving = Some(Leaving {
                page: left.page,
                level: left.level,
                rect: left.rect,
            });
            self.dirty = true;
        }
        if self.aimed != Some(self.level) {
            self.depth.go(
                self.level,
                theme.motion(),
                MotionSource::Pointer,
                crate::motion::frame_now(),
            );
            self.aimed = Some(self.level);
        }
        self.page.build_layout(taffy, theme, node_map)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.page.apply_layout(taffy, node, origin);
        let rect = layout_rect(taffy, node, origin);
        if rect != self.rect {
            self.rect = rect;
            self.dirty = true;
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
        let now = crate::motion::frame_now();
        let motion = theme.motion();
        let moving = self.depth.is_moving(now);
        let depth = self.depth.get(now);
        let width = self.rect.width();
        let clip = self.rect;
        if moving {
            if let Some(leaving) = &mut self.leaving {
                let pose = Drill::pose(level_of(leaving.level) - depth, width, motion);
                let Leaving { page, rect, .. } = leaving;
                Self::draw_posed(
                    page,
                    pose,
                    if clip.width() > 0.0 { clip } else { *rect },
                    scene,
                    transform,
                    text,
                    theme,
                    presence,
                );
            }
        } else if self.leaving.take().is_some() {
            self.dirty = true;
        }
        let pose = Drill::pose(level_of(self.level) - depth, width, motion);
        Self::draw_posed(
            &mut self.page,
            pose,
            clip,
            scene,
            transform,
            text,
            theme,
            presence,
        );
    }

    fn needs_animation_frame(&self) -> bool {
        self.depth.is_moving(crate::motion::frame_now()) || self.page.needs_animation_frame()
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
            || self.leaving.is_some()
            || self.page.needs_redraw()
            || crate::widget::Widget::needs_animation_frame(self)
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        self.page.mark_drawn();
    }

    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        if self.needs_redraw() {
            regions.push(self.paint_bounds());
        }
    }

    fn paint_bounds(&self) -> Rect {
        match &self.leaving {
            Some(leaving) => self.rect.union(leaving.rect),
            None => self.rect,
        }
    }

    fn note_placement(&mut self, placement: Placement) {
        self.page.note_placement(placement);
    }

    fn is_layout_dirty(&self) -> bool {
        self.layout_dirty || self.page.is_layout_dirty()
    }

    fn layout_measured(&mut self) {
        self.layout_dirty = false;
    }
}

/// A page slot a rebuild drops hands its page and its motion to the slot
/// put in its place.
impl Drop for PageSlide {
    fn drop(&mut self) {
        if self.rect.width() <= 0.0 {
            return;
        }
        let page = std::mem::replace(&mut self.page, Box::new(crate::widget::Spacer::new()));
        crate::motion::keep(
            std::mem::take(&mut self.kept_as),
            Some(self.rect),
            Left {
                depth: self.depth,
                page,
                level: self.level,
                rect: self.rect,
            },
        );
    }
}

#[cfg(test)]
#[path = "page_slide_tests.rs"]
mod tests;
