#[allow(
    clippy::wildcard_imports,
    reason = "one part of the broker module: the shared state, its imports and the other parts reach this through the parent glob"
)]
use super::*;

impl AppBrokerState {
    /// Snapshot every action surface currently published by a connected app.
    pub fn published_surfaces(&self) -> Vec<PublishedSurface> {
        let live = self
            .publishers
            .surfaces
            .lock()
            .map(|surfaces| surfaces.values().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        let live_keys = live
            .iter()
            .map(|surface| {
                (
                    surface.app_id.clone(),
                    surface.route.cluster_id.clone(),
                    surface.route.document_id.clone(),
                )
            })
            .collect::<std::collections::HashSet<_>>();
        let mut result = live;
        result.extend(self.launchable.surfaces().into_iter().filter(|surface| {
            !live_keys.contains(&(
                surface.app_id.clone(),
                surface.route.cluster_id.clone(),
                surface.route.document_id.clone(),
            ))
        }));
        result
    }
    /// Resolve against the current selected source, independently of the
    /// asynchronous search snapshot. A live source shadows its retained
    /// launchable catalogue even when it no longer offers the selected action.
    pub fn published_action(
        &self,
        route: &CrdtRoute,
        resource: &str,
        action: &str,
    ) -> Option<(PublishedSurface, PublishedAction)> {
        let surfaces = self.publishers.surfaces.lock().ok()?;
        let mut live_route = false;
        for surface in surfaces
            .values()
            .filter(|surface| surface.app_id == route.app_id && surface.route == *route)
        {
            live_route = true;
            if let Some(selected) = surface.action_projection(resource, action) {
                return Some(selected);
            }
        }
        let selected = if live_route {
            None
        } else {
            self.launchable.action_for(route, resource, action)
        };
        // Keep live-source shadowing coherent with the archive lookup; a
        // publisher must not install a live replacement between those reads.
        drop(surfaces);
        selected
    }
    /// The action `action` on `resource` as the peer `peer` publishes it on `route`, whoever
    /// else publishes the same: what an invocation that peer was handed is held to while it
    /// runs. Another window of the app on the same record publishes it too, and neither
    /// replaces the other's.
    pub(super) fn published_action_of(
        &self,
        peer: u64,
        route: &CrdtRoute,
        resource: &str,
        action: &str,
    ) -> Option<(PublishedSurface, PublishedAction)> {
        let surfaces = self.publishers.surfaces.lock().ok()?;
        surfaces
            .get(&peer)
            .filter(|surface| surface.app_id == route.app_id && surface.route == *route)?
            .action_projection(resource, action)
    }
    /// Check one live action route without cloning the published catalogue.
    pub fn has_live_published_action(
        &self,
        app_id: &str,
        cluster_id: &str,
        document_id: &str,
        resource: &str,
        action: &str,
    ) -> bool {
        self.publishers.surfaces.lock().is_ok_and(|surfaces| {
            surfaces.values().any(|surface| {
                surface.live
                    && surface.app_id == app_id
                    && surface.route.cluster_id == cluster_id
                    && surface.route.document_id == document_id
                    && surface.actions.iter().any(|descriptor| {
                        descriptor.resource == resource && descriptor.action == action
                    })
            })
        })
    }
    /// Generation of the eager action-surface source. Reading this never
    /// clones the potentially large application action inventory.
    #[must_use]
    pub fn published_surfaces_generation(&self) -> u64 {
        *self.publishers.changes.borrow()
    }
}
