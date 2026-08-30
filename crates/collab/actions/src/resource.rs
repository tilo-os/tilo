//! The nested **Resource** tree an app publishes, and the **`ActionSpec`**s
//! each resource exports.
//!
//! A [`Resource`] is something the app currently has *open/presented*
//! (a playing video, an open book); resources **nest** — a video owns
//! its comments, a book its annotations. Each exports [`ActionSpec`]s:
//! the operations Ctrl+K offers for it.
//!
//! The split that matters: [`Resource`]/[`ActionSpec`] is
//! **presentation** — local, per-frame, with human labels and (for a
//! prompted action) a closure. The [`EncodedAction`] an `ActionSpec`
//! yields is the **action** — serializable, replayable. Presentation
//! never crosses a boundary; only the action does.
//!
//! # The typed projection
//!
//! A spec also carries a **serializable projection of itself**: a typed
//! [`params`](ActionSpec::params) list and a builder that turns bound
//! [`ActionArgs`] into that same [`EncodedAction`]. That is what lets a caller
//! with no window enumerate the surface (see [`crate::wire`]) and invoke it by
//! name — the presentation stays local, but a faithful *description* of it can
//! now cross.
//!
//! The projection is derived from the same closure the palette uses, never a
//! second copy of it: [`immediate`](ActionSpec::immediate) projects to zero
//! parameters returning its pre-encoded action, and
//! [`prompted`](ActionSpec::prompted) projects to one required
//! [`Text`](ParamType::Text) parameter named [`PROMPTED_PARAM`] that calls the
//! app's closure. So an affordance, its palette row, and an external invocation
//! all run through one function and produce the byte-identical action — the
//! covenant holds structurally rather than by test discipline.
//!
//! [`typed`](ActionSpec::typed) is the constructor for an action whose argument
//! deserves a real type (a number in `0..=1`, a fixed set of filter names)
//! instead of prose a lenient parser guesses at.

use std::sync::Arc;

use crate::action::{Action, ActionError, EncodedAction, ExecutionLocus};
use crate::params::{ActionArgs, ActionParam, ArgError, ArgValue, ParamType};

/// Builds an action's encoded form from arguments already bound against its
/// declared [`params`](ActionSpec::params).
///
/// Infallible by design: every check a caller can fail — presence, type,
/// choice — happens in [`ActionArgs::bind`] *before* this runs, so a builder
/// that holds an `ActionArgs` is looking at values it declared and can use them
/// directly.
pub type ArgsBuilder = Arc<dyn Fn(&ActionArgs) -> EncodedAction + Send + Sync>;

/// The parameter name a [`prompted`](ActionSpec::prompted) spec projects its
/// single free-text argument under.
///
/// Fixed rather than derived from the human prompt: the prompt is localisable
/// display text, and an external caller addresses parameters by a stable name.
pub const PROMPTED_PARAM: &str = "value";

/// Stable identity of a resource within the publishing app instance
/// (e.g. `tube.video:dQw4w9WgXcQ`, `ebook.annotation:<uuid>`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ResourceId(pub String);

impl ResourceId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One open/presented resource and the actions it exports. Resources
/// nest via [`children`](Resource::children).
#[derive(Clone)]
pub struct Resource {
    pub id: ResourceId,
    /// A stable kind tag (e.g. `"tube.video"`, `"ebook.annotation"`),
    /// shared with the global-search type registry for OS-wide enumeration.
    pub kind: String,
    /// Human, localisable label shown in the palette.
    pub label: String,
    pub children: Vec<Resource>,
    pub actions: Vec<ActionSpec>,
}

impl Resource {
    pub fn new(id: impl Into<String>, kind: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: ResourceId::new(id),
            kind: kind.into(),
            label: label.into(),
            children: Vec::new(),
            actions: Vec::new(),
        }
    }

    pub fn with_action(mut self, action: ActionSpec) -> Self {
        self.actions.push(action);
        self
    }

    pub fn with_child(mut self, child: Resource) -> Self {
        self.children.push(child);
        self
    }

    pub fn with_children(mut self, children: impl IntoIterator<Item = Resource>) -> Self {
        self.children.extend(children);
        self
    }
}

/// What kind of thing an action's one text argument names. Words are
/// what the asker says outright; a content hash addresses bytes already
/// in this machine's blob store (nobody types one, it names something
/// an earlier operation produced). Declared on the spec rather than
/// inferred, so a caller with no screen can tell "give me a new name"
/// from "give me the image to place".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Collects {
    /// Free text, in the words of whoever is asking.
    #[default]
    Words,
    /// The content hash of bytes already in this machine's blob store.
    Content,
}

/// How an [`ActionSpec`] produces its replayable [`EncodedAction`] when chosen.
#[derive(Clone)]
pub enum ActionInput {
    /// Fires immediately with this pre-encoded action.
    Immediate(EncodedAction),
    /// Collects one free-text argument first (the palette shows an argument
    /// step), then builds the action from it. The closure is
    /// presentation-side — only its output [`EncodedAction`] crosses a boundary.
    Prompted {
        prompt: String,
        placeholder: String,
        /// What that argument names. See [`Collects`].
        collects: Collects,
        build: Arc<dyn Fn(&str) -> EncodedAction + Send + Sync>,
    },
}

/// One action a resource exports to the palette.
#[derive(Clone)]
pub struct ActionSpec {
    /// Stable id within the resource (e.g. `"like"`, `"reply"`).
    pub id: String,
    pub label: String,
    pub description: Option<String>,
    /// Declared execution locus (mirrors the concrete action's
    /// [`Action::locus`]), so the palette can show/route it before the
    /// action is built.
    pub locus: ExecutionLocus,
    pub input: ActionInput,
    /// The typed argument surface an external caller invokes against. Private
    /// because it must stay in step with `build` — the two are always set
    /// together by a constructor, never independently.
    params: Vec<ActionParam>,
    /// Builds the encoded action from arguments bound against `params`. Shares
    /// the app's closure with `input` rather than duplicating it, which is what
    /// makes the palette path and the external path provably one function.
    build: ArgsBuilder,
}

impl ActionSpec {
    /// An action that fires immediately. `type_id` must match the app's
    /// registry registration for `A`.
    ///
    /// Projects to zero parameters: invoking it externally with an empty
    /// argument set yields exactly the pre-encoded action the palette fires.
    pub fn immediate<A>(
        id: impl Into<String>,
        label: impl Into<String>,
        type_id: impl Into<String>,
        action: &A,
    ) -> Result<Self, ActionError>
    where
        A: Action + serde::Serialize,
    {
        let locus = action.locus();
        let encoded = EncodedAction::encode(type_id, action)?;
        let for_build = encoded.clone();
        Ok(Self {
            id: id.into(),
            label: label.into(),
            description: None,
            locus,
            input: ActionInput::Immediate(encoded),
            params: Vec::new(),
            build: Arc::new(move |_| for_build.clone()),
        })
    }

    /// An action that fires immediately, from an [`EncodedAction`] the caller
    /// already holds.
    ///
    /// [`immediate`](Self::immediate) is the usual door — it takes a typed
    /// action and reads its [`locus`](Action::locus) — but a caller that has
    /// only the encoded form (a stored action, a fixture, one that arrived over
    /// a wire) has no typed value to hand it. Projects to zero parameters, the
    /// same as `immediate`.
    pub fn immediate_encoded(
        id: impl Into<String>,
        label: impl Into<String>,
        locus: ExecutionLocus,
        encoded: EncodedAction,
    ) -> Self {
        let for_build = encoded.clone();
        Self {
            id: id.into(),
            label: label.into(),
            description: None,
            locus,
            input: ActionInput::Immediate(encoded),
            params: Vec::new(),
            build: Arc::new(move |_| for_build.clone()),
        }
    }

    /// An action that first collects a free-text argument, then builds its
    /// encoded action from it.
    ///
    /// Projects to one required [`Text`](ParamType::Text) parameter named
    /// [`PROMPTED_PARAM`], described by `prompt`. The projection and the
    /// palette share the *same* `build` closure, so an external invocation and
    /// the on-screen affordance produce the byte-identical [`EncodedAction`].
    pub fn prompted(
        id: impl Into<String>,
        label: impl Into<String>,
        prompt: impl Into<String>,
        placeholder: impl Into<String>,
        locus: ExecutionLocus,
        build: impl Fn(&str) -> EncodedAction + Send + Sync + 'static,
    ) -> Self {
        let prompt = prompt.into();
        // One `Arc`, shared by the palette's `ActionInput` and the typed
        // projection. Not two closures that happen to agree.
        let build: Arc<dyn Fn(&str) -> EncodedAction + Send + Sync> = Arc::new(build);
        let for_args = build.clone();
        Self {
            id: id.into(),
            label: label.into(),
            description: None,
            locus,
            params: vec![ActionParam::required_owned(
                PROMPTED_PARAM,
                ParamType::Text,
                prompt.clone(),
            )],
            build: Arc::new(move |args| for_args(args.text(PROMPTED_PARAM).unwrap_or(""))),
            input: ActionInput::Prompted {
                prompt,
                placeholder: placeholder.into(),
                collects: Collects::Words,
                build,
            },
        }
    }

    /// An action with a real typed argument surface.
    ///
    /// This is the constructor for an argument that has a *type* — a number in
    /// `0..=1`, a rate, one of a fixed set of filter names — rather than prose
    /// a lenient parser guesses at. [`ActionArgs::bind`] enforces the
    /// declaration, so a caller that sends `"hlaf"` for a choice parameter gets
    /// a named refusal instead of a silent default.
    ///
    /// The palette can still offer it. `input` is derived from `params`:
    ///
    /// * no parameters → [`Immediate`](ActionInput::Immediate) of the action
    ///   `build` returns for an empty argument set;
    /// * otherwise → [`Prompted`](ActionInput::Prompted) collecting the
    ///   *primary* parameter (the first required one, else the first), parsed
    ///   into its declared type.
    ///
    /// A palette entry is one text field, so for a multi-parameter action it
    /// can only carry the primary argument and the rest are left unbound —
    /// which is why `build` sees `ActionArgs` and decides what an absent
    /// optional means. Text the palette cannot parse into the primary
    /// parameter's type is likewise left unbound rather than coerced; only the
    /// keyboard path can be that lossy, and the external path stays strict.
    pub fn typed(
        id: impl Into<String>,
        label: impl Into<String>,
        placeholder: impl Into<String>,
        locus: ExecutionLocus,
        params: Vec<ActionParam>,
        build: impl Fn(&ActionArgs) -> EncodedAction + Send + Sync + 'static,
    ) -> Self {
        let build: ArgsBuilder = Arc::new(build);
        let input = derive_input(&params, &build, placeholder.into());
        Self {
            id: id.into(),
            label: label.into(),
            description: None,
            locus,
            input,
            params,
            build,
        }
    }

    /// An action whose one argument is the content hash of bytes this
    /// machine already holds, rather than typed words — a separate
    /// constructor (not a parameter on [`Self::prompted`]) so every
    /// existing action keeps saying exactly what it said.
    pub fn prompted_content(
        id: impl Into<String>,
        label: impl Into<String>,
        prompt: impl Into<String>,
        placeholder: impl Into<String>,
        locus: ExecutionLocus,
        build: impl Fn(&str) -> EncodedAction + Send + Sync + 'static,
    ) -> Self {
        let mut spec = Self::prompted(id, label, prompt, placeholder, locus, build);
        if let ActionInput::Prompted { collects, .. } = &mut spec.input {
            *collects = Collects::Content;
        }
        spec
    }

    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// What an invocation of this action must carry. Empty for the nullary
    /// majority.
    ///
    /// Part of the descriptor rather than something a caller discovers by
    /// trying: enumeration is all a caller that cannot see the screen has to
    /// work from, so an action whose parameters were not advertised would be
    /// un-invokable except by guesswork.
    pub fn params(&self) -> &[ActionParam] {
        &self.params
    }

    /// Bind `supplied` against [`params`](Self::params) and build the action.
    ///
    /// Strict: a missing, unknown, wrong-typed or out-of-choice argument is
    /// refused here, and the caller never receives an [`EncodedAction`] to
    /// dispatch — so a bad call cannot become a partial mutation, because
    /// nothing was ever handed to the executor.
    pub fn build_args(
        &self,
        supplied: impl IntoIterator<Item = (String, ArgValue)>,
    ) -> Result<EncodedAction, ArgError> {
        let bound = ActionArgs::bind(&self.params, supplied)?;
        Ok((self.build)(&bound))
    }

    /// [`build_args`](Self::build_args) for arguments that arrived as JSON.
    pub fn build_json(
        &self,
        supplied: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<EncodedAction, ArgError> {
        let bound = ActionArgs::bind_json(&self.params, supplied)?;
        Ok((self.build)(&bound))
    }
}

/// Derive the palette-facing [`ActionInput`] for a [`typed`](ActionSpec::typed)
/// spec from its declared parameters. See that constructor's docs.
fn derive_input(params: &[ActionParam], build: &ArgsBuilder, placeholder: String) -> ActionInput {
    let Some(primary) = params
        .iter()
        .find(|p| p.required)
        .or_else(|| params.first())
    else {
        return ActionInput::Immediate(build(&ActionArgs::none()));
    };
    let name = primary.name.to_string();
    let ty = primary.ty;
    let params = params.to_vec();
    let build = build.clone();
    ActionInput::Prompted {
        prompt: primary.description.to_string(),
        placeholder,
        collects: Collects::Words,
        build: Arc::new(move |text: &str| {
            // Parse the one line the palette can collect into the primary
            // parameter's declared type. Unparseable text binds nothing rather
            // than coercing to a wrong value; the builder decides what absence
            // means, exactly as it does for an unsupplied optional.
            let parsed = match ty {
                ParamType::Text => Some(ArgValue::Text(text.to_string())),
                ParamType::Integer => text.trim().parse::<i64>().ok().map(ArgValue::Integer),
                ParamType::Number => text.trim().parse::<f64>().ok().map(ArgValue::Number),
                ParamType::Boolean => match text.trim() {
                    "true" | "yes" | "on" => Some(ArgValue::Boolean(true)),
                    "false" | "no" | "off" => Some(ArgValue::Boolean(false)),
                    _ => None,
                },
            };
            let supplied: Vec<(String, ArgValue)> =
                parsed.map(|v| vec![(name.clone(), v)]).unwrap_or_default();
            let bound = ActionArgs::bind(&params, supplied).unwrap_or_default();
            build(&bound)
        }),
    }
}

/// An app instance implements this to publish the resource tree it
/// currently has open. The host collects the focused instance's tree
/// each frame; `doc` lets a provider enumerate resources living *in* the
/// document (e.g. a book's annotations).
pub trait ResourceProvider {
    fn resources(&self, doc: &automerge::AutoCommit) -> Vec<Resource>;
}

/// Field separator inside a flattened composite key — a unit-separator
/// control char never appears in a resource or action id.
pub const KEY_SEP: char = '\u{1f}';

/// One (resource, action) pair produced by [`flatten`], addressable by a
/// composite [`key`](FlatAction::key) the palette uses as its row id.
pub struct FlatAction<'a> {
    /// `"<resource_id>\u{1f}<action_id>"` — the palette row id.
    pub key: String,
    pub resource_id: ResourceId,
    pub resource_kind: String,
    pub resource_label: String,
    pub action: &'a ActionSpec,
}

/// Depth-first flatten of a resource forest into every exported action,
/// parents before children — what the palette lists.
pub fn flatten(resources: &[Resource]) -> Vec<FlatAction<'_>> {
    let mut out = Vec::new();
    for r in resources {
        flatten_into(r, &mut out);
    }
    out
}

fn flatten_into<'a>(resource: &'a Resource, out: &mut Vec<FlatAction<'a>>) {
    for action in &resource.actions {
        out.push(FlatAction {
            key: format!("{}{}{}", resource.id.0, KEY_SEP, action.id),
            resource_id: resource.id.clone(),
            resource_kind: resource.kind.clone(),
            resource_label: resource.label.clone(),
            action,
        });
    }
    for child in &resource.children {
        flatten_into(child, out);
    }
}
