//! A view that answers the window bar's search: the query view of an app
//! whose view is a function of its resource.
//!
//! The window's search reaches an app through its widgets: it opens on what
//! they show ([`InPlace`](super::InPlace)) and, on a query view, hands the
//! query to the first [`Queryable`] in the tree. An app that is a widget
//! itself implements that on itself. An app whose view is a function — a
//! framework `ResourceApp`'s `view`, returning the view it draws — has no
//! widget of its own to implement it on, so it wraps the view it returns in
//! a [`QueryView`]: the bar's search then offers "Search `<app>`" in place,
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
#[crate::component::not_a_component(system, "system search")]
pub struct QueryView {
    view: Element,
    kept: Option<FilterCount>,
    answer: Box<dyn FnMut(&str)>,
    /// Whether the view answers the search now ([`Self::offered_while`]).
    offered: Option<Box<dyn Fn() -> bool>>,
    /// What the view does as the search opens ([`Self::on_open`]).
    opened: Option<Box<dyn FnMut()>>,
    /// What the view does as the search closes ([`Self::on_close`]).
    closed: Option<Box<dyn FnMut()>>,
}

impl QueryView {
    /// `view`, the app's view, answers what is typed in the window's bar
    /// through `answer`.
    pub fn new(view: Element, answer: impl FnMut(&str) + 'static) -> Self {
        Self {
            view,
            kept: None,
            answer: Box::new(answer),
            offered: None,
            opened: None,
            closed: None,
        }
    }

    /// The view answers the search only while `offered` says so (a page
    /// that lists the answer to a query, among pages a list narrows to):
    /// otherwise the search acts on what else the window offers.
    #[must_use]
    pub fn offered_while(mut self, offered: impl Fn() -> bool + 'static) -> Self {
        self.offered = Some(Box::new(offered));
        self
    }

    /// Run `opened` as the search opens on the view, before anything is
    /// typed (the view notes what it answers for, or warms its sources).
    #[must_use]
    pub fn opening(mut self, opened: impl FnMut() + 'static) -> Self {
        self.opened = Some(Box::new(opened));
        self
    }

    /// Run `closed` as the search closes, in place of telling the view the
    /// query is empty: the view goes back to what it showed before it
    /// opened.
    #[must_use]
    pub fn closing(mut self, closed: impl FnMut() + 'static) -> Self {
        self.closed = Some(Box::new(closed));
        self
    }

    /// Whether the view answers the search now.
    fn offers(&self) -> bool {
        self.offered.as_ref().is_none_or(|offered| offered())
    }

    /// The view kept `kept` of what it holds.
    #[must_use]
    pub fn kept(mut self, kept: FilterCount) -> Self {
        self.kept = Some(kept);
        self
    }
}

impl Queryable for QueryView {
    fn query_opened(&mut self) {
        if let Some(opened) = self.opened.as_mut() {
            opened();
        }
    }

    fn set_query_text(&mut self, text: &str) {
        (self.answer)(text);
    }

    /// The view goes back to what it showed before the search opened, which
    /// is what it shows with nothing typed unless it says otherwise.
    fn query_closed(&mut self) {
        match self.closed.as_mut() {
            Some(closed) => closed(),
            None => (self.answer)(""),
        }
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
        if request.wants::<dyn Queryable>() && self.offers() {
            request.provide::<dyn Queryable>(self);
            return;
        }
        self.view.provide(request);
    }

    fn provide_mut<'a>(&'a mut self, request: &mut RequestMut<'a, '_>) {
        if request.wants::<dyn Queryable>() && self.offers() {
            request.provide::<dyn Queryable>(self);
            return;
        }
        self.view.provide_mut(request);
    }
}

#[cfg(test)]
#[path = "query_view_tests.rs"]
mod tests;
