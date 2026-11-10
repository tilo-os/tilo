//! The one depth-first walk every view of the accessibility tree takes over
//! the widgets: the AccessKit bridge, the retained
//! [`SemanticTree`](super::SemanticTree) and the built-in reader's reading
//! order. It owns what they must agree on: the bound-dispatch traversal
//! guard, collapsing a widget that exports no node of its own into its
//! parent, which children are walked where ([`exported_children`]) and how
//! the widgets hosted in a synthetic child are reached ([`hosted_in`]). Each
//! view builds its own nodes from it.

use super::{
    ExportedChildren, HostedByIndex, HostedHere, HostedWidget, exported_children, hosted_in,
};
use crate::a11y::AccessNode;
use crate::widget::Widget;

/// What a view makes of one widget.
pub(crate) enum Visit<P, S> {
    /// No node of its own (`accessibility() == None`, ARIA's
    /// `role="presentation"`): its children attach where it would have.
    Collapse,
    /// Neither it nor anything under it is wanted.
    Skip,
    /// A node: `P` is what [`A11yVisitor::close`] finishes it from, `S` the
    /// scope its children attach in.
    Node(P, S),
}

/// One view of the walk.
pub(crate) trait A11yVisitor: Sized {
    /// Where nodes attach: a parent and the children gathered under it.
    type Scope;
    /// A widget's node opened but not yet closed.
    type Pending;
    /// A synthetic node opened but not yet closed.
    type SyntheticPending;

    /// `slot` is the widget's parent and its index there when the parent
    /// walks it as its own child ([`Widget::accessibility_child_description`]).
    fn open(
        &mut self,
        w: &dyn Widget,
        slot: Option<(&dyn Widget, usize)>,
        parent: &mut Self::Scope,
    ) -> Visit<Self::Pending, Self::Scope>;

    /// Finish `w`'s node once its widget children are walked: its synthetic
    /// children ([`walk_synthetic`]), with the widgets `hosted` in them, go
    /// under it too. `represented` are the children it stands for itself.
    fn close<'w>(
        &mut self,
        w: &'w dyn Widget,
        pending: Self::Pending,
        scope: Self::Scope,
        hosted: HostedByIndex<'w>,
        represented: Vec<&'w dyn Widget>,
        parent: &mut Self::Scope,
    );

    /// `path` holds the ordinals from `owner`'s node down to `node`.
    fn open_synthetic(
        &mut self,
        owner: &dyn Widget,
        path: &[usize],
        node: &AccessNode,
        parent: &mut Self::Scope,
    ) -> (Self::SyntheticPending, Self::Scope);

    fn close_synthetic(
        &mut self,
        owner: &dyn Widget,
        path: &[usize],
        node: &AccessNode,
        pending: Self::SyntheticPending,
        scope: Self::Scope,
        parent: &mut Self::Scope,
    );
}

/// Walk `w` and everything under it into `parent`.
pub(crate) fn walk<const BOUND: bool, V: A11yVisitor>(
    v: &mut V,
    w: &dyn Widget,
    slot: Option<(&dyn Widget, usize)>,
    parent: &mut V::Scope,
) {
    let Ok(_entry) = crate::bound_dispatch::enter_traversal::<BOUND>(w) else {
        return;
    };
    match v.open(w, slot, parent) {
        Visit::Skip => {}
        Visit::Collapse => {
            for child in w.children() {
                walk::<BOUND, V>(v, child, None, parent);
            }
        }
        Visit::Node(pending, mut scope) => {
            let ExportedChildren {
                direct,
                hosted,
                represented,
            } = exported_children(w);
            for (index, child) in direct {
                walk::<BOUND, V>(v, child, Some((w, index)), &mut scope);
            }
            v.close(w, pending, scope, hosted, represented, parent);
        }
    }
}

/// Walk `owner`'s synthetic `node` at `path`, its synthetic children and the
/// widgets `hosted` in them, into `parent`.
pub(crate) fn walk_synthetic<const BOUND: bool, V: A11yVisitor>(
    v: &mut V,
    owner: &dyn Widget,
    path: &mut Vec<usize>,
    node: &AccessNode,
    hosted: Vec<HostedWidget<'_>>,
    parent: &mut V::Scope,
) {
    let (pending, mut scope) = v.open_synthetic(owner, path, node, parent);
    let HostedHere { direct, mut nested } = hosted_in(hosted);
    for (index, child) in node.children.iter().enumerate() {
        path.push(index);
        let hosted = nested.remove(&index).unwrap_or_default();
        walk_synthetic::<BOUND, V>(v, owner, path, child, hosted, &mut scope);
        path.pop();
    }
    // A widget whose own node is the synthetic one this walks
    // ([`HostedWidget::as_node`]) has its children attach here, after what
    // the node says of itself.
    for hosted in direct {
        if hosted.as_node {
            walk_children::<BOUND, V>(v, hosted.widget, &mut scope);
        } else {
            walk::<BOUND, V>(v, hosted.widget, None, &mut scope);
        }
    }
    v.close_synthetic(owner, path, node, pending, scope, parent);
}

/// Walk the children of `w` into `parent`, `w`'s own node left out: it is the synthetic node
/// they hang under ([`HostedWidget::as_node`]). The node it states is reached through whatever
/// boxes it wraps itself in — a slot, a box, a toolbar floating over it add no node of their own
/// — and stands in none of them twice.
fn walk_children<const BOUND: bool, V: A11yVisitor>(
    v: &mut V,
    w: &dyn Widget,
    parent: &mut V::Scope,
) {
    let Ok(_entry) = crate::bound_dispatch::enter_traversal::<BOUND>(w) else {
        return;
    };
    let mut stated = crate::a11y::stated_node(w);
    if let Some(node) = &mut stated {
        node.children.clear();
    }
    walk_below::<BOUND, V>(v, w, stated.as_ref(), parent);
}

/// Walk the children of `w` too, but a child stating `stated` folds into `w`: its own children
/// are walked where it stands.
fn walk_below<const BOUND: bool, V: A11yVisitor>(
    v: &mut V,
    w: &dyn Widget,
    stated: Option<&AccessNode>,
    parent: &mut V::Scope,
) {
    for (index, child) in w.children().into_iter().enumerate() {
        let folds = stated.is_some_and(|node| child.accessibility_root().as_ref() == Some(node));
        if folds {
            walk_below::<BOUND, V>(v, child, stated, parent);
        } else {
            walk::<BOUND, V>(v, child, Some((w, index)), parent);
        }
    }
}

/// Walk `owner`'s synthetic `children`, in order, with the widgets `hosted`
/// in them, into `scope`: what a visitor does with its node in
/// [`A11yVisitor::close`], the ordinal being the child's own.
pub(crate) fn walk_synthetic_children<const BOUND: bool, V: A11yVisitor>(
    v: &mut V,
    owner: &dyn Widget,
    children: &[AccessNode],
    mut hosted: HostedByIndex<'_>,
    scope: &mut V::Scope,
) {
    for (index, child) in children.iter().enumerate() {
        let hosted = hosted.remove(&index).unwrap_or_default();
        walk_synthetic::<BOUND, V>(v, owner, &mut vec![index], child, hosted, scope);
    }
}
