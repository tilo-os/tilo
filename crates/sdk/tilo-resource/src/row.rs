//! A live row: one entity a surface publishes beneath its resource that lives
//! outside the resource's document — a process, a device, a container, a
//! journal entry, a fetched catalogue row, a running job — for the palette,
//! agents and scripts.
//!
//! A row is not document state: it has no history, no undo and nothing to
//! sync, so it can never carry a document mutation. A change to the document
//! is the resource's `Action`, bound to a row through the framework's
//! `Publishes::row_edits` or the resource's `children`.

use std::collections::BTreeMap;

use tilo_actions::{
    ActionSpec, Exports, ResourcePresentation, ResourceSemanticIdentity, ResourceSpec,
};

use crate::Effect;

/// The arguments a row sets for the effects it offers, by effect id.
///
/// A row of a declared type offers the effects its type declares on rows
/// (`#[effect(on = Row)]`), and knows from its own state what those effects
/// act on: a saved network knows the profile it keeps and whether forging it
/// once more means going through its proxy. Each effect the row names is
/// offered on it with exactly the arguments it gives bound; an effect it
/// does not name is not offered (`RowBindings::new()` offers none, and
/// [`LiveRow::typed`] offers every declared one with just the id bound).
#[derive(Clone, Default)]
pub struct RowBindings {
    arguments: BTreeMap<&'static str, Vec<serde_json::Map<String, serde_json::Value>>>,
    choices: BTreeMap<(&'static str, &'static str), Vec<String>>,
    specs: Vec<ActionSpec>,
}

impl std::fmt::Debug for RowBindings {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RowBindings")
            .field("arguments", &self.arguments)
            .field("choices", &self.choices)
            .field(
                "specs",
                &self
                    .specs
                    .iter()
                    .map(|spec| spec.id.as_str())
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl RowBindings {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Offer the effect `effect` on the row, with `arguments` bound: the
    /// effect's declaration's parameters, each by the name it declares.
    /// Names the effect does not take are dropped.
    #[must_use]
    pub fn bind<R, A, I, K>(self, effect: crate::ActionRef<R, A>, arguments: I) -> Self
    where
        I: IntoIterator<Item = (K, serde_json::Value)>,
        K: Into<String>,
    {
        self.bind_id(effect.id, arguments)
    }

    /// [`Self::bind`] of an effect known only when it invokes, by the id it
    /// answers to: the arguments its declaration takes, named as it names
    /// them. Names the effect does not take are dropped.
    #[must_use]
    pub fn bind_id<I, K>(mut self, effect: &'static str, arguments: I) -> Self
    where
        I: IntoIterator<Item = (K, serde_json::Value)>,
        K: Into<String>,
    {
        self.arguments.entry(effect).or_default().push(
            arguments
                .into_iter()
                .map(|(name, value)| (name.into(), value))
                .collect(),
        );
        self
    }

    /// Offer `effect` on the row with nothing bound: every argument of it is
    /// asked from whoever invokes it.
    #[must_use]
    pub fn offer<R, A>(self, effect: crate::ActionRef<R, A>) -> Self {
        self.offer_id(effect.id)
    }

    /// [`Self::offer`] of an effect known only when it invokes, by its id.
    #[must_use]
    pub fn offer_id(mut self, effect: &'static str) -> Self {
        self.arguments
            .entry(effect)
            .or_default()
            .push(serde_json::Map::default());
        self
    }

    /// Offer beside what the row already offers, `other`'s: two effects a
    /// row's own state knows separately are one row's bindings.
    #[must_use]
    pub fn extended(mut self, other: &Self) -> Self {
        for (effect, arguments) in &other.arguments {
            self.arguments
                .entry(effect)
                .or_default()
                .extend(arguments.iter().cloned());
        }
        self.choices.extend(other.choices.clone());
        self.specs.extend(other.specs.iter().cloned());
        self
    }

    /// Offer the parameter `param` of `effect`, which the row offers, with
    /// the values the row's state knows it takes (a display's modes, the
    /// scales it supports): what a control or the palette lists.
    #[must_use]
    pub fn choices<R, A>(
        mut self,
        effect: crate::ActionRef<R, A>,
        param: &'static str,
        choices: impl IntoIterator<Item = String>,
    ) -> Self {
        self.choices
            .insert((effect.id, param), choices.into_iter().collect());
        self
    }

    /// Offer `spec` on the row: an action another resource declares, which
    /// the row was given rather than declaring it itself (a settings
    /// section's edit, the open of an installed app), published on the row
    /// as that resource published it.
    #[must_use]
    pub fn spec(mut self, spec: ActionSpec) -> Self {
        self.specs.push(spec);
        self
    }

    /// The arguments the row gives the effect `effect`, once for each time it
    /// offers it: an effect offered twice with different arguments (each
    /// reviewed sign-in method) is two actions on the row.
    #[must_use]
    pub fn arguments(&self, effect: &str) -> Option<&[serde_json::Map<String, serde_json::Value>]> {
        self.arguments.get(effect).map(Vec::as_slice)
    }

    /// The values the row says `param` of `effect` takes, when it says.
    #[must_use]
    pub(crate) fn chosen(&self, effect: &str, param: &str) -> Option<&[String]> {
        self.choices
            .iter()
            .find(|((id, name), _)| *id == effect && *name == param)
            .map(|(_, values)| values.as_slice())
    }

    /// The actions of another resource the row offers
    /// ([`Self::spec`]), as published.
    #[must_use]
    pub(crate) fn declared(&self) -> &[ActionSpec] {
        &self.specs
    }

    /// Whether the row offers no effect of its type's.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.arguments.is_empty() && self.specs.is_empty()
    }
}

/// The resource `id` of the declared type `R`, published where no document
/// holds it: a session service's reading (Wi-Fi's networks, the power
/// policy), a question a service asks, a report a crash left. Its words are
/// the type's, so search and the palette name it by what it is; the service
/// publishes the type's vocabulary on it itself
/// ([`offer`](crate::effect::offer)).
#[must_use]
pub fn typed_resource<R: crate::words::RowType>(
    id: impl Into<String>,
    label: impl Into<String>,
) -> ResourceSpec {
    ResourceSpec::new(id, R::KIND, label).with_type(R::words())
}

/// One live external entity a surface publishes beneath its resource, with
/// the view and effect operations it offers (inspect a process, stop a
/// container).
///
/// Rows are merged by id into the resource's published children: a row with
/// the id of a document child adds its operations to that child instead of
/// publishing a second resource under the same id.
///
/// A row is shared, not copied: a surface that keeps the rows it made while
/// what they show holds hands the same rows to every publication, and the
/// publication reuses what it built from each one instead of building it
/// again.
///
/// [`LiveRow::typed`] is the one way to publish a row: it is an item of a
/// declared [`RowType`](crate::words::RowType), saying what it is to search
/// and offering the type's effects, so a press on it is one of the type's
/// actions or `open`.
#[derive(Clone)]
pub struct LiveRow {
    resource: std::rc::Rc<ResourceSpec>,
}

impl LiveRow {
    /// A row with a stable `id` (unique within the resource, and naming the
    /// same entity for as long as it lives: a process id with its start
    /// time, a container id), a `kind` tag and a human `label`.
    ///
    /// The untyped builder is framework-internal: it names no declared type,
    /// so search cannot say what the row is and a press on it offers only the
    /// operations the caller adds. Production code publishes
    /// [`Self::typed`]; the repository's live-row check refuses this one in
    /// an app or a session surface.
    #[doc(hidden)]
    pub fn new(id: impl Into<String>, kind: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            resource: std::rc::Rc::new(ResourceSpec::new(id, kind, label)),
        }
    }

    /// A row of the declared type `R` (a resource type of `R`'s kind), whose
    /// noun its type publishes, of the app the type is declared in: its
    /// `id`, its `label`, the [`ResourceType`](tilo_actions::ResourceType)
    /// search matches "videos of …" against, and the effects the type offers
    /// on it ([`RowType::effects`](crate::words::RowType::effects), each
    /// bound to `id`). A row an app declares as an item of a `#[resource]`
    /// type publishes (`Video`, `Channel`) is one of these, so what it is is
    /// the type's and the search's, nothing the app writes.
    pub fn typed<R: crate::words::RowType>(
        id: impl Into<String>,
        label: impl Into<String>,
    ) -> Self {
        let id = id.into();
        let mut row = Self {
            resource: std::rc::Rc::new(
                ResourceSpec::new(id.clone(), R::KIND, label).with_type(R::words()),
            ),
        };
        let row_id = crate::Id::from_raw(id);
        for operation in R::effects(&row_id) {
            row = row.operation(operation);
        }
        row
    }

    /// A row of the declared type `R`, offering the effects `bindings`
    /// names among the type's ([`RowType::effects_with`](crate::words::RowType::effects_with)),
    /// each bound to what the row's own state knows: unlike [`Self::typed`],
    /// which offers every effect the type declares on its rows with only the
    /// id bound, this one leaves out the effects that do not apply (a network
    /// with no proxy) and sets the arguments the row holds (the profile, the
    /// direction of a toggle).
    pub fn typed_with<R: crate::words::RowType>(
        id: impl Into<String>,
        label: impl Into<String>,
        bindings: &RowBindings,
    ) -> Self {
        let id = id.into();
        let mut row = Self {
            resource: std::rc::Rc::new(
                ResourceSpec::new(id.clone(), R::KIND, label).with_type(R::words()),
            ),
        };
        let row_id = crate::Id::from_raw(id);
        for operation in R::effects_with(&row_id, bindings) {
            row = row.operation(operation);
        }
        for operation in bindings.declared() {
            row = row.operation(operation.clone());
        }
        row
    }

    /// Attach the stable source-domain identity used to find this row across
    /// live dispatch aliases. It is descriptive metadata only and grants no
    /// permission or authority. Invalid identities are omitted at this
    /// publication boundary.
    #[must_use]
    pub fn semantic_identity(mut self, identity: ResourceSemanticIdentity) -> Self {
        std::rc::Rc::make_mut(&mut self.resource).semantic_identity =
            identity.is_valid().then_some(identity);
        self
    }

    /// `resource`, an entity the surface already describes as a resource
    /// (a search hit, a catalogue entry), as a row: its presentation,
    /// views, operations and children. Its document actions are dropped at
    /// any depth: the resource's own publication carries them.
    ///
    /// Framework-internal, beside [`Self::new`]: the row takes whatever the
    /// resource carries rather than a declared type's words and effects, so
    /// nothing here says what it is to search.
    #[doc(hidden)]
    pub fn from_resource(resource: ResourceSpec) -> Self {
        Self {
            resource: std::rc::Rc::new(crate::share::resolved_outside_document(resource)),
        }
    }

    /// Offer a view or effect operation on the row.
    ///
    /// Offer every effect of the vocabulary `E` the resource itself offers
    /// ([`Effect::specs`](crate::Effect::specs)), as it publishes them: what a
    /// window's own row offers (navigate, show settings), beside the effects
    /// its row types declare.
    #[must_use]
    pub fn vocabulary<E: crate::Effect>(mut self) -> Self {
        for spec in E::specs() {
            self = self.operation(spec);
        }
        self
    }

    /// Offer `operations` the row's type declares but only the row can shape,
    /// each still the type's effect: a page's forms, one `answer-form` per
    /// form, named after the form and asking its own questions. Everything a
    /// type declares for its rows as they are goes through the declaration
    /// (`#[derive(Row)]`'s `effects = E`), never through here.
    ///
    /// Framework-internal: an app's row offers effects through its type's
    /// declaration and the bindings of [`Self::typed_with`].
    #[doc(hidden)]
    #[must_use]
    pub fn specialized(mut self, operations: impl IntoIterator<Item = ActionSpec>) -> Self {
        for operation in operations {
            self = self.operation(operation);
        }
        self
    }

    /// # Panics
    /// In a debug build, when `operation` is a document mutation; a release
    /// build drops it.
    ///
    /// Framework-internal, beside [`Self::new`]: a row offers its effects
    /// through its type's declaration.
    #[doc(hidden)]
    #[must_use]
    pub fn operation(mut self, operation: ActionSpec) -> Self {
        if crate::share::admits_outside_document(&operation, self.id()) {
            std::rc::Rc::make_mut(&mut self.resource)
                .actions
                .push(operation);
        }
        self
    }

    /// How the entity stands right now (a process's CPU use, what a timer
    /// has left), shown beside its label. A row merged into a document child
    /// that has no presentation of its own lends it this one.
    #[must_use]
    pub fn presentation(mut self, presentation: ResourcePresentation) -> Self {
        std::rc::Rc::make_mut(&mut self.resource).presentation = Some(presentation);
        self
    }

    /// Offer `effect`, bound to this row's entity (stop *this* process):
    /// invoked here, it reaches the surface's or view's handler of `E` as
    /// that value, under the effect's declared contract.
    ///
    /// Framework-internal: a row's type declares its effects
    /// (`#[effect(on = Row)]`) and [`Self::typed`] publishes them.
    #[doc(hidden)]
    #[must_use]
    pub fn effect<E: Effect>(self, effect: &E) -> Self {
        self.operation(effect.bound())
    }

    /// Offer the row's effect `effect` as `label` here, where the row can say
    /// what it is more exactly than its type can (a container's row opens
    /// *this* container by name). The effect's contract is its declaration's;
    /// only what the row reads it by is the row's.
    #[must_use]
    pub fn labelled<E, A>(
        mut self,
        effect: crate::ActionRef<E, A>,
        label: impl Into<String>,
    ) -> Self {
        let label = label.into();
        let actions = &mut std::rc::Rc::make_mut(&mut self.resource).actions;
        if let Some(spec) = actions.iter_mut().find(|spec| spec.id == effect.id) {
            spec.type_mut().label = label;
        }
        self
    }

    /// Offer `effect` with `values` already in its parameters' fields, each
    /// still asked: what the row's state knows the effect would act with
    /// unless its caller says otherwise (a calendar's current colour, a
    /// switch's next state). Published as the parameters' defaults, so a
    /// surface fills the field and an agent reads it, and the invocation
    /// binds whatever is finally given. The contract stays the declaration's.
    #[must_use]
    pub fn prefilled<R, A, I, K>(mut self, effect: crate::ActionRef<R, A>, values: I) -> Self
    where
        I: IntoIterator<Item = (K, serde_json::Value)>,
        K: Into<String>,
    {
        let values: Vec<(String, serde_json::Value)> = values
            .into_iter()
            .map(|(name, value)| (name.into(), value))
            .collect();
        for spec in std::rc::Rc::make_mut(&mut self.resource)
            .actions
            .iter_mut()
            .filter(|spec| spec.id == effect.id)
        {
            for (name, value) in &values {
                spec.prefilled(name, value.clone());
            }
        }
        self
    }

    /// Offer `view` of the row's entity: a way to show it (a message in
    /// its conversation, a range of its text) that a link or an agent
    /// navigates to, opened by one of the row's own operations.
    #[must_use]
    pub fn view(mut self, view: tilo_actions::ResourceViewSpec) -> Self {
        std::rc::Rc::make_mut(&mut self.resource).views.push(view);
        self
    }

    /// Offer the row's operation `effect` as unavailable, with `code` a
    /// program reads and `reason` a person reads.
    #[must_use]
    pub fn unavailable(
        self,
        effect: &str,
        code: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        self.amend(effect, move |spec| spec.unavailable(code, reason))
    }

    /// Say what the row's operation `effect` does on this row, where only
    /// the row can say it.
    #[must_use]
    pub fn describe(self, effect: &str, description: impl Into<String>) -> Self {
        self.amend(effect, move |spec| spec.with_description(description))
    }

    /// Change the operation `effect` this row offers, when it offers it.
    fn amend(mut self, effect: &str, change: impl FnOnce(ActionSpec) -> ActionSpec) -> Self {
        let actions = &mut std::rc::Rc::make_mut(&mut self.resource).actions;
        if let Some(at) = actions.iter().position(|action| action.id == effect) {
            let spec = actions.remove(at);
            actions.insert(at, change(spec));
        }
        self
    }

    /// Nest `row` beneath this one (a container's logs).
    #[must_use]
    pub fn row(mut self, row: LiveRow) -> Self {
        std::rc::Rc::make_mut(&mut self.resource)
            .children
            .push(std::rc::Rc::unwrap_or_clone(row.resource));
        self
    }

    pub fn id(&self) -> &str {
        self.resource.id.as_str()
    }

    /// The row `id` names, this one or one nested beneath it, as a row of its own: what a
    /// control showing that entity binds to.
    #[must_use]
    pub fn find(&self, id: &str) -> Option<Self> {
        fn walk<'a>(resource: &'a ResourceSpec, id: &str) -> Option<&'a ResourceSpec> {
            if resource.id.0 == id {
                return Some(resource);
            }
            resource.children.iter().find_map(|child| walk(child, id))
        }
        if self.resource.id.0 == id {
            return Some(self.clone());
        }
        walk(&self.resource, id).map(|found| Self {
            resource: std::rc::Rc::new(found.clone()),
        })
    }

    /// The operations the row offers, as it publishes them.
    #[must_use]
    pub fn operations(&self) -> &[ActionSpec] {
        &self.resource.actions
    }

    /// The row as the published tree carries it.
    pub fn into_resource(self) -> ResourceSpec {
        let mut resource = std::rc::Rc::unwrap_or_clone(self.resource);
        crate::share::offer_tree(&mut resource);
        resource
    }

    /// Whether `self` and `other` are the same row, handed out again: a
    /// clone of one row, not two rows that read alike.
    pub fn same_row(&self, other: &Self) -> bool {
        std::rc::Rc::ptr_eq(&self.resource, &other.resource)
    }
}

/// A control binds to the row it shows: `ActionBinding::unit_for(&row, effect.id())`.
impl Exports for LiveRow {
    fn exported(&self) -> &ResourceSpec {
        &self.resource
    }
}
