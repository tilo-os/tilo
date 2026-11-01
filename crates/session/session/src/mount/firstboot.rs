//! The machine's first boot, which `shell-firstboot.service` runs as
//! `tilo-session --firstboot-only`: the person's config directories are
//! made, the host desktop's input settings are imported where the device is
//! a host package, and the process exits without opening a display. It is
//! cheap and idempotent; the unit guards re-runs with a marker file.
//!
//! It never creates a personal identity: that is an acknowledged ceremony in
//! setup, in front of the person. [`setup_due`] is the other half of that
//! ceremony: whether a starting session opens it.

use std::error::Error;

/// What a `tilo-session` command line asks for besides a session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OneShot {
    /// `--firstboot-only`: set the machine up, then exit.
    SetupOnly,
    /// `--firstboot`: set the machine up, then go on to the session.
    SetupThenSession,
}

/// What the flags among `args` (the program's arguments, without the
/// program) ask for, and `None` for a command line that starts a session.
///
/// # Errors
/// Both first-boot flags together, which would say two things at once.
pub fn one_shot(args: &[String]) -> Result<Option<OneShot>, String> {
    let asked = |flag: &str| args.iter().any(|arg| arg == flag);
    match (asked("--firstboot-only"), asked("--firstboot")) {
        (true, true) => Err(
            "--firstboot and --firstboot-only are mutually exclusive: --firstboot continues into \
             the session while --firstboot-only exits."
                .to_owned(),
        ),
        (true, false) => Ok(Some(OneShot::SetupOnly)),
        (false, true) => Ok(Some(OneShot::SetupThenSession)),
        (false, false) => Ok(None),
    }
}

/// Set the machine up for its person: the config directories, and on a host
/// desktop the input settings of the desktop it replaces.
///
/// # Errors
/// Directory creation failed (permission denied, no space left).
pub fn run_setup(host_desktop: bool) -> Result<(), Box<dyn Error>> {
    tilo_onboarding_host::firstboot::run()?;
    if host_desktop {
        // What the host desktop was set to is imported once, and the person
        // keeps it; a machine whose settings service is not up yet is tried
        // again on the next boot rather than failing this one.
        if let Err(error) = tilo_onboarding_host::firstboot::import_host_input_settings() {
            tracing::warn!(%error, "the host desktop's input settings were not imported");
        }
    }
    Ok(())
}

/// Whether this device has setup to do before its desktop is used: it has no
/// identity yet, the last startup found its identity unusable, or the person
/// began setting it up and has not finished — the ceremony's journal, or a
/// prepared setup neither consumed nor declined. A finished setup leaves none
/// of these behind, so a restart goes straight to the person's desk.
pub fn setup_due(identity_missing: bool, recovery: bool, identity_dir: &std::path::Path) -> bool {
    recovery
        || identity_missing
        || tilo_onboarding_host::view::has_pending_progress(identity_dir)
        || tilo_onboarding_host::prepared::session_pending(identity_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(flags: &[&str]) -> Vec<String> {
        flags.iter().map(|flag| (*flag).to_owned()).collect()
    }

    #[test]
    fn the_two_first_boot_flags_say_one_thing_each() {
        assert_eq!(one_shot(&args(&[])), Ok(None));
        assert_eq!(
            one_shot(&args(&["--firstboot-only"])),
            Ok(Some(OneShot::SetupOnly))
        );
        assert_eq!(
            one_shot(&args(&["--backend", "nested", "--firstboot"])),
            Ok(Some(OneShot::SetupThenSession))
        );
        assert!(
            one_shot(&args(&["--firstboot", "--firstboot-only"])).is_err(),
            "two first-boot flags at once are refused"
        );
    }

    /// This machine's home for one test, so the prepared setup the device
    /// may hold there is this test's own.
    #[cfg(target_os = "linux")]
    fn home() -> (tilo_test_env::ScopedEnv, tempfile::TempDir) {
        let mut env = tilo_test_env::ScopedEnv::new();
        let home = tempfile::tempdir().expect("home");
        env.set("HOME", home.path());
        (env, home)
    }

    #[cfg(target_os = "linux")]
    fn identity_dir(home: &std::path::Path) -> std::path::PathBuf {
        let dir = home.join(".config/tilo/identity");
        std::fs::create_dir_all(&dir).expect("identity directory");
        dir
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_device_that_finished_its_setup_starts_at_its_desk() {
        let (_env, home) = home();
        let dir = identity_dir(home.path());
        assert!(!setup_due(false, false, &dir), "nothing is left to set up");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_device_with_no_identity_sets_up_before_its_desk() {
        let (_env, home) = home();
        let dir = identity_dir(home.path());
        assert!(setup_due(true, false, &dir));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn an_identity_the_last_startup_found_unusable_sets_up_again() {
        let (_env, home) = home();
        let dir = identity_dir(home.path());
        assert!(setup_due(false, true, &dir));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_restart_mid_setup_opens_it_again() {
        let (_env, home) = home();
        let dir = identity_dir(home.path());
        // The ceremony's journal, written ahead of the step it guards: any
        // entry at its path is setup the person had not finished.
        std::fs::write(
            dir.join(".onboarding-progress.json"),
            br#"{"version":1,"stage":"identity-ready"}"#,
        )
        .expect("journal");
        assert!(
            tilo_onboarding_host::view::has_pending_progress(&dir),
            "the journal is the one the ceremony leaves"
        );
        assert!(
            setup_due(false, false, &dir),
            "setup under way is opened again"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_prepared_setup_waits_for_its_owner() {
        let (home, dir, _) = tilo_onboarding_host::browser::fixtures::fixture();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("HOME", home.path());
        assert!(
            tilo_onboarding_host::prepared::session_pending(&dir),
            "the fixture leaves a prepared setup"
        );
        assert!(setup_due(false, false, &dir));
    }
}
