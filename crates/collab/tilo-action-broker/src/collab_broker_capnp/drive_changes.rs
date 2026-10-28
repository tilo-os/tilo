//! The certified Files route's wait for the shell's drive list to change
//! (`drivesChanged`): content-free, so Files asks the tools service for the
//! list again instead of asking on a timer.

use crate::services::HostServices;

/// The wait for the drive list's next change past `after` for a connection
/// on `route_app`: only the certified Files route may wait, as only Files
/// lists drives.
pub(super) fn drive_change(
    services: &HostServices,
    route_app: &str,
    after: u64,
) -> Result<crate::services::DriveChange, capnp::Error> {
    if route_app != super::NEARBY_TRANSFER_APP_ID {
        return Err(capnp::Error::failed(
            "drives are listed only to the certified Files application".into(),
        ));
    }
    services
        .drives
        .as_ref()
        .and_then(|service| service.changed(after))
        .ok_or_else(|| {
            capnp::Error::unimplemented("this host does not say when its drives change".into())
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// Drives that record each wait and answer it with the next revision,
    /// or (`announces: false`) cannot say when they change.
    struct WaitingDrives {
        announces: bool,
        waits: Mutex<Vec<u64>>,
    }

    impl crate::services::DriveServices for WaitingDrives {
        fn changed(&self, after: u64) -> Option<crate::services::DriveChange> {
            self.waits.lock().expect("waits").push(after);
            self.announces
                .then(|| Box::pin(std::future::ready(after.wrapping_add(1))) as _)
        }
    }

    #[test]
    fn only_the_certified_files_route_waits_for_drive_changes() {
        let service = Arc::new(WaitingDrives {
            announces: true,
            waits: Mutex::new(Vec::new()),
        });
        let services = HostServices {
            drives: Some(service.clone()),
            ..Default::default()
        };
        let change = drive_change(&services, super::super::NEARBY_TRANSFER_APP_ID, 4)
            .expect("Files waits on its route");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        assert_eq!(runtime.block_on(change), 5, "the host's next change");

        for app in ["so.tilo.settings", "so.tilo.disks", ""] {
            let refused = drive_change(&services, app, 0)
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
                drives: Some(Arc::new(WaitingDrives {
                    announces: false,
                    waits: Mutex::new(Vec::new()),
                })),
                ..Default::default()
            },
        ] {
            let unannounced = drive_change(&services, super::super::NEARBY_TRANSFER_APP_ID, 0)
                .err()
                .expect("a host that cannot announce changes says so");
            assert_eq!(unannounced.kind, capnp::ErrorKind::Unimplemented);
        }
    }
}
