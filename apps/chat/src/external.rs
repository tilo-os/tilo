//! Browser adapters for external shared spaces. No credentials, account
//! impersonation, imported conversation history or native call transport.
use crate::effects::ChatEffect;

pub(crate) fn browser_destination(effect: &ChatEffect) -> Result<Option<String>, String> {
    let slack_id = |id: &str, prefix: &[char]| {
        id.len() >= 2
            && id.len() <= 64
            && id.starts_with(prefix)
            && id
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
    };
    let snowflake = |id: &str| {
        !id.is_empty()
            && id.len() <= 20
            && id.chars().all(|c| c.is_ascii_digit())
            && id.parse::<u64>().is_ok_and(|value| value > 0)
    };
    match effect {
        ChatEffect::OpenSlackChannel { workspace, channel } => {
            if !slack_id(workspace, &['T']) || !slack_id(channel, &['C', 'G', 'D']) {
                return Err("Use Slack workspace and conversation ids".into());
            }
            Ok(Some(format!(
                "https://slack.com/app_redirect?team={workspace}&channel={channel}"
            )))
        }
        ChatEffect::OpenDiscordChannel { server, channel } => {
            if !snowflake(server) || !snowflake(channel) {
                return Err("Use Discord server and channel ids".into());
            }
            Ok(Some(format!(
                "https://discord.com/channels/{server}/{channel}"
            )))
        }
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_adapters_only_open_service_origins_and_never_accept_tokens_or_urls() {
        let slack = |workspace: &str, channel: &str| ChatEffect::OpenSlackChannel {
            workspace: workspace.into(),
            channel: channel.into(),
        };
        assert_eq!(
            browser_destination(&slack("T123", "C456"))
                .unwrap()
                .as_deref(),
            Some("https://slack.com/app_redirect?team=T123&channel=C456")
        );
        for channel in [
            "C1?token=secret",
            "C1/../../",
            "https://example.test",
            "C1\n",
            "",
        ] {
            assert!(browser_destination(&slack("T123", channel)).is_err());
        }
        let discord = |server: &str, channel: &str| ChatEffect::OpenDiscordChannel {
            server: server.into(),
            channel: channel.into(),
        };
        assert_eq!(
            browser_destination(&discord("123", "456"))
                .unwrap()
                .as_deref(),
            Some("https://discord.com/channels/123/456")
        );
        for server in ["@me", "0", "18446744073709551616", "123#fragment", ""] {
            assert!(browser_destination(&discord(server, "456")).is_err());
        }
        assert_eq!(browser_destination(&ChatEffect::StartCall).unwrap(), None);
    }
}
