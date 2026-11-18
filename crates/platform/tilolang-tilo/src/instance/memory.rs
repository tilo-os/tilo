//! A workspace document kept in memory beside its instance, where the action API stands:
//! it takes each step the instance hands over, writes it down as the person's step and answers it,
//! as a session's API does. What runs an instance with no session around it keeps one: a browser
//! page that runs the app and its window side by side, and a capture.

use serde_json::Value as Json;
use tilo_actions::{ActionContext, ActionInvoker, apply_local_with_context};
use tilo_resource::Doc;

use super::Instance;
use crate::action::{DynAction, DynEdit};
use crate::host::Host;
use crate::publish::{COMMIT, HOST_RESOURCE};

/// The document, and the person whose steps are written into it when a step names no author.
pub struct MemoryDocument {
    doc: Doc,
    person: String,
}

impl MemoryDocument {
    /// The document `doc`, written into as `person`.
    #[must_use]
    pub fn new(doc: Doc, person: impl Into<String>) -> Self {
        Self {
            doc,
            person: person.into(),
        }
    }

    /// The document as it stands.
    #[must_use]
    pub fn doc(&self) -> &Doc {
        &self.doc
    }

    /// Bring `instance` up to date with the document, write down every step it handed over and
    /// answer each, and bring it up to date again with what they wrote. A step dated `sample_at`
    /// (a scenario's sample data, which it writes without a time of its own) is dated a second
    /// before the earliest step it dates, so what the scenario dates is the story it tells.
    /// Whether a step was written.
    ///
    /// # Errors
    /// A step the document refused.
    pub fn take_in(
        &mut self,
        instance: &mut Instance,
        sample_at: Option<&str>,
    ) -> Result<bool, String> {
        instance.sync(Some(&self.doc));
        instance.commit_planned();
        let Some(host) = instance.host() else {
            return Ok(false);
        };
        let mut steps = Vec::new();
        while let Some((invocation, _)) = instance.take_invocation() {
            let address = invocation.address.clone();
            if address.resource == HOST_RESOURCE && address.action == COMMIT {
                let ticket = invocation.arguments.get("ticket").and_then(Json::as_u64);
                if let Some(action) = ticket.and_then(|ticket| host.store().committed(ticket)) {
                    steps.push((address, action));
                    continue;
                }
            }
            instance.answered(&address, None);
        }
        if steps.is_empty() {
            return Ok(false);
        }
        let earliest = sample_at.and_then(|sample_at| {
            steps
                .iter()
                .filter_map(|(_, action)| action.authorship.at.as_deref())
                .filter(|at| *at < sample_at)
                .min()
                .map(second_before)
        });
        for (address, mut action) in steps {
            if let Some(sample_at) = sample_at
                && action.authorship.at.as_deref() == Some(sample_at)
                && let Some(earliest) = earliest.as_deref()
            {
                action.authorship.at = Some(earliest.to_owned());
            }
            self.commit(&host, &action)?;
            instance.answered(&address, None);
        }
        // The steps are in the document now: the marks and catch-ups that read them read the
        // document as it stands after them.
        instance.sync(Some(&self.doc));
        Ok(true)
    }

    /// Apply `action` to the document as the action API applies one for a session: encoded, as the
    /// person's step, with the context a step on the document carries, so what the store tells of
    /// the change (who made it, when, and the version it is of each resource) reads as a
    /// session's.
    fn commit(&mut self, host: &Host, action: &DynAction) -> Result<(), String> {
        // A resource its origin derives (the shell's own document and layout, a session's document)
        // lives in the store's unsaved layer and is never the workspace document's to have, so its
        // steps stay there.
        let Some(action) = only_the_document(action, host) else {
            return Ok(());
        };
        // The step is its author's, and the change commits under their actor: a gesture's
        // continuation is written as the actor's, so a step of another person's committed under
        // this one would be read as a continuation of their gesture.
        let author = action.authorship.invoker.clone().unwrap_or_else(|| {
            tilolang_runtime::Actor::Member(tilolang_runtime::ids::MemberId::person(&self.person))
        });
        let context = ActionContext::for_resource(
            action.resources().first().copied().unwrap_or("workspace"),
            &action.kind,
            &action.kind,
        )
        .with_invoker(ActionInvoker::admitted(&author.clone().into()));
        let encoded = action.encode().map_err(|error| error.to_string())?;
        let actor = author.actor_bytes();
        host.with_schemas(|| {
            apply_local_with_context(&mut self.doc, &action, encoded, Some(&actor), context)
        })
        .map_err(|error| error.to_string())?;
        self.doc.commit();
        Ok(())
    }
}

/// The step `action` as the workspace document takes it: its edits of the resources the document
/// holds, `None` when it has none.
fn only_the_document(action: &DynAction, host: &Host) -> Option<DynAction> {
    let edits: Vec<_> = action
        .edits
        .iter()
        .filter(|edit| !host.store().derives(resource_of(edit)))
        .cloned()
        .collect();
    if edits.is_empty() {
        return None;
    }
    let mut kept = action.clone();
    kept.edits = edits;
    Some(kept)
}

/// The resource `edit` writes.
fn resource_of(edit: &DynEdit) -> &str {
    match edit {
        DynEdit::Create { id, .. }
        | DynEdit::Edit { id, .. }
        | DynEdit::Insert { id, .. }
        | DynEdit::Place { id, .. }
        | DynEdit::Remove { id }
        | DynEdit::OutboxPush { id, .. }
        | DynEdit::OutboxSettle { id, .. }
        | DynEdit::OutboxMark { id, .. }
        | DynEdit::Migrated { id, .. }
        | DynEdit::TriggerRan { id, .. }
        | DynEdit::RemoveItem { id, .. } => id,
        // A run of a collection counts against the document it is a field of, which names no
        // document of its own.
        DynEdit::RunChunk { holder, .. } => holder,
        DynEdit::In { edit, .. } | DynEdit::Chunk { edit, .. } => resource_of(edit),
    }
}

/// The moment a second before `at` (an RFC 3339 time), written as the document writes one.
fn second_before(at: &str) -> String {
    let Ok(parsed) = at.parse::<jiff::Timestamp>() else {
        return at.to_owned();
    };
    let Ok(earlier) = parsed.checked_sub(jiff::SignedDuration::from_secs(1)) else {
        return at.to_owned();
    };
    format!("{earlier:.0}")
}
