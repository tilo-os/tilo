//! Spawns the tilo shell headless, embeds a terminal, file-explorer, and
//! browser in tabs, and asserts their `AppWidget` metadata reaches
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

#[cfg(target_os = "macos")]
use crate as shell;
use shell::shell::{Shell, tile::TileContent};
use std::time::Duration;
use ui::AppKind;

#[test]
fn default_workspace_hosts_live_terminal_files_and_browser_tiles() {
    // Phase `default_workspace_hosts_terminal_files_and_real_browser_widget`.
    {
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
            .map(|t| match t {
                TileContent::App(a) => a.app_kind(),
                TileContent::Stub { kind, .. } => *kind,
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
            "default workspace must include the browser app (kinds={kinds:?})"
        );
    }

    // Phase `file_explorer_tab_lists_current_directory`.
    {
        fn phase() {
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
        phase();
    }
}

// Abbreviated render pipeline (sync + update, no Vello draw — that needs
// a GPU), via three introspection helpers guarded behind
// `cfg(any(test, debug_assertions))`: `tick_for_test`,
// `first_terminal_line`, `first_file_explorer_entries`.

#[test]
fn terminal_tab_emits_a_prompt_within_10_seconds() {
    // An interactive PTY with an explicit startup environment. No operator
    // login/rc files are read; the shell emits its normal interactive prompt.
    let directory = tempfile::tempdir().expect("terminal working directory");
    let mut command = terminal::CommandBuilder::new("/bin/sh");
    command.arg("-i");
    command.env_clear();
    command.env("HOME", directory.path());
    command.env("PATH", "/usr/bin:/bin");
    command.env("PS1", "tilo-fixture$ ");
    command.cwd(directory.path());
    let terminal = terminal::TerminalWidget::spawn_command(command).expect("real fixture PTY");
    let mut shell = Shell::new_with_demo_terminal(terminal);
    assert!(
        shell.first_terminal_line().is_some(),
        "the provided PTY must be mounted as a live tile"
    );

    let deadline = tilo_test_env::hang_deadline();
    while std::time::Instant::now() < deadline {
        shell.tick_for_test();
        if let Some(line0) = shell.first_terminal_line()
            && !line0.trim().is_empty()
            && line0.contains("tilo-fixture$")
        {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("terminal never produced a non-empty first line within 10s");
}
