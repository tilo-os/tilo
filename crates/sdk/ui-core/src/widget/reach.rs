//! Reaching a widget in a retained tree, wherever a view or a page wrapped it.

use super::Widget;

/// The first widget of `root`'s subtree, `root` included, that is a `T`, to
/// change: a view or a page wraps the widget it places (a row's swipe around
/// its row, a docked page around its list), so a caller reaches it wherever
/// it sits.
pub fn find_mut<T: Widget + 'static>(root: &mut dyn Widget) -> Option<&mut T> {
    if root.as_any_mut().is_some_and(|any| any.is::<T>()) {
        return root.as_any_mut().and_then(|any| any.downcast_mut::<T>());
    }
    root.children_mut().into_iter().find_map(find_mut::<T>)
}

/// [`find_mut`], to read.
pub(crate) fn find<T: Widget + 'static>(root: &dyn Widget) -> Option<&T> {
    if let Some(any) = root.as_any() {
        if let Some(found) = any.downcast_ref::<T>() {
            return Some(found);
        }
    }
    root.children().into_iter().find_map(find::<T>)
}
