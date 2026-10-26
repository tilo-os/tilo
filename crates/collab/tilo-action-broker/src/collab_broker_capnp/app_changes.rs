//! The certified Files route's wait for the tools service's app list to
//! change (`applicationsChanged`): content-free, so Files asks the tools
//! service for the list again instead of asking on a timer.

use crate::services::{HostServices, NearbyTransferChange};

/// The wait for the app list's next change past `after` for a connection on
/// `route_app`: only the certified Files route may wait, as only Files lists
/// the person's apps.
pub(super) fn app_change(
    services: &HostServices,
    route_app: &str,
    after: u64,
) -> Result<crate::services::ApplicationChange, capnp::Error> {
    if route_app != super::NEARBY_TRANSFER_APP_ID {
        return Err(capnp::Error::failed(
            "the person's apps are listed only to the certified Files application".into(),
        ));
    }
    services
        .applications
        .as_ref()
        .and_then(|service| service.changed(after))
        .ok_or_else(|| {
            capnp::Error::unimplemented("this host does not say when its apps change".into())
        })
}

impl super::CapnpChannelHandle {
    /// Answer `applicationsChanged`: the admission rechecked for the route
    /// that asked, then the wait for the app list's next change.
    pub(super) fn answered_about_app_changes(
        &self,
        params: &super::channel_handle::ApplicationsChangedParams,
        results: super::channel_handle::ApplicationsChangedResults,
    ) -> capnp::capability::Promise<(), capnp::Error> {
        let route = super::CrdtRoute::from_channel_key(&self.channel_id);
        if let Err(error) = self
            .state
            .revalidate_admission(&route, &self.verified_identity)
        {
            return capnp::capability::Promise::err(error);
        }
        let Ok(params) = params.get() else {
            return capnp::capability::Promise::err(capnp::Error::failed(
                "a malformed request".into(),
            ));
        };
        answer(
            &self.state.services,
            &route.app_id,
            params.get_after(),
            results,
        )
    }
}

/// Answer `applicationsChanged` for a connection on `route_app`: the wait
/// `app_change` hands back, sent as the change to wait from next.
fn answer(
    services: &HostServices,
    route_app: &str,
    after: u64,
    results: super::channel_handle::ApplicationsChangedResults,
) -> capnp::capability::Promise<(), capnp::Error> {
    match app_change(services, route_app, after) {
        Ok(change) => reply(change, results),
        Err(error) => capnp::capability::Promise::err(error),
    }
}

/// Send `change` as the reply: the revision to wait from next.
fn reply(
    change: NearbyTransferChange,
    mut results: super::channel_handle::ApplicationsChangedResults,
) -> capnp::capability::Promise<(), capnp::Error> {
    capnp::capability::Promise::from_future(async move {
        results.get().set_change(change.await);
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// Apps that record each wait and answer it with the next revision, or
    /// (`announces: false`) cannot say when they change.
    struct WaitingApps {
        announces: bool,
        waits: Mutex<Vec<u64>>,
    }

    impl crate::services::ApplicationServices for WaitingApps {
        fn changed(&self, after: u64) -> Option<crate::services::ApplicationChange> {
            self.waits.lock().expect("waits").push(after);
            self.announces
                .then(|| Box::pin(std::future::ready(after.wrapping_add(1))) as _)
        }
    }

    #[test]
    fn only_the_certified_files_route_waits_for_app_changes() {
        let service = Arc::new(WaitingApps {
            announces: true,
            waits: Mutex::new(Vec::new()),
        });
        let services = HostServices {
            applications: Some(service.clone()),
            ..Default::default()
        };
        let change = app_change(&services, super::super::NEARBY_TRANSFER_APP_ID, 4)
            .expect("Files waits on its route");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        assert_eq!(runtime.block_on(change), 5, "the host's next change");

        for app in ["so.tilo.settings", "so.tilo.app-store", ""] {
            let refused = app_change(&services, app, 0)
                .err()
                .expect("only the certified Files route waits");
            assert!(refused.to_string().contains("certified Files"));
        }
        assert_eq!(
            *service.waits.lock().expect("waits"),
            [4],
            "a refused route never reaches the host"
        );

        for services in [
            HostServices::default(),
            HostServices {
                applications: Some(Arc::new(WaitingApps {
                    announces: false,
                    waits: Mutex::new(Vec::new()),
                })),
                ..Default::default()
            },
        ] {
            let unannounced = app_change(&services, super::super::NEARBY_TRANSFER_APP_ID, 0)
                .err()
                .expect("a host that cannot announce changes says so");
            assert_eq!(unannounced.kind, capnp::ErrorKind::Unimplemented);
        }
    }
}
