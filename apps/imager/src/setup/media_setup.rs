//! Compatibility facade for backend-owned native setup operations.
pub use imager_backend::setup::media_setup::*;

/// The image writer's part in a request that lays out a prepared
/// installation medium (original files carried to the new system). With the
/// installation these are the backend's checks and provisioning; the Tilo
/// app's writer, built without it, refuses such a request before touching
/// the drive and links none of it.
pub mod writer {
    use std::io;

    use crate::model::TargetDrive;
    use crate::write::OfflineLayout;

    /// The payload the migration data filesystem holds for `free` bytes.
    ///
    /// # Errors
    /// As the backend's bound; without the installation, always.
    pub fn payload_capacity(free: u64) -> io::Result<u64> {
        #[cfg(feature = "installation")]
        {
            crate::setup::portable_home_media::payload_capacity_for_free_bytes(free)
        }
        #[cfg(not(feature = "installation"))]
        {
            let _ = free;
            Err(refused())
        }
    }

    /// The authenticated caller may lay out `layout`.
    ///
    /// # Errors
    /// As the backend's check; without the installation, always.
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

    /// Lay out `layout` on the written `target`.
    ///
    /// # Errors
    /// As the backend's provisioning; without the installation, always.
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

    /// The disks `layout` carries files from are still the reviewed ones,
    /// and none of them is `target`.
    ///
    /// # Errors
    /// A changed or overlapping source; without the installation, always.
    pub fn validate_source_disks(target: &TargetDrive, layout: &OfflineLayout) -> io::Result<()> {
        #[cfg(feature = "installation")]
        {
            imager_backend::write::pending::validate_pending_source_disks(
                target,
                layout.target_number,
                |number| {
                    crate::setup::migration_handoff::validate_offline_source_disks(
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

    /// The opened `device` is the reviewed target `layout` was planned for.
    ///
    /// # Errors
    /// A replaced descriptor or changed source; without the installation,
    /// always.
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
                    crate::setup::migration_handoff::validate_offline_source_disks(
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

    /// Preparing an installation medium needs the installation.
    #[cfg(not(feature = "installation"))]
    fn refused() -> io::Error {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "Preparing an installation medium needs the Tilo Installer download",
        )
    }
}
