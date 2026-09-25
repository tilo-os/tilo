//! Inert discovery index, built only from a normal validated publication.
//! It never retains `Resource`/`ActionSpec` values or executable callbacks.
use super::{App, ResourceCacheKey};
use runtime_collab::ActionDirectoryRow;
use std::collections::HashMap;
use tilo_actions::{ActionAddress, ActionDescriptor, OperationKind, Resource};

#[derive(Clone, PartialEq, Eq)]
pub(super) struct DirectoryWitness {
    pub key: ResourceCacheKey,
    pub permissions: crate::permissions::PermissionContext,
    pub permission_generation: u64,
    pub route: Option<crate::collaboration::PrimaryRouteWitness>,
    pub owners: Vec<crate::bound_dispatch::BoundDirectoryWitness>,
    pub root_revision: Option<String>,
    pub providers: Vec<String>,
}

#[derive(PartialEq, Eq)]
pub(super) struct DirectoryNode {
    pub rows: Vec<ActionDirectoryRow>,
    pub depth: usize,
}

#[derive(Default, PartialEq, Eq)]
pub(super) struct ActionDirectorySnapshot {
    pub roots: Vec<ActionDirectoryRow>,
    pub nodes: HashMap<String, DirectoryNode>,
    pub actions: HashMap<ActionAddress, ActionDirectoryRow>,
}

struct Builder<'a, F> {
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
        let start = self.offset;
        let mut actions = Vec::new();
        let mut first_view = None;
        for action in &resource.actions {
            let address = ActionAddress::new(resource.id.as_str(), &action.id);
            if let Some(index) = self.by_address.get(&address).copied()
                && let Some(descriptor) = self.descriptors.get(index)
            {
                if first_view.is_none() && descriptor.kind == OperationKind::View {
                    first_view = Some(index);
                }
                let exported = App::exported_directory_action(descriptor);
                let row = ActionDirectoryRow {
                    parent_resource: resource.id.as_str().to_owned(),
                    resource: descriptor.address.resource.clone(),
                    resource_kind: descriptor.resource_kind.clone(),
                    resource_label: descriptor.resource_label.clone(),
                    resource_presentation_json: resource
                        .presentation
                        .as_ref()
                        .filter(|value| value.is_valid())
                        .and_then(|value| serde_json::to_string(value).ok())
                        .unwrap_or_default(),
                    action: Some(descriptor.address.action.clone()),
                    has_children: false,
                    visibility_action: Some(descriptor.address.action.clone()),
                    visibility_contract_json: exported.contract_json.clone(),
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
                    resource_presentation_json: resource
                        .presentation
                        .as_ref()
                        .filter(|value| value.is_valid())
                        .and_then(|value| serde_json::to_string(value).ok())
                        .unwrap_or_default(),
                    action: None,
                    descriptor: None,
                    has_children: !actions.is_empty() || !children.is_empty(),
                    visibility_action: Some(descriptor.address.action.clone()),
                    visibility_contract_json: serde_json::to_string(&descriptor.contract)
                        .unwrap_or_default(),
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
        builder.snapshot
    }
}

impl App {
    pub(super) fn capture_action_directory_snapshot(
        &mut self,
        resources: &[Resource],
        descriptors: &[ActionDescriptor],
    ) {
        // Establish current ownership/generation witnesses before comparing data.
        self.action_directory_revision();
        let snapshot = ActionDirectorySnapshot::build(resources, descriptors, |resource| {
            self.directory_explicit_visibility(resource)
        });
        if self.actions.action_directory_snapshot.as_ref() != Some(&snapshot) {
            if let Some((_, revision)) = &mut self.actions.action_directory_stamp {
                *revision = format!("v5:{}", uuid::Uuid::new_v4());
            }
            self.actions.action_directory_cursors.clear();
            self.actions.action_directory_snapshot = Some(snapshot);
        }
    }
}
