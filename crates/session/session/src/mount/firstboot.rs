//! The machine's first boot, which `shell-firstboot.service` runs as
//! `tilo-session --firstboot-only`: the person's config directories are
//! made, the host desktop's input settings are imported where the device is
//! a host package, and the process exits without opening a display. It is
//! cheap and idempotent; the unit guards re-runs with a marker file.
//!
//! It never creates a personal identity: that is an acknowledged ceremony in
//! setup, in front of the person.

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
}
