//! Shared-layout motion planning used by the native input path.

use settings_store::person_displays::{Layout, ResolvedDisplay};

/// What one relative motion did.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum SpatialMotion {
    /// The pointer stays on this device's screens; the compositor moves it.
    Local,
    /// The pointer is on another device's screen.
    Remote,
    /// The pointer came back, to this desktop point when it is known.
    Returned(Option<(f64, f64)>),
}

/// Where the pointer is before a motion.
#[derive(Clone, Copy, Debug)]
pub(crate) enum PointerAt<'a> {
    /// On this device, at a desktop point.
    Local((f64, f64)),
    /// On another device's display, at a shared-layout point.
    Remote {
        device: &'a str,
        output: &'a str,
        shared: (f64, f64),
    },
}

/// Where one motion takes the pointer.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum MotionPlan {
    Local,
    Remote {
        device: String,
        output: String,
        shared: (f64, f64),
        device_point: (f64, f64),
    },
    Return {
        local_point: Option<(f64, f64)>,
    },
}

/// Decide where a motion by `delta` from `at` goes in `layout`, resolved on
/// `local_device`. `reachable` admits another device's display.
pub(crate) fn plan_motion(
    layout: &Layout,
    local_device: &str,
    at: PointerAt<'_>,
    delta: (f64, f64),
    reachable: impl Fn(&ResolvedDisplay) -> bool,
) -> MotionPlan {
    let (current, from) = match at {
        PointerAt::Local(cursor) => {
            let Some((index, display)) = layout
                .locate(local_device, cursor)
                .and_then(|index| Some((index, layout.displays.get(index)?)))
            else {
                return MotionPlan::Local;
            };
            (index, display.to_shared(cursor))
        }
        PointerAt::Remote {
            device,
            output,
            shared,
        } => {
            let Some(index) = layout
                .displays
                .iter()
                .position(|display| display.is(device, output))
            else {
                return MotionPlan::Return { local_point: None };
            };
            (index, shared)
        }
    };
    let Some((step, display)) = layout
        .step_pointer(current, from, delta, reachable)
        .and_then(|step| {
            let display = layout.displays.get(step.display)?;
            Some((step, display))
        })
    else {
        return MotionPlan::Local;
    };
    match (display.local, at) {
        (true, PointerAt::Local(_)) => MotionPlan::Local,
        (true, PointerAt::Remote { .. }) => MotionPlan::Return {
            local_point: Some(step.device_point),
        },
        (false, _) => MotionPlan::Remote {
            device: display.device.clone(),
            output: display.output.clone(),
            shared: step.shared,
            device_point: step.device_point,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{MotionPlan, PointerAt, plan_motion};
    use settings_store::person_displays::{Arrangement, DeviceOutput, Layout, PersonDevice};

    fn output(id: &str, x: i32, width: u32, height: u32) -> DeviceOutput {
        DeviceOutput {
            id: id.into(),
            name: id.into(),
            builtin: false,
            x,
            y: 0,
            width,
            height,
            mode_width: width,
            mode_height: height,
            refresh_mhz: 60_000,
            scale_milli: 1000,
            connected: true,
            enabled: true,
        }
    }

    fn laptop_and_pi() -> Layout {
        Layout::resolve(
            &[
                PersonDevice {
                    device: "laptop".into(),
                    label: "Laptop".into(),
                    local: true,
                    online: true,
                    outputs: vec![output("panel", 0, 1440, 900)],
                },
                PersonDevice {
                    device: "pi".into(),
                    label: "Orange Pi".into(),
                    local: false,
                    online: true,
                    outputs: vec![output("hdmi", 0, 1920, 1080)],
                },
            ],
            &Arrangement::default(),
        )
    }

    #[test]
    fn the_pointer_leaves_onto_the_pi_and_comes_back_at_the_mapped_point() {
        let layout = laptop_and_pi();
        // Inside the laptop's screen: the compositor moves it.
        assert_eq!(
            plan_motion(
                &layout,
                "laptop",
                PointerAt::Local((700.0, 450.0)),
                (5.0, 0.0),
                |_| true
            ),
            MotionPlan::Local
        );
        // Past the right edge: onto the Pi, carrying the overshoot.
        let entered = plan_motion(
            &layout,
            "laptop",
            PointerAt::Local((1439.0, 450.5)),
            (6.0, -2.0),
            |_| true,
        );
        assert_eq!(
            entered,
            MotionPlan::Remote {
                device: "pi".into(),
                output: "hdmi".into(),
                shared: (1445.0, 448.5),
                device_point: (5.0, 448.5),
            }
        );
        let moved = plan_motion(
            &layout,
            "laptop",
            PointerAt::Remote {
                device: "pi",
                output: "hdmi",
                shared: (1445.0, 448.5),
            },
            (100.0, 600.0),
            |_| true,
        );
        assert_eq!(
            moved,
            MotionPlan::Remote {
                device: "pi".into(),
                output: "hdmi".into(),
                shared: (1545.0, 1048.5),
                device_point: (105.0, 1048.5),
            }
        );
        // Back over the edge: the laptop's pointer resumes at the mapped point.
        let back = plan_motion(
            &layout,
            "laptop",
            PointerAt::Remote {
                device: "pi",
                output: "hdmi",
                shared: (1442.0, 300.0),
            },
            (-10.0, 0.0),
            |_| true,
        );
        assert_eq!(
            back,
            MotionPlan::Return {
                local_point: Some((1432.0, 300.0))
            }
        );
        // Below the shorter laptop screen there is nothing to return to:
        // the pointer stays on the Pi at its left edge.
        let below = plan_motion(
            &layout,
            "laptop",
            PointerAt::Remote {
                device: "pi",
                output: "hdmi",
                shared: (1442.0, 1000.0),
            },
            (-10.0, 0.0),
            |_| true,
        );
        assert!(
            matches!(below, MotionPlan::Remote { device_point, .. } if device_point == (0.0, 1000.0))
        );
    }

    #[test]
    fn an_unreachable_device_is_a_wall_and_a_vanished_display_returns_the_pointer() {
        let layout = laptop_and_pi();
        assert_eq!(
            plan_motion(
                &layout,
                "laptop",
                PointerAt::Local((1439.0, 450.0)),
                (6.0, 0.0),
                |display| display.local
            ),
            MotionPlan::Local
        );
        assert_eq!(
            plan_motion(
                &layout,
                "laptop",
                PointerAt::Remote {
                    device: "tv",
                    output: "panel",
                    shared: (0.0, 0.0),
                },
                (1.0, 0.0),
                |_| true
            ),
            MotionPlan::Return { local_point: None }
        );
    }
}
