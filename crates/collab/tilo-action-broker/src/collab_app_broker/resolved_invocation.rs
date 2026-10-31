//! Revision-bound invocation through the original certified channel.
use super::{
    AppBrokerState, BrokerActor, DIRECTORY_MAX_QUERY_REVISION_BYTES, Duration,
    LocalActionDirectoryRegistry, PrivateWorkerRegistration, PublishedAction, PublishedSurface, Rc,
    RegisteredActionDirectory, published_directory_action, validate_directory_identifier,
};

enum ResolutionRevision<'a> {
    Live(&'a str),
    RetainedLocal(&'a str),
}

struct CapturedResolution {
    registered: RegisteredActionDirectory,
    worker: Option<PrivateWorkerRegistration>,
    resolution: runtime_collab::ActionDirectoryResolution,
    permit: tokio::sync::OwnedSemaphorePermit,
}

pub(super) struct ResolvedOffer {
    pub(super) registered: RegisteredActionDirectory,
    published: PublishedAction,
    worker: Option<PrivateWorkerRegistration>,
    _permit: tokio::sync::OwnedSemaphorePermit,
}

fn check_registration(
    registered: &RegisteredActionDirectory,
    worker: Option<&PrivateWorkerRegistration>,
    state: &AppBrokerState,
    directories: &LocalActionDirectoryRegistry,
    peer: u64,
) -> Result<(), String> {
    let current = directories
        .registered(peer, &registered.route.cluster_id)
        .map_err(|error| error.to_string())?;
    if !Rc::ptr_eq(&current.registration, &registered.registration)
        || current.app_id != registered.app_id
        || current.route != registered.route
    {
        return Err("application directory registration changed during resolution".into());
    }
    let current_worker = state
        .host_workers
        .lock()
        .map_err(|_| "worker registry unavailable")?
        .get(peer, &registered.route)
        .and_then(|current| current.worker_registration.clone());
    if current_worker.as_ref() != worker {
        return Err("application worker connection changed during resolution".into());
    }
    Ok(())
}
impl ResolvedOffer {
    pub(super) fn has_original_worker(&self) -> bool {
        self.worker.is_some()
    }

    pub(super) fn check_registration(
        &self,
        state: &AppBrokerState,
        directories: &LocalActionDirectoryRegistry,
        peer: u64,
    ) -> Result<(), String> {
        check_registration(
            &self.registered,
            self.worker.as_ref(),
            state,
            directories,
            peer,
        )
    }
    pub(super) fn offered(&self, peer_id: u64) -> (PublishedSurface, PublishedAction) {
        (
            PublishedSurface {
                peer_id,
                app_id: self.registered.app_id.clone(),
                route: self.registered.route.clone(),
                actions: vec![self.published.clone()],
                live: true,
            },
            self.published.clone(),
        )
    }
}
impl CapturedResolution {}
impl ResolvedOffer {}
impl AppBrokerState {
    #[expect(
        clippy::future_not_send,
        reason = "runs on the broker's single-threaded LocalSet"
    )]
    async fn capture_directory_resolution(
        &self,
        directories: &LocalActionDirectoryRegistry,
        actor: &BrokerActor,
        cluster: &str,
        peer: u64,
        revision: ResolutionRevision<'_>,
        resource: &str,
        action: &str,
    ) -> Result<CapturedResolution, String> {
        let permit = directories
            .resolved_permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| "resolved invocation capacity exhausted")?;
        let registered = directories
            .registered(peer, cluster)
            .map_err(|error| error.to_string())?;
        let worker = self
            .host_workers
            .lock()
            .map_err(|_| "worker registry unavailable")?
            .get(peer, &registered.route)
            .and_then(|current| current.worker_registration.clone());
        let resolution = tokio::time::timeout(Duration::from_secs(2), async {
            match revision {
                ResolutionRevision::Live(revision) => {
                    self.resolve_action_directory_for_actor(
                        directories,
                        actor,
                        cluster,
                        peer,
                        revision,
                        resource,
                        action,
                    )
                    .await
                }
                ResolutionRevision::RetainedLocal(revision) => {
                    self.resolve_local_document_directory_for_actor(
                        directories,
                        actor,
                        cluster,
                        peer,
                        (revision, resource, action),
                    )
                    .await
                }
            }
        })
        .await
        .map_err(|_| "action directory resolution timeout")??;
        check_registration(&registered, worker.as_ref(), self, directories, peer)?;
        Ok(CapturedResolution {
            registered,
            worker,
            resolution,
            permit,
        })
    }
    #[expect(
        clippy::future_not_send,
        reason = "runs on the broker's single-threaded LocalSet"
    )]
    pub(super) async fn resolve_invocation_offer(
        &self,
        directories: &LocalActionDirectoryRegistry,
        actor: &BrokerActor,
        cluster: &str,
        peer: u64,
        revision: &str,
        resource: &str,
        action: &str,
    ) -> Result<ResolvedOffer, String> {
        validate_directory_identifier(
            revision,
            "invocation revision",
            DIRECTORY_MAX_QUERY_REVISION_BYTES,
            true,
        )
        .map_err(|error| error.to_string())?;
        let captured = self
            .capture_directory_resolution(
                directories,
                actor,
                cluster,
                peer,
                ResolutionRevision::Live(revision),
                resource,
                action,
            )
            .await?;
        let runtime_collab::ActionDirectoryResolution::Found { row, .. } = captured.resolution
        else {
            return Err("directory revision or action changed".into());
        };
        let published = published_directory_action(&row).map_err(|error| error.to_string())?;
        Ok(ResolvedOffer {
            registered: captured.registered,
            published,
            worker: captured.worker,
            _permit: captured.permit,
        })
    }

    #[expect(
        clippy::future_not_send,
        reason = "runs on the broker's single-threaded LocalSet"
    )]
    pub(super) async fn resolve_local_document_offer(
        &self,
        directories: &LocalActionDirectoryRegistry,
        actor: &BrokerActor,
        cluster: &str,
        peer: u64,
        revision: &str,
        resource: &str,
        action: &str,
    ) -> Result<ResolvedOffer, String> {
        validate_directory_identifier(
            revision,
            "invocation revision",
            DIRECTORY_MAX_QUERY_REVISION_BYTES,
            true,
        )
        .map_err(|error| error.to_string())?;
        let captured = self
            .capture_directory_resolution(
                directories,
                actor,
                cluster,
                peer,
                ResolutionRevision::RetainedLocal(revision),
                resource,
                action,
            )
            .await?;
        let runtime_collab::ActionDirectoryResolution::Found { row, .. } = captured.resolution
        else {
            return Err("directory revision or action changed".into());
        };
        let published = published_directory_action(&row).map_err(|error| error.to_string())?;
        Ok(ResolvedOffer {
            registered: captured.registered,
            published,
            worker: captured.worker,
            _permit: captured.permit,
        })
    }
}
