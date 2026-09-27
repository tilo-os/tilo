//! Presenter view on your screen, slides on the big one.
//!
//! When an app pairs an `.audience` surface with its ordinary window, the
//! audience surface takes one output and the app's window becomes the
//! presenter view on another: the built-in panel if there is one, else the
//! smaller screen. That is where a laptop plugged into a television is
//! expected to put things, whichever screen the window happened to be on.
//!
//! The move is a compositor placement, not a change to the workspace: the
//! presenter surface is painted over its output through the fullscreen
//! overlay and configured to that output's size, while its tab stays where
//! it was in the strip. Ending the presentation drops the placement, and the
//! window is back in its tab at its tab size with nothing to restore. A
//! fullscreen surface the placement displaced comes back with it.
//!
//! The ranking is `ui::fullscreen`'s, the same the app used to choose the
//! audience screen, so client and compositor cannot disagree about which
//! screen is the big one.

use super::{FullscreenTarget, State, output_presentation_owner};
use crate::compositor::backend::{Output, OutputId};
use crate::network::protocol::SurfaceId;

/// The presenter view held on its own output while `audience` is live.
pub(crate) struct PresenterPin {
    pub audience: SurfaceId,
    pub presenter: SurfaceId,
    pub output: OutputId,
    /// A fullscreen surface of another window the placement took the
    /// overlay from, put back when the presentation ends.
    displaced: Option<FullscreenTarget>,
}

/// The outputs worth ranking — a zero-sized one is mid-modeset — as
/// `ui::fullscreen` monitors whose `index` is their position in the slice.
fn ranked_outputs(outputs: &[Output]) -> (Vec<&Output>, Vec<ui::fullscreen::MonitorInfo>) {
    let viable: Vec<&Output> = outputs
        .iter()
        .filter(|output| output.size.0 > 0 && output.size.1 > 0)
        .collect();
    let infos = viable
        .iter()
        .enumerate()
        .map(|(index, output)| ui::fullscreen::MonitorInfo {
            index: u32::try_from(index).unwrap_or(u32::MAX),
            width: output.size.0,
            height: output.size.1,
            primary: false,
            scale: f64::from(output.scale),
            builtin: output.identity.is_builtin(),
            physical_mm: output.identity.known_physical_mm(),
        })
        .collect();
    (viable, infos)
}

/// The output an audience surface goes to when its client named none.
/// `None` on one screen, where the caller's single-output fallback applies.
pub(crate) fn audience_output(
    outputs: &[Output],
    presenter_on: Option<OutputId>,
) -> Option<OutputId> {
    let (viable, infos) = ranked_outputs(outputs);
    let presenter_index = presenter_on
        .and_then(|id| viable.iter().position(|output| output.id == id))
        .and_then(|index| u32::try_from(index).ok());
    let index = ui::fullscreen::select_audience_output(
        &infos,
        ui::fullscreen::FullscreenTarget::Largest,
        presenter_index,
    )?;
    viable.get(index as usize).map(|output| output.id)
}

/// The output the presenter view moves to once the audience holds
/// `audience`. `None` when no other output exists.
pub(crate) fn presenter_output(outputs: &[Output], audience: OutputId) -> Option<OutputId> {
    let (viable, infos) = ranked_outputs(outputs);
    let audience_index = viable.iter().position(|output| output.id == audience)?;
    let index =
        ui::fullscreen::select_presenter_output(&infos, u32::try_from(audience_index).ok()?)?;
    viable.get(index as usize).map(|output| output.id)
}

impl State {
    /// Where an audience surface without a requested output goes, judged
    /// against the output its presenter's tab is on now.
    pub(super) fn audience_fallback_output(&self, presenter: SurfaceId) -> Option<OutputId> {
        let presenter_on = crate::compositor::foreign::store()
            .placement(presenter)
            .map(|rect| rect.center())
            .and_then(|centre| {
                self.output_rects
                    .iter()
                    .find(|(_, rect)| rect.contains(centre))
                    .map(|(id, _)| *id)
            });
        audience_output(&self.output_table, presenter_on)
    }

    /// The pin the current audience surfaces call for: the first placed
    /// audience surface with a live presenter, and a second screen for it.
    fn desired_presenter_pin(&self) -> Option<(SurfaceId, SurfaceId, OutputId)> {
        let tree = self.workspace_tree.read();
        self.output_presentation_surfaces
            .iter()
            .find_map(|audience| {
                if audience.rect.area() <= 0.0 || !self.output_rects.contains_key(&audience.output)
                {
                    return None;
                }
                let presenter =
                    output_presentation_owner(&tree, audience.surface_id, &audience.app_id)?;
                let output = presenter_output(&self.output_table, audience.output)?;
                Some((audience.surface_id, presenter, output))
            })
    }

    /// Whether the fullscreen overlay still shows this pin's placement.
    fn pin_holds_overlay(&self, pin: &PresenterPin) -> bool {
        self.fullscreen_target.as_ref().is_some_and(|target| {
            target.surface_id == pin.presenter && target.pinned_output == Some(pin.output)
        })
    }

    /// Bring the presenter placement in line with the audience surfaces and
    /// outputs. Idempotent; runs on promotion, when the audience names its
    /// screen, on hotplug and when an audience surface goes away.
    pub(super) fn reconcile_presenter_pin(&mut self) {
        let desired = self.desired_presenter_pin();
        if let (Some(pin), Some((audience, presenter, output))) = (&self.presenter_pin, desired)
            && pin.audience == audience
            && pin.presenter == presenter
            && pin.output == output
            && self.pin_holds_overlay(pin)
        {
            // Same placement; the output itself may have moved or resized.
            if let Some(rect) = self.output_rects.get(&output).copied() {
                crate::compositor::foreign::store().pin_placement(presenter, Some(rect));
            }
            return;
        }
        self.release_presenter_pin();
        let Some((audience, presenter, output)) = desired else {
            return;
        };
        let Some(rect) = self.output_rects.get(&output).copied() else {
            return;
        };
        let Some(surface) = self
            .xdg_shell_state
            .toplevel_surfaces()
            .iter()
            .find(|toplevel| super::surface_id(toplevel.wl_surface()) == presenter)
            .cloned()
        else {
            return;
        };
        crate::compositor::foreign::store().pin_placement(presenter, Some(rect));
        let displaced = self
            .fullscreen_target
            .take()
            .filter(|target| target.surface_id != presenter);
        self.fullscreen_target = Some(FullscreenTarget {
            surface_id: presenter,
            requested_output: None,
            surface,
            configured_size: None,
            pinned_output: Some(output),
        });
        tracing::info!(
            audience,
            presenter,
            output = output.0,
            "presenter view moved to its own screen"
        );
        self.presenter_pin = Some(PresenterPin {
            audience,
            presenter,
            output,
            displaced,
        });
    }

    /// Drop the presenter placement: the window returns to its tab size and
    /// a displaced fullscreen surface gets the overlay back.
    pub(super) fn release_presenter_pin(&mut self) {
        let Some(pin) = self.presenter_pin.take() else {
            return;
        };
        crate::compositor::foreign::store().pin_placement(pin.presenter, None);
        if self.pin_holds_overlay(&pin) {
            self.fullscreen_target = pin.displaced.map(|mut target| {
                // Its output may have changed while it was hidden.
                target.configured_size = None;
                target
            });
        }
        tracing::info!(presenter = pin.presenter, "presenter view back in its tab");
    }

    /// The configure size, in logical pixels, of a presenter view held
    /// on its own screen, or `None` if `surface` is not one.
    pub(super) fn pinned_presenter_size(&self, surface: SurfaceId) -> Option<(i32, i32)> {
        let pin = self.presenter_pin.as_ref()?;
        if pin.presenter != surface || !self.pin_holds_overlay(pin) {
            return None;
        }
        let rect = self.output_rects.get(&pin.output)?;
        #[allow(
            clippy::cast_possible_truncation,
            reason = "clamped into i32 range and rounded first"
        )]
        let logical = |extent: f64| extent.round().clamp(1.0, f64::from(i32::MAX)) as i32;
        Some((logical(rect.width()), logical(rect.height())))
    }

    /// The presenter surface held on its own screen and that screen, while
    /// a paired audience surface is live.
    pub fn presenter_screen(&self) -> Option<(SurfaceId, OutputId)> {
        self.presenter_pin
            .as_ref()
            .filter(|pin| self.pin_holds_overlay(pin))
            .map(|pin| (pin.presenter, pin.output))
    }

    /// The output an audience surface is painted into.
    pub fn output_presentation_output(&self, surface: SurfaceId) -> Option<OutputId> {
        self.output_presentation_surfaces
            .iter()
            .find(|candidate| candidate.surface_id == surface)
            .map(|candidate| candidate.output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compositor::backend::OutputIdentity;

    fn output(
        id: u32,
        x: i32,
        size: (u32, u32),
        connector: &str,
        mm: Option<(u32, u32)>,
    ) -> Output {
        Output {
            position: (x, 0),
            identity: OutputIdentity {
                connector: connector.into(),
                physical_mm: mm.unwrap_or((0, 0)),
                ..OutputIdentity::default()
            },
            ..Output::new(OutputId(id), size, 1.0)
        }
    }

    #[test]
    fn laptop_and_television_split_audience_and_presenter() {
        // The laptop enumerates first and has more pixels: neither the
        // lowest id nor the pixel count may decide.
        let outputs = [
            output(0, 0, (2880, 1800), "eDP-1", Some((302, 189))),
            output(1, 2880, (1920, 1080), "HDMI-A-1", Some((1430, 800))),
        ];
        assert_eq!(
            audience_output(&outputs, Some(OutputId(1))),
            Some(OutputId(1))
        );
        assert_eq!(audience_output(&outputs, None), Some(OutputId(1)));
        assert_eq!(presenter_output(&outputs, OutputId(1)), Some(OutputId(0)));
    }

    #[test]
    fn one_screen_has_no_second_placement() {
        let outputs = [output(0, 0, (1920, 1080), "eDP-1", None)];
        assert_eq!(audience_output(&outputs, None), None);
        assert_eq!(presenter_output(&outputs, OutputId(0)), None);
    }

    #[test]
    fn a_screen_mid_modeset_is_not_a_candidate() {
        let outputs = [
            output(0, 0, (1920, 1080), "eDP-1", None),
            output(1, 1920, (0, 0), "HDMI-A-1", None),
        ];
        assert_eq!(audience_output(&outputs, None), None);
    }
}
