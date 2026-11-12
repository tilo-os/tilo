//! The action catalogue an agent follows, as changes.
//!
//! The catalogue (every surface the broker lets one follower invoke) changes
//! with nearly every publication, while each change touches one surface and
//! a few of its resources. A [`CatalogueChange`] carries only that: per
//! surface a [`SurfaceDelta`], and the surfaces added or gone. Its shapes
//! and templates travel once for the whole catalogue: a second window of an
//! app offers what the first one did, so it costs its resources alone. The broker
//! keeps a [`CataloguePublication`] per follower, which applies each change
//! to its [`ActionCatalogue`].

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::{
    BrokerActionSurface, ExportedAction, ShapeId, SurfaceDelta, SurfacePublication, SurfaceTable,
};

/// One surface of the catalogue: everything about it but its actions.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct CatalogueSurfaceId {
    pub peer_id: u64,
    #[serde(default)]
    pub provider: Option<String>,
    pub app_id: String,
    pub cluster_id: String,
    pub record_id: String,
    pub live: bool,
}

/// A surface added or changed, as the delta from what the follower holds of
/// it; a reset delta for a surface the follower does not hold.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CatalogueSurfaceChange {
    pub surface: CatalogueSurfaceId,
    pub delta: SurfaceDelta,
}

/// A change to a follower's catalogue. Applies to the catalogue at `since`
/// and leaves it at `revision`; a `reset` applies to any.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CatalogueChange {
    pub since: u64,
    pub revision: u64,
    /// Discard the catalogue held so far before applying the rest.
    pub reset: bool,
    /// Surfaces no longer offered, applied before `surfaces`.
    pub removed: Vec<CatalogueSurfaceId>,
    /// The shapes and templates `surfaces` name that the follower holds in
    /// none of its surfaces; their own deltas carry none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shapes: Vec<(ShapeId, ExportedAction)>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub templates: Vec<(ShapeId, Vec<ShapeId>)>,
    pub surfaces: Vec<CatalogueSurfaceChange>,
}

impl CatalogueChange {
    /// Whether applying it changes nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        !self.reset && self.removed.is_empty() && self.surfaces.is_empty()
    }

    /// The whole catalogue `surfaces` as one reset.
    #[must_use]
    pub fn snapshot(surfaces: &[BrokerActionSurface]) -> Self {
        let surfaces: Vec<_> = surfaces
            .iter()
            .map(|surface| (surface.id(), Arc::from(surface.actions.as_slice())))
            .collect();
        CataloguePublication::default().next(&surfaces)
    }

    /// The one change equivalent to applying `self` and then `newer`, for a
    /// follower that has not been sent `self` yet.
    #[must_use]
    pub fn then(mut self, newer: Self) -> Self {
        if newer.reset {
            return newer;
        }
        // A shape stays whatever becomes of the surface it came with: a
        // later surface may name it as held.
        let held: HashSet<ShapeId> = self.shapes.iter().map(|(id, _)| *id).collect();
        self.shapes.extend(
            newer
                .shapes
                .into_iter()
                .filter(|(id, _)| !held.contains(id)),
        );
        let held: HashSet<ShapeId> = self.templates.iter().map(|(id, _)| *id).collect();
        self.templates.extend(
            newer
                .templates
                .into_iter()
                .filter(|(id, _)| !held.contains(id)),
        );
        for gone in newer.removed {
            self.surfaces.retain(|change| change.surface != gone);
            if !self.reset && !self.removed.contains(&gone) {
                self.removed.push(gone);
            }
        }
        for change in newer.surfaces {
            if let Some(older) = self
                .surfaces
                .iter_mut()
                .find(|older| older.surface == change.surface)
            {
                older.delta = std::mem::take(&mut older.delta).then(change.delta);
            } else {
                self.surfaces.push(change);
            }
        }
        self.revision = newer.revision;
        self
    }
}

struct Held {
    publication: SurfacePublication,
    actions: Arc<[ExportedAction]>,
}

/// What one follower holds of the catalogue, as the broker last sent it.
#[derive(Default)]
pub struct CataloguePublication {
    revision: u64,
    held: HashMap<CatalogueSurfaceId, Held>,
}

impl CataloguePublication {
    /// The change from what the follower holds to `surfaces`, which it holds
    /// from now on; the first is a reset. Empty when nothing changed.
    pub fn next(
        &mut self,
        surfaces: &[(CatalogueSurfaceId, Arc<[ExportedAction]>)],
    ) -> CatalogueChange {
        let reset = self.revision == 0;
        let held_templates: HashMap<ShapeId, &[ShapeId]> = self
            .held
            .values()
            .flat_map(|held| held.publication.templates())
            .map(|(id, members)| (*id, members.as_slice()))
            .collect();
        let held_shapes: HashSet<ShapeId> = held_templates
            .values()
            .flat_map(|members| members.iter())
            .copied()
            .collect();
        let held_templates: HashSet<ShapeId> = held_templates.into_keys().collect();
        let (mut sent_shapes, mut sent_templates) = (HashSet::new(), HashSet::new());
        let mut change = CatalogueChange {
            since: self.revision,
            revision: self.revision.saturating_add(1),
            reset,
            ..CatalogueChange::default()
        };
        let mut prior = std::mem::take(&mut self.held);
        for (id, actions) in surfaces {
            if actions.is_empty() || self.held.contains_key(id) {
                continue;
            }
            let (delta, publication) = match prior.remove(id) {
                Some(held) if Arc::ptr_eq(&held.actions, actions) => {
                    self.held.insert(id.clone(), held);
                    continue;
                }
                Some(held) => {
                    SurfacePublication::next(Some((&held.publication, &held.actions)), actions)
                }
                None => SurfacePublication::next(None, actions),
            };
            let mut delta = delta;
            for (id, shape) in std::mem::take(&mut delta.shapes) {
                if !held_shapes.contains(&id) && sent_shapes.insert(id) {
                    change.shapes.push((id, shape));
                }
            }
            for (id, members) in std::mem::take(&mut delta.templates) {
                if !held_templates.contains(&id) && sent_templates.insert(id) {
                    change.templates.push((id, members));
                }
            }
            if !delta.is_empty() {
                change.surfaces.push(CatalogueSurfaceChange {
                    surface: id.clone(),
                    delta,
                });
            }
            self.held.insert(
                id.clone(),
                Held {
                    publication,
                    actions: Arc::clone(actions),
                },
            );
        }
        if !reset {
            change.removed = prior.into_keys().collect();
        }
        if !change.is_empty() {
            self.revision = change.revision;
        }
        change
    }
}

/// Shapes and templates a follower holds or a change brings, by id.
#[derive(Default)]
struct Pool<'a> {
    shapes: HashMap<&'a ShapeId, &'a ExportedAction>,
    templates: HashMap<&'a ShapeId, &'a Vec<ShapeId>>,
}

/// `delta` with the templates its resources name and the shapes those name
/// from `pool`, when neither it nor the surface's table `own` holds them.
fn filled<'a>(
    delta: &'a SurfaceDelta,
    own: Option<&SurfaceTable>,
    pool: &Pool<'_>,
) -> Cow<'a, SurfaceDelta> {
    let mut templates: Vec<(ShapeId, Vec<ShapeId>)> = Vec::new();
    for resource in &delta.resources {
        let id = &resource.template;
        if own.is_some_and(|own| own.holds_template(id))
            || delta
                .templates
                .iter()
                .chain(&templates)
                .any(|(held, _)| held == id)
        {
            continue;
        }
        if let Some(members) = pool.templates.get(id) {
            templates.push((*id, (*members).clone()));
        }
    }
    let mut carried: HashSet<&ShapeId> = delta.shapes.iter().map(|(id, _)| id).collect();
    let mut shapes: Vec<(ShapeId, ExportedAction)> = Vec::new();
    for member in delta
        .templates
        .iter()
        .chain(&templates)
        .flat_map(|(_, members)| members)
    {
        if !own.is_some_and(|own| own.holds(member))
            && carried.insert(member)
            && let Some(shape) = pool.shapes.get(member)
        {
            shapes.push((*member, (*shape).clone()));
        }
    }
    if templates.is_empty() && shapes.is_empty() {
        return Cow::Borrowed(delta);
    }
    let mut delta = delta.clone();
    delta.templates.extend(templates);
    delta.shapes.extend(shapes);
    Cow::Owned(delta)
}

/// The catalogue as a follower holds it, kept by applying each change.
#[derive(Clone, Debug, Default)]
pub struct ActionCatalogue {
    revision: u64,
    tables: Vec<SurfaceTable>,
    surfaces: Vec<BrokerActionSurface>,
}

impl ActionCatalogue {
    /// Apply `change`. One that does not follow what is held (a bug on either
    /// side) leaves the catalogue empty rather than wrong.
    pub fn apply(&mut self, change: &CatalogueChange) -> Result<(), String> {
        let applied = self.try_apply(change);
        if applied.is_err() {
            *self = Self::default();
        }
        applied
    }

    fn try_apply(&mut self, change: &CatalogueChange) -> Result<(), String> {
        // Each surface's delta with the templates and shapes it names that
        // came with the change or that another surface held before it.
        let deltas: Vec<Cow<'_, SurfaceDelta>> = {
            let mut pool = Pool::default();
            if !change.reset {
                for table in &self.tables {
                    pool.shapes.extend(table.shapes());
                    pool.templates.extend(table.templates());
                }
            }
            pool.shapes
                .extend(change.shapes.iter().map(|(id, shape)| (id, shape)));
            pool.templates
                .extend(change.templates.iter().map(|(id, members)| (id, members)));
            change
                .surfaces
                .iter()
                .map(|surface| {
                    let own = self
                        .position(&surface.surface)
                        .and_then(|index| self.tables.get(index))
                        .filter(|_| !change.reset && !surface.delta.reset);
                    filled(&surface.delta, own, &pool)
                })
                .collect()
        };
        if change.reset {
            self.tables.clear();
            self.surfaces.clear();
        } else if change.since != self.revision {
            return Err(format!(
                "catalogue change from revision {} does not follow revision {}",
                change.since, self.revision
            ));
        }
        for gone in &change.removed {
            if let Some(index) = self.position(gone) {
                self.tables.remove(index);
                self.surfaces.remove(index);
            }
        }
        for (surface, delta) in change.surfaces.iter().zip(&deltas) {
            let index = match self.position(&surface.surface) {
                Some(index) => index,
                None if surface.delta.reset => {
                    self.tables.push(SurfaceTable::default());
                    self.surfaces
                        .push(BrokerActionSurface::with_id(surface.surface.clone()));
                    self.tables.len().saturating_sub(1)
                }
                None => {
                    return Err(format!(
                        "catalogue change names surface {:?} not held",
                        surface.surface
                    ));
                }
            };
            let (Some(table), Some(held)) =
                (self.tables.get_mut(index), self.surfaces.get_mut(index))
            else {
                continue;
            };
            table.apply(delta)?;
            held.actions = table.actions();
            if table.is_empty() {
                self.tables.remove(index);
                self.surfaces.remove(index);
            }
        }
        self.revision = change.revision;
        Ok(())
    }

    fn position(&self, id: &CatalogueSurfaceId) -> Option<usize> {
        self.surfaces.iter().position(|surface| surface.id() == *id)
    }

    /// Every surface held, in the order each was first offered.
    #[must_use]
    pub fn surfaces(&self) -> &[BrokerActionSurface] {
        &self.surfaces
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A window (connection `peer`) offering four actions on each of
    /// `messages` messages, the first `edited` of them relabelled.
    fn window(peer: u64, messages: usize, edited: usize) -> BrokerActionSurface {
        let action = |index: usize, verb: &str| ExportedAction {
            state: tilo_actions::ActionState::default(),
            resource: format!("chat.message:{index:08}"),
            semantic_identity: None,
            action: verb.into(),
            resource_kind: "chat.message".into(),
            resource_label: format!(
                "Message {index}{}",
                if index < edited { ", edited" } else { "" }
            ),
            action_label: format!("{verb} message"),
            keywords: vec!["message".into()],
            icon: Some("message-square".into()),
            description: Some(format!(
                "{verb} this message, keeping its thread and the people in it."
            )),
            operation_kind: tilo_actions::OperationKind::Document,
            params: vec![tilo_actions::ActionParam::required(
                "text",
                tilo_actions::ParamType::Text,
                "What to write",
            )],
            contract: tilo_actions::ActionContract::default(),
            view: None,
            input: crate::ExportedInput::None,
        };
        BrokerActionSurface {
            actions: (0..messages)
                .flat_map(|index| {
                    ["reply", "react", "edit", "delete"].map(|verb| action(index, verb))
                })
                .collect(),
            ..BrokerActionSurface::with_id(CatalogueSurfaceId {
                peer_id: peer,
                provider: None,
                app_id: format!("app-{peer}"),
                cluster_id: "workspace".into(),
                record_id: String::new(),
                live: true,
            })
        }
    }

    fn rows(surfaces: &[BrokerActionSurface]) -> Vec<(CatalogueSurfaceId, Arc<[ExportedAction]>)> {
        surfaces
            .iter()
            .map(|surface| (surface.id(), Arc::from(surface.actions.as_slice())))
            .collect()
    }

    #[test]
    fn a_follower_holds_what_the_broker_offers_after_every_change() {
        let mut publication = CataloguePublication::default();
        let mut held = ActionCatalogue::default();
        for surfaces in [
            vec![window(1, 3, 0), window(2, 2, 0)],
            vec![window(1, 4, 1), window(2, 2, 0)],
            vec![window(1, 4, 1), window(3, 1, 0)],
            vec![],
            vec![window(2, 5, 5)],
        ] {
            held.apply(&publication.next(&rows(&surfaces))).unwrap();
            assert_eq!(held.surfaces(), surfaces.as_slice());
        }
        assert!(publication.next(&rows(&[window(2, 5, 5)])).is_empty());
    }

    #[test]
    fn unread_changes_composed_equal_the_changes_applied_in_turn() {
        let mut publication = CataloguePublication::default();
        let first = publication.next(&rows(&[window(1, 3, 0), window(2, 1, 0)]));
        let (mut stepwise, mut composed) = (ActionCatalogue::default(), ActionCatalogue::default());
        stepwise.apply(&first).unwrap();
        composed.apply(&first).unwrap();
        let mut pending = CatalogueChange::default();
        for surfaces in [
            vec![window(1, 4, 2)],
            vec![window(2, 2, 0), window(1, 4, 2)],
            vec![window(1, 1, 0)],
        ] {
            let change = publication.next(&rows(&surfaces));
            stepwise.apply(&change).unwrap();
            pending = if pending.revision == 0 {
                change
            } else {
                pending.then(change)
            };
        }
        composed.apply(&pending).unwrap();
        assert_eq!(composed.surfaces(), stepwise.surfaces());

        let _skipped = publication.next(&rows(&[window(1, 2, 0)]));
        let next = publication.next(&rows(&[window(1, 3, 0)]));
        assert!(
            stepwise.apply(&next).is_err(),
            "a change that does not follow"
        );
        assert!(stepwise.surfaces().is_empty(), "leaves nothing held");
    }

    /// A second window of an app offers the shapes the first one does: it
    /// costs its resources alone, also when the surface that brought the
    /// shapes is gone before the follower reads the changes.
    #[test]
    fn a_second_window_costs_its_resources_alone() {
        let mut publication = CataloguePublication::default();
        let mut held = ActionCatalogue::default();
        held.apply(&publication.next(&rows(&[window(1, 3, 0)])))
            .unwrap();
        let second = publication.next(&rows(&[window(1, 3, 0), window(2, 3, 0)]));
        assert!(second.shapes.is_empty() && second.templates.is_empty());
        assert!(second.surfaces.iter().all(|surface| {
            surface.delta.shapes.is_empty() && surface.delta.templates.is_empty()
        }));
        let first_gone = publication.next(&rows(&[window(2, 3, 0)]));
        let third = publication.next(&rows(&[window(3, 2, 0)]));
        held.apply(&second.then(first_gone).then(third)).unwrap();
        assert_eq!(held.surfaces(), [window(3, 2, 0)]);

        let mut publication = CataloguePublication::default();
        let mut held = ActionCatalogue::default();
        held.apply(&publication.next(&[])).unwrap();
        let brought = publication.next(&rows(&[window(1, 3, 0)]));
        let replaced = publication.next(&rows(&[window(2, 3, 0)]));
        held.apply(&brought.then(replaced)).unwrap();
        assert_eq!(held.surfaces(), [window(2, 3, 0)]);
    }

    /// Actions whose words name the resource they are on share one shape,
    /// and read back as they were published.
    #[test]
    fn words_naming_the_resource_share_one_shape() {
        let named = |messages| {
            let mut surface = window(1, messages, 0);
            for action in &mut surface.actions {
                action.action_label = format!("{} {}", action.action, action.resource_label);
                action.description = Some(format!("\u{1}L {}\u{1}", action.resource_label));
            }
            surface
        };
        let change = CataloguePublication::default().next(&rows(&[named(5)]));
        assert_eq!(change.shapes.len(), 4);
        let mut held = ActionCatalogue::default();
        held.apply(&change).unwrap();
        assert_eq!(held.surfaces(), [named(5)]);
    }

    /// A whole catalogue is sent once, each action shape in it once; a
    /// change of one message carries that message alone.
    #[test]
    fn a_change_of_one_message_carries_that_message() {
        let before = [window(1, 200, 0), window(2, 60, 0)];
        let whole: usize = before
            .iter()
            .map(|surface| serde_json::to_string(&surface.actions).unwrap().len())
            .sum();
        let mut publication = CataloguePublication::default();
        let bytes = |change: CatalogueChange| serde_json::to_string(&change).unwrap().len();
        let snapshot = bytes(publication.next(&rows(&before)));
        let change = bytes(publication.next(&rows(&[window(1, 200, 1), window(2, 60, 0)])));
        eprintln!("catalogue bytes: whole {whole}, snapshot {snapshot}, one message {change}");
        assert!(snapshot * 3 < whole);
        assert!(change < 1_000);
    }
}
