//! Changes a runtime sends on instead of making them: a task's programs (agents.md, "How a
//! program runs").
//!
//! A task's tilolang host holds no records and runs no app's code. What a program changes it
//! changes by invoking: a method call is its type's declared action, an assignment the
//! resource's field edit, each sent to the [`Invoker`] the host set, which invokes it at the
//! action's own app as a child of the task. The runtime neither decides nor runs it; the
//! invocation's answer is what the program gets.

use crate::ids::{ActionId, AppId, FieldId};
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
    },
    /// The field `field` (its name; `field_id` its id) of the resource `target` set to `value`.
    Edit {
        app: AppId,
        target: ResourceRef,
        field: String,
        field_id: FieldId,
        value: Value,
    },
}

impl Invocation {
    /// The app whose node decides it.
    #[must_use]
    pub fn app(&self) -> &str {
        match self {
            Self::Action { app, .. } | Self::Edit { app, .. } => app,
        }
    }

    /// Its name as a preview lists it: the action's name, or `field =` for an edit.
    #[must_use]
    pub fn name(&self) -> String {
        match self {
            Self::Action { name, .. } => name.clone(),
            Self::Edit { field, .. } => format!("{field} ="),
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
    /// Policy refused it: the program gets the reason as an error value.
    Refused(String),
    /// Policy asks the person first: the program stops here and its step waits on the answer.
    Asks(String),
}

/// Where a runtime sends the changes a program makes.
pub trait Invoker {
    fn invoke(&self, invocation: &Invocation) -> Invoked;
}
