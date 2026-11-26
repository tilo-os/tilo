//! The one depth-first walk every view of the accessibility tree takes over the widgets: the AccessKit
//! bridge, the retained [`SemanticTree`](crate::a11y_tree::SemanticTree) and the built-in reader's reading order.

use crate::a11y::AccessNode;
use crate::widget::Widget;
use std::collections::HashMap;

/// A widget hosted inside its parent's synthetic tree: the synthetic path below the child it hangs off
/// ([`Widget::accessibility_child_path`]), and whether that child *is* the widget node — the
/// parent publishes the widget's semantics itself ([`Widget::accessibility_represents_child`]), so the
/// walk attaches the widget's children to the synthetic node and exports no second node of the widget's
/// own.
pub(crate) struct HostedWidget<'a> {
    pub path: Vec<usize>,
    pub widget: &'a dyn Widget,
    pub as_node: bool,
}

/// Hosted widgets by the index of the synthetic child they hang off.
pub(crate) type HostedByIndex<'a> = HashMap<usize, Vec<HostedWidget<'a>>>;

/// How a widget's children enter the accessibility walk.
pub(crate) struct ExportedChildren<'a> {
    /// Walked as the widget children, in order, with their index.
    pub direct: Vec<(usize, &'a dyn Widget)>,
    /// Walked under the synthetic child they hang off.
    pub hosted: HostedByIndex<'a>,
    /// No node of their own: the widget represents them in its synthetic tree.
    pub represented: Vec<&'a dyn Widget>,
}

pub(crate) fn exported_children(w: &dyn Widget) -> ExportedChildren<'_> {
    let mut children = ExportedChildren {
        direct: Vec::new(),
        hosted: HostedByIndex::new(),
        represented: Vec::new(),
    };
    for (index, child) in w.children().into_iter().enumerate() {
        let represented = w.accessibility_represents_child(index);
        if let Some(mut path) = w.accessibility_child_path(index)
            && !path.is_empty()
        {
            let parent_index = path.remove(0);
            if represented {
                children.represented.push(child);
            }
            children
                .hosted
                .entry(parent_index)
                .or_default()
                .push(HostedWidget {
                    path,
                    widget: child,
                    as_node: represented,
                });
        } else if represented {
            children.represented.push(child);
        } else {
            children.direct.push((index, child));
        }
    }
    children
}

/// The widgets hosted in one synthetic child: those hanging off it directly, and those hanging off its
/// own synthetic children.
pub(crate) struct HostedHere<'a> {
    pub direct: Vec<HostedWidget<'a>>,
    pub nested: HostedByIndex<'a>,
}

pub(crate) fn hosted_in(hosted: Vec<HostedWidget<'_>>) -> HostedHere<'_> {
    let mut here = HostedHere {
        direct: Vec::new(),
        nested: HostedByIndex::new(),
    };
    for hosted in hosted {
        if hosted.path.is_empty() {
            here.direct.push(hosted);
        } else {
            let mut hosted = hosted;
            let child_index = hosted.path.remove(0);
            here.nested.entry(child_index).or_default().push(hosted);
        }
    }
    here
}

/// What a view makes of one widget.
pub(crate) enum Visit<P, S> {
    /// No node of it (`accessibility() == None`, ARIA's `role="presentation"`): its children attach
    /// where it would have.
    Collapse,
    /// Neither it nor anything under it is wanted.
    Skip,
    /// A node: `P` is what [`A11yVisitor::close`] finishes it from, `S` the scope its children attach
    /// in.
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

    /// `slot` is the widget's parent and its index there when the parent walks it as its child
    /// ([`Widget::accessibility_child_name`], [`Widget::accessibility_child_description`]), also
    /// through a wrapper that exports no node of it and has only this child.
    fn open(
        &mut self,
        w: &dyn Widget,
        slot: Option<(&dyn Widget, usize)>,
        parent: &mut Self::Scope,
    ) -> Visit<Self::Pending, Self::Scope>;

    /// Finish `w`'s node once its widget children are walked: its synthetic children
    /// ([`walk_synthetic`]), with the widgets `hosted` in them, go under it too.
    fn close<'w>(
        &mut self,
        w: &'w dyn Widget,
        pending: Self::Pending,
        scope: Self::Scope,
        hosted: HostedByIndex<'w>,
        represented: Vec<&'w dyn Widget>,
        parent: &mut Self::Scope,
    );

    /// `path` has the ordinals from `owner`'s node down to `node`.
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

/// The words a parent has for the child at `index` of its [`children`](Widget::children)
/// ([`Widget::accessibility_child_name`], [`Widget::accessibility_child_description`]), said of
/// `access`: its name where the child has none of it (or only found one, the value it shows), its
/// description where the child describes nothing.
pub(crate) fn with_slot_words(access: &mut AccessNode, slot: Option<&(&dyn Widget, usize)>) {
    let Some((parent, index)) = slot else {
        return;
    };
    if access.name.is_none() || access.name_found {
        if let Some(name) = parent.accessibility_child_name(*index) {
            access.name = Some(name);
            access.name_found = false;
        }
    }
    if access.description.is_none() {
        access.description = parent.accessibility_child_description(*index);
    }
}

/// Walk `w` and everything under it into `parent`.
pub(crate) fn walk<const BOUND: bool, V: A11yVisitor>(
    v: &mut V,
    w: &dyn Widget,
    slot: Option<(&dyn Widget, usize)>,
    parent: &mut V::Scope,
) {
    let Ok(_entry) = crate::widget::bound_nodes::enter_traversal::<BOUND>(w) else {
        return;
    };
    match v.open(w, slot, parent) {
        Visit::Skip => {}
        Visit::Collapse => {
            let children = w.children();
            let through = (children.len() == 1).then_some(slot).flatten();
            for (index, child) in children.into_iter().enumerate() {
                // A widget with no node of its own still names the child it holds the words
                // for (a field's body, named by its label); otherwise its one child takes the
                // words its own parent has for it.
                let slot = if w.accessibility_child_name(index).is_some() {
                    Some((w, index))
                } else {
                    through
                };
                walk::<BOUND, V>(v, child, slot, parent);
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

/// Walk `owner`'s synthetic `node` at `path`, its synthetic children and the widgets `hosted` in them,
/// into `parent`.
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
    for hosted in direct {
        if hosted.as_node {
            walk_children::<BOUND, V>(v, hosted.widget, &mut scope);
        } else {
            walk::<BOUND, V>(v, hosted.widget, None, &mut scope);
        }
    }
    v.close_synthetic(owner, path, node, pending, scope, parent);
}

fn walk_children<const BOUND: bool, V: A11yVisitor>(
    v: &mut V,
    w: &dyn Widget,
    parent: &mut V::Scope,
) {
    let Ok(_entry) = crate::widget::bound_nodes::enter_traversal::<BOUND>(w) else {
        return;
    };
    let mut stated = crate::a11y::stated_node(w);
    if let Some(node) = &mut stated {
        node.children.clear();
    }
    walk_below::<BOUND, V>(v, w, stated.as_ref(), parent);
}

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

/// Walk `owner`'s synthetic `children`, in order, with the widgets `hosted` in them, into `scope`: what
/// a visitor does with its node in [`A11yVisitor::close`], the ordinal being the child.
pub(crate) fn walk_synthetic_children<'a, const BOUND: bool, V: A11yVisitor, C>(
    v: &mut V,
    owner: &dyn Widget,
    children: C,
    mut hosted: HostedByIndex<'_>,
    scope: &mut V::Scope,
) where
    C: IntoIterator<Item = &'a AccessNode>,
{
    for (index, child) in children.into_iter().enumerate() {
        let hosted = hosted.remove(&index).unwrap_or_default();
        walk_synthetic::<BOUND, V>(v, owner, &mut vec![index], child, hosted, scope);
    }
}
