//! Benchmark for the "dropping a dragged window freezes the shell ~3s"
//! bug: the render thread blocks while `resync_strips_from_tree` ->
//! `to_ui_window_group` -> `to_ui_tile` constructs a live app widget
//! synchronously (reads a directory for Files, forks a PTY for Terminal).
//!
//! Run: `cargo test -p shell --test freeze_bench -- --ignored --nocapture`

use std::time::Instant;

use shell::shell::state::{TileContent as ShellTile, WindowGroup};
use shell::shell::to_ui_window_group;
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
    // Map the ui AppKind back to the shell AppKind the tile expects.
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
        }],
        active_tab: tab_id,
        width: shell::shell::state::WindowWidth::Normal,
        maximized: false,
    }
}

#[test]
#[ignore = "a benchmark that prints timings; run with --ignored --nocapture"]
fn terminal_first_sync_crdt_cost() {
    // The actual drop freeze: a terminal's first sync/handle_event lazily
    // populates its grid into the shared Automerge doc (`init_crdt` ->
    // `ensure_lines`).
    use automerge::{AutoCommit, ROOT};
    use terminal::TerminalWidget;
    use ui::widget::Widget;

    let mut w = match TerminalWidget::spawn_default() {
        Ok(w) => w,
        Err(e) => {
            println!("terminal spawn failed ({e}); skipping");
            return;
        }
    };
    // Let the PTY emit its prompt so the grid has content, matching a
    // terminal that's been alive a couple of seconds.
    std::thread::sleep(std::time::Duration::from_millis(300));

    // Fresh shared doc (like the compositor's `test_control_ctx.doc`).
    let mut doc = AutoCommit::new();
    let first = ms(|| {
        Widget::sync(&mut w, &mut doc, &ROOT);
    });
    let second = ms(|| {
        Widget::sync(&mut w, &mut doc, &ROOT);
    });
    println!("--- terminal first-sync CRDT populate (the drop freeze) ---");
    println!("  first  sync (init_crdt): {first:8.2} ms");
    println!("  second sync (idempotent):{second:8.2} ms");
}

#[test]
#[ignore = "a benchmark that prints timings; run with --ignored --nocapture"]
fn rematerialize_group_cost() {
    // Records stay in memory: this measures widget construction, not what a
    // session writes to disk.
    let records = ui::record::RecordStore::in_memory();
    // Warm the OS caches once so we measure steady-state, not first-touch.
    let _ = to_ui_window_group(&placeholder_group(UiKind::Files, "Files"), &records);

    let files = ms(|| {
        let _ = to_ui_window_group(&placeholder_group(UiKind::Files, "Files"), &records);
    });
    let terminal = ms(|| {
        let _ = to_ui_window_group(&placeholder_group(UiKind::Terminal, "Terminal"), &records);
    });
    let notes = ms(|| {
        let _ = to_ui_window_group(
            &placeholder_group(
                UiKind::Editor, // routed to a Stub unless "Notes"/"Editor" label
                "Notes",
            ),
            &records,
        );
    });

    println!("--- re-materialize cost (render-thread stall per detached group) ---");
    println!("  Files group   : {files:8.2} ms");
    println!("  Terminal group: {terminal:8.2} ms");
    println!("  Notes group   : {notes:8.2} ms");
    println!("  (HOME = {:?})", std::env::var("HOME").ok());
}
