//! Native screen boundary routing. No encoder, stream or enrollment is involved.
//!
//! This is the outbound geometry/focus gate, not a seat injector. Callers supply
//! authenticated observations and existing exact-surface grants; the receiver
//! still applies `State::apply_remote_input` and its complete input firewall.
//! Screen positions are logical units, identical to the Displays canvas.

use settings_store::display_placement::Placement;

use super::protocol::{PeerId, SurfaceId};
use super::streaming::{InputEvent, ShareRegistry};

/// A live, authenticated peer's native content target on one logical screen.
/// A connector alone is never an identity: two peers can both have `eDP-1`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeScreen {
    pub peer: PeerId,
    pub connector: String,
    pub surface: SurfaceId,
    pub placement: Placement,
    pub online: bool,
}

/// Event addressed to the exact peer and native content surface.
#[derive(Clone, Debug, PartialEq)]
pub struct RoutedInput {
    pub peer: PeerId,
    pub surface: SurfaceId,
    pub event: InputEvent,
}

/// Ephemeral focus. A topology replacement always ends the old focus lease,
/// including reconnect with identical geometry. Identity never implies a grant.
#[derive(Default)]
pub struct NativeScreenRouter {
    screens: Vec<NativeScreen>,
    focused: Option<(PeerId, SurfaceId)>,
}

impl NativeScreenRouter {
    /// Replace observations after rearrangement, disconnect, or reconnect.
    /// Invalid/ambiguous layouts fail closed and leave no usable targets.
    pub fn replace(&mut self, screens: Vec<NativeScreen>) -> bool {
        self.focused = None;
        self.screens.clear();
        let valid = screens.iter().enumerate().all(|(index, screen)| {
            screen.placement.width > 0
                && screen.placement.height > 0
                && screen
                    .placement
                    .x
                    .checked_add(screen.placement.width)
                    .is_some()
                && screen
                    .placement
                    .y
                    .checked_add(screen.placement.height)
                    .is_some()
                && !screen.connector.is_empty()
                && screens.iter().skip(index.saturating_add(1)).all(|other| {
                    !(screen.peer == other.peer
                        && (screen.connector == other.connector || screen.surface == other.surface))
                        && !(screen.online
                            && other.online
                            && screen.placement.overlaps(other.placement))
                })
        });
        if valid {
            self.screens = screens;
        }
        valid
    }

    /// Cross a shared edge from `source` to the adjacent online peer. Both
    /// points are global logical coordinates. Large jumps across gaps or
    /// unrelated screens are denied, as are NaN/infinity and ungranted targets.
    /// Sensitive/locked source focus must never export physical input.
    pub fn cross(
        &mut self,
        source: (PeerId, SurfaceId),
        motion: ((f64, f64), (f64, f64)),
        grants: &ShareRegistry,
        sensitive: bool,
    ) -> Option<RoutedInput> {
        self.focused = None;
        if sensitive {
            return None;
        }
        let (before, after) = motion;
        let from = self
            .screens
            .iter()
            .find(|screen| screen.online && (screen.peer, screen.surface) == source)?;
        if !contains(from.placement, before) {
            return None;
        }
        let target = self.screens.iter().find(|screen| {
            screen.online
                && screen.peer != from.peer
                && screen.placement.touches(from.placement)
                && contains(screen.placement, after)
                && crosses_edge(from.placement, screen.placement, before, after)
                && grants.accept_remote_input(screen.peer, screen.surface)
        })?;
        self.focused = Some((target.peer, target.surface));
        Some(RoutedInput {
            peer: target.peer,
            surface: target.surface,
            event: InputEvent::PointerMotion {
                x: after.0 - f64::from(target.placement.x),
                y: after.1 - f64::from(target.placement.y),
            },
        })
    }

    /// Keyboard/button routing rechecks current consent for every event.
    /// Revocation or sensitive focus clears the lease before another key.
    pub fn focused_input(
        &mut self,
        event: InputEvent,
        grants: &ShareRegistry,
        sensitive: bool,
    ) -> Option<RoutedInput> {
        let (peer, surface) = self.focused?;
        if sensitive || !grants.accept_remote_input(peer, surface) {
            self.focused = None;
            return None;
        }
        // Absolute motion and enter/leave require geometry validation instead.
        if !matches!(
            event,
            InputEvent::Key { .. } | InputEvent::PointerButton { .. }
        ) {
            return None;
        }
        Some(RoutedInput {
            peer,
            surface,
            event,
        })
    }
}

fn contains(rect: Placement, point: (f64, f64)) -> bool {
    point.0.is_finite()
        && point.1.is_finite()
        && point.0 >= f64::from(rect.x)
        && point.0 < f64::from(rect.right())
        && point.1 >= f64::from(rect.y)
        && point.1 < f64::from(rect.bottom())
}

fn crosses_edge(from: Placement, to: Placement, a: (f64, f64), b: (f64, f64)) -> bool {
    let crossing = |edge: i32, start: f64, end: f64, lo: i32, hi: i32, c: f64, d: f64| {
        let t = (f64::from(edge) - start) / (end - start);
        let at = c + t * (d - c);
        (0.0..=1.0).contains(&t) && at >= f64::from(lo) && at < f64::from(hi)
    };
    if from.right() == to.x || to.right() == from.x {
        let edge = if from.right() == to.x { to.x } else { from.x };
        crossing(
            edge,
            a.0,
            b.0,
            from.y.max(to.y),
            from.bottom().min(to.bottom()),
            a.1,
            b.1,
        )
    } else {
        let edge = if from.bottom() == to.y { to.y } else { from.y };
        crossing(
            edge,
            a.1,
            b.1,
            from.x.max(to.x),
            from.right().min(to.right()),
            a.0,
            b.0,
        )
    }
}
