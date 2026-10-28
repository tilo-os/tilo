//! Track the `wl_output` membership of the surfaces the native compositor
//! actually paints.
//!
//! `wl_surface.enter` is not a one-time "the compositor has an output"
//! announcement. A surface is a member of every output its painted rectangle
//! intersects, and it must leave an output when that stops being true. Keeping
//! this as a small pure state machine makes the wire ordering in `native` both
//! testable and independent of a particular DRM connector arrangement.

use std::collections::{BTreeMap, BTreeSet};

use vello::kurbo::Rect;

use super::backend::{Output, OutputId};

/// One protocol action required to make a surface's output membership current.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Change {
    Enter { surface_id: u64, output: OutputId },
    Leave { surface_id: u64, output: OutputId },
}

/// Last output membership sent for each visible root surface.
///
/// `BTree*` containers are intentional: ordering wire events by surface and
/// output makes a hotplug batch deterministic and makes accidental duplicate
/// transitions observable in tests.
#[derive(Default)]
pub struct Memberships {
    sent: BTreeMap<u64, BTreeSet<OutputId>>,
}

impl Memberships {
    /// Withdraw memberships whose `wl_output` is about to disappear. This is
    /// called before the output global is removed; after removal there is no
    /// protocol object left on which a compliant `wl_surface.leave` can be
    /// sent.
    pub fn leave_removed_outputs(&mut self, outputs: &[Output]) -> Vec<Change> {
        let live: BTreeSet<OutputId> = outputs.iter().map(|output| output.id).collect();
        let mut changes = Vec::new();
        self.sent.retain(|surface_id, memberships| {
            let removed: Vec<OutputId> = memberships.difference(&live).copied().collect();
            for output in removed {
                memberships.remove(&output);
                changes.push(Change::Leave {
                    surface_id: *surface_id,
                    output,
                });
            }
            !memberships.is_empty()
        });
        changes
    }

    /// Reconcile the visible surface rectangles against the current output
    /// layout. A zero-area/touching edge is not an intersection, matching the
    /// Wayland notion that a surface only enters an output when pixels are on
    /// that output.
    pub fn reconcile(&mut self, outputs: &[Output], surfaces: &[(u64, Rect)]) -> Vec<Change> {
        let mut desired: BTreeMap<u64, BTreeSet<OutputId>> = BTreeMap::new();
        for &(surface_id, rect) in surfaces {
            if rect.width() <= 0.0 || rect.height() <= 0.0 {
                continue;
            }
            let memberships = desired.entry(surface_id).or_default();
            for output in outputs {
                if intersects_output(rect, output) {
                    memberships.insert(output.id);
                }
            }
        }

        let mut changes = Vec::new();
        let ids: BTreeSet<u64> = self.sent.keys().chain(desired.keys()).copied().collect();
        for surface_id in ids {
            let previous = self.sent.get(&surface_id).cloned().unwrap_or_default();
            let next = desired.get(&surface_id).cloned().unwrap_or_default();
            for output in previous.difference(&next) {
                changes.push(Change::Leave {
                    surface_id,
                    output: *output,
                });
            }
            for output in next.difference(&previous) {
                changes.push(Change::Enter {
                    surface_id,
                    output: *output,
                });
            }
        }
        self.sent = desired;
        changes
    }
}

/// Whether a surface's logical rectangle overlaps the output's.
fn intersects_output(rect: Rect, output: &Output) -> bool {
    if output.size.0 == 0 || output.size.1 == 0 {
        return false;
    }
    let o = output.logical_rect();
    rect.x0 < o.x1 && rect.x1 > o.x0 && rect.y0 < o.y1 && rect.y1 > o.y0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output(
        id: u32,
        position: (i32, i32),
        logical_position: (i32, i32),
        size: (u32, u32),
        scale: f32,
    ) -> Output {
        Output {
            id: OutputId(id),
            position,
            size,
            scale,
            transform: settings_store::hardware::OutputTransform::Normal,
            color_profile: None,
            refresh_mhz: None,
            logical_position,
            identity: crate::compositor::backend::OutputIdentity::default(),
        }
    }

    /// A 2x panel 100 logical pixels wide and a 1x one beside it.
    fn outputs() -> [Output; 2] {
        [
            output(0, (0, 0), (0, 0), (200, 200), 2.0),
            output(1, (200, 0), (100, 0), (100, 100), 1.0),
        ]
    }

    #[test]
    fn surface_output_membership_enters_and_leaves_exactly_once() {
        {
            let outputs = outputs();
            let mut memberships = Memberships::default();
            assert_eq!(
                memberships.reconcile(&outputs, &[(7, Rect::new(10.0, 10.0, 60.0, 60.0))]),
                vec![Change::Enter {
                    surface_id: 7,
                    output: OutputId(0)
                }]
            );
            assert!(
                memberships
                    .reconcile(&outputs, &[(7, Rect::new(10.0, 10.0, 60.0, 60.0))])
                    .is_empty()
            );
            assert_eq!(
                memberships.reconcile(&outputs, &[(7, Rect::new(120.0, 10.0, 170.0, 60.0))]),
                vec![
                    Change::Leave {
                        surface_id: 7,
                        output: OutputId(0)
                    },
                    Change::Enter {
                        surface_id: 7,
                        output: OutputId(1)
                    }
                ]
            );
        }
        {
            let outputs = outputs();
            let mut memberships = Memberships::default();
            assert_eq!(
                memberships.reconcile(&outputs, &[(7, Rect::new(80.0, 10.0, 120.0, 60.0))]),
                vec![
                    Change::Enter {
                        surface_id: 7,
                        output: OutputId(0)
                    },
                    Change::Enter {
                        surface_id: 7,
                        output: OutputId(1)
                    }
                ]
            );
            assert!(
                memberships
                    .reconcile(&outputs, &[(7, Rect::new(80.0, 10.0, 120.0, 60.0))])
                    .is_empty()
            );
        }
        {
            let outputs = outputs();
            let mut memberships = Memberships::default();
            memberships.reconcile(&outputs, &[(7, Rect::new(10.0, 10.0, 60.0, 60.0))]);
            // Switching a group to a different active tab removes this surface
            // from the frame's visible-geometry list.
            assert_eq!(
                memberships.reconcile(&outputs, &[]),
                vec![Change::Leave {
                    surface_id: 7,
                    output: OutputId(0)
                }]
            );
        }
        {
            let outputs = outputs();
            let mut memberships = Memberships::default();
            memberships.reconcile(&outputs, &[(7, Rect::new(120.0, 10.0, 170.0, 60.0))]);
            assert_eq!(
                memberships.leave_removed_outputs(&outputs[..1]),
                vec![Change::Leave {
                    surface_id: 7,
                    output: OutputId(1)
                }]
            );
            assert!(
                memberships
                    .reconcile(&outputs[..1], &[(7, Rect::new(120.0, 10.0, 170.0, 60.0))])
                    .is_empty()
            );
        }
        {
            let mut outputs = outputs();
            let mut memberships = Memberships::default();
            memberships.reconcile(&outputs, &[(7, Rect::new(10.0, 10.0, 60.0, 60.0))]);
            outputs[0].scale = 1.25;
            assert!(
                memberships
                    .reconcile(&outputs, &[(7, Rect::new(10.0, 10.0, 60.0, 60.0))])
                    .is_empty()
            );
        }
    }
}
