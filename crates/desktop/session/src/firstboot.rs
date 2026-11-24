//! The machine's first boot, which `tilo-firstboot.service` runs as `tilo-session --firstboot-only`
//! : the person's config directories are made, the host desktop's input settings are imported where
//! the device is a host package, and the process exits without opening a display. It is cheap and
//! idempotent; the unit guards re-runs with a marker file.
//! It never creates a personal identity: that is an acknowledged ceremony in setup, in front of the
//! person. [`setup_due`] is the other half of that ceremony: whether a starting session opens it.

use std::error::Error;

/// What a `tilo-session` command line asks for besides a session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OneShot {
    /// `--firstboot-only` : set the machine up, then exit.
    SetupOnly,
    /// `--firstboot` : set the machine up, then go on to the session.
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
    tilo_onboarding_session::firstboot::run()?;
    if host_desktop {
        // What the host desktop was set to is imported once, and the person keeps it; a machine
        // whose settings service is not up yet is tried again on the next boot rather than failing
        // this one.
        if let Err(error) = tilo_onboarding_session::firstboot::import_host_input_settings() {
            tracing::warn!(%error, "the host desktop's input settings were not imported");
        }
    }
    Ok(())
}

/// Whether this device has setup to do before its desktop is used: it has no identity yet, the last
/// startup found its identity unusable, or the person began setting it up and has not finished:
/// the step their settings hold for this device. A finished setup leaves none of these behind, so
/// a restart goes straight to the person's session.
pub fn setup_due(identity_missing: bool, recovery: bool) -> bool {
    recovery || identity_missing || tilo_onboarding_session::view::setup_unfinished()
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

    /// This machine's home for one test, so whatever setup state the device holds there is this
    /// test's own.
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

    /// This device, with a key of its own under `home`, and the person's settings for this test's
    /// thread holding `steps`: each device's step of setup, by the device's key.
    #[cfg(target_os = "linux")]
    fn device(
        env: &mut tilo_test_env::ScopedEnv,
        home: &std::path::Path,
        steps: impl FnOnce(&str) -> serde_json::Map<String, serde_json::Value>,
    ) -> settings_store::SourceScope {
        env.set("TILO_IDENTITY_ROOT", home.join("identity-root"));
        let paths = tilo_onboarding_session::firstboot::collaboration_identity_paths();
        let key = tilo_device_identity::persist::adopt_device_key(&paths, &[5; 32])
            .expect("this device's key");
        let this_device = hex::encode(key.verifying_key().as_bytes());
        let mut settings = serde_json::Map::new();
        settings.insert(
            tilo_onboarding_session::view::PROGRESS_SECTION.to_owned(),
            serde_json::Value::Object(steps(&this_device)),
        );
        settings_store::scoped_source(Some(std::sync::Arc::new(
            settings_store::HostDocument::new(settings, |_| Ok(())),
        )))
    }

    tilo_test_env::scenario! {
        #[cfg(target_os = "linux")]
        fn setup_is_due_until_the_device_finished_it;

        phase a_device_that_finished_its_setup_starts_at_its_shell;
        {
            let (_env, home) = home();
            let dir = identity_dir(home.path());
            assert!(!setup_due(false, false), "nothing is left to set up");
        }

        phase a_device_with_no_identity_sets_up_before_its_shell;
        {
            let (_env, home) = home();
            let dir = identity_dir(home.path());
            assert!(setup_due(true, false));
        }

        phase an_identity_the_last_startup_found_unusable_sets_up_again;
        {
            let (_env, home) = home();
            let dir = identity_dir(home.path());
            assert!(setup_due(false, true));
        }

        phase a_restart_mid_setup_opens_it_again;
        {
            let (mut env, home) = home();
            let dir = identity_dir(home.path());
            // The step setup recorded for this device once its identity was made, as the person's
            // settings hold it: setup the person had not finished.
            let _settings = device(&mut env, home.path(), |this_device| {
                serde_json::Map::from_iter([(this_device.to_owned(), "identity-ready".into())])
            });
            assert!(
                tilo_onboarding_session::view::setup_unfinished(),
                "the step is the one setup records"
            );
            assert!(
                setup_due(false, false),
                "setup under way is opened again"
            );
        }

        phase setup_another_device_of_theirs_is_in_stays_there;
        {
            let (mut env, home) = home();
            let dir = identity_dir(home.path());
            // The person's settings reach all their devices: another one's step is kept under that
            // device's key, and this device's own setup is finished.
            let _settings = device(&mut env, home.path(), |this_device| {
                serde_json::Map::from_iter([
                    ("cd".repeat(32), "tour-organise".into()),
                    (this_device.to_owned(), serde_json::Value::Null),
                ])
            });
            assert!(
                !setup_due(false, false),
                "nothing is left to set up here"
            );
        }
    }
}
