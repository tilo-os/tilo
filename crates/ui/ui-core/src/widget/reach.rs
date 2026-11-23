//! Reaching a widget in a retained tree, wherever a view or a page wrapped it.

use super::Widget;
use super::capability::{capability, capability_mut};
use super::decorator::{Decorated, DecoratedMut};

/// The first widget of `root`'s subtree, `root` included, that is a `T`, to
/// change: a view or a page wraps the widget it places (a row's swipe around
/// its row, a docked page around its list), so a caller reaches it wherever
/// it sits.
pub fn find_mut<T: Widget + 'static>(root: &mut dyn Widget) -> Option<&mut T> {
    if root.as_any_mut().is_some_and(|any| any.is::<T>()) {
        return root.as_any_mut().and_then(|any| any.downcast_mut::<T>());
    }
    // A decorator's children are its inner widget's, so the inner widget
    // itself is reached through the decorator.
    if capability::<dyn Decorated>(root).is_some() {
        return capability_mut::<dyn DecoratedMut>(root)
            .and_then(|decorated| find_mut::<T>(decorated.decorated_mut()));
    }
    root.children_mut().into_iter().find_map(find_mut::<T>)
}

/// [`find_mut`], to read.
pub(crate) fn find<T: Widget + 'static>(root: &dyn Widget) -> Option<&T> {
    if let Some(found) = root.as_any().and_then(|any| any.downcast_ref::<T>()) {
        return Some(found);
    }
    if let Some(decorated) = capability::<dyn Decorated>(root) {
        return find::<T>(decorated.decorated());
    }
    root.children().into_iter().find_map(find::<T>)
}
