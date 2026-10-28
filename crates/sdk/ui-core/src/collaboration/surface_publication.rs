//! Publishing an application's action surface as deltas.
//!
//! The surface is level-triggered and large: every page of a notebook offers
//! the same editing actions, each with the same parameter schema and
//! contract. [`SurfacePublication`] remembers what the broker holds and turns
//! the next surface into a [`SurfaceDelta`]: each distinct catalogue entry
//! once under its content hash, each distinct set of them once as a
//! template, and each resource whose actions or fields changed once, with the
//! fields that are its own and the template of its actions.

use std::collections::{HashMap, HashSet};
use tilo_sort::CompactSort as _;

use runtime_collab::{ExportedAction, ShapeId, SurfaceDelta, SurfaceResource};
use tilo_actions::ActionDescriptor;

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
    pub fn next(
        prior: Option<(&Self, &[ActionDescriptor])>,
        descriptors: &[ActionDescriptor],
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
            let name = descriptor.address.resource.as_str();
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
                let exported = ExportedAction::from(descriptor);
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
}

fn hash16(bytes: &[u8]) -> ShapeId {
    let hash = blake3::hash(bytes);
    let mut id = ShapeId::default();
    let len = id.len();
    id.copy_from_slice(&hash.as_bytes()[..len]);
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
