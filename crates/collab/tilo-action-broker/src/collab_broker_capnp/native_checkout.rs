//! Current certified document checkout for an attested local native worker.
//! This inert owner diagnostic never creates an invocation or document grant.
use super::{
    ActionDirectoryRoot, AdmittedJoinIdentity, Arc, CapnpBrokerState, CapnpChannelHandle,
    CrdtRoute, native_media::LocalAppConnection,
};

#[derive(Clone)]
pub(super) struct NativeCheckout {
    route: CrdtRoute,
    identity: AdmittedJoinIdentity,
    connection: Arc<LocalAppConnection>,
}

impl CapnpChannelHandle {
    pub(super) fn retain_native_checkout(&self, route: &CrdtRoute) {
        let connection = self
            .lease
            .as_ref()
            .and_then(|lease| lease.local_media.clone());
        if let Ok(mut checkouts) = self.state.native_checkouts.lock() {
            checkouts.remove(&self.peer_id);
            if let Some(connection) = connection {
                checkouts.insert(
                    self.peer_id,
                    NativeCheckout {
                        route: route.clone(),
                        identity: self.verified_identity.clone(),
                        connection,
                    },
                );
            }
        }
    }
}

impl NativeCheckout {
    fn matches(&self, app: &str, room: &str, instance: u64, pid: u32, start: u64) -> bool {
        self.route.app_id == app
            && self.route.cluster_id == room
            && self.connection.app_id == app
            && self.connection.workspace_id == room
            && self.connection.continuation_instance == Some(instance)
            && self.connection.matches_process(pid, start)
    }
}

impl CapnpBrokerState {
    /// Only the compositor's authenticated current worker tuple selects this
    /// projection. No continuity fallback is treated as the current document.
    pub(crate) fn native_checkout(
        &self,
        app: &str,
        room: &str,
        instance: u64,
        pid: u32,
        start: u64,
    ) -> Option<ActionDirectoryRoot> {
        let candidates: Vec<_> = self
            .native_checkouts
            .lock()
            .ok()?
            .iter()
            .filter(|(_, checkout)| {
                checkout.route.app_id == app
                    && checkout.route.cluster_id == room
                    && checkout.connection.continuation_instance == Some(instance)
            })
            .map(|(peer, checkout)| (*peer, checkout.clone()))
            .take(129)
            .collect();
        if candidates.len() > 128 {
            return None;
        }
        let mut matched = None;
        for (peer, checkout) in candidates {
            if !checkout.matches(app, room, instance, pid, start)
                || self
                    .revalidate_admission(&checkout.route, &checkout.identity)
                    .is_err()
            {
                continue;
            }
            let roots = self.action_directory_roots.lock().ok()?;
            let registered = roots.get(&peer)?.clone();
            drop(roots);
            if registered.app_id != checkout.route.app_id
                || registered.cluster_id != checkout.route.cluster_id
                || registered.record_id != checkout.route.record_id
            {
                continue;
            }
            let current = self
                .native_checkouts
                .lock()
                .ok()?
                .get(&peer)
                .is_some_and(|current| {
                    current.route == checkout.route
                        && current.identity == checkout.identity
                        && Arc::ptr_eq(&current.connection, &checkout.connection)
                });
            if !current || !checkout.matches(app, room, instance, pid, start) {
                return None;
            }
            // Multiple current channels are not evidence of one checkout.
            if matched.replace(registered).is_some() {
                return None;
            }
        }
        matched
    }
}
