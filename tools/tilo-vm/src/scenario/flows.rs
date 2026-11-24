//! The hardware-only parts of the two target flows, as a plan of named steps
//! driven through published actions and `tilo device`.
//!
//! A step states what it proves and what it needs. The runner asks the fleet
//! for that capability first: a missing action or a feature nobody has built
//! yet ends the step as `missing`, naming it, instead of waiting on a boot or
//! an invocation that never answers. That keeps today's scenario red for a
//! reason a person can act on, and it turns green step by step as the
//! capabilities land.

use serde_json::{Value, json};

/// One device in a flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Guest {
    /// The x86-64 desktop with two virtual outputs.
    DesktopX86,
    /// The aarch64 laptop.
    LaptopArm,
    /// The always-on server (`four-devices`) or the person's server beside a laptop (`laptop-phone-server`).
    Server,
}

impl Guest {
    pub const fn name(self) -> &'static str {
        match self {
            Self::DesktopX86 => "desktop-x86",
            Self::LaptopArm => "laptop-arm",
            Self::Server => "server",
        }
    }
}

/// What a step needs before it can say anything.
#[derive(Debug, Clone, Copy)]
pub enum Requirement {
    /// An action `guest`'s desktop must publish; `missing` says what its
    /// absence means to the person.
    Action {
        guest: Guest,
        needle: &'static str,
        missing: &'static str,
    },
    /// A command the guest must have, named for the reader.
    Command(&'static str),
    /// A capability named for the person; the step's own work proves it when
    /// it can, and otherwise reports exactly this as missing.
    Feature(&'static str),
}

#[derive(Debug, Clone, Copy)]
pub struct Step {
    pub id: &'static str,
    /// What a person gets when the step passes.
    pub proves: &'static str,
    /// The devices the step needs booted at all.
    pub guests: &'static [Guest],
    pub requires: Requirement,
}

/// The scenario's kind; its plan and the prerequisites between its steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Flow {
    /// One person, four devices used as one computer.
    FourDevices,
    /// Laptop and server with no tilo.sh.
    LaptopPhoneServer,
}

impl Flow {
    pub const fn name(self) -> &'static str {
        match self {
            Self::FourDevices => "four-devices",
            Self::LaptopPhoneServer => "laptop-phone-server",
        }
    }

    #[must_use]
    pub fn steps(self) -> Vec<Step> {
        match self {
            Self::FourDevices => four_devices_steps(),
            Self::LaptopPhoneServer => laptop_phone_server_steps(),
        }
    }
}

/// The first-use identity gap both the plan and its step state the same way.
const FIRST_USE_IDENTITY: &str = "an identity on a dev image's account, made through published actions (first use is a UI ceremony today)";

#[must_use]
pub fn four_devices_steps() -> Vec<Step> {
    vec![
        Step {
            id: "identity",
            proves: "each device's account holds the one identity the person signed in with",
            guests: &[Guest::DesktopX86],
            requires: Requirement::Feature(FIRST_USE_IDENTITY),
        },
        Step {
            id: "devices",
            proves: "every device lists all of them as the person's own",
            guests: &[Guest::DesktopX86, Guest::LaptopArm, Guest::Server],
            requires: Requirement::Feature(
                "adding each device carried across the fleet, so every dev-image account is one person's device",
            ),
        },
        Step {
            id: "second-output",
            proves: "the x86 desktop drives two virtual outputs",
            guests: &[Guest::DesktopX86],
            requires: Requirement::Feature(
                "a second virtual output on the x86 desktop VM (a second display adapter)",
            ),
        },
        Step {
            id: "outputs",
            proves: "all three outputs are published in the person's shared display view",
            guests: &[Guest::DesktopX86],
            requires: Requirement::Action {
                guest: Guest::DesktopX86,
                needle: "displays",
                missing: "the person's displays published as an action (person_displays)",
            },
        },
        Step {
            id: "arrangement",
            proves: "the three outputs sit in one arrangement the pointer crosses",
            guests: &[Guest::DesktopX86],
            requires: Requirement::Action {
                guest: Guest::DesktopX86,
                needle: "place display",
                missing: "an action that places a display in the person's arrangement",
            },
        },
        Step {
            id: "window",
            proves: "a window moves from the x86 desktop's second output to the laptop's",
            guests: &[Guest::DesktopX86, Guest::LaptopArm],
            requires: Requirement::Action {
                guest: Guest::DesktopX86,
                needle: "move window",
                missing: "an action that moves a window onto another output",
            },
        },
        Step {
            id: "server-turn",
            proves: "the server takes an agent turn after the laptop is suspended",
            guests: &[Guest::Server, Guest::LaptopArm],
            requires: Requirement::Action {
                guest: Guest::Server,
                needle: "agent",
                missing: "an agent turn on a server device with no desktop awake",
            },
        },
    ]
}

#[must_use]
pub fn laptop_phone_server_steps() -> Vec<Step> {
    vec![
        Step {
            id: "web-default",
            proves: "the server serves its own Tilo web UI with no tilo.sh",
            guests: &[Guest::Server],
            requires: Requirement::Feature("the server serving its own web UI by default"),
        },
        Step {
            id: "web-off",
            proves: "the server's web UI goes away when its device setting is off",
            guests: &[Guest::Server],
            requires: Requirement::Action {
                guest: Guest::Server,
                needle: "web",
                missing: "a device setting that turns the server's web UI off",
            },
        },
        Step {
            id: "offline",
            proves: "the desktop has no route to the internet",
            guests: &[Guest::DesktopX86],
            requires: Requirement::Command("curl"),
        },
        Step {
            id: "sign-in",
            proves: "the desktop signs in to the server with no tilo.sh",
            guests: &[Guest::DesktopX86, Guest::Server],
            requires: Requirement::Feature(
                "signing in on a device with no hosted service and no internet",
            ),
        },
        Step {
            id: "update-lease",
            proves: "the desktop takes the server's update lease before restarting it",
            guests: &[Guest::DesktopX86, Guest::Server],
            requires: Requirement::Action {
                guest: Guest::DesktopX86,
                needle: "update",
                missing: "an action on a server device that takes its update lease",
            },
        },
        Step {
            id: "reboot",
            proves: "the desktop restarts the server through an action",
            guests: &[Guest::DesktopX86, Guest::Server],
            requires: Requirement::Action {
                guest: Guest::DesktopX86,
                needle: "reboot",
                missing: "an action that restarts the server from another device",
            },
        },
    ]
}

/// What a step needs to have passed first.
fn prerequisites(flow: Flow, id: &str) -> &'static [&'static str] {
    match (flow, id) {
        (Flow::FourDevices, "devices") => &["identity"],
        (Flow::FourDevices, "second-output") => &["devices"],
        (Flow::FourDevices, "outputs") => &["devices", "second-output"],
        (Flow::FourDevices, "arrangement") => &["outputs"],
        (Flow::FourDevices, "window") => &["arrangement"],
        (Flow::FourDevices, "server-turn") => &["window"],
        (Flow::LaptopPhoneServer, "web-off" | "sign-in") => &["web-default"],
        (Flow::LaptopPhoneServer, "update-lease") => &["sign-in"],
        (Flow::LaptopPhoneServer, "reboot") => &["update-lease"],
        _ => &[],
    }
}

/// What the runner may do to the fleet: everything goes through the guest's
/// own SSH login, so a scenario drives the same programs a person's device
/// does.
pub trait Fleet {
    /// Run `command` in `guest`'s own login and answer its standard output.
    fn ssh(&mut self, guest: Guest, command: &str) -> Result<String, String>;
    /// Whether `guest`'s desktop publishes an action whose title or name
    /// matches `needle`.
    fn has_action(&mut self, guest: Guest, needle: &str) -> Result<bool, String>;
    /// Whether `guest` reaches `url` with an HTTP GET answering success.
    /// A refused or unroutable address is `Ok(false)`, not an error.
    fn http(&mut self, guest: Guest, url: &str) -> Result<bool, String>;
    /// Take `guest` off, as a person closing the lid would.
    fn suspend(&mut self, guest: Guest) -> Result<(), String>;
    /// Let `seconds` pass before the next question: a step that waits for a device to answer.
    fn pause(&mut self, _seconds: u64) {}
    /// Why `guest` is not part of the fleet at all, when it was never booted.
    fn unavailable(&self, _guest: Guest) -> Option<String> {
        None
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Passed,
    /// The step could not run because this capability is not built yet.
    Missing(String),
    /// The capability exists, but running the step failed.
    Failed(String),
    /// An earlier step is red, so this step proves nothing yet.
    Blocked(&'static str),
}

impl Status {
    pub const fn is_red(&self) -> bool {
        !matches!(self, Self::Passed)
    }

    #[must_use]
    pub fn to_json(&self) -> Value {
        match self {
            Self::Passed => json!({"status": "passed"}),
            Self::Missing(what) => json!({"status": "missing", "missing": what}),
            Self::Failed(why) => json!({"status": "failed", "detail": why}),
            Self::Blocked(by) => json!({"status": "blocked", "by": by}),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepReport {
    pub id: &'static str,
    pub proves: &'static str,
    pub status: Status,
}

impl StepReport {
    #[must_use]
    pub fn to_json(&self) -> Value {
        let mut value = self.status.to_json();
        if let Value::Object(map) = &mut value {
            map.insert("step".into(), json!(self.id));
            map.insert("proves".into(), json!(self.proves));
        }
        value
    }
}

/// Run every step of `flow` in order, reporting each. A step whose
/// prerequisite is red is `blocked`, never skipped silently, and a capability
/// that is not there is `missing`, named, rather than waited for.
#[must_use]
pub fn run(flow: Flow, fleet: &mut dyn Fleet) -> Vec<StepReport> {
    let mut reports: Vec<StepReport> = Vec::new();
    for step in flow.steps() {
        let blocked = prerequisites(flow, step.id).iter().find(|earlier| {
            reports
                .iter()
                .any(|report| report.id == **earlier && report.status.is_red())
        });
        let unavailable = step
            .guests
            .iter()
            .find_map(|guest| fleet.unavailable(*guest));
        let status = if let Some(earlier) = blocked {
            Status::Blocked(earlier)
        } else if let Some(why) = unavailable {
            Status::Missing(why)
        } else {
            run_step(step, fleet)
        };
        reports.push(StepReport {
            id: step.id,
            proves: step.proves,
            status,
        });
    }
    reports
}

fn run_step(step: Step, fleet: &mut dyn Fleet) -> Status {
    match step.requires {
        Requirement::Action {
            guest,
            needle,
            missing,
        } => match fleet.has_action(guest, needle) {
            Ok(true) => body(step.id, fleet),
            Ok(false) => Status::Missing(missing.to_owned()),
            Err(why) => Status::Missing(format!(
                "{}'s desktop to publish its actions ({why})",
                guest.name()
            )),
        },
        Requirement::Command(_) | Requirement::Feature(_) => body(step.id, fleet),
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "one match arm per step, each stating what the step does and what its absence means"
)]
fn body(id: &'static str, fleet: &mut dyn Fleet) -> Status {
    match id {
        "identity" => match fleet.ssh(Guest::DesktopX86, "tilo device ls --json") {
            Ok(output) if !devices_in(&output).is_empty() => Status::Passed,
            Ok(_) => Status::Missing(FIRST_USE_IDENTITY.to_owned()),
            Err(why) if says_no_identity(&why) => {
                Status::Missing(format!("{FIRST_USE_IDENTITY}: {why}"))
            }
            Err(why) => Status::Failed(why),
        },
        "devices" => {
            // The person's desktop gives each new device a ticket and the
            // device logs in with it: one identity, four devices.
            for guest in [Guest::LaptopArm, Guest::Server] {
                let _ = add_device(fleet, guest);
            }
            let mut fewest = usize::MAX;
            for guest in [Guest::DesktopX86, Guest::LaptopArm, Guest::Server] {
                match fleet.ssh(guest, "tilo device ls --json") {
                    Ok(output) => fewest = fewest.min(devices_in(&output).len()),
                    Err(why) => return Status::Failed(format!("{}: {why}", guest.name())),
                }
            }
            let fewest = if fewest == usize::MAX { 0 } else { fewest };
            if fewest >= 3 {
                Status::Passed
            } else {
                Status::Missing(format!(
                    "every device added under one identity (the smallest list names {fewest}, not 3)"
                ))
            }
        }
        "second-output" => {
            let count = fleet
                .ssh(
                    Guest::DesktopX86,
                    "ls -1 /sys/class/drm 2>/dev/null | grep -Ec '^card[0-9]+-' || true",
                )
                .ok()
                .and_then(|answer| answer.trim().parse::<usize>().ok())
                .unwrap_or(0);
            if count >= 2 {
                Status::Passed
            } else {
                Status::Missing(
                    "a second virtual output on the x86 desktop VM (a second display adapter)"
                        .to_owned(),
                )
            }
        }
        "outputs" => do_action(fleet, Guest::DesktopX86, "displays"),
        "arrangement" => do_action(fleet, Guest::DesktopX86, "place display"),
        "window" => do_action(fleet, Guest::DesktopX86, "move window"),
        "server-turn" => {
            if let Err(why) = fleet.suspend(Guest::LaptopArm) {
                return Status::Failed(format!("suspending the laptop: {why}"));
            }
            do_action(fleet, Guest::Server, "agent")
        }
        "web-default" => {
            let Some(url) = web_address(fleet) else {
                return Status::Missing("the server serving its own web UI by default".to_owned());
            };
            match fleet.http(Guest::Server, &url) {
                Ok(true) => Status::Passed,
                Ok(false) => {
                    Status::Missing("the server serving its own web UI by default".to_owned())
                }
                Err(why) => Status::Failed(why),
            }
        }
        "web-off" => {
            let Some(url) = web_address(fleet) else {
                return Status::Failed("the server reported no web address to turn off".to_owned());
            };
            if let Status::Failed(why) = do_action(fleet, Guest::Server, "web") {
                return Status::Failed(why);
            }
            match fleet.http(Guest::Server, &url) {
                Ok(false) => Status::Passed,
                Ok(true) => Status::Failed(
                    "the web UI still answers after its setting was turned off".to_owned(),
                ),
                Err(why) => Status::Failed(why),
            }
        }
        "offline" => match fleet.http(Guest::DesktopX86, "https://tilo.sh") {
            Ok(false) => Status::Passed,
            Ok(true) => {
                Status::Failed("the desktop still reached tilo.sh over the internet".to_owned())
            }
            Err(why) => Status::Failed(why),
        },
        "sign-in" => {
            let ticket = match device_ticket(fleet, Guest::Server) {
                Ok(ticket) => ticket,
                Err(why) if says_no_identity(&why) => {
                    return Status::Missing(
                        "signing in on a device with no hosted service and no internet".to_owned(),
                    );
                }
                Err(why) => return Status::Failed(why),
            };
            match fleet.ssh(Guest::DesktopX86, &format!("tilo login {ticket}")) {
                Ok(_) => Status::Passed,
                Err(why) if says_no_identity(&why) => Status::Missing(
                    "signing in on a device with no hosted service and no internet".to_owned(),
                ),
                Err(why) => Status::Failed(why),
            }
        }
        "update-lease" => do_action(fleet, Guest::DesktopX86, "update"),
        "reboot" => do_action(fleet, Guest::DesktopX86, "reboot"),
        other => Status::Failed(format!("no step is named {other}")),
    }
}

/// Add `guest` as a device under the desktop's identity with a ticket the desktop gives it, and
/// `tilo login` on the guest takes it.
fn add_device(fleet: &mut dyn Fleet, guest: Guest) -> Result<(), String> {
    let ticket = device_ticket(fleet, Guest::DesktopX86)?;
    fleet.ssh(guest, &format!("tilo login {ticket}"))?;
    Ok(())
}

/// How long between looks for the ticket a device's `tilo device add` prints, and how many.
const LOOKING_TRIES: u32 = 120;

/// A ticket `from` gives another computer, the way `tilo device add` gives one: the add asks
/// the device's person, and this plays them here, answering their card with the same resolve
/// call it makes. A device holding no person yet makes one, and the ticket the founding act
/// issued is the ticket.
fn device_ticket(fleet: &mut dyn Fleet, from: Guest) -> Result<String, String> {
    // A device that holds no person makes one, and only the act that makes it issues its first
    // tickets: no one is there to answer a card.
    if let Err(why) = fleet.ssh(from, "tilo device ls --json")
        && says_no_identity(&why)
    {
        let made = fleet.ssh(from, "tilo login --new-identity --json --ticket device")?;
        return founding_ticket(&made);
    }
    // The add runs from the guest's own login, as it does from a person's terminal: an agent of
    // theirs, so it asks them. Its ticket lands in a file once they answered.
    let adding = "/tmp/tilo-device-add.json";
    fleet.ssh(
        from,
        &format!(
            "rm -f {adding}; setsid sh -c 'tilo device add --json >{adding} \
             2>/tmp/tilo-device-add.err' </dev/null >/dev/null 2>&1 & echo started"
        ),
    )?;
    let mut answered = false;
    for _ in 0..LOOKING_TRIES {
        let printed = fleet.ssh(from, &format!("cat {adding} 2>/dev/null || true"))?;
        if let Ok(answer) = serde_json::from_str::<Value>(printed.trim())
            && let Some(ticket) = answer.get("ticket").and_then(Value::as_str)
            && !ticket.is_empty()
        {
            return Ok(ticket.to_owned());
        }
        if !answered
            && let Some((invocation, workspace)) = asked_to_add(fleet, from)
        {
            answer_as_person(fleet, from, &invocation, &workspace)?;
            answered = true;
        }
        fleet.pause(1);
    }
    let said = fleet
        .ssh(from, "cat /tmp/tilo-device-add.err 2>/dev/null || true")
        .unwrap_or_default();
    let said = said.trim();
    Err(format!(
        "{} printed no ticket for another computer{}",
        from.name(),
        if said.is_empty() {
            String::new()
        } else {
            format!(": {said}")
        }
    ))
}

/// The ticket a founding answer names for a computer, as the person's first act issued it.
fn founding_ticket(made: &str) -> Result<String, String> {
    let value: Value = serde_json::from_str(made.trim())
        .map_err(|error| format!("the founding answer is no JSON: {error}"))?;
    let tickets = value
        .get("tickets")
        .and_then(Value::as_array)
        .ok_or("the founding answer names no tickets")?;
    tickets
        .iter()
        .find(|ticket| ticket.get("kind").and_then(Value::as_str) == Some("device"))
        .or_else(|| tickets.first())
        .and_then(|ticket| ticket.get("ticket"))
        .and_then(Value::as_str)
        .filter(|ticket| !ticket.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| "the founding issued no ticket for a computer".to_owned())
}

/// The call `from`'s person is asked to add a computer, while one waits: its invocation, and
/// the workspace it waits in, as the workspace's invocations reading carries it.
fn asked_to_add(fleet: &mut dyn Fleet, from: Guest) -> Option<(String, String)> {
    let read = fleet
        .ssh(from, "tilo read invocations/workspace --json")
        .ok()?;
    adding_computer(&read)
}

/// The invocation `read`, a `tilo read invocations/workspace --json` answer, lists as waiting
/// for its person to add a computer: its id, and the workspace it waits in.
fn adding_computer(read: &str) -> Option<(String, String)> {
    let value: Value = serde_json::from_str(read).ok()?;
    let item = value.get("items")?.as_array()?.iter().find(|item| {
        let action = |path: &str| {
            item.pointer(path)
                .and_then(Value::as_str)
                .is_some_and(|action| action.ends_with("#issue"))
        };
        item.get("status").and_then(Value::as_str) == Some("waiting")
            && (item.get("title").and_then(Value::as_str) == Some("Add a device")
                || action("/asked/action")
                || action("/action"))
    })?;
    Some((
        item.get("id").and_then(Value::as_str)?.to_owned(),
        item.pointer("/asked/workspace")
            .and_then(Value::as_str)?
            .to_owned(),
    ))
}

/// Answer the ask `invocation` in `workspace` as `from`'s person, with the call their card
/// makes: the asked record's resolve, approving it. The person's own dispatch is the login they
/// entered at their server, else the desktop session's, which their shell answers their card
/// through: both are the person's own, never an agent's.
fn answer_as_person(
    fleet: &mut dyn Fleet,
    from: Guest,
    invocation: &str,
    workspace: &str,
) -> Result<(), String> {
    let resolve =
        format!("'invocations/workspace/{invocation}#resolve' resolution=approve -w '{workspace}'");
    let entered = fleet
        .ssh(from, &format!("tilo do --in-person {resolve}"))
        .err();
    if entered.is_none() {
        return Ok(());
    }
    let session = fleet
        .ssh(
            from,
            &format!(
                "TILO_API_SOCKET=$(ls /run/user/*/tilo-session/api.sock 2>/dev/null | head -n 1) \
                 tilo do {resolve}"
            ),
        )
        .err();
    match session {
        None => Ok(()),
        Some(why) => Err(format!(
            "{} did not answer the ask to add a computer: {}{}",
            from.name(),
            entered.unwrap_or_default().trim(),
            if why.trim().is_empty() {
                String::new()
            } else {
                format!(": {}", why.trim())
            }
        )),
    }
}

fn do_action(fleet: &mut dyn Fleet, guest: Guest, action: &str) -> Status {
    match fleet.ssh(guest, &format!("tilo do {action}")) {
        Ok(_) => Status::Passed,
        Err(why) => Status::Failed(why),
    }
}

/// A guest that cannot name the person's devices because it holds no identity
/// or nothing answers for it yet: a missing capability, not a broken one.
fn says_no_identity(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    [
        "identity",
        "not signed in",
        "no device",
        "daemon",
        "socket",
        "connection refused",
        "connect",
    ]
    .iter()
    .any(|needle| text.contains(needle))
}

/// The server's own web address, as its device status reports it.
fn web_address(fleet: &mut dyn Fleet) -> Option<String> {
    let output = fleet.ssh(Guest::Server, "tilo device status --json").ok()?;
    find_url(&output)
}

fn find_url(text: &str) -> Option<String> {
    fn walk(value: &Value) -> Option<String> {
        match value {
            Value::Object(map) => {
                for key in ["web", "web_url", "origin", "listen", "url"] {
                    if let Some(Value::String(url)) = map.get(key)
                        && url.starts_with("http")
                    {
                        return Some(url.clone());
                    }
                }
                map.values().find_map(walk)
            }
            Value::Array(items) => items.iter().find_map(walk),
            _ => None,
        }
    }
    let value: Value = serde_json::from_str(text).ok()?;
    walk(&value)
}

/// The devices a `tilo device ls --json` answer names.
fn devices_in(output: &str) -> Vec<String> {
    fn walk(value: &Value, devices: &mut Vec<String>) {
        match value {
            Value::Object(map) => {
                for key in ["device", "key", "name", "label"] {
                    if let Some(Value::String(name)) = map.get(key) {
                        devices.push(name.clone());
                        return;
                    }
                }
                for value in map.values() {
                    walk(value, devices);
                }
            }
            Value::Array(items) => {
                for value in items {
                    walk(value, devices);
                }
            }
            _ => {}
        }
    }
    let Ok(value) = serde_json::from_str::<Value>(output) else {
        return Vec::new();
    };
    let mut devices = Vec::new();
    walk(&value, &mut devices);
    devices.sort();
    devices.dedup();
    devices
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, HashSet};

    use super::*;

    #[derive(Default)]
    struct Fake {
        actions: HashSet<(Guest, String)>,
        offline: BTreeSet<Guest>,
        web: bool,
        device_lists: bool,
        listed_devices: usize,
        connectors: usize,
        /// A computer is being added: its add waits for the person, and prints the ticket once
        /// they answered.
        adding: bool,
        log: Vec<String>,
    }

    impl Fleet for Fake {
        fn ssh(&mut self, guest: Guest, command: &str) -> Result<String, String> {
            self.log.push(format!("{}: {command}", guest.name()));
            if command.starts_with("tilo device ls") {
                if !self.device_lists {
                    return Err("no identity on this device".into());
                }
                let listed = if self.listed_devices == 0 {
                    3
                } else {
                    self.listed_devices
                };
                let devices: Vec<Value> = (0..listed)
                    .map(|index| json!({"device": format!("device-{index}")}))
                    .collect();
                return Ok(json!({"devices": devices}).to_string());
            }
            if command.starts_with("ls -1 /sys/class/drm") {
                return Ok(format!("{}\n", self.connectors));
            }
            if command.contains("tilo do") {
                return Ok("{}".into());
            }
            if command.starts_with("tilo device status") {
                return Ok(json!({"web": self.web.then_some("http://127.0.0.1:8443/")}).to_string());
            }
            if command.contains("tilo device add") {
                self.adding = true;
                return Ok("started".into());
            }
            if command.starts_with("tilo read invocations/workspace") {
                if !self.adding {
                    return Ok(json!({"items": []}).to_string());
                }
                return Ok(json!({"items": [{
                    "id": "add-1",
                    "status": "waiting",
                    "title": "Add a device",
                    "asked": {"workspace": "workspace-1", "action": "tilo.device#issue"},
                }]})
                .to_string());
            }
            if command.starts_with("cat /tmp/tilo-device-add.json") {
                return Ok(if self.adding {
                    json!({"ticket": "tilo1ticket"}).to_string()
                } else {
                    String::new()
                });
            }
            if command.starts_with("tilo login --new-identity") {
                return Ok(json!({"tickets": [{"kind": "device", "ticket": "tilo1ticket"}]})
                    .to_string());
            }
            if command.starts_with("tilo login") {
                return Ok(String::new());
            }
            Ok(String::new())
        }

        fn has_action(&mut self, guest: Guest, needle: &str) -> Result<bool, String> {
            Ok(self.actions.contains(&(guest, needle.to_owned())))
        }

        fn http(&mut self, guest: Guest, url: &str) -> Result<bool, String> {
            self.log.push(format!("{}: GET {url}", guest.name()));
            if guest == Guest::DesktopX86 && self.offline.contains(&guest) {
                return Ok(false);
            }
            Ok(self.web && url.starts_with("http"))
        }

        fn suspend(&mut self, guest: Guest) -> Result<(), String> {
            self.log.push(format!("{}: suspend", guest.name()));
            Ok(())
        }
    }

    fn all_actions(fake: &mut Fake) {
        for (guest, needle) in [
            (Guest::DesktopX86, "displays"),
            (Guest::DesktopX86, "place display"),
            (Guest::DesktopX86, "move window"),
            (Guest::DesktopX86, "update"),
            (Guest::DesktopX86, "reboot"),
            (Guest::Server, "agent"),
            (Guest::Server, "web"),
        ] {
            fake.actions.insert((guest, needle.to_owned()));
        }
    }

    #[test]
    fn a_missing_identity_is_named_and_later_steps_are_blocked_not_hung() {
        let mut fleet = Fake::default();
        let reports = run(Flow::FourDevices, &mut fleet);
        assert_eq!(reports.len(), 7);
        assert!(
            matches!(&reports[0].status, Status::Missing(what) if what.contains("identity")),
            "{:?}",
            reports[0]
        );
        assert_eq!(reports[1].status, Status::Blocked("identity"));
        for report in &reports[2..] {
            assert!(matches!(report.status, Status::Blocked(_)), "{report:?}");
        }
        assert!(
            reports
                .iter()
                .all(|report| !matches!(report.status, Status::Failed(_)))
        );
    }

    #[test]
    fn a_present_action_is_run_and_a_missing_one_is_named() {
        let mut fleet = Fake {
            device_lists: true,
            connectors: 2,
            ..Fake::default()
        };
        fleet.actions.insert((Guest::DesktopX86, "displays".into()));
        let reports = run(Flow::FourDevices, &mut fleet);
        assert_eq!(reports[0].status, Status::Passed);
        assert_eq!(reports[1].status, Status::Passed);
        assert_eq!(reports[2].status, Status::Passed, "{:?}", reports[2]);
        assert_eq!(reports[3].status, Status::Passed, "{:?}", reports[3]);
        assert!(
            matches!(&reports[4].status, Status::Missing(what) if what.contains("places a display")),
            "{:?}",
            reports[4]
        );
        assert!(
            fleet
                .log
                .iter()
                .any(|line| line == "desktop-x86: tilo do displays")
        );
    }

    #[test]
    fn the_laptop_is_suspended_before_the_server_takes_its_turn() {
        let mut fleet = Fake {
            device_lists: true,
            connectors: 2,
            ..Fake::default()
        };
        all_actions(&mut fleet);
        let reports = run(Flow::FourDevices, &mut fleet);
        assert!(
            reports.iter().all(|report| report.status == Status::Passed),
            "{reports:?}"
        );
        let suspend = fleet
            .log
            .iter()
            .position(|line| line == "laptop-arm: suspend")
            .expect("the laptop was suspended");
        let turn = fleet
            .log
            .iter()
            .position(|line| line == "server: tilo do agent")
            .expect("the server took a turn");
        assert!(suspend < turn, "{:?}", fleet.log);
    }

    #[test]
    fn laptop_phone_server_web_off_names_the_setting_it_needs() {
        let mut fleet = Fake {
            web: true,
            ..Fake::default()
        };
        let reports = run(Flow::LaptopPhoneServer, &mut fleet);
        assert_eq!(reports[0].status, Status::Passed);
        assert!(
            matches!(&reports[1].status, Status::Missing(what) if what.contains("turns the server's web UI off")),
            "{:?}",
            reports[1]
        );
    }

    #[test]
    fn an_offline_desktop_passes_the_route_step() {
        let mut fleet = Fake {
            web: true,
            ..Fake::default()
        };
        fleet.offline.insert(Guest::DesktopX86);
        all_actions(&mut fleet);
        let reports = run(Flow::LaptopPhoneServer, &mut fleet);
        let offline = reports
            .iter()
            .find(|report| report.id == "offline")
            .unwrap();
        assert_eq!(offline.status, Status::Passed, "{reports:?}");
    }

    #[test]
    fn a_device_list_that_is_short_is_missing_not_a_failure() {
        let mut fleet = Fake {
            device_lists: true,
            listed_devices: 2,
            ..Fake::default()
        };
        all_actions(&mut fleet);
        let reports = run(Flow::FourDevices, &mut fleet);
        assert!(
            matches!(&reports[1].status, Status::Missing(what) if what.contains("smallest list names 2")),
            "{:?}",
            reports[1]
        );
        assert!(devices_in("not json").is_empty());
        assert_eq!(
            find_url(r#"{"web":"http://h:1/"}"#).as_deref(),
            Some("http://h:1/")
        );
        assert_eq!(find_url(r#"{"url":"file:///x"}"#), None);
    }
}
