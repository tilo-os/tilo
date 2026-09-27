//! KMS signalling beyond the framebuffer: adaptive sync on the CRTC.
//!
//! Every write goes through the legacy `SETPROPERTY` ioctl, which atomic
//! drivers turn into a one-property atomic commit. `VRR_ENABLED` is not an
//! atomic-only property, so this works on the legacy page-flip path Tilo
//! uses (the same route xf86-video-amdgpu takes).
//!
//! The decisions — which value to write, when to stop retrying — are kept in
//! plain state machines so they are tested without a DRM device.

use smithay::reexports::drm as drm_re;

use drm_re::control::{Device as ControlDevice, ResourceHandle, property};

/// Look up one named property on a KMS object, with its current value.
pub(super) fn find_property<D, T>(
    device: &D,
    object: T,
    name: &str,
) -> Option<(property::Handle, property::RawValue, property::Info)>
where
    D: ControlDevice,
    T: ResourceHandle,
{
    let properties = device.get_properties(object).ok()?;
    properties.iter().find_map(|(handle, value)| {
        let info = device.get_property(*handle).ok()?;
        (info.name().to_bytes() == name.as_bytes()).then_some((*handle, *value, info))
    })
}

/// What the presenter learned about one output's signalling at runtime. The
/// loop thread folds it into the Settings capability snapshot, so a feature a
/// driver refused is reported as unavailable, with the reason.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OutputSignalStatus {
    /// The driver refused to enable adaptive sync.
    pub adaptive_sync_rejected: bool,
}

/// Whether any CRTC of the device exposes `VRR_ENABLED`. Capability is
/// published before a connector is bound to a CRTC, and drivers expose the
/// property on every CRTC or none.
pub(super) fn device_has_adaptive_sync<D: ControlDevice>(device: &D) -> bool {
    device.resource_handles().is_ok_and(|resources| {
        resources
            .crtcs()
            .iter()
            .any(|&crtc| find_property(device, crtc, "VRR_ENABLED").is_some())
    })
}

/// Settings' view of adaptive sync on one connector: usable, or why not.
pub(super) fn adaptive_sync_capability(
    sink_capable: bool,
    driver_supports: bool,
    status: Option<&OutputSignalStatus>,
) -> (
    bool,
    Option<settings_store::hardware::AdaptiveSyncUnavailable>,
) {
    use settings_store::hardware::AdaptiveSyncUnavailable;
    if !sink_capable {
        return (false, None);
    }
    if !driver_supports {
        return (false, Some(AdaptiveSyncUnavailable::DriverUnsupported));
    }
    if status.is_some_and(|status| status.adaptive_sync_rejected) {
        return (false, Some(AdaptiveSyncUnavailable::DriverRejected));
    }
    (true, None)
}

/// Adaptive sync (VRR) on one CRTC: what the driver offers, what the
/// compositor wants, and what was last written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct AdaptiveSync {
    /// The CRTC's `VRR_ENABLED` property, when the driver exposes one.
    property: Option<property::Handle>,
    /// The connector's `vrr_capable` property read 1: the sink accepts it.
    sink_capable: bool,
    wanted: bool,
    /// Value the kernel holds, as far as this process knows. `None` after a
    /// session resume, when another DRM master may have changed it.
    applied: Option<bool>,
    /// The driver refused a write. Latched until the next session resume so
    /// a broken driver is not asked again on every page flip.
    rejected: bool,
}

impl AdaptiveSync {
    pub(super) fn probe<D: ControlDevice>(
        device: &D,
        crtc: drm_re::control::crtc::Handle,
        connector: drm_re::control::connector::Handle,
    ) -> Self {
        let property = find_property(device, crtc, "VRR_ENABLED").map(|(handle, _, _)| handle);
        let sink_capable =
            find_property(device, connector, "vrr_capable").is_some_and(|(_, value, _)| value == 1);
        Self::new(property, sink_capable)
    }

    pub(super) fn new(property: Option<property::Handle>, sink_capable: bool) -> Self {
        Self {
            property,
            sink_capable,
            wanted: false,
            // The property resets to off on a fresh modeset; writing "off"
            // again would be a pointless commit.
            applied: Some(false),
            rejected: false,
        }
    }

    pub(super) fn property(&self) -> Option<property::Handle> {
        self.property
    }

    /// Both ends can do it: the sink says so and the driver has the knob.
    pub(super) fn supported(&self) -> bool {
        self.property.is_some() && self.sink_capable
    }

    pub(super) fn rejected(&self) -> bool {
        self.rejected
    }

    /// Adaptive sync is on in the kernel right now.
    pub(super) fn active(&self) -> bool {
        self.applied == Some(true)
    }

    pub(super) fn want(&mut self, on: bool) {
        self.wanted = on;
    }

    /// After a session resume the kernel state is unknown and a previous
    /// rejection may have been a transient master-less failure.
    pub(super) fn forget_applied(&mut self) {
        self.applied = None;
        self.rejected = false;
    }

    /// The value to write before the next flip, if the kernel differs from
    /// what is wanted.
    pub(super) fn pending_write(&self) -> Option<bool> {
        if self.property.is_none() || self.rejected {
            return None;
        }
        let target = self.wanted && self.sink_capable;
        (self.applied != Some(target)).then_some(target)
    }

    pub(super) fn record_write(&mut self, value: bool, succeeded: bool) {
        if succeeded {
            self.applied = Some(value);
        } else {
            // A refused write leaves the kernel value where it was. Turning
            // it on is what fails in practice, which leaves it off.
            self.rejected = true;
            if value {
                self.applied = Some(false);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handle() -> property::Handle {
        property::Handle::from(std::num::NonZeroU32::new(7).expect("non-zero"))
    }

    #[test]
    fn adaptive_sync_capability_reports_the_first_missing_piece() {
        use settings_store::hardware::AdaptiveSyncUnavailable;
        assert_eq!(adaptive_sync_capability(false, true, None), (false, None));
        assert_eq!(
            adaptive_sync_capability(true, false, None),
            (false, Some(AdaptiveSyncUnavailable::DriverUnsupported))
        );
        let refused = OutputSignalStatus {
            adaptive_sync_rejected: true,
        };
        assert_eq!(
            adaptive_sync_capability(true, true, Some(&refused)),
            (false, Some(AdaptiveSyncUnavailable::DriverRejected))
        );
        assert_eq!(
            adaptive_sync_capability(true, true, Some(&OutputSignalStatus::default())),
            (true, None)
        );
    }

    #[test]
    fn adaptive_sync_writes_only_changes_and_stops_after_a_refusal() {
        // Phase `writes_follow_the_wanted_state_without_repeats`.
        {
            let mut sync = AdaptiveSync::new(Some(handle()), true);
            assert!(sync.supported());
            assert_eq!(sync.pending_write(), None, "fresh modeset is already off");
            sync.want(true);
            assert_eq!(sync.pending_write(), Some(true));
            sync.record_write(true, true);
            assert!(sync.active());
            assert_eq!(sync.pending_write(), None);
            sync.want(false);
            assert_eq!(sync.pending_write(), Some(false));
            sync.record_write(false, true);
            assert!(!sync.active());
        }
        // Phase `a_sink_without_vrr_or_a_driver_without_the_property_never_writes_on`.
        {
            let mut sink_cannot = AdaptiveSync::new(Some(handle()), false);
            sink_cannot.want(true);
            assert!(!sink_cannot.supported());
            assert_eq!(sink_cannot.pending_write(), None);
            let mut no_property = AdaptiveSync::new(None, true);
            no_property.want(true);
            assert!(!no_property.supported());
            assert_eq!(no_property.pending_write(), None);
        }
        // Phase `a_refused_write_latches_until_resume`.
        {
            let mut sync = AdaptiveSync::new(Some(handle()), true);
            sync.want(true);
            sync.record_write(true, false);
            assert!(sync.rejected());
            assert!(!sync.active());
            assert_eq!(sync.pending_write(), None, "no retry on every flip");
            sync.forget_applied();
            assert!(!sync.rejected());
            assert_eq!(sync.pending_write(), Some(true), "resume tries once more");
        }
        // Phase `resume_rewrites_even_an_unchanged_off_state`.
        {
            let mut sync = AdaptiveSync::new(Some(handle()), true);
            sync.forget_applied();
            assert_eq!(sync.pending_write(), Some(false));
        }
    }
}
