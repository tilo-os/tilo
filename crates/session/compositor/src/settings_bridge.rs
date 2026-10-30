//! The compositor's side of the display settings bridge: what the outputs
//! are and where they sit, published where Settings and the session read it.
//!
//! Settings is not allowed to probe DRM itself, so the process that owns KMS
//! is the one authority for which outputs exist, which modes and features
//! they can be driven with and where they sit right now. That is a
//! [`DisplayHardwareSnapshot`]: written to the private runtime directory for
//! Settings, and kept in memory for the session, which reads it on its own
//! loop.

use std::collections::BTreeMap;

use settings_store::hardware::{
    AdaptiveSyncUnavailable, DisplayHardwareSnapshot, DisplayOutputSnapshot, HdrUnavailable,
    LiveOutputPlacement, OutputDescriptor, OutputMode, OutputTransform, OutputUnavailable,
};
use smithay::output::{Mode as SmithayMode, Output};
use smithay::utils::Transform as SmithayTransform;

use crate::output::Monitor;

/// What the sink on an output and the pipeline driving it can do beyond what
/// the output itself reports.
///
/// A backend with no hardware (headless, a session in a window) has nothing
/// to add and returns none of these, which says of its simulated outputs
/// exactly what is true: no sink supports anything special.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Capability {
    /// The output's connector name, the key that joins these to an output.
    pub connector: String,
    /// A panel built into the machine.
    pub builtin: bool,
    /// Whether the display asks for HDR in its EDID, and why HDR cannot be
    /// driven here when it does.
    pub sink_hdr: bool,
    pub hdr_unavailable: Option<HdrUnavailable>,
    /// Whether the display reports adaptive sync, and why it cannot be used
    /// here when it does.
    pub sink_vrr: bool,
    pub vrr_unavailable: Option<AdaptiveSyncUnavailable>,
    /// Why a connected display is dark: the compositor tried to light it and
    /// could not.
    pub unavailable_reason: Option<OutputUnavailable>,
    /// The ICC profiles that may be chosen here, path → the name a person
    /// reads.
    pub color_profiles: BTreeMap<String, String>,
}

impl Capability {
    fn plain(connector: &str) -> Self {
        Self {
            connector: connector.to_owned(),
            ..Self::default()
        }
    }

    /// Whether HDR can be driven on this output.
    #[must_use]
    pub fn hdr(&self) -> bool {
        self.sink_hdr && self.hdr_unavailable.is_none()
    }

    /// Whether adaptive sync can be engaged on this output.
    #[must_use]
    pub fn vrr(&self) -> bool {
        self.sink_vrr && self.vrr_unavailable.is_none()
    }
}

/// The snapshot of `outputs` (`on` while the output is lit), with what
/// `capabilities` say about their sinks, `backend` naming how they are
/// driven and `publisher` this process's snapshot lease.
///
/// Without a lease (no runtime directory) the snapshot is still exact, only
/// untrusted by a reader, so it is in memory and never written to a file.
#[must_use]
pub fn snapshot(
    outputs: &[(Output, bool)],
    capabilities: &[Capability],
    backend: &str,
    publisher: Option<(u32, u64)>,
) -> DisplayHardwareSnapshot {
    let mut entries: Vec<DisplayOutputSnapshot> = Vec::with_capacity(outputs.len());
    let mut layout: Vec<LiveOutputPlacement> = Vec::new();
    for (output, on) in outputs {
        let name = output.name();
        let Some(mode) = output.current_mode() else {
            // An output nothing ever gave a mode is not a display yet.
            continue;
        };
        let capability = capabilities
            .iter()
            .find(|capability| capability.connector == name)
            .cloned()
            .unwrap_or_else(|| Capability::plain(&name));
        let (x, y, width, height) = rectangle(output, mode);
        if *on {
            layout.push(LiveOutputPlacement {
                connector: name.clone(),
                x,
                y,
                width,
                height,
            });
        }
        entries.push(DisplayOutputSnapshot {
            descriptor: OutputDescriptor {
                // A connector another GPU also has is qualified by its card;
                // what a person reads is the connector itself.
                connector_label: name
                    .rsplit_once('/')
                    .map_or_else(|| name.clone(), |(_, label)| label.to_owned()),
                connector: name.clone(),
                modes: modes_of(output),
                hdr: capability.hdr(),
                vrr: capability.vrr(),
                color_profiles: capability.color_profiles.keys().cloned().collect(),
                monitor: output
                    .user_data()
                    .get::<Monitor>()
                    .map(|Monitor(identity)| identity.clone()),
            },
            current_mode: mode_of(mode),
            scale: scale_of(output),
            builtin: capability.builtin,
            sink_hdr: capability.sink_hdr,
            hdr_unavailable_reason: capability.hdr_unavailable,
            sink_vrr: capability.sink_vrr,
            vrr_unavailable_reason: capability.vrr_unavailable,
            unavailable_reason: capability.unavailable_reason,
            color_profile_labels: capability.color_profiles,
        });
    }
    entries.sort_by(|left, right| left.descriptor.connector.cmp(&right.descriptor.connector));
    let (publisher_pid, publisher_start_ticks) = publisher.unwrap_or_default();
    DisplayHardwareSnapshot {
        generated_unix_ms: unix_now_ms(),
        backend: backend.to_owned(),
        publisher_pid,
        publisher_start_ticks,
        outputs: entries,
        layout,
    }
}

/// The current rotation of every output, by connector: what a layout that
/// names no rotation for an output keeps, and what a person reads as its
/// rotation now.
#[must_use]
pub fn rotations(outputs: &[(Output, bool)]) -> BTreeMap<String, OutputTransform> {
    outputs
        .iter()
        .map(|(output, _)| (output.name(), transform_of(output.current_transform())))
        .collect()
}

/// `output`'s rectangle in the logical desktop: its place, and its extent
/// after scale and rotation.
fn rectangle(output: &Output, mode: SmithayMode) -> (i32, i32, u32, u32) {
    let location = output.current_location();
    let (width, height) = extent(mode, scale_of(output), output.current_transform());
    (location.x, location.y, width, height)
}

/// The logical extent of `mode` at `scale`, turned with `transform`: what
/// clients are told the output's logical size is.
fn extent(mode: SmithayMode, scale: f64, transform: SmithayTransform) -> (u32, u32) {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a mode's size is non-negative and the scaled extent is clamped to u32"
    )]
    let axis = |pixels: i32| {
        (f64::from(pixels.max(0)) / scale)
            .round()
            .clamp(0.0, f64::from(u32::MAX)) as u32
    };
    let (width, height) = (axis(mode.size.w), axis(mode.size.h));
    if matches!(transform, SmithayTransform::_90 | SmithayTransform::_270) {
        (height, width)
    } else {
        (width, height)
    }
}

/// The scale clients render at; a value that is not a scale at all counts
/// as 1.
fn scale_of(output: &Output) -> f64 {
    let scale = output.current_scale().fractional_scale();
    if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    }
}

/// `output`'s modes as Settings persists them: smallest first, each once.
fn modes_of(output: &Output) -> Vec<OutputMode> {
    let mut modes: Vec<OutputMode> = output.modes().into_iter().map(mode_of).collect();
    modes.sort_by_key(|mode| (mode.width, mode.height, mode.refresh_mhz));
    modes.dedup();
    modes
}

/// One mode as Settings persists it.
#[allow(
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    reason = "a mode's size and milli-hertz refresh are non-negative by construction"
)]
fn mode_of(mode: SmithayMode) -> OutputMode {
    OutputMode {
        width: mode.size.w.max(0) as u32,
        height: mode.size.h.max(0) as u32,
        refresh_mhz: mode.refresh.max(0) as u32,
    }
}

/// The stored transform `transform` is: the four quarter turns, anything
/// mirrored as the turn it mirrors.
fn transform_of(transform: SmithayTransform) -> OutputTransform {
    if transform == SmithayTransform::_90 {
        OutputTransform::Rotate90
    } else if transform == SmithayTransform::_180 {
        OutputTransform::Rotate180
    } else if transform == SmithayTransform::_270 {
        OutputTransform::Rotate270
    } else {
        OutputTransform::Normal
    }
}

/// The snapshot this compositor last published, kept so a new one reaches
/// the file only when something about the outputs changed, and so the
/// session reads the outputs without reading the file.
#[derive(Default)]
pub(crate) struct Published {
    snapshot: Option<DisplayHardwareSnapshot>,
    /// This process's snapshot lease, looked for once.
    publisher: Option<(u32, u64)>,
    lease_looked_for: bool,
    /// Counts the snapshots this process has taken: readers that only need
    /// to know whether the displays changed compare it instead of copying
    /// the snapshot on every pass.
    generation: u64,
}

impl Published {
    /// Take `outputs` as they are now, with what `capabilities` say about
    /// their sinks driven by `backend`, and publish them if they differ from
    /// what was published last.
    pub(crate) fn refresh(
        &mut self,
        outputs: &[(Output, bool)],
        capabilities: &[Capability],
        backend: &str,
    ) {
        if !self.lease_looked_for {
            self.lease_looked_for = true;
            self.publisher = settings_store::hardware::publisher_identity().ok();
            if self.publisher.is_none() {
                tracing::debug!("no snapshot lease: Settings cannot read these displays");
            }
        }
        let snapshot = snapshot(outputs, capabilities, backend, self.publisher);
        if self
            .snapshot
            .as_ref()
            .is_some_and(|last| same(last, &snapshot))
        {
            return;
        }
        if self.publisher.is_some()
            && let Err(error) = snapshot.save()
        {
            tracing::debug!(%error, "the displays were not published for Settings");
        }
        self.snapshot = Some(snapshot);
        self.generation = self.generation.wrapping_add(1);
    }

    /// The outputs as they were when last published.
    pub(crate) fn snapshot(&self) -> Option<&DisplayHardwareSnapshot> {
        self.snapshot.as_ref()
    }

    /// How many snapshots have been taken: it changes exactly when the
    /// displays, their modes or their layout do.
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }
}

/// Whether `left` and `right` say the same about the outputs: everything but
/// when the snapshot was taken.
fn same(left: &DisplayHardwareSnapshot, right: &DisplayHardwareSnapshot) -> bool {
    left.backend == right.backend && left.outputs == right.outputs && left.layout == right.layout
}

/// Milliseconds since the Unix epoch: when a snapshot was taken.
fn unix_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        })
}

#[cfg(test)]
mod tests {
    use smithay::output::{Mode, PhysicalProperties, Scale, Subpixel};
    use smithay::utils::Transform;

    use super::*;

    /// An output with `size` as its only mode, turned and scaled.
    fn output(name: &str, size: (i32, i32), transform: Transform, scale: f64) -> Output {
        let output = Output::new(
            name.to_owned(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "Tilo".into(),
                model: "test".into(),
            },
        );
        let mode = Mode {
            size: size.into(),
            refresh: 60_000,
        };
        output.change_current_state(
            Some(mode),
            Some(transform),
            Some(Scale::Fractional(scale)),
            Some((0, 0).into()),
        );
        output.set_preferred(mode);
        output
    }

    #[test]
    fn a_turned_monitor_takes_its_sideways_size_in_the_layout() {
        let turned = output("DP-1", (800, 600), Transform::_90, 2.0);
        let snapshot = snapshot(&[(turned, true)], &[], "headless", None);
        let layout = &snapshot.layout[0];
        assert_eq!((layout.x, layout.y), (0, 0));
        assert_eq!(
            (layout.width, layout.height),
            (300, 400),
            "800x600 at 2x turned on its side is 300x400"
        );
        assert_eq!(snapshot.outputs[0].scale, 2.0);
        assert_eq!(
            (
                snapshot.outputs[0].current_mode.width,
                snapshot.outputs[0].current_mode.height,
                snapshot.outputs[0].current_mode.refresh_mhz
            ),
            (800, 600, 60_000),
            "Settings offers the mode the monitor shows"
        );
    }

    #[test]
    fn an_output_nothing_gave_a_mode_is_not_a_display_yet() {
        let bare = Output::new(
            "DP-2".to_owned(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "Tilo".into(),
                model: "test".into(),
            },
        );
        let snapshot = snapshot(&[(bare, true)], &[], "headless", None);
        assert!(snapshot.outputs.is_empty());
        assert!(snapshot.layout.is_empty());
    }

    #[test]
    fn a_snapshot_counts_as_the_same_whatever_second_it_was_taken_in() {
        let left = output("eDP-1", (1280, 800), Transform::Normal, 1.0);
        let first = snapshot(&[(left.clone(), true)], &[], "drm-kms", None);
        let second = snapshot(&[(left, true)], &[], "drm-kms", None);
        assert!(same(&first, &second));
        let rescaled = output("eDP-1", (1280, 800), Transform::Normal, 1.5);
        assert!(!same(
            &first,
            &snapshot(&[(rescaled, true)], &[], "drm-kms", None)
        ));
    }
}
