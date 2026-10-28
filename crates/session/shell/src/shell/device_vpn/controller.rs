//! What this device does about its route through another of the person's
//! devices, decided without doing it: the driver in [`super`] dials, asks
//! the helper and waits, and asks here what each outcome means.
//!
//! The kill switch is the one subtle part. Once the tunnel has been up, the
//! helper's link (routes, and the kill switch's block of everything else)
//! stays while the tunnel is down and the person still wants it, so with the
//! kill switch on nothing leaves directly; without it the link goes, and
//! traffic goes out as usual until the tunnel is back. The person turning
//! the route off, or the other device stopping it, always takes the link
//! away.

use std::time::Duration;

use device_vpn::net::Ipv4Net;
use device_vpn::protocol::{Carrier, ClientLink, Routes as LinkRoutes};
use device_vpn::tunnel::{ClientError, Ended, Offer, Refusal};
use settings_store::device_vpn::{ClientState, Refusal as ShownRefusal, Routes, Through};

/// The longest wait between two attempts to reach the other device.
pub(crate) const MAX_RETRY: Duration = Duration::from_secs(60);
/// The wait after the other device said no for a reason that may change by
/// itself (its switch, its room for devices).
pub(crate) const REFUSED_RETRY: Duration = Duration::from_secs(30);

/// The route the person wants, as the driver follows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Want {
    pub device: [u8; 32],
    pub routes: Routes,
    pub dns: bool,
    pub kill_switch: bool,
}

impl Want {
    /// The route `through` names, if it names a device by a well-formed id.
    pub(crate) fn of(through: &Through) -> Option<Self> {
        let mut device = [0_u8; 32];
        hex::decode_to_slice(through.device.trim(), &mut device).ok()?;
        Some(Self {
            device,
            routes: through.routes,
            dns: through.dns,
            kill_switch: through.kill_switch,
        })
    }
}

/// What to do once a tunnel ended or could not be opened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AfterEnd {
    /// Keep the helper's link: with the kill switch, it blocks traffic until
    /// the tunnel is back.
    pub keep_link: bool,
    /// Clear the person's choice: the other device stopped this one.
    pub clear_through: bool,
    /// Try again after this long; `None` waits for the person.
    pub retry: Option<Duration>,
    /// What Settings shows meanwhile.
    pub state: ClientState,
}

/// The wait before attempt `attempt` (from one) after a lost tunnel or a
/// failed dial: one second, doubling, at most [`MAX_RETRY`].
pub(crate) fn backoff(attempt: u32) -> Duration {
    let doublings = attempt.saturating_sub(1).min(6);
    Duration::from_secs(1_u64 << doublings).min(MAX_RETRY)
}

/// What a refusal from the other device means for this one.
fn after_refusal(refusal: Refusal, link_held: bool, kill_switch: bool) -> AfterEnd {
    let blocking = link_held && kill_switch;
    let shown = match refusal {
        // The person stopped this device from the other one: stop here too.
        Refusal::Stopped => {
            return AfterEnd {
                keep_link: false,
                clear_through: true,
                retry: None,
                state: ClientState::Connecting,
            };
        }
        Refusal::NotYours => ShownRefusal::NotYours,
        Refusal::ExitOff => ShownRefusal::ExitOff,
        Refusal::Full | Refusal::Protocol => ShownRefusal::Full,
        // A newer connection of this device took over: nothing to show, the
        // newer one carries on.
        Refusal::Replaced => {
            return AfterEnd {
                keep_link: link_held,
                clear_through: false,
                retry: Some(backoff(1)),
                state: ClientState::Reconnecting { blocking },
            };
        }
    };
    AfterEnd {
        keep_link: blocking,
        clear_through: false,
        retry: Some(REFUSED_RETRY),
        state: ClientState::Refused { reason: shown },
    }
}

/// What to do after a running tunnel ended. `attempt` counts the tries since
/// it was last up, from one.
pub(crate) fn after_end(ended: &Ended, kill_switch: bool, attempt: u32) -> AfterEnd {
    match ended {
        Ended::Refused(refusal) => after_refusal(*refusal, true, kill_switch),
        // This side hung up: the person changed or stopped the route, and the
        // driver follows the new choice.
        Ended::Closed => AfterEnd {
            keep_link: kill_switch,
            clear_through: false,
            retry: Some(Duration::ZERO),
            state: ClientState::Reconnecting {
                blocking: kill_switch,
            },
        },
        Ended::Lost(_) => AfterEnd {
            keep_link: kill_switch,
            clear_through: false,
            retry: Some(backoff(attempt)),
            state: ClientState::Reconnecting {
                blocking: kill_switch,
            },
        },
    }
}

/// What to do after dialling failed. `link_held` says whether a link from an
/// earlier tunnel is still up.
pub(crate) fn after_failed_dial(
    error: &ClientError,
    link_held: bool,
    kill_switch: bool,
    attempt: u32,
) -> AfterEnd {
    match error {
        ClientError::Refused(refusal) => after_refusal(*refusal, link_held, kill_switch),
        ClientError::Unreachable(_) => {
            let blocking = link_held && kill_switch;
            AfterEnd {
                keep_link: blocking,
                clear_through: false,
                retry: Some(backoff(attempt)),
                state: if link_held {
                    ClientState::Reconnecting { blocking }
                } else {
                    ClientState::Connecting
                },
            }
        }
    }
}

/// Whether a held link must go when the person's choice becomes `want`:
/// when they turned the route off, or chose another device. A change of
/// routes, DNS or kill switch replaces the link when the tunnel is next up.
pub(crate) fn link_must_go(held_for: Option<&Want>, want: Option<&Want>) -> bool {
    match (held_for, want) {
        (None, _) => false,
        (Some(_), None) => true,
        (Some(held), Some(want)) => held.device != want.device,
    }
}

/// The link to ask the helper for, for a tunnel through the exit that made
/// `offer`.
pub(crate) fn link_for(
    want: &Want,
    address: std::net::Ipv4Addr,
    mtu: u16,
    offer: &Offer,
    carrier: Carrier,
) -> Result<ClientLink, String> {
    let routes = match want.routes {
        Routes::All => LinkRoutes::All,
        Routes::HomeNetwork => {
            let networks: Vec<Ipv4Net> = offer
                .home_networks
                .iter()
                .copied()
                .filter(|network| network.is_routable_home())
                .take(device_vpn::protocol::MAX_NETWORKS)
                .collect();
            if networks.is_empty() {
                return Err("the other device is on no network to reach".into());
            }
            LinkRoutes::Networks { networks }
        }
    };
    let link = ClientLink {
        address,
        mtu,
        routes,
        dns: want.dns,
        kill_switch: want.kill_switch,
        carrier,
    };
    link.check()?;
    Ok(link)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn want(kill_switch: bool) -> Want {
        Want {
            device: [7; 32],
            routes: Routes::All,
            dns: true,
            kill_switch,
        }
    }

    #[test]
    fn a_lost_tunnel_keeps_blocking_with_the_kill_switch() {
        let after = after_end(&Ended::Lost("path".into()), true, 1);
        assert!(after.keep_link, "the link and its block stay");
        assert_eq!(after.state, ClientState::Reconnecting { blocking: true });
        assert_eq!(after.retry, Some(Duration::from_secs(1)));
        assert!(!after.clear_through);
    }

    #[test]
    fn a_lost_tunnel_lets_traffic_out_without_the_kill_switch() {
        let after = after_end(&Ended::Lost("path".into()), false, 1);
        assert!(!after.keep_link, "the link goes, traffic goes out directly");
        assert_eq!(after.state, ClientState::Reconnecting { blocking: false });
    }

    #[test]
    fn redialling_keeps_blocking_until_the_tunnel_is_back() {
        let unreachable = ClientError::Unreachable("no path".into());
        let after = after_failed_dial(&unreachable, true, true, 3);
        assert!(after.keep_link);
        assert_eq!(after.state, ClientState::Reconnecting { blocking: true });
        assert_eq!(after.retry, Some(Duration::from_secs(4)));
        // Before the tunnel was ever up there is no link to keep.
        let first = after_failed_dial(&unreachable, false, true, 1);
        assert!(!first.keep_link);
        assert_eq!(first.state, ClientState::Connecting);
    }

    #[test]
    fn being_stopped_from_the_other_device_releases_the_block_and_the_choice() {
        for kill_switch in [false, true] {
            let after = after_end(&Ended::Refused(Refusal::Stopped), kill_switch, 1);
            assert!(!after.keep_link);
            assert!(after.clear_through);
            assert_eq!(after.retry, None);
        }
    }

    #[test]
    fn the_exit_turned_off_keeps_the_block_and_says_why() {
        let after = after_end(&Ended::Refused(Refusal::ExitOff), true, 1);
        assert!(after.keep_link);
        assert_eq!(
            after.state,
            ClientState::Refused {
                reason: ShownRefusal::ExitOff
            }
        );
        assert_eq!(after.retry, Some(REFUSED_RETRY));
        let open = after_end(&Ended::Refused(Refusal::ExitOff), false, 1);
        assert!(!open.keep_link);
    }

    #[test]
    fn another_persons_device_is_shown_as_such() {
        let after = after_failed_dial(&ClientError::Refused(Refusal::NotYours), false, false, 1);
        assert_eq!(
            after.state,
            ClientState::Refused {
                reason: ShownRefusal::NotYours
            }
        );
        assert!(!after.clear_through);
    }

    #[test]
    fn turning_the_route_off_or_choosing_another_device_drops_the_link() {
        assert!(link_must_go(Some(&want(true)), None));
        let elsewhere = Want {
            device: [8; 32],
            ..want(true)
        };
        assert!(link_must_go(Some(&want(true)), Some(&elsewhere)));
        // Changing the kill switch keeps blocking until the new link is up.
        assert!(!link_must_go(Some(&want(true)), Some(&want(false))));
        assert!(!link_must_go(None, None));
    }

    #[test]
    fn retries_back_off_to_a_minute() {
        assert_eq!(backoff(1), Duration::from_secs(1));
        assert_eq!(backoff(2), Duration::from_secs(2));
        assert_eq!(backoff(7), Duration::from_secs(60));
        assert_eq!(backoff(u32::MAX), MAX_RETRY);
    }

    #[test]
    fn home_network_routes_take_what_the_exit_offers() {
        let offer = Offer {
            gateway: device_vpn::net::GATEWAY,
            address: Some("100.100.64.2".parse().unwrap()),
            mtu: 1400,
            home_networks: vec!["192.168.1.0/24".parse().unwrap()],
        };
        let carrier = Carrier {
            udp_ports: vec![40_000],
            relays: vec![],
        };
        let home = Want {
            routes: Routes::HomeNetwork,
            ..want(false)
        };
        let link = link_for(
            &home,
            "100.100.64.2".parse().unwrap(),
            1280,
            &offer,
            carrier.clone(),
        )
        .unwrap();
        assert_eq!(
            link.routes,
            LinkRoutes::Networks {
                networks: vec!["192.168.1.0/24".parse().unwrap()]
            }
        );
        let bare = Offer {
            home_networks: vec![],
            ..offer
        };
        assert!(link_for(&home, "100.100.64.2".parse().unwrap(), 1280, &bare, carrier).is_err());
    }

    #[test]
    fn a_choice_names_a_device_by_its_full_id() {
        let through = Through::device(hex::encode([9_u8; 32]));
        assert_eq!(Want::of(&through).map(|want| want.device), Some([9; 32]));
        assert_eq!(Want::of(&Through::device("abc")), None);
    }
}
