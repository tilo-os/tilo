//! The control under a point of the window: what a person points at when
//! they say "this" (feedback about one button, an agent asked about it).
//! Answered from the same tree assistive technology reads, naming the node
//! by its semantic id, and never by what a field holds.

use vello::kurbo::Point;

use super::{SemanticId, SemanticNode, SemanticTree};
use crate::floating_surface::PointedControl;

fn pointed(node: &SemanticNode) -> PointedControl {
    let role = match serde_json::to_value(node.role) {
        Ok(serde_json::Value::String(role)) => role,
        _ => "Generic".to_owned(),
    };
    PointedControl {
        id: node.id.0.clone(),
        role,
        label: node.name.clone().filter(|label| !label.trim().is_empty()),
    }
}

impl SemanticTree {
    /// The deepest control under `point`, in the window's logical
    /// coordinates, that a person could name: one with a durable identity
    /// or an accessible name, so a bare layout box stands for nothing. Of
    /// two at one depth the later, painted over the earlier, wins; a hidden
    /// node and all below it are skipped, and a child is looked for even
    /// outside its parent (a popover past its anchor). `None` where no
    /// control is.
    #[must_use]
    pub fn pointed_at(&self, point: Point) -> Option<PointedControl> {
        let mut best = None;
        self.visit(self.root(), point, 0, &mut best);
        best.map(|(_, node)| pointed(node))
    }

    fn visit<'a>(
        &'a self,
        id: &SemanticId,
        point: Point,
        depth: usize,
        best: &mut Option<(usize, &'a SemanticNode)>,
    ) {
        let Some(node) = self.node(id) else {
            return;
        };
        if node.state.hidden || depth > 256 {
            return;
        }
        let nameable = node.durable_identity
            || node
                .name
                .as_deref()
                .is_some_and(|name| !name.trim().is_empty());
        if node.id != *self.root()
            && nameable
            && node.bounds.is_some_and(|bounds| bounds.contains(point))
            && best.is_none_or(|(deepest, _)| depth >= deepest)
        {
            *best = Some((depth, node));
        }
        for child in &node.children {
            self.visit(child, point, depth.saturating_add(1), best);
        }
    }
}
