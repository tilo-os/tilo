//! The app's hold on its directory snapshot
//! ([`collaboration::action_directory`]): the witness a revision is reused
//! under, and the snapshots pinned for local input.
use super::{App, ResourceCacheKey};
pub(super) use ::collaboration::action_directory::ActionDirectorySnapshot;
use runtime_collab::ActionDirectoryRow;
use std::collections::HashMap;
use tilo_actions::{ActionAddress, ActionDescriptor, ResourceSpec};
#[cfg(test)]
#[path = "directory_effective_witness.rs"]
mod effective;

#[derive(Clone, PartialEq, Eq)]
pub(super) struct DirectoryWitness {
    pub key: ResourceCacheKey,
    pub registry_generation: u64,
    pub root_domain: crate::widget::ActionDirectoryInvocationDomain,
    pub provider_domains: Vec<crate::widget::ActionDirectoryInvocationDomain>,
    pub permissions: crate::permissions::DirectoryPermissionWitness,
    pub permission_generation: u64,
    pub route: Option<crate::collaboration::PrimaryRouteWitness>,
    pub owners: Vec<crate::bound_dispatch::BoundDirectoryWitness>,
    pub root_revision: Option<String>,
    pub providers: Vec<String>,
}

impl DirectoryWitness {
    fn reusable_model_projection(&self) -> bool {
        self.key.mutation_generation != u64::MAX
            && self.registry_generation != u64::MAX
            && self.root_domain.is_supported()
            && self
                .provider_domains
                .iter()
                .all(|domain| domain.is_supported())
            && self
                .owners
                .iter()
                .all(|owner| owner.supports_model_refresh())
    }

    fn same_non_scheduling_fields(&self, other: &Self) -> bool {
        self.permissions == other.permissions
            && self.permission_generation == other.permission_generation
            && self.route == other.route
            && self.owners.len() == other.owners.len()
            && self
                .owners
                .iter()
                .zip(&other.owners)
                .all(|(current, before)| current.same_model_owner_after(before))
            && self.root_revision == other.root_revision
            && self.providers == other.providers
            && self.registry_generation == other.registry_generation
            && self.root_domain == other.root_domain
            && self.provider_domains == other.provider_domains
    }
}

/// An inert, one-pass candidate. Real model sync and fresh publication still
/// run. No executable declaration or permission decision is retained here.
pub(super) struct ModelDirectoryCandidate {
    witness: std::rc::Rc<DirectoryWitness>,
    revision: String,
    snapshot: std::rc::Rc<ActionDirectorySnapshot>,
    cursors: HashMap<String, super::ActionDirectoryCursor>,
}

const MAX_PINNED_REVISIONS: usize = 64;

/// Why a published surface could not be pinned for local input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PinRefused {
    /// The surface moved under the input: a later one can carry its actions.
    Moved,
    /// As many surfaces as may be retained are held by changes on their way.
    Full,
}
const MAX_PINNED_BYTES: usize = 4 * 1024 * 1024;

/// A pre-event inert directory retained only for local mutation admission.
/// Dropping the last owner makes the revision unavailable to the local RPC.
#[derive(Clone)]
pub(super) struct LocalDocumentDirectoryPin {
    revision: String,
    snapshot: std::rc::Rc<ActionDirectorySnapshot>,
    retained_bytes: usize,
}

impl App {
    pub(super) fn checkpoint_local_directory_row(
        &self,
        revision: &str,
        resource: &str,
        action: &str,
    ) -> Option<ActionDirectoryRow> {
        self.actions
            .local_document_directory_pins
            .iter()
            .filter_map(std::rc::Weak::upgrade)
            .filter(|pin| pin.revision == revision)
            .find_map(|pin| {
                pin.snapshot
                    .actions
                    .get(&ActionAddress::new(resource, action))
                    .cloned()
            })
    }

    pub(super) fn restore_local_directory_row(
        &mut self,
        revision: String,
        resource: String,
        action: String,
        row: ActionDirectoryRow,
    ) -> Option<std::rc::Rc<LocalDocumentDirectoryPin>> {
        if row.resource != resource
            || row.action.as_deref() != Some(action.as_str())
            || row.descriptor.is_none()
        {
            return None;
        }
        let mut snapshot = ActionDirectorySnapshot::default();
        snapshot
            .actions
            .insert(ActionAddress::new(resource, action), row);
        let pins = &mut self.actions.local_document_directory_pins;
        pins.retain(|pin| pin.strong_count() > 0);
        let bytes = snapshot
            .retained_bytes()
            .saturating_add(revision.len())
            .saturating_add(128);
        let retained: usize = pins
            .iter()
            .filter_map(std::rc::Weak::upgrade)
            .map(|pin| pin.retained_bytes)
            .sum();
        if pins.len() >= MAX_PINNED_REVISIONS || retained.saturating_add(bytes) > MAX_PINNED_BYTES {
            return None;
        }
        let pin = std::rc::Rc::new(LocalDocumentDirectoryPin {
            revision,
            snapshot: std::rc::Rc::new(snapshot),
            retained_bytes: bytes,
        });
        pins.push(std::rc::Rc::downgrade(&pin));
        Some(pin)
    }

    /// Pin `revision` for local mutation admission, or say why not: the
    /// published surface moved, or the bound on retained surfaces is reached.
    pub(super) fn try_pin_local_document_action_directory(
        &mut self,
        revision: &str,
    ) -> Result<std::rc::Rc<LocalDocumentDirectoryPin>, PinRefused> {
        if revision.is_empty() {
            return Err(PinRefused::Moved);
        }
        if self.collaboration.manager.is_connected() {
            if self.try_action_directory_revision().ok().as_deref() != Some(revision) {
                return Err(PinRefused::Moved);
            }
        } else if self
            .actions
            .action_directory_stamp
            .as_ref()
            .map(|(_, stamp)| stamp.as_str())
            != Some(revision)
        {
            // Disconnection must not recompute a revision against a now-absent
            // primary witness. Only the previously frozen surface is usable.
            return Err(PinRefused::Moved);
        }
        let snapshot = self
            .actions
            .action_directory_snapshot
            .as_ref()
            .ok_or(PinRefused::Moved)?
            .clone();
        let pins = &mut self.actions.local_document_directory_pins;
        pins.retain(|pin| pin.strong_count() > 0);
        let mut retained_bytes = 0usize;
        for pin in pins.iter().filter_map(std::rc::Weak::upgrade) {
            if pin.revision == revision {
                return Ok(pin);
            }
            retained_bytes = retained_bytes.saturating_add(pin.retained_bytes);
        }
        let bytes = snapshot
            .retained_bytes()
            .saturating_add(revision.len())
            .saturating_add(std::mem::size_of::<LocalDocumentDirectoryPin>() + 64);
        // Bound distinct revisions and their complete retained allocations.
        // Never evict an in-flight report to admit another one.
        if pins.len() >= MAX_PINNED_REVISIONS
            || retained_bytes.saturating_add(bytes) > MAX_PINNED_BYTES
        {
            return Err(PinRefused::Full);
        }
        let pin = std::rc::Rc::new(LocalDocumentDirectoryPin {
            revision: revision.to_owned(),
            snapshot,
            retained_bytes: bytes,
        });
        pins.push(std::rc::Rc::downgrade(&pin));
        Ok(pin)
    }

    pub(super) fn resolve_local_document_action_directory(
        &self,
        revision: &str,
        resource: &str,
        action: &str,
    ) -> runtime_collab::ActionDirectoryResolution {
        let mut revision_found = false;
        let Some(pin) = self
            .actions
            .local_document_directory_pins
            .iter()
            .filter_map(std::rc::Weak::upgrade)
            .find(|pin| {
                if pin.revision != revision {
                    return false;
                }
                revision_found = true;
                pin.snapshot
                    .actions
                    .contains_key(&ActionAddress::new(resource, action))
            })
        else {
            return if revision_found {
                runtime_collab::ActionDirectoryResolution::NotFound
            } else {
                runtime_collab::ActionDirectoryResolution::StaleRevision
            };
        };
        // No live provider fallback: only rows actually frozen before input
        // may explain an already-applied mutation.
        pin.snapshot
            .actions
            .get(&ActionAddress::new(resource, action))
            .map_or(runtime_collab::ActionDirectoryResolution::NotFound, |row| {
                runtime_collab::ActionDirectoryResolution::Found {
                    row: Box::new(row.clone()),
                    revision: Some(pin.revision.clone()),
                }
            })
    }
}

impl App {
    pub(super) fn begin_model_directory_refresh(&mut self) -> Option<ModelDirectoryCandidate> {
        if self.action_publication_failure.is_some() || !self.collaboration.manager.is_connected() {
            return None;
        }
        self.try_action_directory_revision().ok()?;
        if !self
            .actions
            .action_directory_stamp
            .as_ref()?
            .0
            .reusable_model_projection()
        {
            return None;
        }
        self.actions
            .action_directory_snapshot
            .as_ref()?
            .effective_invocations?;
        let snapshot = self.actions.action_directory_snapshot.as_ref()?.clone();
        let (witness, revision) = self.actions.action_directory_stamp.as_ref()?.clone();
        Some(ModelDirectoryCandidate {
            witness,
            revision,
            snapshot,
            cursors: std::mem::take(&mut self.actions.action_directory_cursors),
        })
    }

    pub(super) fn finish_model_directory_refresh(
        &mut self,
        candidate: Option<ModelDirectoryCandidate>,
    ) {
        let Some(candidate) = candidate else { return };
        if self.action_publication_failure.is_some()
            || !self.collaboration.manager.is_connected()
            || self.try_action_directory_revision().is_err()
        {
            return;
        }
        let Some(snapshot) = &self.actions.action_directory_snapshot else {
            return;
        };
        let Some((witness, revision)) = &mut self.actions.action_directory_stamp else {
            return;
        };
        if *revision == candidate.revision {
            // Nothing re-derived the directory during this pass (an idle
            // hosted pump): the same revision still names the same snapshot,
            // so the cursors issued for it remain valid.
            for (key, cursor) in candidate.cursors {
                self.actions
                    .action_directory_cursors
                    .entry(key)
                    .or_insert(cursor);
            }
            return;
        }
        if witness.reusable_model_projection()
            && witness.same_non_scheduling_fields(&candidate.witness)
            && snapshot == &candidate.snapshot
        {
            // Retain the NEW witness and freshly materialized snapshot. Only
            // their externally visible revision/cursors can remain continuous.
            *revision = candidate.revision;
            self.actions.action_directory_cursors = candidate.cursors;
        }
    }

    pub(super) fn capture_action_directory_snapshot(
        &mut self,
        resources: &[ResourceSpec],
        descriptors: &[ActionDescriptor],
    ) {
        // Establish current ownership/generation witnesses before comparing data.
        if self.try_action_directory_revision().is_err() {
            return;
        }
        let visible = self.directory_explicit_visibilities(resources);
        let snapshot = ActionDirectorySnapshot::build(resources, descriptors, |resource| {
            visible.get(resource.id.as_str()).cloned()
        });
        if self.actions.action_directory_snapshot.as_deref() != Some(&snapshot) {
            if let Some((_, revision)) = &mut self.actions.action_directory_stamp {
                *revision = format!("v5:{}", uuid::Uuid::new_v4());
            }
            self.actions.action_directory_cursors.clear();
            self.actions.action_directory_snapshot = Some(std::rc::Rc::new(snapshot));
        }
    }
}
