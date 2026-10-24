//! Where the worker may connect: Discord's API host, its gateway hosts, its
//! CDN and its voice servers, and nothing else (discord-bridge.md §2,
//! "Outside worker").
//!
//! The sandbox pins the worker's egress to the same hosts; this check is the
//! worker's own copy of the rule, so a URL that reaches it from Discord (a
//! `resume_gateway_url`, an attachment URL in an op) cannot point it
//! anywhere else even where no sandbox runs it. Loopback is allowed only
//! when the configuration says so, for tests against a fake Discord.

use url::{Host, Url};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Purpose {
    /// REST calls: `https://discord.com`.
    Api,
    /// The gateway: `wss://gateway.discord.gg` and the regional
    /// `wss://*.discord.gg` hosts `/gateway/bot` and `READY` name.
    Gateway,
    /// Downloads for `fetch-asset`: `https://cdn.discordapp.com`, and
    /// `https://media.discordapp.net`, which serves the same files resized.
    Cdn,
    /// A voice server's WebSocket: `wss://*.discord.media`, the endpoint
    /// `VOICE_SERVER_UPDATE` names. Its UDP media goes only to the address
    /// that socket's Ready names (see [`Egress::check_voice_udp`]).
    Voice,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Egress {
    pub allow_loopback: bool,
}

impl Egress {
    /// Whether the worker may connect to `url` for `purpose`.
    ///
    /// # Errors
    /// Why not, naming the host but never the path (a webhook URL carries
    /// its token in the path).
    pub fn check(self, url: &Url, purpose: Purpose) -> Result<(), String> {
        let host = url.host_str().unwrap_or_default();
        if !url.username().is_empty() || url.password().is_some() {
            return Err(format!("{host}: credentials in a URL are not allowed"));
        }
        if self.allow_loopback && is_loopback(url) {
            return if matches!(url.scheme(), "http" | "https" | "ws" | "wss") {
                Ok(())
            } else {
                Err(format!("{host}: unsupported scheme {}", url.scheme()))
            };
        }
        if url.port().is_some() {
            return Err(format!("{host}: Discord is reached on its default port"));
        }
        let (scheme, allowed) = match purpose {
            Purpose::Api => ("https", host == "discord.com"),
            Purpose::Gateway => (
                "wss",
                host == "gateway.discord.gg" || is_subdomain(host, "discord.gg"),
            ),
            Purpose::Cdn => (
                "https",
                host == "cdn.discordapp.com" || host == "media.discordapp.net",
            ),
            Purpose::Voice => ("wss", is_subdomain(host, "discord.media")),
        };
        if !allowed {
            return Err(format!("{host} is not one of Discord's {purpose:?} hosts"));
        }
        if url.scheme() != scheme {
            return Err(format!("{host}: only {scheme} is allowed"));
        }
        Ok(())
    }
}

impl Egress {
    /// Whether the worker may send voice over UDP to `ip`, the address a
    /// voice server's Ready named: a public address, as Discord's voice
    /// servers have, or loopback when the configuration allows it. The
    /// voice server itself was checked against [`Purpose::Voice`] first.
    ///
    /// # Errors
    /// Why not.
    pub fn check_voice_udp(self, ip: std::net::IpAddr) -> Result<(), String> {
        if ip.is_loopback() {
            return if self.allow_loopback {
                Ok(())
            } else {
                Err(format!("{ip}: a voice server on loopback is not Discord"))
            };
        }
        let private = match ip {
            std::net::IpAddr::V4(v4) => {
                v4.is_private()
                    || v4.is_link_local()
                    || v4.is_unspecified()
                    || v4.is_broadcast()
                    || v4.is_multicast()
                    || v4.is_documentation()
            }
            std::net::IpAddr::V6(v6) => {
                v6.is_unspecified()
                    || v6.is_multicast()
                    || v6.is_unique_local()
                    || v6.is_unicast_link_local()
            }
        };
        if private {
            Err(format!(
                "{ip} is not a public address, as Discord's voice servers have"
            ))
        } else {
            Ok(())
        }
    }
}

fn is_subdomain(host: &str, parent: &str) -> bool {
    host.strip_suffix(parent)
        .and_then(|label| label.strip_suffix('.'))
        .is_some_and(|label| {
            !label.is_empty()
                && label.split('.').all(|part| {
                    !part.is_empty() && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                })
        })
}

fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(Host::Ipv4(ip)) => ip.is_loopback(),
        Some(Host::Ipv6(ip)) => ip.is_loopback(),
        Some(Host::Domain(name)) => name == "localhost",
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(url: &str, purpose: Purpose, allow_loopback: bool) -> bool {
        Egress { allow_loopback }
            .check(&Url::parse(url).unwrap(), purpose)
            .is_ok()
    }

    #[test]
    fn only_discord_hosts_pass() {
        assert!(check(
            "https://discord.com/api/v10/gateway/bot",
            Purpose::Api,
            false
        ));
        assert!(check(
            "wss://gateway.discord.gg/?v=10",
            Purpose::Gateway,
            false
        ));
        assert!(check(
            "wss://gateway-us-east1-b.discord.gg",
            Purpose::Gateway,
            false
        ));
        assert!(check(
            "https://cdn.discordapp.com/avatars/1/a.png",
            Purpose::Cdn,
            false
        ));
        assert!(check(
            "https://media.discordapp.net/attachments/1/2/a.png",
            Purpose::Cdn,
            false
        ));

        assert!(!check(
            "https://discord.com.evil.example/",
            Purpose::Api,
            false
        ));
        assert!(!check(
            "https://evil.example/discord.com",
            Purpose::Api,
            false
        ));
        assert!(
            !check("http://discord.com/api/v10", Purpose::Api, false),
            "plain http"
        );
        assert!(
            !check("https://discord.com:8443/api", Purpose::Api, false),
            "another port"
        );
        assert!(!check(
            "https://user:pw@discord.com/api",
            Purpose::Api,
            false
        ));
        assert!(
            !check("wss://discord.gg", Purpose::Gateway, false),
            "the bare invite domain"
        );
        assert!(!check("wss://evildiscord.gg", Purpose::Gateway, false));
        assert!(
            !check("https://cdn.discordapp.com/x", Purpose::Api, false),
            "CDN is not the API"
        );
        assert!(
            !check("https://discord.com/api/v10/x", Purpose::Cdn, false),
            "API is not the CDN"
        );
    }

    #[test]
    fn voice_goes_to_discord_media_only() {
        assert!(check(
            "wss://c-ams12-3a4b.discord.media/?v=8",
            Purpose::Voice,
            false
        ));
        assert!(
            check(
                "wss://c-ams12-3a4b.discord.media:443/?v=8",
                Purpose::Voice,
                false
            ),
            "the default port written out"
        );
        assert!(!check("wss://discord.media/?v=8", Purpose::Voice, false));
        assert!(!check("wss://evil.example/?v=8", Purpose::Voice, false));
        assert!(!check("ws://x.discord.media/?v=8", Purpose::Voice, false));
        assert!(!check("wss://x.discord.media:2053/", Purpose::Voice, false));
        assert!(
            !check("wss://gateway.discord.gg", Purpose::Voice, false),
            "the gateway is not a voice server"
        );
        assert!(check("ws://127.0.0.1:4001/voice?v=8", Purpose::Voice, true));

        let strict = Egress::default();
        assert!(strict.check_voice_udp([162, 159, 128, 233].into()).is_ok());
        assert!(strict.check_voice_udp([127, 0, 0, 1].into()).is_err());
        assert!(strict.check_voice_udp([10, 0, 0, 1].into()).is_err());
        assert!(strict.check_voice_udp([192, 168, 1, 1].into()).is_err());
        let fake = Egress {
            allow_loopback: true,
        };
        assert!(fake.check_voice_udp([127, 0, 0, 1].into()).is_ok());
    }

    #[test]
    fn loopback_needs_the_override() {
        assert!(!check("http://127.0.0.1:4000/api/v10", Purpose::Api, false));
        assert!(!check("ws://localhost:4001", Purpose::Gateway, false));
        assert!(check("http://127.0.0.1:4000/api/v10", Purpose::Api, true));
        assert!(check("ws://localhost:4001", Purpose::Gateway, true));
        assert!(check("http://[::1]:4000/cdn/x", Purpose::Cdn, true));
        assert!(
            !check("http://10.0.0.1:4000/api", Purpose::Api, true),
            "only loopback"
        );
    }
}
