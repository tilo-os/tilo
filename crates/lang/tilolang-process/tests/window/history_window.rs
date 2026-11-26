//! What tests of a window's history (`std.history`, under the window bar's History) read of it
//! and do to it: whether it shows, pressing the bar's History, the moments it shows as how long
//! ago they were, when the window next wakes, and its pictures.

use crate::change_marks::Driven;

/// What each moment of the window says, as `ui.Time` shows it (`2 min ago`), top to bottom.
pub(crate) fn times(app: &mut impl Driven) -> Vec<String> {
    let snapshot = app.harness().inspection();
    let all = snapshot.root.nodes();
    // The history's own steps, newest first: the page's facts stand over its heading and say
    // when the note was made, and are no step of it.
    let heading = all
        .iter()
        // The section's own heading, which names what it is the history of.
        .filter(|node| {
            node.widget == "Text"
                && node
                    .semantics
                    .as_ref()
                    .and_then(|semantics| semantics.name.as_deref())
                    .is_some_and(|name| name.starts_with("History of "))
        })
        .map(|node| node.rect[1])
        .fold(None, |least: Option<f64>, y| {
            Some(least.map_or(y, |held| held.min(y)))
        });
    all.into_iter()
        .filter(|node| node.widget == "RelativeTime")
        .filter(|node| heading.is_none_or(|heading| node.rect[1] >= heading))
        .filter_map(|node| node.semantics.as_ref()?.name.clone())
        .collect()
}

/// Whether the window shows a history: the section of the system's history view, headed
/// "History of" what it shows (the bar's button, "History", is no text of the window's).
pub(crate) fn shows_history(app: &mut impl Driven) -> bool {
    let snapshot = app.harness().inspection();
    snapshot.root.nodes().iter().any(|node| {
        node.widget == "Text"
            && node
                .semantics
                .as_ref()
                .and_then(|semantics| semantics.name.as_deref())
                .is_some_and(|name| name.starts_with("History of "))
    })
}

/// Press the window bar's History, as the shell's bar hands the press to the window, and let the
/// window settle: it says whether its History stands open after. A window shows its bar, so the
/// window is presented with it before the press.
pub(crate) fn press_history(app: &mut impl Driven) -> bool {
    app.harness().present_in_window_bar();
    app.harness()
        .press_window_history()
        .expect("the window offers its bar's History");
    app.settle();
    app.harness()
        .window_history_control()
        .is_some_and(ui::widget::HistoryControl::is_open)
}

/// How long the window sleeps before something it shows is due to change, if it waits for one.
pub(crate) fn sleeps_for(app: &mut impl Driven) -> Option<ui::time::Duration> {
    let deadline = app.harness().frame_schedule().next_wake_deadline?;
    Some(deadline.saturating_duration_since(ui::time::now()))
}

/// The wall clock `secs` after `at`.
pub(crate) fn after(at: ui::time::SystemTime, secs: u64) -> ui::time::SystemTime {
    at.checked_add(ui::time::Duration::from_secs(secs))
        .expect("the clock reaches it")
}

/// Where the shots go, when asked for with `TILO_HISTORY_SHOTS`: a relative directory is under
/// the build's target directory, where a remote builder can hand it back.
pub(crate) fn shots_dir() -> Option<std::path::PathBuf> {
    let dir = std::path::PathBuf::from(std::env::var_os("TILO_HISTORY_SHOTS")?);
    let dir = match std::env::var_os("CARGO_TARGET_DIR") {
        Some(target) if dir.is_relative() => std::path::PathBuf::from(target).join(dir),
        _ => dir,
    };
    std::fs::create_dir_all(&dir).unwrap();
    Some(dir)
}

/// Write the window as `<name>-light.png` and `<name>-dark.png`, when shots are asked for.
pub(crate) fn shoot<W: Driven>(app: &mut W, name: &str, settle: impl Fn(&mut W)) {
    let Some(dir) = shots_dir() else {
        return;
    };
    for (shade, theme) in [
        ("light", tilo_ui_base::Theme::light()),
        ("dark", tilo_ui_base::Theme::dark()),
    ] {
        app.harness().set_theme(theme);
        settle(app);
        app.harness()
            .render_png(&dir.join(format!("{name}-{shade}.png")));
    }
    app.harness().set_theme(tilo_ui_base::Theme::light());
    settle(app);
}
