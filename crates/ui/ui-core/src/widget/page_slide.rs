//! [`PageSlide`] — the page a panel shows, and the page it replaced sliding out beside it when
//! the view swaps one for another.

use std::collections::HashMap;

use taffy::{NodeId, TaffyTree};
use vello::kurbo::{Point, Rect};
use vello::peniko::{BlendMode, Fill};

use tilo_ui_base::{component::Component, component::Described, component::PartViews};
use tilo_ui_base::{motion::Drill, motion::DrillDepth, motion::Pose};

use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::MotionSource;
use tilo_ui_base::widget::DrawChild;
use tilo_ui_base::widget::identity::concrete::Decorator;
use tilo_ui_base::{widget::Element, widget::Placement, widget::layout_rect};

/// The identity a slide built by [`PageSlide::new`] leaves its motion under: one page part in
/// a window.
const KEY: &str = "page slide";

const PAGE_PART: &str = "page";

/// One page of a stack that a view shows in place, the replaced page leaving as it swaps: a
/// panel opening a deeper page, and Back sliding it away.
#[derive(Component, Default)]
#[component(name = "PageSlide", role = "Group")]
pub struct PageSlideProps {
    /// Its stable key.
    #[component(key)]
    pub key: String,
    /// How deep the page is: 0 the first page, 1 the page it opens.
    pub level: i64,
    /// The page it shows.
    #[component(part(default))]
    pub page: Option<Element>,
}

pub struct PageSlide {
    kept_as: String,
    page: Element,
    /// How deep the page stands: 0 the first page, 1 the one it opens.
    level: usize,
    depth: DrillDepth,
    aimed: Option<usize>,
    /// The page this one replaced, drawn leaving until the motion rests.
    leaving: Option<Leaving>,
    rect: Rect,
    dirty: bool,
    layout_dirty: bool,
}

struct Leaving {
    page: Element,
    level: usize,
    rect: Rect,
}

struct Left {
    depth: DrillDepth,
    page: Element,
    level: usize,
    rect: Rect,
}

impl PageSlide {
    /// `page`, `level` deep in its stack.
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

    #[must_use]
    pub fn page(&self) -> &dyn tilo_ui_base::widget::Widget {
        self.page.as_ref()
    }

    fn no_page() -> Element {
        Box::new(tilo_ui_base::widget::Spacer::new())
    }

    fn put(&mut self, page: Element) {
        self.page = page;
        self.dirty = true;
        self.layout_dirty = true;
    }

    #[cfg(test)]
    pub(crate) fn is_leaving(&self) -> bool {
        self.leaving.is_some()
    }

    /// Draw `page` under `pose`, clipped to `clip`.
    fn draw_posed(
        page: &mut Element,
        pose: Pose,
        clip: Rect,
        ctx: &mut tilo_ui_base::draw::DrawCtx<'_>,
    ) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text, theme, mut env) = ctx.parts();
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
        page.draw_child(scene, transform * pose.transform, text, theme, &env);
        scene.pop_layer();
    }
}

impl PageSlide {
    /// Where the page it shows is drawn: its box, and past its foot what follows a band on its
    /// way in the page.
    fn shown_rect(&self) -> Rect {
        let grows =
            tilo_ui_base::widget::Widget::disclosing(self).map_or(0.0, |moving| moving.reach(0.0));
        self.rect
            .with_size((self.rect.width(), self.rect.height() + grows))
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
    /// part swapped for another came through [`Self::part_views`].
    fn set_props(&mut self, props: PageSlideProps) -> bool {
        depth_of(props.level) == self.level
    }

    fn part_views(&mut self, part: &str) -> Option<&mut dyn PartViews> {
        (part == PAGE_PART).then_some(self as &mut dyn PartViews)
    }
}

impl PartViews for PageSlide {
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
    type Inner = dyn tilo_ui_base::widget::Widget;

    fn inner(&self) -> &(dyn tilo_ui_base::widget::Widget + 'static) {
        self.page.as_ref()
    }

    fn inner_mut(&mut self) -> &mut (dyn tilo_ui_base::widget::Widget + 'static) {
        self.page.as_mut()
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn tilo_ui_base::widget::Widget>,
    ) -> NodeId {
        if self.aimed.is_none()
            && let Some(left) = tilo_ui_base::motion::take_kept::<Left>(&self.kept_as)
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
                tilo_ui_base::motion::frame_now(),
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

    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text, theme, mut env) = ctx.parts();
        let now = tilo_ui_base::motion::frame_now();
        let motion = theme.motion();
        let moving = self.depth.is_moving(now);
        let depth = self.depth.get(now);
        let width = self.rect.width();
        let clip = self.shown_rect();
        if moving {
            if let Some(leaving) = &mut self.leaving {
                let pose = Drill::pose(level_of(leaving.level) - depth, width, motion);
                let Leaving { page, rect, .. } = leaving;
                Self::draw_posed(
                    page,
                    pose,
                    if clip.width() > 0.0 { clip } else { *rect },
                    &mut tilo_ui_base::draw::DrawCtx::new(
                        scene,
                        transform,
                        text,
                        theme,
                        env.clone(),
                    ),
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
            &mut tilo_ui_base::draw::DrawCtx::new(scene, transform, text, theme, env.clone()),
        );
    }

    fn needs_animation_frame(&self) -> bool {
        self.depth.is_moving(tilo_ui_base::motion::frame_now()) || self.page.needs_animation_frame()
    }

    /// A band opening or closing in the page it shows moves what follows the slide, as one in a
    /// column would; a page on its way in or out moves nothing past it.
    fn disclosing(&self) -> Option<tilo_ui_base::widget::Disclosing> {
        self.leaving
            .is_none()
            .then(|| self.page.disclosing())
            .flatten()
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
            || self.leaving.is_some()
            || self.page.needs_redraw()
            || tilo_ui_base::widget::Widget::needs_animation_frame(self)
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
            None => self.shown_rect(),
        }
    }

    fn note_placement(&mut self, placement: Placement) {
        self.page.note_placement(placement);
    }

    fn is_layout_dirty(&self) -> bool {
        self.layout_dirty || self.page.is_layout_dirty()
    }

    /// The slide stands in for its page at the page's own node, so the solve that measured that
    /// node measured the page: its request is met with the slide's.
    fn layout_measured(&mut self) {
        self.layout_dirty = false;
        self.page.layout_measured();
    }
}

impl Drop for PageSlide {
    fn drop(&mut self) {
        if self.rect.width() <= 0.0 {
            return;
        }
        let page = std::mem::replace(
            &mut self.page,
            Box::new(tilo_ui_base::widget::Spacer::new()),
        );
        tilo_ui_base::motion::keep(
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
