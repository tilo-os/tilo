//! Moving a strip under the person's hand: how a wheel or a swipe travels,
//! where a pan leaves it, and which window a pan leaves in view.
//!
//! The row itself does the moving; this module only turns input into travel
//! and travel into a resting place, so the same rules hold for a wheel, a
//! trackpad and a drag.

use ui::time::Instant;
use ui::widget::GroupRow;
use ui::winit::event::MouseScrollDelta;

use tilo_desktop_model::{GroupId, TabId};

use crate::navigation::{self, Admitted, RouteIds};

use super::{DesktopSurface, STEP_TRAVEL_PX, SWIPE_GAP, Swipe};

/// Who owns the sideways wheel gesture in progress, decided by the gesture's first event: the
/// content under the pointer, when it scrolled by that event, or the strip, which takes what
/// content leaves.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Sideways {
    Content,
    Strip,
}

/// The hand a gesture follows. A wheel or a trackpad reports its own momentum after the fingers
/// lift, so the strip is thrown nowhere at its end; the desk's own three fingers carry no momentum,
/// so the strip coasts with the velocity they let go at.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Hand {
    /// A wheel: a mouse's notches, or a trackpad, which coasts the gesture itself.
    Wheel,
    /// Three fingers on the touchpad, which the desk claims and follows.
    Fingers,
}

/// A wheel event's sideways travel in logical pixels when horizontal motion dominates, positive
/// forward (toward later windows, like Super+Right). Winit's deltas are negative for a wheel turned
/// (or fingers scrolling) rightward, which scrolls content toward its later columns and so moves
/// forward; it reports pixels physical. The shell's overlay host classifies wheel input the same
/// way.
pub(super) fn sideways(delta: MouseScrollDelta, scale_factor: f64) -> Option<f64> {
    ui::widget::sideways_travel(&delta, scale_factor)
}

/// A wheel event's vertical travel in logical pixels, positive for a wheel turned (or fingers
/// scrolling) downward, which winit reports negative.
pub(super) fn vertical(delta: MouseScrollDelta, scale_factor: f64) -> f64 {
    match delta {
        MouseScrollDelta::LineDelta(_, y) => -f64::from(y) * ui::widget::line_of_wheel(),
        MouseScrollDelta::PixelDelta(p) => -p.y / scale_factor.max(f64::EPSILON),
    }
}

/// The window a pan left the person looking at: the focused one while it is still wholly in view,
/// else the one showing the most of itself, the one nearer the middle of the view between two
/// showing as much.
pub(super) fn panned_to(row: &GroupRow, focused: Option<GroupId>) -> Option<GroupId> {
    let (left, right) = row.visible_canvas_span();
    let middle = f64::midpoint(left, right);
    let shown = |x0: f64, x1: f64| (x1.min(right) - x0.max(left)).max(0.0);
    let spans = row.group_canvas_spans();
    if spans
        .iter()
        .any(|&(id, x0, x1)| Some(id) == focused && shown(x0, x1) >= x1 - x0 - 0.5)
    {
        return focused;
    }
    spans
        .iter()
        .filter(|&&(_, x0, x1)| shown(x0, x1) > 0.0)
        .min_by(|&&(_, a0, a1), &&(_, b0, b1)| {
            let (a, b) = (shown(a0, a1), shown(b0, b1));
            if (a - b).abs() > 0.5 {
                b.total_cmp(&a)
            } else {
                let off = |x0: f64, x1: f64| (f64::midpoint(x0, x1) - middle).abs();
                off(a0, a1).total_cmp(&off(b0, b1))
            }
        })
        .map(|&(id, _, _)| id)
}

/// Pan `row` to `offset` the way the shell's direct strip pan does, within the legal range, and
/// keep it there when the strip's geometry changes. A strip showing a maximized window stays at
/// home.
pub(super) fn pan_row(row: &mut GroupRow, offset: f64) -> bool {
    if row.groups.iter().any(|g| g.maximized) {
        return false;
    }
    let target = offset.clamp(0.0, row.pan_max_scroll_x());
    let now = ui::time::now();
    row.begin_strip_pan();
    let moved = row.update_strip_pan(target - row.scroll_x(), now);
    row.end_strip_pan(now);
    if moved {
        row.hold_strip_pan();
    }
    moved
}

/// Whether `row` can pan `travel` from where it rests.
pub(super) fn can_pan(row: &GroupRow, travel: f64) -> bool {
    if row.groups.iter().any(|g| g.maximized) {
        return false;
    }
    let offset = row.scroll_x();
    if travel > 0.0 {
        offset < row.pan_max_scroll_x() - 0.5
    } else {
        travel < 0.0 && offset > 0.5
    }
}

impl super::DesktopHandle {
    /// The app of `tab` left a sideways wheel to the desk: its content did not scroll with it, so
    /// the strip its window is on moves by `travel` logical pixels, positive toward later windows,
    /// as the wheel over the strip's own canvas does. What the app scrolled with itself never
    /// arrives here, so a gesture is never split between its content and the strip: the app decided
    /// at its first event ([`ui::widget::sideways_travel`], and the app's own relay of an event it
    /// left).
    pub fn sideways_wheel(&self, tab: TabId, travel: f64) {
        let mut shared = self.shared.borrow_mut();
        shared.sideways.push((tab, travel));
        // Where the windows are is nothing the workspace publishes: the desk draws the moved strip
        // in the frame this asks for, and nothing else.
        shared.wake.wake();
    }
}

impl DesktopSurface {
    /// Move the shown strip by what the apps of its windows left of their sideways wheels
    /// ([`DesktopHandle::sideways_wheel`]): the same routing a wheel over the desk takes
    /// ([`DesktopSurface::take_sideways`]), with the content's answer already in — the app did not
    /// scroll with the event, so the strip takes the whole gesture from its first event on.
    pub(super) fn take_app_sideways(&mut self, now: ui::time::Instant) -> bool {
        let left = std::mem::take(&mut self.shared.borrow_mut().sideways);
        let mut panned = false;
        for (tab, travel) in left {
            // Only a window on the strip the person looks at moves it: an app's window on another
            // workspace's strip is not in view.
            let shown = self.shown_row_ref().is_some_and(|row| {
                row.groups
                    .iter()
                    .any(|group| group.tabs().iter().any(|shown| shown.id == tab))
            });
            if shown {
                panned |= self.take_sideways(travel, false, now);
            }
        }
        panned
    }
}

/// How a wheel the desk routes moves the strip it stands on, and who owns the gesture it is part
/// of.
impl DesktopSurface {
    /// A wheel event in a gesture the strip owns: the strip moves by its
    /// travel, and the content under the pointer never sees it
    /// ([`Self::drive_strip`]). Hands back whether the strip took the event,
    /// so its caller can stop it there.
    pub(super) fn route_strip_swipe(&mut self, travel: f64, now: Instant) -> bool {
        let Some(mut swipe) = self.swipe.filter(|swipe| swipe.owner == Sideways::Strip) else {
            return false;
        };
        swipe.last = now;
        self.drive_strip(&mut swipe, travel, now);
        self.swipe = Some(swipe);
        true
    }

    /// Move the strip by a sideways wheel event's `travel`, as part of the
    /// gesture `swipe`: it pans the strip under the pointer 1:1 within the
    /// strip and against the rubber band past either end, and when the
    /// gesture ends the strip settles and focus goes to the window panned to
    /// ([`Self::end_idle_swipe`]). A strip whose windows all fit cannot pan,
    /// so there the travel steps focus a window per [`STEP_TRAVEL_PX`].
    fn drive_strip(&mut self, swipe: &mut Swipe, travel: f64, now: Instant) {
        let forward = if travel > 0.0 { 1 } else { -1 };
        let fits = self
            .shown_row_ref()
            .is_some_and(|row| !can_pan(row, 1.0) && !can_pan(row, -1.0));
        if fits && swipe.pan.is_none() && self.neighbour(forward).is_some() {
            // A reversal starts its travel afresh.
            if swipe.travel * travel < 0.0 {
                swipe.travel = 0.0;
            }
            swipe.travel += travel;
            loop {
                if swipe.travel.abs() < STEP_TRAVEL_PX {
                    break;
                }
                let step = if swipe.travel > 0.0 { 1 } else { -1 };
                if !self.step_focus(step) {
                    swipe.travel = 0.0;
                    break;
                }
                swipe.travel -= f64::from(step) * STEP_TRAVEL_PX;
            }
        } else {
            self.pull_strip(swipe, travel, now);
        }
    }

    /// Route a wheel event the content under the pointer has had its turn
    /// with (`taken`: it scrolled by the event). The gesture's first event
    /// decides who owns the whole gesture: the content, when it took it,
    /// else the strip, which moves by that very event, the first event of a
    /// gesture on a strip that fits stepping at once so a single notch is
    /// felt. The rest of the gesture only keeps its owner.
    pub(super) fn take_sideways(&mut self, travel: f64, taken: bool, now: Instant) -> bool {
        self.take_sideways_from(travel, taken, now, Hand::Wheel, None)
    }

    /// [`Self::take_sideways`] for the hand `travel` came from: `reported` is how far it has
    /// carried the strip since it landed, for a hand that reports its total rather than one event's
    /// travel.
    fn take_sideways_from(
        &mut self,
        travel: f64,
        taken: bool,
        now: Instant,
        hand: Hand,
        reported: Option<f64>,
    ) -> bool {
        if let Some(mut swipe) = self.swipe {
            swipe.last = now;
            if let Some(total) = reported {
                swipe.carried = total;
            }
            let strip = swipe.owner == Sideways::Strip;
            if strip {
                self.drive_strip(&mut swipe, travel, now);
            }
            self.swipe = Some(swipe);
            return strip;
        }
        let forward = if travel > 0.0 { 1 } else { -1 };
        let stepping = !taken
            && self
                .shown_row_ref()
                .is_some_and(|row| !can_pan(row, 1.0) && !can_pan(row, -1.0))
            && self.neighbour(forward).is_some();
        let mut swipe = Swipe {
            owner: if taken {
                Sideways::Content
            } else {
                Sideways::Strip
            },
            hand,
            // On a strip that fits, the first event steps at once.
            travel: if stepping {
                f64::from(forward) * STEP_TRAVEL_PX - travel
            } else {
                0.0
            },
            panned: false,
            pan: None,
            carried: reported.unwrap_or(0.0),
            last: now,
        };
        if !taken {
            self.drive_strip(&mut swipe, travel, now);
        }
        self.swipe = Some(swipe);
        !taken
    }

    /// Move the shown strip by `travel` as part of `swipe`'s direct pan,
    /// beginning it on the gesture's first pull. A strip showing a
    /// maximized window stays at home.
    ///
    /// The pan follows the hand 1:1 within the strip and the rubber band past
    /// either end, from where the strip stood when the gesture took it, so a
    /// release carries the velocity the hand let go at.
    fn pull_strip(&mut self, swipe: &mut Swipe, travel: f64, now: Instant) {
        let Some(row) = self.shown_row() else {
            return;
        };
        if row.groups.iter().any(|g| g.maximized) {
            return;
        }
        // Where the gesture took the strip, and where it has carried it.
        let began = swipe.pan.is_none();
        let from = swipe.pan.unwrap_or_else(|| row.scroll_x());
        let carried = from + travel;
        swipe.pan = Some(carried);
        let moved = match swipe.hand {
            // A wheel reports where it is: the strip shows each event at once, with nothing of its
            // own to resample.
            Hand::Wheel => row.hold_strip_pull(carried, now),
            // The desk resamples three fingers' travel at each frame, so a steady swipe moves the
            // strip evenly whatever their report rate.
            Hand::Fingers => {
                if began {
                    row.begin_strip_pan();
                }
                row.update_strip_pan(swipe.carried, now)
            }
        };
        if moved {
            // Moving the windows is no change of anything the workspace publishes, as a scroll
            // region's pan is none of its app's.
            ui::widget::note_view_only_pan();
        }
        swipe.panned |= moved;
    }

    /// Three fingers carrying the shown strip `travel` pixels since they
    /// landed, positive toward later windows. `began` on their first report,
    /// which takes the strip from where it is. The strip follows them exactly,
    /// past either end against the rubber band, and a strip whose windows all
    /// fit steps focus by their travel instead.
    pub fn carry_strip(&mut self, travel: f64, began: bool) -> bool {
        let now = ui::time::now();
        if began {
            // A wheel gesture the fingers interrupt is over, and the fingers start their own.
            self.end_idle_swipe(now);
            self.swipe = None;
        }
        // The gesture reports its total, the pan takes what is new.
        let step = match self.swipe {
            Some(swipe) if swipe.hand == Hand::Fingers => travel - swipe.carried,
            _ => travel,
        };
        if step == 0.0 {
            return self.swipe.is_some();
        }
        self.take_sideways_from(step, false, now, Hand::Fingers, Some(travel))
    }

    /// The fingers carrying the strip lifted at `cancelled` : the strip coasts on the velocity they
    /// let go at, or, with the hand taken back, stays where it was thrown nowhere. A pan's end
    /// leaves focus to follow the strip once it rests ([`super::DesktopSurface`]).
    pub fn release_strip(&mut self, cancelled: bool) -> bool {
        let now = ui::time::now();
        let Some(swipe) = self.swipe.take() else {
            return false;
        };
        self.finish_swipe(swipe, now, cancelled);
        true
    }
}

/// What the strip does with a gesture once it is over: where it comes to rest, and the window
/// focus follows it to.
impl DesktopSurface {
    /// The strip travel of a wheel event: its sideways travel, or over empty canvas, where nothing
    /// scrolls, its vertical travel too, so a plain mouse wheel moves the strip there (down toward
    /// later windows).
    pub(super) fn strip_travel(&self, delta: MouseScrollDelta, scale_factor: f64) -> Option<f64> {
        // Sideways is the strip's everywhere. A plain wheel belongs to the app under it, so a mouse
        // that has only that one pans the strip where no app can scroll: over the strip's own
        // canvas and window bars. The bar is the shell's, as on the native desktop, where the wheel
        // over the tab strip is not a client's.
        sideways(delta, scale_factor).or_else(|| {
            self.shown_row_ref()
                .is_some_and(GroupRow::pointer_over_chrome)
                .then(|| vertical(delta, scale_factor))
                .filter(|travel| *travel != 0.0)
        })
    }

    /// Whether this event is in a sideways gesture the strip owns and pans with it, answered
    /// without routing it. A pan moves where the windows are and nothing else. Stepping focus on a
    /// strip that fits, a swipe at the strip's end and the end of an earlier pan, which moves focus
    /// to the window panned to, all do more. A gesture's first event is the content's to take, so
    /// only a gesture already the strip's counts.
    pub(super) fn pans_strip(&self, delta: MouseScrollDelta, now: Instant) -> bool {
        // Only the direction matters here, which no scale factor changes.
        let Some(travel) = self.strip_travel(delta, 1.0) else {
            return false;
        };
        let pans = self.shown_row_ref().is_some_and(|row| can_pan(row, travel));
        match self.swipe {
            Some(swipe) if now.saturating_duration_since(swipe.last) < SWIPE_GAP => {
                swipe.owner == Sideways::Strip && pans
            }
            Some(swipe) if swipe.panned => false,
            _ => false,
        }
    }

    /// End the swipe once wheel input has paused for [`SWIPE_GAP`]. Fingers the desk claims report
    /// their own end, so this only ends a wheel's.
    pub(super) fn end_idle_swipe(&mut self, now: Instant) {
        let Some(swipe) = self.swipe else {
            return;
        };
        if swipe.hand == Hand::Fingers || now.saturating_duration_since(swipe.last) < SWIPE_GAP {
            return;
        }
        self.swipe = None;
        self.finish_swipe(swipe, now, false);
    }

    /// The gesture `swipe` is over `cancelled` at `now` : a strip it panned settles, the hand's
    /// velocity carried on where the hand throws it, and focus follows the pan to the window the
    /// person is left looking at, so the keyboard and Super+Left/Right carry on from it.
    fn finish_swipe(&mut self, swipe: Swipe, now: Instant, cancelled: bool) {
        if swipe.pan.is_some()
            && let Some(row) = self.shown_row()
        {
            // A trackpad and a wheel already coasted the gesture (their momentum, a spun wheel's
            // notches), so the strip is thrown nowhere: a pull past an end springs back, and a
            // strip left within its range stays there through later geometry changes. Three fingers
            // carry no momentum of their own, so the strip coasts with the velocity they let go at,
            // unless the hand was taken back.
            let settling = match (swipe.hand, cancelled) {
                (Hand::Wheel, _) | (Hand::Fingers, true) => row.cancel_strip_pan(now),
                (Hand::Fingers, false) => row.end_strip_pan(now),
            };
            if !settling {
                row.hold_strip_pan();
            }
        }
        if swipe.panned {
            self.pan_follow = self.shown;
        }
    }

    /// Focus the window the strip has come to rest on, once the pan that moved it is over: a thrown
    /// strip coasts past the window the fingers let go of, and a pull past an end springs back, so
    /// focus waits for where the strip ends. The strip itself stays where the person left it
    /// ([`Self::settle`]).
    pub(super) fn settle_pan_focus(&mut self) {
        let Some(panned) = self.pan_follow else {
            return;
        };
        if self.shown != Some(panned) {
            self.pan_follow = None;
            return;
        }
        if self
            .shown_row_ref()
            .is_some_and(ui::widget::GroupRow::direct_pan_in_progress)
        {
            return;
        }
        self.pan_follow = None;
        let Some((current, group, tab)) =
            self.shown
                .zip(self.shown_row_ref())
                .and_then(|(strip, row)| {
                    let current = navigation::active_route(&self.shared.borrow().tree)?;
                    if strip != current.strip {
                        return None;
                    }
                    let group = panned_to(row, Some(current.group))?;
                    let tab = row
                        .groups
                        .iter()
                        .find(|g| g.id == group)
                        .map(ui::widget::WindowGroup::active_tab_id)?;
                    Some((current, group, tab))
                })
        else {
            return;
        };
        if group == current.group {
            return;
        }
        self.shared
            .borrow_mut()
            .queue
            .push(Admitted::Select(RouteIds {
                group,
                tab,
                ..current
            }));
        self.settle = Some(group);
    }

    /// Focus the window `steps` along the shown strip, walking every tab of every group in order
    /// and stopping at either end, as Super+Left/Right does in the shell. False when there is
    /// nowhere to go.
    fn step_focus(&self, steps: i32) -> bool {
        let Some(target) = self.neighbour(steps) else {
            return false;
        };
        self.shared
            .borrow_mut()
            .queue
            .push(Admitted::Select(target));
        true
    }

    /// The window `steps` along the shown strip from the focused one (or the one a queued select is
    /// about to focus), as [`Self::step_focus`] walks it.
    fn neighbour(&self, steps: i32) -> Option<RouteIds> {
        let shared = self.shared.borrow();
        // A step queued since the last update has moved focus already.
        let current = shared
            .queue
            .iter()
            .rev()
            .find_map(|admitted| match admitted {
                Admitted::Select(route) => Some(*route),
                _ => None,
            })
            .or_else(|| navigation::active_route(&shared.tree))?;
        let strip = shared
            .tree
            .workspaces
            .iter()
            .find(|w| w.slot == current.workspace)
            .and_then(|w| w.strips.iter().find(|s| s.id == current.strip))?;
        let windows: Vec<(GroupId, TabId)> = strip
            .window_groups
            .iter()
            .flat_map(|g| g.tabs.iter().map(move |t| (g.id, t.id)))
            .collect();
        let at = windows
            .iter()
            .position(|&(g, t)| g == current.group && t == current.tab)?;
        let &(group, tab) = at
            .checked_add_signed(isize::try_from(steps).unwrap_or(0))
            .and_then(|index| windows.get(index))?;
        Some(RouteIds {
            group,
            tab,
            ..current
        })
    }
}
