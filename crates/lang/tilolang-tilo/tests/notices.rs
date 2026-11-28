//! The notification centre reading a notice whole. A notice that names nowhere to lead opens as
//! a page that says who sent it, its title and everything it says, and the way back names the
//! list it returns to. A sender that asked for no preview says so in place of its words, and a
//! page longer than a phone's room scrolls by the keyboard.
//!
//! The scenarios stage the centre's reading as the session publishes it; what the page's controls
//! call is recorded as the app's own actions, so a press is held to the verb it ran.

#![cfg(target_os = "linux")]
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "test assertions"
)]

use tilo_ui_base::Theme;
use tilolang_runtime::UiNode;

use crate::shell_staged::StagedWindow;

/// The centre staged as its scenario in `theme` and `viewport`, with no view failed to draw.
fn staged(scenario: &str, theme: Theme, viewport: (u32, u32)) -> StagedWindow {
    let mut theme = theme;
    theme.reduce_motion = true;
    let shell = StagedWindow::staged(scenario, viewport, theme);
    let failed = shell
        .instance
        .host()
        .expect("the window runs a host")
        .runtime()
        .take_view_failures();
    assert!(failed.is_empty(), "a view failed: {failed:?}");
    let tree: UiNode = shell.instance.root_tree().expect("the window's tree");
    if let Some(reason) = tree
        .of_kind("ui.Text")
        .into_iter()
        .find(|node| node.key.contains("/reason"))
    {
        panic!("a view failed: {:?}", reason.props);
    }
    shell
}

/// A named key pressed into the window, as a person does.
fn press_key(shell: &mut StagedWindow, key: tilo_ui_base::winit::keyboard::NamedKey) {
    let event = tilo_ui_base::synthetic_input::key_press(
        tilo_ui_base::winit::keyboard::Key::Named(key),
        None,
    );
    shell.harness.send(&event);
    shell.exchange();
}

/// Whether the way back to the list stands on the page, named for what it returns to.
fn way_back(shell: &StagedWindow) -> bool {
    shell
        .accessible()
        .iter()
        .any(|node| node.name.as_deref() == Some("Back to notifications"))
}

/// A notice that names nowhere to lead is read whole, the way back returns to the list, and
/// Escape is that same way back from the keyboard.
#[test]
fn a_notice_nothing_can_open_is_read_in_full_and_the_way_back_returns_to_the_list() {
    let mut shell = staged(
        "the notification centre, reading a notice",
        Theme::light(),
        (1280, 900),
    );
    for words in [
        "Disk almost full",
        "Settings",
        "A backup of 40 GB is still writing to Home",
    ] {
        assert!(
            shell.shows(words),
            "the page says `{words}`: {:?}",
            shell.said()
        );
    }
    assert!(
        !shell.shows("Backup finished"),
        "the list stands behind the page, not beside it: {:?}",
        shell.said()
    );
    assert!(way_back(&shell), "the way back stands on the page");
    shell.press("Back to notifications");
    assert!(
        shell.ran("close-reading"),
        "the way back returns to the list: {:?}",
        shell.called.calls()
    );

    // Escape is the way back without reaching for the button.
    let mut shell = staged(
        "the notification centre, reading a notice",
        Theme::light(),
        (1280, 900),
    );
    press_key(&mut shell, tilo_ui_base::winit::keyboard::NamedKey::Escape);
    assert!(
        shell.ran("close-reading"),
        "Escape returns to the list: {:?}",
        shell.called.calls()
    );
}

/// A sender that asked for no preview keeps its words unseen; the same page on a phone, in the
/// dark, stands whole and scrolls by the keyboard.
#[test]
fn a_notice_it_may_not_preview_says_so_and_a_long_page_scrolls_on_a_phone() {
    let shell = staged(
        "the notification centre, reading a notice it may not preview",
        Theme::light(),
        (1280, 900),
    );
    assert!(
        shell.shows("Notification preview hidden"),
        "the page says the preview is hidden: {:?}",
        shell.said()
    );
    assert!(
        !shell.shows("The launch date is the 14th."),
        "none of the words it was sent with shows: {:?}",
        shell.said()
    );

    let mut shell = staged(
        "the notification centre, reading a notice",
        Theme::dark(),
        (320, 420),
    );
    assert!(shell.shows("Disk almost full"), "{:?}", shell.said());
    assert!(way_back(&shell), "the way back stands on a phone too");
    for node in shell.harness.inspection().root.nodes() {
        if node.rect[2] <= 0.0 || node.rect[3] <= 0.0 {
            continue;
        }
        assert!(
            node.rect[0] >= -1.0 && node.rect[0] + node.rect[2] <= 321.0,
            "nothing of the page runs off the display: {node:?}"
        );
    }
    // Tab walks the page's controls and then the region its words are in; the page keys move the
    // words a screen at a time.
    let top = |shell: &StagedWindow| {
        shell
            .harness
            .inspection()
            .root
            .nodes()
            .into_iter()
            .filter(|node| {
                node.semantics
                    .as_ref()
                    .and_then(|access| access.name.clone())
                    .is_some_and(|name| name.contains("A backup of 40"))
            })
            .map(|node| node.rect[1])
            .fold(f64::INFINITY, f64::min)
    };
    let before = top(&shell);
    assert!(
        before.is_finite(),
        "the words stand on the page: {:?}",
        shell.said()
    );
    for _ in 0..3 {
        press_key(&mut shell, tilo_ui_base::winit::keyboard::NamedKey::Tab);
    }
    press_key(
        &mut shell,
        tilo_ui_base::winit::keyboard::NamedKey::PageDown,
    );
    let after = top(&shell);
    assert!(
        after < before - 1.0,
        "PageDown pages the words: {before} → {after}"
    );
}
