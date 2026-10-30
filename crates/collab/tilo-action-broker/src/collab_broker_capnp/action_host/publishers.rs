//! Which connection of an app takes an action: the one that publishes it
//! (as named, or bound to the record it shows), can receive invocations and,
//! for an action that shows something, has a window to show it in.

use tilo_actions::remote::{APPLICATION_OPEN_ACTION, ActionHostError};

use super::super::{CapnpBrokerState, PublishedAction};
use super::LaunchDestination;

impl CapnpBrokerState {
    pub(crate) fn is_windowless(&self, peer_id: u64) -> bool {
        self.launchable
            .windowless_peers
            .lock()
            .is_ok_and(|windowless| windowless.contains(&peer_id))
    }

    /// Whether `address` in `app_id` shows something, and so is taken only
    /// by a window of the app: its `open`, and any action of the view kind
    /// it declares (an issue's page, its chat), as published in `room` or
    /// offered by its manifest. An instance with no window has nothing to
    /// show it in.
    pub(crate) fn action_shows(
        &self,
        app_id: &str,
        room: &str,
        address: &tilo_actions::ActionAddress,
    ) -> bool {
        if address.action == APPLICATION_OPEN_ACTION {
            return true;
        }
        let logical = tilo_actions::selection::logical_resource_id(&address.resource);
        self.published_surfaces()
            .into_iter()
            .chain(self.launchable.surfaces())
            .filter(|surface| surface.app_id == app_id && surface.route.cluster_id == room)
            .flat_map(|surface| surface.actions)
            .find(|action| {
                action.action == address.action
                    && tilo_actions::selection::logical_resource_id(&action.resource) == logical
            })
            .or_else(|| self.manifest_offer(app_id, address))
            .is_some_and(|action| action.operation_kind == tilo_actions::OperationKind::View)
    }

    /// The refusal of an action no app took within `timeout`, naming what
    /// each of the app's connections lacks ([`Self::why_no_publisher`]).
    pub(super) fn offered_too_late(
        &self,
        app_id: &str,
        room: &str,
        destination: &LaunchDestination,
        address: &tilo_actions::ActionAddress,
        timeout: std::time::Duration,
    ) -> ActionHostError {
        let why = self.why_no_publisher(app_id, room, destination.record.as_deref(), address);
        tracing::warn!(%app_id, action = %address.action, %why, "an action found no app to take it");
        ActionHostError::Unavailable {
            reason: format!(
                "{app_id} was launched but did not offer \"{}\" within {:.1} s ({why})",
                address.action,
                timeout.as_secs_f32()
            ),
        }
    }

    /// Why no connection of `app_id` in `room` is [ready](Self::ready_publisher) to take
    /// `address`: each of the app's surfaces there and what it lacks, said when a wait for one
    /// ends, so the refusal names the link that is missing.
    pub(crate) fn why_no_publisher(
        &self,
        app_id: &str,
        room: &str,
        record: Option<&str>,
        address: &tilo_actions::ActionAddress,
    ) -> String {
        let (Ok(subscribers), Ok(surfaces)) =
            (self.invocation_subscribers.lock(), self.surfaces.lock())
        else {
            return "the broker's tables are unavailable".into();
        };
        let mut said: Vec<String> = surfaces
            .values()
            .filter(|surface| surface.app_id == app_id && surface.route.cluster_id == room)
            .map(|surface| {
                let mut lacks = Vec::new();
                if !surface.live {
                    lacks.push("not live");
                }
                if !self.shows(surface.peer_id, &address.action) {
                    lacks.push("windowless, and the action shows something");
                }
                if record.is_some_and(|record| surface.route.record_id != record) {
                    lacks.push("another record");
                }
                if !subscribers.contains_key(&surface.peer_id) {
                    lacks.push("no invocation stream");
                }
                if published_as(&surface.actions, address).is_none() {
                    lacks.push("does not publish the action");
                }
                format!(
                    "peer {} on record {:?}: {}",
                    surface.peer_id,
                    surface.route.record_id,
                    if lacks.is_empty() {
                        "ready".into()
                    } else {
                        lacks.join(", ")
                    }
                )
            })
            .collect();
        if said.is_empty() {
            said.push(format!("no surface of {app_id} in this room"));
        }
        said.join("; ")
    }

    /// The live connection of `app_id` in `room` that publishes `address`
    /// and can receive invocations.
    pub(crate) fn ready_publisher(
        &self,
        app_id: &str,
        room: &str,
        record: Option<&str>,
        address: &tilo_actions::ActionAddress,
    ) -> Option<u64> {
        self.ready_publication(app_id, room, record, address)
            .map(|(peer, _)| peer)
    }

    /// [`Self::ready_publisher`], and the address it publishes the action
    /// under. A caller may name the resource as the app's kind declares it
    /// (`tasks.tracker`, the logical id a manifest offer and a caller that
    /// holds no window name it by) while a window publishes it bound to the
    /// record it shows; both name one resource
    /// ([`tilo_actions::selection::logical_resource_id`]), and the action is
    /// delivered under the address the app published.
    pub(crate) fn ready_publication(
        &self,
        app_id: &str,
        room: &str,
        record: Option<&str>,
        address: &tilo_actions::ActionAddress,
    ) -> Option<(u64, tilo_actions::ActionAddress)> {
        // Asked before the tables are held: it reads them itself.
        let shows = self.action_shows(app_id, room, address);
        let subscribers = self.invocation_subscribers.lock().ok()?;
        let surfaces = self.surfaces.lock().ok()?;
        surfaces
            .values()
            .filter(|surface| {
                surface.live
                    && self.shows(surface.peer_id, &address.action)
                    && surface.app_id == app_id
                    && surface.route.cluster_id == room
                    && record.is_none_or(|record| surface.route.record_id == record)
                    && subscribers.contains_key(&surface.peer_id)
            })
            .filter_map(|surface| {
                published_as(&surface.actions, address)
                    .map(|published| (surface.peer_id, published))
            })
            .filter(|(peer, _)| !(shows && self.is_windowless(*peer)))
            .min_by_key(|(peer, _)| *peer)
    }
}

/// The address under which `actions` publish `address`: as named, else bound
/// to a record of the resource it names
/// ([`tilo_actions::selection::logical_resource_id`]).
fn published_as(
    actions: &[PublishedAction],
    address: &tilo_actions::ActionAddress,
) -> Option<tilo_actions::ActionAddress> {
    let named = |action: &&PublishedAction| {
        action.action == address.action && action.resource == address.resource
    };
    let logical = tilo_actions::selection::logical_resource_id(&address.resource);
    let bound = |action: &&PublishedAction| {
        action.action == address.action
            && tilo_actions::selection::logical_resource_id(&action.resource) == logical
    };
    actions
        .iter()
        .find(named)
        .or_else(|| actions.iter().find(bound))
        .map(|action| tilo_actions::ActionAddress::new(&action.resource, &action.action))
}
