//! A window's page as a viewer holds it, for a desk that shows another device's window: the API a
//! host asks through, and the code that answers it ([`PAGE`]).
//!
//! The tile itself is [`PageStream`](super::page_view::PageStream) and
//! [`PageView`](super::page_view::PageView), reached only through [`PageCode`]: the browser desktop
//! keeps them out of its start module (`lazy-page-view`) and fetches them the first time a page
//! travels to a desk, so a page that never shows a hosted window never loads them. A build without
//! that feature links them.

use std::rc::Rc;

use ui::widget::AppWidget;

use super::page_view::Viewer;

/// One window's page, as the desk holds it.
pub trait PageHandle {
    /// Take one object of the page: the payload as its publisher encoded it.
    /// Whether the frame the tile shows changed.
    fn apply(&self, payload: &[u8]) -> bool;
    /// The tile drawing this page.
    fn tile(&self, alt: String) -> Box<dyn AppWidget>;
}

/// What the page view's code hands over.
#[derive(Clone, Copy)]
pub struct PageCode {
    open: fn() -> Rc<dyn PageHandle>,
}

impl PageCode {
    /// A viewer for one window's page, holding nothing yet.
    #[must_use]
    pub fn open(self) -> Rc<dyn PageHandle> {
        (self.open)()
    }
}

/// The page view this build links.
#[must_use]
pub fn builtin_page() -> PageCode {
    PageCode {
        open: || Rc::new(Viewer::new()),
    }
}

ui::lazy_code! {
    /// A window's page as a viewer holds it: linked, or with `lazy-page-view`
    /// fetched the first time a page travels to a desk.
    pub static PAGE: PageCode = builtin_page, feature = "lazy-page-view";
}
