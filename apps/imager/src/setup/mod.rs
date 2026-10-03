pub mod actions;
pub mod controller;
pub mod desktop_catalog;
mod desktop_continuation;
pub mod desktop_download;
pub mod desktop_flow;
pub mod desktop_install;
pub mod desktop_probe;
pub mod desktop_worker;
mod destination_security;
pub mod folders;
pub mod import_actions;
pub mod import_view;
pub mod internal_bundle;
mod internal_cache;
pub mod internal_download;
#[cfg(any(target_os = "linux", windows, all(test, unix)))]
mod internal_download_flow;
#[cfg(any(target_os = "linux", windows, all(test, unix)))]
mod internal_install_flow;
pub mod known_folders;
pub mod migration;
mod migration_durability;
pub mod migration_journal;
mod migration_journal_log;
#[cfg(any(target_os = "linux", test))]
pub mod migration_native;
#[cfg(any(target_os = "linux", test))]
mod migration_native_journal;
mod migration_review;
mod migration_source;
mod migration_store;
mod migration_worker;
pub mod model;
mod nofollow;
pub mod staging;
mod strings;
#[cfg(all(test, target_os = "linux"))]
mod test_support;
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

/// A page's width: the wizard's readable column inside the page gutter.
pub(crate) fn readable_page_width() -> f32 {
    crate::app::READABLE_WIDTH as f32 + 2.0 * crate::rhythm::px::PAGE_GUTTER
}

/// `widget` in the column beside a page's scrolling body, ending where the
/// body's content ends: the scroll view keeps its scroll bar gutter on the
/// trailing edge, and the page's fixed rows keep the same edge.
pub(crate) fn beside_scroll(widget: Box<dyn ui::widget::Widget>) -> Box<dyn ui::widget::Widget> {
    Box::new(
        stack()
            .with_horizontal_padding(0.0, ui::tokens::Metrics::BASE.scrollbar_gutter)
            .push(widget),
    )
}
