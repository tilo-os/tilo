//! A function-based app's view as the window bar's search sees it: what it
//! offers, the query it is told, and the count it kept.

use std::cell::RefCell;
use std::rc::Rc;

use super::*;
use crate::tokens::MotionSource;
use crate::widget::find_bar::FindOptions;
use crate::widget::search_field::Filterable;
use crate::widget::{Column, InPlace, SearchDriver, Text, Widget, WindowSearch};

/// The rows both views narrow: what a [`Filterable`] list narrows and what a
/// query view answers with.
const ROWS: [&str; 4] = ["Ada's change", "Bo's change", "Ada again", "Cy's change"];

fn found(query: &str) -> usize {
    let query = query.to_lowercase();
    ROWS.iter()
        .filter(|row| row.to_lowercase().contains(&query))
        .count()
}

/// What the view built for `query` kept of the rows.
fn kept(query: &str) -> FilterCount {
    FilterCount::new(found(query), ROWS.len())
}

/// What the window's search told the view, in order.
type Told = Rc<RefCell<Vec<String>>>;

/// The app's view of the rows `query` leaves: a function of the query, as a
/// framework app's `view` is a function of its resource, wrapped in the query
/// view that answers the window's bar.
fn view(query: &str, told: &Told) -> QueryView {
    let mut rows = Column::new();
    for row in ROWS {
        if found(query) == ROWS.len() || row.to_lowercase().contains(&query.to_lowercase()) {
            rows = rows.push(Box::new(Text::new(row)));
        }
    }
    let heard = Rc::clone(told);
    QueryView::new(rows.into_widget(), move |typed| {
        heard.borrow_mut().push(typed.to_owned());
    })
    .kept(kept(query))
}

#[test]
fn a_functions_view_offers_the_bars_search_and_is_told_what_is_typed() {
    let told = Told::default();
    let mut root = view("", &told);
    assert_eq!(
        InPlace::of(&mut root),
        InPlace::Query,
        "the bar's search acts on the app's view"
    );

    let mut driver = SearchDriver::default();
    driver.open(InPlace::Query);
    driver.drive(&mut root, "", FindOptions::default(), None, false);
    assert_eq!(*told.borrow(), [""], "an open search with nothing typed");

    assert!(
        driver.drive(&mut root, "ada", FindOptions::default(), None, false),
        "what is typed shows"
    );
    assert_eq!(
        *told.borrow(),
        ["", "ada"],
        "the view is told what is typed"
    );

    // The app built the view its resource now asks for: the same query is
    // not carried again, and the bar counts what that view kept.
    let mut narrowed = view("ada", &told);
    driver.drive(&mut narrowed, "ada", FindOptions::default(), None, false);
    assert_eq!(*told.borrow(), ["", "ada"], "the same query runs once");
    assert_eq!(
        driver.count(),
        Some(kept("ada")),
        "the bar shows what the view kept"
    );
    assert_eq!(
        driver.count().map(FilterCount::label).as_deref(),
        Some("2 of 4")
    );

    // Closing returns the view to the one before the search opened.
    assert!(driver.drive(&mut narrowed, "ada", FindOptions::default(), None, true));
    assert_eq!(
        told.borrow().last().map(String::as_str),
        Some(""),
        "the close leaves nothing typed"
    );
    assert_eq!(driver.count(), None, "no count with the search closed");
    assert_eq!(driver.in_place(), InPlace::Nothing);
}

/// The same rows as a list that narrows itself, which the field counts.
struct Names {
    shown: usize,
}

impl Filterable for Names {
    fn set_filter(&mut self, query: &str) {
        self.shown = found(query);
    }

    fn filter_count(&self) -> Option<FilterCount> {
        Some(FilterCount::new(self.shown, ROWS.len()))
    }
}

impl Widget for Names {
    crate::provides!(dyn Filterable);

    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        _theme: &crate::theme::Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        use crate::widget::TaffyTreeExt;
        taffy.widget_leaf(taffy::Style::default(), node_map, self)
    }

    fn name(&self) -> &'static str {
        "Names"
    }
}

fn typed(search: &mut WindowSearch, query: &str) {
    search.field.set_query(query);
}

#[test]
fn the_bars_field_counts_a_query_view_as_it_counts_a_narrowed_list() {
    let mut names = Names { shown: ROWS.len() };
    let mut listed = WindowSearch::new();
    listed.open(InPlace::of(&mut names), MotionSource::Keyboard);
    assert_eq!(listed.in_place(), InPlace::List, "the list narrows itself");
    typed(&mut listed, "ada");
    assert!(listed.drive(&mut names));

    let told = Told::default();
    // As the app's view returns it: an element, boxed.
    let mut root: Box<dyn Widget> = Box::new(view("", &told));
    let mut asked = WindowSearch::new();
    asked.open(InPlace::of(&mut *root), MotionSource::Keyboard);
    assert_eq!(
        asked.in_place(),
        InPlace::Query,
        "the app's view answers the query"
    );
    typed(&mut asked, "ada");
    asked.drive(&mut *root);
    let mut narrowed = view("ada", &told);
    asked.drive(&mut narrowed);

    assert_eq!(told.borrow().last().map(String::as_str), Some("ada"));
    assert_eq!(
        asked.count(),
        Some(FilterCount::new(2, ROWS.len())),
        "the count is the rows the view kept of all of them"
    );
    assert_eq!(
        asked.count(),
        listed.count(),
        "the bar reads a view's count as it reads a list's"
    );
}
