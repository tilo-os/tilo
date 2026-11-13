//! Compatibility facade for backend-owned native setup operations.

pub use imager_backend::setup::media_setup::*;

pub mod writer {
    use std::io;

    use crate::model::TargetDrive;
    use crate::write::OfflineLayout;

    pub fn payload_capacity(free: u64) -> io::Result<u64> {
        #[cfg(feature = "installation")]
        {
            imager_backend::setup::portable_home_media::payload_capacity_for_free_bytes(free)
        }
        #[cfg(not(feature = "installation"))]
        {
            let _ = free;
            Err(refused())
        }
    }

    pub fn validate_caller(layout: &OfflineLayout, caller: u32) -> io::Result<()> {
        #[cfg(feature = "installation")]
        {
            super::validate_pending_caller(layout, caller)
        }
        #[cfg(not(feature = "installation"))]
        {
            let _ = (layout, caller);
            Err(refused())
        }
    }

    pub fn provision_after_write(
        target: &TargetDrive,
        layout: &OfflineLayout,
        caller: u32,
        should_cancel: &impl Fn() -> bool,
    ) -> io::Result<()> {
        #[cfg(feature = "installation")]
        {
            super::provision_after_write(target, layout, caller, should_cancel)
        }
        #[cfg(not(feature = "installation"))]
        {
            let _ = (target, layout, caller, should_cancel);
            Err(refused())
        }
    }

    pub fn validate_source_disks(target: &TargetDrive, layout: &OfflineLayout) -> io::Result<()> {
        #[cfg(feature = "installation")]
        {
            imager_backend::write::pending::validate_pending_source_disks(
                target,
                layout.target_number,
                |number| {
                    imager_backend::setup::migration_handoff::validate_offline_source_disks(
                        &layout.source_plans,
                        number,
                    )
                },
            )
        }
        #[cfg(not(feature = "installation"))]
        {
            let _ = (target, layout);
            Err(refused())
        }
    }

    pub fn validate_descriptor(
        target: &TargetDrive,
        device: &std::fs::File,
        layout: &OfflineLayout,
    ) -> io::Result<()> {
        #[cfg(feature = "installation")]
        {
            imager_backend::write::pending::validate_pending_descriptor(
                target,
                device,
                layout.target_number,
                |number| {
                    imager_backend::setup::migration_handoff::validate_offline_source_disks(
                        &layout.source_plans,
                        number,
                    )
                },
            )
        }
        #[cfg(not(feature = "installation"))]
        {
            let _ = (target, device, layout);
            Err(refused())
        }
    }

    #[cfg(not(feature = "installation"))]
    fn refused() -> io::Error {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "Preparing an installation medium needs the Tilo Installer download",
        )
    }
}
