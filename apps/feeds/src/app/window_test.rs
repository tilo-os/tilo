//! A compact window shows one screen whole.

use super::tests::{loaded, synced};
use super::*;
use ui::headless::Framed;

/// Mount `app` in a window of `viewport` and run it until its layout
/// settles, so the test can read the geometry the app lays out in.
fn settled_at(mut app: FeedsApp, viewport: (u32, u32)) -> Framed<FeedsApp> {
    let doc = synced(&mut app);
    let mut state = ui::State::new();
    assert!(state.adopt_doc(doc));
    let mut mounted = Framed::new(state, app, viewport, Theme::dark());
    settle(&mut mounted);
    mounted
}

/// Run frames until the window's push stack has stopped moving: the motion
/// each frame advances is the only thing left to wait for here.
fn settle(mounted: &mut Framed<FeedsApp>) {
    let settled = |m: &ui::headless::FrameHarness| {
        let column = m.root::<FeedsApp>().shell().geometry().content_column;
        column.x0.abs() < 1e-6
    };
    Framed::harness_mut(mounted).wait_until(ui::time::Duration::from_secs(10), settled);
}

/// The rect a screen covers, in window coordinates.
fn screen_of(shell: &NavShell, pane: NavPane) -> Rect {
    let geometry = shell.geometry();
    match pane {
        NavPane::Sidebar => geometry.sidebar_column,
        NavPane::Overview => geometry.overview_column,
        NavPane::Content => geometry.content_column,
    }
}

/// Assert the window shows its one screen whole: the screen in front fills
/// the window from its leading edge to its trailing edge, and every other
/// screen stands off the window rather than half under one of its edges.
fn assert_one_screen_whole(shell: &NavShell, viewport: (u32, u32), why: &str) {
    let width = f64::from(viewport.0);
    assert!(
        !shell.geometry().stack.is_empty(),
        "{why}: a window this narrow shows one screen at a time"
    );
    let front = shell.front_pane();
    let front_rect = screen_of(shell, front);
    assert!(
        front_rect.x0.abs() < 1e-6 && (front_rect.x1 - width).abs() < 1e-6,
        "{why}: the screen in front ({front:?}) fills the window, got {front_rect:?}"
    );
    for pane in [NavPane::Sidebar, NavPane::Overview, NavPane::Content] {
        if pane == front {
            continue;
        }
        let other = screen_of(shell, pane);
        assert!(
            other.width() <= 0.0 || other.x0 >= width - 1e-6 || other.x1 <= 1e-6,
            "{why}: {pane:?} stands over the screen in front, at {other:?}"
        );
    }
}

/// A compact window with an article open shows the article whole: no part of
/// the article list is left under the window's leading edge beside it.
#[test]
fn a_compact_window_with_an_article_open_shows_no_slice_of_the_list() {
    let viewport = (420, 860);
    let app = FeedsApp::new_demo();
    // The library lands on the compact window's stack before the person
    // chooses an article, as the framework presents it.
    let mut mounted = settled_at(app, viewport);
    let id = mounted.core().store.articles_for(&Filter::All)[1]
        .id
        .clone();
    {
        let (a, st) = Framed::split(&mut mounted);
        a.core_mut()
            .select_article(&id, &mut ui::widget::Edits::lend(&mut st.doc));
        a.shell_mut().snap();
    }
    settle(&mut mounted);
    assert_eq!(
        mounted.shell().front_pane(),
        NavPane::Content,
        "choosing an article pushes it in"
    );
    assert_one_screen_whole(mounted.shell(), viewport, "an article opened from the list");
}

/// A library change the person did not make on screen never brings the list
/// back in front of the article: after unsubscribing from the open article's
/// feed, the compact window still shows the article whole.
#[test]
fn a_compact_window_keeps_the_article_whole_while_the_library_changes() {
    let viewport = (420, 860);
    let app = FeedsApp::new_demo();
    let mut mounted = settled_at(app, viewport);
    let (id, feed) = {
        let core = mounted.core();
        let article = &core.store.articles_for(&Filter::All)[1];
        (article.id.clone(), article.feed_id.clone())
    };
    {
        let (a, st) = Framed::split(&mut mounted);
        a.core_mut()
            .select_article(&id, &mut ui::widget::Edits::lend(&mut st.doc));
        a.shell_mut().snap();
    }
    settle(&mut mounted);
    assert_eq!(mounted.shell().front_pane(), NavPane::Content);
    // A subscription is taken away (here from the feed's own menu, as the
    // sidebar offers it), and the window reads the library again.
    {
        let (a, st) = Framed::split(&mut mounted);
        let mut doc = std::mem::replace(&mut st.doc, AutoCommit::new());
        a.core_mut().apply_menu_action(
            MenuAction::Unsubscribe(feed),
            &mut ui::widget::Edits::lend(&mut doc),
        );
        st.doc = doc;
        let library = Rc::new(loaded(&st.doc));
        a.present_library(library);
    }
    settle(&mut mounted);
    assert_eq!(
        mounted.shell().front_pane(),
        NavPane::Content,
        "a library change leaves the article the person reads in front"
    );
    assert_one_screen_whole(
        mounted.shell(),
        viewport,
        "a subscription taken away under the open article",
    );
}
