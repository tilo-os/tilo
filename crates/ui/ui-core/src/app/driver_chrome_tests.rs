//! A driven app's window chrome as a person finds it: the titles across its columns' header
//! row, the sidebar button that flips its sidebar, and whether the app's own header row stands
//! under the window's bar or is the window's top.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test fixture: a failure is the assertion"
)]

use crate::headless::Driver;
use tilo_ui_base::theme::Theme;
use crate::widget::{NavShell, SidebarPlacement};

/// A mail window: mailboxes in the sidebar, a mailbox's messages beside them and the message
/// read, each column titled in its header row.
fn mail_shell() -> NavShell {
    NavShell::new()
        .sidebar(Box::new(crate::widget::Column::new()))
        .overview(Box::new(crate::widget::Column::new()))
        .content(Box::new(crate::widget::Column::new()))
        .sidebar_header("Mailboxes", Vec::new())
        .overview_header("Inbox", Vec::new())
        .content_header("Design review", Vec::new())
}

/// A mail app whose header is the window bar's parts, as an app wires its shell to the bar:
/// each sync it runs what the person pressed there and declares the shell's parts.
struct MailApp {
    shell: NavShell,
}

impl tilo_ui_base::widget::Decorator for MailApp {
    type Inner = NavShell;

    fn inner(&self) -> &Self::Inner {
        &self.shell
    }

    fn inner_mut(&mut self) -> &mut Self::Inner {
        &mut self.shell
    }

    fn sync(&mut self, state: &tilo_ui_base::widget::AppModel, path: &tilo_ui_base::widget::ModelPath) {
        for event in crate::window_toolbar::take_events() {
            self.shell.window_toolbar_event(&event);
        }
        crate::window_toolbar::declare(Some(self.shell.window_toolbar()));
        tilo_ui_base::widget::Widget::sync(&mut self.shell, state, path);
    }
}

fn driven(shell: NavShell, width: u32) -> Driver {
    driven_root(shell, width)
}

fn driven_root<W: tilo_ui_base::widget::Widget + 'static>(root: W, width: u32) -> Driver {
    let mut driver = Driver::for_root(
        crate::State::new(),
        root,
        (width, 800),
        Theme::dark().with_reduce_motion(true),
    );
    driver.relayout();
    driver
}

#[test]
fn a_wide_mail_window_reads_each_columns_title_left_to_right_across_one_header_row() {
    let _landed = tilo_ui_base::motion::testing::settled();
    let driver = driven(mail_shell(), 1280);

    let names = driver.header_row_names();
    let at = |title: &str| {
        names
            .iter()
            .position(|name| name == title)
            .unwrap_or_else(|| panic!("the header row reads {title}: {names:?}"))
    };
    assert!(
        at("Mailboxes") < at("Inbox") && at("Inbox") < at("Design review"),
        "the titles read in column order: {names:?}"
    );

    let frames = driver.column_frames();
    let keys: Vec<&str> = frames.iter().map(|frame| frame.key).collect();
    assert_eq!(
        frames.len(),
        3,
        "three columns stand side by side: {keys:?}"
    );
    for pair in frames.windows(2) {
        assert!(
            pair[0].column.x1 <= pair[1].column.x0 + 0.5,
            "the columns stand left to right without overlapping: {frames:?}"
        );
    }
    let row = driver.window_header_row_height();
    assert!(row > 0.0, "the window draws a header row of its own");
    for frame in &frames {
        assert!(
            frame.header.y0.abs() < 0.5 && (frame.header.height() - row).abs() < 0.5,
            "each column's header stands at the window's top, as tall as the row: {frame:?}"
        );
        assert!(
            frame.column.contains(frame.header.center()),
            "a column's header is the top of that column: {frame:?}"
        );
    }
    assert!(
        !driver.row_holds_bar(),
        "a window with no bar offered to its row keeps its row the app's"
    );
}

#[test]
fn a_window_with_no_header_titles_reads_no_header_row() {
    let _landed = tilo_ui_base::motion::testing::settled();
    let driver = driven(
        NavShell::new()
            .content(Box::new(crate::widget::Column::new()))
            .with_window_toolbar(),
        900,
    );
    assert_eq!(driver.window_header_row_height(), 0.0);
    assert!(
        driver.header_row_names().is_empty(),
        "nothing stands in a row that is not drawn: {:?}",
        driver.header_row_names()
    );
    assert!(
        driver.window_sidebar_shown().is_err(),
        "a window with no sidebar has no sidebar to report"
    );
}

#[test]
fn the_header_rows_sidebar_button_hides_and_shows_a_docked_sidebar_and_all_three_agree() {
    let _landed = tilo_ui_base::motion::testing::settled();
    let mut driver = driven(mail_shell(), 1280);
    assert_eq!(
        driver.root::<NavShell>().geometry().sidebar,
        SidebarPlacement::Docked
    );
    let before = driver.window_sidebar_shown().unwrap();
    assert_eq!(
        before.agreed(),
        Some(true),
        "the sidebar opens shown: {before:?}"
    );

    let mut expected = true;
    for click in 1..=3 {
        driver.click_window_sidebar_button().unwrap();
        driver.relayout();
        expected = !expected;
        let shown = driver.window_sidebar_shown().unwrap();
        assert_eq!(
            shown.agreed(),
            Some(expected),
            "click {click}: the button, the shell and the frame agree: {shown:?}"
        );
    }
}

#[test]
fn the_window_bars_sidebar_button_flips_the_sidebar_of_an_app_whose_header_is_its_parts() {
    let _landed = tilo_ui_base::motion::testing::settled();
    let mut driver = driven_root(
        MailApp {
            shell: mail_shell().with_window_toolbar(),
        },
        1280,
    );
    driver.present_in_window_bar();
    driver.relayout();
    assert!(driver.presents_window_bar());
    assert_eq!(
        driver.window_header_row_height(),
        0.0,
        "the app's header is the bar's parts: no second row stands under the bar"
    );
    assert!(
        !driver.row_holds_bar(),
        "the bar stands above the app, not in a row of the app's"
    );
    let bar_bottom = crate::widget::tabs::TOOLBAR_ROW_HEIGHT;
    let before = driver.window_sidebar_shown().unwrap();
    assert_eq!(before.agreed(), Some(true), "{before:?}");

    let mut expected = true;
    for click in 1..=3 {
        driver.click_window_sidebar_button().unwrap();
        driver.relayout();
        expected = !expected;
        let shown = driver.window_sidebar_shown().unwrap();
        assert_eq!(
            shown.agreed(),
            Some(expected),
            "click {click} in the bar flips the sidebar, and the bar's button says so: {shown:?}"
        );
        let sidebar = driver.root::<MailApp>().shell.geometry().sidebar_column;
        if expected {
            assert!(
                sidebar.y0 >= bar_bottom - 0.5,
                "the sidebar stands under the bar: {sidebar:?}"
            );
        }
    }
}

/// A shell's window frame hosting the mail shell, as a window of the session hosts a tile.
fn framed_mail(width: u32) -> Driver {
    let mut driver = Driver::in_window_frame(
        crate::State::new(),
        Box::new(mail_shell()),
        (width, 800),
        Theme::dark().with_reduce_motion(true),
    );
    // Asked for the driver's own bar, a framed window still wears only the frame's.
    driver.present_in_window_bar();
    driver.relayout();
    driver
}

#[test]
fn a_window_frame_hands_its_bar_to_the_mail_header_row_and_wears_no_second_bar() {
    let _landed = tilo_ui_base::motion::testing::settled();
    let driver = framed_mail(1280);
    assert!(
        !driver.presents_window_bar(),
        "the frame's bar is the window's: no row of parts stands over it"
    );
    assert!(
        driver.row_holds_bar(),
        "the mail header row is the window's bar"
    );
    assert_eq!(
        driver.window_header_row_height(),
        0.0,
        "the row holding the bar is no second row"
    );
    let names = driver.header_row_names();
    for title in ["Mailboxes", "Inbox", "Design review"] {
        assert!(
            names.iter().any(|name| name == title),
            "{title} stands in the row that is the window's bar: {names:?}"
        );
    }
    let frames = driver.column_frames();
    assert_eq!(frames.len(), 3, "{frames:?}");
    assert!(
        frames.iter().all(|frame| frame.header.y0.abs() < 0.5),
        "every column's header stands at the window's top: {frames:?}"
    );
}

#[test]
fn the_sidebar_button_in_a_framed_windows_header_row_flips_the_sidebar() {
    let _landed = tilo_ui_base::motion::testing::settled();
    let mut driver = framed_mail(1280);
    assert_eq!(driver.window_sidebar_shown().unwrap().agreed(), Some(true));
    driver.click_window_sidebar_button().unwrap();
    driver.relayout();
    let shown = driver.window_sidebar_shown().unwrap();
    assert_eq!(shown.agreed(), Some(false), "{shown:?}");
}
