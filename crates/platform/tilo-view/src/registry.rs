//! The views an app declares, by the type of resource each takes and its name.
//!
//! A `#[view]` function registers itself when the app's code is linked, so nothing names it in
//! a list that could drift (as the toolkit's `#[derive(Component)]` registers a widget). The
//! shell finds a view the way it finds a tilolang one: by the type of the resource a place names
//! and the view the place names, or the type's default view ([`ViewRegistry::default_for`]).
//!
//! The registry is filled once, when the app's code is loaded: every view of the crate that
//! declares it is here, whatever module it lives in. A web page's registry is empty: nothing on
//! it draws a Rust app's view, and each app's code arrives in a module of its own.

use crate::cx::ViewCx;
use crate::node::{Node, ResourceRef};

/// A view function: what a `#[view]` declares, taking a resource's reading and the context.
///
/// It is handed the resource as its reading (the fields a place's resource has, typed by the
/// app's own `#[resource]` type), and answers the tree the place draws.
pub type ViewFn = fn(&ViewInput<'_>, &ViewCx) -> Node;

/// What a view is handed of the resource it shows: its identity, and its reading as JSON.
///
/// The view reads its fields through the app's own model types, from the reading the session
/// publishes; a resource whose reading has not arrived yet is handed none, and the view shows
/// what it can without it.
#[derive(Clone, Debug, PartialEq)]
pub struct ViewInput<'a> {
    /// The resource the view shows: its id and type.
    pub target: ResourceRef,
    /// Its state, as the session publishes its reading; `None` before one arrives.
    pub reading: Option<&'a serde_json::Value>,
}

/// One view a `#[view]` declares.
#[derive(Clone)]
pub struct View {
    /// The view's name (`page`), which a place names as `shown_as`, and `default` for the one a
    /// place shows unasked is [`View::default`].
    pub name: &'static str,
    /// The resource type it takes, by the app's kind (`tube.Channel`).
    pub kind: &'static str,
    /// Whether it is the type's default view: what `open(r)` places it with.
    pub is_default: bool,
    /// The function itself.
    pub render: ViewFn,
    /// The app that declares it.
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

// A view is registered by its declaration, at link time.
inventory::collect!(View);

/// A resource type a view shows: the app's own model type, which says what kind it is and which
/// app it belongs to, so its views are found by a place's `shown_as` and by `open(r)`.
///
/// `#[resource(kind = "tube.Channel")]` derives it with the model, so a type that is a resource
/// is a view's type already; a type with only views says it by hand.
pub trait ViewResource: Default + serde::de::DeserializeOwned {
    /// The type's kind (`tube.channel`, or the tilolang package's `tl.<app>.<Type>` reconciled by
    /// the host): what a place's resource names as its type, and what a view is found by.
    const KIND: &'static str;
    /// The app the kind belongs to (`tube`), as the shell names it.
    const APP: &'static str;
}

/// Every view registered so far, by resource kind and name: the lookup a shell does when a place
/// names a view of a resource of an app written in Rust.
#[derive(Clone, Copy, Debug)]
pub struct ViewRegistry;

impl ViewRegistry {
    /// The view `name` of a resource of `kind`, if the app declares one.
    #[must_use]
    pub fn get(kind: &str, name: &str) -> Option<&'static View> {
        Self::views().find(|view| view.kind == kind && view.name == name)
    }

    /// The default view of a resource of `kind`: what a place shows it with when it names none.
    #[must_use]
    pub fn default_for(kind: &str) -> Option<&'static View> {
        Self::views().find(|view| view.kind == kind && view.is_default)
    }

    /// The view `name` of a resource of `kind`, or the type's default view when `name` is empty
    /// or names none: what a place draws.
    #[must_use]
    pub fn resolve(kind: &str, name: &str) -> Option<&'static View> {
        if name.is_empty() {
            Self::default_for(kind)
        } else {
            Self::get(kind, name).or_else(|| Self::default_for(kind))
        }
    }

    /// Every view any app declares, the default of a type first among its type's.
    pub fn views() -> impl Iterator<Item = &'static View> {
        let mut all: Vec<&'static View> = inventory::iter::<View>.into_iter().collect();
        all.sort_by(|a, b| (a.kind, !a.is_default, a.name).cmp(&(b.kind, !b.is_default, b.name)));
        all.into_iter()
    }

    /// Whether any app declares views at all: a Rust app's views are linked in with its code.
    #[must_use]
    pub fn is_empty() -> bool {
        Self::views().next().is_none()
    }

    /// The apps that declare at least one view.
    #[must_use]
    pub fn apps() -> Vec<&'static str> {
        let mut apps: Vec<&'static str> = Self::views().map(|view| view.app).collect();
        apps.sort_unstable();
        apps.dedup();
        apps
    }
}

/// The views a kind's type may be shown through, by name: a place's `shown_as` finds one here, or
/// the type's default view when it names none.
#[must_use]
pub fn views_of(kind: &str) -> Vec<&'static View> {
    ViewRegistry::views()
        .filter(|view| view.kind == kind)
        .collect()
}
