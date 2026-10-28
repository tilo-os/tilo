//! The search launcher as a described component.

use crate::component::{Component, Described};
use crate::widget::{Button, Decorator};

use super::{SearchRequest, search_button};

/// The one search affordance an app puts in its chrome: an icon button that opens the
/// platform search over the whole app. It never draws a field, and a window bar that searches
/// for the window takes its place.
#[derive(Component, Default)]
#[component(name = "SearchButton", role = "Button")]
pub struct SearchButtonProps {}

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

    fn from_props(_key: &str, _props: SearchButtonProps) -> Self {
        Self(search_button(SearchRequest::this_app()))
    }

    /// It takes nothing: the one it is stays.
    fn set_props(&mut self, _props: SearchButtonProps) -> bool {
        true
    }
}
