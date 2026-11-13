//! `ActionProposal`: one model call's answer, shown before it is written.

use serde::{Deserialize, Serialize};
use tilo_resource::{Edit, EditOp, Path, Segment};

mod view;

pub use view::ActionProposalView;

/// Most bytes one part of a context may carry: past it the person is sending
/// a whole document rather than a selection, and is told so before it goes.
pub const MAX_CONTEXT_BYTES: usize = 32 * 1024;

/// Most parts a context may send.
pub const MAX_CONTEXT_PARTS: usize = 8;

pub const MAX_DIFF_LINES: usize = 80;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ModelPlace {
    OnDevice,
    Hosted,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelPass {
    pub names: String,
    pub place: ModelPlace,
}

impl ModelPass {
    pub fn on_device(names: impl Into<String>) -> Self {
        Self {
            names: names.into(),
            place: ModelPlace::OnDevice,
        }
    }

    pub fn hosted(names: impl Into<String>) -> Self {
        Self {
            names: names.into(),
            place: ModelPlace::Hosted,
        }
    }

    pub fn placed(&self) -> String {
        let key = match self.place {
            ModelPlace::OnDevice => "proposal-model-on-device",
            ModelPlace::Hosted => "proposal-model-hosted",
        };
        let mut args = fluent::FluentArgs::new();
        args.set("model", self.names.as_str());
        crate::i18n::I18nManager::lookup(key, Some(&args))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextPart {
    pub name: String,
    pub text: String,
}

impl ContextPart {
    pub fn new(name: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            text: text.into(),
        }
    }

    pub fn titled(&self) -> String {
        titled(&self.name)
    }
}

/// The name a title or a part is read under; the key itself when the
/// catalogue has nothing for it, so a missing entry never hides a part.
pub fn titled(key: &str) -> String {
    if crate::i18n::I18nManager::has_message(key) {
        crate::i18n::I18nManager::lookup(key, None)
    } else {
        key.to_owned()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProposalContext {
    pub instruction: Option<ContextPart>,
    /// The parts, in the order they are sent.
    pub parts: Vec<ContextPart>,
}

impl ProposalContext {
    pub fn new(instruction: Option<ContextPart>, parts: Vec<ContextPart>) -> Self {
        Self { instruction, parts }
    }

    pub fn of(parts: Vec<ContextPart>) -> Self {
        Self::new(None, parts)
    }

    /// The prompt a model is sent: each part under its name, in this order,
    /// and nothing else.
    pub fn prompt(&self) -> String {
        let mut out = String::new();
        for part in self.instruction.iter().chain(&self.parts) {
            if !out.is_empty() {
                out.push_str("\n\n");
            }
            out.push_str(&part.titled());
            out.push_str(":\n");
            out.push_str(&part.text);
        }
        out
    }

    /// The bytes of text this sends.
    pub fn bytes(&self) -> usize {
        self.parts
            .iter()
            .fold(0, |sum, part| sum.saturating_add(part.text.len()))
    }

    /// Why this may not be sent, in the words the person reads: a part past
    /// [`MAX_CONTEXT_BYTES`], or more than [`MAX_CONTEXT_PARTS`] parts.
    /// `None` when it may.
    pub fn refusal(&self) -> Option<String> {
        if self.parts.len() > MAX_CONTEXT_PARTS {
            let mut args = fluent::FluentArgs::new();
            args.set(
                "limit",
                i64::try_from(MAX_CONTEXT_PARTS).unwrap_or(i64::MAX),
            );
            return Some(crate::i18n::I18nManager::lookup(
                "proposal-context-too-many",
                Some(&args),
            ));
        }
        self.parts
            .iter()
            .find(|part| part.text.len() > MAX_CONTEXT_BYTES)
            .map(|part| {
                let mut args = fluent::FluentArgs::new();
                args.set("name", part.titled());
                args.set(
                    "limit",
                    i64::try_from(MAX_CONTEXT_BYTES / 1024).unwrap_or(i64::MAX),
                );
                crate::i18n::I18nManager::lookup("proposal-context-too-long", Some(&args))
            })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DiffTag {
    Same,
    /// Only in the answer.
    Added,
    /// Only in the original.
    Removed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiffLine {
    pub tag: DiffTag,
    pub text: String,
}

/// The lines of `before` and `after`: the ones both share once, and the ones
/// only one of them has, in the order they stand — a removed line before the
/// added one that took its place.
///
/// Lines are compared as they are; nothing is inferred about what moved.
pub fn line_diff(before: &str, after: &str) -> Vec<DiffLine> {
    let old: Vec<&str> = before.lines().collect();
    let new: Vec<&str> = after.lines().collect();
    // The longest common subsequence, tabulated by suffix, so a line met
    // again later is kept where it is instead of removed and added again.
    let mut shared = vec![vec![0usize; new.len().saturating_add(1)]; old.len().saturating_add(1)];
    for i in (0..old.len()).rev() {
        for j in (0..new.len()).rev() {
            let skip = shared[i + 1][j].max(shared[i][j + 1]);
            let take = if old[i] == new[j] {
                shared[i + 1][j + 1].saturating_add(1)
            } else {
                0
            };
            shared[i][j] = skip.max(take);
        }
    }
    let mut lines = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < old.len() && j < new.len() {
        let (tag, text) = if old[i] == new[j] {
            i = i.saturating_add(1);
            j = j.saturating_add(1);
            (DiffTag::Same, old[i.saturating_sub(1)])
        } else if shared[i + 1][j] >= shared[i][j + 1] {
            i = i.saturating_add(1);
            (DiffTag::Removed, old[i.saturating_sub(1)])
        } else {
            j = j.saturating_add(1);
            (DiffTag::Added, new[j.saturating_sub(1)])
        };
        lines.push(DiffLine {
            tag,
            text: text.to_owned(),
        });
    }
    for line in old.get(i..).unwrap_or_default() {
        lines.push(DiffLine {
            tag: DiffTag::Removed,
            text: (*line).to_owned(),
        });
    }
    for line in new.get(j..).unwrap_or_default() {
        lines.push(DiffLine {
            tag: DiffTag::Added,
            text: (*line).to_owned(),
        });
    }
    lines
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ProposalChange {
    Text {
        original: String,
        proposed: String,
    },
    /// Any other value of a resource: what it is now, and what it would be.
    Value {
        before: String,
        after: String,
    },
}

impl ProposalChange {
    pub fn text(original: impl Into<String>, proposed: impl Into<String>) -> Self {
        Self::Text {
            original: original.into(),
            proposed: proposed.into(),
        }
    }

    pub fn value(before: impl Into<String>, after: impl Into<String>) -> Self {
        Self::Value {
            before: before.into(),
            after: after.into(),
        }
    }

    pub fn proposed(&self) -> &str {
        match self {
            Self::Text { proposed, .. } => proposed,
            Self::Value { after, .. } => after,
        }
    }

    pub fn diff(&self) -> Vec<DiffLine> {
        match self {
            Self::Text { original, proposed } => line_diff(original, proposed),
            Self::Value { before, after } => vec![
                DiffLine {
                    tag: DiffTag::Removed,
                    text: before.clone(),
                },
                DiffLine {
                    tag: DiffTag::Added,
                    text: after.clone(),
                },
            ],
        }
    }

    /// Whether accepting would change anything.
    pub fn changes_anything(&self) -> bool {
        match self {
            Self::Text { original, proposed } => original != proposed,
            Self::Value { before, after } => before != after,
        }
    }
}

/// Where an accepted proposal writes: the field of the record `R` the answer
/// belongs to, and the characters of it the answer replaces.
///
/// `T` is the field's own value type, so a [`ProposalChange::Value`] answer
/// is written as that field is stored — a boolean, a number, a choice's key —
/// and a [`ProposalChange::Text`] answer splices the characters of a text.
#[derive(Debug, PartialEq, Eq)]
pub struct ProposalTarget<R: tilo_resource::Resource, T = String> {
    pub path: Path<R, T>,
    pub range: Option<std::ops::Range<usize>>,
}

impl<R: tilo_resource::Resource, T> Clone for ProposalTarget<R, T> {
    fn clone(&self) -> Self {
        Self {
            path: self.path.clone(),
            range: self.range.clone(),
        }
    }
}

impl<R: tilo_resource::Resource, T> ProposalTarget<R, T> {
    pub fn whole(path: Path<R, T>) -> Self {
        Self { path, range: None }
    }

    pub fn selection(path: Path<R, T>, range: std::ops::Range<usize>) -> Self {
        Self {
            path,
            range: Some(range),
        }
    }

    pub fn characters(&self, original: &str) -> (usize, usize) {
        match &self.range {
            Some(range) => (range.start, range.end.saturating_sub(range.start)),
            None => (0, original.chars().count()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProposalDecision {
    Accept { change: ProposalChange },
    Reject,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionProposal {
    pub title: String,
    pub note: Option<String>,
    pub context: ProposalContext,
    pub change: ProposalChange,
    pub model: ModelPass,
}

impl ActionProposal {
    pub fn new(
        title: impl Into<String>,
        context: ProposalContext,
        change: ProposalChange,
        model: ModelPass,
    ) -> Self {
        Self {
            title: title.into(),
            note: None,
            context,
            change,
            model,
        }
    }

    #[must_use]
    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        let note = note.into();
        self.note = (!note.is_empty()).then_some(note);
        self
    }

    pub fn titled(&self) -> String {
        titled(&self.title)
    }

    pub fn prompt(&self) -> String {
        self.context.prompt()
    }

    /// Why this may not be sent ([`ProposalContext::refusal`]).
    pub fn refusal(&self) -> Option<String> {
        self.context.refusal()
    }

    pub fn accept_action<R, T>(&self, target: ProposalTarget<R, T>) -> ProposalAccept<R>
    where
        R: tilo_resource::Resource,
        T: tilo_resource::Settable + serde::de::DeserializeOwned,
    {
        match &self.change {
            ProposalChange::Text { original, proposed } => {
                let (at, delete) = target.characters(original);
                let mut accept = self.accept_edit(Edit::new(
                    target.path.segments().to_vec(),
                    EditOp::Splice {
                        at,
                        delete,
                        insert: proposed.clone(),
                    },
                ));
                accept.selection = target.range.clone().map(|range| (range.start, range.end));
                accept.expecting(at, delete, original)
            }
            ProposalChange::Value { after, .. } => {
                match serde_json::from_str::<T>(after) {
                    Ok(value) => self.accept_edit(target.path.set(value)),
                    // An answer that is not the field's type is never
                    // written: the accept carries the refusal instead.
                    Err(error) => self
                        .accept_edit(Edit::new(
                            Vec::new(),
                            EditOp::Set {
                                value: serde_json::Value::Null,
                            },
                        ))
                        .refusing(error.to_string()),
                }
            }
        }
    }

    fn accept_edit<R: tilo_resource::Resource>(&self, edit: Edit<R>) -> ProposalAccept<R> {
        let field = edit.path().last().map(field_of).unwrap_or_default();
        // The action that carries the edit through the app's door; an edit
        // that does not encode is never written, and carries why.
        let (invoke, refusal) = match tilo_resource::Invoke::edit(&edit) {
            Ok(invoke) => (Some(invoke), None),
            Err(error) => (
                None,
                Some(format!("this answer cannot be written: {error}")),
            ),
        };
        ProposalAccept {
            edit,
            invoke,
            field,
            change: self.change.clone(),
            refusal,
            selection: None,
            expects: None,
        }
    }

    pub fn reject(&self) -> ProposalDecision {
        ProposalDecision::Reject
    }
}

pub struct ProposalAccept<R: tilo_resource::Resource> {
    edit: Edit<R>,
    invoke: Option<tilo_resource::Invoke<R>>,
    field: String,
    change: ProposalChange,
    refusal: Option<String>,
    selection: Option<(usize, usize)>,
    expects: Option<(std::ops::Range<usize>, String)>,
}

impl<R: tilo_resource::Resource> Clone for ProposalAccept<R> {
    fn clone(&self) -> Self {
        Self {
            edit: self.edit.clone(),
            invoke: self.invoke.clone(),
            field: self.field.clone(),
            change: self.change.clone(),
            refusal: self.refusal.clone(),
            selection: self.selection,
            expects: self.expects.clone(),
        }
    }
}

impl<R: tilo_resource::Resource> std::fmt::Debug for ProposalAccept<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProposalAccept")
            .field("path", &self.edit.path())
            .field("field", &self.field)
            .field("change", &self.change)
            .field("refusal", &self.refusal)
            .field("selection", &self.selection)
            .field("expects", &self.expects)
            .field("invoke", &self.invoke.as_ref().map(|invoke| invoke.id))
            .finish()
    }
}

impl<R: tilo_resource::Resource> ProposalAccept<R> {
    /// What this accept wrote, for a caller that synced the document itself.
    pub fn decision(&self) -> ProposalDecision {
        ProposalDecision::Accept {
            change: self.change.clone(),
        }
    }

    /// The field edit itself, for a caller that reads or records it.
    pub fn edit(&self) -> &Edit<R> {
        &self.edit
    }

    /// Why this accept writes nothing: the answer is not the field's own
    /// type, so it is refused rather than stored as something else.
    pub fn refusal(&self) -> Option<&str> {
        self.refusal.as_deref()
    }

    fn refusing(mut self, why: String) -> Self {
        self.refusal = Some(why);
        self
    }

    /// This accept, which only writes while the field still holds `original`
    /// at the characters it replaces.
    fn expecting(mut self, at: usize, delete: usize, original: &str) -> Self {
        if self.selection.is_some() {
            self.expects = Some((at..at.saturating_add(delete), original.to_owned()));
        }
        self
    }

    /// Why this accept must not be written over the field holding `now`, or
    /// `None` when it still writes what the model answered about.
    ///
    /// The characters an answer replaces are checked as the field stands: if
    /// the person typed inside or before them while the model ran, they moved,
    /// and the answer would land on letters it never saw. Refused, it is
    /// never written and the person keeps their text.
    pub fn refusal_over(&self, now: &str) -> Option<String> {
        if let Some(why) = self.refusal.as_deref() {
            return Some(why.to_owned());
        }
        let (range, expected) = self.expects.as_ref()?;
        let held: String = now
            .chars()
            .skip(range.start)
            .take(range.end.saturating_sub(range.start))
            .collect();
        (*held != *expected).then(|| expected.clone())
    }

    pub fn selection(&self) -> Option<(usize, usize)> {
        self.selection
    }

    /// The action to submit through the app's own door — `cx
    /// .dispatcher().submit(...)`, or `app.invoke(...)` in a test — which
    /// applies it as the record's one field edit: one change, one History
    /// row, one undo step, and the refusal the door gives when the field
    /// moved on while the model ran.
    ///
    /// # Errors
    ///
    /// Why the answer cannot be written at all — see
    /// [`Self::refusal`] — so a caller says it instead of writing.
    pub fn invoke(&self) -> Result<tilo_resource::Invoke<R>, String> {
        match (&self.invoke, self.refusal.as_deref()) {
            (Some(invoke), None) => Ok(invoke.clone()),
            (_, Some(why)) => Err(why.to_owned()),
            (None, None) => Err("this answer cannot be written".to_owned()),
        }
    }
}

fn field_of(step: &Segment) -> String {
    match step {
        Segment::Field(key) | Segment::Key(key) => key.clone(),
        Segment::Item(id) => id.as_str().to_owned(),
        Segment::Cell { row, column } => format!("{row}:{column}"),
    }
}
