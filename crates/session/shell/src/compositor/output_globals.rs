//! `wl_output` globals, kept in step with the backend's output set.
//!
//! Without a `wl_output` global, no client can learn what displays exist,
//! at what size or scale — so a toolkit can't pick a buffer scale, and
//! plugging in a monitor is invisible to anything running in the session.
//!
//! This module owns the mapping. The decision half — which globals to
//! create, update or destroy — is [`plan`], a pure function over plain
//! data; the [`OutputGlobals`] registry performs it against a live
//! `DisplayHandle`.
//!
//! A `wl_output` that is destroyed and re-created reads to a client as a
//! monitor vanishing and a different one appearing (`leave`/`enter`, a
//! toolkit re-running scale logic, a visible re-layout), so a display
//! that merely changed mode keeps its global and gets updated in place.
//! Matching by [`OutputId`] — which the DRM registry mints per
//! `(device, connector)` and keeps stable across re-enumeration — is what
//! makes that possible.
//!
//! A different monitor on the same connector keeps the [`OutputId`] but is
//! still a different display to a client — `wl_output` sends its name,
//! make and model only once per bind — so an identity change replaces the
//! global.

use std::collections::HashMap;

use smithay::output::{
    Mode as OutputMode, Output as WlOutputHandle, PhysicalProperties, Scale, Subpixel,
};
use smithay::reexports::wayland_server::DisplayHandle;
use smithay::reexports::wayland_server::backend::GlobalId;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Size, Transform};

use super::backend::{Output, OutputId, OutputIdentity};

/// One change to the advertised set.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum GlobalChange {
    /// No global exists for this output yet — advertise one.
    Create(OutputId),
    /// The output exists but its size, position or scale moved. Update the
    /// existing global rather than replacing it, so clients see a mode
    /// change instead of a monitor disappearing.
    Update(OutputId),
    /// A different monitor now sits behind this output — withdraw the old
    /// global and advertise a new one, since clients never re-read a bound
    /// output's name, make or model.
    Replace(OutputId),
    /// The output is gone — withdraw its global.
    Destroy(OutputId),
}

/// What one advertised output currently looks like. Compared against the
/// backend's list to decide whether an update is needed at all.
#[derive(Clone, Debug, PartialEq)]
pub struct AdvertisedState {
    pub size: (u32, u32),
    /// Where clients are told the output sits: its logical position.
    pub position: (i32, i32),
    pub scale: f32,
    pub transform: settings_store::hardware::OutputTransform,
    pub refresh_mhz: Option<u32>,
    pub identity: OutputIdentity,
}

impl From<&Output> for AdvertisedState {
    fn from(o: &Output) -> Self {
        Self {
            size: o.size,
            position: o.logical_position,
            scale: o.scale,
            transform: o.transform,
            refresh_mhz: o.refresh_mhz,
            identity: o.identity.clone(),
        }
    }
}

/// Diff the advertised set against the backend's.
///
/// Returned in destroy-last order so a caller applying them in sequence
/// never has two globals claiming the same identity, and so the
/// re-creation of a re-moded output cannot race its own teardown.
pub fn plan(advertised: &[(OutputId, AdvertisedState)], desired: &[Output]) -> Vec<GlobalChange> {
    let mut changes = Vec::new();
    for want in desired {
        match advertised.iter().find(|(id, _)| *id == want.id) {
            None => changes.push(GlobalChange::Create(want.id)),
            Some((_, have)) if have.identity != want.identity => {
                changes.push(GlobalChange::Replace(want.id));
            }
            Some((_, have)) if *have != AdvertisedState::from(want) => {
                changes.push(GlobalChange::Update(want.id));
            }
            Some(_) => {}
        }
    }
    for (id, _) in advertised {
        if !desired.iter().any(|o| o.id == *id) {
            changes.push(GlobalChange::Destroy(*id));
        }
    }
    changes
}

/// A `wl_output` the compositor is advertising.
struct LiveGlobal {
    handle: WlOutputHandle,
    global: GlobalId,
    state: AdvertisedState,
}

/// The advertised `wl_output` set.
#[derive(Default)]
pub struct OutputGlobals {
    live: HashMap<OutputId, LiveGlobal>,
}

impl OutputGlobals {
    pub fn new() -> Self {
        Self::default()
    }

    /// Output ids currently advertised, sorted — for tests and logging.
    pub fn advertised_ids(&self) -> Vec<OutputId> {
        let mut ids: Vec<OutputId> = self.live.keys().copied().collect();
        ids.sort_by_key(|i| i.0);
        ids
    }

    pub fn len(&self) -> usize {
        self.live.len()
    }

    pub fn is_empty(&self) -> bool {
        self.live.is_empty()
    }

    /// The `wl_output` handle for an output, if it is advertised. The
    /// surface-enter/leave bookkeeping needs it.
    pub fn handle(&self, id: OutputId) -> Option<&WlOutputHandle> {
        self.live.get(&id).map(|l| &l.handle)
    }

    /// Resolve a client-provided `wl_output` resource back to the stable
    /// backend identity.  xdg-shell fullscreen requests carry the resource,
    /// while hotplug/mode reconciliation is keyed by [`OutputId`].
    pub fn id_for_resource(
        &self,
        resource: &smithay::reexports::wayland_server::protocol::wl_output::WlOutput,
    ) -> Option<OutputId> {
        let requested = WlOutputHandle::from_resource(resource)?;
        self.live
            .iter()
            .find_map(|(id, live)| (live.handle == requested).then_some(*id))
    }

    /// Send the `wl_surface.enter`/`leave` events a membership reconcile
    /// decided on, on each output's own `wl_output`.
    ///
    /// Call ordering matters: removed outputs' leaves go out before
    /// [`Self::sync`] withdraws their globals, and current outputs' enters
    /// after it created them — otherwise there is no client-side output
    /// object to name in the event.
    pub fn send_surface_changes(
        &self,
        surfaces: &HashMap<u64, WlSurface>,
        changes: Vec<super::surface_outputs::Change>,
    ) {
        use super::surface_outputs::Change;
        for change in changes {
            let (surface_id, output_id) = match change {
                Change::Enter { surface_id, output } | Change::Leave { surface_id, output } => {
                    (surface_id, output)
                }
            };
            let Some(surface) = surfaces.get(&surface_id) else {
                // Destruction needs no final protocol leave: the client has
                // already destroyed the resource. The tracker drops the entry
                // on its next geometry reconciliation.
                continue;
            };
            let Some(output) = self.handle(output_id) else {
                // An invariant breach rather than a retryable state (see the
                // ordering above): there is no client-side output object on
                // which the event could be sent.
                tracing::error!(
                    surface_id,
                    output = output_id.0,
                    "wl_surface output transition has no advertised wl_output handle"
                );
                continue;
            };
            match change {
                Change::Enter { .. } => output.enter(surface),
                Change::Leave { .. } => output.leave(surface),
            }
        }
    }

    /// Snapshot of what is advertised, in the shape [`plan`] compares.
    fn snapshot(&self) -> Vec<(OutputId, AdvertisedState)> {
        let mut v: Vec<(OutputId, AdvertisedState)> = self
            .live
            .iter()
            .map(|(id, l)| (*id, l.state.clone()))
            .collect();
        v.sort_by_key(|(id, _)| id.0);
        v
    }

    /// Bring the advertised set in line with `desired`.
    ///
    /// Idempotent: called on every output change and safe to call when
    /// nothing moved, because [`plan`] returns an empty list then and no
    /// wire traffic is generated.
    ///
    /// Each output advertises its own mode's refresh rate, in millihertz;
    /// clients pace their frames to it. An output that reports none gets
    /// `fallback_refresh_mhz` (the compositor's pacing rate), and failing
    /// that the conventional 60 Hz rather than zero, which some toolkits
    /// divide by.
    pub fn sync<D>(
        &mut self,
        dh: &DisplayHandle,
        desired: &[Output],
        fallback_refresh_mhz: Option<u32>,
    ) where
        D: smithay::reexports::wayland_server::GlobalDispatch<
                smithay::reexports::wayland_server::protocol::wl_output::WlOutput,
                smithay::wayland::output::WlOutputData,
            > + 'static,
    {
        for change in plan(&self.snapshot(), desired) {
            match change {
                GlobalChange::Create(id) | GlobalChange::Replace(id) => {
                    let Some(out) = desired.iter().find(|o| o.id == id) else {
                        continue;
                    };
                    if let Some(old) = self.live.remove(&id) {
                        dh.remove_global::<D>(old.global);
                        tracing::info!(output = id.0, "wl_output: withdrawn for a new monitor");
                    }
                    let handle = WlOutputHandle::new(
                        advertised_name(out, desired),
                        PhysicalProperties {
                            // Zero when neither EDID nor the connector
                            // reported a size: a fabricated one would give
                            // clients a wrong DPI.
                            size: Size::from((
                                i32::try_from(out.identity.physical_mm.0).unwrap_or(0),
                                i32::try_from(out.identity.physical_mm.1).unwrap_or(0),
                            )),
                            subpixel: Subpixel::Unknown,
                            make: non_empty_or(&out.identity.make, "Unknown"),
                            model: non_empty_or(&out.identity.model, "Unknown"),
                        },
                    );
                    let mode = mode_for(out, fallback_refresh_mhz);
                    handle.change_current_state(
                        Some(mode),
                        Some(wayland_transform(out.transform)),
                        Some(Scale::Fractional(f64::from(out.scale))),
                        Some(out.logical_position.into()),
                    );
                    handle.set_preferred(mode);
                    handle
                        .user_data()
                        .insert_if_missing_threadsafe(|| BackendOutput(id));
                    let global = handle.create_global::<D>(dh);
                    tracing::info!(
                        output = id.0,
                        name = %handle.name(),
                        description = %handle.description(),
                        size = ?out.size,
                        position = ?out.logical_position,
                        scale = out.scale,
                        "wl_output: advertised"
                    );
                    self.live.insert(
                        id,
                        LiveGlobal {
                            handle,
                            global,
                            state: AdvertisedState::from(out),
                        },
                    );
                }
                GlobalChange::Update(id) => {
                    let Some(out) = desired.iter().find(|o| o.id == id) else {
                        continue;
                    };
                    let Some(live) = self.live.get_mut(&id) else {
                        continue;
                    };
                    let mode = mode_for(out, fallback_refresh_mhz);
                    live.handle.change_current_state(
                        Some(mode),
                        Some(wayland_transform(out.transform)),
                        Some(Scale::Fractional(f64::from(out.scale))),
                        Some(out.logical_position.into()),
                    );
                    live.handle.set_preferred(mode);
                    live.state = AdvertisedState::from(out);
                    tracing::info!(
                        output = id.0,
                        size = ?out.size,
                        position = ?out.logical_position,
                        scale = out.scale,
                        "wl_output: updated in place"
                    );
                }
                GlobalChange::Destroy(id) => {
                    if let Some(live) = self.live.remove(&id) {
                        dh.remove_global::<D>(live.global);
                        tracing::info!(output = id.0, "wl_output: withdrawn");
                    }
                }
            }
        }
    }
}

/// The backend output a `wl_output` global stands for, stored on its
/// Smithay handle when the global is created.
#[derive(Clone, Copy, Debug)]
struct BackendOutput(OutputId);

/// The backend output a client's `wl_output` resource names, for protocols
/// that take a `wl_output` argument (fullscreen, output power). `None` for
/// an output that is not one of the compositor's advertised globals.
pub fn output_id_of(
    resource: &smithay::reexports::wayland_server::protocol::wl_output::WlOutput,
) -> Option<OutputId> {
    WlOutputHandle::from_resource(resource)?
        .user_data()
        .get::<BackendOutput>()
        .map(|output| output.0)
}

/// The rate an output's `wl_output` mode advertises, in millihertz: its own
/// mode's, else the compositor-wide fallback, else 60 Hz.
fn advertised_refresh(out: &Output, fallback_refresh_mhz: Option<u32>) -> i32 {
    let mhz = out
        .refresh_mhz
        .filter(|mhz| *mhz > 0)
        .or_else(|| fallback_refresh_mhz.filter(|mhz| *mhz > 0))
        .unwrap_or(60_000);
    // A wl_output mode's refresh rate is i32; clamp rather than wrap on the
    // (unrealistic) chance a reported rate exceeds i32::MAX.
    i32::try_from(mhz).unwrap_or(i32::MAX)
}

fn mode_for(out: &Output, fallback_refresh_mhz: Option<u32>) -> OutputMode {
    let physical = out.physical_size();
    // A wl_output mode's dimensions are i32; clamp rather than wrap if a
    // backend ever reports something absurd, since wrapping could turn a
    // huge width/height into a negative one.
    let width = i32::try_from(physical.0).unwrap_or(i32::MAX);
    let height = i32::try_from(physical.1).unwrap_or(i32::MAX);
    OutputMode {
        size: Size::from((width, height)),
        refresh: advertised_refresh(out, fallback_refresh_mhz),
    }
}

pub(crate) use super::output_heads::advertised_name;

fn non_empty_or(value: &str, fallback: &str) -> String {
    if value.is_empty() {
        fallback.to_owned()
    } else {
        value.to_owned()
    }
}

fn wayland_transform(transform: settings_store::hardware::OutputTransform) -> Transform {
    use settings_store::hardware::OutputTransform as O;
    match transform {
        O::Normal => Transform::Normal,
        O::Rotate90 => Transform::_90,
        O::Rotate180 => Transform::_180,
        O::Rotate270 => Transform::_270,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn out(id: u32, position: (i32, i32), size: (u32, u32), scale: f32) -> Output {
        Output {
            id: OutputId(id),
            position,
            size,
            scale,
            transform: settings_store::hardware::OutputTransform::Normal,
            color_profile: None,
            refresh_mhz: None,
            logical_position: position,
            identity: crate::compositor::backend::OutputIdentity::default(),
        }
    }

    fn advertised(o: &Output) -> (OutputId, AdvertisedState) {
        (o.id, AdvertisedState::from(o))
    }

    #[test]
    fn output_globals_track_plugs_unplugs_and_in_place_updates() {
        // Phase `the_first_sync_advertises_every_output`.
        {
            let desired = [out(0, (0, 0), (1920, 1080), 1.0)];
            assert_eq!(plan(&[], &desired), vec![GlobalChange::Create(OutputId(0))]);
        }
        // Phase `an_unchanged_set_produces_no_wire_traffic`.
        {
            let o = out(0, (0, 0), (1920, 1080), 1.0);
            assert!(plan(&[advertised(&o)], std::slice::from_ref(&o)).is_empty());
        }
        // Phase `plugging_in_a_monitor_creates_exactly_one_global`.
        {
            let laptop = out(0, (0, 0), (1920, 1080), 1.5);
            let external = out(1, (1920, 0), (2560, 1440), 1.0);
            let changes = plan(&[advertised(&laptop)], &[laptop.clone(), external]);
            assert_eq!(changes, vec![GlobalChange::Create(OutputId(1))]);
        }
        // Phase `unplugging_a_monitor_destroys_exactly_its_global`.
        {
            let laptop = out(0, (0, 0), (1920, 1080), 1.5);
            let external = out(1, (1920, 0), (2560, 1440), 1.0);
            let changes = plan(
                &[advertised(&laptop), advertised(&external)],
                std::slice::from_ref(&laptop),
            );
            assert_eq!(changes, vec![GlobalChange::Destroy(OutputId(1))]);
        }
        // Phase `a_mode_change_updates_in_place_rather_than_replacing`.
        {
            // Replacing the global would tell every client the monitor
            // vanished and a different one appeared: `leave`, `enter`, and a
            // visible re-layout for a change that is only a resolution.
            let before = out(0, (0, 0), (1920, 1080), 1.0);
            let after = out(0, (0, 0), (3840, 2160), 1.0);
            let changes = plan(&[advertised(&before)], &[after]);
            assert_eq!(changes, vec![GlobalChange::Update(OutputId(0))]);
            assert!(
                !changes.contains(&GlobalChange::Destroy(OutputId(0))),
                "a re-mode must not withdraw the output's identity"
            );
        }
        // Phase `a_scale_change_updates_the_global`.
        {
            // Changing the display-scale setting has to reach clients, or a
            // HiDPI toolkit keeps rendering at the old factor.
            let before = out(0, (0, 0), (3840, 2160), 1.0);
            let after = out(0, (0, 0), (3840, 2160), 2.0);
            assert_eq!(
                plan(&[advertised(&before)], &[after]),
                vec![GlobalChange::Update(OutputId(0))]
            );
        }
        // Phase `a_survivor_that_slid_left_is_updated`.
        {
            // Unplugging the *first* of two monitors re-packs the second to
            // the origin. Its global carries the position, so it has to move
            // too — otherwise clients place windows on a coordinate space that
            // no longer matches the compositor's.
            let first = out(0, (0, 0), (1920, 1080), 1.0);
            let second = out(1, (1920, 0), (2560, 1440), 1.0);
            let second_moved = out(1, (0, 0), (2560, 1440), 1.0);
            let changes = plan(&[advertised(&first), advertised(&second)], &[second_moved]);
            assert_eq!(
                changes,
                vec![
                    GlobalChange::Update(OutputId(1)),
                    GlobalChange::Destroy(OutputId(0))
                ]
            );
        }
    }

    #[test]
    fn each_output_advertises_its_own_refresh_rate() {
        // One compositor-wide rate would tell a client on a 144 Hz monitor
        // to pace at the laptop panel's 60 Hz, or the reverse.
        let mut laptop = out(0, (0, 0), (2560, 1600), 2.0);
        laptop.refresh_mhz = Some(60_000);
        let mut external = out(1, (1280, 0), (2560, 1440), 1.0);
        external.refresh_mhz = Some(143_912);
        assert_eq!(mode_for(&laptop, Some(143_912)).refresh, 60_000);
        assert_eq!(mode_for(&external, Some(60_000)).refresh, 143_912);
    }

    #[test]
    fn an_output_without_a_rate_falls_back_to_pacing_then_sixty_hertz() {
        let o = out(0, (0, 0), (1920, 1080), 1.0);
        assert_eq!(mode_for(&o, Some(59_940)).refresh, 59_940);
        assert_eq!(mode_for(&o, None).refresh, 60_000);
        // Zero is no rate at all; some toolkits divide by it.
        let mut zero = o;
        zero.refresh_mhz = Some(0);
        assert_eq!(mode_for(&zero, Some(0)).refresh, 60_000);
    }

    #[test]
    fn a_refresh_change_updates_the_global() {
        let before = out(0, (0, 0), (2560, 1440), 1.0);
        let mut after = before.clone();
        after.refresh_mhz = Some(144_000);
        assert_eq!(
            plan(&[advertised(&before)], &[after]),
            vec![GlobalChange::Update(OutputId(0))]
        );
    }

    #[test]
    fn another_monitor_on_the_same_connector_replaces_the_global() {
        let mut before = out(1, (0, 0), (2560, 1440), 1.0);
        before.identity.connector = "DP-2".into();
        before.identity.model = "DELL U2720Q".into();
        let mut after = before.clone();
        after.identity.model = "LG HDR 4K".into();
        assert_eq!(
            plan(&[advertised(&before)], &[after]),
            vec![GlobalChange::Replace(OutputId(1))]
        );
    }

    #[test]
    fn outputs_are_named_by_connector_and_kept_unique() {
        let mut laptop = out(0, (0, 0), (1920, 1080), 1.0);
        laptop.identity.connector = "eDP-1".into();
        let mut left = out(1, (1920, 0), (1920, 1080), 1.0);
        left.identity.connector = "HDMI-A-1".into();
        let mut right = out(2, (3840, 0), (1920, 1080), 1.0);
        right.identity.connector = "HDMI-A-1".into();
        let unnamed = out(3, (5760, 0), (1920, 1080), 1.0);
        let all = [laptop.clone(), left.clone(), right.clone(), unnamed.clone()];
        assert_eq!(advertised_name(&laptop, &all), "eDP-1");
        assert_eq!(advertised_name(&left, &all), "HDMI-A-1-1");
        assert_eq!(advertised_name(&right, &all), "HDMI-A-1-2");
        assert_eq!(advertised_name(&unnamed, &all), "VIRTUAL-4");
    }

    #[test]
    fn output_global_ordering_and_empty_sets() {
        // Phase `destroys_come_after_creates_and_updates`.
        {
            // Applying the list in order must never leave two globals claiming
            // one identity, and must not tear an output down before its
            // replacement exists.
            let gone = out(0, (0, 0), (1920, 1080), 1.0);
            let stays = out(1, (1920, 0), (1280, 1024), 1.0);
            let fresh = out(2, (3200, 0), (1024, 768), 1.0);
            let changes = plan(
                &[advertised(&gone), advertised(&stays)],
                &[stays.clone(), fresh],
            );
            let first_destroy = changes
                .iter()
                .position(|c| matches!(c, GlobalChange::Destroy(_)))
                .expect("something was destroyed");
            let last_other = changes
                .iter()
                .rposition(|c| !matches!(c, GlobalChange::Destroy(_)))
                .expect("something was created");
            assert!(
                last_other < first_destroy,
                "destroys must come last: {changes:?}"
            );
        }
        // Phase `everything_unplugged_destroys_everything`.
        {
            let a = out(0, (0, 0), (1920, 1080), 1.0);
            let b = out(1, (1920, 0), (1280, 1024), 1.0);
            assert_eq!(
                plan(&[advertised(&a), advertised(&b)], &[]),
                vec![
                    GlobalChange::Destroy(OutputId(0)),
                    GlobalChange::Destroy(OutputId(1))
                ]
            );
        }
        // Phase `a_fresh_registry_advertises_nothing`.
        {
            let g = OutputGlobals::new();
            assert!(g.is_empty());
            assert!(g.advertised_ids().is_empty());
            assert!(g.handle(OutputId(0)).is_none());
        }
    }
}
