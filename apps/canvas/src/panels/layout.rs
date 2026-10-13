//! Canvas panel composition resolves its measure from the current theme.
//! This decorator only changes layout; shared widgets retain interaction,
//! accessibility, drawing and resource state.

use std::collections::HashMap;

use ui::Theme;
use ui::prelude::Element;
use ui::taffy::{NodeId, Style, TaffyTree};
use ui::widget::{Decorator, Widget};

type ResolveLayout = dyn Fn(&Theme, &mut Style);

struct ThemedLayout {
    child: Element,
    resolve: Box<ResolveLayout>,
}

impl Decorator for ThemedLayout {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.child.as_ref()
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.child.as_mut()
    }

    fn build_layout(
        &mut self,
        tree: &mut TaffyTree<()>,
        theme: &Theme,
        nodes: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = self.child.build_layout(tree, theme, nodes);
        #[allow(
            clippy::expect_used,
            reason = "build_layout just created this node in this tree, as in TaffyTreeExt"
        )]
        let mut style = tree.style(node).expect("child layout node").clone();
        (self.resolve)(theme, &mut style);
        #[allow(
            clippy::expect_used,
            reason = "the freshly built node remains in this tree while its style is resolved"
        )]
        tree.set_style(node, style).expect("child layout style");
        node
    }
}

pub(super) fn themed(child: Element, resolve: impl Fn(&Theme, &mut Style) + 'static) -> Element {
    Box::new(ThemedLayout {
        child,
        resolve: Box::new(resolve),
    })
}

pub(super) fn width(
    child: Element,
    measure: impl Fn(&ui::tokens::Metrics) -> f32 + 'static,
) -> Element {
    themed(child, move |theme, style| {
        style.size.width = ui::taffy::Dimension::length(measure(&theme.tokens.metrics));
    })
}

/// Layout cache announcing the viewport's size class to retained composition.
/// It holds no artwork state and delegates all controls and input unchanged.
pub(crate) struct ViewportMode<T: Widget> {
    child: T,
    compact: ui::LocalState<bool>,
    threshold: f64,
}

impl<T: Widget> ViewportMode<T> {
    pub(crate) fn new(child: T, compact: ui::LocalState<bool>) -> Self {
        Self {
            child,
            compact,
            threshold: 0.0,
        }
    }
}

impl<T: Widget + 'static> Decorator for ViewportMode<T> {
    type Inner = T;
    fn inner(&self) -> &T {
        &self.child
    }
    fn inner_mut(&mut self) -> &mut T {
        &mut self.child
    }
    fn build_layout(
        &mut self,
        tree: &mut TaffyTree<()>,
        theme: &Theme,
        nodes: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.threshold = f64::from(theme.tokens.metrics.content_min_width);
        self.child.build_layout(tree, theme, nodes)
    }
    fn apply_layout(
        &mut self,
        tree: &TaffyTree<()>,
        node: NodeId,
        origin: ui::vello::kurbo::Point,
    ) {
        self.child.apply_layout(tree, node, origin);
        let rect = self.child.get_rect();
        let compact = rect.width().min(rect.height()) < self.threshold;
        if self.compact.get() != compact {
            self.compact.set(compact);
        }
    }
}
