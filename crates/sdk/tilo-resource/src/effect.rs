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
//! A variant that only changes what this participant sees — open a snippet,
//! reveal a tag, show a result — is a `view`: published as a `View`
//! operation (any reader may invoke it, a collaborator may not move someone
//! else's view, repeating it is harmless), answered by the same handler.
//!
//! An effect that reaches the files, hardware or services of the device it
//! runs on — set this computer up, kill a process here, read a local
//! folder — is `device_only`: the door runs it for the person at that
//! device and refuses collaborators and remote participants. Mark one
//! variant, or the whole vocabulary (`#[effect(operation = "…",
//! device_only)]`).
//!
//! An effect that acts on one item — open *this* snippet, connect *this*
//! connection — is offered on the item itself with its arguments bound
//! ([`Effect::bound`]): a published child or a live row carries it, and
//! invoking it there reaches the handler as that value. Such an effect is
//! often `row`: offered only bound, never on the resource with an id for
//! the caller to type.
//!
//! Variant options: `label = "…"` (else the first doc line), `view` (changes
//! only what is shown; answers at once), `row` (offered only bound to the
//! items it acts on), `job` (runs in the background and reports progress,
//! then its result), `cancellable` (a job the caller may cancel), `returns`
//! (answers with a JSON value), `content` (answers with content another
//! app's operation takes: bytes in this device's content store, named by
//! their hash as a `tilo.content` resource; an operation that takes content
//! collects it with `ActionSpec::prompted_content`, so a caller chains the
//! two by type), `destructive` (asks for confirmation),
//! `risk = "path"` (a `fn(&Self) -> RiskLevel` classifying one invocation:
//! one it finds destructive asks for confirmation, so formatting the system
//! disk can, and another disk need not), `privileged` (hands out or shows a
//! credential, such as copying a one-time
//! code: asks for confirmation too), `sensitive` (discloses a copy of private
//! data, such as exporting an address book: asks for confirmation too), `idempotent` (safe to repeat after an
//! uncertain outcome), `device_only` (only the person at this device),
//! `programmatic` (for programs and agents, never offered in the palette),
//! `untrusted` (answers with content from outside the user's data, such as a
//! web page's text: the caller reasons about it and never follows it),
//! `participant` (a view that moves only the invoking participant's own
//! view, such as an agent's caret, so a remote participant may invoke it) and
//! `permission = "read"` (any participant may invoke
//! it; an effect needs write access by default, a view read access). Field
//! `#[param("…")]` describes a parameter; `#[param(secret, "…")]` makes it a
//! secret the host keeps out of logs, history and confirmations, and
//! `#[param("…", choices = EXPR)]` names the values it takes when they are a
//! fixed list (`EXPR` yields strings: a constant table, an array of names).

use tilo_actions::{
    ActionContract, ActionParam, ActionSpec, ConfirmationPolicy, EncodedAction, ExecutionMode,
    OperationKind, PermissionRequirement, ResultSpec, RetryPolicy, RiskLevel,
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

    /// The published action of every effect offered on the resource: all
    /// but the `row` ones, which are offered only [bound](Self::bound).
    fn specs() -> Vec<ActionSpec> {
        Self::declarations()
            .into_iter()
            .filter(|decl| !decl.row)
            .map(|decl| decl.spec(Self::OPERATION))
            .collect()
    }

    /// This effect with its arguments as one action that takes none: what a
    /// published child or a live row offers (open *this* snippet, stop
    /// *this* process), answered by the vocabulary's handler like any other
    /// invocation of the effect, under the same contract as its
    /// declaration.
    fn bound(&self) -> ActionSpec {
        let id = self.id();
        let encoded = EncodedAction::encode_operation(
            Self::OPERATION,
            &Payload {
                effect: id.to_owned(),
                args: self.arguments(),
            },
        )
        .unwrap_or_else(|_| EncodedAction {
            type_id: Self::OPERATION.to_owned(),
            payload: Vec::new(),
        });
        match Self::declarations().into_iter().find(|decl| decl.id == id) {
            Some(decl) => decl.spec_with(Self::OPERATION, Vec::new(), move |_| encoded.clone()),
            None => ActionSpec::typed_operation(
                id,
                id,
                id,
                OperationKind::Effect,
                Self::OPERATION,
                Vec::new(),
                move |_| encoded.clone(),
            ),
        }
    }

    /// This effect bound to the item it acts on, as [`bound`](Self::bound),
    /// still asking its caller for the arguments named in `asking`: rename
    /// *this* file, to a name the caller gives. The bound arguments are this
    /// effect's own; an asked one the caller supplies replaces it.
    fn bound_asking(&self, asking: &[&str]) -> ActionSpec {
        let params = Self::declarations()
            .into_iter()
            .find(|decl| decl.id == self.id())
            .map(|decl| {
                decl.params
                    .into_iter()
                    .filter(|param| asking.contains(&&*param.name))
                    .collect()
            })
            .unwrap_or_default();
        self.bound_asking_params(params)
    }

    /// [`bound_asking`](Self::bound_asking) with the asked parameters given
    /// whole, narrowed for this item: the applications that can open *this*
    /// file as the choices of `app`.
    fn bound_asking_params(&self, params: Vec<ActionParam>) -> ActionSpec {
        let id = self.id();
        let Some(decl) = Self::declarations().into_iter().find(|decl| decl.id == id) else {
            return self.bound();
        };
        let bound = self.arguments();
        decl.spec_with(Self::OPERATION, params, move |args| {
            let mut merged = bound.clone();
            merged.extend(args.to_json());
            EncodedAction::encode_operation(
                Self::OPERATION,
                &Payload {
                    effect: id.to_owned(),
                    args: merged,
                },
            )
            .unwrap_or_else(|_| EncodedAction {
                type_id: Self::OPERATION.to_owned(),
                payload: Vec::new(),
            })
        })
    }

    /// The effects offered on the item `item` of the collection `field`
    /// (`#[effect(item = field)]`), each with its first parameter bound to
    /// the item and the rest still asked.
    fn item_specs(field: &str, item: &crate::Id) -> Vec<ActionSpec> {
        Self::declarations()
            .into_iter()
            .filter(|decl| decl.item == Some(field))
            .filter_map(|decl| {
                let (first, rest) = decl.params.split_first()?;
                let bound = (
                    first.name.to_string(),
                    serde_json::Value::String(item.as_str().to_owned()),
                );
                let id = decl.id;
                Some(decl.spec_with(Self::OPERATION, rest.to_vec(), move |args| {
                    let mut merged = args.to_json();
                    merged.insert(bound.0.clone(), bound.1.clone());
                    EncodedAction::encode_operation(
                        Self::OPERATION,
                        &Payload {
                            effect: id.to_owned(),
                            args: merged,
                        },
                    )
                    .unwrap_or_else(|_| EncodedAction {
                        type_id: Self::OPERATION.to_owned(),
                        payload: Vec::new(),
                    })
                }))
            })
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
#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag is an independent property a declaration states, as the derive's options do"
)]
pub struct EffectDecl {
    pub id: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub params: Vec<ActionParam>,
    /// `View` for an effect that only changes what its caller sees, else
    /// `Effect`.
    pub kind: OperationKind,
    /// Offered only bound to the items it acts on, not on the resource.
    pub row: bool,
    pub answers: Answers,
    /// How much harm invoking it can do: `Destructive` (deletes,
    /// overwrites, sends) and `Privileged` (hands out or shows a credential)
    /// ask for confirmation; a view is `None`.
    pub risk: RiskLevel,
    /// Safe to repeat after an uncertain outcome.
    pub idempotent: bool,
    pub permission: PermissionRequirement,
    /// Reaches files, hardware or services of the device it runs on: only
    /// the person at that device may invoke it, never a collaborator or a
    /// remote participant.
    pub device_only: bool,
    /// How much harm one invocation can do, classified from its arguments
    /// (formatting the system disk, against any other): an invocation the
    /// classifier finds destructive asks for the confirmation a declared
    /// destructive effect does.
    pub risk_of: Option<fn(&tilo_actions::ActionArgs) -> RiskLevel>,
    /// Invoked by programs and agents, never offered to a person picking
    /// from the palette or a menu (an agent reading a page's structure).
    pub programmatic: bool,
    /// Answers with content from outside the user's own data (a web page's
    /// text and logs): data to reason about, never instructions to follow.
    pub untrusted: bool,
    /// A view that changes only the invoking participant's own view (an
    /// agent's caret, its Find), so a remote participant may invoke it
    /// without moving anybody else's.
    pub participant: bool,
    /// A view of the invoker that submits only edits of this record (typing
    /// at the invoker's caret): its kind is Undoable, offered to whoever may
    /// edit, and the edits it submits are checked like any other.
    pub edits: bool,
    /// The collection field whose items the effect is offered on
    /// (`#[effect(item = connections)]`), its first parameter bound to the
    /// item's id: published on each item resource, as an item verb is.
    pub item: Option<&'static str>,
}

/// The contract tag of an effect that submits only edits of its record
/// ([`EffectDecl::edits`]): its kind is Undoable.
pub const EDITS_TAG: &str = "submits-edits";

/// How an effect answers its caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answers {
    /// At once, with nothing to report.
    Nothing,
    /// At once, with a JSON value.
    Value,
    /// At once, with content: one `tilo.content` resource another app's
    /// content-collecting operation takes.
    Content,
    /// Later: a job that reports progress, then its result.
    Later {
        cancellable: bool,
        /// The completed job returns a JSON value, independently of its job id.
        returns: bool,
    },
}

impl EffectDecl {
    /// The published action of this effect under `operation`.
    pub fn spec(&self, operation: &'static str) -> ActionSpec {
        let id = self.id;
        self.spec_with(operation, self.params.clone(), move |args| {
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
        })
    }

    /// The published action of this effect, taking `params` and encoding
    /// an invocation with `build`.
    /// What kind of action the effect is: a view reads, anything else
    /// reaches outside the document.
    #[must_use]
    pub fn action_kind(&self) -> crate::ActionKind {
        if self.edits {
            crate::ActionKind::Undoable
        } else if self.kind == OperationKind::View {
            crate::ActionKind::Read
        } else {
            crate::ActionKind::Outside
        }
    }

    fn spec_with(
        &self,
        operation: &'static str,
        params: Vec<ActionParam>,
        build: impl Fn(&tilo_actions::ActionArgs) -> EncodedAction + Send + Sync + 'static,
    ) -> ActionSpec {
        let view = self.kind == OperationKind::View;
        let mut spec = ActionSpec::typed_operation(
            self.id,
            self.label,
            self.label,
            if view {
                OperationKind::View
            } else {
                OperationKind::Effect
            },
            operation,
            params,
            build,
        );
        if !self.description.is_empty() {
            spec = spec.with_description(self.description);
        }
        let base = if view {
            ActionContract::view()
        } else {
            ActionContract::default()
        };
        spec.contract = ActionContract {
            permission: self.permission,
            risk: self.risk,
            confirmation: if matches!(
                self.risk,
                RiskLevel::Destructive | RiskLevel::Privileged | RiskLevel::Sensitive
            ) {
                ConfirmationPolicy::Confirm
            } else {
                ConfirmationPolicy::None
            },
            execution: match self.answers {
                Answers::Later { cancellable, .. } => ExecutionMode::Background { cancellable },
                Answers::Nothing | Answers::Value | Answers::Content => ExecutionMode::Immediate,
            },
            result: match self.answers {
                Answers::Later { returns: false, .. } => ResultSpec::Job,
                Answers::Value | Answers::Later { returns: true, .. } => ResultSpec::Json {
                    schema: serde_json::json!({}),
                },
                Answers::Content => ResultSpec::Resource {
                    resource_kind: tilo_actions::CONTENT_RESOURCE_KIND.to_owned(),
                },
                Answers::Nothing => ResultSpec::None,
            },
            retry: if view || self.idempotent {
                RetryPolicy::Idempotent
            } else {
                base.retry
            },
            tags: {
                let mut tags = if self.device_only {
                    ::std::vec![tilo_actions::LOCAL_DEVICE_ONLY.to_owned()]
                } else {
                    base.tags.clone()
                };
                if self.programmatic {
                    tags.push(tilo_actions::PROGRAMMATIC_TAG.to_owned());
                }
                if self.untrusted {
                    tags.push(tilo_actions::UNTRUSTED_CONTENT_TAG.to_owned());
                }
                if self.participant {
                    tags.push(tilo_actions::PARTICIPANT_VIEW_TAG.to_owned());
                }
                if self.edits {
                    tags.push(EDITS_TAG.to_owned());
                }
                tags
            },
            ..base
        };
        match self.risk_of {
            Some(classify) => spec.with_risk(classify),
            None => spec,
        }
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
        /// Copy a thing's key.
        #[effect(privileged, permission = "read")]
        CopyKey { thing: String },
        /// Paint a thing.
        Paint {
            #[param("The colour", choices = ["red", "green"])]
            colour: String,
            #[param(choices = SHADES)]
            shade: Option<String>,
        },
    }

    const SHADES: [&str; 2] = ["light", "dark"];

    #[test]
    fn a_derived_vocabulary_publishes_each_variant_and_round_trips_through_its_spec() {
        let specs = Sample::specs();
        let ids: Vec<&str> = specs.iter().map(|spec| spec.id.as_str()).collect();
        assert_eq!(ids, ["reload", "rename-thing", "wipe", "copy-key", "paint"]);

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

        let paint = &specs[4];
        assert_eq!(paint.params()[0].description, "The colour");
        let choices = |index: usize| -> Vec<&str> {
            paint.params()[index]
                .choices
                .iter()
                .map(std::convert::AsRef::as_ref)
                .collect()
        };
        assert_eq!(choices(0), ["red", "green"]);
        assert_eq!(choices(1), ["light", "dark"]);
        assert!(!paint.params()[1].required);

        let wipe = &specs[2];
        assert_eq!(wipe.label, "Wipe");
        assert_eq!(wipe.contract.risk, RiskLevel::Destructive);
        assert_eq!(wipe.contract.confirmation, ConfirmationPolicy::Confirm);
        assert_eq!(
            wipe.contract.execution,
            ExecutionMode::Background { cancellable: true }
        );

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

        // Bound to its arguments, the effect takes none and keeps its
        // declaration's contract.
        let bound = Sample::Wipe { passes: 3 }.bound();
        assert_eq!(bound.id, "wipe");
        assert_eq!(bound.label, "Wipe");
        assert!(bound.params().is_empty());
        assert_eq!(bound.contract.confirmation, ConfirmationPolicy::Confirm);
        let encoded = bound.build_json(&serde_json::Map::new()).unwrap();
        assert_eq!(
            Sample::from_payload(&encoded.payload).unwrap(),
            Sample::Wipe { passes: 3 }
        );
        assert!(Sample::decode("format", &serde_json::Map::new()).is_err());

        // Bound to the thing it acts on, it still asks for the rest.
        let asking = Sample::RenameThing {
            thing: "a".into(),
            label: None,
        }
        .bound_asking(&["label"]);
        assert_eq!(asking.id, "rename-thing");
        let asked: Vec<&str> = asking.params().iter().map(|param| &*param.name).collect();
        assert_eq!(asked, ["label"]);
        let mut label = serde_json::Map::new();
        label.insert("label".into(), "b".into());
        let encoded = asking.build_json(&label).unwrap();
        assert_eq!(
            Sample::from_payload(&encoded.payload).unwrap(),
            Sample::RenameThing {
                thing: "a".into(),
                label: Some("b".into()),
            }
        );
    }

    /// A vocabulary with presentation-only effects and effects offered only
    /// on the items they act on.
    #[derive(Clone, Debug, PartialEq, Effect)]
    #[effect(operation = "test.rows")]
    enum Rows {
        /// Open an item.
        #[effect(view, row)]
        Open { item: String },
        /// Connect to an item.
        #[effect(row, job, idempotent)]
        Connect { item: String },
        /// Search.
        #[effect(view)]
        Search,
    }

    #[test]
    fn views_publish_as_views_and_row_effects_only_bound_to_their_item() {
        let specs = Rows::specs();
        let ids: Vec<&str> = specs.iter().map(|spec| spec.id.as_str()).collect();
        assert_eq!(
            ids,
            ["search"],
            "a row effect is not offered on the resource"
        );
        let search = &specs[0];
        assert_eq!(search.kind, OperationKind::View);
        assert_eq!(search.contract.permission, PermissionRequirement::Read);
        assert_eq!(search.contract.risk, RiskLevel::None);
        assert_eq!(search.contract.retry, RetryPolicy::Idempotent);

        let open = Rows::Open { item: "a".into() };
        let bound = open.bound();
        assert_eq!(
            (bound.id.as_str(), bound.kind),
            ("open", OperationKind::View)
        );
        assert!(bound.params().is_empty());
        let encoded = bound.build_json(&serde_json::Map::new()).unwrap();
        assert_eq!(encoded.type_id, "test.rows");
        assert_eq!(Rows::from_payload(&encoded.payload).unwrap(), open);

        let connect = Rows::Connect { item: "b".into() }.bound();
        assert_eq!(connect.kind, OperationKind::Effect);
        assert_eq!(connect.contract.permission, PermissionRequirement::Write);
        assert_eq!(connect.contract.retry, RetryPolicy::Idempotent);
        assert_eq!(
            connect.contract.execution,
            ExecutionMode::Background { cancellable: false }
        );
        assert_eq!(
            Sample::specs()[0].contract.retry,
            RetryPolicy::AtMostOnce,
            "an effect is not repeated unless it says it may be"
        );
    }

    #[test]
    fn a_privileged_effect_asks_first_and_binds_to_the_entity_it_names() {
        let copy = Sample::CopyKey { thing: "a".into() };
        let bound = copy.bound();
        assert_eq!(bound.id, "copy-key");
        assert_eq!(bound.label, "Copy a thing's key");
        assert_eq!(bound.kind, OperationKind::Effect);
        assert!(bound.params().is_empty(), "the row names the thing already");
        assert_eq!(bound.contract.risk, RiskLevel::Privileged);
        assert_eq!(bound.contract.confirmation, ConfirmationPolicy::Confirm);
        assert_eq!(bound.contract.permission, PermissionRequirement::Read);
        let encoded = bound.build_json(&serde_json::Map::new()).unwrap();
        assert_eq!(encoded.type_id, "test.effects");
        assert_eq!(Sample::from_payload(&encoded.payload).unwrap(), copy);
    }

    /// A copy of private data leaving the app asks first, without being a
    /// credential or a deletion.
    #[derive(Clone, Debug, PartialEq, Effect)]
    #[effect(operation = "test.exports")]
    enum Exports {
        /// Export the address book.
        #[effect(job, sensitive)]
        ExportBook,
    }

    #[test]
    fn a_sensitive_effect_asks_first() {
        let spec = Exports::ExportBook.bound();
        assert_eq!(spec.contract.risk, RiskLevel::Sensitive);
        assert_eq!(spec.contract.confirmation, ConfirmationPolicy::Confirm);
    }

    /// A secret argument is published as one, and still reaches the handler.
    #[derive(Clone, Debug, PartialEq, Effect)]
    #[effect(operation = "test.secrets")]
    enum Pairing {
        /// Pair a device.
        Pair {
            #[param(secret, "The invite the other device shows")]
            invite: String,
            #[param(secret)]
            pin: Option<String>,
        },
    }

    #[test]
    fn a_secret_parameter_is_published_as_a_secret() {
        let specs = Pairing::specs();
        let params = specs[0].params();
        assert_eq!(params[0].ty, tilo_actions::ParamType::Secret);
        assert_eq!(&*params[0].description, "The invite the other device shows");
        assert_eq!(params[1].ty, tilo_actions::ParamType::Secret);
        assert!(!params[1].required);
        let value = Pairing::Pair {
            invite: "grayjay://invite".into(),
            pin: None,
        };
        let encoded = specs[0].build_json(&value.arguments()).unwrap();
        assert_eq!(Pairing::from_payload(&encoded.payload).unwrap(), value);
    }

    #[test]
    fn variant_refs_bind_controls_by_id() {
        assert_eq!(Sample::RENAME_THING.id, "rename-thing");
        assert_eq!(
            Sample::RENAME_THING
                .arguments(&("b".to_owned(), Some("B".to_owned())))
                .unwrap(),
            Sample::RenameThing {
                thing: "b".into(),
                label: Some("B".into())
            }
            .arguments()
        );
    }
}
