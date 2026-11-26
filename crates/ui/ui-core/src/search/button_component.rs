//! The search launcher as a described component.

use crate::widget::Button;
use tilo_ui_base::widget::Decorator;
use tilo_ui_base::{component::Component, component::Described, component::ResourceId};

use crate::widget::search_request::{ScopeDecl, SearchRequest, search_button};

/// The one search affordance an app puts in its chrome: an icon button that opens the
/// platform search over the whole app, or — given the `item` it shows — over that one resource
/// and what it holds (a book's passages, a notebook's notes). It never draws a field, and a
/// window bar that searches for the window takes its place.
#[derive(Component, Default)]
#[component(name = "SearchButton", role = "Button")]
pub struct SearchButtonProps {
    /// The resource the search is scoped to, when it is one: the app's search narrowed to it,
    /// as the scope pill beside the query reads. Without one, the search reaches everything the
    /// app publishes.
    #[component(data)]
    pub item: Option<ResourceId>,
    /// What the scoped search calls the resource it is narrowed to, in the scope pill: the
    /// book's title. The item's own name is read when this is empty.
    #[component(data)]
    pub label: Option<String>,
}

/// What the button opens: the app's search, narrowed to the resource it shows when it shows
/// one.
impl SearchButtonProps {
    #[must_use]
    pub fn request(&self) -> SearchRequest {
        match &self.item {
            Some(item) if !item.0.is_empty() => SearchRequest::within(ScopeDecl::new(
                item.0.clone(),
                self.label.clone().unwrap_or_default(),
            )),
            _ => SearchRequest::this_app(),
        }
    }
}

/// [`search_button`] for the whole app, made from its description.
pub struct SearchButton(Button);

impl Decorator for SearchButton {
    type Inner = Button;

    fn inner(&self) -> &Button {
        &self.0
    }

    fn inner_mut(&mut self) -> &mut Button {
        &mut self.0
    }

    fn name(&self) -> &'static str {
        "SearchButton"
    }
}

impl Described for SearchButton {
    type Props = SearchButtonProps;

    fn from_props(_key: &str, props: SearchButtonProps) -> Self {
        Self(search_button(props.request()))
    }

    fn set_props(&mut self, _props: SearchButtonProps) -> bool {
        // The request travels with the button's own action: a scope that moved (a window now
        // showing another book) is another button, so it is built again.
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The resource a window shows is the scope its search opens on: a book's passages are
    /// found by a search opened on that book, and a button given none searches the whole app.
    #[test]
    fn the_scope_the_button_is_given_is_the_one_its_search_opens_on() {
        let scoped = SearchButtonProps {
            item: Some(ResourceId("tl:so.tilo.ebook:book1".into())),
            label: Some("A river".into()),
        };
        assert_eq!(
            scoped.request().part(),
            Some(&ScopeDecl::new("tl:so.tilo.ebook:book1", "A river"))
        );
        assert_eq!(
            SearchButtonProps::default().request(),
            SearchRequest::this_app()
        );
        assert!(
            SearchButtonProps {
                item: Some(ResourceId(String::new())),
                label: None,
            }
            .request()
            .part()
            .is_none(),
            "an empty id names no resource to narrow to"
        );
    }
}
