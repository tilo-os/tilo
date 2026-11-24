//! [`Fullscreen`] — a view that fills the audience's screen while it is presented: a slide, a
//! page shown to a room, a video played large.
//!
//! A view says whether it is presented ([`FullscreenProps::showing`]); the window it stands in
//! answers the host's question by asking down the tree, and the host fills the screen the
//! audience looks at (the largest external display before the built-in panel). Nothing in the
//! layout changes: the same view draws, at the window's new size.

use crate::component::{Component, Described, PartViews};
use crate::fullscreen::{FullscreenRequest, FullscreenTarget};
use crate::widget::decorator::concrete::Decorator;
use crate::widget::{Element, FullscreenProvider, Widget};

/// Which screen the window fills while the view is presented.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Screen {
    /// Let the compositor choose.
    Auto,
    /// The audience's screen: the largest external display before the built-in panel.
    #[default]
    Largest,
    /// The primary screen.
    Primary,
    /// The primary screen someone plugged in, the built-in panel when there is none.
    PrimaryExternal,
}

impl Screen {
    /// The output the window is asked to fill for it.
    #[must_use]
    pub fn target(self) -> FullscreenTarget {
        match self {
            Self::Auto => FullscreenTarget::Auto,
            Self::Largest => FullscreenTarget::Largest,
            Self::Primary => FullscreenTarget::Primary,
            Self::PrimaryExternal => FullscreenTarget::PrimaryExternal,
        }
    }
}

crate::component::choice!(Screen, "Screen", {
    Auto => "auto", Largest => "largest", Primary => "primary",
    PrimaryExternal => "primary_external",
});

/// A view shown filling the audience's screen while it is presented.
#[derive(Component, Default)]
#[component(name = "Fullscreen", role = "Group", label = "name")]
pub struct FullscreenProps {
    /// Its stable key.
    #[component(key)]
    pub key: String,
    /// What it is, for assistive technology and agents.
    pub name: Option<String>,
    /// The view is presented: the window fills the screen until it is not.
    #[component(required)]
    pub showing: bool,
    /// Which screen it fills: the audience's, unless said.
    pub screen: Screen,
    /// The view it presents.
    #[component(part(default))]
    pub body: Option<Element>,
}

/// The view `Fullscreen` presents: the window fills the audience's screen while `showing`.
pub struct Fullscreen {
    body: Element,
    showing: bool,
    screen: Screen,
}

impl Fullscreen {
    fn no_body() -> Element {
        Box::new(crate::widget::Spacer::new())
    }

    fn put(&mut self, body: Element) {
        self.body = body;
    }
}

impl Decorator for Fullscreen {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.body.as_ref()
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.body.as_mut()
    }

    /// The fullscreen intent is this view's own; everything else it knows is the view's it
    /// presents.
    fn provide<'a>(&'a self, request: &mut crate::widget::capability::Request<'a, '_>) {
        if request.wants::<dyn FullscreenProvider>() {
            request.provide::<dyn FullscreenProvider>(self);
            return;
        }
        self.body.provide(request);
    }

    fn provide_mut<'a>(&'a mut self, request: &mut crate::widget::capability::RequestMut<'a, '_>) {
        if request.wants::<dyn FullscreenProvider>() {
            request.provide::<dyn FullscreenProvider>(self);
            return;
        }
        self.body.provide_mut(request);
    }
}

impl FullscreenProvider for Fullscreen {
    fn fullscreen_intent(&self) -> Option<bool> {
        Some(self.showing)
    }

    fn fullscreen_params(&self) -> FullscreenRequest {
        FullscreenRequest::mirror(self.screen.target())
    }
}

impl Described for Fullscreen {
    type Props = FullscreenProps;

    fn from_props(_key: &str, props: FullscreenProps) -> Self {
        Self {
            body: props.body.unwrap_or_else(Self::no_body),
            showing: props.showing,
            screen: props.screen,
        }
    }

    fn set_props(&mut self, props: FullscreenProps) -> bool {
        self.showing = props.showing;
        self.screen = props.screen;
        true
    }

    fn part_views(&mut self, part: &str) -> Option<&mut dyn PartViews> {
        (part == "body").then_some(self as &mut dyn PartViews)
    }
}

impl PartViews for Fullscreen {
    fn len(&self) -> usize {
        1
    }

    fn view_mut(&mut self, index: usize) -> Option<&mut Element> {
        (index == 0).then_some(&mut self.body)
    }

    fn insert(&mut self, _index: usize, view: Element) {
        self.put(view);
    }

    fn remove(&mut self, _index: usize) {
        self.put(Self::no_body());
    }

    fn move_view(&mut self, _from: usize, _to: usize) {}

    fn replace(&mut self, _index: usize, view: Element) {
        self.put(view);
    }
}

