//! One part of a view, chosen by view-local state.

use std::collections::HashMap;
use std::rc::Rc;
use tilo_ui_base::{widget::AppModel, widget::ModelPath};

use taffy::{NodeId, TaffyTree};

use tilo_ui_base::widget::DrawChild;
use tilo_ui_base::{LocalState, Theme};
use tilo_ui_base::{widget::Element, widget::Widget};
use ui_core::taffy;
use ui_core::vello::kurbo::{Point, Rect};
use ui_core::winit::event::WindowEvent;

type BuildPart<K> = Box<dyn Fn(&K, &Rc<()>) -> Element>;

/// The part for the key now, built with the given liveness token, when the
/// key moved since the part shown was built, or when the part is to be built
/// again whatever the key (`true`); `None` otherwise.
type Rebuild = Box<dyn FnMut(&Rc<()>, bool) -> Option<Element>>;

/// Whether the part shown, kept alive by the token this is handed, read something as not
/// here while it was built that has come since: it is then built again for the key it has.
pub(crate) type Arrived = Box<dyn Fn(&Rc<()>) -> bool>;

/// The part [`ViewCx::switch`](super::ViewCx::switch) shows for the current
/// key. Rebuilt when the key changes; dropping the old part retires every
/// binding it registered. The key's type stays in the constructor, so the
/// widget is one type whatever the key.
pub(crate) struct Switch {
    rebuild: Rebuild,
    /// What says the part shown is to be built again for the same key; `None` for a part
    /// that follows its key alone.
    arrived: Option<Arrived>,
    /// Dropping it retires every binding the shown part registered.
    alive: Rc<()>,
    part: Element,
    structure_dirty: bool,
}

impl Switch {
    pub(crate) fn keyed<K: Clone + PartialEq + 'static>(
        key: LocalState<K>,
        build: BuildPart<K>,
        arrived: Option<Arrived>,
    ) -> Element {
        let mut shown = key.with(Clone::clone);
        let alive = Rc::new(());
        let part = build(&shown, &alive);
        Self::element(
            Box::new(move |alive, again| {
                let current = key.with(Clone::clone);
                if current == shown && !again {
                    return None;
                }
                let part = build(&current, alive);
                shown = current;
                Some(part)
            }),
            arrived,
            alive,
            part,
        )
    }

    /// `part`, built once, whose bindings `alive` keeps until the part goes.
    pub(crate) fn mounted(alive: Rc<()>, part: Element) -> Element {
        Self::element(Box::new(|_, _| None), None, alive, part)
    }

    /// The switch as an element, its widget table compiled once whatever
    /// the key.
    fn element(
        rebuild: Rebuild,
        arrived: Option<Arrived>,
        alive: Rc<()>,
        part: Element,
    ) -> Element {
        Box::new(Self {
            rebuild,
            arrived,
            alive,
            part,
            structure_dirty: false,
        })
    }

    /// The part showing.
    pub(crate) fn part(&self) -> &dyn Widget {
        self.part.as_ref()
    }

    /// [`Self::part`], to change.
    pub(crate) fn part_mut(&mut self) -> &mut dyn Widget {
        self.part.as_mut()
    }

    /// Take the part out, with what keeps its bindings alive, for a host
    /// that shows it in the switch's place from now on; `with` stands in it.
    pub(crate) fn take_part(&mut self, with: Element) -> (Element, Rc<()>) {
        self.structure_dirty = true;
        (
            std::mem::replace(&mut self.part, with),
            std::mem::replace(&mut self.alive, Rc::new(())),
        )
    }

    fn reconcile(&mut self) {
        // What the part shown read as not here while it was built, and has come since.
        let again = self
            .arrived
            .as_ref()
            .is_some_and(|arrived| arrived(&self.alive));
        let alive = Rc::new(());
        if let Some(part) = (self.rebuild)(&alive, again) {
            self.part = part;
            self.alive = alive;
            self.structure_dirty = true;
        }
    }
}

impl Widget for Switch {
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.reconcile();
        self.part.sync(state, path);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.part.build_layout(taffy, theme, node_map)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.part.apply_layout(taffy, node, origin);
    }

    fn draw(&mut self, ctx: &mut tilo_ui_base::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text, theme, mut env) = ctx.parts();
        self.part.draw_child(scene, transform, text, theme, &env);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut tilo_ui_base::widget::EventCtx<'_>) {
        self.part.handle_event(event, ctx);
    }

    fn wants_pointer_capture(&self) -> bool {
        self.part.wants_pointer_capture()
    }

    // The switch stands for its part in its container: a part that is a
    // band on its way, or a column one moves in, moves what follows the
    // switch as it would its own followers.
    fn disclosing(&self) -> Option<tilo_ui_base::widget::Disclosing> {
        self.part.disclosing()
    }

    fn place_disclosing(&mut self, slot: Rect) {
        self.part.place_disclosing(slot);
    }

    fn is_band(&self) -> bool {
        self.part.is_band()
    }

    fn note_placement(&mut self, placement: tilo_ui_base::widget::Placement) {
        self.part.note_placement(placement);
    }

    fn paint_bounds(&self) -> Rect {
        self.part.paint_bounds()
    }

    // The switch draws nothing of its own: what changed is what changed in
    // its part, not the whole part's box.
    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        self.part.collect_damage(regions);
    }

    fn damage_covers_own_box(&self) -> bool {
        self.part.damage_covers_own_box()
    }

    fn take_structure_dirty(&mut self) -> bool {
        // The key may change between syncs (a selection made in this very
        // event), so the structure is re-checked here as well.
        self.reconcile();
        let own = std::mem::take(&mut self.structure_dirty);
        self.part.take_structure_dirty() | own
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![self.part.as_ref()]
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![self.part.as_mut()]
    }

    fn name(&self) -> &'static str {
        "Switch"
    }

    fn get_rect(&self) -> Rect {
        self.part.get_rect()
    }
}
