//! Descriptions of the open resource's changes: asked of the shell, shown as
//! they arrive, and written into the document so every member reads them.
//!
//! The app holds no model. While History is open the host asks the shell,
//! a few of the newest undescribed changes at a time, with what each change
//! did (never who did it); the shell asks the model on this device and
//! answers. An answer shows at once, and the owner writes it into the
//! resource's own document ([`crate::history_source::descriptions`]) when no
//! edit is open, through the door a restore takes. A member without write
//! permission keeps what it was told for as long as History is open.
#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;
use crate::HistoryEntryId;
use crate::floating_surface::HistoryChangeFacts;
use crate::history_source::descriptions;
use crate::widget::history_group::hex16;
use std::collections::{HashMap, HashSet};

/// How many changes one request asks about.
const BATCH: usize = 12;

#[derive(Default)]
pub(super) struct Describing {
    /// Descriptions the shell gave, by change, with their generator.
    received: HashMap<HistoryEntryId, (String, String)>,
    /// Received and not yet in the document.
    #[cfg(feature = "native")]
    unwritten: Vec<HistoryEntryId>,
    /// Changes asked about, answered or not: each is asked once.
    asked: HashSet<HistoryEntryId>,
    outbox: Vec<HistoryChangeFacts>,
    in_flight: bool,
}

impl HistoryHost {
    /// Queue the newest listed changes nobody has described, when no request
    /// is out. `document` is the projected resource.
    pub(super) fn queue_descriptions(&mut self, document: &mut automerge::AutoCommit) {
        if self.describing.in_flight
            || !self.describing.outbox.is_empty()
            || !self.writes_document()
        {
            return;
        }
        let Some(controller) = &self.controller else {
            return;
        };
        let known = controller.descriptions();
        let mut facts: Vec<(HistoryEntryId, HistoryChangeFacts)> = controller
            .entries()
            .iter()
            .rev()
            .filter(|entry| {
                !entry.is_genesis() && entry.redaction.is_none() && !entry.is_effect_receipt()
            })
            .filter(|entry| {
                !known.contains_key(&entry.id)
                    && !self.describing.received.contains_key(&entry.id)
                    && !self.describing.asked.contains(&entry.id)
            })
            .take(BATCH)
            .map(|entry| {
                let context = entry.context.as_ref();
                let resource = context.and_then(|context| {
                    let item = context.item.as_ref().map(|item| item.label.as_str());
                    let resource = context.resource.as_ref().map(|r| r.label.as_str());
                    item.or(resource)
                        .filter(|label| !label.is_empty())
                        .map(str::to_owned)
                });
                (
                    entry.id,
                    HistoryChangeFacts {
                        change: hex16(&entry.id),
                        summary: entry.summary.clone(),
                        action: context.and_then(|context| context.purpose.clone()),
                        resource,
                        wrote: String::new(),
                    },
                )
            })
            .collect();
        if facts.is_empty() {
            return;
        }
        let ids: Vec<HistoryEntryId> = facts.iter().map(|(id, _)| *id).collect();
        let mut wrote = descriptions::change_evidence(document, &ids);
        for (id, fact) in &mut facts {
            fact.wrote = wrote.remove(id).unwrap_or_default();
        }
        self.describing.asked.extend(ids);
        self.describing.outbox = facts.into_iter().map(|(_, fact)| fact).collect();
    }

    /// Whether this scope is a document History can write into.
    fn writes_document(&self) -> bool {
        matches!(
            self.source(),
            HistorySourceRegistration::HostDocument { .. }
                | HistorySourceRegistration::OwnedDocument { .. }
        )
    }
}

/// The descriptions the document carries, and those received meanwhile.
pub(super) fn shown_descriptions(
    written: &HashMap<HistoryEntryId, String>,
    describing: &Describing,
) -> HashMap<HistoryEntryId, String> {
    let mut shown = written.clone();
    for (id, (text, _)) in &describing.received {
        shown.entry(*id).or_insert_with(|| text.clone());
    }
    shown
}

/// The exchange with the shell, which only the native runner holds: the
/// portable build asks nobody and so never receives or writes descriptions.
#[cfg(feature = "native")]
mod shell {
    #[allow(
        clippy::wildcard_imports,
        reason = "private implementation module shares its enclosing host types"
    )]
    use super::*;
    use crate::State;
    use crate::floating_surface::HistoryChangeDescription;
    use crate::permissions::{PermissionContext, UserId};
    use crate::widget::history_group::from_hex16;

    impl HistoryHost {
        /// The changes to ask the shell about, once: the request stays out
        /// until it is answered.
        pub(crate) fn take_describe_request(&mut self) -> Vec<HistoryChangeFacts> {
            if self.describing.outbox.is_empty() {
                return Vec::new();
            }
            self.describing.in_flight = true;
            std::mem::take(&mut self.describing.outbox)
        }

        /// The shell went away: what it was asked and never answered may be
        /// asked of the next one.
        pub(crate) fn describing_disconnected(&mut self) {
            let received = &self.describing.received;
            self.describing.asked.retain(|id| received.contains_key(id));
            self.describing.outbox.clear();
            self.describing.in_flight = false;
        }

        /// Take the shell's answer: descriptions of changes this host asked
        /// about show at once and wait to be written.
        pub(crate) fn receive_descriptions(&mut self, described: Vec<HistoryChangeDescription>) {
            self.describing.in_flight = false;
            for description in described {
                let Some(id) = from_hex16(&description.change) else {
                    continue;
                };
                let text = description.text.trim();
                if !self.describing.asked.contains(&id)
                    || self.describing.received.contains_key(&id)
                    || !descriptions::is_acceptable(text)
                {
                    continue;
                }
                self.describing
                    .received
                    .insert(id, (text.to_owned(), description.generator));
                self.describing.unwritten.push(id);
            }
            if let Some(controller) = &self.controller {
                let shown = shown_descriptions(controller.descriptions(), &self.describing);
                self.timeline.set_descriptions(shown);
            }
        }

        pub(crate) fn has_unwritten_descriptions(&self) -> bool {
            !self.describing.unwritten.is_empty()
        }

        /// Write the received descriptions into the resource's document as one
        /// change, when no edit is open. Whether the document changed.
        pub(crate) fn service_descriptions(
            &mut self,
            state: &mut State,
            authority: &PermissionContext,
            author: &UserId,
        ) -> bool {
            let Some(scope) = self.registration.definition.scopes.get(self.active_scope) else {
                return false;
            };
            let Some(now) = (self.clock)() else {
                return false;
            };
            let described: Vec<(HistoryEntryId, String, String)> = self
                .describing
                .unwritten
                .iter()
                .filter_map(|id| {
                    let (text, generator) = self.describing.received.get(id)?;
                    Some((*id, text.clone(), generator.clone()))
                })
                .collect();
            let result = crate::state::owned_restore::describe_history(
                state,
                self.source(),
                scope.title,
                (authority, author),
                now,
                &described,
            );
            match result {
                Ok(receipt) => {
                    self.describing.unwritten.clear();
                    if receipt.changed {
                        self.restored_source = Some(self.source().clone());
                        if let HistorySourceRegistration::OwnedDocument { owner, .. } =
                            self.source()
                        {
                            owner.schedule_committed_history();
                        }
                    }
                    receipt.changed
                }
                // An open edit: the descriptions wait for the next quiet frame.
                Err(
                    crate::state::owned_restore::OwnedRestoreError::PendingEdits
                    | crate::state::owned_restore::OwnedRestoreError::StaleReview
                    | crate::state::owned_restore::OwnedRestoreError::UnfinishedActionBoundary
                    | crate::state::owned_restore::OwnedRestoreError::ActiveAction,
                ) => false,
                // No write permission, or no document to write: shown, unshared.
                Err(_) => {
                    self.describing.unwritten.clear();
                    false
                }
            }
        }
    }
}
