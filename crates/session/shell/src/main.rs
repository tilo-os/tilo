//! Native platform entry points share the shell and UI libraries.

#[cfg(target_os = "linux")]
mod linux_main;

/// Hands every allocation straight to glibc unless `PPROF_ALLOC_SAMPLE_RATE`
/// was set when the compositor started; then it also samples the live heap by
/// call site (`shell::shell::heap_profile`).
#[cfg(all(target_os = "linux", feature = "hosted-test-control"))]
#[global_allocator]
static HEAP_PROFILE: pprof_alloc::PprofAlloc = pprof_alloc::PprofAlloc::new()
    .with_pprof()
    .with_pprof_sample_rate_from_env(0);

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux_main::run()
}

#[cfg(test)]
#[cfg(target_os = "macos")]
mod macos_tests {
    #![allow(clippy::unwrap_used, clippy::panic, reason = "test assertions")]
    use shell::compositor::macos::{Options, prepare};

    fn options(root: &std::path::Path) -> Options {
        Options {
            state_dir: root.join("state"),
            files_dir: root.to_path_buf(),
            shots: None,
        }
    }

    #[test]
    #[serial_test::serial]
    fn macos_help_exits_successfully() {
        assert_eq!(
            shell::compositor::macos::run_args(&["--help".into()]),
            std::process::ExitCode::SUCCESS
        );
    }

    #[test]
    #[serial_test::serial]
    fn macos_session_persists_its_profile_and_refuses_a_second_session() {
        let directory = tempfile::tempdir().unwrap();
        let options = options(directory.path());
        {
            let session = prepare(&options).unwrap();
            assert_eq!(session.tree.read().desktop.workspaces.len(), 1);
            assert!(
                session
                    .tree
                    .read()
                    .desktop
                    .workspaces
                    .iter()
                    .all(|ws| ws.collaborators.is_empty())
            );
            assert!(
                prepare(&options).is_err(),
                "a second session must not overwrite this profile"
            );
            session.tree.write().create_hosted_workspace("Mac work");
            session.tree.read().queue_save();
        }
        let session = prepare(&options).unwrap();
        assert!(
            session
                .tree
                .read()
                .desktop
                .workspaces
                .iter()
                .any(|ws| ws.name == "Mac work")
        );
    }

    #[test]
    #[serial_test::serial]
    fn macos_session_refuses_broken_state_and_bad_arguments() {
        let directory = tempfile::tempdir().unwrap();
        let options = options(directory.path());
        std::fs::create_dir_all(&options.state_dir).unwrap();
        let path = options.state_dir.join("state.json");
        std::fs::write(&path, "broken state").unwrap();
        assert!(prepare(&options).is_err());
        assert_eq!(std::fs::read_to_string(path).unwrap(), "broken state");
        assert!(shell::compositor::macos::parse(&["--state-dir".into()]).is_err());
        assert!(shell::compositor::macos::parse(&["--backend".into(), "drm".into()]).is_err());
    }

    #[test]
    #[serial_test::serial]
    fn macos_shortcuts_toggle_the_command_palette() {
        let directory = tempfile::tempdir().unwrap();
        let session = prepare(&options(directory.path())).unwrap();
        let mut driver =
            ui::headless::FrameHarness::new(ui::State::new(), Box::new(session.host), (1280, 800));
        for (modifier, key, opens) in [
            ("meta", "Space", false),
            ("alt", "Space", true),
            ("meta", "k", true),
        ] {
            driver.set_modifiers(&[modifier.into()]);
            driver.key(key);
            driver.set_modifiers(&[]);
            driver
                .expect(
                    &ui::headless::SemanticExpectation::named("command_palette_input")
                        .expanded(opens),
                )
                .unwrap();
            driver.key("Escape");
            driver
                .expect(
                    &ui::headless::SemanticExpectation::named("command_palette_input")
                        .expanded(false),
                )
                .unwrap();
        }
    }
}

#[cfg(target_os = "macos")]
fn main() -> std::process::ExitCode {
    shell::compositor::macos::run()
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn main() -> std::process::ExitCode {
    eprintln!("The Tilo shell currently supports Linux and macOS.");
    std::process::ExitCode::FAILURE
}
