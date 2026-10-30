//! The machine's first boot: which binary the unit starts, and what that
//! binary does with the flag it is given.

use tilo_session::mount::firstboot::{OneShot, one_shot};

/// The unit the image ships for the machine's own first-boot setup.
const UNIT: &str = include_str!("../../../../os/usr/lib/systemd/system/shell-firstboot.service");

/// The image's components, which say where each binary is installed.
const COMPONENTS: &str = include_str!("../../../../os/components.toml");

fn exec_line() -> &'static str {
    UNIT.lines()
        .find_map(|line| line.strip_prefix("ExecStart="))
        .expect("the first-boot unit starts something")
}

#[test]
fn the_machines_first_boot_sets_up_and_stops_in_the_new_session() {
    let mut words = exec_line().split_whitespace();
    let program = words.next().expect("a program to run");
    let args: Vec<String> = words.map(str::to_owned).collect();
    assert_eq!(
        program, "/usr/bin/tilo-session",
        "first boot no longer starts the shell it replaces"
    );
    assert!(
        COMPONENTS.contains(&format!("to = \"{program}\"")),
        "the image installs the binary the unit starts"
    );
    assert_eq!(
        one_shot(&args),
        Ok(Some(OneShot::SetupOnly)),
        "setup, then the process exits: this unit is not a session"
    );
    // The live installer's boots are disposable and must not consume the
    // one-shot marker of the machine it installs onto.
    assert!(
        UNIT.lines()
            .any(|line| line == "ConditionKernelCommandLine=!tilo.install"),
        "the live medium still skips the installed machine's first boot"
    );
}
