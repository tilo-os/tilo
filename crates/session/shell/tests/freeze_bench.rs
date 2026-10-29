//! Benchmark for the "dropping a dragged window freezes the shell ~3s"
//! bug: the render thread blocks while `resync_strips_from_tree` ->
//! `to_ui_window_group` -> `to_ui_tile` constructs a live app widget
//! synchronously (reads a directory for Files, forks a PTY for Terminal).
//!
//! Run: `cargo test -p shell --test freeze_bench -- --ignored --nocapture`

use std::time::Instant;

use shell::shell::state::{TileContent as ShellTile, WindowGroup};
use shell::shell::to_ui_window_group_with_inputs;
use ui::widget::AppKind as UiKind;

fn ms(f: impl FnOnce()) -> f64 {
    let t = Instant::now();
    f();
    t.elapsed().as_secs_f64() * 1000.0
}

fn placeholder_group(kind: ui::widget::AppKind, label: &str) -> WindowGroup {
    // A one-tab group whose tile is an AppPlaceholder — the shape the
    // persisted tree carries into `to_ui_window_group`.
    use shell::shell::state::Tab;
    let tab_id: u64 = 9_000_001;
    let shell_kind = match kind {
        UiKind::Terminal => shell::shell::state::AppKind::Terminal,
        UiKind::Files => shell::shell::state::AppKind::FileExplorer,
        _ => shell::shell::state::AppKind::Other(label.to_string()),
    };
    WindowGroup {
        id: 9_000_000,
        tabs: vec![Tab {
            id: tab_id,
            title: label.to_string(),
            tile: ShellTile::AppPlaceholder {
                kind: shell_kind,
                label: label.to_string(),
            },
            app_title: None,
        }],
        active_tab: tab_id,
        width: shell::shell::state::WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    }
}

#[test]
#[ignore = "a benchmark that prints timings; run with --ignored --nocapture"]
fn rematerialize_group_cost() {
    let inputs = shell::shell::hosted_inputs::HostedTileInputs::fixture().unwrap();
    // Warm the OS caches once so we measure steady-state, not first-touch.
    let _ = to_ui_window_group_with_inputs(&placeholder_group(UiKind::Files, "Files"), &inputs);

    let files = ms(|| {
        let _ = to_ui_window_group_with_inputs(&placeholder_group(UiKind::Files, "Files"), &inputs);
    });
    let terminal = ms(|| {
        let _ = to_ui_window_group_with_inputs(
            &placeholder_group(UiKind::Terminal, "Terminal"),
            &inputs,
        );
    });
    let notes = ms(|| {
        let _ = to_ui_window_group_with_inputs(
            &placeholder_group(
                UiKind::Editor, // routed to a Stub unless "Notes"/"Editor" label
                "Notes",
            ),
            &inputs,
        );
    });

    println!("--- re-materialize cost (render-thread stall per detached group) ---");
    println!("  Files group   : {files:8.2} ms");
    println!("  Terminal group: {terminal:8.2} ms");
    println!("  Notes group   : {notes:8.2} ms");
}
