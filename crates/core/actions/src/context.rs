//! The history entry an action carries into its committed change.
//!
//! The action payload says how to reproduce a mutation. [`ActionContext`]
//! is the entry history keeps of it: who acted and for whom, in which task or
//! run, on which resource and item (with the item's label at the time), what
//! was done ([`Did`]), and what kind of change it was. It contains no
//! sentence: readers render one in their own language, and agents, scripts
//! and logs read the data. The caller supplies who and why (invoker,
//! purpose, group); the executor fills in what, from the action itself.

use serde::{Deserialize, Serialize};

use crate::InvocationId;
use crate::entry::{ActionItem, Described, Did, PathStep};
use crate::key::ActionKind;
use tilo_action_policy::{Acting, Actor};

/// Prefix used when an entry is embedded in an Automerge change message,
/// followed by the entry in its compact form ([`crate::compact`]).
/// Ordinary application-authored commit messages remain untouched.
pub const CHANGE_MESSAGE_PREFIX: &str = "tilo.entry/3 ";

/// Prefix of a change message that continues its writer's last whole entry
/// (the next keystroke of a word, or the first of the next word):
/// `tilo.cont/3 <gesture> <claim> <kept> <tail> <middle>`. It is that entry
/// again but for its gesture and the action's payload, so it carries only
/// the gesture (its [`crate::compact`] form), what the change claims to be
/// ([`ContinuedAction`]) and how its payload differs from that entry's: that
/// payload's first `kept` and last `tail` bytes around `middle`. Readers
/// fold it into that entry ([`ActionContext::continued`]).
pub const CONTINUATION_PREFIX: &str = "tilo.cont/3 ";

#[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(remote = "Self", default)]
pub struct ActionContext {
    /// Publishing app, when the host knows it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app: Option<ActionApp>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource: Option<ActionResource>,
    /// The item inside the resource the action addressed, with its label at
    /// the time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item: Option<ActionItem>,
    /// What the action did, described by the action before it applied.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub what: Option<Did>,
    /// What the action is able to do (read, change the document, reach
    /// outside it). Written by the executor from the action.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<ActionKind>,
    /// What the actor says they are trying to accomplish. Never inferred.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub purpose: Option<String>,
    /// Stable cross-action work unit: issue, agent run, task, or session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<WorkGroup>,
    /// Browseable dimensions that do not fit the resource hierarchy.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub facets: Vec<ActionFacet>,
    /// Who invoked the action, for whom, and whose view it resolves
    /// against. `None` is the local person acting in this window.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invoker: Option<ActionInvoker>,
    /// The invocation admitted by the action API, when this change came
    /// through one. This is correlation, not authority: executors replace
    /// this field from their retained admission before applying an action.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_invocation: Option<InvocationId>,
    /// The invocation as its caller signed it, when a host ran the action for
    /// someone who asked it under their own key: the host signs the change as
    /// the one who ran it, and this says who asked, for what, with what
    /// arguments. A peer reading the change checks it against the person's
    /// root ([`crate::remote::SignedCall`]); the host never makes one up.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signed: Option<Box<crate::remote::SignedCall>>,
    /// The bot run that made this change, when one did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caused_by: Option<Cause>,
    /// The action this change ran, encoded as the executor dispatched it:
    /// what a device that receives the change replays to verify it, and
    /// what redo runs again. The executor records it on every change it
    /// commits whose action the change documents whole
    /// ([`WHOLE_ACTION_BUDGET`]); a larger one records its large parts as
    /// content ([`crate::content`]) and names them, and one too large even
    /// then ([`HISTORY_BUDGET`]) is refused. It describes the change; it
    /// carries no caller authority.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<crate::EncodedAction>,
    /// The inverse of [`action`](Self::action) at its before state, as the
    /// canonical executor computed it, for an action whose undo is an action
    /// (it queued an outside consequence). Absent for one undone by
    /// reverting the change. Carries no caller authority.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compensation: Option<crate::EncodedAction>,
    /// The gesture this change belongs to: an id every change of one
    /// actor's gesture shares (the keystrokes of a word, the values one drag
    /// of a slider passes through), which undo takes back as one step.
    /// Absent for a change that is a step of its own.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gesture: Option<crate::ActionId>,
    /// What the change is to the people sharing the document, declared by
    /// the action ([`crate::Action::change_kind`]) or, for a change that is
    /// nobody's step whatever it does (what a brand-new record starts with),
    /// by the context it is applied with.
    #[serde(skip_serializing_if = "ChangeKind::is_content")]
    pub change: ChangeKind,
    /// The facts an ask shows, as the calling runtime evaluated them, when
    /// caller and callee are one app: values only that app can read (a
    /// draft's subject). Taken only from that app's own call; any other
    /// caller's are ignored and admission reads them from the arguments.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shows: Vec<crate::remote::asked::Fact>,
    /// The person's approve of the asked record this invocation waited in, when it is that
    /// call sent again ([`crate::remote::InvokeRequest::approval`]): what an app's handler
    /// reads to run what it refused for want of their yes. Set only by the host that
    /// admitted the invocation, from the call itself, never from a caller's context.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval: Option<crate::remote::asked::Approval>,
}

read_through_json_value!(ActionContext);

/// The gesture a change records for the undo step `gesture` its caller
/// named, under the caller's `group`: one id for every change of that step,
/// and never one the caller chose.
fn ambient_gesture(gesture: crate::ActionId, group: Option<&WorkGroup>) -> crate::ActionId {
    recorded_gesture(
        gesture,
        group.map(|group| (group.kind.as_str(), group.id.as_str())),
    )
}

/// The gesture history records for the undo step `gesture` a caller named,
/// under the run (its kind and id, as history lists them) the change was
/// stamped with: how the caller finds its own step's changes in a record's
/// history, without it ever naming a gesture someone else's change records.
#[must_use]
pub fn recorded_gesture(gesture: crate::ActionId, run: Option<(&str, &str)>) -> crate::ActionId {
    let mut hasher = blake3::Hasher::new_derive_key("tilo ambient gesture v1");
    hasher.update(gesture.0.as_bytes());
    if let Some((kind, id)) = run {
        for part in [kind, id] {
            hasher.update(&u64::try_from(part.len()).unwrap_or(u64::MAX).to_le_bytes());
            hasher.update(part.as_bytes());
        }
    }
    let id = *hasher
        .finalize()
        .as_bytes()
        .first_chunk::<16>()
        .unwrap_or(&[0; 16]);
    crate::ActionId(uuid::Builder::from_custom_bytes(id).into_uuid())
}

// Written out rather than derived: a derived clone is inlined into every
// crate that clones a context.
impl Clone for ActionContext {
    fn clone(&self) -> Self {
        Self {
            app: self.app.clone(),
            resource: self.resource.clone(),
            item: self.item.clone(),
            what: self.what.clone(),
            kind: self.kind,
            purpose: self.purpose.clone(),
            group: self.group.clone(),
            facets: self.facets.clone(),
            invoker: self.invoker.clone(),
            source_invocation: self.source_invocation,
            signed: self.signed.clone(),
            caused_by: self.caused_by.clone(),
            action: self.action.clone(),
            compensation: self.compensation.clone(),
            gesture: self.gesture,
            change: self.change,
            shows: self.shows.clone(),
            approval: self.approval.clone(),
        }
    }
}

/// The most an entry's [`compensation`](ActionContext::compensation) may
/// take, encoded: an action whose inverse is larger is refused.
pub const COMPENSATION_BUDGET: usize = 128 * 1024;

/// The most a change documents whole of the action it records, its payload inline: a larger
/// action keeps its long parts as content ([`crate::content`]) and names them instead. Every
/// device a change reaches reads that entry to verify the change, so what one change carries
/// of an action stays small: 256 KiB of payload is a few hundred items of an import, or a long
/// note's body.
pub const WHOLE_ACTION_BUDGET: usize = 256 * 1024;

/// The most an entry's [`action`](ActionContext::action) may take, its large parts kept as
/// content included: a larger action is refused.
pub const HISTORY_BUDGET: usize = 1024 * 1024;

/// What a change is to the people sharing a document. Every kind is durable,
/// shared and kept in the document's own history; they differ in whether a
/// person takes the change back and whether a feed of what people did to the
/// workspace shows it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ChangeKind {
    /// Something a person did to the content: an undo step of its author's,
    /// shown in workspace activity.
    #[default]
    Content,
    /// Housekeeping about the content (a read marker, a reading position):
    /// no undo step, and workspace activity leaves it out.
    Housekeeping,
    /// Content that came from outside rather than from anyone's edit:
    /// articles a refresh fetched, a process's output, what a brand-new
    /// record starts with from state that already existed. Members
    /// see it like any content, but it is nobody's undo step, and workspace
    /// activity leaves it out.
    External,
}

impl ChangeKind {
    pub const fn is_content(&self) -> bool {
        matches!(self, Self::Content)
    }

    /// Whether a change of this kind is an undo step of its author's. Only
    /// a person's own edits of the content are.
    pub const fn is_step(&self) -> bool {
        self.is_content()
    }
}

impl ActionContext {
    /// Fill missing provenance from a task/session context while preserving
    /// every value supplied by the concrete action/resource.
    #[must_use]
    pub fn enriched_by(mut self, ambient: &Self) -> Self {
        if self.app.is_none() {
            self.app.clone_from(&ambient.app);
        }
        if self.resource.is_none() {
            self.resource.clone_from(&ambient.resource);
        }
        if self.item.is_none() {
            self.item.clone_from(&ambient.item);
        }
        if self.purpose.is_none() {
            self.purpose.clone_from(&ambient.purpose);
        }
        if self.group.is_none() {
            self.group.clone_from(&ambient.group);
        }
        if self.invoker.is_none() {
            self.invoker.clone_from(&ambient.invoker);
        }
        if self.source_invocation.is_none() {
            self.source_invocation = ambient.source_invocation;
        }
        if self.signed.is_none() {
            self.signed.clone_from(&ambient.signed);
        }
        if self.caused_by.is_none() {
            self.caused_by.clone_from(&ambient.caused_by);
        }
        // The caller's undo step: what an API joins its calls under (one
        // task step), so they are taken back as one. Rebound under the
        // caller's group, never recorded as the caller wrote it: a caller
        // cannot name a gesture of someone else's and have its change
        // continue theirs.
        if self.gesture.is_none() {
            self.gesture = ambient
                .gesture
                .map(|gesture| ambient_gesture(gesture, ambient.group.as_ref()));
        }
        for facet in &ambient.facets {
            if !self.facets.contains(facet) {
                self.facets.push(facet.clone());
            }
        }
        self
    }

    /// Pin the invocation retained by the admitted dispatch boundary.
    ///
    /// This replaces rather than fills an ambient value. Only the executor
    /// that owns admission should call it.
    #[must_use]
    pub fn with_source_invocation(mut self, invocation: Option<InvocationId>) -> Self {
        self.source_invocation = invocation;
        self
    }

    /// Record the bot run this change continues.
    #[must_use]
    pub fn with_caused_by(mut self, cause: Option<Cause>) -> Self {
        self.caused_by = cause;
        self
    }

    pub fn for_resource(
        id: impl Into<String>,
        kind: impl Into<String>,
        label: impl Into<String>,
    ) -> Self {
        Self::of_resource(ActionResource {
            id: id.into(),
            kind: kind.into(),
            label: label.into(),
        })
    }

    /// [`Self::for_resource`] once its strings are made: not generic, so the
    /// rest of the context is built by code compiled once rather than in
    /// every crate that names a resource with `&str`s.
    #[inline(never)]
    fn of_resource(resource: ActionResource) -> Self {
        Self {
            resource: Some(resource),
            ..Self::default()
        }
    }

    /// The entry of an action published by one app.
    pub fn for_app(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self::default().with_app(id, label)
    }

    #[must_use]
    pub fn with_app(mut self, id: impl Into<String>, label: impl Into<String>) -> Self {
        self.app = Some(ActionApp {
            id: id.into(),
            label: label.into(),
        });
        self
    }

    /// Fill app ownership from the registered action type when a generic
    /// palette/API caller has only resource metadata. Explicit app context
    /// always wins. Action types conventionally start with the package stem
    /// (`word-processor.action`, `api_tester.action`).
    #[must_use]
    pub fn with_inferred_app(mut self, type_id: &str) -> Self {
        if self.app.is_some() {
            return self;
        }
        let stem = type_id.split('.').next().unwrap_or("").trim();
        let words: Vec<&str> = stem
            .split(['-', '_'])
            .filter(|word| !word.is_empty())
            .collect();
        if words.is_empty() {
            return self;
        }
        let title_word = |word: &str| {
            let mut chars = word.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_uppercase().collect::<String>() + chars.as_str()
            })
        };
        let component = words
            .iter()
            .map(|word| title_word(word))
            .collect::<String>();
        let label = words
            .iter()
            .map(|word| title_word(word))
            .collect::<Vec<_>>()
            .join(" ");
        self.app = Some(ActionApp {
            id: format!("org.tilo.{component}"),
            label,
        });
        self
    }

    #[must_use]
    pub fn with_purpose(mut self, purpose: impl Into<String>) -> Self {
        let purpose = purpose.into();
        self.purpose = nonempty(&purpose);
        self
    }

    #[must_use]
    pub fn with_group(
        mut self,
        id: impl Into<String>,
        label: impl Into<String>,
        kind: impl Into<String>,
    ) -> Self {
        self.group = Some(WorkGroup {
            id: id.into(),
            label: Some(label.into()),
            kind: kind.into(),
            declared_id: None,
            session: None,
        });
        self
    }

    #[must_use]
    pub fn with_invoker(mut self, invoker: ActionInvoker) -> Self {
        self.invoker = Some(invoker);
        self
    }

    /// Host-side rewrite of a caller-supplied context: the invoker becomes
    /// exactly the authenticated `actor` and the host-derived `presence`,
    /// discarding whatever invoker the caller put in the envelope.
    #[must_use]
    pub fn with_host_actor(mut self, actor: &Acting, presence: Option<PresenceId>) -> Self {
        let mut invoker = ActionInvoker::admitted(actor);
        invoker.presence = presence;
        self.invoker = Some(invoker);
        self
    }

    #[must_use]
    pub fn with_facet(mut self, facet: ActionFacet) -> Self {
        self.facets.push(facet);
        self
    }

    /// Join the undo step open on this thread
    /// ([`crate::executor::open_step`]) unless the entry names one.
    #[must_use]
    pub fn in_step(mut self) -> Self {
        if self.gesture.is_none() {
            self.gesture = crate::executor::current_step();
        }
        self
    }

    /// Continue the gesture `gesture` (see [`Self::gesture`]).
    #[must_use]
    pub fn in_gesture(mut self, gesture: Option<crate::ActionId>) -> Self {
        self.gesture = gesture;
        self
    }

    #[must_use]
    pub fn with_change(mut self, change: ChangeKind) -> Self {
        self.change = change;
        self
    }

    /// Record what the action did and the item it addressed.
    #[must_use]
    pub fn with_described(mut self, described: Described) -> Self {
        self.what = Some(described.what);
        if described.item.is_some() {
            self.item = described.item;
        }
        self
    }

    /// What the action did and to which item, as one value.
    pub fn described(&self) -> Option<Described> {
        Some(Described {
            what: self.what.clone()?,
            item: self.item.clone(),
        })
    }

    /// Whether the change this context describes is housekeeping, not content.
    pub fn is_housekeeping(&self) -> bool {
        self.change == ChangeKind::Housekeeping
    }

    /// Whether workspace activity shows this change: content, not
    /// housekeeping and not what came from outside.
    pub fn is_activity(&self) -> bool {
        self.change.is_content()
    }

    /// Who invoked the action, when the context names anyone.
    pub fn acting(&self) -> Option<&Acting> {
        self.invoker.as_ref().map(|invoker| &invoker.acting)
    }

    /// Encode into the Automerge change message. Returning `None` keeps an
    /// empty/default context from displacing an app's ordinary history path.
    pub fn change_message(&self) -> Option<String> {
        if self == &Self::default() {
            return None;
        }
        serde_json::to_value(wire::Entry::of(self))
            .ok()
            .map(|entry| format!("{CHANGE_MESSAGE_PREFIX}{}", crate::compact::encode(&entry)))
    }

    /// Read an entry from a change message. Unknown and malformed messages
    /// are ordinary app commits, never an error that hides their history. A
    /// continuation is no entry on its own: read it with
    /// [`continued`](Self::continued) from its writer's last whole entry.
    pub fn from_change_message(message: &str) -> Option<Self> {
        serde_json::from_value::<wire::Entry>(entry_value(message)?)
            .ok()
            .map(wire::Entry::into_context)
    }

    /// Read only what an entry says about who made its change, in which run
    /// and step, and what kind of change it is (its group, invoker, invocation,
    /// gesture, kind and change kind; everything else left default): what an index
    /// over a long history keeps of every change, read without decoding
    /// what the change did. `None` where [`from_change_message`](Self::from_change_message)
    /// reads no entry.
    pub fn head_of_change_message(message: &str) -> Option<Self> {
        let entry = crate::compact::json_of(message.strip_prefix(CHANGE_MESSAGE_PREFIX)?)?;
        serde_json::from_slice::<wire::Head>(&entry)
            .ok()
            .map(wire::Head::into_context)
    }

    /// This entry, written as the continuation of `first` (its writer's
    /// last whole entry): when it is in a gesture and differs from `first`
    /// only in its gesture and its action's payload. `None` when it has to
    /// be written whole.
    #[must_use]
    pub fn continuation_of(&self, first: &Self) -> Option<String> {
        let gesture = self.gesture?;
        let action = self.action.as_ref()?;
        let earlier = first.action.as_ref()?;
        let bare = |entry: &Self| Self {
            action: None,
            gesture: None,
            ..entry.clone()
        };
        if self.compensation.is_some()
            || first.compensation.is_some()
            || action.type_id != earlier.type_id
            || bare(self) != bare(first)
        {
            return None;
        }
        let claim = ContinuedAction::token(self)?;
        let now = std::str::from_utf8(&action.payload).ok()?;
        let then = std::str::from_utf8(&earlier.payload).ok()?;
        let kept = now
            .char_indices()
            .zip(then.chars())
            .find(|((_, now), then)| now != then)
            .map_or_else(|| now.len().min(then.len()), |((at, _), _)| at);
        let (now_rest, then_rest) = (now.get(kept..)?, then.get(kept..)?);
        let tail: usize = now_rest
            .chars()
            .rev()
            .zip(then_rest.chars().rev())
            .take_while(|(now, then)| now == then)
            .map(|(now, _)| now.len_utf8())
            .sum();
        let middle = now_rest.get(..now_rest.len().checked_sub(tail)?)?;
        Some(format!(
            "{CONTINUATION_PREFIX}{} {claim} {kept} {tail} {middle}",
            gesture_token(gesture)
        ))
    }

    /// The gesture a continuation `message` continues.
    #[must_use]
    pub fn continued_gesture(message: &str) -> Option<crate::ActionId> {
        let rest = message.strip_prefix(CONTINUATION_PREFIX)?;
        gesture_of(rest.split(' ').next()?)
    }

    /// The entry a continuation `message` stands for, from its writer's last
    /// whole entry `first`.
    #[must_use]
    pub fn continued(message: &str, first: &Self) -> Option<Self> {
        let rest = message.strip_prefix(CONTINUATION_PREFIX)?;
        let mut parts = rest.splitn(3, ' ');
        let gesture = gesture_of(parts.next()?)?;
        let _claim = parts.next()?;
        let mut diff = parts.next()?.splitn(3, ' ');
        let kept = diff.next()?.parse::<usize>().ok()?;
        let tail = diff.next()?.parse::<usize>().ok()?;
        let earlier = first.action.as_ref()?;
        let then = std::str::from_utf8(&earlier.payload).ok()?;
        let payload = [
            then.get(..kept)?,
            diff.next()?,
            then.get(then.len().checked_sub(tail)?..)?,
        ]
        .concat();
        Some(Self {
            action: Some(crate::EncodedAction {
                type_id: earlier.type_id.clone(),
                payload: payload.into_bytes(),
            }),
            gesture: Some(gesture),
            ..first.clone()
        })
    }
}

/// What a continuation says its change is, in the terms a change's claim is
/// written in, as the entry it continues says it: an edit of a resource kind
/// at a path (the whole resource for an empty one), or one of its verbs on an
/// item. A continuation writes it as one token: `e:<kind>:<step>/…` or
/// `v:<app>/<kind>/<action>[:<item>]`, a step being `f<field>`, `i<id>`,
/// `k<key>` or `c<row>:<column>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContinuedAction {
    Edit {
        kind: String,
        path: Vec<PathStep>,
    },
    Verb {
        action: crate::ActionKey,
        item: Option<String>,
    },
}

impl ContinuedAction {
    /// The claim a continuation of `entry` writes; `None` for one whose names
    /// hold a byte the token reserves, which writes the entry whole instead.
    fn token(entry: &ActionContext) -> Option<String> {
        let edit = |path: &[PathStep]| Self::Edit {
            kind: entry
                .resource
                .as_ref()
                .map_or_else(String::new, |resource| resource.kind.clone()),
            path: path.to_vec(),
        };
        let action = match &entry.what {
            Some(Did::Verb(verb)) => Self::Verb {
                action: verb.action.clone(),
                item: entry.item.as_ref().map(|item| item.id.clone()),
            },
            Some(Did::Edit(changed)) => edit(&changed.path),
            None => edit(&[]),
            Some(_) => return None,
        };
        let mut token = action.write()?;
        // Who made the gesture, when not the signing person: what every
        // receiver decides each of its changes as. A person's own keystrokes
        // carry nothing more.
        if let Some(invoker) = &entry.invoker {
            let tree = serde_json::to_value(&invoker.acting).ok()?;
            token.push(INVOKER_MARK);
            token.push_str(&hex::encode(crate::compact::encode(&tree)));
        }
        Some(token)
    }

    /// Who made the gesture a continuation `message` continues, as its token
    /// names them: `None` for a person's own keystrokes, or a token that does
    /// not read.
    #[must_use]
    pub fn invoker_of_message(message: &str) -> Option<crate::Acting> {
        let rest = message.strip_prefix(CONTINUATION_PREFIX)?;
        let (_, invoker) = rest.split(' ').nth(1)?.split_once(INVOKER_MARK)?;
        let text = crate::compact::decode(std::str::from_utf8(&hex::decode(invoker).ok()?).ok()?)?;
        serde_json::from_str(&text).ok()
    }

    /// The claim a continuation `message` writes.
    #[must_use]
    pub fn of_message(message: &str) -> Option<Self> {
        let rest = message.strip_prefix(CONTINUATION_PREFIX)?;
        let token = rest.split(' ').nth(1)?;
        Self::parse(
            token
                .split_once(INVOKER_MARK)
                .map_or(token, |(claim, _)| claim),
        )
    }

    /// The token this claim writes, read back by [`Self::parse`].
    fn write(&self) -> Option<String> {
        /// A name as the token carries it; the separators end it.
        fn name(text: &str) -> Option<&str> {
            (!text.contains([' ', ':', '/', INVOKER_MARK])).then_some(text)
        }
        fn write_step(token: &mut String, at: usize, step: &PathStep) -> Option<()> {
            if at > 0 {
                token.push('/');
            }
            let (tag, text) = match step {
                PathStep::Field(field) => ('f', name(field)?),
                PathStep::Item(id) => ('i', name(id)?),
                PathStep::Key(key) => ('k', name(key)?),
                PathStep::Cell(row, column) => {
                    token.push('c');
                    token.push_str(name(row)?);
                    token.push(':');
                    token.push_str(name(column)?);
                    return Some(());
                }
            };
            token.push(tag);
            token.push_str(text);
            Some(())
        }
        let mut token = String::new();
        match self {
            Self::Edit { kind, path } => {
                token.push_str("e:");
                token.push_str(name(kind)?);
                token.push(':');
                for (at, step) in path.iter().enumerate() {
                    write_step(&mut token, at, step)?;
                }
            }
            Self::Verb { action, item } => {
                token.push_str("v:");
                for part in [&action.app, &action.kind, &action.action] {
                    token.push_str(name(part)?);
                    token.push('/');
                }
                token.pop();
                if let Some(item) = item {
                    token.push(':');
                    token.push_str(name(item)?);
                }
            }
        }
        Some(token)
    }

    /// The claim a continuation's token writes.
    fn parse(token: &str) -> Option<Self> {
        fn read_step(step: &str) -> Option<PathStep> {
            let (tag, rest) = step.split_at_checked(1)?;
            Some(match tag {
                "f" => PathStep::Field(rest.to_owned()),
                "i" => PathStep::Item(rest.to_owned()),
                "k" => PathStep::Key(rest.to_owned()),
                "c" => {
                    let (row, column) = rest.split_once(':')?;
                    PathStep::Cell(row.to_owned(), column.to_owned())
                }
                _ => return None,
            })
        }
        let (tag, rest) = token.split_once(':')?;
        Some(match tag {
            "e" => {
                let (kind, path) = rest.split_once(':')?;
                let path = if path.is_empty() {
                    Vec::new()
                } else {
                    path.split('/').map(read_step).collect::<Option<Vec<_>>>()?
                };
                Self::Edit {
                    kind: kind.to_owned(),
                    path,
                }
            }
            "v" => {
                let (app, rest) = rest.split_once('/')?;
                let (kind, rest) = rest.split_once('/')?;
                let (action, item) = match rest.split_once(':') {
                    Some((action, item)) => (action, Some(item.to_owned())),
                    None => (rest, None),
                };
                Self::Verb {
                    action: crate::ActionKey::new(app, kind, action),
                    item,
                }
            }
            _ => return None,
        })
    }
}

/// What separates a continuation's claim from the invoker it names: the
/// invoker's tree, in its [`crate::compact`] form, as hex.
const INVOKER_MARK: char = '~';

/// The gesture `gesture` as a continuation writes it: its [`crate::compact`]
/// form.
fn gesture_token(gesture: crate::ActionId) -> String {
    crate::compact::encode(&serde_json::Value::String(
        gesture.0.hyphenated().to_string(),
    ))
}

/// The gesture a continuation's token writes.
fn gesture_of(token: &str) -> Option<crate::ActionId> {
    let text = crate::compact::decode(token)?;
    let text = serde_json::from_str::<String>(&text).ok()?;
    uuid::Uuid::try_parse(&text).ok().map(crate::ActionId)
}

/// A change message's entry as a JSON value. Entries are read through a value,
/// not straight from their text, so every reader of entries shares one set of
/// compiled field readers.
fn entry_value(message: &str) -> Option<serde_json::Value> {
    let entry = crate::compact::json_of(message.strip_prefix(CHANGE_MESSAGE_PREFIX)?)?;
    serde_json::from_slice(&entry).ok()
}

/// The entry as a change message carries it: every field of
/// [`ActionContext`] under a short key, the resource and item as arrays, an
/// action with its JSON payload inline, and what a reader derives again
/// left out (the app the action's type names, an undoable kind, a field
/// edit's path that is just its field). Reading it back and writing it again
/// gives the same bytes, so a replaying device writes the same change.
mod wire {
    use serde::{Deserialize, Serialize};

    use super::{
        ActionApp, ActionContext, ActionFacet, ActionInvoker, ActionItem, ActionResource, Cause,
        ChangeKind, WorkGroup,
    };
    use crate::entry::{Did, PathStep};
    use crate::key::ActionKind;

    #[derive(Default, Serialize, Deserialize)]
    #[serde(default)]
    pub(super) struct Entry {
        /// Absent: the app the action's type names; `null`: none.
        #[serde(
            rename = "p",
            skip_serializing_if = "Option::is_none",
            deserialize_with = "present"
        )]
        app: Option<App>,
        #[serde(rename = "r", skip_serializing_if = "Option::is_none")]
        resource: Option<Resource>,
        #[serde(rename = "i", skip_serializing_if = "Option::is_none")]
        item: Option<Item>,
        #[serde(rename = "w", skip_serializing_if = "Option::is_none")]
        what: Option<Did>,
        /// The path every field edit of a group shares, written once.
        #[serde(rename = "wp", skip_serializing_if = "Option::is_none")]
        shared_path: Option<Vec<PathStep>>,
        #[serde(rename = "k", skip_serializing_if = "Option::is_none")]
        kind: Option<ActionKind>,
        #[serde(rename = "pu", skip_serializing_if = "Option::is_none")]
        purpose: Option<String>,
        #[serde(rename = "gr", skip_serializing_if = "Option::is_none")]
        group: Option<WorkGroup>,
        #[serde(rename = "f", skip_serializing_if = "Vec::is_empty")]
        facets: Vec<ActionFacet>,
        #[serde(
            rename = "by",
            skip_serializing_if = "Option::is_none",
            deserialize_with = "readable"
        )]
        invoker: Option<ActionInvoker>,
        #[serde(rename = "s", skip_serializing_if = "Option::is_none")]
        source_invocation: Option<crate::InvocationId>,
        #[serde(rename = "sc", skip_serializing_if = "Option::is_none")]
        signed: Option<Box<crate::remote::SignedCall>>,
        #[serde(rename = "cb", skip_serializing_if = "Option::is_none")]
        caused_by: Option<Cause>,
        #[serde(rename = "a", skip_serializing_if = "Option::is_none")]
        action: Option<Action>,
        #[serde(rename = "u", skip_serializing_if = "Option::is_none")]
        compensation: Option<Action>,
        #[serde(rename = "g", skip_serializing_if = "Option::is_none")]
        gesture: Option<String>,
        #[serde(rename = "c", skip_serializing_if = "ChangeKind::is_content")]
        change: ChangeKind,
        #[serde(rename = "sh", default, skip_serializing_if = "Vec::is_empty")]
        shows: Vec<crate::remote::asked::Fact>,
    }

    /// The fields of an [`Entry`] an index keeps of every change, each left
    /// as its text until read; the rest are skipped unread.
    #[derive(Default, Deserialize)]
    #[serde(default)]
    pub(super) struct Head<'a> {
        #[serde(rename = "k", borrow)]
        kind: Option<&'a serde_json::value::RawValue>,
        #[serde(rename = "gr", borrow)]
        group: Option<&'a serde_json::value::RawValue>,
        #[serde(rename = "by", borrow)]
        invoker: Option<&'a serde_json::value::RawValue>,
        #[serde(rename = "s", borrow)]
        invocation: Option<&'a serde_json::value::RawValue>,
        /// Only whether the entry carries an action: then it is undoable
        /// unless it says otherwise.
        #[serde(rename = "a", borrow)]
        action: Option<&'a serde_json::value::RawValue>,
        #[serde(rename = "g", borrow)]
        gesture: Option<&'a serde_json::value::RawValue>,
        #[serde(rename = "c", borrow)]
        change: Option<&'a serde_json::value::RawValue>,
    }

    /// A field read through a JSON value, as whole entries are read, so the
    /// field readers are compiled once.
    fn field<T: serde::de::DeserializeOwned>(
        raw: Option<&serde_json::value::RawValue>,
    ) -> Option<T> {
        let value = serde_json::from_slice::<serde_json::Value>(raw?.get().as_bytes()).ok()?;
        serde_json::from_value(value).ok()
    }

    impl Head<'_> {
        pub(super) fn into_context(self) -> ActionContext {
            ActionContext {
                kind: field(self.kind).or_else(|| self.action.map(|_| ActionKind::Undoable)),
                group: field(self.group),
                invoker: field(self.invoker),
                source_invocation: field(self.invocation),
                gesture: field::<String>(self.gesture)
                    .and_then(|gesture| uuid::Uuid::try_parse(&gesture).ok())
                    .map(crate::ActionId),
                change: field(self.change).unwrap_or_default(),
                ..ActionContext::default()
            }
        }
    }

    /// An encoded action: its type and its payload, inline as JSON when it
    /// is canonical JSON, else as bytes.
    #[derive(Serialize, Deserialize)]
    #[serde(untagged)]
    enum Action {
        Json(String, serde_json::Value),
        Bytes { t: String, b: Vec<u8> },
    }

    impl Action {
        fn of(action: &crate::EncodedAction) -> Self {
            serde_json::from_slice::<serde_json::Value>(&action.payload)
                .ok()
                .filter(|value| {
                    serde_json::to_vec(value).is_ok_and(|bytes| bytes == action.payload)
                })
                .map_or_else(
                    || Self::Bytes {
                        t: action.type_id.clone(),
                        b: action.payload.clone(),
                    },
                    |value| Self::Json(action.type_id.clone(), value),
                )
        }

        fn into_encoded(self) -> Option<crate::EncodedAction> {
            Some(match self {
                Self::Json(type_id, value) => crate::EncodedAction {
                    type_id,
                    payload: serde_json::to_vec(&value).ok()?,
                },
                Self::Bytes { t, b } => crate::EncodedAction {
                    type_id: t,
                    payload: b,
                },
            })
        }
    }

    /// The resource: id, kind and label, or kind and label for a resource
    /// whose id is its kind (a record's own root).
    #[derive(Serialize, Deserialize)]
    #[serde(untagged)]
    enum Resource {
        Full(String, String, String),
        Root(String, String),
    }

    impl Resource {
        fn of(resource: &ActionResource) -> Self {
            if resource.id == resource.kind {
                Self::Root(resource.kind.clone(), resource.label.clone())
            } else {
                Self::Full(
                    resource.id.clone(),
                    resource.kind.clone(),
                    resource.label.clone(),
                )
            }
        }

        fn into_resource(self) -> ActionResource {
            match self {
                Self::Full(id, kind, label) => ActionResource { id, kind, label },
                Self::Root(kind, label) => ActionResource {
                    id: kind.clone(),
                    kind,
                    label,
                },
            }
        }
    }

    /// The item: its id and label, or its label alone when the resource's
    /// id ends with the item's (`<kind>/<field>/<id>`).
    #[derive(Serialize, Deserialize)]
    #[serde(untagged)]
    enum Item {
        Full(String, String),
        Named((String,)),
    }

    impl Item {
        fn of(item: &ActionItem, resource: Option<&ActionResource>) -> Self {
            let named = resource.is_some_and(|resource| {
                !item.id.is_empty()
                    && !item.id.contains('/')
                    && resource
                        .id
                        .strip_suffix(item.id.as_str())
                        .is_some_and(|rest| rest.ends_with('/'))
            });
            if named {
                Self::Named((item.label.clone(),))
            } else {
                Self::Full(item.id.clone(), item.label.clone())
            }
        }

        fn into_item(self, resource: Option<&ActionResource>) -> ActionItem {
            match self {
                Self::Full(id, label) => ActionItem { id, label },
                Self::Named((label,)) => ActionItem {
                    id: resource
                        .and_then(|resource| resource.id.rsplit('/').next())
                        .unwrap_or_default()
                        .to_owned(),
                    label,
                },
            }
        }
    }

    /// An app named in the entry, or `null` for none.
    #[derive(Serialize, Deserialize)]
    #[serde(untagged)]
    enum App {
        Named(ActionApp),
        None(()),
    }

    impl App {
        fn of(app: Option<ActionApp>) -> Self {
            app.map_or(Self::None(()), Self::Named)
        }

        fn into_app(self) -> Option<ActionApp> {
            match self {
                Self::Named(app) => Some(app),
                Self::None(()) => None,
            }
        }
    }

    /// A field that is there, `null` included.
    fn present<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
    where
        D: serde::Deserializer<'de>,
        T: Deserialize<'de>,
    {
        T::deserialize(deserializer).map(Some)
    }

    /// A field read as `None` when it does not parse, as [`Head`] reads it: an
    /// invoker written in an older form leaves the rest of its entry (the
    /// action, what it did and placed) readable.
    fn readable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
    where
        D: serde::Deserializer<'de>,
        T: serde::de::DeserializeOwned,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        Ok(serde_json::from_value(value).ok())
    }

    /// What a reader derives again, left out: a field edit's path that is
    /// just its field, and a group's distinct actions when the group kept
    /// every member (they are its members without their values).
    fn trimmed(what: &Did) -> Did {
        let mut what = what.clone();
        trim(&mut what);
        what
    }

    fn trim(what: &mut Did) {
        match what {
            Did::Edit(edit) if matches!(edit.path.as_slice(), [PathStep::Field(field)] if *field == edit.field) =>
            {
                edit.path.clear();
            }
            Did::Group(group) => {
                if group.count as usize == group.members.len()
                    && distinct_of(&group.members) == group.distinct
                {
                    group.distinct.clear();
                }
                group.members.iter_mut().for_each(trim);
            }
            _ => {}
        }
    }

    fn filled(mut what: Did) -> Did {
        fill(&mut what);
        what
    }

    fn fill(what: &mut Did) {
        match what {
            Did::Edit(edit) if edit.path.is_empty() => {
                edit.path.push(PathStep::Field(edit.field.clone()));
            }
            Did::Group(group) => {
                group.members.iter_mut().for_each(fill);
                if group.distinct.is_empty() && group.count as usize == group.members.len() {
                    group.distinct = distinct_of(&group.members);
                }
            }
            _ => {}
        }
    }

    /// Where each field edit of `what` sits as the action the entry carries
    /// says it (a field edit's `path` argument; a group's members in order),
    /// when that action says it for every edit `what` describes.
    fn action_paths(
        action: Option<&crate::EncodedAction>,
        what: &Did,
    ) -> Option<Vec<Option<Vec<PathStep>>>> {
        fn path_of(stamped: &serde_json::Value) -> Option<Vec<PathStep>> {
            serde_json::from_value(stamped.get("op")?.get("args")?.get("path")?.clone()).ok()
        }
        let action = action?;
        let value: serde_json::Value = serde_json::from_slice(&action.payload).ok()?;
        let paths: Vec<Option<Vec<PathStep>>> = if crate::group_form::is_group(&action.type_id) {
            crate::group_form::members(value)?
                .iter()
                .map(path_of)
                .collect()
        } else {
            vec![path_of(&value)]
        };
        let fits = match what {
            Did::Edit(_) => paths.len() == 1,
            Did::Group(group) => paths.len() == group.members.len(),
            _ => false,
        };
        (fits && paths.iter().any(Option::is_some)).then_some(paths)
    }

    /// `what` with every edit's path that the action says too left out (a
    /// group's distinct actions too, as [`trimmed`] leaves them); a path
    /// the action does not say stays written, even one that is just the
    /// field.
    fn paths_from_action(what: &Did, paths: &[Option<Vec<PathStep>>]) -> Did {
        let mut what = what.clone();
        let mut members: Vec<&mut Did> = match &mut what {
            Did::Group(group) => {
                if group.count as usize == group.members.len()
                    && distinct_of(&group.members) == group.distinct
                {
                    group.distinct.clear();
                }
                group.members.iter_mut().collect()
            }
            single => vec![single],
        };
        for (member, path) in members.iter_mut().zip(paths) {
            if let Did::Edit(edit) = member
                && path.as_ref() == Some(&edit.path)
            {
                edit.path.clear();
            }
        }
        what
    }

    /// The paths [`paths_from_action`] left out, given back from the action.
    fn paths_given_back(mut what: Did, paths: &[Option<Vec<PathStep>>]) -> Did {
        let members: Vec<&mut Did> = match &mut what {
            Did::Group(group) => group.members.iter_mut().collect(),
            single => vec![single],
        };
        for (member, path) in members.into_iter().zip(paths) {
            if let (Did::Edit(edit), Some(path)) = (member, path)
                && edit.path.is_empty()
            {
                edit.path.clone_from(path);
            }
        }
        what
    }

    /// The path every field edit member of a group shares, when there are
    /// at least two and the path is more than the field itself.
    fn shared_path(what: &Did) -> Option<Vec<PathStep>> {
        let Did::Group(group) = what else {
            return None;
        };
        let mut edits = group.members.iter().filter_map(|member| match member {
            Did::Edit(edit) => Some(edit),
            _ => None,
        });
        let first = edits.next()?;
        let mut more = false;
        for edit in edits {
            if edit.path != first.path {
                return None;
            }
            more = true;
        }
        let trivial =
            matches!(first.path.as_slice(), [PathStep::Field(field)] if *field == first.field);
        (more && !first.path.is_empty() && !trivial).then(|| first.path.clone())
    }

    /// `what` with its edit members' shared path left to [`shared_path`].
    fn without_shared_path(mut what: Did) -> Did {
        if shared_path(&what).is_some()
            && let Did::Group(group) = &mut what
        {
            for member in &mut group.members {
                if let Did::Edit(edit) = member {
                    edit.path.clear();
                }
            }
        }
        what
    }

    /// `what` with the shared path given back to its edit members.
    fn with_shared_path(mut what: Did, shared: Option<&[PathStep]>) -> Did {
        if let (Some(shared), Did::Group(group)) = (shared, &mut what) {
            for member in &mut group.members {
                if let Did::Edit(edit) = member {
                    edit.path = shared.to_vec();
                }
            }
        }
        what
    }

    /// A group's distinct actions, as [`Did::group`] keeps them.
    fn distinct_of(members: &[Did]) -> Vec<Did> {
        match Did::group(members.iter().cloned()) {
            Did::Group(group) => group.distinct,
            _ => Vec::new(),
        }
    }

    /// The app a reader infers from the action's type.
    fn inferred(action: Option<&crate::EncodedAction>) -> Option<ActionApp> {
        ActionContext::default()
            .with_inferred_app(&action?.type_id)
            .app
    }

    impl Entry {
        pub(super) fn of(context: &ActionContext) -> Self {
            let undoable = context.action.is_some() && context.kind == Some(ActionKind::Undoable);
            let paths = context
                .what
                .as_ref()
                .and_then(|what| action_paths(context.action.as_ref(), what));
            Self {
                app: (context.app != inferred(context.action.as_ref()))
                    .then(|| App::of(context.app.clone())),
                resource: context.resource.as_ref().map(Resource::of),
                item: context
                    .item
                    .as_ref()
                    .map(|item| Item::of(item, context.resource.as_ref())),
                shared_path: context
                    .what
                    .as_ref()
                    .filter(|_| paths.is_none())
                    .and_then(shared_path),
                what: context.what.as_ref().map(|what| match &paths {
                    Some(paths) => paths_from_action(what, paths),
                    None => without_shared_path(trimmed(what)),
                }),
                kind: context.kind.filter(|_| !undoable),
                purpose: context.purpose.clone(),
                group: context.group.clone(),
                facets: context.facets.clone(),
                invoker: context.invoker.clone(),
                source_invocation: context.source_invocation,
                signed: context.signed.clone(),
                caused_by: context.caused_by.clone(),
                action: context.action.as_ref().map(Action::of),
                compensation: context.compensation.as_ref().map(Action::of),
                gesture: context
                    .gesture
                    .map(|gesture| gesture.0.simple().to_string()),
                change: context.change,
                shows: context.shows.clone(),
            }
        }

        pub(super) fn into_context(self) -> ActionContext {
            let action = self.action.and_then(Action::into_encoded);
            let resource = self.resource.map(Resource::into_resource);
            ActionContext {
                app: self
                    .app
                    .map_or_else(|| inferred(action.as_ref()), App::into_app),
                resource: resource.clone(),
                item: self.item.map(|item| item.into_item(resource.as_ref())),
                what: self.what.map(|what| {
                    // A shared path is written only when the action does not say it.
                    let paths = self
                        .shared_path
                        .is_none()
                        .then(|| action_paths(action.as_ref(), &what))
                        .flatten();
                    match paths {
                        Some(paths) => filled(paths_given_back(what, &paths)),
                        None => filled(with_shared_path(what, self.shared_path.as_deref())),
                    }
                }),
                kind: self
                    .kind
                    .or_else(|| action.as_ref().map(|_| ActionKind::Undoable)),
                purpose: self.purpose,
                group: self.group,
                facets: self.facets,
                invoker: self.invoker,
                source_invocation: self.source_invocation,
                signed: self.signed,
                caused_by: self.caused_by,
                compensation: self.compensation.and_then(Action::into_encoded),
                gesture: self
                    .gesture
                    .and_then(|gesture| uuid::Uuid::try_parse(&gesture).ok())
                    .map(crate::ActionId),
                change: self.change,
                shows: self.shows,
                action,
                approval: None,
            }
        }
    }
}

/// The longest label of a work group, in characters ([`one_line`]).
pub const MAX_LABEL_CHARS: usize = 96;

/// `text` as one line people read: its first line with words on it, without
/// control characters, at most `max` characters, trimmed. `None` when it holds
/// no words.
#[must_use]
pub fn one_line(text: &str, max: usize) -> Option<String> {
    let line: String = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())?
        .chars()
        .filter(|character| !character.is_control())
        .take(max)
        .collect();
    nonempty(&line)
}

fn nonempty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Upper bound on a [`PresenceId`] in bytes. Presences key bounded
/// per-view maps in app workers; they are never free text.
pub const MAX_PRESENCE_ID_BYTES: usize = 128;

/// One presence in a shared record: a person's window or a host-minted
/// execution. Each presence owns its own view state (caret, selection,
/// Find), and the app builds that presence's resource tree from it
/// (the toolkit's `PublishesResources::resources_for`).
///
/// Opaque, non-empty, ASCII-printable and at most
/// [`MAX_PRESENCE_ID_BYTES`] bytes. It is a routing key for view state,
/// never authority: the authenticated actor is [`ActionInvoker::acting`].
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct PresenceId(String);

impl PresenceId {
    /// Validate and wrap a presence id.
    pub fn new(id: impl Into<String>) -> Result<Self, String> {
        let id = id.into();
        if id.is_empty()
            || id.len() > MAX_PRESENCE_ID_BYTES
            || !id.bytes().all(|b| b.is_ascii_graphic())
        {
            return Err(format!(
                "presence id must be 1..={MAX_PRESENCE_ID_BYTES} printable ASCII bytes"
            ));
        }
        Ok(Self(id))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// One host-minted execution's private view. This key is subordinate to the
    /// admitted actor and confers no authority. Hosts derive it from their
    /// execution lease, never a client-supplied run label or context.
    pub fn for_execution(execution: uuid::Uuid) -> Self {
        Self(format!("execution:{execution}"))
    }
}

impl TryFrom<String> for PresenceId {
    type Error = String;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<PresenceId> for String {
    fn from(value: PresenceId) -> Self {
        value.0
    }
}

impl std::fmt::Display for PresenceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The longest name of a device kept with an invoker, in characters.
const MAX_DEVICE_NAME_CHARS: usize = 64;

/// Another of its person's devices that an agent works from: the device the
/// MCP server that admitted the agent's connection verified it came from, by
/// the certificate the person's root signed for it.
///
/// It says where the agent runs, and grants nothing: who the agent is and
/// what it may do is [`ActionInvoker::acting`] and the admission behind it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct InvokerDevice {
    /// The device's public key, in lowercase hex.
    pub id: String,
    /// What the device's certificate calls it ("Studio laptop"); empty when
    /// it gives the device no name.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
}

impl InvokerDevice {
    /// The device of the key `key`, called `label` by the certificate an API
    /// verified for it.
    #[must_use]
    pub fn certified(key: [u8; 32], label: &str) -> Self {
        Self {
            id: hex::encode(key),
            name: label
                .trim()
                .chars()
                .filter(|character| !character.is_control())
                .take(MAX_DEVICE_NAME_CHARS)
                .collect(),
        }
    }

    /// The start of the device's key: what tells two devices of one name
    /// apart, and what names a device whose certificate names none.
    #[must_use]
    pub fn short_id(&self) -> &str {
        self.id.get(..8).unwrap_or(&self.id)
    }

    /// The device as a person reads it: its name, or the start of its key
    /// when it has none.
    #[must_use]
    pub fn label(&self) -> &str {
        if self.name.is_empty() {
            self.short_id()
        } else {
            &self.name
        }
    }
}

/// The invoker of one action: the authenticated acting tree, and optionally
/// the presence whose view the invocation resolves against.
///
/// `presence: None` means the actor's single/default view (the local
/// window for a person; the only view of a single-view app).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ActionInvoker {
    pub acting: Acting,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence: Option<PresenceId>,
    /// The name and avatar a bot showed for this action. Set only by the
    /// API that admitted the invocation, and only for a bot allowed it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub appearance: Option<crate::Appearance>,
    /// The name an agent goes by: the name its harness gave the session it works
    /// in ("Release prep"), else the one the host that admitted it knows from the
    /// key its person certified for it ("Jane Doe's agent"). What History and a
    /// message's byline name it by; display only, never authority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The other device of its person's an agent works from, when it came to
    /// this host through its MCP server. Set only by the host that admitted
    /// the connection, from the device certificate the API verified, and
    /// never taken from a request. `None` for an actor on the host's own
    /// device.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<InvokerDevice>,
}

impl ActionInvoker {
    /// The actor the host admitted.
    #[must_use]
    pub fn admitted(acting: &Acting) -> Self {
        Self {
            acting: acting.clone(),
            presence: None,
            appearance: None,
            name: None,
            device: None,
        }
    }

    /// A member acting for themselves.
    #[must_use]
    pub fn member(member: tilo_action_policy::MemberId) -> Self {
        Self::admitted(&Acting::member(member))
    }

    /// A person the host authenticated, acting for themselves.
    #[track_caller]
    #[must_use]
    pub fn person(id: impl Into<tilo_action_policy::PersonId>) -> Self {
        Self::admitted(&Acting::person(id))
    }

    #[must_use]
    pub fn agent(id: impl Into<String>) -> Self {
        Self::admitted(&Acting::agent(id))
    }

    #[must_use]
    pub fn bot(id: impl Into<String>) -> Self {
        Self::admitted(&Acting::bot(id))
    }

    #[must_use]
    pub fn automation(id: impl Into<String>) -> Self {
        Self::admitted(&Acting::automation(id))
    }

    /// The view the invocation resolves against: its presence, else the member's default view.
    /// `None` for an automation, which has no view.
    #[must_use]
    pub fn view(&self) -> Option<PresenceId> {
        if let Some(presence) = &self.presence {
            return Some(presence.clone());
        }
        let member = self.acting.by.member_id()?.actor_bytes();
        PresenceId::new(String::from_utf8(member).ok()?).ok()
    }

    #[must_use]
    pub fn with_presence(mut self, presence: PresenceId) -> Self {
        self.presence = Some(presence);
        self
    }

    /// The name the admitting host knows this agent by ([`Self::name`]).
    #[must_use]
    pub fn named(mut self, name: Option<String>) -> Self {
        self.name = name;
        self
    }

    /// The other device of its person's the agent works from
    /// ([`Self::device`]), as the API that admitted its connection verified
    /// it; `None` clears one, so an invoker copied from a caller's context
    /// never keeps a device no API checked.
    #[must_use]
    pub fn from_device(mut self, device: Option<InvokerDevice>) -> Self {
        self.device = device;
        self
    }

    /// The same invoker, for `person`. A person acting for themselves
    /// records nobody.
    #[must_use]
    pub fn acting_for(mut self, person: impl Into<tilo_action_policy::PersonId>) -> Self {
        let person = person.into();
        if person.trim().is_empty() || self.acting.person_id() == Some(person.as_str()) {
            self.acting.acted_for.clear();
        } else {
            self.acting.acted_for = vec![Acting::person(person)];
        }
        self
    }

    /// The appearance the API admitted for this one invocation; `None`
    /// clears one, so an invoker copied from a caller's context never keeps
    /// what no API checked.
    #[must_use]
    pub fn with_appearance(mut self, appearance: Option<crate::Appearance>) -> Self {
        self.appearance = appearance;
        self
    }

    /// The invoker an application worker may act on for one delivered
    /// invocation. `actor` is the actor the host authenticated and put on
    /// the delivery; `context` is the host-built provenance of the same
    /// delivery. The presence, the person it acts for and the device it
    /// works from are kept only when the context names this exact actor, or an
    /// automation acting for exactly that actor. A context the host did not rewrite for this actor
    /// therefore never routes to anybody's view.
    pub fn delivered(actor: &Actor, context: Option<&ActionContext>) -> Self {
        let invoker = context.and_then(|context| context.invoker.as_ref());
        // An automation's step is delivered as the person it runs for; the
        // host names the automation acting for exactly that person.
        if let Some(automation) = invoker.filter(|invoker| {
            invoker.acting.is_automation()
                && invoker.acting.acted_for.first().map(|node| &node.by) == Some(actor)
        }) {
            return automation.clone();
        }
        match invoker.filter(|invoker| invoker.acting.by == *actor) {
            Some(named) => named.clone(),
            None => Self::admitted(&Acting::from(actor.clone())),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionApp {
    /// Stable package/application id.
    pub id: String,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionResource {
    pub id: String,
    pub kind: String,
    pub label: String,
}

/// The run of a bot that made a change, and how deep in a chain of runs it
/// is: a run fired by a change that itself came from a run is one deeper.
/// Correlation only; it grants nothing. The dispatcher fires nothing past
/// a depth of four, so two bots answering each other stop.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Cause {
    pub installation: String,
    /// The firing change's hash (hex) or the connector event's id.
    pub event: String,
    pub depth: u8,
}

/// The [`WorkGroup::kind`] of an agent session: every action one connection of
/// an agent (or a key it holds) takes, which history keeps, lists in Activity
/// and undoes whole.
pub const AGENT_SESSION_GROUP: &str = "agent-session";

/// The group kind of one command line a shell ran: every file action it and
/// the commands it dispatched carried out, undone as one.
pub const COMMAND_GROUP: &str = "command";

/// Whether a group of this kind names a session that undo can name: an agent's
/// connection, or one command line of a shell.
#[must_use]
pub fn names_a_session(kind: &str) -> bool {
    kind == AGENT_SESSION_GROUP || kind == COMMAND_GROUP
}

/// The unit of work an action belongs to: an issue, a task, an agent run.
/// Not a group of actions applied as one; that is `tilo_resource::ActionGroup`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkGroup {
    /// Stable key shared by every action in the work unit.
    pub id: String,
    /// What the work is called, for people to read: an issue's title, what an
    /// agent session was asked to do, or the title the agent gave its work
    /// ("Fill Jane's application"). Never the group itself, which a host
    /// decides on its own. `None` when nothing says so.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Open vocabulary: `issue`, `task`, `agent-run`, `session`, …
    pub kind: String,
    /// The id the caller itself gave the work, when it named one: the run an
    /// agent says each of its calls is a step of. A host's own [`Self::id`]
    /// may span several of them (every call of one connection), so this is
    /// what tells the caller's runs apart in history; never the group itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declared_id: Option<String>,
    /// The session's own name, as the client's harness names it ("Office
    /// Hours"): what an agent that works through a connection of its own is
    /// shown by, where the session is what people see it as. Display only,
    /// like the label; `None` for work that names no session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
}

/// [`ActionFacet::key`] of the risk ([`crate::RiskLevel::name`]) of the
/// action a context describes, for a question about it put to a person.
pub const RISK_FACET: &str = "risk";

/// [`ActionFacet::key`] of where in its app's record a step happened, as
/// everyone the record is shared with sees that place named: a chat post's
/// channel, `#general`. Only such a place is named; a direct conversation
/// never is.
pub const PLACE_FACET: &str = "place";

/// A filterable activity dimension. Open key/value vocabulary lets future
/// apps add facets without changing the wire enum.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionFacet {
    /// e.g. `path`, `directory`, `file-type`, `mime`, `repository`.
    pub key: String,
    pub value: String,
}

impl ActionFacet {
    pub fn new(key: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::{EditOp, FieldEdit, PathStep, Scalar};
    use crate::key::ActionKey;
    use tilo_action_policy::MemberId;

    fn laptop() -> InvokerDevice {
        InvokerDevice::certified([0x5c; 32], "  Studio\u{7} laptop ")
    }

    fn starred() -> Described {
        Described::new(Did::Edit(FieldEdit {
            ty: "snippet".into(),
            field: "favorite".into(),
            path: vec![
                PathStep::Field("snippets".into()),
                PathStep::Item("s1".into()),
                PathStep::Field("favorite".into()),
            ],
            op: EditOp::Set,
            old: Some(false.into()),
            new: Some(true.into()),
            args: std::collections::BTreeMap::new(),
        }))
        .on("s1", "Deploy script")
    }

    /// The entry `message` carries, as JSON text.
    fn written(message: &str) -> String {
        let entry = message.strip_prefix(CHANGE_MESSAGE_PREFIX).unwrap();
        crate::compact::decode(entry).unwrap()
    }

    tilo_test_env::scenario! {
        fn the_host_states_the_invoker;

        phase the_host_rewriting_the_invoker_drops_an_appearance_the_caller_put_there;
        {
            let smuggled = ActionContext::default().with_invoker(
                ActionInvoker::person("mallory")
                    .with_appearance(Some(crate::Appearance::new("Ada", None, None).unwrap())),
            );
            let hosted = smuggled.with_host_actor(&crate::Acting::person("mallory"), None);
            assert_eq!(hosted.invoker.unwrap().appearance, None);
        }

        phase a_delivered_invoker_keeps_the_appearance_only_for_the_named_actor;
        {
            let appearance = crate::Appearance::new("alice", None, None).unwrap();
            let context = ActionContext::default().with_invoker(
                ActionInvoker::bot("7f1c").with_appearance(Some(appearance.clone())),
            );
            assert_eq!(
                ActionInvoker::delivered(&MemberId::bot("7f1c").into(), Some(&context)).appearance,
                Some(appearance)
            );
            assert_eq!(
                ActionInvoker::delivered(&MemberId::person("b").into(), Some(&context)).appearance,
                None
            );
        }

        phase a_device_is_named_by_its_certificate_or_the_start_of_its_key;
        {
            let laptop = laptop();
            assert_eq!(laptop.id, "5c".repeat(32));
            assert_eq!(
                laptop.name, "Studio laptop",
                "trimmed, no control characters"
            );
            assert_eq!(laptop.short_id(), "5c5c5c5c");
            assert_eq!(laptop.label(), "Studio laptop");
            let unnamed = InvokerDevice::certified([0x40; 32], " ");
            assert_eq!(unnamed.label(), "40404040");
            let long = InvokerDevice::certified([1; 32], &"x".repeat(200));
            assert_eq!(long.name.chars().count(), MAX_DEVICE_NAME_CHARS);
        }

        phase the_host_rewriting_the_invoker_drops_a_device_the_caller_put_there;
        {
            let smuggled = ActionContext::default().with_invoker(
                ActionInvoker::agent("mallory").from_device(Some(laptop())),
            );
            let hosted = smuggled.with_host_actor(&crate::Acting::agent("mallory"), None);
            assert_eq!(hosted.invoker.unwrap().device, None);
        }

        phase a_delivered_invoker_keeps_the_device_only_for_the_named_actor;
        {
            let context = ActionContext::default().with_invoker(
                ActionInvoker::agent("task-7").from_device(Some(laptop())),
            );
            assert_eq!(
                ActionInvoker::delivered(&MemberId::agent("task-7").into(), Some(&context)).device,
                Some(laptop())
            );
            assert_eq!(
                ActionInvoker::delivered(&MemberId::agent("task-8").into(), Some(&context)).device,
                None,
                "another actor is not on that device because a context names it"
            );
            assert_eq!(ActionInvoker::delivered(&MemberId::agent("task-7").into(), None).device, None);
        }

        phase invoker_round_trips_and_is_enriched_from_ambient;
        {
            let invoker = ActionInvoker::agent("alpha")
                .with_presence(PresenceId::new("task-7").unwrap());
            let context = ActionContext::for_resource("r", "k", "l").with_invoker(invoker.clone());
            let message = context.change_message().unwrap();
            assert_eq!(
                ActionContext::from_change_message(&message),
                Some(context.clone())
            );
            let enriched = ActionContext::default().enriched_by(&context);
            assert_eq!(enriched.invoker, Some(invoker));
        }

        phase a_person_acting_for_themselves_records_nobody;
        {
            assert_eq!(
                ActionInvoker::person("ada").acting_for("ada").acting.acted_for_person(),
                None
            );
            let agent = ActionInvoker::agent("claude").acting_for("ada");
            assert!(agent.acting.is_agent());
            assert_eq!(agent.acting.acted_for_person(), Some("ada"));
        }

        phase host_invoker_overwrites_a_forged_envelope_invoker;
        {
            let forged = ActionContext::for_resource("r", "k", "l").with_invoker(
                ActionInvoker::person("owner")
                    .acting_for("other")
                    .with_presence(PresenceId::new("owner-window").unwrap()),
            );
            let task = PresenceId::new("agent-alpha-view").unwrap();
            let rewritten = forged
                .clone()
                .with_host_actor(&crate::Acting::agent("alpha"), Some(task.clone()));
            assert_eq!(
                rewritten.invoker,
                Some(ActionInvoker::agent("alpha").with_presence(task))
            );
            let delegated = forged.with_host_actor(
                &crate::Acting::agent("alpha").acting_for(crate::Acting::person("owner")),
                None,
            );
            let invoker = delegated.invoker.unwrap();
            assert!(invoker.acting.is_agent());
            assert_eq!(
                invoker.acting.acted_for_person(),
                Some("owner"),
                "the host records whom the admitted actor acts for"
            );
        }

        phase delivered_invoker_keeps_the_presence_only_for_the_delivered_actor;
        {
            let presence = PresenceId::new("agent-a-view").unwrap();
            let context =
                ActionContext::default()
                    .with_host_actor(&crate::Acting::agent("a"), Some(presence.clone()));
            assert_eq!(
                ActionInvoker::delivered(&MemberId::agent("a").into(), Some(&context)),
                ActionInvoker::agent("a").with_presence(presence)
            );
            // A context naming another actor never routes to a view, and the
            // actor always comes from the delivery.
            assert_eq!(
                ActionInvoker::delivered(&MemberId::person("b").into(), Some(&context)),
                ActionInvoker::person("b")
            );
            assert_eq!(
                ActionInvoker::delivered(&MemberId::person("b").into(), None),
                ActionInvoker::person("b")
            );
            // A Shortcut step is delivered as its person; the Shortcut acting for
            // exactly that person is kept, for anybody else it is not.
            let shortcut = ActionInvoker::automation("morning").acting_for("b");
            let run = ActionContext::default().with_invoker(shortcut.clone());
            assert_eq!(ActionInvoker::delivered(&MemberId::person("b").into(), Some(&run)), shortcut);
            assert_eq!(
                ActionInvoker::delivered(&MemberId::person("c").into(), Some(&run)),
                ActionInvoker::person("c")
            );
        }

        phase presence_ids_are_bounded;
        {
            assert!(PresenceId::new("").is_err());
            assert!(PresenceId::new("has space").is_err());
            assert!(PresenceId::new("x".repeat(MAX_PRESENCE_ID_BYTES + 1)).is_err());
            assert!(PresenceId::new("x".repeat(MAX_PRESENCE_ID_BYTES)).is_ok());
            assert!(serde_json::from_str::<PresenceId>("\"\"").is_err());
        }
    }

    tilo_test_env::scenario! {
        fn a_context_reads_back_from_its_change_message;

        phase an_appearance_round_trips_through_the_change_message;
        {
            let appearance = crate::Appearance::new(
                "alice",
                None,
                Some(crate::Persona {
                    service: "discord".into(),
                    account: "80351110224678912".into(),
                    linked: None,
                }),
            )
            .unwrap();
            let entry = ActionContext::default().with_invoker(
                ActionInvoker::bot("7f1c")
                    .acting_for("ada")
                    .with_appearance(Some(appearance.clone())),
            );
            let read = ActionContext::from_change_message(&entry.change_message().unwrap()).unwrap();
            assert_eq!(read, entry);
            assert_eq!(read.invoker.unwrap().appearance, Some(appearance));
            let plain = ActionContext::default().with_invoker(ActionInvoker::person("ada"));
            let json = serde_json::to_string(&plain.invoker).unwrap();
            assert!(!json.contains("appearance"), "{json}");
        }

        phase the_device_an_agent_works_from_round_trips_through_the_change_message;
        {
            let entry = ActionContext::default().with_invoker(
                ActionInvoker::agent("task-7")
                    .acting_for("ada")
                    .from_device(Some(laptop())),
            );
            let read = ActionContext::from_change_message(&entry.change_message().unwrap()).unwrap();
            assert_eq!(read, entry);
            assert_eq!(read.invoker.unwrap().device, Some(laptop()));
            // An actor on the host's own device says nothing of a device.
            let local = ActionContext::default().with_invoker(ActionInvoker::agent("task-8"));
            let json = serde_json::to_string(&local.invoker).unwrap();
            assert!(!json.contains("device"), "{json}");
        }

        phase a_cause_survives_the_change_message;
        {
            let context = ActionContext::for_app("so.tilo.chat", "Chat").with_caused_by(Some(Cause {
                installation: "4a6c0c49-8a0d-4c3e-9d55-0a1b2c3d4e5f".into(),
                event: "ab".repeat(32),
                depth: 3,
            }));
            let message = context.change_message().unwrap();
            let read = ActionContext::from_change_message(&message).unwrap();
            assert_eq!(read.caused_by, context.caused_by);
            // A context without a cause writes no key for it.
            let plain = ActionContext::for_app("so.tilo.chat", "Chat")
                .change_message()
                .unwrap();
            assert!(
                ActionContext::from_change_message(&plain)
                    .unwrap()
                    .caused_by
                    .is_none()
            );
        }

        phase entries_round_trip_through_the_change_message;
        {
            let entry = ActionContext::for_resource("lib", "snippets.library", "Snippets")
                .with_app("org.tilo.Snippets", "Snippets")
                .with_described(starred())
                .with_group("run:7", "Tidy snippets", "agent-session")
                .with_invoker(
                    ActionInvoker::agent("claude").acting_for("ada"),
                )
                .with_source_invocation(Some(InvocationId(uuid::Uuid::from_u128(42))));
            let message = entry.change_message().unwrap();
            let read = ActionContext::from_change_message(&message).unwrap();
            assert_eq!(read, entry);
            assert!(read.acting().unwrap().is_agent());
            assert_eq!(
                read.acting().unwrap().acted_for_person(),
                Some("ada")
            );
            assert_eq!(read.described(), Some(starred()));

            let verb = ActionContext::default().with_described(Described::new(Did::verb(
                ActionKey::new("snippets", "snippets.library", "new-snippet"),
                [("title".to_string(), Scalar::from("Deploy"))],
            )));
            let read = ActionContext::from_change_message(&verb.change_message().unwrap()).unwrap();
            assert_eq!(read, verb);
        }

        phase an_entry_leaves_out_what_its_reader_derives_and_reads_back_the_same;
        {
            let action = crate::EncodedAction {
                type_id: "word-processor.edit".into(),
                payload: br#"{"op":"type","text":"a"}"#.to_vec(),
            };
            let entry = ActionContext {
                what: Some(Did::Edit(crate::FieldEdit {
                    ty: "document".into(),
                    field: "body".into(),
                    path: vec![PathStep::Field("body".into())],
                    op: crate::EditOp::Splice,
                    old: None,
                    new: None,
                    args: std::collections::BTreeMap::new(),
                })),
                kind: Some(ActionKind::Undoable),
                action: Some(action),
                gesture: Some(crate::ActionId::new()),
                ..ActionContext::for_resource("doc:1", "word-processor.document", "Plan")
                    .with_inferred_app("word-processor.edit")
            };
            let message = entry.change_message().unwrap();
            // The app the action's type names, an undoable kind and a path that
            // is just the field are left out; the payload is inline JSON.
            for derived in ["org.tilo.WordProcessor", "undoable", "path", "[123,"] {
                assert!(!message.contains(derived), "{derived} in {message}");
            }
            assert!(message.len() < 230, "{} B: {message}", message.len());
            let read = ActionContext::from_change_message(&message).unwrap();
            assert_eq!(read, entry);
            assert_eq!(
                read.change_message().unwrap(),
                message,
                "written again the same"
            );
            // No app at all stays no app.
            let bare = ActionContext { app: None, ..entry };
            let read = ActionContext::from_change_message(&bare.change_message().unwrap()).unwrap();
            assert_eq!(read, bare);
        }

        phase a_group_writes_what_its_members_share_once;
        {
            let path = vec![
                PathStep::Field("sheets".into()),
                PathStep::Item("s1".into()),
                PathStep::Field("cells".into()),
            ];
            let edit = |op| {
                Did::Edit(FieldEdit {
                    ty: "sheet".into(),
                    field: "cells".into(),
                    path: path.clone(),
                    op,
                    old: None,
                    new: None,
                    args: std::collections::BTreeMap::new(),
                })
            };
            let mut entry =
                ActionContext::for_resource("sheets.workbook", "sheets.workbook", "Workbook");
            entry.what = Some(Did::group([edit(EditOp::InsertRow), edit(EditOp::SetCell)]));
            let message = entry.change_message().unwrap();
            assert!(!message.contains("distinct"), "{message}");
            assert_eq!(
                written(&message).matches("\"cells\"").count(),
                3,
                "the path once: {message}"
            );
            let read = ActionContext::from_change_message(&message).unwrap();
            assert_eq!(read, entry);
            assert_eq!(
                read.change_message().unwrap(),
                message,
                "written again the same"
            );
        }

        phase an_edit_takes_its_path_from_the_action_it_carries;
        {
            let path = vec![
                PathStep::Field("pages".into()),
                PathStep::Item("p1".into()),
                PathStep::Field("body".into()),
            ];
            let mut entry = ActionContext::for_resource("notes.notebook", "notes.notebook", "Notebook");
            entry.what = Some(Did::Edit(FieldEdit {
                ty: "page".into(),
                field: "body".into(),
                path,
                op: EditOp::Splice,
                old: None,
                new: None,
                args: std::collections::BTreeMap::new(),
            }));
            entry.action = Some(crate::EncodedAction {
                type_id: "notes.notebook".into(),
                payload: br#"{"op":{"action":"field-edit","args":{"op":"rich","path":[{"field":"pages"},{"item":"p1"},{"field":"body"}]}},"at":0}"#.to_vec(),
            });
            entry.kind = Some(ActionKind::Undoable);
            let message = entry.change_message().unwrap();
            assert_eq!(
                written(&message).matches("\"pages\"").count(),
                1,
                "the path once: {message}"
            );
            assert!(
                message.len() * 10 < written(&message).len() * 7,
                "the compact form is well under the JSON one: {message}"
            );
            let read = ActionContext::from_change_message(&message).unwrap();
            assert_eq!(read, entry);
            assert_eq!(
                read.change_message().unwrap(),
                message,
                "written again the same"
            );
        }

        phase an_item_named_by_its_resource_writes_its_id_once;
        {
            let mut entry =
                ActionContext::for_resource("notes.notebook/pages/p1", "tilo.rich-text", "Document");
            entry.item = Some(ActionItem {
                id: "p1".into(),
                label: "Plans".into(),
            });
            let message = entry.change_message().unwrap();
            assert_eq!(message.matches("p1").count(), 1, "the id once: {message}");
            let read = ActionContext::from_change_message(&message).unwrap();
            assert_eq!(read, entry);
            assert_eq!(
                read.change_message().unwrap(),
                message,
                "written again the same"
            );
            // An item the resource does not name keeps its id.
            entry.item = Some(ActionItem {
                id: "p2".into(),
                label: "Other".into(),
            });
            let read = ActionContext::from_change_message(&entry.change_message().unwrap()).unwrap();
            assert_eq!(read, entry);
        }

        phase an_invoker_written_in_an_older_form_keeps_the_rest_of_its_entry;
        {
            let context = ActionContext::for_resource("r", "k", "l");
            let mut json: serde_json::Value =
                serde_json::from_str(&written(&context.change_message().unwrap())).unwrap();
            json["by"] = serde_json::json!({ "actor": "ada" });
            let message = format!("{CHANGE_MESSAGE_PREFIX}{}", crate::compact::encode(&json));
            assert_eq!(ActionContext::from_change_message(&message), Some(context));
        }

        phase defaults_are_not_written;
        {
            let context = ActionContext::for_resource("r", "k", "l");
            let message = context.change_message().unwrap();
            let json: serde_json::Value = serde_json::from_str(&written(&message)).unwrap();
            let written: Vec<&str> = json
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            assert_eq!(written, ["r"], "{message}");
            assert_eq!(ActionContext::from_change_message(&message), Some(context));
            let outside = ActionContext::default().with_change(ChangeKind::External);
            let mut outside = outside;
            outside.kind = Some(ActionKind::Outside);
            let read = ActionContext::from_change_message(&outside.change_message().unwrap()).unwrap();
            assert_eq!(read.kind, Some(ActionKind::Outside));
            assert!(
                !read.is_activity(),
                "what came from outside is not activity"
            );
        }

        phase housekeeping_is_carried;
        {
            let content = ActionContext::for_resource("r", "k", "l");
            let marker = content.clone().with_change(ChangeKind::Housekeeping);
            let message = marker.change_message().unwrap();
            let read = ActionContext::from_change_message(&message).unwrap();
            assert!(read.is_housekeeping());
            assert_eq!(read, marker);
            let bare = ActionContext::default().with_change(ChangeKind::Housekeeping);
            assert!(
                bare.change_message().is_some(),
                "housekeeping alone is still written"
            );
            assert!(
                !ActionContext::from_change_message(&content.change_message().unwrap())
                    .unwrap()
                    .is_housekeeping()
            );
        }

        phase unrelated_or_malformed_messages_are_not_entries;
        {
            assert!(ActionContext::from_change_message("saved draft").is_none());
            assert!(ActionContext::from_change_message("tilo.entry/1 {").is_none());
            assert!(ActionContext::default().change_message().is_none());
        }
    }

    tilo_test_env::scenario! {
        fn ambient_context_fills_what_the_action_leaves_open;

        phase generic_palette_context_infers_stable_app_ownership;
        {
            let context = ActionContext::for_resource("doc:1", "document", "Brief")
                .with_inferred_app("word-processor.action");
            assert_eq!(
                context.app,
                Some(ActionApp {
                    id: "org.tilo.WordProcessor".into(),
                    label: "Word Processor".into(),
                })
            );
        }

        phase ambient_context_fills_intent_without_overwriting_the_action;
        {
            let action = ActionContext::for_resource("file:1", "file", "brief.md")
                .with_app("org.tilo.Code", "Code");
            let ambient = ActionContext::default()
                .with_purpose("Prepare launch")
                .with_group("issue:42", "Launch", "issue")
                .with_app("wrong", "Wrong");
            let merged = action.enriched_by(&ambient);
            assert_eq!(
                merged.app.as_ref().map(|app| app.id.as_str()),
                Some("org.tilo.Code")
            );
            assert_eq!(merged.purpose.as_deref(), Some("Prepare launch"));
            assert_eq!(
                merged.group.as_ref().map(|group| group.id.as_str()),
                Some("issue:42")
            );
        }
    }

    #[test]
    fn a_keystroke_continues_its_writers_last_entry_with_its_gesture_bytes_and_claim() {
        let typed = |letter: &str, gesture: u128| ActionContext {
            action: Some(crate::EncodedAction {
                type_id: "notes.notebook".into(),
                payload: format!(
                    r#"{{"SpliceText":{{"at":12,"ins":{letter}}},"path":[{{"item":"8dc4"}}]}}"#
                )
                .into_bytes(),
            }),
            ..ActionContext::default()
                .with_described(starred())
                .in_gesture(Some(crate::ActionId(uuid::Uuid::from_u128(gesture))))
        };
        // The next word's first letter starts its own gesture.
        let (first, next) = (typed(r#""n""#, 7), typed(r#""é \"q\"""#, 8));
        let message = next.continuation_of(&first).unwrap();
        assert_eq!(
            message,
            r#"tilo.cont/3 U8000000000000000000000 e::fsnippets/is1/ffavorite 30 28 é \"q\""#
        );
        assert_eq!(
            ContinuedAction::of_message(&message),
            Some(ContinuedAction::Edit {
                kind: String::new(),
                path: vec![
                    PathStep::Field("snippets".into()),
                    PathStep::Item("s1".into()),
                    PathStep::Field("favorite".into()),
                ],
            })
        );
        assert_eq!(
            ActionContext::continued(&message, &first),
            Some(next),
            "read back byte for byte"
        );
    }
}
