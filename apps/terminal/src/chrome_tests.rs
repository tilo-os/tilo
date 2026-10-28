//! The terminal's own chrome around the grid: the ended session's banner
//! with its restart, and the history bar over the right padding.

use super::{build_test_widget, send_cursor_move, send_left_press, shell_command};
use crate::TerminalWidget;
use automerge::{AutoCommit, ROOT};
use ui::widget::Widget as _;
use vello::kurbo::Rect;

#[test]
fn ended_session_banner_reserves_its_own_band_above_the_grid() {
    use ui::widget::Widget;
    use ui::{Theme, presence::PresenceCtx, text_render::TextRenderer};
    use vello::{Scene, kurbo::Affine};

    let mut widget = TerminalWidget::spawn_command(shell_command("true", "exit /b 0"))
        .expect("spawn short-lived pty command");
    let doc = AutoCommit::new();
    tilo_test_env::wait_until("the short-lived command to end", || {
        widget.sync(&doc, &ROOT);
        widget.is_disconnected()
    });

    let viewport = Rect::new(0.0, 0.0, 390.0, 844.0);
    let mut text = TextRenderer::new();
    for scale in [1.0, 2.0] {
        widget.force_geometry(viewport, 10.0, 20.0);
        let theme = Theme::dark().with_text_scale(scale);
        let mut scene = Scene::new();
        widget.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &theme,
            &PresenceCtx::empty(),
        );
        let banner = widget.banner_test_rect();
        assert!(banner.width() > 0.0, "an ended session shows its banner");
        assert!(banner.x0 >= viewport.x0 && banner.x1 <= viewport.x1);
        assert!(
            banner.y1 <= widget.grid_rect().y0,
            "the grid starts under the banner at text scale {scale}"
        );
    }
}

/// A terminal whose shell ended offers to start it again in place: the
/// ended session's banner carries "Restart shell", which is the `restart`
/// action, and a press on it puts a new process behind the same terminal on
/// a clean screen.
#[test]
fn an_ended_shell_restarts_in_place() {
    use crate::resource::TerminalEffect;
    use ui::widget::Widget;
    use ui::{Theme, presence::PresenceCtx, text_render::TextRenderer};
    use vello::{Scene, kurbo::Affine};

    let mut widget = TerminalWidget::spawn_command(shell_command("echo before", "echo before"))
        .expect("spawn short-lived pty command");
    let mut doc = AutoCommit::new();
    let settle = |widget: &mut TerminalWidget,
                  doc: &AutoCommit,
                  what: &str,
                  done: &dyn Fn(&TerminalWidget) -> bool| {
        tilo_test_env::wait_until(what, || {
            widget.sync(doc, &ROOT);
            done(widget)
        });
    };
    settle(&mut widget, &doc, "the command ends", &|w| {
        w.is_disconnected() && w.grid_text().contains("before")
    });
    widget.force_geometry(Rect::new(0.0, 0.0, 600.0, 400.0), 10.0, 20.0);
    widget.draw(
        &mut Scene::new(),
        Affine::IDENTITY,
        &mut TextRenderer::new(),
        &Theme::dark(),
        &PresenceCtx::empty(),
    );
    let tree = Widget::accessibility(&widget).expect("terminal semantics");
    let restart = tree
        .children
        .iter()
        .find(|node| node.name.as_deref() == Some("Restart shell"))
        .expect("the ended session offers to restart its shell");
    assert_eq!(restart.role, ui::a11y::Role::Button);
    let button = restart.bounds.expect("the button is on screen").to_rect();
    let restart_unavailable = |widget: &TerminalWidget| {
        widget
            .effect_statuses()
            .iter()
            .any(|status| format!("{status:?}").contains("\"restart\""))
    };
    assert!(
        button.y1 <= widget.grid_rect().y0,
        "Restart shell sits in the banner above the grid"
    );
    assert!(
        !restart_unavailable(&widget),
        "the restart action is available"
    );

    send_cursor_move(&mut widget, &mut doc, button.center().x, button.center().y);
    send_left_press(&mut widget, &mut doc);
    assert!(!widget.is_disconnected(), "a new process runs");
    assert!(
        !widget.grid_text().contains("before"),
        "the new process starts on a clean screen"
    );
    assert!(
        widget.perform(TerminalEffect::Restart).is_err(),
        "a running shell is not restarted"
    );
    assert!(restart_unavailable(&widget));
    settle(&mut widget, &doc, "the new process prints", &|w| {
        w.grid_text().contains("before")
    });
}

/// The scroll bar lies over the body padding, as every overlay bar does:
/// the grid keeps the columns a gutter beside it would take.
#[test]
fn the_scroll_bar_takes_no_columns_from_the_grid() {
    let (mut w, _) = build_test_widget();
    w.place_at(Rect::new(0.0, 0.0, 1000.0, 640.0));
    let pad = f64::from(ui::Theme::default().tokens.spacing.s3);
    assert!(
        (w.grid_rect().x1 - (1000.0 - pad)).abs() < 1e-9,
        "the grid reaches the body padding on the right: {:?}",
        w.grid_rect()
    );
}

/// The history bar is an overlay: output alone does not show it, scrolling
/// back does and it fades after, and neither resizes the session's screen.
/// Under reduced motion it stays shown.
#[test]
fn the_history_bar_shows_on_scroll_and_fades_at_rest() {
    use ui::{Theme, presence::PresenceCtx, text_render::TextRenderer};
    use vello::{Scene, kurbo::Affine};

    let draw = |w: &mut TerminalWidget, theme: &Theme| {
        w.draw(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            theme,
            &PresenceCtx::empty(),
        );
    };
    let theme = Theme::dark();
    let (mut w, _) = build_test_widget();
    w.place_at(Rect::new(0.0, 0.0, 600.0, 400.0));
    let mut payload = Vec::new();
    for i in 0..80 {
        payload.extend_from_slice(format!("LINE{i:03}\r\n").as_bytes());
    }
    w.feed(&payload);
    draw(&mut w, &theme);
    let screen = w.grid_size();
    w.feed(b"one more\r\n");
    draw(&mut w, &theme);
    assert!(w.scrollbar_opacity() <= 0.003, "output alone shows no bar");

    w.scroll_up(5);
    draw(&mut w, &theme);
    assert!(w.scrollbar_opacity() > 0.99, "scrolling back shows the bar");
    assert!(
        ui::widget::Widget::next_wake_deadline(&w).is_some(),
        "the terminal wakes when the bar starts to fade"
    );
    assert_eq!(w.grid_size(), screen, "the bar resizes nothing");

    let mut calm = Theme::dark();
    calm.reduce_motion = true;
    let (mut w, _) = build_test_widget();
    w.place_at(Rect::new(0.0, 0.0, 600.0, 400.0));
    w.feed(&payload);
    draw(&mut w, &calm);
    assert!(
        w.scrollbar_opacity() > 0.99,
        "under reduced motion the bar does not fade"
    );
}
