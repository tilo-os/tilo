//! Where the worker may connect: Discord's API host, its gateway hosts and
//! its CDN, and nothing else (discord-bridge.md §2, "Outside worker").
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
