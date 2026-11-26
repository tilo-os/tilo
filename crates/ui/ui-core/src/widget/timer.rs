//! A view's own clock: an event the window fires on its own device, every so often or once after a
//! while, for as long as the view that holds it is shown and the person can see the window.
//!
//! A countdown that belongs to one window (a camera's self-timer, a slide that moves on) is
//! presentation, not the workspace's: a trigger runs wherever the workspace takes its turn, while
//! this runs where the window is. It draws nothing and takes no room. A window nobody sees (hidden,
//! minimised, covered) holds its timers: nothing fires until it is seen again, and then a tick
//! that came due while it was away fires once.
//!
//! Given what it counts from (`restart`), it counts again whenever that changes: a timer `after`
//! a short while from the text being typed ticks once the typing pauses, not at every key.

use std::cell::Cell;

use taffy::{NodeId, TaffyTree};
use vello::kurbo::{Point, Rect};

use tilo_ui_base::{component::Component, component::Described, component::Event};
use tilo_ui_base::theme::Theme;
use crate::time::{Duration, Instant};
use tilo_ui_base::{widget::TaffyTreeExt as _, widget::Widget};

thread_local! {
    /// Whether the person can see the window this thread draws: its loop says so as the window
    /// is shown, hidden or covered.
    static SHOWN: Cell<bool> = const { Cell::new(true) };
}

/// Say whether the person can see the window this thread draws: a window that is not seen holds
/// its timers.
pub fn set_window_shown(shown: bool) {
    SHOWN.with(|held| held.set(shown));
}

/// Whether the person can see the window this thread draws.
#[must_use]
pub fn window_shown() -> bool {
    SHOWN.with(Cell::get)
}

/// A clock the view holds: `tick` every `every` seconds, or once `after` seconds, counted from
/// when the view first showed it.
#[derive(Component, Default)]
#[component(
    name = "Timer",
    role = "Timer",
    not_in_gallery = "it draws nothing: its ticks are what a view does with them"
)]
pub struct TimerProps {
    /// How many seconds between ticks: it ticks again and again while it is shown; 0 for a timer
    /// that ticks once.
    pub every: f64,
    /// How many seconds until its one tick: it ticks once and then rests; 0 for one that ticks
    /// again and again.
    pub after: f64,
    /// What it counts from: whenever this changes, it counts again from now. A field's text here
    /// makes a timer `after` a short while tick once the person stops typing, not at every key.
    pub restart: String,
    /// Its time came.
    #[component(event)]
    pub tick: Event,
}

/// [`TimerProps`]' widget: nothing drawn, a deadline kept.
pub struct Timer {
    every: Option<Duration>,
    after: Option<Duration>,
    /// What it counts from ([`TimerProps::restart`]).
    restart: String,
    tick: Event,
    /// When it ticks next; `None` once a one-off tick fired, or for a timer that has no length.
    due: Cell<Option<Instant>>,
    rect: Rect,
    laid_out: bool,
    id: String,
}

/// A length of time given in seconds, when it is one: a timer of no length or a negative one
/// never ticks.
fn length(seconds: f64) -> Option<Duration> {
    (seconds.is_finite() && seconds > 0.0).then(|| Duration::from_secs_f64(seconds))
}

impl Timer {
    fn new(key: &str, props: TimerProps) -> Self {
        let mut timer = Self {
            every: None,
            after: None,
            restart: String::new(),
            tick: Event::None,
            due: Cell::new(None),
            rect: Rect::ZERO,
            laid_out: false,
            id: key.to_owned(),
        };
        timer.start(props);
        timer
    }

    /// Count from now on with `props`.
    fn start(&mut self, props: TimerProps) {
        self.every = length(props.every);
        self.after = length(props.after);
        self.restart = props.restart;
        self.tick = props.tick;
        let first = self.every.or(self.after);
        self.due
            .set(first.and_then(|length| crate::time::now().checked_add(length)));
    }

    /// Tick when the time came and the window is seen. True when it ticked.
    fn serve(&self, now: Instant) -> bool {
        let Some(due) = self.due.get() else {
            return false;
        };
        if now < due || !window_shown() {
            return false;
        }
        // The next tick counts from this one: a window that was away fires one tick, not every
        // one it missed.
        self.due
            .set(self.every.and_then(|every| now.checked_add(every)));
        self.tick.fire(None);
        true
    }
}

impl Widget for Timer {
    tilo_ui_base::described!();

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.widget_leaf(tilo_ui_base::widget::layout::removed_from_flow(), node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = tilo_ui_base::widget::layout_rect(taffy, node, origin);
        self.laid_out = true;
    }

    fn draw(&mut self, _ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {}

    fn name(&self) -> &'static str {
        "Timer"
    }

    fn test_id(&self) -> Option<&str> {
        Some(&self.id)
    }

    fn needs_redraw(&self) -> bool {
        false
    }

    fn is_layout_dirty(&self) -> bool {
        !self.laid_out
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    /// A window seen again ticks what came due while it was away.
    fn update(
        &mut self,
        _state: &tilo_ui_base::widget::AppModel,
        _path: &tilo_ui_base::widget::ModelPath,
    ) -> bool {
        self.serve(crate::time::now())
    }

    fn serve_deadline(&mut self, now: Instant) -> bool {
        self.serve(now)
    }

    /// Its next tick, while the window is seen: a window nobody sees is not woken for it.
    fn next_wake_deadline(&self) -> Option<Instant> {
        window_shown().then(|| self.due.get()).flatten()
    }
}

impl Described for Timer {
    type Props = TimerProps;

    fn from_props(key: &str, props: TimerProps) -> Self {
        Self::new(key, props)
    }

    /// Another length, or another thing it counts from, counts again from now; the same keep
    /// their count, so a view drawn again does not put its tick off.
    fn set_props(&mut self, props: TimerProps) -> bool {
        if length(props.every) != self.every
            || length(props.after) != self.after
            || props.restart != self.restart
        {
            self.start(props);
        } else {
            self.tick = props.tick;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counting() -> (tilo_ui_base::Intents<tilo_ui_base::component::Fired>, Timer) {
        let queue = tilo_ui_base::Intents::default();
        let props = TimerProps {
            every: 1.0,
            after: 0.0,
            restart: String::new(),
            tick: Event::View(tilo_ui_base::component::Intent::new(&queue, "clock", "tick")),
        };
        (queue, Timer::new("clock", props))
    }

    #[test]
    fn a_timer_ticks_when_its_time_comes_and_only_while_its_window_is_seen() {
        let (queue, timer) = counting();
        let due = timer
            .next_wake_deadline()
            .expect("a timer asks to be woken");
        assert!(!timer.serve(due - Duration::from_millis(1)), "not yet");
        set_window_shown(false);
        assert!(!timer.serve(due), "a window nobody sees holds its timer");
        assert_eq!(timer.next_wake_deadline(), None, "and is not woken for it");
        set_window_shown(true);
        let late = due + Duration::from_secs(5);
        assert!(timer.serve(late), "seen again, what came due ticks");
        assert_eq!(
            queue.take().len(),
            1,
            "once, not once for every second missed"
        );
        assert_eq!(
            timer.next_wake_deadline(),
            Some(late + Duration::from_secs(1)),
            "the next tick counts from this one"
        );
    }

    #[test]
    fn a_one_off_timer_ticks_once_and_rests() {
        let queue = tilo_ui_base::Intents::default();
        let timer = Timer::new(
            "once",
            TimerProps {
                every: 0.0,
                after: 3.0,
                restart: String::new(),
                tick: Event::View(tilo_ui_base::component::Intent::new(&queue, "once", "tick")),
            },
        );
        let due = timer
            .next_wake_deadline()
            .expect("it waits for its one tick");
        assert!(timer.serve(due));
        assert_eq!(timer.next_wake_deadline(), None, "it rests");
        assert!(!timer.serve(due + Duration::from_secs(10)));
        assert_eq!(queue.take().len(), 1);
    }

    /// A timer counting from what is typed waits for the typing to pause: each change counts
    /// again, and drawing it again with the same text keeps where it stands.
    #[test]
    fn a_timer_counts_again_when_what_it_counts_from_changes() {
        let queue = tilo_ui_base::Intents::default();
        let props = |typed: &str| TimerProps {
            every: 0.0,
            after: 0.15,
            restart: typed.to_owned(),
            tick: Event::View(tilo_ui_base::component::Intent::new(&queue, "pause", "tick")),
        };
        let mut timer = Timer::new("pause", props("w"));
        let due = timer.next_wake_deadline().expect("it waits");
        assert!(timer.serve(due), "the pause came");
        timer.set_props(props("w"));
        assert_eq!(
            timer.next_wake_deadline(),
            None,
            "the same text drawn again rests"
        );
        timer.set_props(props("wi"));
        let again = timer
            .next_wake_deadline()
            .expect("a key typed counts again");
        assert!(timer.serve(again));
        assert_eq!(queue.take().len(), 2, "one tick for each pause");
    }
}
