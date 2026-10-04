//! Effects: work an app does that changes no document — reloading a list,
//! running a job, talking to a server, opening a system picker.
//!
//! An app names its effects as one typed enum and derives [`Effect`]:
//!
//! ```
//! use tilo_resource::Effect;
//!
//! #[derive(Clone, Debug, Effect)]
//! #[effect(operation = "example.backups")]
//! pub enum Backups {
//!     /// Back up now.
//!     #[effect(job, cancellable)]
//!     BackUp,
//!     /// Restore a snapshot.
//!     #[effect(job, destructive)]
//!     Restore {
//!         #[param("Id of the snapshot")]
//!         snapshot: String,
//!     },
//! }
//!
//! let restore = Backups::Restore { snapshot: "monday".into() };
//! assert_eq!(restore.id(), "restore");
//! let decoded = Backups::decode("restore", &restore.arguments()).unwrap();
//! assert!(matches!(decoded, Backups::Restore { snapshot } if snapshot == "monday"));
//! ```
//!
//! Each variant is one published action of kind `Effect`, with typed
//! parameters, so the palette, agents and scripts invoke it like the app's
//! buttons do. Unlike a resource's `Action`, an effect is never replayed: it
//! runs where it was invoked, and what it changes in the document it changes
//! through ordinary actions. `tilo-app` runs the handler.
//!
//! Variant options: `label = "…"` (else the first doc line), `job` (runs in
//! the background and reports progress, then its result), `cancellable`
//! (a job the caller may cancel), `returns` (answers with a JSON value),
//! `destructive` (asks for confirmation) and `permission = "read"` (any
//! participant may invoke it; the default needs write access). Field
//! `#[param("…")]` describes a parameter.

use tilo_actions::{
    ActionContract, ActionParam, ActionSpec, ConfirmationPolicy, EncodedAction, ExecutionMode,
    OperationKind, PermissionRequirement, ResultSpec, RiskLevel,
};

/// A typed vocabulary of effects. Derive it (`#[derive(Effect)]`); see the
/// [module docs](self).
pub trait Effect: Sized + 'static {
    /// The operation type every effect of the vocabulary travels under:
    /// unique per vocabulary, like a resource's kind.
    const OPERATION: &'static str;

    /// One declaration per effect, in order.
    fn declarations() -> Vec<EffectDecl>;

    /// This effect's id among the declarations.
    fn id(&self) -> &'static str;

    /// This effect's arguments, named as its declaration's parameters.
    fn arguments(&self) -> serde_json::Map<String, serde_json::Value>;

    /// The effect `id` with `arguments`, as a caller invoked it.
    ///
    /// # Errors
    /// Returns why the id or the arguments do not name an effect.
    fn decode(
        id: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Self, String>;

    /// The published action of every effect.
    fn specs() -> Vec<ActionSpec> {
        Self::declarations()
            .into_iter()
            .map(|decl| decl.spec(Self::OPERATION))
            .collect()
    }

    /// Decode one invocation's payload, as the operation's specs encode it.
    ///
    /// # Errors
    /// Returns why the payload is not an effect of this vocabulary.
    fn from_payload(payload: &[u8]) -> Result<Self, String> {
        let Payload { effect, args } =
            serde_json::from_slice(payload).map_err(|error| error.to_string())?;
        Self::decode(&effect, &args)
    }
}

/// How the effect's specs carry an invocation: its id and its arguments.
#[derive(serde::Serialize, serde::Deserialize)]
struct Payload {
    effect: String,
    #[serde(default)]
    args: serde_json::Map<String, serde_json::Value>,
}

/// The declaration of one effect: what callers see of it.
#[derive(Clone, Debug)]
pub struct EffectDecl {
    pub id: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub params: Vec<ActionParam>,
    pub answers: Answers,
    /// How much harm invoking it can do. A destructive effect (deletes,
    /// overwrites, sends) or a privileged one (creates or reveals a
    /// credential) asks for confirmation.
    pub risk: RiskLevel,
    pub permission: PermissionRequirement,
}

/// How an effect answers its caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answers {
    /// At once, with nothing to report.
    Nothing,
    /// At once, with a JSON value.
    Value,
    /// Later: a job that reports progress, then its result.
    Later { cancellable: bool },
}

impl EffectDecl {
    /// The published action of this effect under `operation`.
    pub fn spec(&self, operation: &'static str) -> ActionSpec {
        let id = self.id;
        let mut spec = ActionSpec::typed_operation(
            id,
            self.label,
            self.label,
            OperationKind::Effect,
            operation,
            self.params.clone(),
            move |args| {
                EncodedAction::encode_operation(
                    operation,
                    &Payload {
                        effect: id.to_owned(),
                        args: args.to_json(),
                    },
                )
                .unwrap_or_else(|_| EncodedAction {
                    type_id: operation.to_owned(),
                    payload: Vec::new(),
                })
            },
        );
        if !self.description.is_empty() {
            spec = spec.with_description(self.description);
        }
        spec.contract = ActionContract {
            permission: self.permission,
            risk: self.risk,
            confirmation: if matches!(self.risk, RiskLevel::Destructive | RiskLevel::Privileged) {
                ConfirmationPolicy::Confirm
            } else {
                ConfirmationPolicy::None
            },
            execution: match self.answers {
                Answers::Later { cancellable } => ExecutionMode::Background { cancellable },
                Answers::Nothing | Answers::Value => ExecutionMode::Immediate,
            },
            result: match self.answers {
                Answers::Later { .. } => ResultSpec::Job,
                Answers::Value => ResultSpec::Json {
                    schema: serde_json::json!({}),
                },
                Answers::Nothing => ResultSpec::None,
            },
            ..ActionContract::default()
        };
        spec
    }

    /// Whether the effect runs in the background, reporting its result later.
    pub fn is_job(&self) -> bool {
        matches!(self.answers, Answers::Later { .. })
    }
}

/// The vocabulary of a surface that has no effects.
#[derive(Clone, Copy, Debug)]
pub enum NoEffect {}

impl Effect for NoEffect {
    const OPERATION: &'static str = "tilo.no-effect";

    fn declarations() -> Vec<EffectDecl> {
        Vec::new()
    }

    // No value of the type exists to ask.
    fn id(&self) -> &'static str {
        ""
    }

    fn arguments(&self) -> serde_json::Map<String, serde_json::Value> {
        serde_json::Map::new()
    }

    fn decode(id: &str, _: &serde_json::Map<String, serde_json::Value>) -> Result<Self, String> {
        Err(format!("there is no effect `{id}`"))
    }
}

/// Read one argument of an effect being decoded. An absent argument reads
/// as `null`, which an `Option` field takes as `None`.
///
/// # Errors
/// Returns why the argument does not have the field's type.
#[doc(hidden)]
pub fn argument<T: serde::de::DeserializeOwned>(
    arguments: &serde_json::Map<String, serde_json::Value>,
    name: &str,
) -> Result<T, String> {
    serde_json::from_value(
        arguments
            .get(name)
            .cloned()
            .unwrap_or(serde_json::Value::Null),
    )
    .map_err(|error| format!("`{name}`: {error}"))
}

/// Put one argument of an effect being encoded; an absent optional one
/// (`null`) is left out, as a caller leaves it out.
#[doc(hidden)]
pub fn put<T: serde::Serialize>(
    arguments: &mut serde_json::Map<String, serde_json::Value>,
    name: &str,
    value: &T,
) {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::Null) | Err(_) => {}
        Ok(value) => {
            arguments.insert(name.to_owned(), value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Effect;

    /// What the derive must produce, checked through the spec a caller sees.
    #[derive(Clone, Debug, PartialEq, Effect)]
    #[effect(operation = "test.effects")]
    enum Sample {
        /// Reload everything.
        Reload,
        /// Rename a thing.
        ///
        /// It keeps its id.
        #[effect(returns, permission = "read")]
        RenameThing {
            #[param("The thing")]
            thing: String,
            label: Option<String>,
        },
        #[effect(label = "Wipe", job, cancellable, destructive)]
        Wipe { passes: u32 },
        /// Reveal the signing key.
        #[effect(privileged, permission = "admin")]
        RevealKey,
    }

    #[test]
    fn a_derived_vocabulary_publishes_each_variant_and_round_trips_through_its_spec() {
        let specs = Sample::specs();
        let ids: Vec<&str> = specs.iter().map(|spec| spec.id.as_str()).collect();
        assert_eq!(ids, ["reload", "rename-thing", "wipe", "reveal-key"]);
        assert_eq!(specs[0].contract.risk, RiskLevel::Low);
        assert_eq!(specs[0].contract.confirmation, ConfirmationPolicy::None);

        let rename = &specs[1];
        assert_eq!(rename.label, "Rename a thing");
        assert_eq!(
            rename.description.as_deref(),
            Some("Rename a thing. It keeps its id.")
        );
        assert_eq!(rename.kind, OperationKind::Effect);
        assert_eq!(rename.contract.permission, PermissionRequirement::Read);
        assert!(matches!(rename.contract.result, ResultSpec::Json { .. }));
        assert_eq!(rename.contract.execution, ExecutionMode::Immediate);

        let wipe = &specs[2];
        assert_eq!(wipe.label, "Wipe");
        assert_eq!(wipe.contract.risk, RiskLevel::Destructive);
        assert_eq!(wipe.contract.confirmation, ConfirmationPolicy::Confirm);
        assert_eq!(
            wipe.contract.execution,
            ExecutionMode::Background { cancellable: true }
        );

        let reveal = &specs[3];
        assert_eq!(reveal.contract.risk, RiskLevel::Privileged);
        assert_eq!(reveal.contract.confirmation, ConfirmationPolicy::Confirm);
        assert_eq!(reveal.contract.permission, PermissionRequirement::Admin);

        // A caller's arguments, encoded by the published spec, decode to the
        // value the handler receives.
        let value = Sample::RenameThing {
            thing: "a".into(),
            label: None,
        };
        let encoded = rename.build_json(&value.arguments()).unwrap();
        assert_eq!(encoded.type_id, "test.effects");
        assert_eq!(Sample::from_payload(&encoded.payload).unwrap(), value);
        assert_eq!(
            Sample::from_payload(
                &specs[0]
                    .build_json(&serde_json::Map::new())
                    .unwrap()
                    .payload
            )
            .unwrap(),
            Sample::Reload
        );
        assert!(Sample::decode("wipe", &serde_json::Map::new()).is_err());
        assert!(Sample::decode("format", &serde_json::Map::new()).is_err());
    }

    #[test]
    fn variant_refs_bind_controls_by_id() {
        assert_eq!(Sample::RENAME_THING.id, "rename-thing");
        assert_eq!(
            Sample::RENAME_THING.arguments(&("b".to_owned(), Some("B".to_owned()))),
            Sample::RenameThing {
                thing: "b".into(),
                label: Some("B".into())
            }
            .arguments()
        );
    }
}
