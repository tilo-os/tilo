//! The certified Files route's wait for the tools service's app list to
//! change (`applicationsChanged`): content-free, so Files asks the tools
//! service for the list again instead of asking on a timer.

use crate::services::HostServices;

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
