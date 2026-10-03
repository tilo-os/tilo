//! A framework app's resource type, by the kind its records name.

use tilo_resource::{Resource, ResourceKind};

use super::app::ResourceApp;

/// `A`'s resource type as a kind a host can look up: hosted by `A`, and an
/// open resource named as `A` names it ([`ResourceApp::label`]). Every
/// framework app gets its kind from here, so a host working with whatever a
/// room holds reads, publishes and replays it without any of the app's own
/// code.
#[must_use]
pub const fn resource_kind<A: ResourceApp>() -> ResourceKind {
    ResourceKind::of::<A::Resource>()
        .hosted_by(A::ID, A::TITLE)
        .named(name::<A>)
}

fn name<A: ResourceApp>(state: &dyn std::any::Any) -> Option<String> {
    state
        .downcast_ref::<<A::Resource as Resource>::State>()
        .map(A::label)
}
