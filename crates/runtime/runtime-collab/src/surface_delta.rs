//! A published action surface as a delta of shared catalogue entries.
//!
//! An application's surface repeats one catalogue entry across many
//! resources: every page of a notebook offers the same editing actions, each
//! with the same parameter schema, labels and contract, and the pages offer
//! the same set of them. Sending the whole surface on every change repeats
//! all of that per resource and per change, which for a few dozen pages is
//! megabytes. A [`SurfaceDelta`] sends
//!
//! * each distinct action catalogue entry once, a *shape*: an
//!   [`ExportedAction::shape`], with nothing of the resource it is offered
//!   on (its label in the words marked, so "Focus Chat" and "Focus Notes" are
//!   one), named by a content hash;
//! * each distinct ordered set of shapes once, a *template*, named by a hash
//!   of its shapes' ids;
//! * each resource once, with the fields that are its own and the template of
//!   the actions it offers.
//!
//! [`SurfacePublication`] turns the next surface into a delta from what the
//! receiver holds. The broker's [`SurfaceTable`] applies deltas and expands the result into
//! the same `Vec<ExportedAction>` a full `publishActions` installs, so
//! nothing that reads a surface changes.

use std::collections::{HashMap, HashSet};
use tilo_sort::CompactSort as _;

use crate::ExportedAction;

/// Bytes of a content hash naming a shape or a template.
pub const SHAPE_ID_BYTES: usize = 16;

/// A catalogue entry's or template's content hash, chosen by the publisher.
pub type ShapeId = [u8; SHAPE_ID_BYTES];

pub use tilo_actions::published::ResourceFields;

/// One resource: the fields that are its own, and the template of the actions
/// it offers.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SurfaceResource {
    pub resource: String,
    pub fields: ResourceFields,
    pub template: ShapeId,
}

/// A change to a connection's published surface.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SurfaceDelta {
    /// Discard the surface published so far before applying the rest.
    pub reset: bool,
    /// Catalogue entries the broker does not hold yet.
    pub shapes: Vec<(ShapeId, ExportedAction)>,
    /// Ordered sets of shapes the broker does not hold yet.
    pub templates: Vec<(ShapeId, Vec<ShapeId>)>,
    /// Resources added or changed; each replaces what the broker holds of it.
    pub resources: Vec<SurfaceResource>,
    /// Resources no longer published.
    pub removed: Vec<String>,
}

impl SurfaceDelta {
    /// Whether applying this delta changes nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        !self.reset && self.resources.is_empty() && self.removed.is_empty()
    }

    /// The one delta equivalent to applying `self` and then `newer`, for a
    /// publisher whose earlier delta has not been sent yet.
    #[must_use]
    pub fn then(mut self, newer: Self) -> Self {
        if newer.reset {
            return newer;
        }
        let mut shapes: HashSet<ShapeId> = self.shapes.iter().map(|(id, _)| *id).collect();
        for (id, shape) in newer.shapes {
            if shapes.insert(id) {
                self.shapes.push((id, shape));
            }
        }
        let mut templates: HashSet<ShapeId> = self.templates.iter().map(|(id, _)| *id).collect();
        for (id, members) in newer.templates {
            if templates.insert(id) {
                self.templates.push((id, members));
            }
        }
        // A removal cancels what this delta published of the resource, and
        // is kept unless a reset already dropped everything before it.
        for name in newer.removed {
            self.resources.retain(|resource| resource.resource != name);
            if !self.reset && !self.removed.contains(&name) {
                self.removed.push(name);
            }
        }
        for incoming in newer.resources {
            self.removed.retain(|removed| *removed != incoming.resource);
            if let Some(older) = self
                .resources
                .iter_mut()
                .find(|older| older.resource == incoming.resource)
            {
                *older = incoming;
            } else {
                self.resources.push(incoming);
            }
        }
        self
    }
}

/// A connection's surface as the broker holds it: the shapes and templates
/// its resources name, and the resources in the order they were first
/// published.
#[derive(Clone, Debug, Default)]
pub struct SurfaceTable {
    shapes: HashMap<ShapeId, ExportedAction>,
    templates: HashMap<ShapeId, Vec<ShapeId>>,
    resources: HashMap<String, (u64, SurfaceResource)>,
    next: u64,
}

impl SurfaceTable {
    /// Forget everything published.
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Whether `delta` applies to what is held, changing nothing: a catalogue
    /// checks every delta of a change before it applies any.
    pub fn validate(&self, delta: &SurfaceDelta) -> Result<(), String> {
        for (_, shape) in &delta.shapes {
            if !shape.resource.is_empty()
                || !shape.resource_kind.is_empty()
                || !shape.resource_label.is_empty()
                || shape.semantic_identity.is_some()
            {
                return Err(format!(
                    "shape `{}` carries a resource's own fields",
                    shape.action
                ));
            }
        }
        let held = !delta.reset;
        let shape_known = |id: &ShapeId| {
            delta.shapes.iter().any(|(known, _)| known == id)
                || (held && self.shapes.contains_key(id))
        };
        for (id, members) in &delta.templates {
            if let Some(missing) = members.iter().find(|member| !shape_known(member)) {
                return Err(format!(
                    "template {id:02x?} names shape {missing:02x?} the broker does not hold"
                ));
            }
        }
        for resource in &delta.resources {
            let known = delta
                .templates
                .iter()
                .any(|(id, _)| *id == resource.template)
                || (held && self.templates.contains_key(&resource.template));
            if !known {
                return Err(format!(
                    "resource `{}` names a template the broker does not hold",
                    resource.resource
                ));
            }
        }
        Ok(())
    }

    /// Apply `delta`, or change nothing and say why it does not apply.
    pub fn apply(&mut self, delta: &SurfaceDelta) -> Result<(), String> {
        self.validate(delta)?;
        if delta.reset {
            self.clear();
        }
        for (id, shape) in &delta.shapes {
            self.shapes.insert(*id, shape.clone());
        }
        for (id, members) in &delta.templates {
            self.templates.insert(*id, members.clone());
        }
        for name in &delta.removed {
            self.resources.remove(name);
        }
        for resource in &delta.resources {
            let seq = if let Some((seq, _)) = self.resources.get(&resource.resource) {
                *seq
            } else {
                self.next = self.next.saturating_add(1);
                self.next
            };
            self.resources
                .insert(resource.resource.clone(), (seq, resource.clone()));
        }
        let templates: HashSet<ShapeId> = self
            .resources
            .values()
            .map(|(_, resource)| resource.template)
            .collect();
        self.templates.retain(|id, _| templates.contains(id));
        let shapes: HashSet<ShapeId> = self.templates.values().flatten().copied().collect();
        self.shapes.retain(|id, _| shapes.contains(id));
        Ok(())
    }

    /// The surface as the actions `publishActions` would have carried.
    #[must_use]
    pub fn actions(&self) -> Vec<ExportedAction> {
        let mut rows: Vec<&(u64, SurfaceResource)> = self.resources.values().collect();
        rows.compact_sort_by_key(|(seq, _)| *seq);
        let mut actions = Vec::new();
        for (_, resource) in rows {
            let Some(members) = self.templates.get(&resource.template) else {
                continue;
            };
            for member in members {
                let Some(shape) = self.shapes.get(member) else {
                    continue;
                };
                actions.push(shape.on_resource(&resource.resource, &resource.fields));
            }
        }
        actions
    }

    /// Whether it holds the shape `id`.
    #[must_use]
    pub fn holds(&self, id: &ShapeId) -> bool {
        self.shapes.contains_key(id)
    }

    /// Whether it holds the template `id`.
    #[must_use]
    pub fn holds_template(&self, id: &ShapeId) -> bool {
        self.templates.contains_key(id)
    }

    /// The templates held, by id.
    pub fn templates(&self) -> impl Iterator<Item = (&ShapeId, &Vec<ShapeId>)> {
        self.templates.iter()
    }

    /// The shapes held, by id.
    pub fn shapes(&self) -> impl Iterator<Item = (&ShapeId, &ExportedAction)> {
        self.shapes.iter()
    }

    /// Number of resources held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.resources.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.resources.is_empty()
    }
}

/// One entry a surface is published from: an action as the app describes it,
/// or as the broker already holds it.
pub trait SurfaceEntry: PartialEq {
    /// The resource it is offered on.
    fn resource(&self) -> &str;
    /// It as the surface exports it.
    fn exported(&self) -> ExportedAction;
}

impl SurfaceEntry for tilo_actions::ActionDescriptor {
    fn resource(&self) -> &str {
        &self.address.resource
    }

    fn exported(&self) -> ExportedAction {
        ExportedAction::from(self)
    }
}

impl SurfaceEntry for ExportedAction {
    fn resource(&self) -> &str {
        &self.resource
    }

    fn exported(&self) -> ExportedAction {
        self.clone()
    }
}

/// One published resource and where its actions sit in the descriptors the
/// state was built from.
struct Held {
    indices: Vec<usize>,
    resource: SurfaceResource,
}

/// What the broker holds of one connection's surface, as the publisher last
/// sent it.
#[derive(Default)]
pub struct SurfacePublication {
    resources: HashMap<String, Held>,
    /// The templates the resources name, and so the shapes they name.
    templates: HashMap<ShapeId, Vec<ShapeId>>,
}

impl SurfacePublication {
    /// The delta from `prior` (what the broker holds, with the descriptors it
    /// was built from) to `descriptors`, and the state once it is applied.
    /// Without a `prior` the delta is a reset: the broker discards whatever
    /// it holds. Nothing is remembered until the caller keeps the returned
    /// state, so a delta that was not sent changes nothing.
    #[must_use]
    pub fn next<E: SurfaceEntry>(
        prior: Option<(&Self, &[E])>,
        descriptors: &[E],
    ) -> (SurfaceDelta, Self) {
        let empty = Self::default();
        let (base, base_descriptors) = prior.unwrap_or((&empty, &[]));
        let mut delta = SurfaceDelta {
            reset: prior.is_none(),
            ..SurfaceDelta::default()
        };
        let held_templates: HashSet<ShapeId> = base.templates.keys().copied().collect();
        let held_shapes: HashSet<ShapeId> = base.templates.values().flatten().copied().collect();

        // A resource's actions, in the order they were surveyed.
        let mut order: Vec<&str> = Vec::new();
        let mut groups: HashMap<&str, Vec<usize>> = HashMap::new();
        for (index, descriptor) in descriptors.iter().enumerate() {
            let name = descriptor.resource();
            groups
                .entry(name)
                .or_insert_with(|| {
                    order.push(name);
                    Vec::new()
                })
                .push(index);
        }

        let mut resources = HashMap::with_capacity(order.len());
        let mut templates: HashMap<ShapeId, Vec<ShapeId>> = HashMap::new();
        let mut sent_shapes: HashSet<ShapeId> = HashSet::new();
        let mut sent_templates: HashSet<ShapeId> = HashSet::new();
        for name in order {
            let indices = groups.remove(name).unwrap_or_default();
            let old = base.resources.get(name);
            if let Some(old) = old
                && old.indices.len() == indices.len()
                && indices
                    .iter()
                    .zip(&old.indices)
                    .all(|(new, old)| base_descriptors.get(*old) == descriptors.get(*new))
            {
                if let Some(members) = base.templates.get(&old.resource.template) {
                    templates
                        .entry(old.resource.template)
                        .or_insert_with(|| members.clone());
                }
                resources.insert(
                    name.to_owned(),
                    Held {
                        indices,
                        resource: old.resource.clone(),
                    },
                );
                continue;
            }
            let mut members = Vec::with_capacity(indices.len());
            let mut shapes = Vec::new();
            let mut fields = None;
            for index in &indices {
                let Some(descriptor) = descriptors.get(*index) else {
                    continue;
                };
                let exported = descriptor.exported();
                fields.get_or_insert_with(|| exported.resource_fields());
                let shape = exported.shape();
                let id = shape_id(&shape);
                members.push(id);
                shapes.push((id, shape));
            }
            let Some(fields) = fields else { continue };
            let template = template_id(&members);
            let resource = SurfaceResource {
                resource: name.to_owned(),
                fields,
                template,
            };
            if old.map(|old| &old.resource) != Some(&resource) {
                for (id, shape) in shapes {
                    if !held_shapes.contains(&id) && sent_shapes.insert(id) {
                        delta.shapes.push((id, shape));
                    }
                }
                if !held_templates.contains(&template) && sent_templates.insert(template) {
                    delta.templates.push((template, members.clone()));
                }
                delta.resources.push(resource.clone());
            }
            templates.entry(template).or_insert(members);
            resources.insert(name.to_owned(), Held { indices, resource });
        }
        delta.removed = base
            .resources
            .keys()
            .filter(|name| !resources.contains_key(*name))
            .cloned()
            .collect();
        delta.removed.compact_sort();
        (
            delta,
            Self {
                resources,
                templates,
            },
        )
    }

    /// The templates the published resources name.
    pub fn templates(&self) -> impl Iterator<Item = (&ShapeId, &Vec<ShapeId>)> {
        self.templates.iter()
    }

    /// Where the actions of the published resource `resource` sit in the descriptors this
    /// state was built from.
    #[must_use]
    pub fn indices(&self, resource: &str) -> Option<&[usize]> {
        self.resources
            .get(resource)
            .map(|held| held.indices.as_slice())
    }

    /// The delta for resources whose own fields (a label) changed in `descriptors`, which are
    /// the descriptors this state was built from with only those fields patched; the state is
    /// then that delta applied. Costs what the `changed` resources hold, whatever the surface.
    /// `None`, changing nothing, when one of them is not published.
    pub fn relabel<E: SurfaceEntry>(
        &mut self,
        descriptors: &[E],
        changed: &[&str],
    ) -> Option<SurfaceDelta> {
        let mut resources = Vec::with_capacity(changed.len());
        for name in changed {
            let held = self.resources.get(*name)?;
            let descriptor = descriptors.get(*held.indices.first()?)?;
            resources.push(SurfaceResource {
                resource: (*name).to_owned(),
                fields: descriptor.exported().resource_fields(),
                template: held.resource.template,
            });
        }
        let mut delta = SurfaceDelta::default();
        for resource in resources {
            let Some(held) = self.resources.get_mut(&resource.resource) else {
                continue;
            };
            if held.resource != resource {
                held.resource = resource.clone();
                delta.resources.push(resource);
            }
        }
        Some(delta)
    }
}

fn hash16(bytes: &[u8]) -> ShapeId {
    let mut id = ShapeId::default();
    for (to, from) in id.iter_mut().zip(blake3::hash(bytes).as_bytes()) {
        *to = *from;
    }
    id
}

/// The content hash that names a catalogue entry on the wire.
fn shape_id(shape: &ExportedAction) -> ShapeId {
    // Serializing an `ExportedAction` cannot fail: it holds only strings,
    // numbers and the typed action vocabulary.
    hash16(&serde_json::to_vec(shape).unwrap_or_default())
}

/// The content hash that names an ordered set of shapes.
fn template_id(members: &[ShapeId]) -> ShapeId {
    hash16(&members.concat())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ExportedInput;
    use tilo_actions::{ActionContract, OperationKind};

    fn id(n: u8) -> ShapeId {
        [n; SHAPE_ID_BYTES]
    }

    fn shape(action: &str, label: &str) -> ExportedAction {
        ExportedAction {
            state: tilo_actions::ActionState::default(),
            resource: String::new(),
            semantic_identity: None,
            action: action.into(),
            resource_kind: String::new(),
            resource_label: String::new(),
            action_label: label.into(),
            keywords: Vec::new(),
            icon: None,
            description: None,
            operation_kind: OperationKind::Document,
            params: Vec::new(),
            contract: ActionContract::default(),
            view: None,
            input: ExportedInput::None,
        }
    }

    fn resource(name: &str, label: &str, template: u8) -> SurfaceResource {
        SurfaceResource {
            resource: name.into(),
            fields: ResourceFields {
                resource_kind: "notes.page".into(),
                resource_label: label.into(),
                semantic_identity: None,
            },
            template: id(template),
        }
    }

    /// Shapes 1 (`apply`) and 2 (`open`); template 10 is `apply`, template 11
    /// is `apply` then `open`.
    fn catalogue() -> SurfaceDelta {
        SurfaceDelta {
            reset: true,
            shapes: vec![
                (id(1), shape("apply", "Apply")),
                (id(2), shape("open", "Open")),
            ],
            templates: vec![(id(10), vec![id(1)]), (id(11), vec![id(1), id(2)])],
            ..SurfaceDelta::default()
        }
    }

    fn labels(table: &SurfaceTable) -> Vec<(String, String, String)> {
        table
            .actions()
            .into_iter()
            .map(|a| (a.resource, a.action, a.resource_label))
            .collect()
    }

    #[test]
    fn a_template_is_held_once_and_expands_onto_every_resource() {
        let mut table = SurfaceTable::default();
        let mut delta = catalogue();
        delta.resources = (0..40)
            .map(|n| resource(&format!("page-{n}"), &format!("Page {n}"), 11))
            .collect();
        table.apply(&delta).unwrap();
        let actions = table.actions();
        assert_eq!(actions.len(), 80);
        assert_eq!(actions[14].resource, "page-7", "published order is kept");
        assert_eq!(actions[14].action, "apply");
        assert_eq!(actions[15].action, "open", "template order is kept");
        assert_eq!(actions[14].resource_label, "Page 7");
    }

    #[test]
    fn a_delta_changes_only_the_resources_it_names() {
        let mut table = SurfaceTable::default();
        let mut delta = catalogue();
        delta.resources = vec![
            resource("a", "A", 11),
            resource("b", "B", 11),
            resource("c", "C", 10),
        ];
        table.apply(&delta).unwrap();
        table
            .apply(&SurfaceDelta {
                resources: vec![resource("b", "B renamed", 10)],
                removed: vec!["c".into()],
                ..SurfaceDelta::default()
            })
            .unwrap();
        assert_eq!(
            labels(&table),
            vec![
                ("a".into(), "apply".into(), "A".into()),
                ("a".into(), "open".into(), "A".into()),
                ("b".into(), "apply".into(), "B renamed".into()),
            ]
        );
    }

    #[test]
    fn a_delta_that_does_not_apply_changes_nothing() {
        let mut table = SurfaceTable::default();
        let mut delta = catalogue();
        delta.resources = vec![resource("a", "A", 11)];
        table.apply(&delta).unwrap();
        let before = labels(&table);
        let unknown_template = SurfaceDelta {
            resources: vec![resource("z", "Z", 99)],
            removed: vec!["a".into()],
            ..SurfaceDelta::default()
        };
        assert!(
            table
                .apply(&unknown_template)
                .unwrap_err()
                .contains("template")
        );
        let unknown_shape = SurfaceDelta {
            templates: vec![(id(20), vec![id(9)])],
            resources: vec![resource("z", "Z", 20)],
            ..SurfaceDelta::default()
        };
        assert!(table.apply(&unknown_shape).unwrap_err().contains("shape"));
        let carries_resource = SurfaceDelta {
            shapes: vec![(
                id(3),
                ExportedAction {
                    state: tilo_actions::ActionState::default(),
                    resource: "leak".into(),
                    ..shape("x", "X")
                },
            )],
            ..SurfaceDelta::default()
        };
        assert!(table.apply(&carries_resource).is_err());
        assert_eq!(labels(&table), before);
        // A reset cannot lean on what the broker held before it.
        let reset_without_catalogue = SurfaceDelta {
            reset: true,
            resources: vec![resource("a", "A", 11)],
            ..SurfaceDelta::default()
        };
        assert!(table.apply(&reset_without_catalogue).is_err());
        assert_eq!(labels(&table), before);
    }

    #[test]
    fn what_no_resource_names_is_dropped() {
        let mut table = SurfaceTable::default();
        let mut delta = catalogue();
        delta.resources = vec![resource("a", "A", 11)];
        table.apply(&delta).unwrap();
        table
            .apply(&SurfaceDelta {
                removed: vec!["a".into()],
                ..SurfaceDelta::default()
            })
            .unwrap();
        assert!(table.is_empty());
        let again = SurfaceDelta {
            resources: vec![resource("a", "A", 11)],
            ..SurfaceDelta::default()
        };
        assert!(
            table.apply(&again).is_err(),
            "the publisher must send the template again"
        );
    }

    #[test]
    fn two_deltas_folded_equal_the_two_applied_in_turn() {
        let mut base = catalogue();
        base.resources = vec![
            resource("a", "A", 11),
            resource("b", "B", 11),
            resource("c", "C", 10),
        ];
        let first = SurfaceDelta {
            resources: vec![resource("a", "A1", 10), resource("d", "D", 11)],
            removed: vec!["c".into()],
            ..SurfaceDelta::default()
        };
        let second = SurfaceDelta {
            resources: vec![resource("c", "C again", 10), resource("a", "A2", 11)],
            removed: vec!["d".into()],
            ..SurfaceDelta::default()
        };
        let mut turn = SurfaceTable::default();
        turn.apply(&base).unwrap();
        turn.apply(&first).unwrap();
        turn.apply(&second).unwrap();
        let sort = |mut rows: Vec<(String, String, String)>| {
            rows.compact_sort();
            rows
        };
        let mut folded = SurfaceTable::default();
        folded.apply(&base).unwrap();
        folded.apply(&first.clone().then(second.clone())).unwrap();
        assert_eq!(sort(labels(&folded)), sort(labels(&turn)));

        let mut from_reset = SurfaceTable::default();
        from_reset
            .apply(&base.clone().then(first.clone()).then(second))
            .unwrap();
        assert_eq!(sort(labels(&from_reset)), sort(labels(&turn)));
        let mut reset_again = catalogue();
        reset_again.resources = vec![resource("z", "Z", 10)];
        assert_eq!(first.then(reset_again.clone()), reset_again);
    }
}
