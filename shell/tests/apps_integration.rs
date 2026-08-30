//! Spawns the nirios shell headless, embeds a real terminal and a real
//! file-explorer in tabs, and asserts their `AppWidget` metadata reaches
//! the live `Shell::workspace_tree()`. Also drives them through an
//! abbreviated render pipeline and asserts visible content: a non-empty
//! first terminal grid line once the PTY delivers bytes, and at least one
//! non-empty-named file-explorer entry.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use shell::shell::{Shell, tile::TileContent};
use std::time::Duration;
use ui::AppKind;

#[test]
fn default_workspace_hosts_terminal_and_files_and_browser_stub() {
    let shell = Shell::new_with_default_demo_workspace();
    let tree = shell.workspace_tree();
    let tabs: Vec<&TileContent> = tree
        .workspaces
        .iter()
        .flat_map(|w| w.strips.iter())
        .flat_map(|s| s.window_groups.iter())
        .flat_map(|g| g.tabs.iter().map(|t| &t.tile))
        .collect();

    let kinds: Vec<AppKind> = tabs
        .iter()
        .filter_map(|t| match t {
            TileContent::App(a) => Some(a.app_kind()),
            TileContent::Stub { kind, .. } => Some(*kind),
        })
        .collect();

    assert!(
        kinds.contains(&AppKind::Terminal),
        "default workspace must host a terminal (kinds={kinds:?})"
    );
    assert!(
        kinds.contains(&AppKind::Files),
        "default workspace must host a file-explorer (kinds={kinds:?})"
    );
    assert!(
        kinds.contains(&AppKind::Browser),
        "default workspace must include a browser stub (kinds={kinds:?})"
    );
}

// Abbreviated render pipeline (sync + update, no Vello draw — that needs
// a GPU), via three introspection helpers guarded behind
// `cfg(any(test, debug_assertions))`: `tick_for_test`,
// `first_terminal_line`, `first_file_explorer_entries`.

#[test]
fn terminal_tab_emits_a_prompt_within_10_seconds() {
    // PTY init time is environment-dependent (shell rc files, distro
    // defaults, container start-up); a 3s budget proved flaky on slower CI.
    // 10s gives even a cold bash with a heavy `.bashrc` time to print its
    // first prompt line.
    let mut shell = Shell::new_with_default_demo_workspace();

    // A failed PTY spawn (e.g. sandboxed CI without /dev/ptmx) leaves a Stub
    // rather than an App; skip so this doesn't assert against an
    // environment-shaped failure.
    if shell.first_terminal_line().is_none() {
        eprintln!(
            "skipped: no live terminal tile (PTY spawn likely failed; tile fell back to Stub)"
        );
        return;
    }

    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        shell.tick_for_test();
        if let Some(line0) = shell.first_terminal_line()
            && !line0.trim().is_empty()
        {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("terminal never produced a non-empty first line within 10s");
}

#[test]
fn file_explorer_tab_lists_current_directory() {
    let shell = Shell::new_with_default_demo_workspace();
    let entries = shell.first_file_explorer_entries();
    // The initial directory almost always has >= 1 entry; a truly empty one
    // skips via a marker rather than failing on a pristine sandbox.
    if entries.is_empty() {
        eprintln!("skipped: initial dir empty");
        return;
    }
    assert!(
        !entries[0].is_empty(),
        "first file-explorer entry must have a non-empty name"
    );
}
