pub mod actions;
pub mod controller;
pub mod desktop_catalog;
mod desktop_continuation;
pub mod desktop_download;
pub mod desktop_flow;
pub mod desktop_install;
pub mod desktop_probe;
pub mod desktop_worker;
pub mod folders;
pub mod import_view;
pub mod internal_bundle;
mod internal_download_flow;
pub mod known_folders;
pub mod model;
pub mod view;

pub use view::app_root;

use ui::widget::Column;

/// A padding-free, content-sized column: one section of a page.
pub(crate) fn stack() -> Column {
    Column::new().with_no_padding().with_intrinsic_height()
}

/// A page body: its header, then each section a `GROUP` apart.
pub(crate) fn page(header: Column) -> Column {
    stack()
        .with_gap(crate::rhythm::px::GROUP)
        .push(Box::new(header))
}
