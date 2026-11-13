//! The worker's settings, from the environment the host starts it with.
//! None of them is a secret: the bot token and the webhook tokens come only
//! as protocol `Secret` messages.

use url::Url;

use crate::egress::{Egress, Purpose};

pub const API_BASE: &str = "https://discord.com/api/v10";
pub const CDN_BASE: &str = "https://cdn.discordapp.com";

/// `DISCORD_API_BASE`: the REST base, default [`API_BASE`].
pub const ENV_API_BASE: &str = "DISCORD_API_BASE";
/// `DISCORD_CDN_BASE`: where avatar URLs point, default [`CDN_BASE`].
pub const ENV_CDN_BASE: &str = "DISCORD_CDN_BASE";
/// `DISCORD_ALLOW_LOOPBACK=1`: let the bases above, the gateway and
/// downloads be loopback URLs, for a fake Discord in tests.
pub const ENV_ALLOW_LOOPBACK: &str = "DISCORD_ALLOW_LOOPBACK";
/// `DISCORD_READ_ONLY=1`: receive events but change nothing in Discord.
/// Every op other than `fetch-asset` is refused, slash commands are not
/// registered and interactions are not answered.
pub const ENV_READ_ONLY: &str = "DISCORD_READ_ONLY";
/// `DISCORD_GUILD_ID`: the guild to bridge; default the only guild the
/// bot is in.
pub const ENV_GUILD_ID: &str = "DISCORD_GUILD_ID";
/// `DISCORD_CHANNEL_ID`: bridge only this channel and the threads in it.
/// Events elsewhere are not passed on, ops elsewhere or on the server
/// itself (layout, roles, permissions, members) are refused, and no slash
/// command is registered.
pub const ENV_CHANNEL_ID: &str = "DISCORD_CHANNEL_ID";
/// `DISCORD_LINK_URL`: the workspace's web entry, which `/link`'s button opens
/// with a one-time token. Without it `/link` says linking is not set up.
pub const ENV_LINK_URL: &str = "DISCORD_LINK_URL";

#[derive(Clone, Debug)]
pub struct Config {
    pub api_base: Url,
    pub cdn_base: Url,
    pub egress: Egress,
    pub read_only: bool,
    pub guild_id: Option<String>,
    pub only_channel: Option<String>,
    pub link_url: Option<String>,
}

impl Config {
    /// Read the settings through `var`.
    pub fn from_vars(var: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let flag = |name: &str| var(name).is_some_and(|v| matches!(v.trim(), "1" | "true" | "yes"));
        let egress = Egress {
            allow_loopback: flag(ENV_ALLOW_LOOPBACK),
        };
        let base = |name: &str, default: &str, purpose: Purpose| -> Result<Url, String> {
            let text = var(name).unwrap_or_else(|| default.to_owned());
            let url = Url::parse(text.trim_end_matches('/'))
                .map_err(|error| format!("{name}: {error}"))?;
            egress
                .check(&url, purpose)
                .map_err(|error| format!("{name}: {error}"))?;
            Ok(url)
        };
        let snowflake = |name: &str| -> Result<Option<String>, String> {
            match var(name).filter(|id| !id.is_empty()) {
                Some(id) if !id.bytes().all(|b| b.is_ascii_digit()) => {
                    Err(format!("{name}: {id:?} is not a Discord id"))
                }
                id => Ok(id),
            }
        };
        let guild_id = snowflake(ENV_GUILD_ID)?;
        let only_channel = snowflake(ENV_CHANNEL_ID)?;
        Ok(Self {
            api_base: base(ENV_API_BASE, API_BASE, Purpose::Api)?,
            cdn_base: base(ENV_CDN_BASE, CDN_BASE, Purpose::Cdn)?,
            egress,
            read_only: flag(ENV_READ_ONLY),
            guild_id,
            only_channel,
            link_url: var(ENV_LINK_URL)
                .map(|url| url.trim().to_owned())
                .filter(|url| url.starts_with("https://") || url.starts_with("http://")),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(vars: &[(&str, &str)]) -> Result<Config, String> {
        Config::from_vars(|name| {
            vars.iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| (*v).to_owned())
        })
    }

    #[test]
    fn defaults_are_discord() {
        let config = config(&[]).unwrap();
        assert_eq!(config.api_base.as_str(), "https://discord.com/api/v10");
        assert_eq!(config.cdn_base.as_str(), "https://cdn.discordapp.com/");
        assert!(!config.read_only);
        assert!(!config.egress.allow_loopback);
    }

    #[test]
    fn a_loopback_base_needs_the_override() {
        let fake = [(ENV_API_BASE, "http://127.0.0.1:9/api/v10")];
        assert!(config(&fake).is_err());
        let allowed = config(&[fake[0], (ENV_ALLOW_LOOPBACK, "1"), (ENV_READ_ONLY, "1")]).unwrap();
        assert_eq!(allowed.api_base.as_str(), "http://127.0.0.1:9/api/v10");
        assert!(allowed.read_only);
        assert!(config(&[(ENV_API_BASE, "https://example.com/api")]).is_err());
        assert!(config(&[(ENV_GUILD_ID, "abc")]).is_err());
        assert!(config(&[(ENV_CHANNEL_ID, "general")]).is_err());
        let one = config(&[(ENV_CHANNEL_ID, "1101")]).unwrap();
        assert_eq!(one.only_channel.as_deref(), Some("1101"));
    }

    /// Each setting on its own: a flag is on for exactly three spellings, an
    /// id is digits and nothing else, and the link's web entry is an http(s)
    /// URL or nothing.
    #[test]
    fn flags_ids_and_the_link_url_are_read_strictly() {
        for (value, on) in [
            ("1", true),
            ("true", true),
            ("yes", true),
            (" 1\n", true),
            ("", false),
            ("0", false),
            ("no", false),
            ("2", false),
        ] {
            let read = config(&[(ENV_READ_ONLY, value)]).unwrap();
            assert_eq!(read.read_only, on, "{ENV_READ_ONLY}={value:?}");
        }
        assert!(!config(&[]).unwrap().read_only, "unset");

        for name in [ENV_GUILD_ID, ENV_CHANNEL_ID] {
            for (value, expected) in [
                ("", Ok(None)),
                ("0", Ok(Some("0"))),
                ("1101", Ok(Some("1101"))),
                (
                    "123456789012345678901234567890",
                    Ok(Some("123456789012345678901234567890")),
                ),
                ("-1", Err(())),
                ("+1", Err(())),
                (" 1101", Err(())),
                ("1101\n", Err(())),
                ("11 01", Err(())),
                ("0x44d", Err(())),
                ("general", Err(())),
            ] {
                let read = config(&[(name, value)]).map(|settings| {
                    if name == ENV_GUILD_ID {
                        settings.guild_id
                    } else {
                        settings.only_channel
                    }
                });
                let expected = expected
                    .map(|id: Option<&str>| id.map(str::to_owned))
                    .map_err(|()| format!("{name}: {value:?} is not a Discord id"));
                assert_eq!(read, expected, "{name}={value:?}");
            }
        }

        for (value, expected) in [
            ("https://tilo.example/", Some("https://tilo.example/")),
            ("  https://tilo.example/\n", Some("https://tilo.example/")),
            ("http://localhost:8080/", Some("http://localhost:8080/")),
            ("", None),
            ("tilo.example", None),
            ("ftp://tilo.example/", None),
            ("javascript:alert(1)", None),
        ] {
            let read = config(&[(ENV_LINK_URL, value)]).unwrap();
            assert_eq!(
                read.link_url.as_deref(),
                expected,
                "{ENV_LINK_URL}={value:?}"
            );
        }
        assert_eq!(config(&[]).unwrap().link_url, None, "unset");
    }

    /// A base elsewhere than Discord's is refused by the name of its setting
    /// and its host, never its path; one of Discord's is taken without its
    /// trailing slash.
    #[test]
    fn a_base_off_discord_is_refused_by_setting_and_host() {
        for (name, value, error) in [
            (
                ENV_API_BASE,
                "https://cdn.discordapp.com",
                "DISCORD_API_BASE: cdn.discordapp.com is not one of Discord's Api hosts",
            ),
            (
                ENV_CDN_BASE,
                "https://discord.com",
                "DISCORD_CDN_BASE: discord.com is not one of Discord's Cdn hosts",
            ),
            (
                ENV_API_BASE,
                "https://example.com/api/webhooks/1/SECRET",
                "DISCORD_API_BASE: example.com is not one of Discord's Api hosts",
            ),
            (
                ENV_API_BASE,
                "https://user:pw@discord.com/api/v10",
                "DISCORD_API_BASE: discord.com: credentials in a URL are not allowed",
            ),
            (
                ENV_API_BASE,
                "http://discord.com/api/v10",
                "DISCORD_API_BASE: discord.com: only https is allowed",
            ),
            (
                ENV_API_BASE,
                "https://discord.com:8443/api/v10",
                "DISCORD_API_BASE: discord.com: Discord is reached on its default port",
            ),
            (
                ENV_CDN_BASE,
                "http://127.0.0.1:9/cdn",
                "DISCORD_CDN_BASE: 127.0.0.1: Discord is reached on its default port",
            ),
        ] {
            assert_eq!(
                config(&[(name, value)]).unwrap_err(),
                error,
                "{name}={value}"
            );
        }
        let read = config(&[
            (ENV_API_BASE, "https://discord.com/api/v10/"),
            (ENV_CDN_BASE, "https://media.discordapp.net/"),
        ])
        .unwrap();
        assert_eq!(read.api_base.as_str(), "https://discord.com/api/v10");
        assert_eq!(read.cdn_base.as_str(), "https://media.discordapp.net/");
    }
}
