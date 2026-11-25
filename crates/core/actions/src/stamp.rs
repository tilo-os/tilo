//! What a resource store's change names in its message: the action that made it, which every
//! holder judges the change against, and which history and undo group it by.

use serde::{Deserialize, Serialize};

/// The action a change was made by, and the membership its author saw: what a holder judges
/// the change against. It travels in the change itself.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stamp {
    /// The verb invoked.
    pub verb: String,
    /// Who invoked it: a person, or an agent acting for one, as [`Stamp::by`] writes the actor.
    /// History names the change's author by it, so it is the actor every reader parses back
    /// ([`Stamp::invoked_by`]), never a bare id.
    pub invoker: String,
    /// The person the invoker acted for, when someone acted for a person and the invoker is not
    /// one (an agent): the records it makes are that person's, so a record under
    /// `readers: author` is read by them and their devices, as one made by them directly is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acted_for: Option<String>,
    /// The invocation: every change it made, in every document, names it, so history and undo
    /// take them as one step.
    pub invocation: String,
    /// The heads of the membership the author saw when it wrote the change, as the membership
    /// names them: who may do what is judged as of there, not as of the receiver's now.
    pub membership: Vec<String>,
    /// The invocation this one takes back, when it is an undo: it names that invocation's verb.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub undoes: Option<String>,
    /// When it was invoked, ISO-8601 UTC, recorded like an argument so every holder reads the
    /// same; empty where the invoker named no time.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub at: String,
    /// The resource the invocation was made from, when it was made from one (a task made from a
    /// message): what the resources it made read as made from, on every holder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<Origin>,
    /// The work the invocation is one step of, when it is one of several taken together (an
    /// agent's session): what a review of that work lists, and what reading its changes groups.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<Group>,
    /// Who proposed the invocation, when the invoker accepted someone's suggestion: the change
    /// is the invoker's, and it says who proposed it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub suggested_by: String,
    /// The arguments the action was invoked with, in the order it declares them, as the runtime
    /// recorded them: what a program following the change reads as the verb's arguments. Empty
    /// where the change names no arguments of its own (an edit, an undo).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<serde_json::Value>,
    /// When the writer wrote it, by its clock, as a number that orders: the order a resource's
    /// versions read in across the documents it is kept in, and what tells two writes of one
    /// invocation apart. Zero where the writer kept none.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub moment: u64,
    /// The invocation as its caller signed it, when a host ran the action for someone who asked
    /// it under their own key: the device that wrote the change signs it as its own, and this
    /// says who asked, for what, with what arguments. A holder checks it against the person's
    /// root ([`crate::remote::SignedCall`]); the host never makes one up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signed: Option<Box<crate::remote::SignedCall>>,
    /// The device that wrote the change, its key in hex: where a resource the change made live
    /// is live (`tilo_resource_store::Store::live`). Empty where the writer named none.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub device: String,
    /// What that device calls itself ("Framework", "Phone"), as its certificate names it: a row
    /// live on it says where it plays. Empty where the writer named none.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub device_name: String,
    /// What the change did, as history tells it: the item it addressed, the field it edited, the
    /// gesture it is one keystroke of, the purpose its invoker gave. `None` where the writer said
    /// no more than the verb.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<Box<crate::ActionContext>>,
}

#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde passes the field by reference"
)]
const fn is_zero(moment: &u64) -> bool {
    *moment == 0
}

/// A resource an invocation was made from: its kind and id.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Origin {
    pub kind: String,
    pub id: String,
}

/// Several invocations taken together, as one piece of work: its id, and what it was called
/// when it ran.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Group {
    pub id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
}

/// The form a change's label was written in ([`Stamp::read`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Label {
    /// An action's entry, written whole.
    Entry,
    /// One keystroke of a gesture, written as what changed since its writer's last whole entry.
    Continuation,
    /// A change the framework wrote rather than an app's action.
    Framework(crate::FrameworkChange),
    /// A resource store's stamp.
    Stamp,
}

impl Label {
    /// The form `message` was written in, told by how it starts, without reading what it says:
    /// what a reader that only sorts changes by form asks. `None` for a message in no form a
    /// label is written in; a stamp's is not checked here ([`Stamp::read`] reads it).
    #[must_use]
    pub fn of(message: &str) -> Option<Self> {
        if message.starts_with(crate::context::CHANGE_MESSAGE_PREFIX) {
            return Some(Self::Entry);
        }
        if message.starts_with(crate::context::CONTINUATION_PREFIX) {
            return Some(Self::Continuation);
        }
        if let Some(change) = crate::FrameworkChange::from_change_message(message) {
            return Some(Self::Framework(change));
        }
        message.starts_with('{').then_some(Self::Stamp)
    }
}

/// The verb a change whose entry says it did `what` names: its action's, the content edit
/// ([`Stamp::TYPE_VERB`]) for an edit or a change that names nothing finer, the one verb every
/// member of a group shares, and a history step's. Empty where no single declared verb names it.
fn verb_of(what: Option<&crate::Did>) -> String {
    use crate::{Did, HistoryOp};
    match what {
        None | Some(Did::Edit(_)) => Stamp::TYPE_VERB.to_owned(),
        Some(Did::Verb(verb)) => verb.action.action.clone(),
        Some(Did::Group(group)) => {
            let members = if group.distinct.is_empty() {
                &group.members
            } else {
                &group.distinct
            };
            let mut verbs = members.iter().map(|member| verb_of(Some(member)));
            let first = verbs.next().unwrap_or_default();
            if verbs.all(|verb| verb == first) {
                first
            } else {
                String::new()
            }
        }
        Some(Did::History(step)) => match step.op {
            HistoryOp::Checkpoint { .. } => Stamp::CHECKPOINT_VERB.to_owned(),
            HistoryOp::Migrate { .. } => Stamp::MIGRATE_VERB.to_owned(),
            HistoryOp::Undo | HistoryOp::Redo | HistoryOp::Restore => String::new(),
        },
    }
}

impl Stamp {
    /// The verb of a change an app's outside origin wrote (`$origin`): what its service said,
    /// taken into the resources of the types it keeps. A store keeps no facets, so a change
    /// stamped with it is how a received message reaches a trigger.
    pub const ORIGIN_VERB: &str = "$origin";

    /// The verb of a change that names the version it stands at, with the name as its one
    /// argument: a change with no operations every schema declares.
    pub const CHECKPOINT_VERB: &str = "checkpoint";

    /// The text a stamp names `actor` by as its invoker.
    #[must_use]
    pub fn by(actor: &tilo_action_policy::Actor) -> String {
        serde_json::to_string(actor).unwrap_or_default()
    }

    /// The message a change carries it as.
    #[must_use]
    pub fn message(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// The verb of a change that edits a record's content with no finer action naming it (a
    /// window's typing, a group of field edits): each kind whose content is written so declares
    /// it on its surface.
    pub const TYPE_VERB: &str = "type";

    /// The verb of a change that brings a record to a storage version of its model: the step its
    /// kind and id derive, or the app's rewrite that follows it. Every store has it, and no
    /// surface declares it.
    pub const MIGRATE_VERB: &str = "migrate";

    /// The verb of a change that founds a record in a branch of its workspace: the fork's
    /// founding step on the history the branch copies, under the verb every store has and no app
    /// declares.
    pub const BRANCH_VERB: &str = "branch";

    /// The verb of a change that takes back a step (`$undo`), and of one that brings a version
    /// back (`$restore`): the platform's own writes through history, under verbs whose `$` no
    /// app declares.
    pub const UNDO_VERB: &str = "$undo";
    pub const RESTORE_VERB: &str = "$restore";

    /// The verbs the platform writes under, which no app declares: a call a device carries out
    /// (`$invocation`), an agent's suggestion and the decision on it (`$suggest`,
    /// `$decide-invocation`), a block that settles one (`$settle-block`), and a step taken back or
    /// brought back ([`Self::UNDO_VERB`], [`Self::RESTORE_VERB`]). A store's schema declares them,
    /// and under them a change may write any field or content of a declared kind, so a surface an
    /// app ships may declare none of them.
    pub const PLATFORM_VERBS: [&str; 6] = [
        "$invocation",
        "$suggest",
        "$decide-invocation",
        "$settle-block",
        Self::UNDO_VERB,
        Self::RESTORE_VERB,
    ];

    /// The verbs no surface an app ships may declare, whether or not a schema admits them yet:
    /// the platform's own ([`Self::PLATFORM_VERBS`]), which take a step back or bring a version
    /// back too ([`Self::UNDO_VERB`], [`Self::RESTORE_VERB`]), and the migration
    /// ([`Self::MIGRATE_VERB`]), which a store admits by its derived step or by who may write the
    /// record, never by an app's write set. The `$` names are the platform's, so an app's own
    /// `undo` or `restore` action is a different verb, judged by its own surface's write set.
    pub const RESERVED_VERBS: [&str; 7] = [
        "$invocation",
        "$suggest",
        "$decide-invocation",
        "$settle-block",
        Self::UNDO_VERB,
        Self::RESTORE_VERB,
        Self::MIGRATE_VERB,
    ];

    /// The stamp a change's message `message` names; `None` for one that names none.
    #[must_use]
    pub fn of_message(message: &str) -> Option<Self> {
        Self::read(message).map(|(_, stamp)| stamp)
    }

    /// The one reader of a change's label: what `message` names, in whichever form it was
    /// written ([`Label`]), as a stamp. An action's entry names its context, and its verb is the
    /// action's; a framework change and a continuation name their verb alone. A verb no
    /// declared one could name (a group of several actions, a step through history) reads as
    /// empty, which no surface declares.
    #[must_use]
    pub fn read(message: &str) -> Option<(Label, Self)> {
        if let Some(context) = crate::context::entry_of_message(message) {
            return Some((Label::Entry, Self::of_context(context)));
        }
        Self::read_unentered(message)
    }

    /// [`Self::read`], of an entry only what it says of who made the change and in which step
    /// ([`crate::ActionContext::head_of_change_message`]).
    #[must_use]
    pub fn read_head(message: &str) -> Option<(Label, Self)> {
        if let Some(context) = crate::context::entry_head_of_message(message) {
            return Some((Label::Entry, Self::of_context(context)));
        }
        Self::read_unentered(message)
    }

    /// What a message that carries no whole entry names.
    fn read_unentered(message: &str) -> Option<(Label, Self)> {
        use crate::context::ContinuedAction;
        if let Some(continued) = ContinuedAction::of_message(message) {
            let verb = match continued {
                ContinuedAction::Edit { .. } => Self::TYPE_VERB.to_owned(),
                ContinuedAction::Verb { action, .. } => action.action,
            };
            let invoker = ContinuedAction::invoker_of_message(message);
            return Some((
                Label::Continuation,
                Self {
                    verb,
                    invoker: invoker
                        .as_ref()
                        .map(|acting| Self::by(&acting.by))
                        .unwrap_or_default(),
                    ..Self::default()
                },
            ));
        }
        if let Some(change) = crate::FrameworkChange::from_change_message(message) {
            let verb = match change {
                crate::FrameworkChange::Native => Self::TYPE_VERB,
                crate::FrameworkChange::Run => Self::CHECKPOINT_VERB,
                crate::FrameworkChange::Migrate => Self::MIGRATE_VERB,
                crate::FrameworkChange::Genesis
                | crate::FrameworkChange::Structure
                | crate::FrameworkChange::Transfer
                | crate::FrameworkChange::Upgrade => "",
            };
            return Some((
                Label::Framework(change),
                Self {
                    verb: verb.to_owned(),
                    ..Self::default()
                },
            ));
        }
        serde_json::from_str(message)
            .ok()
            .map(|stamp| (Label::Stamp, stamp))
    }

    /// The stamp an action's entry `context` stands for: the action's verb, who invoked it and
    /// for whom, its invocation, and the context whole.
    fn of_context(context: crate::ActionContext) -> Self {
        let acting = context.invoker.as_ref().map(|invoker| &invoker.acting);
        Self {
            verb: verb_of(context.what.as_ref()),
            invoker: acting
                .map(|acting| Self::by(&acting.by))
                .unwrap_or_default(),
            acted_for: acting
                .and_then(|acting| acting.acted_for.first())
                .and_then(|person| match &person.by {
                    tilo_action_policy::Actor::Member(member) => member.person_id(),
                    _ => None,
                })
                .map(str::to_owned),
            invocation: context
                .source_invocation
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_default(),
            signed: context.signed.clone(),
            context: Some(Box::new(context)),
            ..Self::default()
        }
    }

    /// Who invoked it: the actor the writer named, or the person the workspace's resource route
    /// stamped on a person's own write (their person id alone). Nobody for a stamp naming
    /// neither.
    #[must_use]
    pub fn invoked_by(&self) -> Option<tilo_action_policy::Actor> {
        // An empty invoker names nobody: it must not read as a person with no id.
        if self.invoker.is_empty() {
            return None;
        }
        if let Ok(actor) = tilo_failure::ignored!(
            "a stamp the route wrote holds a person id, which is read next",
            serde_json::from_str::<tilo_action_policy::Actor>(&self.invoker)
        ) {
            return Some(actor);
        }
        tilo_failure::ignored!(
            "a stamp naming neither an actor nor a person names nobody",
            tilo_action_policy::PersonId::parse(&self.invoker)
        )
        .ok()
        .map(|person| {
            tilo_action_policy::Actor::Member(tilo_action_policy::MemberId::person(person))
        })
    }

    /// What the change did, as history reads it: the context its writer gave, with who invoked
    /// it, the work it is a step of, its invocation and, for a step through history itself (an
    /// undo, a checkpoint), that step, from the stamp where the context left them out.
    #[must_use]
    pub fn action_context(&self) -> crate::ActionContext {
        let mut context = self.context.as_deref().cloned().unwrap_or_default();
        if context.invoker.is_none()
            && let Some(by) = self.invoked_by()
        {
            // How an agent acted (handed its session by its person's device, or under a key of its
            // own) is what its writer recorded in the context: a stamp that records none names an
            // agent with a key of its own, which shows the call it signed.
            let acting = tilo_action_policy::Acting {
                by,
                app: None,
                agent_session: None,
                cause: None,
                foreign_device: false,
                handed: false,
                acted_for: self
                    .acted_for
                    .iter()
                    .filter_map(|person| tilo_action_policy::PersonId::parse(person).ok())
                    .map(tilo_action_policy::Acting::person)
                    .collect(),
            };
            context.invoker = Some(crate::ActionInvoker::admitted(&acting));
        }
        if context.group.is_none()
            && let Some(group) = &self.group
        {
            context.group = Some(crate::WorkGroup {
                id: group.id.clone(),
                label: (!group.label.is_empty()).then(|| group.label.clone()),
                kind: crate::context::AGENT_SESSION_GROUP.to_owned(),
                declared_id: None,
                session: None,
            });
        }
        if context.source_invocation.is_none() {
            context.source_invocation = self.invocation.parse().ok();
        }
        // A store write keeps the call its caller signed in its stamp: the change's history
        // entry names it, so every holder checks that caller asked for it, and with which key.
        if context.signed.is_none() {
            context.signed.clone_from(&self.signed);
        }
        if context.what.is_none() {
            let op = match (self.verb.as_str(), &self.undoes) {
                (Self::CHECKPOINT_VERB, _) => Some(crate::HistoryOp::Checkpoint {
                    name: self
                        .args
                        .first()
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                }),
                (_, Some(_)) => Some(crate::HistoryOp::Undo),
                _ => None,
            };
            context.what = op.map(|op| {
                crate::Did::History(crate::entry::HistoryStep {
                    op,
                    steps: 1,
                    of: None,
                    undoes: self.undoes.clone(),
                    verb: if self.undoes.is_some() {
                        self.verb.clone()
                    } else {
                        String::new()
                    },
                })
            });
        }
        context
    }

    /// The stamp `change` carries; `None` for a change that carries none.
    #[must_use]
    pub fn of(change: &automerge::Change) -> Option<Self> {
        Self::of_message(change.message()?)
    }
}
