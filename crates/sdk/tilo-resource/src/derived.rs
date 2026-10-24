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

use std::any::TypeId;
use std::cell::RefCell;
use std::collections::HashMap;
use std::marker::PhantomData;
use std::rc::Rc;

use serde::Serialize;

use crate::action::ActionRef;
use crate::path::{Path, Segment};
use crate::resource::{ActionDecl, Resource};
use crate::vocabulary::Declaration;

/// A verb with some of its parameters bound: what a derived resource offers.
pub struct Bound<R> {
    verb: BoundVerb,
    marker: PhantomData<fn() -> R>,
}

/// A [`Bound`] without its resource type, so a [`Place`] holding it is
/// compared and published by code compiled once.
#[derive(Clone, Debug, PartialEq)]
struct BoundVerb {
    action: &'static str,
    values: Vec<(String, serde_json::Value)>,
}

impl<R> Clone for Bound<R> {
    fn clone(&self) -> Self {
        Self {
            verb: self.verb.clone(),
            marker: PhantomData,
        }
    }
}

impl<R> PartialEq for Bound<R> {
    fn eq(&self, other: &Self) -> bool {
        self.verb == other.verb
    }
}

impl<R> std::fmt::Debug for Bound<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Bound")
            .field("action", &self.verb.action)
            .field("values", &self.verb.values)
            .finish()
    }
}

impl<R> Bound<R> {
    fn new(action: &'static str, values: Vec<(String, serde_json::Value)>) -> Self {
        Self {
            verb: BoundVerb { action, values },
            marker: PhantomData,
        }
    }

    /// Bind one more parameter.
    ///
    #[must_use]
    pub fn and<P, V: Serialize>(mut self, param: Path<P, V>, value: V) -> Self {
        self.verb.values.push(bound_value(param, &value));
        self
    }

    pub fn action(&self) -> &'static str {
        self.verb.action
    }

    pub fn values(&self) -> &[(String, serde_json::Value)] {
        &self.verb.values
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
            self.verb.action,
            &self.verb.values,
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
    let invokes = (
        verb.id.to_owned(),
        values
            .iter()
            .filter(|(_, value)| !value.is_null())
            .cloned()
            .collect(),
    );
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
    decl.invokes = Some(invokes);
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
        Bound::new(self.id, vec![bound_value(param, &value)])
    }

    /// This verb with nothing bound: every parameter still asked, offered on
    /// a place as it is on the record (the terminal's verbs on its place).
    #[must_use]
    pub fn unbound(&self) -> Bound<R> {
        Bound::new(self.id, Vec::new())
    }
}

/// One derived resource. See the module docs.
pub struct Derived<R> {
    place: Place,
    marker: PhantomData<fn() -> R>,
}

/// A [`Derived`] without its resource type: what it publishes, compared and
/// published by code compiled once, in the runtime, rather than into every
/// app for each resource it declares.
#[derive(Clone)]
struct Place {
    id: String,
    /// The field of the record it is an item of (`blocks`), published as
    /// `<record kind>/<field>/<id>`; `None` for `<record kind>/derived/<id>`.
    field: Option<String>,
    label: String,
    kind: Option<String>,
    actions: Vec<BoundVerb>,
    effects: Vec<tilo_actions::ActionSpec>,
    /// What each of `effects` was made from, in order.
    offers: Vec<Offer>,
    /// Shared between copies, so a copy is as cheap as the resource alone
    /// and is told equal to its original without comparing its children.
    children: Rc<Vec<Place>>,
}

/// What an offered effect or edit of a [`Derived`] was made from: two equal
/// offers publish the same action.
#[derive(Clone, Debug, PartialEq)]
enum Offer {
    Effect {
        operation: &'static str,
        id: &'static str,
        arguments: serde_json::Map<String, serde_json::Value>,
        asking: Vec<String>,
    },
    Edit {
        id: &'static str,
        label: String,
        path: Vec<Segment>,
        op: crate::EditOp,
    },
    /// Made by a function of its caller's value, which cannot be compared:
    /// equal to no other.
    Asking,
}

impl PartialEq for Place {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
            && self.field == other.field
            && self.label == other.label
            && self.kind == other.kind
            && self.actions == other.actions
            && self.offers == other.offers
            && !self.offers.contains(&Offer::Asking)
            && (Rc::ptr_eq(&self.children, &other.children) || self.children == other.children)
    }
}

impl Place {
    /// The child resource this is published as, among the derived resources
    /// of a record of `vocabulary`.
    fn publish(&self, vocabulary: &'static crate::Vocabulary) -> tilo_actions::Resource {
        self.publish_under(
            &format!(
                "{}/{}",
                vocabulary.kind,
                self.field.as_deref().unwrap_or("derived")
            ),
            vocabulary,
        )
    }

    fn publish_under(
        &self,
        parent: &str,
        vocabulary: &'static crate::Vocabulary,
    ) -> tilo_actions::Resource {
        let id = format!("{parent}/{}", self.id);
        // Built only when a verb is bound here: a long list of derived
        // places offering effects alone stays cheap to publish.
        let declarations = if self.actions.is_empty() {
            Vec::new().into()
        } else {
            vocabulary.declarations()
        };
        let kind = self.kind.as_deref().unwrap_or("derived");
        let mut child =
            tilo_actions::Resource::new(&id, format!("{}.{kind}", vocabulary.kind), &self.label);
        for bound in &self.actions {
            if let Some(decl) = declarations.iter().find(|decl| decl.id == bound.action) {
                child = child.with_action(crate::items::bound(decl, &bound.values));
            }
        }
        for effect in &self.effects {
            child = child.with_action(effect.clone());
        }
        child.with_children(
            self.children
                .iter()
                .map(|nested| nested.publish_under(&id, vocabulary)),
        )
    }
}

impl<R> Clone for Derived<R> {
    fn clone(&self) -> Self {
        Self {
            place: self.place.clone(),
            marker: PhantomData,
        }
    }
}

impl<R> PartialEq for Derived<R> {
    fn eq(&self, other: &Self) -> bool {
        self.place == other.place
    }
}

impl<R> Derived<R> {
    /// A derived resource named `label`, identified by it among its
    /// siblings.
    pub fn new(label: impl Into<String>) -> Self {
        let label = label.into();
        Self {
            place: Place {
                id: label.clone(),
                field: None,
                label,
                kind: None,
                actions: Vec::new(),
                effects: Vec::new(),
                offers: Vec::new(),
                children: Rc::default(),
            },
            marker: PhantomData,
        }
    }

    /// Nest `child` beneath it (a table's cells), identified within it.
    #[must_use]
    pub fn with_child(mut self, child: Self) -> Self {
        Rc::make_mut(&mut self.place.children).push(child.place);
        self
    }

    /// What kind of thing it is among the record's derived resources
    /// (`folder`, `tag`), published as `<record kind>.<kind>`.
    #[must_use]
    pub fn of_kind(mut self, kind: impl Into<String>) -> Self {
        self.place.kind = Some(kind.into());
        self
    }

    /// Offer the effect `effect` on it, its arguments bound (a view that
    /// reveals the folder).
    #[must_use]
    pub fn with_effect<E: crate::Effect>(mut self, effect: &E) -> Self {
        self.place.effects.push(effect.bound());
        self.place.offers.push(Offer::Effect {
            operation: E::OPERATION,
            id: effect.id(),
            arguments: effect.arguments(),
            asking: Vec::new(),
        });
        self
    }

    /// Offer the effect `effect` on it with the arguments named in `asking`
    /// left to its caller, the rest bound (select a passage of *this* book,
    /// from a chapter the caller names).
    #[must_use]
    pub fn with_effect_asking<E: crate::Effect>(mut self, effect: &E, asking: &[&str]) -> Self {
        self.place.effects.push(effect.bound_asking(asking));
        self.place.offers.push(Offer::Effect {
            operation: E::OPERATION,
            id: effect.id(),
            arguments: effect.arguments(),
            asking: asking.iter().map(|name| (*name).to_owned()).collect(),
        });
        self
    }

    /// Identify it by `id` rather than its label, when the label can change.
    #[must_use]
    pub fn identified(mut self, id: impl Into<String>) -> Self {
        self.place.id = id.into();
        self
    }

    /// Publish it as an item of the record's `field` (a document's
    /// paragraphs, `blocks`), as `<record kind>/<field>/<id>`: what a place
    /// parameter naming that field takes (`place = "blocks"`).
    #[must_use]
    pub fn in_field(mut self, field: impl Into<String>) -> Self {
        self.place.field = Some(field.into());
        self
    }

    /// Offer `action` on it.
    #[must_use]
    pub fn with(mut self, action: Bound<R>) -> Self {
        self.place.actions.push(action.verb);
        self
    }

    pub fn id(&self) -> &str {
        &self.place.id
    }

    pub fn label(&self) -> &str {
        &self.place.label
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
        let label = label.into();
        self.place.offers.push(Offer::Edit {
            id,
            label: label.clone(),
            path: edit.path().to_vec(),
            op: edit.op().clone(),
        });
        let decl = ActionDecl::<R>::edit(id, label, edit);
        self.place
            .effects
            .push(crate::resource::declaration_spec(&decl));
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
        self.place
            .effects
            .push(crate::resource::declaration_spec(&decl));
        self.place.offers.push(Offer::Asking);
        self
    }

    /// The child resource this is published as.
    #[must_use]
    pub fn publish(&self) -> tilo_actions::Resource {
        self.place.publish(crate::vocabulary::<R>())
    }

    /// The published resource `id` among `derived` and their children.
    #[must_use]
    pub fn find(derived: &[Self], id: &str) -> Option<tilo_actions::Resource> {
        let vocabulary = crate::vocabulary::<R>();
        derived
            .iter()
            .find_map(|entry| find_published(entry.place.publish(vocabulary), id))
    }
}

/// The resource `id` among `resource` and its children.
fn find_published(resource: tilo_actions::Resource, id: &str) -> Option<tilo_actions::Resource> {
    if resource.id.as_str() == id {
        return Some(resource);
    }
    resource
        .children
        .into_iter()
        .find_map(|child| find_published(child, id))
}

/// A record's nested resources ([`crate::child::record_nested_children`])
/// and derived ones ([`Derived`]) as published, with the declarations they
/// offer with arguments bound.
#[derive(Default)]
pub struct PublishedDerived {
    resources: Rc<[Rc<tilo_actions::Resource>]>,
    /// Each action id they offer, with the fewest parameters it takes on
    /// one of them.
    offered: HashMap<String, usize>,
}

impl PublishedDerived {
    /// The published resources, in order: each the very one published last
    /// while the derived resource it publishes is unchanged.
    #[must_use]
    pub fn resources(&self) -> &Rc<[Rc<tilo_actions::Resource>]> {
        &self.resources
    }

    /// Whether some resource offers the declaration `id` with `params`
    /// parameters with some of its arguments bound.
    #[must_use]
    pub fn offers_bound(&self, id: &str, params: usize) -> bool {
        self.offered.get(id).is_some_and(|&taken| taken < params)
    }

    /// Copies of the published resources, in order.
    #[must_use]
    pub fn to_resources(&self) -> Vec<tilo_actions::Resource> {
        self.resources
            .iter()
            .map(|resource| (**resource).clone())
            .collect()
    }

    /// The published resource `id` among them and their children.
    #[must_use]
    pub fn find(&self, id: &str) -> Option<tilo_actions::Resource> {
        fn walk<'a>(
            resource: &'a tilo_actions::Resource,
            id: &str,
        ) -> Option<&'a tilo_actions::Resource> {
            if resource.id.as_str() == id {
                return Some(resource);
            }
            resource.children.iter().find_map(|child| walk(child, id))
        }
        self.resources
            .iter()
            .find_map(|resource| walk(resource, id))
            .cloned()
    }
}

/// The derived resources of one record type, seen without that type.
trait Places {
    fn len(&self) -> usize;
    fn place(&self, index: usize) -> Option<&Place>;
    /// Where they are held, which stays theirs while they are kept.
    fn address(&self) -> *const ();
}

impl<R: 'static> Places for Rc<[Derived<R>]> {
    fn len(&self) -> usize {
        <[Derived<R>]>::len(self)
    }

    fn place(&self, index: usize) -> Option<&Place> {
        self.get(index).map(|derived| &derived.place)
    }

    fn address(&self) -> *const () {
        Rc::as_ptr(self).cast()
    }
}

/// The derived resources a record type published last.
struct Kept {
    epoch: u64,
    derived: Rc<dyn Places>,
    published: Rc<PublishedDerived>,
}

thread_local! {
    static PUBLISHED: RefCell<HashMap<TypeId, Rc<Kept>>> = RefCell::new(HashMap::new());
}

/// The nested resources of the record `R` followed by `derived`, published:
/// the very publication of the last ones of the same resource type when
/// `derived` are the same ones or equal to them, so a change that leaves a
/// record's derived resources as they were publishes nothing again and hands
/// its publisher what it already holds. Otherwise each derived resource
/// equal to one published last is that very resource again, and only the
/// others are published. The nested resources depend on the type alone.
#[must_use]
pub fn published<R: Resource>(derived: impl Into<Rc<[Derived<R>]>>) -> Rc<PublishedDerived> {
    let derived: Rc<[Derived<R>]> = derived.into();
    published_places(
        TypeId::of::<R>(),
        crate::vocabulary::<R>(),
        Rc::new(derived),
    )
}

/// [`published`] of a record that derives no resources: its nested ones.
#[must_use]
pub fn published_nested<R: Resource>() -> Rc<PublishedDerived> {
    published_places(
        TypeId::of::<R>(),
        crate::vocabulary::<R>(),
        Rc::new(NoPlaces),
    )
}

/// The derived resources of a record that derives none.
struct NoPlaces;

impl Places for NoPlaces {
    fn len(&self) -> usize {
        0
    }

    fn place(&self, _index: usize) -> Option<&Place> {
        None
    }

    fn address(&self) -> *const () {
        std::ptr::null()
    }
}

/// [`published`] for the record type `of`, whose vocabulary is `vocabulary`.
fn published_places(
    of: TypeId,
    vocabulary: &'static crate::Vocabulary,
    derived: Rc<dyn Places>,
) -> Rc<PublishedDerived> {
    let epoch = crate::resource::type_epoch();
    let kept = PUBLISHED
        .with_borrow(|kept| kept.get(&of).cloned())
        .filter(|kept| kept.epoch == epoch);
    if let Some(kept) = kept.as_ref().filter(|kept| {
        std::ptr::eq(kept.derived.address(), derived.address()) || equal(&*kept.derived, &*derived)
    }) {
        return Rc::clone(&kept.published);
    }
    let resources: Rc<[Rc<tilo_actions::Resource>]> = match &kept {
        Some(kept) => {
            let last = &*kept.derived;
            let nested = kept.published.resources.len().saturating_sub(last.len());
            let at: HashMap<&str, usize> = (0..last.len())
                .filter_map(|index| Some((last.place(index)?.id.as_str(), index)))
                .collect();
            let published = |index: usize| {
                kept.published
                    .resources
                    .get(nested.saturating_add(index))
                    .cloned()
            };
            kept.published
                .resources
                .iter()
                .take(nested)
                .cloned()
                .chain((0..derived.len()).filter_map(|index| {
                    let next = derived.place(index)?;
                    let same = if last.place(index) == Some(next) {
                        Some(index)
                    } else {
                        at.get(next.id.as_str())
                            .copied()
                            .filter(|&at| last.place(at) == Some(next))
                    };
                    Some(
                        same.and_then(published)
                            .unwrap_or_else(|| Rc::new(next.publish(vocabulary))),
                    )
                }))
                .collect()
        }
        None => crate::child::nested_children_of(vocabulary)
            .into_iter()
            .map(Rc::new)
            .chain(
                (0..derived.len())
                    .filter_map(|index| derived.place(index))
                    .map(|next| Rc::new(next.publish(vocabulary))),
            )
            .collect(),
    };
    let mut offered = HashMap::new();
    for resource in resources.iter() {
        offered_on(resource, &mut offered);
    }
    let published = Rc::new(PublishedDerived { resources, offered });
    let kept = Rc::new(Kept {
        epoch,
        derived,
        published: Rc::clone(&published),
    });
    PUBLISHED.with_borrow_mut(|all| all.insert(of, kept));
    published
}

/// Whether `a` and `b` are equal, one by one.
fn equal(a: &dyn Places, b: &dyn Places) -> bool {
    a.len() == b.len() && (0..a.len()).all(|index| a.place(index) == b.place(index))
}

/// Note each action `resource` and its children offer in `offered`, with
/// the fewest parameters it takes on one of them.
fn offered_on(resource: &tilo_actions::Resource, offered: &mut HashMap<String, usize>) {
    for spec in &resource.actions {
        let taken = spec.params().len();
        offered
            .entry(spec.id.clone())
            .and_modify(|fewest| *fewest = (*fewest).min(taken))
            .or_insert(taken);
    }
    for child in &resource.children {
        offered_on(child, offered);
    }
}
