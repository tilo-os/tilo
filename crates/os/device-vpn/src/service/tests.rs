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

/// A carrier with more than one socket and relay, so a plan and its kill
/// switch are seen with both address families.
fn rich_link(kill_switch: bool) -> crate::protocol::ClientLink {
    crate::protocol::ClientLink {
        routes: Routes::All,
        kill_switch,
        carrier: crate::protocol::Carrier {
            udp_ports: vec![40_000, 40_001],
            relays: vec![
                "203.0.113.7".parse().unwrap(),
                "2001:db8::7".parse().unwrap(),
            ],
        },
        ..crate::protocol::tests::link()
    }
}

tilo_test_env::scenario! {
    fn the_service_opens_guards_and_undoes_tunnels_for_their_owner;

    phase an_opened_client_routes_sets_dns_and_closes_back_to_nothing;
    {
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
            matches!(status, Response::Status { client: Some(ref client), exit: None, profile_block: None } if client.interface_up)
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

    phase every_plan_guards_only_its_owner_and_lets_through_only_what_it_names;
    // The nftables a plan writes: the rules that keep one person's traffic
    // theirs, the kill switch that holds the rest, the exit that forwards only
    // its tunnel, and a profile block that lets through only what it names.
    // What real traffic does under them is the namespaces check's.
    {
        let mut service = service();
        let (_, fd) = service.handle(
            PERSON,
            Request::OpenClient {
                link: rich_link(true),
            },
        );
        assert!(fd.is_some());
        let lines = service.backend().lines();
        assert!(
            lines
                .iter()
                .filter(|line| line.contains("rule add"))
                .all(|line| line.contains("uidrange 1000-1000")),
            "no rule reaches another person's traffic"
        );
        let script = service
            .backend()
            .commands
            .iter()
            .find_map(|command| command.input.clone())
            .expect("an nft script");
        for line in [
            "meta skuid 1000 jump owner",
            "oifname \"tilovpn0\" return",
            "udp sport { 40000, 40001 } return",
            "ip daddr { 203.0.113.7 } return",
            "ip6 daddr { 2001:db8::7 } return",
        ] {
            assert!(script.contains(line), "missing {line}");
        }
        assert!(script.contains("ip daddr { 10.0.0.0/8"));
        assert!(script.contains("ip6 daddr { fe80::/10"));
        assert_eq!(
            service.handle(PERSON, Request::CloseClient).0,
            Response::Done
        );

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
            .rev()
            .find_map(|command| command.input.clone())
            .expect("an exit script");
        for line in [
            "ip saddr 100.100.64.0/24 oifname != \"tiloexit0\" masquerade",
            "iifname \"tiloexit0\" ip saddr != 100.100.64.0/24 drop",
            "oifname \"tiloexit0\" ct state != { established, related } drop",
            "iifname != \"tiloexit0\" oifname != \"tiloexit0\" drop",
        ] {
            assert!(script.contains(line), "missing {line}");
        }
        service.handle(PERSON, Request::CloseExit);

        assert_eq!(hold(&mut service, PERSON, false), Response::Done);
        let script = service
            .backend()
            .commands
            .iter()
            .rev()
            .find_map(|command| command.input.clone())
            .expect("a profile block script");
        for allowed in [
            "oifname \"lo\" return",
            "meta oifkind { \"tun\", \"wireguard\", \"ppp\" } return",
            "rt ipsec exists return",
            "udp sport 68 udp dport 67 return",
            "udp sport 546 udp dport 547 return",
            "nd-neighbor-solicit",
            "ip daddr { 198.51.100.7 } return",
            "ip6 daddr { 2001:db8::7 } return",
        ] {
            assert!(script.contains(allowed), "missing {allowed}");
        }
        assert!(
            script
                .trim_end()
                .ends_with("reject with icmpx admin-prohibited\n  }\n}"),
            "what nothing let through is refused"
        );
        assert!(!script.contains("skuid"), "a block is not the kill switch");

        hold(&mut service, PERSON, true);
        let script = service
            .backend()
            .commands
            .iter()
            .rev()
            .find_map(|command| command.input.clone())
            .expect("a profile block script");
        assert!(script.contains("ip daddr { 10.0.0.0/8"));
        assert!(script.contains("ip6 daddr { fe80::/10"));
        service.handle(PERSON, Request::ReleaseProfileBlock);

        let before = service.backend().commands.len();
        let (response, fd) = service.handle(
            PERSON,
            Request::OpenClient {
                link: crate::protocol::ClientLink {
                    routes: Routes::Networks {
                        networks: vec![
                            "192.168.1.0/24".parse().unwrap(),
                            "10.20.0.0/16".parse().unwrap(),
                        ],
                    },
                    ..crate::protocol::tests::link()
                },
            },
        );
        assert!(matches!(response, Response::Opened { .. }) && fd.is_some());
        let routes: Vec<String> = service.backend().commands[before..]
            .iter()
            .map(|command| command.args.join(" "))
            .collect();
        assert!(
            routes.contains(&"-4 route replace 192.168.1.0/24 dev tilovpn0 table 5270".to_owned())
        );
        assert!(
            routes.contains(&"-4 route replace 10.20.0.0/16 dev tilovpn0 table 5270".to_owned())
        );
        assert!(
            !routes.iter().any(|line| line.contains("default")),
            "only the home networks are routed"
        );
        assert!(
            !routes
                .iter()
                .any(|line| line.starts_with("-6 rule add priority 5263")),
            "IPv6 is left alone"
        );
    }

    phase another_person_cannot_touch_a_tunnel_while_its_owner_is_logged_in;
    {
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

    phase a_request_out_of_bounds_is_refused_before_anything_runs;
    {
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

    phase a_failure_part_way_undoes_what_was_set_up;
    {
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

    phase a_dropped_tunnel_is_undone_unless_its_kill_switch_holds_traffic;
    {
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

    phase an_exit_turns_forwarding_on_only_while_it_needs_it;
    {
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

    phase reset_undoes_what_an_earlier_helper_left;
    {
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
}

fn hold(service: &mut Service<Fake>, caller: Caller, allow_local: bool) -> Response {
    let block = crate::protocol::ProfileBlock {
        allow_local,
        ..crate::protocol::tests::block()
    };
    let (response, fd) = service.handle(caller, Request::HoldProfileBlock { block });
    assert!(fd.is_none(), "a block hands out no descriptor");
    response
}

fn profile_table_lines(service: &Service<Fake>) -> Vec<String> {
    service
        .backend()
        .lines()
        .into_iter()
        .filter(|line| line.contains("tilo_device_vpn_profile") || line == "-f -")
        .collect()
}

tilo_test_env::scenario! {
    fn the_service_holds_a_profile_block_for_its_owner_until_lifted;

    phase a_profile_block_is_held_replaced_and_lifted;
    {
        let mut service = service();
        assert_eq!(hold(&mut service, PERSON, false), Response::Done);
        assert!(service.is_idle(), "a block needs no running helper");
        let script = service
            .backend()
            .commands
            .iter()
            .find_map(|command| command.input.clone())
            .unwrap();
        assert!(script.contains("table inet tilo_device_vpn_profile {"));
        let (status, _) = service.handle(PERSON, Request::Status);
        assert!(matches!(
            status,
            Response::Status { profile_block: Some(ref block), .. } if !block.allow_local
        ));

        // Allowing the local network replaces the block in one transaction:
        // nothing lifts it in between.
        let before = service.backend().commands.len();
        assert_eq!(hold(&mut service, PERSON, true), Response::Done);
        assert!(
            !service.backend().lines()[before..]
                .iter()
                .any(|line| line == "delete table inet tilo_device_vpn_profile")
        );
        assert!(
            service
                .record()
                .profile_block
                .as_ref()
                .unwrap()
                .block
                .allow_local
        );

        assert_eq!(
            service.handle(PERSON, Request::ReleaseProfileBlock).0,
            Response::Done
        );
        assert!(service.is_idle());
        assert_eq!(
            service.backend().lines().last().map(String::as_str),
            Some("delete table inet tilo_device_vpn_profile")
        );
        assert_eq!(service.backend().saved.last(), Some(&Record::default()));
    }

    phase another_person_cannot_lift_or_replace_a_profile_block_while_its_owner_is_logged_in;
    {
        let mut service = service();
        hold(&mut service, PERSON, false);
        let before = profile_table_lines(&service).len();
        assert!(matches!(
            hold(&mut service, OTHER, true),
            Response::Refused { .. }
        ));
        assert!(matches!(
            service.handle(OTHER, Request::ReleaseProfileBlock).0,
            Response::Refused { .. }
        ));
        assert_eq!(profile_table_lines(&service).len(), before, "nothing ran");
        assert_eq!(service.record().profile_block.as_ref().unwrap().owner, 1000);
    }

    phase a_profile_block_out_of_bounds_is_refused_before_anything_runs;
    {
        let mut service = service();
        let block = crate::protocol::ProfileBlock {
            servers: vec!["127.0.0.1".parse().unwrap()],
            allow_local: false,
        };
        let (response, _) = service.handle(PERSON, Request::HoldProfileBlock { block });
        assert!(matches!(response, Response::Refused { .. }));
        assert!(service.backend().commands.is_empty());
        assert!(service.is_idle());
    }

    phase a_profile_block_nftables_refuses_is_not_left_recorded;
    {
        let mut service = service();
        service.backend.fail_on = Some("-f -");
        assert!(matches!(
            hold(&mut service, PERSON, false),
            Response::Failed { .. }
        ));
        assert!(service.is_idle());
        assert_eq!(
            service.backend().lines().last().map(String::as_str),
            Some("delete table inet tilo_device_vpn_profile")
        );
    }

    phase a_profile_block_outlasts_its_owners_session_until_it_is_lifted;
    {
        let mut service = service();
        hold(&mut service, PERSON, false);
        let ran = service.backend().commands.len();
        // The owner's shell crashed and took the session with it.
        service.backend.sessions.remove(&1000);
        service.housekeeping();
        assert!(service.record().profile_block.is_some());
        assert_eq!(service.backend().commands.len(), ran, "nothing ran");
        assert!(service.is_idle(), "the block needs no running helper");
        // Logged out, the owner no longer holds it against the person at the
        // seat.
        assert!(matches!(
            service.handle(OTHER, Request::ReleaseProfileBlock).0,
            Response::Done
        ));
        assert!(service.record().profile_block.is_none());
        assert_eq!(
            service.backend().lines().last().map(String::as_str),
            Some("delete table inet tilo_device_vpn_profile")
        );
    }

    phase reset_lifts_a_profile_block_an_earlier_helper_left;
    {
        let mut fake = Fake::default();
        fake.sessions.insert(1000);
        let mut first = Service::new(fake, Record::default());
        hold(&mut first, PERSON, false);
        let left = first.record().clone();
        let mut fake = Fake::default();
        fake.sessions.insert(1000);
        let mut next = Service::new(fake, left);
        next.reset();
        assert!(next.is_idle());
        assert_eq!(
            next.backend().lines(),
            ["delete table inet tilo_device_vpn_profile"]
        );
    }
}

tilo_test_env::scenario! {
    fn the_socket_loop_answers_with_descriptors_and_stops_promptly;

    phase requests_carry_descriptors_and_refuse_bad_callers_or_envelopes;
    {
        use std::io::{BufRead as _, BufReader, Write as _};
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{Arc, Mutex};
        use std::time::{Duration, Instant};

        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("control.sock");
        let listener = tilo_files::native::socket::bind(&socket).unwrap();
        let allow = Arc::new(Mutex::new(Ok(PERSON)));
        let stop = Arc::new(AtomicBool::new(false));
        let mut service = service();

        let thread = std::thread::spawn({
            let allow = Arc::clone(&allow);
            let stop = Arc::clone(&stop);
            move || {
                super::server::serve(
                    &listener,
                    &mut service,
                    |_| allow.lock().unwrap().clone(),
                    super::server::Pacing {
                        // Far longer than the 500 ms poll ceiling so a stop
                        // cannot hide behind a short housekeeping period.
                        housekeeping: Duration::from_secs(10),
                        idle_exit: None,
                    },
                    &stop,
                );
            }
        });

        let bound = tilo_test_env::hang_bound();
        let (response, fd) = crate::protocol::call(
            &socket,
            &Request::OpenClient {
                link: crate::protocol::tests::link(),
            },
            bound,
        )
        .unwrap();
        assert!(matches!(response, Response::Opened { .. }));
        assert!(fd.is_some(), "opening a tunnel returns its descriptor");

        let (response, fd) = crate::protocol::call(&socket, &Request::CloseClient, bound).unwrap();
        assert_eq!(response, Response::Done);
        assert!(fd.is_none());

        // Unauthorized caller is refused before sending a request byte. Because
        // the server refuses without reading and closes, writing the request
        // can race with the close and see a broken pipe; ignore the write
        // result and read the refusal line directly.
        *allow.lock().unwrap() = Err("not the seat shell".into());
        {
            let mut stream = tilo_files::native::socket::connect(&socket).unwrap();
            stream.set_read_timeout(Some(bound)).unwrap();
            stream.set_write_timeout(Some(bound)).unwrap();
            let mut line = serde_json::to_vec(&crate::protocol::Envelope {
                protocol: crate::protocol::PROTOCOL_VERSION,
                request: Request::Status,
            })
            .unwrap();
            line.push(b'\n');
            let _ = stream.write_all(&line);
            let (answer, fd) =
                crate::fdpass::read_line_with_fd(&stream, crate::protocol::MAX_LINE_BYTES).unwrap();
            let response: Response = serde_json::from_slice(&answer).unwrap();
            assert_eq!(
                response,
                Response::Refused {
                    reason: "not the seat shell".into()
                }
            );
            assert!(fd.is_none());
        }

        // Wrong protocol version and malformed JSON are refused.
        *allow.lock().unwrap() = Ok(PERSON);
        for (raw, expected) in [
            (
                r#"{"protocol":99,"request":{"op":"status"}}"#,
                "unsupported protocol version",
            ),
            ("{not json", "malformed request"),
        ] {
            let mut stream = tilo_files::native::socket::connect(&socket).unwrap();
            stream.set_read_timeout(Some(bound)).unwrap();
            stream.set_write_timeout(Some(bound)).unwrap();
            stream.write_all(raw.as_bytes()).unwrap();
            stream.write_all(b"\n").unwrap();
            let mut line = String::new();
            BufReader::new(&stream).read_line(&mut line).unwrap();
            assert_eq!(
                serde_json::from_str::<Response>(&line).unwrap(),
                Response::Refused {
                    reason: expected.into()
                }
            );
        }

        // Stopping must wake within the 500 ms poll ceiling rather than waiting
        // out the 10 s housekeeping interval.
        let stopping = Instant::now();
        stop.store(true, Ordering::Release);
        thread.join().unwrap();
        assert!(
            stopping.elapsed() < Duration::from_secs(2),
            "server took {:?} to stop with a 10 s housekeeping interval",
            stopping.elapsed()
        );
    }
}
