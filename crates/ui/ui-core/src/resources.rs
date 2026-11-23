//! Resources a widget publishes, reached from the app layer: the tree of open
//! [`tilo_actions::ResourceSpec`]s with the actions each exports, and the walks
//! that compose a host's catalogue from the widgets it lays out.

pub use crate::widget::resources::*;

/// Whether an operation may be published on a widget below its component's root: views and
/// effects may, a document mutation never does.
pub use tilo_resource::share::admits_outside_document;
/// `resource` without document actions, at any depth: what a widget below its component's root may
/// publish.
pub use tilo_resource::share::outside_document;
/// `resource`, resolved by id below its component's root, without document actions.
pub use tilo_resource::share::resolved_outside_document;
