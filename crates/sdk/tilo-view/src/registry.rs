//! The views an app declares, by the type of resource each takes and its name.
//!
//! A `#[view]` function registers itself when the app's code is linked, so no list
//! can drift, and the shell finds a view by resource kind and view name.

use crate::cx::ViewCx;
use crate::node::{Node, ResourceRef};

pub type ViewFn = fn(&ViewInput<'_>, &ViewCx) -> Node;

#[derive(Clone, Debug, PartialEq)]
pub struct ViewInput<'a> {
    pub target: ResourceRef,
    pub reading: Option<&'a serde_json::Value>,
}

#[derive(Clone)]
pub struct View {
    pub name: &'static str,
    pub kind: &'static str,
    pub is_default: bool,
    pub render: ViewFn,
    pub app: &'static str,
}

impl std::fmt::Debug for View {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("View")
            .field("name", &self.name)
            .field("kind", &self.kind)
            .field("is_default", &self.is_default)
            .field("app", &self.app)
            .finish_non_exhaustive()
    }
}

inventory::collect!(View);

/// A resource type a view shows: the app's own model type, which says what kind it
/// is and which app it belongs to, so its views are found by a place's `shown_as`
/// and by `open(r)`.
///
/// `#[resource(kind = "tube.Channel")]` derives it with the model; a type with
/// only views says it by hand.
pub trait ViewResource: Default + serde::de::DeserializeOwned {
    const KIND: &'static str;
    const APP: &'static str;
}

/// Every view registered so far, by resource kind and name.
#[derive(Clone, Copy, Debug)]
pub struct ViewRegistry;

impl ViewRegistry {
    #[must_use]
    pub fn get(kind: &str, name: &str) -> Option<&'static View> {
        Self::views().find(|view| view.kind == kind && view.name == name)
    }

    #[must_use]
    pub fn default_for(kind: &str) -> Option<&'static View> {
        Self::views().find(|view| view.kind == kind && view.is_default)
    }

    /// The view `name` of a resource of `kind`, or the type's default view when `name`
    /// is empty or names none.
    #[must_use]
    pub fn resolve(kind: &str, name: &str) -> Option<&'static View> {
        if name.is_empty() {
            Self::default_for(kind)
        } else {
            Self::get(kind, name).or_else(|| Self::default_for(kind))
        }
    }

    pub fn views() -> impl Iterator<Item = &'static View> {
        let mut all: Vec<&'static View> = inventory::iter::<View>.into_iter().collect();
        all.sort_by(|a, b| (a.kind, !a.is_default, a.name).cmp(&(b.kind, !b.is_default, b.name)));
        all.into_iter()
    }

    #[must_use]
    pub fn is_empty() -> bool {
        Self::views().next().is_none()
    }

    #[must_use]
    pub fn apps() -> Vec<&'static str> {
        let mut apps: Vec<&'static str> = Self::views().map(|view| view.app).collect();
        apps.sort_unstable();
        apps.dedup();
        apps
    }
}

#[must_use]
pub fn views_of(kind: &str) -> Vec<&'static View> {
    ViewRegistry::views()
        .filter(|view| view.kind == kind)
        .collect()
}
