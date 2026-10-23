//! The painted page's embedded parts, asking for Music's own actions: the
//! framework's [`page_part`](ui::framework::page_part).

pub(super) use ui::framework::page_part::Placed;

use super::Action;

/// Where a part's widgets leave the actions they ask for.
pub(super) type Asked = ui::framework::page_part::Asked<Action>;

pub(super) type PagePart = ui::framework::page_part::PagePart<Action>;

/// A widget callback that asks for `action`.
pub(super) fn asking(asked: &Asked, action: Action) -> impl FnMut() + 'static {
    ui::framework::page_part::asking(asked, action)
}
