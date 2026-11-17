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

    /// Opening a channel in the browser waits for the desktop's opener off the UI thread, so
    /// it answers later: through the app's api it runs as the job it declares, and what the
    /// desktop is handed is the service's own address.
    #[test]
    fn opening_a_channel_in_the_browser_is_a_job_the_desktop_answers() {
        use crate::resource::ChatShell;
        use ui::framework::testing::TestApp;

        // The worker that asks the desktop sees the host installed for the process.
        let (host, recorder) = ui::host::HostServices::recording();
        ui::host::install_default(host);
        let mut app = TestApp::<ChatShell>::new().unwrap();
        app.effect(&ChatEffect::OpenSlackChannel {
            workspace: "T123".into(),
            channel: "C456".into(),
        })
        .expect("the desktop opened the Slack channel");
        app.effect(&ChatEffect::OpenDiscordChannel {
            server: "123".into(),
            channel: "456".into(),
        })
        .expect("the desktop opened the Discord channel");
        assert_eq!(
            recorder.opened_uris(),
            [
                "https://slack.com/app_redirect?team=T123&channel=C456",
                "https://discord.com/channels/123/456",
            ]
        );
    }
}
