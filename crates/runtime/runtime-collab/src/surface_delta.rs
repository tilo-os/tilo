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
//!   [`ExportedAction`] with nothing of the resource it is offered on, named
//!   by a content hash;
//! * each distinct ordered set of shapes once, a *template*, named by a hash
//!   of its shapes' ids;
//! * each resource once, with the fields that are its own and the template of
//!   the actions it offers.
//!
//! The broker's [`SurfaceTable`] applies deltas and expands the result into
//! the same `Vec<ExportedAction>` a full `publishActions` installs, so
//! nothing that reads a surface changes.

use std::collections::{HashMap, HashSet};
use tilo_sort::CompactSort as _;

use tilo_actions::ResourceSemanticIdentity;

use crate::ExportedAction;

/// Bytes of a content hash naming a shape or a template.
pub const SHAPE_ID_BYTES: usize = 16;

/// A catalogue entry's or template's content hash, chosen by the publisher.
pub type ShapeId = [u8; SHAPE_ID_BYTES];

/// The fields of a resource every one of its actions carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceFields {
    pub resource_kind: String,
    pub resource_label: String,
    pub semantic_identity: Option<ResourceSemanticIdentity>,
}

/// One resource: the fields that are its own, and the template of the actions
/// it offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SurfaceResource {
    pub resource: String,
    pub fields: ResourceFields,
    pub template: ShapeId,
}

impl ExportedAction {
    /// The catalogue entry of this action: everything but its resource's own
    /// fields, which a [`SurfaceResource`] carries.
    #[must_use]
    pub fn shape(&self) -> Self {
        Self {
            resource: String::new(),
            semantic_identity: None,
            resource_kind: String::new(),
            resource_label: String::new(),
            ..self.clone()
        }
    }

    /// The fields of this action's resource.
    #[must_use]
    pub fn resource_fields(&self) -> ResourceFields {
        ResourceFields {
            resource_kind: self.resource_kind.clone(),
            resource_label: self.resource_label.clone(),
            semantic_identity: self.semantic_identity.clone(),
        }
    }
}

/// A change to a connection's published surface.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
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

    /// Apply `delta`, or change nothing and say why it does not apply.
    ///
    /// # Errors
    ///
    /// A shape that carries a resource's own fields, a template naming a
    /// shape neither this delta nor the table holds, or a resource naming a
    /// template neither holds.
    pub fn apply(&mut self, delta: &SurfaceDelta) -> Result<(), String> {
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
                actions.push(ExportedAction {
                    resource: resource.resource.clone(),
                    semantic_identity: resource.fields.semantic_identity.clone(),
                    resource_kind: resource.fields.resource_kind.clone(),
                    resource_label: resource.fields.resource_label.clone(),
                    ..shape.clone()
                });
            }
        }
        actions
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
            resource: String::new(),
            semantic_identity: None,
            action: action.into(),
            resource_kind: String::new(),
            resource_label: String::new(),
            action_label: label.into(),
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

        // Folding onto a reset keeps it a reset, and a later reset wins.
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
