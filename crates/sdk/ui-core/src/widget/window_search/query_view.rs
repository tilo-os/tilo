//! A view that answers the window bar's search: the query view of an app
//! whose view is a function of its resource.
//!
//! The window's search reaches an app through its widgets: it opens on what
//! they show ([`InPlace`](super::InPlace)) and, on a query view, hands the
//! query to the first [`Queryable`] in the tree. An app that is a widget
//! itself implements that on itself. An app whose view is a function — a
//! framework `ResourceApp`'s `view`, returning the view it draws — has no
//! widget of its own to implement it on, so it wraps the view it returns in
//! a [`QueryView`]: the bar's search then offers "Search <app>" in place,
//! hands over what is typed as it changes, and shows the count the view
//! kept, as it shows a [`Filterable`](crate::widget::Filterable) list's.
//!
//! The view keeps the query where its resource keeps it and answers with the
//! count its build left: the search holds no state of its own beyond the
//! query it drives with, on the same count a filtered list reports.

use crate::widget::capability::{Request, RequestMut};
use crate::widget::decorator::concrete;
use crate::widget::search_field::FilterCount;
use crate::widget::{Element, Widget};

use super::Queryable;

/// An app's view, and how it answers what is typed in the window bar's
/// search.
///
/// ```ignore
/// Box::new(
///     QueryView::new(page(rows), move |query| narrow(cx, query))
///         .kept(FilterCount::new(found, all)),
/// )
/// ```
///
/// Every change of what is typed reaches `answer`, and so does the close of
/// the search (`""`): the view shows what the resource says, and the closed
/// search leaves it as it was before it opened. `kept` is what the last
/// build left of what the view holds — the number the bar's field counts
/// ("3 of 12"), as a [`Filterable`](crate::widget::Filterable) list reports
/// its own; without it the bar shows no count.
///
/// Opening the search and Enter in its field ask nothing of the view: it
/// already is the query view, and the bar hands the keyboard on to it.
pub struct QueryView {
    view: Element,
    kept: Option<FilterCount>,
    answer: Box<dyn FnMut(&str)>,
}

impl QueryView {
    /// `view`, the app's view, answers what is typed in the window's bar
    /// through `answer`.
    pub fn new(view: Element, answer: impl FnMut(&str) + 'static) -> Self {
        Self {
            view,
            kept: None,
            answer: Box::new(answer),
        }
    }

    /// The view kept `kept` of what it holds.
    #[must_use]
    pub fn kept(mut self, kept: FilterCount) -> Self {
        self.kept = Some(kept);
        self
    }
}

impl Queryable for QueryView {
    fn set_query_text(&mut self, text: &str) {
        (self.answer)(text);
    }

    /// The view goes back to what it showed before the search opened, which
    /// is what it shows with nothing typed.
    fn query_closed(&mut self) {
        (self.answer)("");
    }

    fn query_count(&self) -> Option<FilterCount> {
        self.kept
    }
}

// Not the blanket `Decorator`: apps box a query view, so its `Widget` methods
// are compiled once, here (`concrete_decorator_widget!` in decorator.rs).
impl concrete::Decorator for QueryView {
    type Inner = dyn Widget;

    fn inner(&self) -> &Self::Inner {
        &*self.view
    }

    fn inner_mut(&mut self) -> &mut Self::Inner {
        &mut *self.view
    }

    fn provide<'a>(&'a self, request: &mut Request<'a, '_>) {
        if request.wants::<dyn Queryable>() {
            request.provide::<dyn Queryable>(self);
            return;
        }
        self.view.provide(request);
    }

    fn provide_mut<'a>(&'a mut self, request: &mut RequestMut<'a, '_>) {
        if request.wants::<dyn Queryable>() {
            request.provide::<dyn Queryable>(self);
            return;
        }
        self.view.provide_mut(request);
    }
}

#[cfg(test)]
#[path = "query_view_tests.rs"]
mod tests;
