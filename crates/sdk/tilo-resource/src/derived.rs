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
use crate::resource::{ActionDecl, Resource};
use crate::vocabulary::Declaration;

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

impl<R: Resource> Bound<R> {
    /// This verb, its bound arguments filled, as a declaration of its own
    /// under `id`: what a surface offers bound to its view state ("split the
    /// clip at the playhead"), built and checked as the verb is.
    #[must_use]
    pub fn declaration(&self, id: &'static str, label: impl Into<String>) -> Option<ActionDecl<R>> {
        bound_declaration(
            crate::vocabulary::<R>(),
            self.action,
            &self.values,
            id,
            label.into(),
        )
        .map(ActionDecl::declared)
    }
}

/// [`Bound::declaration`] of the verb `action` of the resource `vocabulary`
/// with `values` bound.
fn bound_declaration(
    vocabulary: &crate::Vocabulary,
    action: &str,
    values: &[(String, serde_json::Value)],
    id: &'static str,
    label: String,
) -> Option<Declaration> {
    let declarations = vocabulary.declarations();
    let verb = declarations.iter().find(|decl| decl.id == action)?;
    let mut params = Vec::new();
    for param in &verb.params {
        if !values.iter().any(|(name, _)| param.name == name.as_str()) {
            params.push(param.clone());
        }
    }
    let all = verb.params.clone();
    let build = std::sync::Arc::clone(&verb.build);
    let values = values.to_vec();
    let build: crate::vocabulary::Builds = std::sync::Arc::new(move |args, now_ms| {
        let mut arguments = args.to_json();
        for (name, value) in &values {
            // An optional parameter bound to nothing is left out.
            if !value.is_null() {
                arguments.insert(name.clone(), value.clone());
            }
        }
        match tilo_actions::ActionArgs::bind_json(&all, &arguments) {
            Ok(args) => build(&args, now_ms),
            Err(error) => Err(error.to_string()),
        }
    });
    let mut decl = Declaration::new(vocabulary, id, label, params, build);
    decl.kind = verb.kind;
    decl.field = verb.field;
    decl.description.clone_from(&verb.description);
    decl.destructive = verb.destructive;
    // Bound or not, the verb is undone the same way, and housekeeping
    // stays housekeeping.
    decl.tags.extend(
        verb.tags
            .iter()
            .filter(|tag| {
                *tag == tilo_actions::COMPENSATED_UNDO_TAG || *tag == tilo_actions::HOUSEKEEPING_TAG
            })
            .cloned(),
    );
    Some(decl)
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
        crate::__private::json_via_bytes(value).unwrap_or(serde_json::Value::Null),
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

    /// This verb with nothing bound: every parameter still asked, offered on
    /// a place as it is on the record (the terminal's verbs on its place).
    #[must_use]
    pub fn unbound(&self) -> Bound<R> {
        Bound {
            action: self.id,
            values: Vec::new(),
            marker: PhantomData,
        }
    }
}

/// One derived resource. See the module docs.
pub struct Derived<R> {
    id: String,
    /// The field of the record it is an item of (`blocks`), published as
    /// `<record kind>/<field>/<id>`; `None` for `<record kind>/derived/<id>`.
    field: Option<String>,
    label: String,
    kind: Option<String>,
    actions: Vec<Bound<R>>,
    effects: Vec<tilo_actions::ActionSpec>,
    children: Vec<Derived<R>>,
}

impl<R> Derived<R> {
    /// A derived resource named `label`, identified by it among its
    /// siblings.
    pub fn new(label: impl Into<String>) -> Self {
        let label = label.into();
        Self {
            id: label.clone(),
            field: None,
            label,
            kind: None,
            actions: Vec::new(),
            effects: Vec::new(),
            children: Vec::new(),
        }
    }

    /// Nest `child` beneath it (a table's cells), identified within it.
    #[must_use]
    pub fn with_child(mut self, child: Self) -> Self {
        self.children.push(child);
        self
    }

    /// What kind of thing it is among the record's derived resources
    /// (`folder`, `tag`), published as `<record kind>.<kind>`.
    #[must_use]
    pub fn of_kind(mut self, kind: impl Into<String>) -> Self {
        self.kind = Some(kind.into());
        self
    }

    /// Offer the effect `effect` on it, its arguments bound (a view that
    /// reveals the folder).
    #[must_use]
    pub fn with_effect<E: crate::Effect>(mut self, effect: &E) -> Self {
        self.effects.push(effect.bound());
        self
    }

    /// Offer the effect `effect` on it with the arguments named in `asking`
    /// left to its caller, the rest bound (select a passage of *this* book,
    /// from a chapter the caller names).
    #[must_use]
    pub fn with_effect_asking<E: crate::Effect>(mut self, effect: &E, asking: &[&str]) -> Self {
        self.effects.push(effect.bound_asking(asking));
        self
    }

    /// Identify it by `id` rather than its label, when the label can change.
    #[must_use]
    pub fn identified(mut self, id: impl Into<String>) -> Self {
        self.id = id.into();
        self
    }

    /// Publish it as an item of the record's `field` (a document's
    /// paragraphs, `blocks`), as `<record kind>/<field>/<id>`: what a place
    /// parameter naming that field takes (`place = "blocks"`).
    #[must_use]
    pub fn in_field(mut self, field: impl Into<String>) -> Self {
        self.field = Some(field.into());
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
    /// Offer the field edit `edit` on it as the action `id` labelled
    /// `label`, its value bound ("Use Proton for Adobe Reader" on the app
    /// it routes): the same edit, checked and recorded as any other.
    #[must_use]
    pub fn with_edit(
        mut self,
        id: &'static str,
        label: impl Into<String>,
        edit: crate::Edit<R>,
    ) -> Self
    where
        crate::Edit<R>: Send + Sync,
    {
        let decl = ActionDecl::<R>::edit(id, label, edit);
        self.effects.push(crate::resource::declaration_spec(&decl));
        self
    }

    /// Offer a field edit whose one value its caller supplies
    /// ([`ActionDecl::edit_asking`]): "Rename playlist" on the playlist.
    #[must_use]
    pub fn with_edit_asking<V>(
        mut self,
        id: &'static str,
        label: impl Into<String>,
        param: &'static str,
        description: &'static str,
        make: impl Fn(V) -> crate::Edit<R> + Send + Sync + 'static,
    ) -> Self
    where
        V: serde::de::DeserializeOwned + crate::JsonSchema,
    {
        let decl = ActionDecl::<R>::edit_asking(id, label, param, description, make);
        self.effects.push(crate::resource::declaration_spec(&decl));
        self
    }

    /// The child resource this is published as.
    #[must_use]
    pub fn publish(&self) -> tilo_actions::Resource {
        self.publish_under(&format!(
            "{}/{}",
            R::KIND,
            self.field.as_deref().unwrap_or("derived")
        ))
    }

    /// The published resource `id` among `derived` and their children.
    #[must_use]
    pub fn find(derived: &[Self], id: &str) -> Option<tilo_actions::Resource> {
        fn walk(resource: tilo_actions::Resource, id: &str) -> Option<tilo_actions::Resource> {
            if resource.id.as_str() == id {
                return Some(resource);
            }
            resource
                .children
                .into_iter()
                .find_map(|child| walk(child, id))
        }
        derived.iter().find_map(|entry| walk(entry.publish(), id))
    }

    fn publish_under(&self, parent: &str) -> tilo_actions::Resource {
        let id = format!("{parent}/{}", self.id);
        // Built only when a verb is bound here: a long list of derived
        // places offering effects alone stays cheap to publish.
        let declarations = if self.actions.is_empty() {
            Vec::new().into()
        } else {
            crate::vocabulary::<R>().declarations()
        };
        let kind = self.kind.as_deref().unwrap_or("derived");
        let mut child =
            tilo_actions::Resource::new(&id, format!("{}.{kind}", R::KIND), &self.label);
        for bound in &self.actions {
            if let Some(decl) = declarations.iter().find(|decl| decl.id == bound.action) {
                child = child.with_action(crate::items::bound(decl, &bound.values));
            }
        }
        for effect in &self.effects {
            child = child.with_action(effect.clone());
        }
        child.with_children(self.children.iter().map(|nested| nested.publish_under(&id)))
    }
}
