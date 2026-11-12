//! What the banner stack paints, routes and asks: layout, clamp, keyboard,
//! swipe, motion and the inline reply.

use super::super::{HistoryActivation, NotificationLog};
use super::*;
use std::cell::{Cell, RefCell};
use ui::time::Duration;

/// A host with no senders: dismissing retains, activating retains.
#[derive(Default)]
pub(crate) struct TestHost {
    pub log: RefCell<NotificationLog>,
    pub locked: Cell<bool>,
    pub quiet: RefCell<Option<String>>,
    pub actions: RefCell<Vec<(String, String)>>,
    pub invoked: RefCell<Vec<(u64, String)>>,
    pub centre_opened: Cell<usize>,
}

impl TestHost {
    pub fn push(&self, notification: Notification) -> u64 {
        let id = notification.id;
        let evicted = self.log.borrow_mut().show(notification);
        for n in evicted {
            self.log.borrow_mut().retain(n);
        }
        id
    }
}

/// Run `f` on `stack` over what `host` shows now, and carry out what it asked, as a host does.
fn act<T>(
    stack: &mut NotificationStack,
    host: &TestHost,
    f: impl FnOnce(&mut NotificationStack) -> T,
) -> T {
    stack.set_feed(BannerFeed::of(host));
    let out = f(stack);
    for ask in stack.take_asks() {
        ask.answer(host);
    }
    out
}

fn paint(
    stack: &mut NotificationStack,
    host: &TestHost,
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
) {
    act(stack, host, |stack| {
        stack.draw(scene, transform, text, theme);
    });
}

impl NotificationHost for TestHost {
    fn actions_for(&self, _notice: &Notification) -> Vec<(String, String)> {
        self.actions.borrow().clone()
    }
    fn invoke_action(&self, id: u64, key: &str) {
        self.invoked.borrow_mut().push((id, key.to_owned()));
    }
    fn log(&self) -> std::cell::Ref<'_, NotificationLog> {
        self.log.borrow()
    }
    fn session_locked(&self) -> bool {
        self.locked.get()
    }
    fn quiet_status(&self) -> Option<String> {
        self.quiet.borrow().clone()
    }
    fn present(&self, now: Instant, on_screen: &[u64], held: bool) {
        self.log.borrow_mut().present(now, on_screen, held);
    }
    fn open_center(&self) {
        self.centre_opened
            .set(self.centre_opened.get().saturating_add(1));
    }
    fn dismiss(&self, id: u64) {
        let taken = self.log.borrow_mut().take_active(id);
        if let Some(n) = taken {
            self.log.borrow_mut().retain(n);
        }
    }
    fn activate(&self, id: u64) {
        self.dismiss(id);
    }
    fn activate_history(&self, id: u64) -> HistoryActivation {
        let Some(n) = self.log.borrow().history_entry(id).cloned() else {
            return HistoryActivation::Missing;
        };
        let Some(app_id) = n.app_id.clone() else {
            return HistoryActivation::Unavailable;
        };
        self.log.borrow_mut().take_history(id);
        HistoryActivation::OpenApp {
            app_id,
            label: super::super::group_label(&n),
        }
    }
    fn dismiss_history(&self, id: u64) {
        self.log.borrow_mut().take_history(id);
    }
    fn clear_history(&self) {
        self.log.borrow_mut().clear_history(ui::time::now());
    }
    fn undo_clear(&self) {
        self.log.borrow_mut().undo_clear();
    }
}

#[test]
fn clicking_a_painted_close_rect_dismisses_only_that_notification() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = ui::motion::testing::settled();
    let host = TestHost::default();
    let first = host.push(Notification::new("First", ""));
    let second = host.push(Notification::new("Second", ""));
    let mut stack = NotificationStack::new();
    // The paint records this exact geometry; seeding it keeps the routing test independent of a GPU
    // while exercising the production hit test.
    stack.hit_rects.push((
        first,
        Rect::new(100.0, 100.0, 460.0, 164.0),
        Rect::new(430.0, 106.0, 452.0, 128.0),
    ));
    stack.hit_rects.push((
        second,
        Rect::new(100.0, 172.0, 460.0, 236.0),
        Rect::new(430.0, 178.0, 452.0, 200.0),
    ));
    assert!(act(&mut stack, &host, |s| s.click_at(Point::new(441.0, 189.0))));
    let remaining: Vec<_> = host
        .log()
        .active()
        .iter()
        .map(|n| n.title.clone())
        .collect();
    assert_eq!(remaining, vec!["First".to_string()]);
    assert_eq!(
        host.log().history().len(),
        1,
        "a dismissed banner is retained"
    );
    stack.hit_rects.clear();
    assert!(!act(&mut stack, &host, |s| s.click_at(Point::new(441.0, 189.0))));
}

#[test]
fn shared_choice_buttons_route_their_keys_and_retire_stale_geometry() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = ui::motion::testing::settled();
    for theme in [Theme::light(), Theme::dark(), Theme::high_contrast()] {
        let theme = theme.with_reduce_motion(true).with_text_scale(2.0);
        let host = TestHost::default();
        host.actions.replace(vec![
            ("dismiss".into(), "Dismiss".into()),
            ("snooze".into(), "Snooze".into()),
        ]);
        let id = host.push(Notification::new("Alarm", "Synthetic alarm"));
        let mut stack = NotificationStack::new();
        stack.set_viewport(Rect::new(0.0, 0.0, 320.0, 600.0));
        let mut text = TextRenderer::new();
        paint(
            &mut stack,
            &host,
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut text,
            &theme,
        );
        assert_eq!(
            stack.action_rects.len(),
            2,
            "both real painted controls must exist"
        );
        let actions = stack.action_rects.clone();
        let buttons = act(&mut stack, &host, |s| s.accessibility())
            .expect("a live region")
            .children;
        let mut named: Vec<_> = buttons
            .iter()
            .map(|button| (button.role, button.name.clone().unwrap_or_default()))
            .collect();
        named.sort_by(|a, b| a.1.cmp(&b.1));
        assert_eq!(
            named,
            [
                (ui::a11y::Role::Button, "Alarm".to_owned()),
                (ui::a11y::Role::Button, "Dismiss".to_owned()),
                (ui::a11y::Role::Button, "Snooze".to_owned())
            ],
            "the painted choices are buttons a reader can press"
        );
        // Assistive technology presses them by their targets, as a click does: each choice reaches
        // the host with its key.
        for (_, key, _) in &actions {
            let label = if key == "dismiss" {
                "Dismiss"
            } else {
                "Snooze"
            };
            let target = buttons
                .iter()
                .find(|button| button.name.as_deref() == Some(label))
                .and_then(|button| button.action_target.clone())
                .expect("a choice a reader can press carries its target");
            assert!(act(&mut stack, &host, |s| s
                .perform_semantic(&target, &ui::a11y::Action::Activate)));
            assert_eq!(host.invoked.borrow().last(), Some(&(id, key.clone())));
        }
        assert!(!act(&mut stack, &host, |s| s.perform_semantic(
            "notification:none",
            &ui::a11y::Action::Activate
        )));
        let card = stack.hit_rects[0].1;
        for (actual, _, rect) in &actions {
            assert_eq!(*actual, id);
            assert!(rect.width() > 0.0 && rect.height() >= 28.0);
            assert!(card.contains(rect.origin()) && card.x1 >= rect.x1 && card.y1 >= rect.y1);
        }
        assert!(actions[0].2.intersect(actions[1].2).area() <= 0.0);
        for (_, key, rect) in &actions {
            assert!(act(&mut stack, &host, |s| s.click_at(rect.center())));
            assert_eq!(host.invoked.borrow().last(), Some(&(id, key.clone())));
        }
        assert_eq!(
            host.log().active().len(),
            1,
            "host owns durable completion, not the painter"
        );
        host.log.borrow_mut().take_active(id);
        paint(
            &mut stack,
            &host,
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut text,
            &theme,
        );
        assert!(stack.action_rects.is_empty());
        assert!(!act(&mut stack, &host, |s| s.click_at(actions[1].2.center())));
    }
}

#[test]
fn the_frame_a_banner_arrives_differs_from_the_one_before() {
    // The running clock: an arriving banner's fade is at its start.
    for theme in [Theme::light(), Theme::dark(), Theme::high_contrast()] {
        for theme in [theme.clone().with_reduce_motion(true), theme] {
            let host = TestHost::default();
            let mut stack = NotificationStack::new();
            stack.set_viewport(Rect::new(0.0, 0.0, 800.0, 600.0));
            let mut text = TextRenderer::new();
            let mut before = Scene::new();
            paint(
                &mut stack,
                &host,
                &mut before,
                Affine::IDENTITY,
                &mut text,
                &theme,
            );
            host.push(Notification::new("Build finished", "it passed its tests"));
            let mut arriving = Scene::new();
            paint(
                &mut stack,
                &host,
                &mut arriving,
                Affine::IDENTITY,
                &mut text,
                &theme,
            );
            assert_ne!(
                arriving.encoding().n_paths,
                before.encoding().n_paths,
                "the frame a banner arrives records it, though unseen"
            );
            assert!(!stack.rect().is_zero_area(), "it covers where it arrives");
        }
    }
}

#[test]
fn scaled_attributed_banner_reserves_text_above_real_action_controls() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = ui::motion::testing::settled();
    for theme in [Theme::light(), Theme::dark(), Theme::high_contrast()] {
        let theme = theme.with_reduce_motion(true).with_text_scale(2.0);
        let host = TestHost::default();
        host.actions.replace(vec![
            ("dismiss".into(), "Dismiss".into()),
            ("snooze".into(), "Snooze".into()),
        ]);
        host.push(
            Notification::new("Meeting begins", "Join your workspace").with_app_name("Calendar"),
        );
        let mut stack = NotificationStack::new();
        stack.set_viewport(Rect::new(0.0, 0.0, 320.0, 600.0));
        let mut text = TextRenderer::new();
        paint(
            &mut stack,
            &host,
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut text,
            &theme,
        );
        let card = stack.hit_rects[0].1;
        let actions_top = stack
            .action_rects
            .iter()
            .map(|(_, _, rect)| rect.y0)
            .reduce(f64::min)
            .expect("real shared actions");
        let content_height: f64 = [
            ("Calendar", theme.text(TextRole::Caption)),
            ("Meeting begins", theme.text(TextRole::Label).strong()),
            ("Join your workspace", theme.text(TextRole::Caption)),
        ]
        .into_iter()
        .map(|(label, style)| f64::from(text.measure_style(label, None, &style).1))
        .sum();
        assert!(
            actions_top - card.y0 >= content_height + 2.0 * f64::from(theme.tokens.spacing.s2),
            "painted text requires {content_height}px plus padding before actions at {}px",
            actions_top - card.y0
        );
    }
}

#[test]
fn persistent_alarm_choices_fit_the_viewport_and_keep_the_newest_alarm_visible() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = ui::motion::testing::settled();
    for theme in [Theme::light(), Theme::dark(), Theme::high_contrast()] {
        let theme = theme.with_reduce_motion(true).with_text_scale(2.0);
        let host = TestHost::default();
        host.actions.replace(vec![
            ("dismiss".into(), "Dismiss".into()),
            ("snooze".into(), "Snooze".into()),
        ]);
        let mut newest = 0;
        for index in 0..super::super::MAX_NOTIFICATIONS {
            newest = host.push(
                Notification::new(format!("Alarm {index}"), "Fixture")
                    .with_dismiss_after(super::super::PERSISTENT_DISMISS),
            );
        }
        let viewport = Rect::new(0.0, 0.0, 320.0, 600.0);
        let mut stack = NotificationStack::new();
        stack.set_viewport(viewport);
        paint(
            &mut stack,
            &host,
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &theme,
        );
        assert!(
            !stack.hit_rects.is_empty(),
            "the newest alarm must actually paint"
        );
        assert!(
            stack.hit_rects.len() < host.log().active().len(),
            "fixture must exercise capacity"
        );
        assert_eq!(stack.hit_rects[0].0, newest);
        for (_, card, _) in &stack.hit_rects {
            assert!(
                card.y0 >= viewport.y0 && card.y1 <= viewport.y1,
                "a painted card escaped the viewport"
            );
        }
        for (_, _, control) in &stack.action_rects {
            assert!(
                control.y0 >= viewport.y0 && control.y1 <= viewport.y1,
                "a real alarm choice is unreachable below the viewport"
            );
        }
        assert_eq!(
            host.log().active().len(),
            super::super::MAX_NOTIFICATIONS,
            "fitting banners must not discard alarm data"
        );
    }
}

#[test]
fn compact_viewport_keeps_notification_inside_both_edges() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = ui::motion::testing::settled();
    let mut stack = NotificationStack::new();
    stack.set_viewport(Rect::new(0.0, 0.0, 320.0, 480.0));
    let width = stack.toast_width();
    let x0 = stack.viewport.x1 - CORNER_INSET - width;
    assert_eq!(width, 320.0 - CORNER_INSET * 2.0);
    assert_eq!(x0, CORNER_INSET);
}

fn draw(stack: &mut NotificationStack, host: &TestHost, theme: &Theme) {
    paint(
        stack,
        host,
        &mut Scene::new(),
        Affine::IDENTITY,
        &mut TextRenderer::new(),
        theme,
    );
}

/// A transient's slot shifts below any window bar it would cover, and stays where nothing is under
/// it.
#[test]
fn a_window_bar_wins_and_the_transient_shifts() {
    let viewport = Rect::new(0.0, 40.0, 1600.0, 900.0);
    let free = transient_slot(viewport, 0.0, (360.0, 60.0), &[]);
    assert_eq!(free.y0, 40.0 + CORNER_INSET);
    assert_eq!(free.x1, 1600.0 - CORNER_INSET);
    let bars = [
        Rect::new(900.0, 48.0, 1590.0, 92.0),
        Rect::new(0.0, 48.0, 880.0, 400.0),
    ];
    let slot = transient_slot(viewport, 0.0, (360.0, 60.0), &bars);
    assert!(bars.iter().all(|bar| bar.intersect(slot).area() <= 0.0));
    assert_eq!(slot.y0, 92.0 + TOAST_GAP, "just below the bar it met");
}

/// Banners start below the window bars the host keeps clear, so a window's maximise and close under
/// the corner keep their clicks.
#[test]
fn banners_start_below_the_window_bars() {
    // Motion reads at its end: this checks where the banner rests.
    let _settled = ui::motion::testing::settled();
    let host = TestHost::default();
    host.push(Notification::new("Keep your name and work?", "A passkey"));
    let mut stack = NotificationStack::new();
    stack.set_viewport(Rect::new(0.0, 32.0, 1440.0, 900.0));
    assert!((stack.first_top() - (32.0 + CORNER_INSET)).abs() < f64::EPSILON);
    // The windows' content starts at 76: their bars run from 32 to 76.
    stack.set_clear_top(76.0);
    draw(&mut stack, &host, &Theme::dark());
    let (_, card, _) = stack
        .hit_rects
        .first()
        .copied()
        .expect("the banner is painted");
    assert!(
        (card.y0 - (76.0 + TOAST_GAP)).abs() < 0.5,
        "{card:?} rests just below the window bars"
    );
    assert!(
        stack.rect().y0 >= 76.0,
        "{:?} covers a window bar",
        stack.rect()
    );
}

/// The stack keeps clear of the window bars under its column at the top, and only of those: a bar
/// beside the column or a window further down leaves the banners at the corner.
#[test]
fn banners_keep_clear_of_the_window_bars_under_the_corner() {
    let _settled = ui::motion::testing::settled();
    let host = TestHost::default();
    host.push(Notification::new("Jonas is calling", "Video call"));
    let viewport = Rect::new(0.0, 32.0, 1440.0, 900.0);
    let corner = 32.0 + CORNER_INSET;
    let mut stack = NotificationStack::new();
    stack.set_viewport(viewport);

    // A window on the left half: its bar is not under the banners.
    stack.set_window_bars(vec![Rect::new(0.0, 36.0, 700.0, 80.0)]);
    assert!((stack.first_top() - corner).abs() < f64::EPSILON);
    // A window lower down, under the first slot: no bar to clear.
    stack.set_window_bars(vec![Rect::new(0.0, 400.0, 1440.0, 444.0)]);
    assert!((stack.first_top() - corner).abs() < f64::EPSILON);

    // A window reaching the right edge, its bar with a toolbar row: the banner starts below it.
    let bar = Rect::new(720.0, 36.0, 1428.0, 120.0);
    stack.set_window_bars(vec![Rect::new(0.0, 36.0, 700.0, 80.0), bar]);
    draw(&mut stack, &host, &Theme::dark());
    let (_, card, _) = stack
        .hit_rects
        .first()
        .copied()
        .expect("the banner is painted");
    assert!(
        (card.y0 - (bar.y1 + TOAST_GAP)).abs() < 0.5,
        "{card:?} rests just below the window bar {bar:?}"
    );
    assert!(
        stack.rect().intersect(bar).area() <= 0.0,
        "{:?} covers the window bar {bar:?}",
        stack.rect()
    );

    // The window goes: the banners go back to the corner.
    stack.set_window_bars(Vec::new());
    assert!((stack.first_top() - corner).abs() < f64::EPSILON);
}

#[test]
fn live_notification_requests_entry_frames_and_a_future_exit_wake() {
    let host = TestHost::default();
    host.push(Notification::new("Saved", "Ready"));
    let mut stack = NotificationStack::new();
    assert_eq!(
        act(&mut stack, &host, |s| s.next_wake_deadline()),
        None,
        "a banner not yet on screen has no time running"
    );
    stack.set_viewport(Rect::new(0.0, 0.0, 800.0, 600.0));
    draw(&mut stack, &host, &Theme::dark());
    assert!(act(&mut stack, &host, |s| s.needs_animation_frame()));
    assert!(act(&mut stack, &host, |s| s.next_wake_deadline()).is_some());
}

/// Reduced motion is a designed experience, not "off": a banner fades
/// in where it rests at `fast` (a change of what is shown still reads),
/// nothing slides, and it fades out so it is gone at expiry.
#[test]
fn under_reduced_motion_a_banner_fades_in_place() {
    let host = TestHost::default();
    host.push(Notification::new("Saved", "Ready"));
    let mut stack = NotificationStack::new();
    stack.set_viewport(Rect::new(0.0, 0.0, 800.0, 600.0));
    draw(&mut stack, &host, &Theme::dark().with_reduce_motion(true));
    assert!(
        act(&mut stack, &host, |s| s.needs_animation_frame()),
        "it fades in"
    );
    assert!(act(&mut stack, &host, |s| s.next_wake_deadline()).is_some());
    let n = host.log().active()[0].clone();
    let shown = n.dwell.first_shown().unwrap();
    let (alpha, dx) = NotificationStack::animation_state(
        &n,
        shown + Duration::from_millis(40),
        (true, false),
        272.0,
    );
    assert!(alpha > 0.0 && alpha < 1.0, "mid-fade: {alpha}");
    assert!(dx.abs() < f64::EPSILON, "nothing slides");
    assert_eq!(
        NotificationStack::animation_state(
            &n,
            shown + Duration::from_secs(1),
            (true, false),
            272.0
        ),
        (1.0, 0.0),
        "then it stands"
    );
}

/// A banner below one that leaves springs up to its new slot instead of jumping
/// (`motion::ListChange`), and lands there; while the person holds the stack, a slot closing upward
/// waits, so nothing moves under the pointer.
#[test]
fn a_banner_springs_to_its_slot_and_waits_while_held() {
    let mut stack = NotificationStack::new();
    let t0 = ui::time::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    assert!(
        (stack.slot_top(7, 100.0, t0, false) - 100.0).abs() < 1e-9,
        "a new banner starts in its slot"
    );
    // The banner above it left: its slot is higher now.
    assert!(
        (stack.slot_top(7, 20.0, t0, false) - 100.0).abs() < 1e-9,
        "no jump"
    );
    let mid = stack.slot_top(7, 20.0, at(60), false);
    assert!(mid < 100.0 && mid > 20.0, "on its way up: {mid}");
    assert!(stack.slots.iter().any(|(_, slot)| slot.is_moving(at(60))));
    let landed = stack.slot_top(7, 20.0, at(600), false);
    assert!((landed - 20.0).abs() < 0.5, "it lands in the freed slot");

    stack.slot_top(8, 200.0, t0, false);
    let held = stack.slot_top(8, 120.0, at(10), true);
    assert!((held - 200.0).abs() < 1e-9, "held: it waits");
    stack.slot_top(8, 120.0, at(20), false);
    let released = stack.slot_top(8, 120.0, at(80), false);
    assert!(released < 200.0, "let go: it closes up");
}

/// Banners that do not fit fold into one "N more" card that opens the centre; they stand still
/// until they come on screen, and only the banners on screen age.
#[test]
fn banners_that_do_not_fit_fold_into_a_more_card_and_do_not_age() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = ui::motion::testing::settled();
    for theme in [Theme::light(), Theme::dark(), Theme::high_contrast()] {
        let theme = theme.with_reduce_motion(true);
        let host = TestHost::default();
        let ids: Vec<u64> = (0..super::super::MAX_NOTIFICATIONS)
            .map(|i| host.push(Notification::new(format!("Banner {i}"), "Body")))
            .collect();
        // Three stand at most, and this corner has room for one of them.
        let viewport = Rect::new(0.0, 0.0, 800.0, 180.0);
        let mut stack = NotificationStack::new();
        stack.set_viewport(viewport);
        draw(&mut stack, &host, &theme);
        let shown = stack.on_screen.clone();
        assert!(!shown.is_empty() && shown.len() < ids.len());
        assert_eq!(shown[0], *ids.last().unwrap(), "the newest leads");
        let more = stack.more_rect;
        assert!(more.area() > 0.0, "the rest fold into one card");
        assert!(more.y1 <= viewport.y1 - CORNER_INSET);
        let hidden = ids.len() - shown.len();
        let label = t_args("notification-more", &[("count", hidden.into())]);
        assert!(
            stack
                .action_buttons
                .iter()
                .any(|(rect, name, _)| *rect == more && *name == label),
            "the card names how many more: {label}"
        );
        let log = host.log();
        for n in log.active() {
            assert_eq!(
                n.dwell.is_running(),
                shown.contains(&n.id),
                "only a banner on screen ages"
            );
        }
        drop(log);
        assert!(act(&mut stack, &host, |s| s.click_at(more.center())));
        assert_eq!(host.centre_opened.get(), 1, "the card opens the centre");
    }
}

/// The corner stands at most three banners. Further arrivals from one app fold into the last one's
/// caption, which opens the centre where they all are; their own choices and close still reach
/// them.
#[test]
fn a_fourth_banner_folds_into_the_last_ones_caption_and_opens_the_centre() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = ui::motion::testing::settled();
    let host = TestHost::default();
    for i in 0..5 {
        host.push(
            Notification::new(format!("Note {i}"), "Body")
                .with_app_id("notes")
                .with_app_name("Notes"),
        );
    }
    let mut stack = NotificationStack::new();
    stack.set_viewport(Rect::new(0.0, 0.0, 800.0, 900.0));
    paint(
        &mut stack,
        &host,
        &mut Scene::new(),
        Affine::IDENTITY,
        &mut TextRenderer::new(),
        &Theme::dark().with_reduce_motion(true),
    );
    assert_eq!(stack.on_screen.len(), MOST_BANNERS, "three stand, no more");
    assert_eq!(stack.more_rect, Rect::ZERO, "no separate card");
    let label = t_args(
        "notification-more-from",
        &[("count", 2.into()), ("app", "Notes".into())],
    );
    let folded = stack
        .action_buttons
        .iter()
        .find(|(_, name, _)| *name == label)
        .unwrap_or_else(|| panic!("the last banner folds the rest: {label}"));
    assert_eq!(folded.2, BannerTarget::More.key());
    // The oldest of the three carries the fold; the others open themselves.
    assert_eq!(stack.folded, stack.on_screen.last().copied());
    let log = host.log();
    for n in log.active() {
        let standing = stack.on_screen.contains(&n.id);
        assert_eq!(
            n.dwell.is_running(),
            standing,
            "only a banner on screen ages"
        );
    }
    drop(log);
    let at = folded.0.center();
    assert!(act(&mut stack, &host, |s| s.click_at(at)));
    assert_eq!(host.centre_opened.get(), 1, "the caption opens the centre");
    assert_eq!(host.log().active().len(), 5, "and nothing was dismissed");
}

/// Resting the pointer on the stack holds every banner on it: none fades or expires under the
/// pointer, and letting go starts them again.
#[test]
fn the_pointer_resting_on_the_stack_holds_the_banners() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = ui::motion::testing::settled();
    let host = TestHost::default();
    host.push(Notification::new("Ada", "Can you look?"));
    let mut stack = NotificationStack::new();
    stack.set_viewport(Rect::new(0.0, 0.0, 800.0, 600.0));
    draw(&mut stack, &host, &Theme::dark().with_reduce_motion(true));
    let device_id = winit::event::DeviceId::dummy();
    let at = |point: Point| WindowEvent::CursorMoved {
        device_id,
        position: winit::dpi::PhysicalPosition::new(point.x, point.y),
    };
    let mut handled = false;
    let centre = stack.rect().center();
    assert!(act(&mut stack, &host, |s| s.handle_pointer_event(
        &at(centre),
        1.0,
        &mut handled
    )));
    assert!(stack.held());
    assert!(host.log().held());
    assert!(!host.log().active()[0].dwell.is_running());
    assert_eq!(act(&mut stack, &host, |s| s.next_wake_deadline()), None);
    let n = host.log().active()[0].clone();
    assert_eq!(
        NotificationStack::animation_state(
            &n,
            ui::time::now() + Duration::from_secs(60),
            (false, true),
            272.0
        )
        .0,
        1.0,
        "a held banner does not fade"
    );
    assert!(act(&mut stack, &host, |s| s.handle_pointer_event(
        &at(Point::new(5.0, 590.0)),
        1.0,
        &mut handled
    )));
    assert!(!stack.held());
    assert!(host.log().active()[0].dwell.is_running());
}

/// Passing over the stack on the way elsewhere is transit and holds nothing; heading for it and
/// slowing to a stop there holds it, and a hand that stops short lets go after the aim lapses.
#[test]
fn only_a_pointer_reaching_for_the_stack_holds_it() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = ui::motion::testing::settled();
    let host = TestHost::default();
    host.push(Notification::new("Ada", "Can you look?"));
    let mut stack = NotificationStack::new();
    stack.set_viewport(Rect::new(0.0, 0.0, 800.0, 600.0));
    draw(&mut stack, &host, &Theme::dark().with_reduce_motion(true));
    let card = stack.rect();
    let y = card.center().y;
    let t0 = ui::time::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);

    // A fast sweep down the screen across the stack, 40 px every 8 ms.
    let x = card.center().x;
    let mut ms = 0;
    for step in 0..16_u32 {
        let py = card.y0 - 120.0 + 40.0 * f64::from(step);
        act(&mut stack, &host, |s| {
            s.note_pointer_at(Some(Point::new(x, py)), at(ms))
        });
        if card.contains(Point::new(x, py)) {
            assert!(!stack.pointer_holds(at(ms)), "in transit at {py}");
        }
        ms += 8;
    }
    assert!(!host.log().held());
    act(&mut stack, &host, |s| s.note_pointer_at(None, at(ms)));

    // Reaching for the stack from the left: slowing to a stop on it.
    let t1 = ms + 500;
    let path = [
        (card.x0 - 200.0, 0),
        (card.x0 - 100.0, 16),
        (card.x0 - 40.0, 32),
        (card.x0 - 10.0, 48),
        (card.x0 + 2.0, 64),
    ];
    for (px, dt) in path {
        act(&mut stack, &host, |s| {
            s.note_pointer_at(Some(Point::new(px, y)), at(t1 + dt))
        });
    }
    assert!(
        stack.pointer_holds(at(t1 + 64)),
        "heading there and coming to rest"
    );
    assert!(host.log().held());
    assert!(
        stack.pointer_holds(at(t1 + 64 + 200)),
        "resting on the stack keeps holding it"
    );

    // Heading for it but stopping short: the aim lapses.
    act(&mut stack, &host, |s| {
        s.note_pointer_at(None, at(t1 + 1000))
    });
    let t2 = t1 + 2000;
    for (px, dt) in [
        (card.x0 - 300.0, 0),
        (card.x0 - 150.0, 16),
        (card.x0 - 60.0, 32),
    ] {
        act(&mut stack, &host, |s| {
            s.note_pointer_at(Some(Point::new(px, y)), at(t2 + dt))
        });
    }
    let lapse = at(t2 + 32) + AIM_HOLD;
    assert!(
        !stack.pointer_holds(lapse),
        "a hand that stops short lets go"
    );
}

/// The Notifications chord moves focus into the newest banner: arrows
/// walk its controls and the banners, Return runs the focused control,
/// Escape dismisses the banner; either gives focus back.
#[test]
fn keyboard_focus_reaches_banner_actions_and_escape_dismisses() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = ui::motion::testing::settled();
    let host = TestHost::default();
    host.actions.replace(vec![
        ("dismiss".into(), "Dismiss".into()),
        ("snooze".into(), "Snooze".into()),
    ]);
    let older = host.push(Notification::new("Alarm", "Tea"));
    let newest = host.push(Notification::new("Alarm", "Meeting"));
    let mut stack = NotificationStack::new();
    stack.set_viewport(Rect::new(0.0, 0.0, 800.0, 900.0));
    let theme = Theme::dark().with_reduce_motion(true);
    draw(&mut stack, &host, &theme);
    assert!(!stack.owns_key(&Key::Named(NamedKey::Enter)));
    assert!(act(&mut stack, &host, NotificationStack::focus_newest));
    assert!(host.log().held(), "a focused banner does not age");
    assert_eq!(
        stack.focus,
        Some(BannerTarget::Action(newest, "dismiss".into())),
        "focus lands on the first choice, as painted"
    );
    assert!(!stack.owns_key(&Key::Character("a".into())));
    assert!(act(&mut stack, &host, |s| s
        .handle_key(&Key::Named(NamedKey::ArrowRight), false)));
    assert_eq!(
        stack.focus,
        Some(BannerTarget::Action(newest, "snooze".into()))
    );
    assert!(act(&mut stack, &host, |s| s
        .handle_key(&Key::Named(NamedKey::ArrowDown), false)));
    assert_eq!(stack.focus, Some(BannerTarget::Open(older)));
    assert!(act(&mut stack, &host, |s| s
        .handle_key(&Key::Named(NamedKey::ArrowUp), false)));
    assert_eq!(stack.focus, Some(BannerTarget::Open(newest)));
    assert!(act(&mut stack, &host, |s| s
        .handle_key(&Key::Named(NamedKey::Tab), false)));
    assert!(act(&mut stack, &host, |s| s
        .handle_key(&Key::Named(NamedKey::Tab), false)));
    assert!(act(&mut stack, &host, |s| s
        .handle_key(&Key::Named(NamedKey::Enter), false)));
    assert_eq!(
        host.invoked.borrow().last(),
        Some(&(newest, "snooze".to_owned()))
    );
    assert!(!stack.has_keyboard_focus(), "acting gives focus back");
    assert!(!host.log().held());

    draw(&mut stack, &host, &theme);
    assert!(act(&mut stack, &host, NotificationStack::focus_newest));
    assert!(act(&mut stack, &host, |s| s
        .handle_key(&Key::Named(NamedKey::Escape), false)));
    assert!(!stack.has_keyboard_focus());
    assert!(
        host.log().active().iter().all(|n| n.id != newest),
        "Escape dismisses the focused banner"
    );
}

/// Do Not Disturb puts no card on screen: with no banner, the stack paints nothing.
#[test]
fn quiet_mode_paints_no_card_of_its_own() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = ui::motion::testing::settled();
    let host = TestHost::default();
    host.quiet.replace(Some("Do Not Disturb".into()));
    host.log.borrow_mut().retain(Notification::new("Held", ""));
    let mut stack = NotificationStack::new();
    stack.set_viewport(Rect::new(0.0, 0.0, 800.0, 600.0));
    draw(&mut stack, &host, &Theme::dark());
    assert!(stack.rect().is_zero_area());
    assert!(
        act(&mut stack, &host, |s| s.accessibility())
            .unwrap()
            .children
            .is_empty()
    );
}

#[test]
fn accessibility_exposes_a_named_live_region_with_latest_notification() {
    use ui::a11y::{Politeness, Role};
    // Motion reads at its end: this test checks the resting frame.
    let _settled = ui::motion::testing::settled();
    let host = TestHost::default();
    let mut stack = NotificationStack::new();
    let node = act(&mut stack, &host, |s| s.accessibility()).expect("the stack is a live region");
    assert_eq!(node.role, Role::Group);
    assert_eq!(node.name.as_deref(), Some("Notifications"));
    assert_eq!(node.live, Some(Politeness::Polite));
    assert_eq!(
        node.value.as_deref(),
        Some(""),
        "an empty stack announces nothing"
    );
    host.push(Notification::new("New mail", "secret subject"));
    let node = act(&mut stack, &host, |s| s.accessibility()).unwrap();
    assert_eq!(node.value.as_deref(), Some("New mail. secret subject"));
    assert_eq!(
        node.live,
        Some(Politeness::Polite),
        "an ordinary arrival waits its turn"
    );
    host.push(Notification::new("Backup failed", "").with_severity(NotificationSeverity::Error));
    let node = act(&mut stack, &host, |s| s.accessibility()).unwrap();
    assert_eq!(node.value.as_deref(), Some("Backup failed"));
    assert_eq!(node.live, Some(Politeness::Assertive), "an error cuts in");
    // Locked: no title/body/app reaches the a11y tree.
    host.locked.set(true);
    let node = act(&mut stack, &host, |s| s.accessibility()).unwrap();
    assert_eq!(node.value.as_deref(), Some("2 notifications"));
    assert_eq!(node.live, Some(Politeness::Polite));
}

/// A notification the person keeps off the lock screen does not lend its count to it.
#[test]
fn the_locked_live_region_counts_only_what_is_allowed_there() {
    let host = TestHost::default();
    host.locked.set(true);
    let mut kept_off = Notification::new("Private", "secret");
    kept_off.show_on_lock_screen = false;
    host.push(kept_off);
    let mut stack = NotificationStack::new();
    assert!(
        act(&mut stack, &host, |s| s.accessibility()).is_none(),
        "nothing the person keeps off the lock screen is counted"
    );
    let mut allowed = Notification::new("New mail", "secret");
    allowed.show_on_lock_screen = true;
    host.push(allowed);
    let node = act(&mut stack, &host, |s| s.accessibility()).unwrap();
    assert_eq!(node.value.as_deref(), Some("1 notification"));
}

/// Set `stack` up over one banner and paint it once, so the gesture tests work on the geometry the
/// stack really records.
fn one_painted_banner(
    stack: &mut NotificationStack,
    host: &TestHost,
    text: &mut TextRenderer,
    theme: &Theme,
) -> Rect {
    stack.set_viewport(Rect::new(0.0, 0.0, 480.0, 800.0));
    paint(
        stack,
        host,
        &mut Scene::new(),
        Affine::IDENTITY,
        text,
        theme,
    );
    stack
        .hit_rects
        .first()
        .map(|(_, card, _)| *card)
        .expect("the banner painted its card")
}

/// Moving a banner sideways past the slop is a swipe, not a press: it dislodges the card without
/// opening it, and letting go past the threshold dismisses it once it has slid out.
#[test]
fn a_swipe_dismisses_the_banner_without_activating_it() {
    let _settled = ui::motion::testing::settled();
    let host = TestHost::default();
    let id = host.push(
        Notification::new("Ana", "Are we still on for lunch?")
            .with_app_name("Messages")
            .with_from("Ana", "ana@example.org"),
    );
    let mut stack = NotificationStack::new();
    let mut text = TextRenderer::new();
    let theme = Theme::dark();
    let card = one_painted_banner(&mut stack, &host, &mut text, &theme);
    let centre = card.center();
    stack.pointer = Some(centre);
    stack.begin_swipe();
    assert_eq!(
        stack.swipe.map(|drag| drag.id),
        Some(id),
        "the press starts a swipe"
    );
    assert!(stack.drag_to(Point::new(centre.x + 200.0, centre.y), ui::time::now()));
    assert!(
        !stack.targets.wants_pointer_capture(),
        "the carried press was cancelled, so it cannot also click the card"
    );
    assert!(
        stack.end_swipe(ui::time::now()),
        "letting go ends the swipe"
    );
    assert!(stack.needs_animation_frame() || !stack.slides.is_empty());

    // The next paint carries the finished slide out and asks the host to dismiss the banner;
    // nothing asked to open it.
    stack.set_feed(BannerFeed::of(&host));
    stack.draw(&mut Scene::new(), Affine::IDENTITY, &mut text, &theme);
    let asks = stack.take_asks();
    assert!(
        asks.iter()
            .any(|ask| matches!(ask, BannerAsk::Dismiss(asked) if *asked == id)),
        "the swipe dismisses {id}: {asks:?}"
    );
    assert!(
        !asks
            .iter()
            .any(|ask| matches!(ask, BannerAsk::Activate(asked) if *asked == id)),
        "a swipe never opens the card: {asks:?}"
    );
}

/// A drag that falls short springs the card back to its slot, and the press it started is still
/// cancelled (a drag is not a click).
#[test]
fn a_short_drag_springs_back_without_dismissing() {
    let _settled = ui::motion::testing::settled();
    let host = TestHost::default();
    let id = host.push(Notification::new("Build finished", "All tests passed"));
    let mut stack = NotificationStack::new();
    let mut text = TextRenderer::new();
    let theme = Theme::light();
    let card = one_painted_banner(&mut stack, &host, &mut text, &theme);
    let centre = card.center();
    stack.pointer = Some(centre);
    stack.begin_swipe();
    let start = ui::time::now();
    // 30 logical px in 120 ms: a slow nudge, well under both the width
    // and the throw speed.
    assert!(stack.drag_to(
        Point::new(centre.x + 30.0, centre.y),
        start + Duration::from_millis(120)
    ));
    assert!(stack.end_swipe(start + Duration::from_millis(140)));
    stack.set_feed(BannerFeed::of(&host));
    stack.draw(&mut Scene::new(), Affine::IDENTITY, &mut text, &theme);
    let asks = stack.take_asks();
    assert!(
        !asks
            .iter()
            .any(|ask| matches!(ask, BannerAsk::Dismiss(asked) if *asked == id)),
        "a short drag is not a dismissal: {asks:?}"
    );
}

/// A horizontal drag that starts on an action button is the button's press, not the card's swipe.
#[test]
fn a_drag_that_starts_on_a_control_is_not_a_swipe() {
    let _settled = ui::motion::testing::settled();
    let host = TestHost::default();
    host.actions.replace(vec![
        ("dismiss".into(), "Dismiss".into()),
        ("snooze".into(), "Snooze".into()),
    ]);
    host.push(Notification::new("Design review", "Starts in 5 minutes"));
    let mut stack = NotificationStack::new();
    let mut text = TextRenderer::new();
    one_painted_banner(&mut stack, &host, &mut text, &Theme::dark());
    let action = stack.action_rects[0].2.center();
    stack.pointer = Some(action);
    stack.begin_swipe();
    assert!(stack.swipe.is_none(), "a control keeps its own press");
}

/// Measurement: how long one paint takes while five banners reflow into four. Not a gate; run with
/// `--ignored --nocapture` . A debug build paints slower than a person's, so the numbers bound the
/// cost.
#[test]
#[ignore = "measurement; run with --ignored --nocapture"]
fn reflow_with_five_banners_costs_one_frame() {
    let host = TestHost::default();
    let mut ids = Vec::new();
    for i in 0..5 {
        ids.push(
            host.push(
                Notification::new(
                    format!("Message {i}"),
                    "The body runs onto a second line so the card clamps it where it must.",
                )
                .with_app_name("Messages")
                .with_app_id("so.tilo.chat"),
            ),
        );
    }
    let mut stack = NotificationStack::new();
    stack.set_viewport(Rect::new(0.0, 0.0, 480.0, 800.0));
    let mut text = TextRenderer::new();
    let theme = Theme::dark();
    let mut scene = Scene::new();
    let start = ui::time::now();
    while ui::time::now().duration_since(start) < Duration::from_millis(400) {
        paint(
            &mut stack,
            &host,
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &theme,
        );
        scene.reset();
    }
    let top = ids.last().copied().unwrap();
    let retired = host.log.borrow_mut().take_active(top);
    if let Some(retired) = retired {
        host.log.borrow_mut().retain(retired);
    }
    let mut samples = Vec::new();
    let start = ui::time::now();
    while ui::time::now().duration_since(start) < Duration::from_millis(600) {
        let t = std::time::Instant::now();
        paint(
            &mut stack,
            &host,
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &theme,
        );
        samples.push(t.elapsed());
        scene.reset();
    }
    let first = samples.first().copied().unwrap_or_default();
    let over_budget = samples.iter().filter(|s| s.as_secs_f64() > 0.0167).count();
    samples.sort();
    let percentile = |per_mille: usize| samples[(samples.len() - 1) * per_mille / 1000];
    println!(
        "reflow 5->4 banners: {} frames, first {:.0} us, p50 {:.0} us, p95 {:.0} us, \
         max {:.0} us, over a 60 Hz frame: {over_budget}",
        samples.len(),
        first.as_secs_f64() * 1e6,
        percentile(500).as_secs_f64() * 1e6,
        percentile(950).as_secs_f64() * 1e6,
        percentile(1000).as_secs_f64() * 1e6
    );
}

/// A banner whose sender takes a reply offers its field, and what the person types goes back as the
/// reply: not as opening the banner.
#[test]
fn a_banner_that_takes_a_reply_sends_what_was_typed() {
    let _settled = ui::motion::testing::settled();
    let host = TestHost::default();
    let mut notification =
        Notification::new("Ada", "Can you look at the review?").with_app_name("Messages");
    notification.actions = vec![
        ("default".into(), "Open".into()),
        (INLINE_REPLY.into(), "Reply".into()),
    ];
    let id = host.push(notification);
    let mut stack = NotificationStack::new();
    stack.set_replies(true);
    let mut text = TextRenderer::new();
    let theme = Theme::dark();
    one_painted_banner(&mut stack, &host, &mut text, &theme);
    let field = stack
        .reply_rects
        .iter()
        .find(|(banner, _)| *banner == id)
        .map(|(_, rect)| *rect)
        .expect("the card offers a reply field");
    stack.pointer = Some(field.center());
    stack.dispatch_target(&BannerTarget::Reply(id));
    assert!(stack.wants_typing(), "the field takes the keys");
    for letter in ["O", "n", " ", "i", "t"] {
        assert!(stack.handle_key(&Key::Character(letter.into()), false));
    }
    assert!(stack.handle_key(&Key::Named(NamedKey::Enter), false));
    let asks = stack.take_asks();
    assert!(
        asks.iter().any(
            |ask| matches!(ask, BannerAsk::Reply { id: asked, text } if *asked == id && text == "On it")
        ),
        "the reply goes back with its text: {asks:?}"
    );
    assert!(
        !asks
            .iter()
            .any(|ask| matches!(ask, BannerAsk::Activate(asked) if *asked == id)),
        "typing a reply does not open the banner: {asks:?}"
    );
}

/// Escape gives up on the reply without sending it, and a stack whose host cannot carry an answer
/// never offers the field.
#[test]
fn a_reply_can_be_given_up_and_is_only_offered_when_it_can_be_carried() {
    let _settled = ui::motion::testing::settled();
    let host = TestHost::default();
    let mut notification = Notification::new("Ada", "Review?").with_app_name("Messages");
    notification.actions = vec![(INLINE_REPLY.into(), "Reply".into())];
    let id = host.push(notification);
    let mut stack = NotificationStack::new();
    let mut text = TextRenderer::new();
    let theme = Theme::dark();
    one_painted_banner(&mut stack, &host, &mut text, &theme);
    assert!(
        stack.reply_rects.is_empty(),
        "a host that takes no reply gets no field"
    );
    stack.set_replies(true);
    one_painted_banner(&mut stack, &host, &mut text, &theme);
    stack.dispatch_target(&BannerTarget::Reply(id));
    stack.handle_key(&Key::Character("No".into()), false);
    assert!(stack.handle_key(&Key::Named(NamedKey::Escape), false));
    assert!(!stack.wants_typing());
    assert!(
        !stack
            .take_asks()
            .iter()
            .any(|ask| matches!(ask, BannerAsk::Reply { .. })),
        "Escape sends nothing"
    );
}

/// An agent's own notice ("Claude Code finished: 8 changes in Workspace", sent in its name) leads
/// with its face and says nothing over its title repeating that name; an app's notice about a
/// person keeps the app as its source line.
#[test]
fn a_notice_in_its_senders_own_name_has_no_source_line_repeating_it() {
    let own = Notification::new(
        "Claude Code finished: 8 changes in Workspace",
        "Working in Notes",
    )
    .with_from("Claude Code", "agent-key")
    .with_app_name("Claude Code");
    assert_eq!(NotificationStack::source_line(&own), "");
    let about = Notification::new("Ada sent a message", "Lunch?")
        .with_from("Ada", "ada-key")
        .with_app_name("Chat");
    assert_eq!(NotificationStack::source_line(&about), "Chat");
    let plain = Notification::new("Saved", "").with_app_name("Notes");
    assert_eq!(NotificationStack::source_line(&plain), "Notes");
}
