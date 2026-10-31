//! What tests of a window's history (`std.history`) read of it: the moments it shows as how
//! long ago they were, when the window next wakes, and its pictures.

use ui::framework::testing::HostedApp;

use crate::change_marks::walk;

/// What each moment of the window says, as `ui.Time` shows it (`2 min ago`), top to bottom.
pub(crate) fn times(app: &mut HostedApp) -> Vec<String> {
    let snapshot = app.harness().inspection();
    let mut all = Vec::new();
    walk(&snapshot.root, &mut all);
    all.into_iter()
        .filter(|node| node.widget == "RelativeTime")
        .filter_map(|node| node.semantics.as_ref()?.name.clone())
        .collect()
}

/// How long the window sleeps before something it shows is due to change, if it waits for one.
pub(crate) fn sleeps_for(app: &mut HostedApp) -> Option<ui::time::Duration> {
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
pub(crate) fn shoot(app: &mut HostedApp, name: &str, settle: impl Fn(&mut HostedApp)) {
    let Some(dir) = shots_dir() else {
        return;
    };
    for (shade, theme) in [("light", ui::Theme::light()), ("dark", ui::Theme::dark())] {
        app.harness().set_theme(theme);
        settle(app);
        app.harness()
            .render_png(&dir.join(format!("{name}-{shade}.png")));
    }
    app.harness().set_theme(ui::Theme::light());
    settle(app);
}
