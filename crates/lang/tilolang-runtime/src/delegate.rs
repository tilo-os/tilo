//! Changes a runtime sends on instead of making them: a task's programs (agents.md, "How a
//! program runs").
//!
//! A task's tilolang host holds no records and runs no app's code. What a program changes it
//! changes by invoking: a method call is its type's declared action, an assignment the
//! resource's field edit, each sent to the [`Invoker`] the host set, which invokes it at the
//! action's own app as a child of the task. The runtime neither decides nor runs it; the
//! invocation's answer is what the program gets.

use crate::ids::{ActionId, AppId, FieldId, ModuleHash};
use crate::meta::EffectClass;
use crate::value::{ResourceRef, Value};

/// One change a program asks for.
#[derive(Clone, Debug, PartialEq)]
pub enum Invocation {
    /// A declared action of an app, with every argument by its parameter's name, in order.
    Action {
        app: AppId,
        action: ActionId,
        /// Its name, as the app publishes it.
        name: String,
        args: Vec<(String, Value)>,
        class: EffectClass,
        /// It destroys something (`destructive action`).
        destructive: bool,
        /// The module of the code that invokes it: the view's or the block's author, so the
        /// app it goes to can record whose code asked. `None` for an invocation no code of
        /// this runtime made.
        author: Option<ModuleHash>,
        /// The resource it is invoked on: the target an action of another app names as the
        /// resource whose action API takes the child invocation.
        target: Option<ResourceRef>,
        /// The resource the invocation is made from, when it is made from one: the action API that
        /// takes it records what it makes as made from it.
        from: Option<ResourceRef>,
        /// A person is asked before it runs: the code that makes it may not make it unasked, so
        /// the dispatch keeps it as its invocation record, asked, until they answer.
        asks: bool,
        /// What the person asked about it is shown, as labels and the values the action's
        /// `shows:` names, read where the call is made: the run that makes it reads what it
        /// wrote before the call, which nothing outside it can read yet. Empty for a call that
        /// does not ask, and for an action that declares nothing to show.
        shows: Vec<Shown>,
    },
    /// The field `field` (its name; `field_id` its id) of the resource `target` set to `value`,
    /// at `path` when the field is one of a record inside the resource.
    Edit {
        app: AppId,
        target: ResourceRef,
        /// The fields from the resource down to the record `field` is a field of, by the keys
        /// the record stores them under; empty when `field` is a field of the resource itself.
        path: Vec<String>,
        field: String,
        field_id: FieldId,
        value: Value,
    },
}

impl Invocation {
    /// An action's arguments as its target's action API takes them, by name, as any caller gives them
    /// ([`crate::value::to_argument`]): the target itself and every option left out are not sent.
    #[must_use]
    pub fn api_arguments(&self) -> serde_json::Map<String, serde_json::Value> {
        let Self::Action { args, target, .. } = self else {
            return serde_json::Map::new();
        };
        let target = target.as_ref().map(ResourceRef::id_string);
        args.iter()
            .filter(|(_, value)| match value {
                Value::Ref(r) => target.as_ref() != Some(&r.target.id_string()),
                value => !matches!(value, Value::Opt(None)),
            })
            .map(|(name, value)| {
                let json = crate::value::to_argument(value).unwrap_or_default();
                (name.clone(), json)
            })
            .collect()
    }

    /// The app whose node decides it.
    #[must_use]
    pub fn app(&self) -> &str {
        match self {
            Self::Action { app, .. } | Self::Edit { app, .. } => app,
        }
    }

    /// Its name as a preview lists it: the action's name, or `field =` for an edit, under the
    /// record it is a field of when it is nested.
    #[must_use]
    pub fn name(&self) -> String {
        match self {
            Self::Action { name, .. } => name.clone(),
            Self::Edit { path, field, .. } if path.is_empty() => format!("{field} ="),
            Self::Edit { path, field, .. } => format!("{}.{field} =", path.join(".")),
        }
    }

    /// The resources it names: an action's resource arguments, an edit's target.
    #[must_use]
    pub fn resources(&self) -> Vec<ResourceRef> {
        match self {
            Self::Action { args, .. } => args
                .iter()
                .filter_map(|(_, value)| match value {
                    Value::Ref(r) => Some(ResourceRef::clone(&r.target)),
                    _ => None,
                })
                .collect(),
            Self::Edit { target, .. } => vec![target.clone()],
        }
    }

    /// Whether it destroys something.
    #[must_use]
    pub fn destroys(&self) -> bool {
        matches!(
            self,
            Self::Action {
                destructive: true,
                ..
            }
        )
    }
}

/// How an invocation the runtime sent on ended.
#[derive(Clone, Debug, PartialEq)]
pub enum Invoked {
    /// It ran; its result (unit for none).
    Done(Value),
    /// It ran and failed, and changed nothing.
    Failed(String),
    /// Policy refused it: the program gets the reason as an error value. An answer to a call a
    /// run waits on ([`crate::Core::answer_call`]) refuses the run itself: nothing of it stands.
    Refused(String),
    /// It went to the app that holds the resource as the call named here, which answers it
    /// later: a run that made it waits on that answer ([`crate::Core::answer_call`]).
    Pending(String),
}

/// Where a runtime sends the changes a program makes.
pub trait Invoker {
    fn invoke(&self, invocation: &Invocation) -> Invoked;
}

/// One thing a person asked to allow a call reads about it: what it is, in their language, the
/// value it holds where the call is made, and how it is shown (`addresses`, `long`, `secret`;
/// `None` for plain text), as the action's `shows:` declares.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shown {
    pub label: String,
    pub value: String,
    pub render: Option<String>,
}
