//! What claiming the machine's display and giving it back did, in words a person reads: which
//! session let go of it, and which took it back.

use tilo_device_protocol::Action;

/// The greeter a laptop's display waits at while no account has claimed it.
const GREETER_UNIT: &str = "greetd.service";

/// The start of the unit names a board image starts its own session under.
const BOARD_SESSION: &str = "tilo-board-";

/// What a claim did: the account that took the display, and the session that let go of it.
pub(crate) fn describe_claim(account: &str, actions: &[Action], changed: bool) -> String {
    if !changed {
        return format!("account {account} already holds seat0; nothing to do");
    }
    let stopped: Vec<&str> = actions
        .iter()
        .filter_map(|action| match action {
            Action::StopUnit { unit } => Some(unit.as_str()),
            _ => None,
        })
        .collect();
    let whose = if stopped.contains(&GREETER_UNIT) {
        "greetd is stopped".to_owned()
    } else if stopped.iter().any(|unit| unit.starts_with(BOARD_SESSION)) {
        format!(
            "the machine's own session is stopped ({}), it held the display",
            stopped.join(", ")
        )
    } else if stopped.is_empty() {
        "the machine's own session was already away".to_owned()
    } else {
        "the account that held it is back on its own display".to_owned()
    };
    format!("account {account} now holds seat0 (the machine's display); {whose}; session restarted")
}

/// What a release did: the account that left the display, and the session that has it back.
pub(crate) fn describe_release(account: &str, actions: &[Action], changed: bool) -> String {
    if !changed {
        return format!("account {account} does not hold seat0; nothing to do");
    }
    let started: Vec<&str> = actions
        .iter()
        .filter_map(|action| match action {
            Action::StartUnit { unit } => Some(unit.as_str()),
            _ => None,
        })
        .collect();
    let whose = if started.contains(&GREETER_UNIT) {
        "seat0 returned to greetd".to_owned()
    } else if started.iter().any(|unit| unit.starts_with(BOARD_SESSION)) {
        format!(
            "the machine's own session is back on seat0 ({})",
            started.join(", ")
        )
    } else {
        "seat0 returned".to_owned()
    };
    format!("{whose}; account {account} restarted on its own display")
}
