//! A resource type's actions with the type erased.
//!
//! What the framework publishes for a resource — its declared actions,
//! `apply`, `group`, field edits, item resources and their schemas — is the
//! same for every resource, apart from a few facts and functions of the
//! type: its kind and label, its declarations, how a JSON value or a field
//! edit becomes one of its actions. A [`Vocabulary`] gathers those once per
//! type and a [`Built`] carries one of its actions without its type, so the
//! publishing code is compiled once rather than once per resource of every
//! app.

use std::any::{Any, TypeId};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use tilo_actions::{
    ActionArgs, ActionError, ActionNeed, ActionParam, ActionSpec, EncodedAction, ResourceViewSpec,
    RiskLevel,
};

use crate::action::ActionKind;
use crate::collection::Id;
use crate::path::Edit;
use crate::resource::Resource;
use crate::shape::Shape;

/// One action of some resource, built and not yet encoded.
pub struct Built(Box<dyn AnyAction>);

/// What the framework does with a built action, whatever its type.
trait AnyAction {
    fn stamp(&mut self, now_ms: i64);
    fn action_type(&self) -> &'static str;
    fn write_json(&self, out: &mut Vec<u8>) -> Result<(), serde_json::Error>;
    fn need(&self) -> ActionNeed;
    fn risk(&self) -> RiskLevel;
    fn into_any(self: Box<Self>) -> Box<dyn Any>;
}

struct Typed<R: Resource>(R::Action);

impl<R: Resource> AnyAction for Typed<R> {
    fn stamp(&mut self, now_ms: i64) {
        R::stamp(&mut self.0, now_ms);
    }

    fn action_type(&self) -> &'static str {
        R::ACTION_TYPE
    }

    fn write_json(&self, out: &mut Vec<u8>) -> Result<(), serde_json::Error> {
        serde_json::to_writer(out, &self.0)
    }

    fn need(&self) -> ActionNeed {
        crate::resource::need_of::<R>(&self.0)
    }

    fn risk(&self) -> RiskLevel {
        tilo_actions::Action::risk(&self.0)
    }

    fn into_any(self: Box<Self>) -> Box<dyn Any> {
        Box::new(self.0)
    }
}

impl Built {
    /// `action`, an action of `R`.
    #[must_use]
    pub fn of<R: Resource>(action: R::Action) -> Self {
        Self(Box::new(Typed::<R>(action)))
    }

    /// Stamp the origin's clock on it ([`Resource::stamp`]).
    pub fn stamp(&mut self, now_ms: i64) {
        self.0.stamp(now_ms);
    }

    /// What it is, for the grants deciding it ([`crate::need_of`]).
    #[must_use]
    pub fn need(&self) -> ActionNeed {
        self.0.need()
    }

    /// Its consequence class.
    #[must_use]
    pub fn risk(&self) -> RiskLevel {
        self.0.risk()
    }

    /// Encoded under its resource's registry type id.
    ///
    /// # Errors
    /// Returns [`ActionError::Encode`] when the action does not serialize.
    pub fn encode(&self) -> Result<EncodedAction, ActionError> {
        let mut payload = Vec::with_capacity(128);
        self.0
            .write_json(&mut payload)
            .map_err(ActionError::Encode)?;
        Ok(EncodedAction {
            type_id: self.0.action_type().to_owned(),
            payload,
        })
    }

    /// Its JSON form, appended to `out`.
    pub(crate) fn write_json(&self, out: &mut Vec<u8>) -> Result<(), serde_json::Error> {
        self.0.write_json(out)
    }

    /// The action as the type of the resource `R` it was built for; `None`
    /// when it is another resource's.
    #[must_use]
    pub fn into_action<R: Resource>(self) -> Option<R::Action> {
        self.0
            .into_any()
            .downcast::<R::Action>()
            .ok()
            .map(|action| *action)
    }
}

/// Builds a resource's action from arguments already bound against the
/// declaration's parameters. `now_ms` is the origin's clock, captured once so
/// replay is deterministic.
pub type Builds = Arc<dyn Fn(&ActionArgs, i64) -> Result<Built, String> + Send + Sync>;

/// One mutation offered by name, of whichever resource: what
/// [`ActionDecl`](crate::ActionDecl) holds for its resource type.
#[derive(Clone)]
pub struct Declaration {
    pub id: &'static str,
    pub label: String,
    pub description: Option<String>,
    pub params: Vec<ActionParam>,
    /// What the action is able to do. See [`tilo_actions::ActionContract::kind`].
    pub kind: ActionKind,
    /// The one field it writes, if only one. See [`Resource::field_of`].
    pub field: Option<&'static str>,
    pub destructive: bool,
    /// What the action answers its caller with, beside the change: the state
    /// it left, as the declaring resource's surface reports it.
    pub result: Option<tilo_actions::ResultSpec>,
    /// Contract tags its callers read: a precondition on the invoker's view
    /// (`ContextRequirement`), a text that may arrive in parts.
    pub tags: Vec<String>,
    /// The prompt of an action whose one input is content another action
    /// produced (an image to place), carried by its callers rather than
    /// typed: the build reads its content hash as the
    /// [`PROMPTED_PARAM`](tilo_actions::PROMPTED_PARAM) argument.
    pub content: Option<String>,
    /// A bound edit that refines the resource's declared action of the same
    /// id: it builds the same action from the same arguments, and also
    /// places its invoker (the caret an append leaves its writer at).
    /// See [`bind_declaration`](crate::bind_declaration).
    pub refines: bool,
    /// The units the action writes ([`crate::footprint`]): `record`, an
    /// item's unit, declared fields'.
    pub writes: Vec<String>,
    /// The kind of resource it is declared on ([`Resource::KIND`]).
    pub resource: &'static str,
    /// The registry type id the action it builds replays under
    /// ([`Resource::ACTION_TYPE`]).
    pub action_type: &'static str,
    pub build: Builds,
}

impl AsRef<Self> for Declaration {
    fn as_ref(&self) -> &Self {
        self
    }
}

impl Declaration {
    /// The declaration `id` of the resource `vocabulary`, labelled `label`,
    /// taking `params`, built by `build`.
    pub(crate) fn new(
        vocabulary: &Vocabulary,
        id: &'static str,
        label: String,
        params: Vec<ActionParam>,
        build: Builds,
    ) -> Self {
        Self {
            id,
            label,
            description: None,
            params,
            kind: ActionKind::Undoable,
            field: None,
            destructive: false,
            result: None,
            tags: Vec::new(),
            content: None,
            refines: false,
            writes: vec![crate::footprint::RECORD_UNIT.to_owned()],
            resource: vocabulary.kind,
            action_type: vocabulary.action_type,
            build,
        }
    }
}

/// What the framework's publishing code needs of one resource type, as
/// plain values and functions. See the [module docs](self).
pub struct Vocabulary {
    /// [`Resource::KIND`].
    pub kind: &'static str,
    /// [`Resource::LABEL`].
    pub label: &'static str,
    /// [`Resource::ACTION_TYPE`].
    pub action_type: &'static str,
    /// [`Resource::EDITS`].
    pub edits: bool,
    pub(crate) type_id: fn() -> TypeId,
    pub(crate) shape: fn() -> Option<Shape>,
    pub(crate) op_schemas: fn() -> Vec<(String, serde_json::Value)>,
    pub(crate) action_schema: fn() -> Option<serde_json::Value>,
    pub(crate) least_need: fn() -> ActionNeed,
    /// Builds every declaration afresh; read them through
    /// [`Vocabulary::declarations`], which keeps them.
    pub(crate) declare: fn() -> Vec<Declaration>,
    pub(crate) item_effects: fn(&str, &Id) -> Vec<ActionSpec>,
    pub(crate) item_views: fn(&str, &Id) -> Vec<ResourceViewSpec>,
    /// An action from its JSON form (serde's), unstamped.
    pub(crate) read: fn(serde_json::Value) -> Result<Built, String>,
    /// The action a field edit is ([`Resource::edit_action`]), unstamped.
    pub(crate) edit: fn(Edit<()>) -> Option<Built>,
}

impl Vocabulary {
    /// Every declaration of the resource. They depend on its type alone, so
    /// each thread builds them once per generation of app code
    /// ([`reset_type_schemas`](crate::reset_type_schemas)) and answers the
    /// same list after: publishing, binding a verb or reading an action no
    /// longer rebuilds every verb's parameters and builder.
    #[must_use]
    pub fn declarations(&self) -> Rc<[Declaration]> {
        type Kept = (u64, HashMap<TypeId, Rc<[Declaration]>>);
        thread_local! {
            static KEPT: RefCell<Kept> = RefCell::new((0, HashMap::new()));
        }
        let epoch = crate::resource::type_epoch();
        let key = (self.type_id)();
        let found = KEPT.with(|kept| {
            let kept = kept.borrow();
            if kept.0 == epoch {
                kept.1.get(&key).cloned()
            } else {
                None
            }
        });
        if let Some(found) = found {
            return found;
        }
        // Built outside the borrow: a declaration may read another
        // resource's vocabulary.
        let built: Rc<[Declaration]> = (self.declare)().into();
        KEPT.with(|kept| {
            let mut kept = kept.borrow_mut();
            if kept.0 != epoch {
                *kept = (epoch, HashMap::new());
            }
            kept.1.insert(key, Rc::clone(&built));
        });
        built
    }
}

fn declarations<R: Resource>() -> Vec<Declaration> {
    let mut out = Vec::new();
    for decl in R::declarations() {
        out.push(decl.into_declaration());
    }
    out
}

fn read<R: Resource>(value: serde_json::Value) -> Result<Built, String> {
    match serde_json::from_value::<R::Action>(value) {
        Ok(action) => Ok(Built::of::<R>(action)),
        Err(error) => Err(error.to_string()),
    }
}

fn edit<R: Resource>(edit: Edit<()>) -> Option<Built> {
    R::edit_action(edit.retyped()).map(Built::of::<R>)
}

struct VocabularyOf<R>(std::marker::PhantomData<R>);

impl<R: Resource> VocabularyOf<R> {
    const VOCABULARY: Vocabulary = Vocabulary {
        kind: R::KIND,
        label: R::LABEL,
        action_type: R::ACTION_TYPE,
        edits: R::EDITS,
        type_id: TypeId::of::<R>,
        shape: R::shape,
        op_schemas: R::op_schemas,
        action_schema: R::action_schema,
        least_need: R::least_need,
        declare: declarations::<R>,
        item_effects: R::item_effects,
        item_views: R::item_views,
        read: read::<R>,
        edit: edit::<R>,
    };
}

/// The vocabulary of `R`.
#[must_use]
pub fn vocabulary<R: Resource>() -> &'static Vocabulary {
    &VocabularyOf::<R>::VOCABULARY
}
