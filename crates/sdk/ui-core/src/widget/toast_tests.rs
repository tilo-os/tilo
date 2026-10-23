use super::*;
use crate::testing::dispatch;
use std::collections::HashMap;

/// A card's lifetime counts only time on screen and unheld: off screen
/// or under the pointer it keeps what it has left, and letting go of it
/// leaves a moment before it starts to leave.
#[test]
fn a_dwell_runs_only_while_shown_and_unheld() {
    let lifetime = Duration::from_secs(5);
    let t0 = crate::time::now();
    let at = |ms| t0 + Duration::from_millis(ms);
    let mut dwell = Dwell::new();
    assert!(!dwell.is_over(lifetime, at(60_000)), "unseen never ends");
    assert_eq!(dwell.first_shown(), None);
    dwell.present(at(1_000), true, false);
    assert_eq!(dwell.first_shown(), Some(at(1_000)));
    assert_eq!(dwell.left(lifetime, at(3_000)), Duration::from_secs(3));
    assert_eq!(dwell.ends_at(lifetime), Some(at(6_000)));
    dwell.present(at(3_000), true, true);
    assert!(!dwell.is_running());
    assert_eq!(dwell.ends_at(lifetime), None);
    assert_eq!(
        dwell.left(lifetime, at(30_000)),
        Duration::from_secs(3),
        "a held card keeps its time"
    );
    dwell.present(at(30_000), false, false);
    assert_eq!(dwell.left(lifetime, at(40_000)), Duration::from_secs(3));
    dwell.present(at(40_000), true, false);
    assert_eq!(dwell.first_shown(), Some(at(1_000)));
    assert!(dwell.is_over(lifetime, at(43_000)));
    dwell.grant(lifetime, at(43_000), Duration::from_secs(1));
    assert_eq!(dwell.left(lifetime, at(43_000)), Duration::from_secs(1));
    assert!(dwell.is_over(lifetime, at(44_000)));
    dwell.spend(Duration::from_secs(9));
    assert!(dwell.is_over(lifetime, at(43_000)));
}

/// A stack that shows what it painted asks for no frame, so a pointer
/// moving over a window with a notice up repaints nothing; a new or
/// dismissed notice asks for one.
#[test]
fn a_painted_stack_needs_a_frame_only_when_its_notices_change() {
    let mut stack = ToastStack::new();
    lay_out(&mut stack);
    assert!(stack.needs_redraw(), "a stack never drawn needs a frame");
    stack.mark_drawn();
    assert!(
        !stack.needs_redraw(),
        "an empty stack it painted needs none"
    );
    stack.push("Saved", ToastKind::Success);
    assert!(stack.needs_redraw(), "a new notice needs a frame");
    stack.mark_drawn();
    assert!(!stack.needs_redraw(), "the notice it painted needs none");
    stack.dismiss(0);
    assert!(stack.needs_redraw(), "a dismissed notice needs a frame");
}

/// A notice's one button ("Undo") is reported to its owner once and
/// takes the notice down; a press elsewhere on the card does not report
/// it.
#[test]
fn a_notice_without_an_action_leaves_on_its_own_and_wakes_its_widget() {
    use crate::time::testing::with_monotonic_time;
    use crate::window_toolbar::{ToolbarNotice, notice as bar_notice, notify};
    notify(None);
    let start = crate::time::now();
    let after = start + NOTICE_LIFETIME + Duration::from_millis(1);

    let mut notice = Notice::default();
    with_monotonic_time(start, || {
        notice.show("Couldn’t open x.epub", ToastKind::Error);
    });
    assert_eq!(
        notice.deadline(),
        Some(start + NOTICE_LIFETIME),
        "the widget asks to be woken when the notice is due to leave"
    );
    with_monotonic_time(start, || {
        assert!(!notice.expire());
        notice.publish();
    });
    assert_eq!(
        bar_notice(),
        Some(ToolbarNotice::new("Couldn’t open x.epub", ToastKind::Error)),
        "the window's bar shows it"
    );
    with_monotonic_time(after, || {
        assert!(!notice.is_shown(), "a notice past its time reads as gone");
        notice.publish();
        assert_eq!(bar_notice(), None, "and leaves the bar");
        assert!(notice.expire(), "waking at its deadline takes it down");
    });
    assert_eq!(notice.deadline(), None, "nothing left to wake for");

    // Showing a message again gives it its full time again.
    let mut status = Transient::new();
    with_monotonic_time(start, || status.show(String::from("Saved")));
    with_monotonic_time(after, || {
        assert_eq!(status.as_deref(), None);
        status.show(String::from("Saved"));
        assert_eq!(status.as_deref(), Some("Saved"));
    });
}

/// Notices replace each other in the order they came, and none stacks: an
/// older one leaving takes nothing newer down, and one gone with its owner
/// leaves the bar.
#[test]
fn notices_replace_each_other_in_order_and_leave_with_their_owner() {
    use crate::window_toolbar::{ToolbarNotice, notice as bar_notice, notify};
    notify(None);
    let mut saved = Notice::default();
    let mut failed = Notice::default();
    saved.show("Saved", ToastKind::Success);
    saved.publish();
    failed.show("Couldn’t sync", ToastKind::Error);
    failed.publish();
    saved.publish();
    assert_eq!(
        bar_notice(),
        Some(ToolbarNotice::new("Couldn’t sync", ToastKind::Error)),
        "the newest replaces the older, which does not take the bar back"
    );
    assert!(saved.dismiss());
    saved.publish();
    assert_eq!(
        bar_notice().map(|notice| notice.text),
        Some("Couldn’t sync".to_owned()),
        "the older leaving takes nothing newer down"
    );
    saved.show("Saved again", ToastKind::Success);
    saved.publish();
    assert_eq!(
        bar_notice().map(|notice| notice.text),
        Some("Saved again".to_owned()),
        "news replaces what is up"
    );
    drop(saved);
    assert_eq!(bar_notice(), None, "a notice leaves with its owner");
    drop(failed);
}

/// Progress stays while its app reports the work running, however long
/// that takes, and wakes the widget to be renewed; it leaves once the
/// work stops, or once the app stops reporting it at all.
#[test]
fn a_progress_notice_stays_while_its_work_runs() {
    use crate::time::testing::with_monotonic_time;
    let start = crate::time::now();
    let at = |seconds: u64| start + Duration::from_secs(seconds);
    let mut notice = Notice::default();
    with_monotonic_time(start, || notice.show_progress("Opening x.epub"));
    assert_eq!(notice.deadline(), Some(start + NOTICE_LIFETIME));
    for second in [3, 6, 9] {
        with_monotonic_time(at(second), || {
            assert!(notice.is_shown(), "still opening at {second}s");
            assert!(!notice.keep_while(true));
            assert!(!notice.expire());
        });
    }
    assert_eq!(
        notice.deadline(),
        Some(at(9) + NOTICE_LIFETIME),
        "renewed, it wakes the widget to be renewed again"
    );
    with_monotonic_time(at(10), || {
        assert!(notice.keep_while(false), "the work ended");
        assert!(!notice.is_shown());
    });
    assert_eq!(notice.deadline(), None);

    // Progress nobody reports any more does not stay.
    let mut stale = Transient::showing_progress(());
    let started = stale.since().expect("up");
    with_monotonic_time(started + NOTICE_LIFETIME + Duration::from_millis(1), || {
        assert!(!stale.is_shown());
        assert!(stale.expire());
    });

    // Reporting work running keeps plain information to its own time.
    let mut plain = Transient::new();
    with_monotonic_time(start, || plain.show("Saved"));
    with_monotonic_time(at(3), || assert!(!plain.keep_while(true)));
    with_monotonic_time(at(5), || assert_eq!(plain.get(), None));
}

#[test]
fn a_notice_button_is_reported_once_and_takes_the_notice_down() {
    let mut stack = ToastStack::new();
    stack.push_with_action("Archived", ToastKind::Success, "Undo");
    lay_out(&mut stack);
    let mut scene = Scene::new();
    Widget::draw(
        &mut stack,
        &mut scene,
        Affine::IDENTITY,
        &mut TextRenderer::new(),
        &Theme::dark(),
        &PresenceCtx::empty(),
    );
    let button = stack
        .action_rects
        .first()
        .copied()
        .flatten()
        .expect("the notice shows its button");
    let at = button.center();
    dispatch(
        &mut stack,
        &WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: (at.x, at.y).into(),
        },
    );
    assert!(
        stack.needs_redraw(),
        "the button under the pointer lights up"
    );
    dispatch(&mut stack, &crate::synthetic_input::mouse_press());
    assert_eq!(stack.take_activated().as_deref(), Some("Archived"));
    assert!(stack.is_empty(), "the notice is done with");
    assert_eq!(stack.take_activated(), None, "and reported once");
}

fn lay_out(stack: &mut ToastStack) {
    let mut tree = TaffyTree::<()>::new();
    let mut nm: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let theme = Theme::dark();
    let node = stack.build_layout(&mut tree, &theme, &mut nm);
    tree.compute_layout(
        node,
        Size {
            width: taffy::AvailableSpace::Definite(f32_of_f64(TOAST_WIDTH)),
            height: taffy::AvailableSpace::Definite(1000.0),
        },
    )
    .unwrap();
    stack.apply_layout(&tree, node, Point::ZERO);
}

/// A card's one action sits beside its text while the text keeps room
/// to read; large text in a narrow column puts it under the text.
#[test]
fn a_lone_action_moves_under_text_it_would_squeeze() {
    let mut tr = TextRenderer::new();
    let mut card = ToastCard::new("Launch checklist wasn't saved", ToastKind::Warning)
        .caption("Notes")
        .body("Your changes are still in Notes.")
        .closable(true);
    card.primary_action = "Try again";
    let paint = |tr: &mut TextRenderer, theme: &Theme, width: f64| {
        let rect = Rect::new(0.0, 0.0, width, card_height(tr, theme, &card, width));
        let hit = paint_card(&mut Scene::new(), Affine::IDENTITY, tr, theme, rect, &card);
        (rect, hit.primary_action_rect)
    };

    let (rect, beside) = paint(&mut tr, &Theme::dark(), 440.0);
    assert!(
        beside.y0 < rect.center().y && beside.y1 > rect.center().y,
        "{beside:?} sits beside the text in {rect:?}"
    );

    let large = Theme::light().with_text_scale(2.0);
    let (rect, under) = paint(&mut tr, &large, 320.0);
    assert!(
        under.y0 > rect.center().y && under.y1 <= rect.y1,
        "{under:?} ends the card {rect:?}"
    );
    assert!(rect.height() > minimum_card_height(&mut tr, &large, &card));
    // Every letter of the caption, title, body and button is painted:
    // an elided line would drop most of them.
    let mut scene = Scene::new();
    paint_card(&mut scene, Affine::IDENTITY, &mut tr, &large, rect, &card);
    let glyphs: usize = scene
        .encoding()
        .resources
        .glyph_runs
        .iter()
        .map(|run| run.glyphs.len())
        .sum();
    let letters = [card.caption, card.title, card.body, card.primary_action]
        .concat()
        .chars()
        .filter(|c| !c.is_whitespace())
        .count();
    assert!(
        glyphs >= letters,
        "{glyphs} glyphs for {letters} letters: a line was elided"
    );
}

/// A card asking the person to choose ("Not now" / "Keep with passkey")
/// grows to say its whole question: a body of three lines at the
/// notification width is not elided to one.
#[test]
fn a_choice_card_wraps_its_whole_body() {
    let mut tr = TextRenderer::new();
    let theme = Theme::dark();
    let card = ToastCard::new("Keep your name and work?", ToastKind::Info)
        .caption("Identity")
        .body(
            "A passkey on this device brings you back as yourself, on any browser. \
             Any time later: Ctrl+K, \u{201c}Keep your name and work\u{201d}.",
        )
        .closable(false)
        .actions(
            "Not now",
            crate::icons::Icon::X,
            false,
            "Keep with passkey",
            crate::icons::Icon::Check,
            false,
        );
    let width = 360.0;
    let rect = Rect::new(0.0, 0.0, width, card_height(&mut tr, &theme, &card, width));
    assert!(rect.height() > minimum_card_height(&mut tr, &theme, &card));
    let mut scene = Scene::new();
    let hit = paint_card(&mut scene, Affine::IDENTITY, &mut tr, &theme, rect, &card);
    assert!(hit.primary_action_rect.y1 <= rect.y1);
    let glyphs: usize = scene
        .encoding()
        .resources
        .glyph_runs
        .iter()
        .map(|run| run.glyphs.len())
        .sum();
    let letters = [
        card.caption,
        card.title,
        card.body,
        card.secondary_action,
        card.primary_action,
    ]
    .concat()
    .chars()
    .filter(|c| !c.is_whitespace())
    .count();
    assert!(
        glyphs >= letters,
        "{glyphs} glyphs for {letters} letters: the question was elided"
    );
}

#[test]
fn toast_card_paints_text_close_and_actions_inside_the_card() {
    // Case `three_line_text_block_stays_inside_the_card`.
    // Caption, title and body stack on their measured line boxes with an
    // `s1` gap, so in the 64 px card a desktop notification uses every
    // painted glyph line lands inside the card.
    {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        let rect = Rect::new(0.0, 0.0, 360.0, 64.0);
        paint_card(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            rect,
            &ToastCard::new("Build finished", ToastKind::Success)
                .caption("Terminal")
                .body("3 warnings, 0 errors")
                .closable(true),
        );
        let runs = &scene.encoding().resources.glyph_runs;
        assert!(runs.len() >= 3, "caption, title and body all paint");
        let mut tops: Vec<f64> = Vec::new();
        for run in runs {
            let top = f64::from(run.transform.translation[1]);
            let (_, h) = tr.measure("Ag", run.font_size, None);
            assert!(
                top >= rect.y0 - 0.01 && top + f64::from(h) <= rect.y1 + 0.01,
                "a glyph line at y={top} (h={h}) leaves the {rect:?} card",
            );
            tops.push(top);
        }
        tops.sort_by(f64::total_cmp);
        tops.dedup_by(|a, b| (*a - *b).abs() < 0.5);
        assert_eq!(tops.len(), 3, "three distinct baselines: {tops:?}");
        for pair in tops.windows(2) {
            assert!(
                pair[1] - pair[0] >= f64::from(theme.tokens.typography.size(TextRole::Caption)),
                "lines do not overlap: {tops:?}"
            );
        }
    }

    {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        let rect = Rect::new(100.0, 40.0, 460.0, 104.0);
        let hit = paint_card(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            rect,
            &ToastCard::new("New mail", ToastKind::Info)
                .body("secret subject")
                .caption("Mailer")
                .closable(true)
                .actionable(true),
        );
        assert!(
            hit.close_rect.area() > 0.0,
            "closable card returns a close rect"
        );
        assert!(
            hit.close_rect.x1 <= rect.x1 && hit.close_rect.x1 > rect.center().x,
            "close rect hugs the card's right edge",
        );
        assert!(
            hit.close_rect.y0 >= rect.y0
                && hit.close_rect.y0 - rect.y0 <= f64::from(theme.tokens.spacing.s2) + 0.5,
            "close rect sits at the top of the card",
        );
        assert_eq!(
            hit.close_rect.height(),
            f64::from(theme.tokens.metrics.bar_control_height),
            "the close control is a Small icon button"
        );
    }

    {
        let mut scene = Scene::new();
        let mut tr = TextRenderer::new();
        let theme = Theme::dark();
        let card_rect = Rect::new(20.0, 30.0, 374.0, 142.0);
        let hit = paint_card(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            card_rect,
            &ToastCard::new("No visible video", ToastKind::Warning)
                .caption("Picture in Picture")
                .actions(
                    "Float page",
                    crate::icons::Icon::PictureInPicture2,
                    false,
                    "Select area",
                    crate::icons::Icon::MousePointer2,
                    false,
                ),
        );
        assert!(hit.secondary_action_rect.area() > 0.0);
        assert!(hit.primary_action_rect.area() > 0.0);
        assert!(hit.secondary_action_rect.x1 < hit.primary_action_rect.x0);
        assert!(card_rect.contains(hit.secondary_action_rect.center()));
        assert!(card_rect.contains(hit.primary_action_rect.center()));
    }

    {
        use vello_encoding::DrawTag;
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        let rect = Rect::new(0.0, 0.0, 360.0, 64.0);
        paint_card(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            rect,
            &ToastCard::new("Build finished", ToastKind::Success),
        );
        let enc = scene.encoding();
        let color_fills = enc
            .draw_tags
            .iter()
            .filter(|t| **t == DrawTag::COLOR)
            .count();
        assert!(
            color_fills >= 2,
            "surface fill and the severity icon; got {color_fills}"
        );
        assert!(
            !enc.resources.glyph_runs.is_empty(),
            "the title must push a glyph run into the scene",
        );
    }

    {
        assert_eq!(
            single_line_text("  App\nfailed:\u{00a0}profile\tmissing  "),
            "App failed: profile missing"
        );

        let rect = Rect::new(0.0, 0.0, 360.0, 64.0);
        // This is the shape emitted for a launcher failure: caption, title,
        // and long body all share one fixed-height card. Exercise both the
        // normal and accessibility-large text scales so a localized newline
        // cannot introduce an extra baseline or push the close affordance
        // outside the bounded card.
        for theme in [Theme::dark(), Theme::dark().with_text_scale(1.5)] {
            let mut tr = TextRenderer::new();
            let mut scene = Scene::new();
            let hit = paint_card(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            rect,
            &ToastCard::new("Couldn't open\nNotes", ToastKind::Error)
                .caption("Applications\nlauncher")
                .body("The app stopped\nbefore opening. This deliberately long localized recovery message must elide safely.")
                .closable(true),
        );
            assert_eq!(hit.card_rect, rect);
            assert!(hit.close_rect.area() > 0.0);
            assert!(
                !scene.encoding().resources.glyph_runs.is_empty(),
                "launcher failure text must render at text scale {}",
                theme.text_scale
            );
        }

        // The painter's text inputs are normalized before elision; this
        // assertion protects the helper contract independently of glyph
        // backend details.
        assert!(!single_line_text("Couldn't open\nNotes").contains('\n'));
        assert!(!single_line_text("The app stopped\nbefore opening").contains('\n'));
    }
}

#[test]
fn toast_interaction_dismisses_pauses_and_announces() {
    {
        let mut s = ToastStack::new();
        s.push("first", ToastKind::Info);
        s.push("second", ToastKind::Error);
        lay_out(&mut s);
        // Card 1 ("second") spans y ∈ [56, 104); centre ≈ (160, 80).
        dispatch(
            &mut s,
            &WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: (160.0, 80.0).into(),
            },
        );
        dispatch(&mut s, &crate::synthetic_input::mouse_press());
        assert_eq!(s.len(), 1);
        assert_eq!(s.iter().next().unwrap().text, "first");
    }

    {
        use crate::a11y::{Politeness, Role};
        // Empty stack: a named, assertive live region with no value yet.
        let s = ToastStack::new();
        let node = Widget::accessibility(&s).expect("toast stack is not presentational");
        assert_eq!(node.role, Role::Group);
        assert_eq!(node.name.as_deref(), Some("Notifications"));
        assert_eq!(node.live, Some(Politeness::Assertive));
        assert_eq!(node.value, None, "an empty stack has no current toast");

        // With a live toast the stack surfaces the most recent text as value,
        // so the host's live-region scan announces it.
        let mut s = ToastStack::new();
        s.push("hello", ToastKind::Info);
        s.push("world", ToastKind::Error);
        let node = Widget::accessibility(&s).unwrap();
        assert_eq!(
            node.value.as_deref(),
            Some("world"),
            "value is the newest toast"
        );
        assert_eq!(node.live, Some(Politeness::Assertive));
    }

    {
        let mut s = ToastStack::new().with_default_ttl(Duration::from_millis(100));
        s.push("stay", ToastKind::Info);
        lay_out(&mut s);
        // Hover over the only card.
        dispatch(
            &mut s,
            &WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: (160.0, 24.0).into(),
            },
        );
        assert!(s.is_hovered());
        // tick longer than the TTL — but hovered, so it must NOT expire.
        s.tick(Duration::from_millis(500));
        assert_eq!(s.len(), 1, "hovered toast does not expire");
        // Move away → expiry resumes.
        dispatch(
            &mut s,
            &WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: (900.0, 900.0).into(),
            },
        );
        assert!(!s.is_hovered());
        s.tick(Duration::from_millis(500));
        assert_eq!(s.len(), 0, "expires once un-hovered");
    }
}

#[test]
fn notices_with_a_button_go_once_what_they_act_on_moves_on() {
    let mut s = ToastStack::new();
    s.push_with_action("Deleted Groceries", ToastKind::Info, "Undo");
    s.push("Imported", ToastKind::Success);
    s.dismiss_actionable();
    assert_eq!(s.len(), 1, "only the notice without a button stays");
    assert!(s.iter().all(|toast| toast.action.is_none()));
}
