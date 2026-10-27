//! Public channel destinations; credential resolution remains in the admitted Browser.
use browser_continuity::sessions::Service;

#[derive(Clone, Debug)]
pub struct Destination {
    pub service: Service,
    pub uri: String,
}
impl Destination {
    pub fn parse(uri: &str) -> Option<Self> {
        if uri.len() > 512 || uri.chars().any(char::is_control) || uri.contains('|') {
            return None;
        }
        let url = url::Url::parse(uri).ok()?;
        if url.scheme() != "https"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.port().is_some()
            || url.fragment().is_some()
            || url.as_str() != uri
        {
            return None;
        }
        let service = match url.host_str()? {
            "slack.com" if url.path() == "/app_redirect" => {
                let parts: Vec<_> = url.query_pairs().collect();
                let [(team, workspace), (channel, id)] = parts.as_slice() else {
                    return None;
                };
                let slack_id = |value: &str, prefix: &[char]| {
                    value.len() >= 2
                        && value.len() <= 64
                        && value.starts_with(prefix)
                        && value
                            .bytes()
                            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
                };
                if team != "team"
                    || channel != "channel"
                    || !slack_id(workspace, &['T'])
                    || !slack_id(id, &['C', 'G', 'D'])
                {
                    return None;
                }
                Service::Slack
            }
            "discord.com" if url.query().is_none() => {
                let parts: Vec<_> = url.path().split('/').collect();
                let [empty, channels, server, channel] = parts.as_slice() else {
                    return None;
                };
                let snowflake = |value: &str| {
                    !value.is_empty()
                        && value.len() <= 20
                        && value.bytes().all(|c| c.is_ascii_digit())
                        && value.parse::<u64>().is_ok_and(|v| v > 0)
                };
                if !empty.is_empty()
                    || *channels != "channels"
                    || !snowflake(server)
                    || !snowflake(channel)
                {
                    return None;
                }
                Service::Discord
            }
            _ => return None,
        };
        Some(Self {
            service,
            uri: uri.to_owned(),
        })
    }
}
pub fn anchor(id: &str, destination: Option<&Destination>) -> String {
    match destination {
        Some(destination) => format!("service:{id}|{}", destination.uri),
        None => format!("service:{id}"),
    }
}
pub fn parse_anchor(value: &str) -> Option<(String, Option<Destination>)> {
    let value = value.strip_prefix("service:")?;
    let (id, destination) = match value.split_once('|') {
        Some((id, uri)) => (id, Some(Destination::parse(uri)?)),
        None => (value, None),
    };
    browser_continuity::sessions::valid_id(id).then(|| (id.to_owned(), destination))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_chat_destinations_refuse_foreign_origins_credentials_and_extra_components() {
        for uri in [
            "https://slack.com/app_redirect?team=T123&channel=C456",
            "https://discord.com/channels/123/456",
        ] {
            let destination = Destination::parse(uri).unwrap();
            let encoded = anchor("selected-account", Some(&destination));
            let (id, decoded) = parse_anchor(&encoded).unwrap();
            assert_eq!(id, "selected-account");
            assert_eq!(decoded.unwrap().uri, uri);
        }
        for uri in [
            "http://discord.com/channels/123/456",
            "https://discord.com.evil.example/channels/123/456",
            "https://discord.com@evil.example/channels/123/456",
            "https://person:secret@discord.com/channels/123/456", // example credentials
            "https://discord.com/channels/123/456/789",
            "https://discord.com/channels/123/456?token=secret",
            "https://discord.com/channels/123/456#secret",
            "https://discord.com/channels/0/456",
            "https://slack.com/app_redirect?team=T123&channel=C456&token=secret",
            "https://slack.com/app_redirect?team=T123&team=T456",
            "https://slack.com/app_redirect?team=X123&channel=C456",
            "https://slack.com.evil.example/app_redirect?team=T123&channel=C456",
        ] {
            assert!(
                Destination::parse(uri).is_none(),
                "accepted foreign destination {uri}"
            );
        }
        assert!(parse_anchor("service:selected-account|https://evil.example").is_none());
        assert!(parse_anchor("service:../account").is_none());
    }
}
