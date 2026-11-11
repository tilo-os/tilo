//! Inert discovery index, built only from a normal validated publication.
//! It never retains `ResourceSpec`/`ActionSpec` values or executable callbacks.
use super::effective;
use runtime_collab::ActionDirectoryRow;
use std::collections::HashMap;
use tilo_actions::{ActionAddress, ActionDescriptor, OperationKind, ResourceSpec};

/// One resource's directory level: its actions' rows, then its children's.
#[derive(Debug, PartialEq, Eq)]
pub struct DirectoryNode {
    pub rows: Vec<ActionDirectoryRow>,
    /// How far below a top-level resource the resource is.
    pub depth: usize,
}

/// The directory of one publication: its top level, each resource's level,
/// and each action's row by address, in the tree order the publication
/// declares them.
#[derive(Default, Debug)]
pub struct ActionDirectorySnapshot {
    // Estimated once, when a pin for local input first needs it: every
    // publication builds a snapshot, and most are never pinned (an agent's
    // reads, a sync, a save), while the estimate serializes every action.
    // An input pins right after the publication that built its snapshot, so
    // computing it there costs that input what building it did before.
    retained_bytes: std::cell::OnceCell<usize>,
    /// Digest of the invocations the published actions encode, when every
    /// one of them is covered; see `effective`.
    pub effective_invocations: Option<[u8; 32]>,
    pub roots: Vec<ActionDirectoryRow>,
    pub nodes: HashMap<String, DirectoryNode>,
    pub actions: HashMap<ActionAddress, ActionDirectoryRow>,
    /// Every action's address, in tree order: what a search walks.
    pub order: Vec<ActionAddress>,
}

impl PartialEq for ActionDirectorySnapshot {
    fn eq(&self, other: &Self) -> bool {
        self.effective_invocations == other.effective_invocations
            && self.roots == other.roots
            && self.nodes == other.nodes
            && self.actions == other.actions
            && self.order == other.order
    }
}
impl Eq for ActionDirectorySnapshot {}

/// The descriptor a directory row carries. Directory pages have never
/// carried a resource view: the broker proves a view's mount and range
/// bindings against the actions on the same page, which a bounded page
/// cannot promise, so views reach the shell through the published
/// surface instead.
#[must_use]
pub fn exported_directory_action(descriptor: &ActionDescriptor) -> runtime_collab::ExportedAction {
    runtime_collab::ExportedAction {
        view: None,
        ..runtime_collab::ExportedAction::from(descriptor)
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
impl<F: Fn(&ResourceSpec) -> Option<ActionAddress>> Builder<'_, F> {
    fn node(
        &mut self,
        resource: &ResourceSpec,
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
                let exported = exported_directory_action(descriptor);
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
                self.snapshot.order.push(address.clone());
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
    /// What the snapshot is estimated to hold, estimated the first time it
    /// is asked.
    #[must_use]
    pub fn retained_bytes(&self) -> usize {
        *self
            .retained_bytes
            .get_or_init(|| self.estimate_retained_bytes())
    }

    /// Whether what the snapshot holds has been estimated yet.
    #[must_use]
    pub fn retention_estimated(&self) -> bool {
        self.retained_bytes.get().is_some()
    }

    fn estimate_retained_bytes(&self) -> usize {
        // An action's row is held twice (by its resource's node and by the
        // action index), each a clone of the same descriptor: its wire
        // length is measured once and counted for each.
        fn rows<'a>(
            rows: &'a [ActionDirectoryRow],
            measured: &mut HashMap<(&'a str, &'a str), usize>,
        ) -> usize {
            rows.iter().fold(0usize, |bytes, row| {
                let strings = row
                    .parent_resource
                    .capacity()
                    .saturating_add(row.resource.capacity())
                    .saturating_add(row.resource_kind.capacity())
                    .saturating_add(row.resource_label.capacity())
                    .saturating_add(row.action.as_ref().map_or(0, String::capacity));
                // Count into a sink, not a temporary JSON buffer. The typed
                // descriptor, presentation and contract hold their nested
                // Vec/enum/map slots, allocator headers and small owned values
                // beside the row slots and strings counted here; measured
                // against the allocator they take under their wire length, so
                // twice it bounds them. These are cloned wire data, not
                // arbitrary application caches.
                let measure = || {
                    runtime_collab::wire_json_len(&row.descriptor)
                        .saturating_add(runtime_collab::wire_json_len(&row.presentation))
                        .saturating_add(row.visibility.as_ref().map_or(0, |visibility| {
                            visibility
                                .action
                                .capacity()
                                .saturating_add(runtime_collab::wire_json_len(&visibility.contract))
                        }))
                        .saturating_mul(2)
                };
                let nested = match row.action.as_deref() {
                    Some(action) => *measured
                        .entry((row.resource.as_str(), action))
                        .or_insert_with(measure),
                    None => measure(),
                };
                bytes.saturating_add(strings).saturating_add(nested)
            })
        }
        let mut measured = HashMap::new();
        fn row_slots(capacity: usize) -> usize {
            capacity.saturating_mul(std::mem::size_of::<ActionDirectoryRow>())
        }
        // HashMap capacity is usable entries, not buckets. Doubling covers
        // spare buckets/control bytes, including the small-table case.
        let mut bytes = std::mem::size_of::<Self>()
            .saturating_add(64)
            .saturating_add(row_slots(self.roots.capacity()))
            .saturating_add(rows(&self.roots, &mut measured))
            .saturating_add(
                self.nodes
                    .capacity()
                    .saturating_mul(2 * (std::mem::size_of::<(String, DirectoryNode)>() + 16)),
            )
            .saturating_add(self.actions.capacity().saturating_mul(
                2 * (std::mem::size_of::<(ActionAddress, ActionDirectoryRow)>() + 16),
            ));
        for (key, node) in &self.nodes {
            bytes = bytes
                .saturating_add(key.capacity())
                .saturating_add(row_slots(node.rows.capacity()))
                .saturating_add(rows(&node.rows, &mut measured));
        }
        for (address, row) in &self.actions {
            bytes = bytes
                .saturating_add(address.resource.capacity())
                .saturating_add(address.action.capacity())
                .saturating_add(rows(std::slice::from_ref(row), &mut measured));
        }
        for address in &self.order {
            bytes = bytes
                .saturating_add(std::mem::size_of::<ActionAddress>())
                .saturating_add(address.resource.capacity())
                .saturating_add(address.action.capacity());
        }
        bytes
    }

    /// The directory of `resources`, whose actions `descriptors` describe
    /// (the publication's [`tilo_actions::door::survey`]). A resource is
    /// disclosed through the action `visibility` names for it, or else its
    /// first view, preferring one that reads the document; one disclosed by
    /// neither is flattened into its actions and children.
    #[must_use]
    pub fn build(
        resources: &[ResourceSpec],
        descriptors: &[ActionDescriptor],
        visibility: impl Fn(&ResourceSpec) -> Option<ActionAddress>,
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
