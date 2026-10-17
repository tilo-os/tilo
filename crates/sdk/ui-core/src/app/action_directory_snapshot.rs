//! Inert discovery index, built only from a normal validated publication.
//! It never retains `Resource`/`ActionSpec` values or executable callbacks.
use super::{App, ResourceCacheKey};
use runtime_collab::ActionDirectoryRow;
use std::collections::HashMap;
use tilo_actions::{ActionAddress, ActionDescriptor, OperationKind, Resource};
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

#[derive(PartialEq, Eq)]
pub(super) struct DirectoryNode {
    pub rows: Vec<ActionDirectoryRow>,
    pub depth: usize,
}

#[derive(Default, PartialEq, Eq)]
pub(super) struct ActionDirectorySnapshot {
    pub(super) effective_invocations: Option<[u8; 32]>,
    pub roots: Vec<ActionDirectoryRow>,
    pub nodes: HashMap<String, DirectoryNode>,
    pub actions: HashMap<ActionAddress, ActionDirectoryRow>,
}

/// A pre-event inert directory retained only for local mutation admission.
/// Dropping the last owner makes the revision unavailable to the local RPC.
pub(super) struct LocalDocumentDirectoryPin {
    revision: String,
    snapshot: std::rc::Rc<ActionDirectorySnapshot>,
}

impl App {
    pub(super) fn pin_local_document_action_directory(
        &mut self,
        revision: &str,
    ) -> Option<std::rc::Rc<LocalDocumentDirectoryPin>> {
        if revision.is_empty() || self.try_action_directory_revision().ok()? != revision {
            return None;
        }
        let snapshot = self.actions.action_directory_snapshot.as_ref()?.clone();
        let pins = &mut self.actions.local_document_directory_pins;
        pins.retain(|pin| pin.strong_count() > 0);
        // Never evict an in-flight report to admit another one.
        if pins.len() >= 64 {
            return None;
        }
        let pin = std::rc::Rc::new(LocalDocumentDirectoryPin {
            revision: revision.to_owned(),
            snapshot,
        });
        pins.push(std::rc::Rc::downgrade(&pin));
        Some(pin)
    }

    pub(super) fn resolve_local_document_action_directory(
        &self,
        revision: &str,
        resource: &str,
        action: &str,
    ) -> runtime_collab::ActionDirectoryResolution {
        let Some(pin) = self
            .actions
            .local_document_directory_pins
            .iter()
            .filter_map(std::rc::Weak::upgrade)
            .find(|pin| pin.revision == revision)
        else {
            return runtime_collab::ActionDirectoryResolution::StaleRevision;
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

struct Builder<'a, F> {
    effective: effective::Builder,
    snapshot: ActionDirectorySnapshot,
    descriptors: &'a [ActionDescriptor],
    by_address: HashMap<ActionAddress, usize>,
    offset: usize,
    visibility: F,
}
impl<F: Fn(&Resource) -> Option<ActionAddress>> Builder<'_, F> {
    fn node(
        &mut self,
        resource: &Resource,
        parent: &str,
        depth: usize,
    ) -> (Vec<ActionDirectoryRow>, Option<usize>) {
        self.effective.resource(resource);
        let start = self.offset;
        let mut actions = Vec::new();
        let mut first_view: Option<usize> = None;
        for action in &resource.actions {
            let address = ActionAddress::new(resource.id.as_str(), &action.id);
            if let Some(index) = self.by_address.get(&address).copied()
                && let Some(descriptor) = self.descriptors.get(index)
            {
                if descriptor.kind == OperationKind::View
                    && (first_view.is_none()
                        || (descriptor.contract.reads_document()
                            && first_view.is_some_and(|old| {
                                !self.descriptors[old].contract.reads_document()
                            })))
                {
                    // Prefer document disclosure over the first private UI
                    // view, as the bounded provider path does too.
                    first_view = Some(index);
                }
                self.effective.action(resource.id.as_str(), action);
                let exported = App::exported_directory_action(descriptor);
                let row = ActionDirectoryRow {
                    parent_resource: resource.id.as_str().to_owned(),
                    resource: descriptor.address.resource.clone(),
                    resource_kind: descriptor.resource_kind.clone(),
                    resource_label: descriptor.resource_label.clone(),
                    presentation: resource
                        .presentation
                        .clone()
                        .filter(tilo_actions::ResourcePresentation::is_valid),
                    action: Some(descriptor.address.action.clone()),
                    has_children: false,
                    visibility: Some(tilo_actions::remote::RowVisibility {
                        action: descriptor.address.action.clone(),
                        contract: exported.contract.clone(),
                        governing_kind: Some(descriptor.kind),
                    }),
                    descriptor: Some(exported),
                };
                self.snapshot.actions.insert(address, row.clone());
                actions.push(row);
            }
            self.offset = self.offset.saturating_add(1);
        }
        let mut children = Vec::new();
        for child in &resource.children {
            let (exposed, view) = self.node(child, resource.id.as_str(), depth.saturating_add(1));
            first_view = first_view.or(view);
            children.extend(exposed);
        }
        let explicit = (self.visibility)(resource);
        let visible = match explicit {
            Some(address) => self
                .by_address
                .get(&address)
                .copied()
                .filter(|index| *index >= start && *index < self.offset),
            None => first_view,
        };
        let exposed =
            if let Some(descriptor) = visible.and_then(|index| self.descriptors.get(index)) {
                vec![ActionDirectoryRow {
                    parent_resource: parent.to_owned(),
                    resource: resource.id.as_str().to_owned(),
                    resource_kind: resource.kind.clone(),
                    resource_label: resource.label.clone(),
                    presentation: resource
                        .presentation
                        .clone()
                        .filter(tilo_actions::ResourcePresentation::is_valid),
                    action: None,
                    descriptor: None,
                    has_children: !actions.is_empty() || !children.is_empty(),
                    visibility: Some(tilo_actions::remote::RowVisibility {
                        action: descriptor.address.action.clone(),
                        contract: descriptor.contract.clone(),
                        governing_kind: Some(descriptor.kind),
                    }),
                }]
            } else {
                // Mutating/effect contracts disclose only their typed actions,
                // never a container label using an invented read policy.
                let mut exposed = actions.clone();
                exposed.extend(children.clone());
                for row in &mut exposed {
                    parent.clone_into(&mut row.parent_resource);
                }
                exposed
            };
        actions.extend(children);
        self.snapshot.nodes.insert(
            resource.id.as_str().to_owned(),
            DirectoryNode {
                rows: actions,
                depth,
            },
        );
        (exposed, first_view)
    }
}

impl ActionDirectorySnapshot {
    pub fn build(
        resources: &[Resource],
        descriptors: &[ActionDescriptor],
        visibility: impl Fn(&Resource) -> Option<ActionAddress>,
    ) -> Self {
        let mut builder = Builder {
            effective: effective::Builder::new(),
            snapshot: Self::default(),
            descriptors,
            by_address: descriptors
                .iter()
                .enumerate()
                .map(|(index, descriptor)| (descriptor.address.clone(), index))
                .collect(),
            offset: 0,
            visibility,
        };
        for resource in resources {
            let (rows, _) = builder.node(resource, "", 0);
            builder.snapshot.roots.extend(rows);
        }
        builder.snapshot.effective_invocations =
            builder.effective.finish(builder.snapshot.actions.len());
        builder.snapshot
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
        resources: &[Resource],
        descriptors: &[ActionDescriptor],
    ) {
        // Establish current ownership/generation witnesses before comparing data.
        if self.try_action_directory_revision().is_err() {
            return;
        }
        let snapshot = ActionDirectorySnapshot::build(resources, descriptors, |resource| {
            self.directory_explicit_visibility(resource)
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
