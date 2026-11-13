//! Which relays a Tilo endpoint uses, and whether number 0's DNS may find its peers.
//!
//! This is the one reading of that setting: [`relays`] is what every endpoint of a person's
//! device binds with (the session's, the filesystem's, a runtime's), so the same setting decides
//! them all.

use iroh::{RelayMap, RelayUrl};
use settings_store::relay::Relays;

/// The relays an endpoint binds with — or, held back, the ones a [`crate::Reachability`] wakes —
/// and whether number 0's DNS resolves the peers it dials.
#[derive(Clone, Debug)]
pub struct EndpointRelays {
    /// The endpoint's relay map: number 0's relays, one relay of the person's own, or none.
    pub relays: RelayMap,
    /// Whether peers are resolved on number 0's DNS and this device published to it. Off with no
    /// relay and with the person's own: nothing then reaches number 0.
    pub n0_dns: bool,
}

#[must_use]
pub fn relays() -> EndpointRelays {
    EndpointRelays::for_choice(&settings_store::relay::relays())
}

impl EndpointRelays {
    #[must_use]
    pub fn for_choice(choice: &Relays) -> Self {
        match choice {
            Relays::N0 => Self {
                relays: iroh::endpoint::default_relay_mode().relay_map(),
                n0_dns: true,
            },
            Relays::None => Self {
                relays: RelayMap::empty(),
                n0_dns: false,
            },
            Relays::Own(url) => match url.parse::<RelayUrl>() {
                Ok(url) => Self {
                    relays: RelayMap::from(url),
                    n0_dns: false,
                },
                // A device that cannot parse the URL it was given still has to reach its peers:
                // it stays on the relays it used before rather than on none.
                Err(error) => {
                    tracing::warn!(%error, relay = %url, "this device's relay setting is not a relay URL; it relays through number 0's relays");
                    Self::for_choice(&Relays::N0)
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use iroh::RelayMap;
    use settings_store::relay::Relays;

    use super::EndpointRelays;

    fn urls(relays: &EndpointRelays) -> Vec<String> {
        relays
            .relays
            .urls::<Vec<_>>()
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    /// Unset, a device binds exactly as it did before the setting existed: number 0's relays and
    /// its DNS.
    #[test]
    fn the_default_is_number_0s_relays_and_dns() {
        let default = EndpointRelays::for_choice(&Relays::N0);
        assert_eq!(
            urls(&default),
            iroh::endpoint::default_relay_mode()
                .relay_map()
                .urls::<Vec<_>>()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            "the relays are number 0's own"
        );
        assert!(!default.relays.is_empty(), "a device relays by default");
        assert!(default.n0_dns, "and is found on number 0's DNS");

        let absent = EndpointRelays::for_choice(&Relays::parse(""));
        assert_eq!(urls(&absent), urls(&default));
    }

    /// One relay of the person's own, and nothing of number 0's: no relay of its in the map, and
    /// no lookup of its either.
    #[test]
    fn the_persons_own_relay_is_the_only_one_and_reaches_no_number_0_dns() {
        let own = EndpointRelays::for_choice(&Relays::Own("https://relay.example.com.".into()));
        assert_eq!(urls(&own), ["https://relay.example.com./"]);
        assert!(!own.n0_dns, "nothing asks number 0's DNS");
    }

    /// An endpoint built from the setting wakes the person's relay, and names it as its home: the
    /// address its peers are given names their relay, not number 0's.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_persons_own_relay_carries_the_endpoint() {
        let relay = tilo_test_env::relay::relay().await;
        let own = relay.relays();
        let url = own.urls::<Vec<_>>().first().cloned().expect("one relay");
        let relays = EndpointRelays::for_choice(&Relays::Own(url.to_string()));
        assert_eq!(
            relays.relays.urls::<Vec<_>>().as_slice(),
            std::slice::from_ref(&url)
        );
        assert!(!relays.n0_dns);

        let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .relay_mode(iroh::RelayMode::Custom(iroh::RelayMap::empty()))
            .bind_addr((std::net::Ipv4Addr::LOCALHOST, 0))
            .expect("a loopback address")
            .bind()
            .await
            .expect("an endpoint for the person's relay");
        let reachability = crate::Reachability::defer(
            &endpoint,
            &relays.relays,
            tokio::runtime::Handle::current(),
            Some(std::time::Duration::from_secs(30)),
        );
        assert!(reachability.is_dormant(), "the relay waits for a reason");
        // A need rather than a bare wake: a wake alone rests the relay after its delay, which a
        // loaded runner's net report can outlast, and an endpoint whose relay rested never picks
        // one as home.
        reachability.need(
            crate::Need::Session,
            Some("a test of the person's own relay"),
        );
        tilo_test_env::within("the endpoint's home relay to be the person's own", async {
            while !endpoint.addr().relay_urls().any(|named| *named == url) {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await;
        endpoint.close().await;
    }

    #[test]
    fn no_relay_leaves_the_map_empty_and_number_0_dns_off() {
        let none = EndpointRelays::for_choice(&Relays::None);
        assert_eq!(none.relays, RelayMap::empty());
        assert!(!none.n0_dns);
    }

    /// A setting nobody can dial through must not leave the device unreachable: it keeps the
    /// relays it had.
    #[test]
    fn a_relay_that_is_not_a_url_leaves_the_device_on_number_0s_relays() {
        let broken = EndpointRelays::for_choice(&Relays::Own("not a relay".into()));
        assert_eq!(
            urls(&broken),
            urls(&EndpointRelays::for_choice(&Relays::N0))
        );
        assert!(broken.n0_dns);
    }

    /// The setting is read where a device's settings are: a person's choice reaches every
    /// endpoint of the device, not only the pane that showed it.
    #[test]
    fn the_persons_choice_is_read_from_this_process_document() {
        let own = vec!["https://relay.example.com./".to_owned()];
        for (stored, expected, n0_dns) in [
            (serde_json::json!(null), None, true),
            (serde_json::json!("none"), Some(Vec::new()), false),
            (
                serde_json::json!("https://relay.example.com."),
                Some(own),
                false,
            ),
        ] {
            let mut document = serde_json::Map::new();
            if !stored.is_null() {
                document.insert("network".into(), serde_json::json!({ "relay": stored }));
            }
            let settings = settings_store::HostDocument::new(document, |_| Ok(()));
            let _scoped = settings_store::scoped_source(Some(std::sync::Arc::new(settings)));
            let relays = super::relays();
            assert_eq!(
                urls(&relays),
                expected.unwrap_or_else(|| urls(&EndpointRelays::for_choice(&Relays::N0))),
                "an unset setting is number 0's relays, unchanged"
            );
            assert_eq!(relays.n0_dns, n0_dns);
        }
    }
}
