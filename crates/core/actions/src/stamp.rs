//! What a resource store's change names in its message: the action that made it, which every
//! holder judges the change against, and which history and undo group it by.

use serde::{Deserialize, Serialize};

/// The action a change was made by, and the membership its author saw: what a holder judges
/// the change against. It travels in the change itself.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stamp {
    /// The verb invoked.
    pub verb: String,
    /// The action invoked, where [`Self::verb`] ran as a step of it: the action at the top of
    /// the calls the invocation made (Chat's report, whose message its `send` writes). A trigger
    /// naming the action invoked fires on what its steps wrote. Empty where the verb is the
    /// action invoked; a change written before it was kept reads as empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub invoked: String,
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
    /// The call the change was made for and the arguments recorded for it, by their digest
    /// ([`Self::call_digest`]) in hex: a call that writes many records of one document keeps
    /// itself once there, in the first change of its write ([`Self::signed`], [`Self::args`]),
    /// and every other change of that write names it by this. A holder reads such a change with
    /// the calls its write keeps ([`KeptCalls`]) and refuses one whose call is not there, or is
    /// not the one named.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call: Option<String>,
    /// What the change keeps of its call's argument, where the call's statement signs the root of
    /// the argument's parts ([`crate::remote::argument_parts`]): the parts the change holds
    /// itself, with the proof that they are the signed argument's. The call it keeps, or names,
    /// then keeps no argument, and its [`Self::args`] hold only what the change holds too. A
    /// holder refuses a change whose parts do not prove against its call's statement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parts: Option<crate::remote::argument_parts::KeptParts>,
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
    pub fn by(actor: &tilo_member_types::Actor) -> String {
        serde_json::to_string(actor).unwrap_or_default()
    }

    /// The message a change carries it as. A call on its way to its changes is never kept as
    /// it is: a writer that did not say what the change holds ([`Self::for_change`]) keeps none
    /// of its argument.
    #[must_use]
    pub fn message(&self) -> String {
        if self.in_flight() {
            return self
                .for_change(&crate::remote::argument_parts::Held::nothing())
                .message();
        }
        serde_json::to_string(self).unwrap_or_default()
    }

    /// Whether it keeps a call on its way to its changes ([`crate::remote::SignedCall::in_flight`]):
    /// one a writer keeps only as a change that holds what it wrote does ([`Self::for_change`]).
    #[must_use]
    pub fn in_flight(&self) -> bool {
        let in_flight = |call: &Option<Box<crate::remote::SignedCall>>| {
            call.as_deref()
                .is_some_and(crate::remote::SignedCall::in_flight)
        };
        in_flight(&self.signed)
            || self
                .context
                .as_deref()
                .is_some_and(|context| in_flight(&context.signed))
    }

    /// This stamp as a change that holds `held` once written keeps it, where its call signs its
    /// argument's parts: the call without its argument, the parts of the argument the change
    /// holds with their proof ([`Self::parts`]), and the recorded arguments with every value
    /// the change does not hold made null; a mark keeps the arguments that say what it marks
    /// ([`crate::remote::argument_parts::Held::mark`]). Any other stamp as it is.
    #[must_use]
    pub fn for_change(&self, held: &crate::remote::argument_parts::Held) -> Self {
        let mut stamp = self.clone();
        if let Some(context) = stamp.context.as_deref_mut()
            && let Some(call) = context.signed.as_deref_mut()
        {
            *call = call.kept();
        }
        let Some(call) = self.signed.as_deref().filter(|call| call.in_flight()) else {
            return stamp;
        };
        stamp.parts = call.kept_parts(held);
        stamp.signed = Some(Box::new(call.kept()));
        if !held.is_mark() {
            stamp.args = self.args.iter().map(|arg| held.redact(arg)).collect();
        }
        stamp
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
    /// (`$invocation`), a block that settles one (`$settle-block`), and a step taken back or
    /// brought back ([`Self::UNDO_VERB`], [`Self::RESTORE_VERB`]). A store's schema declares them,
    /// and under them a change may write any field or content of a declared kind, so a surface an
    /// app ships may declare none of them.
    pub const PLATFORM_VERBS: [&str; 4] = [
        "$invocation",
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
    pub const RESERVED_VERBS: [&str; 5] = [
        "$invocation",
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
                    tilo_member_types::Actor::Member(member) => member.person_id(),
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
    pub fn invoked_by(&self) -> Option<tilo_member_types::Actor> {
        // An empty invoker names nobody: it must not read as a person with no id.
        if self.invoker.is_empty() {
            return None;
        }
        if let Ok(actor) = tilo_failure::ignored!(
            "a stamp the route wrote holds a person id, which is read next",
            serde_json::from_str::<tilo_member_types::Actor>(&self.invoker)
        ) {
            return Some(actor);
        }
        tilo_failure::ignored!(
            "a stamp naming neither an actor nor a person names nobody",
            tilo_member_types::PersonId::parse(&self.invoker)
        )
        .ok()
        .map(|person| tilo_member_types::Actor::Member(tilo_member_types::MemberId::person(person)))
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
            let acting = tilo_member_types::Acting {
                by,
                app: None,
                agent_session: None,
                cause: None,
                foreign_device: false,
                handed: false,
                acted_for: self
                    .acted_for
                    .iter()
                    .filter_map(|person| tilo_member_types::PersonId::parse(person).ok())
                    .map(tilo_member_types::Acting::person)
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

    /// The version a migration change brings its record to, and whether it is a rewrite by
    /// one who may write the record rather than the step every holder derives alike; `None`
    /// for any other change.
    #[must_use]
    pub fn migration(&self) -> Option<(u32, bool)> {
        match &self.context.as_deref()?.what {
            Some(crate::Did::History(step)) => match step.op {
                crate::HistoryOp::Migrate { to, rewrite } => Some((to, rewrite)),
                _ => None,
            },
            _ => None,
        }
    }

    /// The stamp `change` carries; `None` for a change that carries none.
    #[must_use]
    pub fn of(change: &automerge::Change) -> Option<Self> {
        Self::of_message(change.message()?)
    }

    /// The digest a change names the call it keeps by ([`Self::call`]): the signed call's own
    /// ([`crate::remote::SignedCall::digest`]) and the arguments the runtime recorded for it,
    /// hashed together. `None` for a stamp that keeps no call.
    #[must_use]
    pub fn call_digest(&self) -> Option<String> {
        let call = self.signed.as_deref()?;
        let mut hasher = blake3::Hasher::new();
        if self.parts.is_some() {
            // Each change keeps its own parts and arguments: the call alone is named.
            hasher.update(b"tilo-stamped-call-v2\0");
            hasher.update(&call.digest());
            return Some(hex::encode(hasher.finalize().as_bytes()));
        }
        hasher.update(b"tilo-stamped-call-v1\0");
        hasher.update(&call.digest());
        // Read back from the change's message, the arguments write the same bytes again.
        hasher.update(&serde_json::to_vec(&self.args).unwrap_or_default());
        Some(hex::encode(hasher.finalize().as_bytes()))
    }

    /// Name the call this stamp keeps, and the arguments recorded for it, by their digest, and
    /// drop both where `kept`: what a writer stamps each change of one call's write in one
    /// document with, so the call and its arguments travel once per document, in the first of
    /// them, and a call that writes many records writes them in a size that grows with how
    /// many, not with that times its argument.
    pub fn name_call(&mut self, kept: bool) {
        let Some(digest) = self.call_digest() else {
            return;
        };
        self.call = Some(digest);
        if kept {
            self.signed = None;
            if self.parts.is_none() {
                self.args.clear();
            }
        }
    }

    /// Read the call this stamp names by digest, and the arguments recorded for it, from
    /// `calls`, the calls its write keeps.
    ///
    /// # Errors
    /// [`CallUnresolved::Missing`] when the call it names is not among them, and
    /// [`CallUnresolved::Mismatch`] when it keeps a call, or arguments, other than those it
    /// names.
    pub fn resolve(&mut self, calls: &KeptCalls) -> Result<(), CallUnresolved> {
        if let Some(named) = self.call.as_deref() {
            if self.signed.is_some() {
                if self.call_digest().as_deref() != Some(named) {
                    return Err(CallUnresolved::Mismatch);
                }
            } else if self.parts.is_some() {
                // Its parts and arguments are its own; the call alone is named.
                let (call, _) = calls.0.get(named).ok_or(CallUnresolved::Missing)?;
                self.signed = Some(Box::new(call.clone()));
            } else {
                // A change that names its call keeps no arguments of its own: they are the call's.
                if !self.args.is_empty() {
                    return Err(CallUnresolved::Mismatch);
                }
                let (call, args) = calls.0.get(named).ok_or(CallUnresolved::Missing)?;
                self.signed = Some(Box::new(call.clone()));
                self.args.clone_from(args);
            }
        }
        self.proven()
    }

    /// Whether the parts of the argument it keeps are the signed argument's: the call it keeps
    /// signs the root they climb to, and keeps no argument or salt beside them.
    ///
    /// # Errors
    /// [`CallUnresolved::Unproven`] for parts that are not, or that no call signs.
    pub fn proven(&self) -> Result<(), CallUnresolved> {
        let Some(parts) = &self.parts else {
            return Ok(());
        };
        let call = self.signed.as_deref().ok_or(CallUnresolved::Unproven)?;
        if call.argument.is_some()
            || call.salt.is_some()
            || !parts.proves(&call.signed.statement().arguments)
        {
            return Err(CallUnresolved::Unproven);
        }
        Ok(())
    }
}

/// The calls a write keeps, by their digests ([`Stamp::call_digest`]), each with the arguments
/// recorded for it: what a change of it that names its call by digest is read with
/// ([`Stamp::resolve`]).
#[derive(Clone, Debug, Default)]
pub struct KeptCalls(
    std::collections::BTreeMap<String, (crate::remote::SignedCall, Vec<serde_json::Value>)>,
);

impl KeptCalls {
    /// The calls the changes whose messages are `messages` keep, each under the digest it is
    /// named by. A change that keeps a call under a digest that is not the call's adds none:
    /// it is refused itself when it is read ([`Stamp::resolve`]).
    #[must_use]
    pub fn of_messages<'a>(messages: impl IntoIterator<Item = &'a str>) -> Self {
        let mut calls = Self::default();
        for message in messages {
            // Only a stamp that keeps a call and names it keeps one here: no other is parsed.
            if message.contains("\"call\"")
                && message.contains("\"signed\"")
                && let Some(stamp) = Stamp::of_message(message)
            {
                calls.keep(&stamp);
            }
        }
        calls
    }

    /// The calls `changes` keep ([`Self::of_messages`]).
    #[must_use]
    pub fn of_changes<'a>(changes: impl IntoIterator<Item = &'a automerge::Change>) -> Self {
        Self::of_messages(changes.into_iter().filter_map(automerge::Change::message))
    }

    /// Keep the call `stamp` keeps, under the digest it names it by.
    pub fn keep(&mut self, stamp: &Stamp) {
        if let (Some(call), Some(named)) = (stamp.signed.as_deref(), stamp.call.as_deref())
            && stamp.call_digest().as_deref() == Some(named)
        {
            self.0
                .insert(named.to_owned(), (call.clone(), stamp.args.clone()));
        }
    }

    /// Keep the calls `other` keeps too.
    pub fn extend(&mut self, other: Self) {
        self.0.extend(other.0);
    }

    /// Whether it keeps the call named `digest`.
    #[must_use]
    pub fn holds(&self, digest: &str) -> bool {
        self.0.contains_key(digest)
    }

    /// How many calls it keeps.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether it keeps none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Why a change's call does not read ([`Stamp::resolve`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CallUnresolved {
    #[error("the call the change names is not kept with its write")]
    Missing,
    #[error("the call the change keeps is not the one it names")]
    Mismatch,
    #[error("the parts of the argument the change keeps are not the ones its caller signed")]
    Unproven,
}
