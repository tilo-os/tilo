//! **Adapters**: a legacy app's typed actions, declared by someone who is not
//! that app.
//!
//! Collaboration, automation and agency in Tilo rest on *typed actions*
//! (see the [crate docs](crate)). An app that predates them — a
//! scriptable desktop program behind D-Bus, a command-line tool, a mobile
//! app reached over an intent, a page behind its own API — has no
//! [`Resource`](crate::Resource) tree and no [`ActionSpec`]s to publish, so
//! everything built on actions (Shortcuts, skills, tasks, search, agents)
//! cannot reach it at all.
//!
//! An adapter closes that gap with **declaration only**: it names the app it
//! applies to, the actions that app could perform, and, for each action, the
//! ways of performing it ordered most precise first. It is an ordinary typed
//! value that is serialized like any other resource, so it can be written by
//! the app's author, by Tilo, by a person, or by a task that recorded what it
//! did — and shared between collaborators, reviewed and versioned like the
//! rest of their work.
//!
//! # Layered fallbacks
//!
//! A layer is *how* an action is performed, and they are ordered by how much
//! of the app they have to guess about ([`Layer::rank`]):
//!
//! 0. [`Layer::Interface`] — an interface the app already exposes: a D-Bus
//!    method, a CLI invocation, an automation call, an HTTP or MCP endpoint.
//!    Deterministic and cheap; what an author writes first.
//! 1. [`Layer::Files`] — the app's own file formats, as typed resources. The
//!    action becomes "change the resource", and Undo comes from the history
//!    the resource layer already keeps.
//! 2. [`Layer::Accessibility`] — semantic UI actions through the
//!    accessibility tree (AT-SPI, AccessKit, UI Automation). Works almost
//!    everywhere; fragile, so every step carries a check.
//! 3. [`Layer::Pixels`] — screenshots plus mouse and keyboard. Last resort,
//!    and the only layer available inside a sandbox that cannot reach the
//!    app's interfaces at all.
//!
//! Two more layers describe where an adapter *came from* rather than a
//! distinct way to act: [`Layer::Native`], an action the app itself declares,
//! which needs no adapter, and [`Layer::Invalid`], an adapter that fails its
//! own check. Neither is ever chosen.
//!
//! # Record, check, fall back, repair
//!
//! Every implementation declares at least one [`Check`] — what must be true
//! once it has run — because a layer that cannot say whether it worked
//! cannot be trusted ahead of the layer below it. [`ActionAdapter::plan`]
//! walks the layers in order and returns the first one that both matches what
//! the caller asked for and can be reached here and now; a layer whose check
//! failed after running is what pushes the retry to the next one. When no
//! layer applies, the caller is told why each was passed over rather than
//! simply "no".

use serde::{Deserialize, Serialize};

use crate::contract::{ActionContract, ActionKind};
use crate::params::ActionParam;
use crate::resource::ActionSpec;
use crate::{ActionError, EncodedAction, ExecutionLocus, OperationKind};

/// How an adapter performs one of an app's actions, most precise first.
///
/// The numeric order is the order adapters fall back in, and it is stable:
/// a layer is never inserted between two others, because a recording of a run
/// names the layer it used and must keep meaning the same thing.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Layer {
    /// The app itself declares this action; no adapter is involved. Never
    /// chosen by an adapter — it is the layer an adapter exists because
    /// nothing was at.
    Native,
    /// An interface the app already exposes (D-Bus, CLI, automation call,
    /// REST, MCP), wrapped by a small adapter.
    #[default]
    Interface,
    /// The app's file formats, understood as typed resources: the action is a
    /// change to the resource, and its undo is that change's.
    Files,
    /// Semantic UI actions through the accessibility tree.
    Accessibility,
    /// Screenshots plus mouse and keyboard.
    Pixels,
    /// An adapter that fails its own [`ActionAdapter::check`]. Last, so a
    /// broken declaration never outranks a working one.
    Invalid,
}

impl Layer {
    /// Every layer, most precise first.
    pub const ALL: [Self; 6] = [
        Self::Native,
        Self::Interface,
        Self::Files,
        Self::Accessibility,
        Self::Pixels,
        Self::Invalid,
    ];

    /// The layer as an adapter stores it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Interface => "interface",
            Self::Files => "files",
            Self::Accessibility => "accessibility",
            Self::Pixels => "pixels",
            Self::Invalid => "invalid",
        }
    }

    /// The layer `name` names; `None` for any other word, including the
    /// historical spellings this vocabulary does not use.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|layer| layer.name() == name)
    }

    /// Where this layer sits in the fallback order: lower is tried first.
    #[must_use]
    pub const fn rank(self) -> u8 {
        match self {
            Self::Native => 0,
            Self::Interface => 1,
            Self::Files => 2,
            Self::Accessibility => 3,
            Self::Pixels => 4,
            Self::Invalid => 5,
        }
    }

    /// Whether a layer performs the action at all, as opposed to describing
    /// the situation in which none works: [`Native`](Self::Native) performs it
    /// — the app itself does — while [`Invalid`](Self::Invalid) knows of no
    /// way and so is planned nowhere.
    #[must_use]
    pub const fn performs(self) -> bool {
        matches!(
            self,
            Self::Native | Self::Interface | Self::Files | Self::Accessibility | Self::Pixels
        )
    }

    /// Whether the layer acts by driving the app's UI, and so needs the app
    /// on screen: it is the only place a check can establish that the app is
    /// still in the state the recorded steps assume.
    #[must_use]
    pub const fn drives_ui(self) -> bool {
        matches!(self, Self::Accessibility | Self::Pixels)
    }
}

/// What must be true once a step or an implementation has run.
///
/// A check is deliberately small and comparable: it is a *testable claim*
/// ("the save dialog is open", "the file exists and is 12 480 bytes") that
/// both a deterministic replay and an outside observer can evaluate the same
/// way. A check that nothing can evaluate is not a check, so
/// [`Implementation::check`] refuses an implementation whose checks are
/// empty.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Check {
    /// The addressed node of the accessibility tree exists, with this role
    /// and name, when the step finishes.
    Element {
        /// The accessibility role (`push button`, `text field`, `dialog`).
        role: String,
        /// The node's accessible name, as the tree reports it.
        name: String,
    },
    /// The resource at `address` (a `tilo://` resource address) has the
    /// content hash `content`, after the step as before it.
    Resource {
        /// Stable address of the resource the action acts on.
        address: String,
        /// The `blake3` hash of the content it must hold, as hex.
        content: String,
    },
    /// The file at `path` exists with exactly `bytes` bytes. A size pins the
    /// check to a concrete outcome without pinning it to a timestamp.
    File {
        /// Absolute path of the file, in the space the adapter runs in.
        path: String,
        /// Its expected size, in bytes.
        bytes: u64,
    },
    /// The command `program` exits successfully. For a read this is the
    /// interface answering; for a write, the tool reporting what it did.
    Command {
        /// The program to run, exactly as the adapter spells it.
        program: String,
        /// Its arguments, in order.
        args: Vec<String>,
    },
}

impl Check {
    /// Whether the check says anything at all: a claim with a blank part
    /// cannot be tested, so it is refused where it is declared.
    #[must_use]
    pub fn is_testable(&self) -> bool {
        match self {
            Self::Element { role, name } => !role.trim().is_empty() && !name.trim().is_empty(),
            Self::Resource { address, content } => {
                !address.trim().is_empty() && !content.trim().is_empty()
            }
            Self::File { path, .. } => path.starts_with('/') && path.len() > 1,
            Self::Command { program, .. } => !program.trim().is_empty(),
        }
    }
}

/// What Undo means for an action an adapter performs, which is the one fact
/// about a legacy app a caller cannot infer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Undo {
    /// The app can take it back itself.
    Native,
    /// Tilo snapshots the affected resources before the action and Undo
    /// restores them: "undo by restore". The contract keeps its kind (a
    /// change Undo takes back is still undoable) and its compensation is the
    /// restore.
    #[default]
    Restore,
    /// Nothing can take it back, and nothing was snapshot: an action that
    /// counts as external and must be approved before it runs. It is marked
    /// [`IRREVERSIBLE_TAG`](crate::IRREVERSIBLE_TAG).
    None,
}

impl Undo {
    /// The undo method as the adapter stores it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Restore => "restore",
            Self::None => "none",
        }
    }

    /// The undo method `name` names; `None` for any other word.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        [Self::Native, Self::Restore, Self::None]
            .into_iter()
            .find(|undo| undo.name() == name)
    }
}

/// One action of a legacy app, declared, with the ways of performing it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdapterAction {
    /// The action's id, unique within its adapter (`"export-pdf"`).
    pub id: String,
    /// What the action does, as a person reads it ("Export as PDF").
    pub label: String,
    /// The other words people use for it; search finds it by them too.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<String>,
    /// The action's typed argument list, in the same vocabulary an app
    /// declares its own (see [`crate::params`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub params: Vec<ActionParam>,
    /// Whether the action reads, changes only a resource, or reaches outside
    /// it — the contract the grants deciding it are drawn from.
    #[serde(default)]
    pub contract: ActionContract,
    /// What Undo means for this action.
    #[serde(default)]
    pub undo: Undo,
    /// The ways of performing it, any order as written; [`ranked`] sorts them
    /// into the fallback order an invocation walks.
    ///
    /// [`ranked`]: AdapterAction::ranked
    pub implementations: Vec<Implementation>,
}

impl AdapterAction {
    /// The implementations that perform the action, most precise first. Two
    /// implementations of one layer keep the order they were written in, so
    /// an author's preference within a layer is respected; [`Layer::Native`]
    /// and [`Layer::Invalid`] are never among them.
    #[must_use]
    pub fn ranked(&self) -> Vec<&Implementation> {
        let mut layers: Vec<&Implementation> = self
            .implementations
            .iter()
            .filter(|implementation| {
                implementation.layer.performs() && implementation.check().is_empty()
            })
            .collect();
        layers.sort_by_key(|implementation| implementation.layer.rank());
        layers
    }

    /// The implementation [`AdapterAction::ranked`] puts first that can be
    /// reached here and now, and why each of those ahead of it was passed
    /// over.
    ///
    /// `reachable` answers "can this layer be reached in the circumstances of
    /// this invocation at all" — a D-Bus name that is not on the session bus,
    /// a sandbox with no path to the app's interfaces — not "did it work":
    /// whether it worked is the check, evaluated after it runs.
    #[must_use]
    pub fn plan(
        &self,
        reachable: impl Fn(Layer) -> bool,
    ) -> (Option<&Implementation>, Vec<Skipped>) {
        let mut skipped = Vec::new();
        for implementation in self.ranked() {
            if reachable(implementation.layer) {
                return (Some(implementation), skipped);
            }
            skipped.push(Skipped {
                layer: implementation.layer,
                reason: format!(
                    "the {} layer cannot be reached here",
                    implementation.layer.name()
                ),
            });
        }
        if self.implementations.is_empty() {
            skipped.push(Skipped {
                layer: Layer::Invalid,
                reason: "the action declares no implementation".into(),
            });
        }
        (None, skipped)
    }

    /// The action as a typed [`ActionSpec`], so a Shortcut, a skill, a task
    /// and an agent's catalogue see it exactly as they see an app's own.
    ///
    /// The spec's parameters are the declared ones, and the action it builds
    /// is [`AdapterInvocation`] — the adapter, the action and the bound
    /// arguments — which an executor performs through the layer it plans.
    #[must_use]
    pub fn spec(&self, adapter: &ActionAdapter) -> ActionSpec {
        let params = self.params.clone();
        let locus = ExecutionLocus::ClientLocal;
        let effect =
            self.contract.kind == ActionKind::Outside || self.contract.kind == ActionKind::Read;
        let build = {
            let app = adapter.app.clone();
            let action = self.id.clone();
            let listed = params.clone();
            move |args: &crate::ActionArgs| {
                // Every bound argument travels; one the declaration does not
                // name was already refused by the bind before this runs.
                let mut arguments = args.to_json();
                arguments.retain(|name, _| listed.iter().any(|param| param.name == *name));
                AdapterInvocation::new(app.clone(), action.clone(), arguments).encode()
            }
        };
        let mut spec = if effect {
            ActionSpec::typed_operation(
                self.id.clone(),
                self.label.clone(),
                self.label.clone(),
                OperationKind::Effect,
                ADAPTER_OPERATION_TYPE,
                params,
                build,
            )
        } else {
            ActionSpec::typed(
                self.id.clone(),
                self.label.clone(),
                self.label.clone(),
                locus,
                params,
                build,
            )
        };
        let ty = spec.type_mut();
        ty.keywords.clone_from(&self.keywords);
        ty.contract = self.contract.clone();
        // Undo is the one fact about a legacy app a caller cannot infer, and
        // it is stated in the vocabulary every gate already reads: an effect
        // Tilo snapshots is one Undo takes back (the restore is its
        // compensation), and one nothing can take back says so.
        match self.undo {
            Undo::Restore => {
                if ty.contract.kind == ActionKind::Outside {
                    ty.contract
                        .tags
                        .push(crate::COMPENSATED_UNDO_TAG.to_owned());
                }
            }
            Undo::None => ty.contract.tags.push(crate::IRREVERSIBLE_TAG.to_owned()),
            Undo::Native => {}
        }
        spec
    }

    /// Why this action could never be performed as declared, if it could not.
    /// Usable at both ends of publication: an adapter's author checks before
    /// sharing one, and the reader checks again on arrival.
    #[must_use]
    pub fn check(&self) -> Vec<String> {
        let mut errors = Vec::new();
        if self.id.trim().is_empty() {
            errors.push("an adapter action needs an id".into());
        }
        if !self.id.trim().is_empty()
            && self
                .id
                .bytes()
                .any(|byte| byte.is_ascii_whitespace() || byte == b'/' || byte == b'#')
        {
            errors.push(format!(
                "adapter action id `{}` must not contain whitespace, `/` or `#`: it is addressed by it",
                self.id
            ));
        }
        if self.label.trim().is_empty() {
            errors.push(format!("adapter action `{}` needs a label", self.id));
        }
        if self.undo == Undo::None && self.contract.can_be_undone() {
            errors.push(format!(
                "adapter action `{}` says it cannot be undone but its contract says Undo takes it back",
                self.id
            ));
        }
        if self.undo != Undo::None
            && !self.contract.can_be_undone()
            && !self
                .contract
                .tags
                .iter()
                .any(|tag| tag == crate::IRREVERSIBLE_TAG)
        {
            errors.push(format!(
                "adapter action `{}` declares undo `{}` but its contract cannot be undone",
                self.id,
                self.undo.name()
            ));
        }
        for param in &self.params {
            for error in param.validate_declaration() {
                errors.push(format!("adapter action `{}`: {error}", self.id));
            }
        }
        for error in self.contract.validate_declaration() {
            errors.push(format!("adapter action `{}`: {error}", self.id));
        }
        if self.implementations.is_empty() {
            errors.push(format!(
                "adapter action `{}` declares no implementation",
                self.id
            ));
        }
        for implementation in &self.implementations {
            for error in implementation.check() {
                errors.push(format!("adapter action `{}`: {error}", self.id));
            }
        }
        errors
    }
}

/// One way of performing an action: on which layer, by what steps, checked
/// how.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Implementation {
    /// Which layer performs it.
    pub layer: Layer,
    /// The steps, in order: a D-Bus call, a command line, a resource edit, an
    /// accessibility click, a key sequence.
    pub steps: Vec<Step>,
    /// What must be true once the steps have run; at least one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<Check>,
    /// The app version this implementation was written against, so a replay
    /// can say which recorded run it replays and a repair can say what
    /// changed. Free text (`"24.2"`, `"any"`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub app_version: String,
    /// Where it came from, when it was recorded rather than written by
    /// hand: the id of the run that produced it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recorded_from: Option<String>,
}

impl Implementation {
    /// Why this implementation could never run as written, if it could not.
    #[must_use]
    pub fn check(&self) -> Vec<String> {
        let mut errors = Vec::new();
        if !self.layer.performs() {
            errors.push(format!(
                "an implementation on the {} layer performs nothing",
                self.layer.name()
            ));
        }
        if self.steps.is_empty() {
            errors.push(format!(
                "the {} implementation has no steps",
                self.layer.name()
            ));
        }
        if self.checks.is_empty() {
            errors.push(format!(
                "the {} implementation has no check, so nothing establishes that it worked",
                self.layer.name()
            ));
        }
        for step in &self.steps {
            for error in step.check() {
                errors.push(format!("{} implementation: {error}", self.layer.name()));
            }
        }
        for check in &self.checks {
            if !check.is_testable() {
                errors.push(format!(
                    "the {} implementation has a check that nothing can evaluate",
                    self.layer.name()
                ));
            }
        }
        errors
    }

    /// Whether the checks are satisfied by `observed`: the state the layer
    /// reports once it has run, keyed the way each check names what it reads.
    /// A check with nothing observed is not satisfied — silence is not
    /// success.
    #[must_use]
    pub fn satisfied_by(&self, observed: &Observation) -> bool {
        !self.checks.is_empty() && self.checks.iter().all(|check| observed.holds(check))
    }
}

/// What one layer reports after running, so its checks can be evaluated.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Observation {
    /// Accessibility nodes seen, as (role, name).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub elements: Vec<(String, String)>,
    /// Resources read, as (address, content hash).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resources: Vec<(String, String)>,
    /// Files read, as (path, size in bytes).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<(String, u64)>,
    /// Commands that were run, as (program, succeeded).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<(String, bool)>,
}

impl Observation {
    fn holds(&self, check: &Check) -> bool {
        match check {
            Check::Element { role, name } => self
                .elements
                .iter()
                .any(|(seen_role, seen_name)| seen_role == role && seen_name == name),
            Check::Resource { address, content } => self
                .resources
                .iter()
                .any(|(seen, hash)| seen == address && hash == content),
            Check::File { path, bytes } => self
                .files
                .iter()
                .any(|(seen, size)| seen == path && size == bytes),
            Check::Command { program, .. } => self
                .commands
                .iter()
                .any(|(seen, ok)| seen == program && *ok),
        }
    }
}

/// One step of an implementation, in the vocabulary of the layer it runs on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum Step {
    /// Call a D-Bus method, or a method of another bus-speaking interface.
    Call {
        /// The bus name (`org.example.App`).
        bus: String,
        /// The object path (`/org/example/App`).
        path: String,
        /// The interface (`org.example.App.Documents`).
        interface: String,
        /// The method (`Export`).
        method: String,
        /// Its arguments, in the JSON form a parameter's value takes.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        arguments: Vec<serde_json::Value>,
    },
    /// Run a program with arguments, each part of which may be a `{{param}}`
    /// or `{{steps.0.content}}` reference to a parameter or an earlier
    /// step's result.
    Run {
        /// The program to run.
        program: String,
        /// Its arguments, in order.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        args: Vec<String>,
        /// The standard input to give it, when a step pipes one step into
        /// the next.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stdin: Option<String>,
    },
    /// Ask an HTTP endpoint, for an app that is reached through a REST API.
    Request {
        /// The method (`POST`).
        method: String,
        /// The absolute URL.
        url: String,
        /// The request body, when the method takes one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        body: Option<serde_json::Value>,
    },
    /// Call a tool on an MCP server the workspace already knows.
    Tool {
        /// The server's id, as the workspace's connectors name it.
        server: String,
        /// The tool's name.
        tool: String,
        /// The tool's arguments.
        #[serde(default)]
        arguments: serde_json::Map<String, serde_json::Value>,
    },
    /// Change a typed resource: the action *is* this change, and its undo is
    /// this change's.
    Edit {
        /// Stable address of the resource.
        address: String,
        /// The declarative edit, in the resource layer's vocabulary.
        edit: serde_json::Value,
    },
    /// Invoke an element of the accessibility tree, addressed by role and
    /// accessible name — never by position, which changes with the window.
    Invoke {
        /// The node's role.
        role: String,
        /// The node's accessible name.
        name: String,
        /// What to do with it (`press`, `set-value`, `focus`).
        action: String,
        /// The value to set, for `set-value`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<String>,
    },
    /// Send a key sequence to the focused window.
    Press {
        /// The keys, in order (`["ctrl", "s"]`).
        keys: Vec<String>,
    },
    /// Click at a pixel position, relative to the window's origin.
    Click {
        /// Horizontal offset.
        x: i32,
        /// Vertical offset.
        y: i32,
    },
    /// Type text into the focused window.
    Type {
        /// The text to type; may reference a parameter.
        text: String,
    },
}

impl Step {
    /// The layer a step can only run on, when it is specific to one, so an
    /// implementation's declared layer can be checked against its steps.
    #[must_use]
    pub fn required_layer(&self) -> Option<Layer> {
        match self {
            Self::Call { .. } | Self::Run { .. } | Self::Request { .. } | Self::Tool { .. } => {
                Some(Layer::Interface)
            }
            Self::Edit { .. } => Some(Layer::Files),
            Self::Invoke { .. } => Some(Layer::Accessibility),
            Self::Press { .. } | Self::Click { .. } | Self::Type { .. } => Some(Layer::Pixels),
        }
    }

    /// Why this step could never run as written, if it could not.
    #[must_use]
    pub fn check(&self) -> Vec<String> {
        match self {
            Self::Call {
                bus,
                path,
                interface,
                method,
                ..
            } => [
                ("bus", bus.as_str()),
                ("path", path.as_str()),
                ("interface", interface.as_str()),
                ("method", method.as_str()),
            ]
            .into_iter()
            .filter(|(_, part)| part.trim().is_empty())
            .map(|(name, _)| format!("a call step needs its {name}"))
            .collect(),
            Self::Run { program, .. } if program.trim().is_empty() => {
                vec!["a run step needs a program".into()]
            }
            Self::Request { method, url, .. } => {
                let mut errors = Vec::new();
                if method.trim().is_empty() {
                    errors.push("a request step needs a method".into());
                }
                if !(url.starts_with("http://") || url.starts_with("https://")) {
                    errors.push("a request step needs an http or https URL".into());
                }
                errors
            }
            Self::Tool { server, tool, .. } => {
                let mut errors = Vec::new();
                if server.trim().is_empty() {
                    errors.push("a tool step needs a server".into());
                }
                if tool.trim().is_empty() {
                    errors.push("a tool step needs a tool".into());
                }
                errors
            }
            Self::Edit { address, .. } => {
                if address.trim().is_empty() {
                    vec!["an edit step needs the address of the resource it changes".into()]
                } else {
                    Vec::new()
                }
            }
            Self::Invoke {
                role, name, action, ..
            } => {
                let mut errors = Vec::new();
                if role.trim().is_empty() || name.trim().is_empty() {
                    errors.push("an invoke step addresses a node by role and name".into());
                }
                if action.trim().is_empty() {
                    errors.push("an invoke step names the action to perform".into());
                }
                errors
            }
            Self::Press { keys } => {
                if keys.is_empty() {
                    vec!["a press step needs at least one key".into()]
                } else {
                    Vec::new()
                }
            }
            Self::Type { text } => {
                if text.is_empty() {
                    vec!["a type step needs some text".into()]
                } else {
                    Vec::new()
                }
            }
            Self::Run { .. } | Self::Click { .. } => Vec::new(),
        }
    }
}

/// A layer that was passed over, and why.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Skipped {
    /// The layer that was passed over.
    pub layer: Layer,
    /// Why it was.
    pub reason: String,
}

/// An adapter: a legacy app's typed actions, and how to perform each.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionAdapter {
    /// The app the adapter applies to: its Flatpak app id, its package name or
    /// its desktop id (`org.example.LegacyEditor`).
    pub app: String,
    /// The versions of that app the adapter applies to. Free text, because
    /// apps version themselves in their own ways (`"24.*"`, `"any"`).
    #[serde(default = "any_version")]
    pub app_version: String,
    /// Which platform's conventions the layers follow (`linux`, `web`,
    /// `android`, `windows`). An adapter that names a platform is only
    /// planned on that platform.
    pub platform: String,
    /// What the adapter is for, in one sentence.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// The actions it declares.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<AdapterAction>,
}

fn any_version() -> String {
    "any".into()
}

/// Whether a version `pattern` an adapter names accepts the app `version` the
/// caller is driving.
///
/// A pattern is `any`, one version (`24.2`), a prefix of one ended by `.`
/// (`24.`, for every `24.x`), a major wildcard (`24.*`, the same), or a
/// comma-separated list of those (`24.*, 25.0`). Deliberately small and
/// total: an adapter says which app it was written for, and a pattern it
/// cannot read matches nothing, because replaying a recording against the
/// wrong version of an app is exactly what the field exists to prevent.
#[must_use]
pub fn version_matches(pattern: &str, version: &str) -> bool {
    let version = version.trim();
    pattern.split(',').map(str::trim).any(|pattern| {
        if pattern == "any" {
            return true;
        }
        if let Some(prefix) = pattern.strip_suffix('*') {
            return !prefix.is_empty() && version.starts_with(prefix);
        }
        if let Some(prefix) = pattern.strip_suffix('.') {
            return version
                .strip_prefix(prefix)
                .is_some_and(|rest| !rest.is_empty());
        }
        !pattern.is_empty() && pattern == version
    })
}

impl ActionAdapter {
    /// An adapter for `app` on `platform` declaring no actions yet.
    #[must_use]
    pub fn new(app: impl Into<String>, platform: impl Into<String>) -> Self {
        Self {
            app: app.into(),
            app_version: any_version(),
            platform: platform.into(),
            description: String::new(),
            actions: Vec::new(),
        }
    }

    /// The action `id`, if this adapter declares it.
    #[must_use]
    pub fn action(&self, id: &str) -> Option<&AdapterAction> {
        self.actions.iter().find(|action| action.id == id)
    }

    /// Whether this adapter applies to a run on `platform` of app version
    /// `version`: the adapter's platform must match, and the versions it
    /// names — `any`, or one pattern or a comma-separated list of them — must
    /// accept it (see [`version_matches`]).
    #[must_use]
    pub fn applies_to(&self, platform: &str, version: &str) -> bool {
        self.platform == platform && version_matches(&self.app_version, version)
    }

    /// Every action as a typed [`ActionSpec`], in declaration order, so the
    /// adapter's actions join a palette, a search provider, a Shortcut and an
    /// agent's catalogue by the same door an app's own do.
    #[must_use]
    pub fn specs(&self) -> Vec<ActionSpec> {
        self.actions
            .iter()
            .map(|action| action.spec(self))
            .collect()
    }

    /// Why this adapter could never be used as declared, if it could not.
    ///
    /// An adapter that fails this is not rejected outright: it is planned on
    /// the [`Layer::Invalid`] layer, which outranks nothing, so a broken
    /// declaration costs nobody a working path — but the errors say exactly
    /// what to fix, which is what a repair writes back.
    #[must_use]
    pub fn check(&self) -> Vec<String> {
        let mut errors = Vec::new();
        if self.app.trim().is_empty() {
            errors.push("an adapter names the app it applies to".into());
        }
        if self.platform.trim().is_empty() {
            errors.push(format!("adapter `{}` names its platform", self.app));
        }
        if self.app_version.trim().is_empty() {
            errors.push(format!(
                "adapter `{}` names the app versions it applies to (or `any`)",
                self.app
            ));
        }
        if self.actions.is_empty() {
            errors.push(format!("adapter `{}` declares no actions", self.app));
        }
        let mut seen: Vec<&str> = Vec::new();
        for action in &self.actions {
            if seen.contains(&action.id.as_str()) {
                errors.push(format!(
                    "adapter `{}` declares action `{}` twice",
                    self.app, action.id
                ));
            }
            seen.push(&action.id);
            for error in action.check() {
                errors.push(format!("adapter `{}`: {error}", self.app));
            }
        }
        errors
    }

    /// The layer this adapter can be trusted to act on, given the result of
    /// [`ActionAdapter::check`]: [`Layer::Interface`] (its best usable layer)
    /// when it is sound, [`Layer::Invalid`] when it is not.
    #[must_use]
    pub fn usable_layer(&self) -> Layer {
        if self.check().is_empty() {
            Layer::Interface
        } else {
            Layer::Invalid
        }
    }
}

/// The operation type an adapter action is routed as: one protocol type for
/// every adapter, which the executor hands to the adapter that declares the
/// action.
pub const ADAPTER_OPERATION_TYPE: &str = "tilo.adapter.invoke";

/// One invocation of an adapter's action with its arguments bound: the value
/// an adapter's [`ActionSpec`] builds, and what an executor performs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdapterInvocation {
    /// The app whose adapter declares the action.
    pub app: String,
    /// The action's id in that adapter.
    pub action: String,
    /// The bound arguments, by parameter name.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub arguments: serde_json::Map<String, serde_json::Value>,
}

impl AdapterInvocation {
    /// The invocation of `action` on `app` with `arguments`.
    #[must_use]
    pub fn new(
        app: impl Into<String>,
        action: impl Into<String>,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Self {
        Self {
            app: app.into(),
            action: action.into(),
            arguments,
        }
    }

    /// This invocation as the encoded operation an executor dispatches. An
    /// adapter drives an app, so it is an external effect, never a document
    /// change replayed from a peer's history.
    #[must_use]
    pub fn encode(&self) -> EncodedAction {
        // Serialising this struct cannot fail: it holds a string, a string and
        // a JSON object, all of which are already serialisable values.
        EncodedAction::encode_operation(ADAPTER_OPERATION_TYPE, self)
            .unwrap_or_else(|_| EncodedAction::written(ADAPTER_OPERATION_TYPE.into(), Vec::new()))
    }

    /// The invocation an encoded operation carries, when it is one.
    ///
    /// # Errors
    /// When `operation` is not an adapter invocation at all — a different
    /// operation type, or a payload that does not decode.
    pub fn from_encoded(operation: &EncodedAction) -> Result<Self, ActionError> {
        if operation.type_id != ADAPTER_OPERATION_TYPE {
            return Err(ActionError::Refused(format!(
                "`{}` is not an adapter invocation",
                operation.type_id
            )));
        }
        serde_json::from_slice(&operation.payload).map_err(|error| ActionError::Decode {
            type_id: operation.type_id.clone(),
            source: error,
        })
    }

    /// The declared parameters a caller left out, which the layer would
    /// otherwise interpolate as nothing. This runs before any layer is
    /// reached, next to the bind a typed [`ActionSpec`] already performs, so a
    /// caller and a replay refuse the same arguments.
    pub fn missing_arguments(&self, declared: &[ActionParam]) -> Vec<String> {
        declared
            .iter()
            .filter(|param| param.required && !self.arguments.contains_key(param.name.as_ref()))
            .map(|param| format!("`{}` is required", param.name))
            .collect()
    }
}
