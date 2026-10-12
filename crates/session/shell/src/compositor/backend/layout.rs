//! One output arrangement, in the two coordinate spaces it lives in.
//!
//! The **logical** layout is the desktop as people and clients see it: an
//! output's extent is its mode divided by its scale (rotated), and outputs
//! sit edge to edge in those units. Settings stores it, `wl_output` and
//! `zxdg_output_v1` advertise it, and the pointer moves in it — so a 2x
//! laptop panel beside a 1x monitor is 1280 logical pixels wide, and the
//! pointer enters the monitor at the height it left the panel.
//!
//! The **scene** is the desktop in physical pixels: the one scene the
//! compositor builds each frame. Each output renders its own view of it,
//! the sub-rect at its scene position, into a target of its own size; only
//! the readback fallback rasterises the bounding box of every output. A
//! high-density output covers more pixels than logical units, so scene
//! positions are derived from the logical layout by [`scene_positions`]
//! rather than equal to it — keeping outputs from overlapping in the scene,
//! and every output's view at whole-pixel offsets, so a window spanning two
//! outputs meets itself without a seam. For an arrangement where every scale
//! is 1 the two spaces coincide.

/// An output's extent in logical pixels: its mode over its scale, rotated.
pub use settings_store::hardware::logical_extent;

/// One output as [`scene_positions`] takes it: `(logical position, logical
/// size, scene size)`.
pub type ArrangedOutput = ((i32, i32), (u32, u32), (u32, u32));

/// One output in both spaces.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Placement {
    pub logical_position: (i32, i32),
    pub logical_size: (u32, u32),
    pub scene_position: (i32, i32),
    pub scene_size: (u32, u32),
}

impl Placement {
    fn logical_contains(&self, p: (f64, f64)) -> bool {
        let (x, y) = (
            f64::from(self.logical_position.0),
            f64::from(self.logical_position.1),
        );
        p.0 >= x
            && p.1 >= y
            && p.0 < x + f64::from(self.logical_size.0)
            && p.1 < y + f64::from(self.logical_size.1)
    }

    fn scene_contains(&self, p: (f64, f64)) -> bool {
        let (x, y) = (
            f64::from(self.scene_position.0),
            f64::from(self.scene_position.1),
        );
        p.0 >= x
            && p.1 >= y
            && p.0 < x + f64::from(self.scene_size.0)
            && p.1 < y + f64::from(self.scene_size.1)
    }

    /// Physical pixels per logical pixel along each axis. Taken from the two
    /// extents rather than the scale so the far edges map onto each other
    /// exactly, whatever rounding produced the logical size.
    fn ratio(&self) -> (f64, f64) {
        let axis = |scene: u32, logical: u32| {
            if logical == 0 {
                1.0
            } else {
                f64::from(scene) / f64::from(logical)
            }
        };
        (
            axis(self.scene_size.0, self.logical_size.0),
            axis(self.scene_size.1, self.logical_size.1),
        )
    }
}

/// Both placements of each output in a backend's table.
pub fn placements(outputs: &[super::Output]) -> Vec<Placement> {
    outputs
        .iter()
        .map(|o| Placement {
            logical_position: o.logical_position,
            logical_size: o.logical_size(),
            scene_position: o.position,
            scene_size: o.size,
        })
        .collect()
}

/// Move a logical pointer by a relative `delta` (logical pixels) and keep it
/// on an output. Motion is continuous in the logical layout, so a pointer
/// leaving a 2x panel's right edge enters the monitor beside it at the same
/// logical height, whatever either output's scale or vertical offset.
pub fn move_pointer(
    placements: &[Placement],
    pointer: (f64, f64),
    delta: (f64, f64),
) -> (f64, f64) {
    clamp_to_outputs(placements, (pointer.0 + delta.0, pointer.1 + delta.1))
}

/// Keep a logical point on the union of the logical output rectangles.
pub fn clamp_to_outputs(placements: &[Placement], p: (f64, f64)) -> (f64, f64) {
    super::drm::hotplug::clamp_pointer_to_outputs(
        p,
        placements
            .iter()
            .map(|o| (o.logical_position, o.logical_size)),
    )
}

/// Place outputs in the scene from their logical layout.
///
/// `outputs` is `(logical position, logical size, scene size)`. Each axis is
/// resolved on its own: an output keeps its logical coordinate unless an
/// output wholly before it on that axis (to its left, or above it) now
/// reaches further in the scene, in which case it is pushed just past that
/// output, keeping the logical gap between them. Two outputs that do not
/// overlap logically are separated along the axis on which they are
/// disjoint, so they never overlap in the scene; outputs at one logical
/// position (mirrors) share a scene position.
pub fn scene_positions(outputs: &[ArrangedOutput]) -> Vec<(i32, i32)> {
    let xs = resolve_axis(
        &outputs
            .iter()
            .map(|&(pos, logical, scene)| (pos.0, logical.0, scene.0))
            .collect::<Vec<_>>(),
    );
    let ys = resolve_axis(
        &outputs
            .iter()
            .map(|&(pos, logical, scene)| (pos.1, logical.1, scene.1))
            .collect::<Vec<_>>(),
    );
    xs.into_iter().zip(ys).collect()
}

/// One axis of [`scene_positions`]: `(logical start, logical length, scene
/// length)` per output.
fn resolve_axis(spans: &[(i32, u32, u32)]) -> Vec<i32> {
    let mut order: Vec<usize> = (0..spans.len()).collect();
    order.sort_by_key(|&i| spans.get(i).map_or(0, |s| s.0));
    let mut scene: Vec<i64> = spans.iter().map(|s| i64::from(s.0)).collect();
    for (rank, &i) in order.iter().enumerate() {
        let Some(&(start, _, _)) = spans.get(i) else {
            continue;
        };
        let start = i64::from(start);
        let mut at = start;
        for &j in order.iter().take(rank) {
            let (Some(&(before_start, before_len, before_scene_len)), Some(&before_at)) =
                (spans.get(j), scene.get(j))
            else {
                continue;
            };
            let before_end = i64::from(before_start).saturating_add(i64::from(before_len));
            if before_end <= start {
                let gap = start.saturating_sub(before_end);
                at = at.max(
                    before_at
                        .saturating_add(i64::from(before_scene_len))
                        .saturating_add(gap),
                );
            }
        }
        if let Some(slot) = scene.get_mut(i) {
            *slot = at;
        }
    }
    scene
        .into_iter()
        .map(|v| i32::try_from(v).unwrap_or(i32::MAX))
        .collect()
}

/// Map a logical point to the scene through the output it is on. A point on
/// no output maps through the nearest one, so the result is always somewhere
/// an output presents.
pub fn logical_to_scene(placements: &[Placement], p: (f64, f64)) -> (f64, f64) {
    let Some(out) = placements
        .iter()
        .find(|o| o.logical_contains(p))
        .or_else(|| nearest(placements, p, |o| (o.logical_position, o.logical_size)))
    else {
        return p;
    };
    let (rx, ry) = out.ratio();
    (
        f64::from(out.scene_position.0) + (p.0 - f64::from(out.logical_position.0)) * rx,
        f64::from(out.scene_position.1) + (p.1 - f64::from(out.logical_position.1)) * ry,
    )
}

/// Map a scene point back to the logical layout — the inverse of
/// [`logical_to_scene`], for input that arrives in scene coordinates (a
/// touchscreen calibrated to its panel's scene rect).
pub fn scene_to_logical(placements: &[Placement], p: (f64, f64)) -> (f64, f64) {
    let Some(out) = placements
        .iter()
        .find(|o| o.scene_contains(p))
        .or_else(|| nearest(placements, p, |o| (o.scene_position, o.scene_size)))
    else {
        return p;
    };
    let (rx, ry) = out.ratio();
    (
        f64::from(out.logical_position.0) + (p.0 - f64::from(out.scene_position.0)) / rx,
        f64::from(out.logical_position.1) + (p.1 - f64::from(out.scene_position.1)) / ry,
    )
}

/// Whether the scene is the logical layout unchanged: every output presents
/// at scale 1 where it sits logically.
pub fn is_identity(placements: &[Placement]) -> bool {
    placements
        .iter()
        .all(|o| o.scene_position == o.logical_position && o.scene_size == o.logical_size)
}

/// Carry a backend input event from the scene, where backends report
/// positions, into the logical desktop, where the shell hit-tests and
/// clients receive coordinates. Events without a position pass through.
#[must_use]
pub fn event_to_logical(
    placements: &[Placement],
    event: super::BackendEvent,
) -> super::BackendEvent {
    use super::{BackendEvent, TouchEvent};
    use winit::event::WindowEvent;
    if is_identity(placements) {
        return event;
    }
    let map = |x: f64, y: f64| scene_to_logical(placements, (x, y));
    match event {
        BackendEvent::Input(WindowEvent::CursorMoved {
            device_id,
            position,
        }) => {
            let (x, y) = map(position.x, position.y);
            BackendEvent::Input(WindowEvent::CursorMoved {
                device_id,
                position: winit::dpi::PhysicalPosition::new(x, y),
            })
        }
        BackendEvent::Input(WindowEvent::Touch(mut touch)) => {
            let (x, y) = map(touch.location.x, touch.location.y);
            touch.location = winit::dpi::PhysicalPosition::new(x, y);
            BackendEvent::Input(WindowEvent::Touch(touch))
        }
        BackendEvent::Touch(TouchEvent::Down { slot, x, y }) => {
            let (x, y) = map(x, y);
            BackendEvent::Touch(TouchEvent::Down { slot, x, y })
        }
        BackendEvent::Touch(TouchEvent::Motion { slot, x, y }) => {
            let (x, y) = map(x, y);
            BackendEvent::Touch(TouchEvent::Motion { slot, x, y })
        }
        other => other,
    }
}

fn nearest(
    placements: &[Placement],
    point: (f64, f64),
    rect: impl Fn(&Placement) -> ((i32, i32), (u32, u32)),
) -> Option<&Placement> {
    // Distance from `point` to the span `[start, start + len)` on one axis.
    let outside = |coord: f64, start: i32, len: u32| {
        let start = f64::from(start);
        (start - coord)
            .max(coord - (start + f64::from(len)))
            .max(0.0)
    };
    placements
        .iter()
        .map(|placement| {
            let (origin, extent) = rect(placement);
            let dx = outside(point.0, origin.0, extent.0);
            let dy = outside(point.1, origin.1, extent.1);
            (placement, dx.mul_add(dx, dy * dy))
        })
        .min_by(|left, right| left.1.total_cmp(&right.1))
        .map(|(placement, _)| placement)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn placements(outputs: &[ArrangedOutput]) -> Vec<Placement> {
        outputs
            .iter()
            .zip(scene_positions(outputs))
            .map(
                |(&(logical_position, logical_size, scene_size), scene_position)| Placement {
                    logical_position,
                    logical_size,
                    scene_position,
                    scene_size,
                },
            )
            .collect()
    }

    #[test]
    fn an_all_1x_arrangement_is_its_own_scene() {
        let outs = [
            ((0, 0), (1920, 1080), (1920, 1080)),
            ((1920, 200), (2560, 1440), (2560, 1440)),
            ((-1080, 0), (1080, 1920), (1080, 1920)),
        ];
        assert_eq!(
            scene_positions(&outs),
            vec![(0, 0), (1920, 200), (-1080, 0)]
        );
    }

    #[test]
    fn a_hidpi_output_pushes_its_right_neighbour_along_the_scene() {
        // A 2x panel is 1280 logical wide but 2560 pixels in the texture.
        let outs = [
            ((0, 0), (1280, 800), (2560, 1600)),
            ((1280, 0), (2560, 1440), (2560, 1440)),
        ];
        assert_eq!(scene_positions(&outs), vec![(0, 0), (2560, 0)]);
    }

    #[test]
    fn a_hidpi_output_pushes_the_output_below_it_down_the_scene() {
        let outs = [
            ((0, 0), (1920, 1080), (3840, 2160)),
            ((320, 1080), (1280, 1024), (1280, 1024)),
        ];
        assert_eq!(scene_positions(&outs), vec![(0, 0), (320, 2160)]);
    }

    #[test]
    fn logically_disjoint_outputs_never_overlap_in_the_scene() {
        let outs = [
            ((0, 0), (1280, 800), (2560, 1600)),
            ((1280, 200), (1920, 1080), (1920, 1080)),
            ((0, 800), (1536, 960), (1920, 1200)),
            ((3200, 0), (1080, 1920), (1080, 1920)),
        ];
        let p = placements(&outs);
        for (i, a) in p.iter().enumerate() {
            for b in p.iter().skip(i.saturating_add(1)) {
                let span = |start: i32, len: u32| {
                    (
                        i64::from(start),
                        i64::from(start).saturating_add(i64::from(len)),
                    )
                };
                let (ax, ax1) = span(a.scene_position.0, a.scene_size.0);
                let (bx, bx1) = span(b.scene_position.0, b.scene_size.0);
                let (ay, ay1) = span(a.scene_position.1, a.scene_size.1);
                let (by, by1) = span(b.scene_position.1, b.scene_size.1);
                let overlap_x = ax < bx1 && bx < ax1;
                let overlap_y = ay < by1 && by < ay1;
                assert!(!(overlap_x && overlap_y), "{a:?} overlaps {b:?}");
            }
        }
    }

    #[test]
    fn mirrors_share_a_scene_position() {
        let outs = [
            ((0, 0), (1280, 800), (2560, 1600)),
            ((1280, 0), (1920, 1080), (1920, 1080)),
            ((1280, 0), (1920, 1080), (1920, 1080)),
        ];
        let scene = scene_positions(&outs);
        assert_eq!(scene[1], scene[2]);
    }

    #[test]
    fn logical_and_scene_points_round_trip_through_each_output() {
        let p = placements(&[
            ((0, 0), (1280, 800), (2560, 1600)),
            ((1280, 200), (1920, 1080), (1920, 1080)),
        ]);
        // Logical row 400 is scene row 800 on the 2x panel ...
        assert_eq!(logical_to_scene(&p, (640.0, 400.0)), (1280.0, 800.0));
        // ... and scene row 400 on the 1x monitor, whose top is at logical
        // 200 and scene 200.
        assert_eq!(logical_to_scene(&p, (1300.0, 400.0)), (2580.0, 400.0));
        for point in [(640.0, 400.0), (1300.0, 400.0), (3199.0, 1279.0)] {
            assert_eq!(scene_to_logical(&p, logical_to_scene(&p, point)), point);
        }
    }

    #[test]
    fn a_point_between_outputs_maps_through_the_nearest() {
        let p = placements(&[
            ((0, 0), (1280, 800), (2560, 1600)),
            ((1280, 200), (1920, 1080), (1920, 1080)),
        ]);
        // Below the panel's bottom edge, left of the monitor.
        let scene = logical_to_scene(&p, (100.0, 900.0));
        assert_eq!(scene, (200.0, 1800.0));
    }
}
