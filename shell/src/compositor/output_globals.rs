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

use std::collections::HashMap;

use smithay::output::{
    Mode as OutputMode, Output as WlOutputHandle, PhysicalProperties, Scale, Subpixel,
};
use smithay::reexports::wayland_server::DisplayHandle;
use smithay::reexports::wayland_server::backend::GlobalId;
use smithay::utils::{Size, Transform};

use super::backend::{Output, OutputId};

/// One change to the advertised set.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum GlobalChange {
    /// No global exists for this output yet — advertise one.
    Create(OutputId),
    /// The output exists but its size, position or scale moved. Update the
    /// existing global rather than replacing it, so clients see a mode
    /// change instead of a monitor disappearing.
    Update(OutputId),
    /// The output is gone — withdraw its global.
    Destroy(OutputId),
}

/// What one advertised output currently looks like. Compared against the
/// backend's list to decide whether an update is needed at all.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct AdvertisedState {
    pub size: (u32, u32),
    pub position: (i32, i32),
    pub scale: f32,
}

impl From<&Output> for AdvertisedState {
    fn from(o: &Output) -> Self {
        Self {
            size: o.size,
            position: o.position,
            scale: o.scale,
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

    /// Snapshot of what is advertised, in the shape [`plan`] compares.
    fn snapshot(&self) -> Vec<(OutputId, AdvertisedState)> {
        let mut v: Vec<(OutputId, AdvertisedState)> =
            self.live.iter().map(|(id, l)| (*id, l.state)).collect();
        v.sort_by_key(|(id, _)| id.0);
        v
    }

    /// Bring the advertised set in line with `desired`.
    ///
    /// Idempotent: called on every output change and safe to call when
    /// nothing moved, because [`plan`] returns an empty list then and no
    /// wire traffic is generated.
    ///
    /// `refresh_mhz` is the presentation rate to advertise, in
    /// millihertz. Clients use it for frame pacing; `None` advertises the
    /// conventional 60 Hz rather than zero, which some toolkits divide by.
    pub fn sync<D>(&mut self, dh: &DisplayHandle, desired: &[Output], refresh_mhz: Option<u32>)
    where
        D: smithay::reexports::wayland_server::GlobalDispatch<
                smithay::reexports::wayland_server::protocol::wl_output::WlOutput,
                smithay::wayland::output::WlOutputData,
            > + 'static,
    {
        let refresh = refresh_mhz.unwrap_or(60_000) as i32;
        for change in plan(&self.snapshot(), desired) {
            match change {
                GlobalChange::Create(id) => {
                    let Some(out) = desired.iter().find(|o| o.id == id) else {
                        continue;
                    };
                    let handle = WlOutputHandle::new(
                        format!("qOS-{}", id.0),
                        PhysicalProperties {
                            // The backend reports pixels and derives the
                            // scale itself; the physical millimetres are
                            // not carried through the `Output` struct, so
                            // advertise zero rather than a fabricated
                            // size a client might compute a DPI from.
                            size: Size::from((0, 0)),
                            subpixel: Subpixel::Unknown,
                            make: "qOS".into(),
                            model: format!("Output {}", id.0),
                        },
                    );
                    let mode = mode_for(out, refresh);
                    handle.change_current_state(
                        Some(mode),
                        Some(Transform::Normal),
                        Some(Scale::Fractional(f64::from(out.scale))),
                        Some(out.position.into()),
                    );
                    handle.set_preferred(mode);
                    let global = handle.create_global::<D>(dh);
                    tracing::info!(
                        output = id.0,
                        size = ?out.size,
                        position = ?out.position,
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
                    let mode = mode_for(out, refresh);
                    live.handle.change_current_state(
                        Some(mode),
                        Some(Transform::Normal),
                        Some(Scale::Fractional(f64::from(out.scale))),
                        Some(out.position.into()),
                    );
                    live.handle.set_preferred(mode);
                    live.state = AdvertisedState::from(out);
                    tracing::info!(
                        output = id.0,
                        size = ?out.size,
                        position = ?out.position,
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

fn mode_for(out: &Output, refresh: i32) -> OutputMode {
    OutputMode {
        size: Size::from((out.size.0 as i32, out.size.1 as i32)),
        refresh,
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
        }
    }

    fn advertised(o: &Output) -> (OutputId, AdvertisedState) {
        (o.id, AdvertisedState::from(o))
    }

    #[test]
    fn the_first_sync_advertises_every_output() {
        let desired = [out(0, (0, 0), (1920, 1080), 1.0)];
        assert_eq!(plan(&[], &desired), vec![GlobalChange::Create(OutputId(0))]);
    }

    #[test]
    fn an_unchanged_set_produces_no_wire_traffic() {
        let o = out(0, (0, 0), (1920, 1080), 1.0);
        assert!(plan(&[advertised(&o)], std::slice::from_ref(&o)).is_empty());
    }

    #[test]
    fn plugging_in_a_monitor_creates_exactly_one_global() {
        let laptop = out(0, (0, 0), (1920, 1080), 1.5);
        let external = out(1, (1920, 0), (2560, 1440), 1.0);
        let changes = plan(&[advertised(&laptop)], &[laptop.clone(), external]);
        assert_eq!(changes, vec![GlobalChange::Create(OutputId(1))]);
    }

    #[test]
    fn unplugging_a_monitor_destroys_exactly_its_global() {
        let laptop = out(0, (0, 0), (1920, 1080), 1.5);
        let external = out(1, (1920, 0), (2560, 1440), 1.0);
        let changes = plan(
            &[advertised(&laptop), advertised(&external)],
            std::slice::from_ref(&laptop),
        );
        assert_eq!(changes, vec![GlobalChange::Destroy(OutputId(1))]);
    }

    #[test]
    fn a_mode_change_updates_in_place_rather_than_replacing() {
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

    #[test]
    fn a_scale_change_updates_the_global() {
        // Changing the display-scale setting has to reach clients, or a
        // HiDPI toolkit keeps rendering at the old factor.
        let before = out(0, (0, 0), (3840, 2160), 1.0);
        let after = out(0, (0, 0), (3840, 2160), 2.0);
        assert_eq!(
            plan(&[advertised(&before)], &[after]),
            vec![GlobalChange::Update(OutputId(0))]
        );
    }

    #[test]
    fn a_survivor_that_slid_left_is_updated() {
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

    #[test]
    fn destroys_come_after_creates_and_updates() {
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

    #[test]
    fn everything_unplugged_destroys_everything() {
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

    #[test]
    fn a_fresh_registry_advertises_nothing() {
        let g = OutputGlobals::new();
        assert!(g.is_empty());
        assert!(g.advertised_ids().is_empty());
        assert!(g.handle(OutputId(0)).is_none());
    }
}
