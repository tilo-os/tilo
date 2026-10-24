use std::collections::BTreeSet;
use std::os::fd::OwnedFd;

use super::plan::{Command, Program};
use super::record::Record;
use super::{Backend, Caller, Service};
use crate::net::{CLIENT_INTERFACE, EXIT_INTERFACE};
use crate::protocol::{ExitLink, Request, Response, Routes};

#[derive(Default)]
struct Fake {
    commands: Vec<Command>,
    interfaces: BTreeSet<String>,
    sessions: BTreeSet<u32>,
    forwarding: bool,
    saved: Vec<Record>,
    /// Fail the first required command whose arguments contain this.
    fail_on: Option<&'static str>,
}

impl Fake {
    fn lines(&self) -> Vec<String> {
        self.commands
            .iter()
            .map(|command| command.args.join(" "))
            .collect()
    }
}

impl Backend for Fake {
    fn create_tun(&mut self, name: &str) -> Result<OwnedFd, String> {
        self.interfaces.insert(name.to_owned());
        Ok(OwnedFd::from(tempfile::tempfile().unwrap()))
    }

    fn run(&mut self, command: &Command) -> Result<(), String> {
        self.commands.push(command.clone());
        if command.args.first().map(String::as_str) == Some("link")
            && command.args.get(1).map(String::as_str) == Some("delete")
            && let Some(name) = command.args.get(2)
        {
            self.interfaces.remove(name);
        }
        match self.fail_on {
            Some(needle) if command.required && command.args.join(" ").contains(needle) => {
                Err(format!("{needle} failed"))
            }
            _ => Ok(()),
        }
    }

    fn interface_exists(&self, name: &str) -> bool {
        self.interfaces.contains(name)
    }

    fn has_session(&self, uid: u32) -> bool {
        self.sessions.contains(&uid)
    }

    fn forwarding(&self) -> Result<bool, String> {
        Ok(self.forwarding)
    }

    fn set_forwarding(&mut self, on: bool) -> Result<(), String> {
        self.forwarding = on;
        Ok(())
    }

    fn save(&mut self, record: &Record) -> Result<(), String> {
        self.saved.push(record.clone());
        Ok(())
    }
}

const PERSON: Caller = Caller { uid: 1000 };
const OTHER: Caller = Caller { uid: 1001 };

fn service() -> Service<Fake> {
    let mut fake = Fake::default();
    fake.sessions.extend([1000, 1001]);
    Service::new(fake, Record::default())
}

fn open(service: &mut Service<Fake>, caller: Caller, kill_switch: bool) -> Response {
    let link = crate::protocol::ClientLink {
        kill_switch,
        ..crate::protocol::tests::link()
    };
    let (response, fd) = service.handle(caller, Request::OpenClient { link });
    assert_eq!(fd.is_some(), matches!(response, Response::Opened { .. }));
    response
}

#[test]
fn an_opened_client_routes_sets_dns_and_closes_back_to_nothing() {
    let mut service = service();
    assert!(
        matches!(open(&mut service, PERSON, false), Response::Opened { ref interface, mtu: 1280 } if interface == CLIENT_INTERFACE)
    );
    let lines = service.backend().lines();
    assert!(
        lines
            .iter()
            .any(|line| line.ends_with("servers as 1 100.100.64.1"))
    );
    assert!(service.record().dns_set());
    assert!(!service.is_idle());
    let (status, _) = service.handle(PERSON, Request::Status);
    assert!(
        matches!(status, Response::Status { client: Some(ref client), exit: None } if client.interface_up)
    );

    assert_eq!(
        service.handle(PERSON, Request::CloseClient).0,
        Response::Done
    );
    assert!(service.is_idle());
    let lines = service.backend().lines();
    assert!(
        lines
            .last()
            .unwrap()
            .ends_with("GlobalDnsConfiguration a{sv} 0"),
        "DNS put back"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("rule del priority 5263"))
    );
    assert!(!service.backend().interface_exists(CLIENT_INTERFACE));
    assert_eq!(service.backend().saved.last(), Some(&Record::default()));
}

#[test]
fn another_person_cannot_touch_a_tunnel_while_its_owner_is_logged_in() {
    let mut service = service();
    open(&mut service, PERSON, true);
    assert!(matches!(
        open(&mut service, OTHER, false),
        Response::Refused { .. }
    ));
    assert!(matches!(
        service.handle(OTHER, Request::CloseClient).0,
        Response::Refused { .. }
    ));
    assert!(service.record().client.is_some());
    // Once the owner's session is gone, housekeeping takes it down and the
    // other person may open their own.
    service.backend.sessions.remove(&1000);
    service.housekeeping();
    assert!(service.record().client.is_none());
    assert!(matches!(
        open(&mut service, OTHER, false),
        Response::Opened { .. }
    ));
}

#[test]
fn a_request_out_of_bounds_is_refused_before_anything_runs() {
    let mut service = service();
    let link = crate::protocol::ClientLink {
        routes: Routes::Networks {
            networks: vec!["0.0.0.0/0".parse().unwrap()],
        },
        ..crate::protocol::tests::link()
    };
    let (response, fd) = service.handle(PERSON, Request::OpenClient { link });
    assert!(matches!(response, Response::Refused { .. }));
    assert!(fd.is_none());
    assert!(service.backend().commands.is_empty());
    assert!(!service.backend().interface_exists(CLIENT_INTERFACE));
    let (response, _) = service.handle(
        PERSON,
        Request::OpenExit {
            link: ExitLink { mtu: 20 },
        },
    );
    assert!(matches!(response, Response::Refused { .. }));
}

#[test]
fn a_failure_part_way_undoes_what_was_set_up() {
    let mut service = service();
    service.backend.fail_on = Some("priority 5263");
    assert!(matches!(
        open(&mut service, PERSON, true),
        Response::Failed { .. }
    ));
    assert!(service.is_idle());
    let lines = service.backend().lines();
    assert!(
        lines
            .iter()
            .any(|line| line.contains("rule del priority 5260"))
    );
    assert!(
        lines
            .iter()
            .any(|line| line == "delete table inet tilo_device_vpn_client")
    );
    assert!(
        !lines.iter().any(|line| line.contains("servers as 1")),
        "DNS never pointed at a tunnel that failed"
    );
}

#[test]
fn a_dropped_tunnel_is_undone_unless_its_kill_switch_holds_traffic() {
    let mut service = service();
    open(&mut service, PERSON, false);
    service.backend.interfaces.remove(CLIENT_INTERFACE);
    service.housekeeping();
    assert!(
        service.record().client.is_none(),
        "routes and DNS go with the tunnel"
    );

    open(&mut service, PERSON, true);
    service.backend.interfaces.remove(CLIENT_INTERFACE);
    let before = service.backend().commands.len();
    service.housekeeping();
    assert!(service.record().client.is_some(), "the kill switch stays");
    assert_eq!(service.backend().commands.len(), before, "nothing undone");
    let (status, _) = service.handle(PERSON, Request::Status);
    assert!(
        matches!(status, Response::Status { client: Some(ref client), .. } if !client.interface_up)
    );
    // Reconnecting replaces it without a gap: the old rules go, the new
    // kill switch replaces the old one in one transaction.
    open(&mut service, PERSON, true);
    let lines = service.backend().lines();
    let replaced = &lines[before..];
    assert!(
        replaced
            .iter()
            .any(|line| line.contains("rule del priority 5263"))
    );
    assert!(
        !replaced
            .iter()
            .any(|line| line == "delete table inet tilo_device_vpn_client")
    );
    assert!(
        service.backend().commands[before..]
            .iter()
            .any(|command| command.program == Program::Nft && command.input.is_some())
    );
}

#[test]
fn an_exit_turns_forwarding_on_only_while_it_needs_it() {
    let mut service = service();
    let (response, fd) = service.handle(
        PERSON,
        Request::OpenExit {
            link: ExitLink { mtu: 1400 },
        },
    );
    assert!(
        matches!(response, Response::Opened { ref interface, mtu: 1400 } if interface == EXIT_INTERFACE)
    );
    assert!(fd.is_some());
    assert!(service.backend().forwarding);
    let script = service
        .backend()
        .commands
        .iter()
        .find_map(|command| command.input.clone())
        .unwrap();
    assert!(
        script.contains("iifname != \"tiloexit0\" oifname != \"tiloexit0\" drop"),
        "nothing else is forwarded"
    );
    assert_eq!(service.handle(PERSON, Request::CloseExit).0, Response::Done);
    assert!(!service.backend().forwarding);

    // Forwarding someone else turned on stays on, and is not guarded.
    service.backend.forwarding = true;
    service.backend.commands.clear();
    service.handle(
        PERSON,
        Request::OpenExit {
            link: ExitLink { mtu: 1400 },
        },
    );
    let script = service
        .backend()
        .commands
        .iter()
        .find_map(|command| command.input.clone())
        .unwrap();
    assert!(!script.contains("iifname != "));
    service.backend.interfaces.remove(EXIT_INTERFACE);
    service.housekeeping();
    assert!(service.record().exit.is_none(), "a dead exit is undone");
    assert!(service.backend().forwarding);
}

#[test]
fn reset_undoes_what_an_earlier_helper_left() {
    let mut fake = Fake::default();
    fake.sessions.insert(1000);
    let mut first = Service::new(fake, Record::default());
    open(&mut first, PERSON, true);
    let left = first.record().clone();
    let mut fake = Fake::default();
    fake.sessions.insert(1000);
    let mut next = Service::new(fake, left);
    next.reset();
    assert!(next.is_idle());
    assert!(
        next.backend()
            .lines()
            .last()
            .unwrap()
            .ends_with("GlobalDnsConfiguration a{sv} 0")
    );
}
