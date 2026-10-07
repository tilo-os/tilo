//! Closed custody for the ordinary launcher-attested canonical Chat publisher.
//! This is not a Person permission grant or an Agent execution credential.
use super::{
    AdmittedJoinIdentity, Arc, CapnpBrokerState, CapnpCollabService, CrdtRoute,
    PrivateWorkerRegistration, native_media,
};

#[derive(Clone)]
pub(super) struct LauncherCustody(Arc<native_media::LocalAppConnection>);

impl LauncherCustody {
    pub(super) fn current(&self) -> bool {
        self.0.current()
    }
    fn matches(&self, local: &Arc<native_media::LocalAppConnection>) -> bool {
        Arc::ptr_eq(&self.0, local)
    }
}

impl CapnpBrokerState {
    pub(super) fn retire_launcher_message_publisher(
        &self,
        peer: u64,
        local: &Arc<native_media::LocalAppConnection>,
    ) {
        let Ok(_admission) = self.admission_lock.lock() else {
            return;
        };
        let matches = self.host_workers.lock().is_ok_and(|workers| {
            workers
                .get(&peer)
                .and_then(|entry| entry.worker_registration.as_ref())
                .and_then(|registration| registration.launcher.as_ref())
                .is_some_and(|custody| custody.matches(local))
        });
        if !matches {
            return;
        }
        if let Ok(mut sources) = self.message_sources.lock() {
            sources.retire(Some(peer));
        }
        if let Ok(mut directories) = self.canonical_directories.lock() {
            directories.remove(peer);
        }
        // Historical uncertainty stays in its separately bounded immutable
        // ledger. This registry only retains authority for live publishers.
        if let Ok(mut workers) = self.host_workers.lock() {
            workers.remove(&peer);
        }
    }
}
impl std::fmt::Debug for LauncherCustody {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("LauncherCustody(..)")
    }
}
impl PartialEq for LauncherCustody {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for LauncherCustody {}

impl CapnpCollabService {
    pub(super) fn launcher_message_registration(
        &self,
        route: &CrdtRoute,
        admitted: &AdmittedJoinIdentity,
        participant_kind: &str,
    ) -> Result<Option<PrivateWorkerRegistration>, capnp::Error> {
        // Subscriber and source custody are per connection, not per joined
        // handle. A Chat launcher connection must never create another app's
        // or Agent's handle that could replace its Person invocation stream.
        if self.worker.is_none()
            && let Some(local) = self.lease.local_media.as_ref()
            && local.app_id == "so.tilo.chat"
        {
            if !local.current()
                || local.app_id != route.app_id
                || local.workspace_id != route.cluster_id
            {
                return Err(capnp::Error::failed(
                    "launcher Chat connection route or process changed".into(),
                ));
            }
            if !matches!(participant_kind, "" | "person")
                || admitted.agent_id.is_some()
                || admitted.managed_peer.is_some()
                || admitted.host_scope.is_some()
                || admitted
                    .certificate
                    .as_ref()
                    .is_some_and(|cert| cert.capabilities.can_act_as_host)
            {
                return Err(capnp::Error::failed(
                    "launcher Chat connection requires its certified Person".into(),
                ));
            }
        }
        if self.worker.is_some()
            || route.app_id != "so.tilo.chat"
            || !route.record_id.is_empty()
            || !matches!(participant_kind, "" | "person")
            || admitted.agent_id.is_some()
            || admitted.managed_peer.is_some()
            || admitted.host_scope.is_some()
        {
            return Ok(None);
        }
        let Some(certificate) = admitted
            .certificate
            .as_ref()
            .filter(|cert| !cert.capabilities.can_act_as_host)
        else {
            return Ok(None);
        };
        let Some(local) = self.lease.local_media.as_ref() else {
            return Ok(None);
        };
        if !local.current()
            || local.app_id != route.app_id
            || local.workspace_id != route.cluster_id
        {
            return Err(capnp::Error::failed(
                "launcher Message publisher route or process changed".into(),
            ));
        }
        let custody = LauncherCustody(local.clone());
        let retained = self
            .state
            .host_workers
            .lock()
            .map_err(|_| capnp::Error::failed("launcher publisher registry unavailable".into()))?
            .get(&self.peer_id)
            .and_then(|entry| entry.worker_registration.clone());
        if let Some(retained) = retained {
            if retained.route != *route
                || retained.certificate != *certificate
                || retained.launcher.as_ref() != Some(&custody)
                || !retained.current()
            {
                return Err(capnp::Error::failed(
                    "launcher Message publisher registration changed".into(),
                ));
            }
            return Ok(Some(retained));
        }
        Ok(Some(PrivateWorkerRegistration {
            identity: uuid::Uuid::new_v4(),
            route: route.clone(),
            certificate: certificate.clone(),
            launcher: Some(custody),
        }))
    }
}
