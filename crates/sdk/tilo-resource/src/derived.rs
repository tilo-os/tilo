//! Derived resources: things computed from a record's state that people act
//! on (the folders of a snippet library, its tags, a smart list), published
//! as children whose actions are the record's own verbs with some arguments
//! bound.
//!
//! ```ignore
//! #[resource(kind = "snippets.library", children = Snippets::folders)]
//! impl Snippets { /* … */ }
//!
//! impl Snippets {
//!     fn folders(&self) -> Vec<Derived<Self>> {
//!         self.folder_names().map(|name| {
//!             Derived::new(&name).with(
//!                 Snippets::NEW_SNIPPET.bind(snippets::NewSnippet::paths().folder, name.clone()),
//!             )
//!         }).collect()
//!     }
//! }
//! ```

use std::marker::PhantomData;

use serde::Serialize;

use crate::action::ActionRef;
use crate::path::{Path, Segment};
use crate::resource::Resource;

/// A verb with some of its parameters bound: what a derived resource offers.
pub struct Bound<R> {
    action: &'static str,
    values: Vec<(String, serde_json::Value)>,
    marker: PhantomData<fn() -> R>,
}

impl<R> Clone for Bound<R> {
    fn clone(&self) -> Self {
        Self {
            action: self.action,
            values: self.values.clone(),
            marker: PhantomData,
        }
    }
}

impl<R> std::fmt::Debug for Bound<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Bound")
            .field("action", &self.action)
            .field("values", &self.values)
            .finish()
    }
}

impl<R> Bound<R> {
    /// Bind one more parameter.
    ///
    #[must_use]
    pub fn and<P, V: Serialize>(mut self, param: Path<P, V>, value: V) -> Self {
        self.values.push(bound_value(param, &value));
        self
    }

    pub fn action(&self) -> &'static str {
        self.action
    }

    pub fn values(&self) -> &[(String, serde_json::Value)] {
        &self.values
    }
}

fn bound_value<P, V: Serialize>(param: Path<P, V>, value: &V) -> (String, serde_json::Value) {
    let segments = param.into_segments();
    let name = match segments.as_slice() {
        [Segment::Field(name)] => name.clone(),
        other => other
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("."),
    };
    (
        name,
        serde_json::to_value(value).unwrap_or(serde_json::Value::Null),
    )
}

impl<R, P: Serialize> ActionRef<R, P> {
    /// This verb with the parameter at `param` (a path of its parameter
    /// struct, `NewSnippet::paths().folder`) bound to `value`.
    #[must_use]
    pub fn bind<V: Serialize>(&self, param: Path<P, V>, value: V) -> Bound<R> {
        Bound {
            action: self.id,
            values: vec![bound_value(param, &value)],
            marker: PhantomData,
        }
    }
}

/// One derived resource. See the module docs.
pub struct Derived<R> {
    id: String,
    label: String,
    actions: Vec<Bound<R>>,
}

impl<R> Derived<R> {
    /// A derived resource named `label`, identified by it among its
    /// siblings.
    pub fn new(label: impl Into<String>) -> Self {
        let label = label.into();
        Self {
            id: label.clone(),
            label,
            actions: Vec::new(),
        }
    }

    /// Identify it by `id` rather than its label, when the label can change.
    #[must_use]
    pub fn identified(mut self, id: impl Into<String>) -> Self {
        self.id = id.into();
        self
    }

    /// Offer `action` on it.
    #[must_use]
    pub fn with(mut self, action: Bound<R>) -> Self {
        self.actions.push(action);
        self
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn actions(&self) -> &[Bound<R>] {
        &self.actions
    }
}

impl<R: Resource> Derived<R> {
    /// The child resource this is published as.
    #[must_use]
    pub fn publish(&self) -> tilo_actions::Resource {
        let declarations = R::declarations();
        let mut child = tilo_actions::Resource::new(
            format!("{}/derived/{}", R::KIND, self.id),
            format!("{}.derived", R::KIND),
            &self.label,
        );
        for bound in &self.actions {
            if let Some(decl) = declarations.iter().find(|decl| decl.id == bound.action) {
                child = child.with_action(crate::items::bound(decl, &bound.values));
            }
        }
        child
    }
}
