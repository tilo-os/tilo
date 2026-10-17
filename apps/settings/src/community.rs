//! The community workspace this computer holds, as the host's community
//! actions answer it.
//!
//! The host joins the site's shared workspace and remembers whether the
//! person left it. Settings asks it where the person stands, to join again
//! and to leave, through the action door; the answers never enter widget or
//! Automerge state.

use ui::tilo_actions::remote::community::{self, CommunityStatus};
use ui::tilo_actions::remote::{Confirmation, InvokeArgument};

/// Ask the host where the person stands with the community workspace.
pub(crate) fn status(remote: &ui::RemoteActions) -> ui::LocalState<ui::InvocationCall> {
    remote.invoke(
        community::target(community::STATUS),
        InvokeArgument::None,
        None,
    )
}

/// Ask the host to join the community workspace again; it answers once
/// this computer holds it.
pub(crate) fn join(remote: &ui::RemoteActions) -> ui::LocalState<ui::InvocationCall> {
    remote.invoke(
        community::target(community::JOIN),
        InvokeArgument::None,
        None,
    )
}

/// Ask the host to leave the community workspace. Choosing the destructive
/// button is the person's confirmation.
pub(crate) fn leave(remote: &ui::RemoteActions) -> ui::LocalState<ui::InvocationCall> {
    let mut arguments = serde_json::Map::new();
    arguments.insert("confirm".into(), true.into());
    remote.invoke(
        community::target(community::LEAVE),
        InvokeArgument::Json(arguments),
        Some(Confirmation::Confirmed),
    )
}

/// Where the person stands, as a status, join or leave answered it.
///
/// # Errors
///
/// When the answer is missing or not a community status.
pub(crate) fn parse(value: Option<serde_json::Value>) -> Result<CommunityStatus, String> {
    value
        .and_then(|value| serde_json::from_value(value).ok())
        .ok_or_else(|| "the community status the host answered is not readable".to_string())
}

/// The host name `site` is shown by: `https://tilo.coffee/` is
/// `tilo.coffee`.
pub(crate) fn site_name(site: &str) -> &str {
    let rest = site.split_once("://").map_or(site, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    if host.starts_with('[') {
        return host.split_inclusive(']').next().unwrap_or(host);
    }
    host.split_once(':').map_or(host, |(name, _)| name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_status_the_host_answers() {
        assert_eq!(
            parse(Some(serde_json::json!({
                "site": "https://tilo.coffee/", "joined": true, "left": false,
            }))),
            Ok(CommunityStatus {
                site: Some("https://tilo.coffee/".into()),
                joined: true,
                left: false,
            })
        );
        assert_eq!(
            parse(Some(
                serde_json::json!({"site": null, "joined": false, "left": false})
            )),
            Ok(CommunityStatus {
                site: None,
                joined: false,
                left: false,
            }),
            "a computer without a community workspace answers no site"
        );
    }

    #[test]
    fn refuses_an_unreadable_answer() {
        assert!(parse(None).is_err(), "no answer is no status");
        assert!(parse(Some(serde_json::json!({"joined": "yes"}))).is_err());
        assert!(parse(Some(serde_json::json!([]))).is_err());
    }

    #[test]
    fn shows_the_site_by_its_host_name() {
        assert_eq!(site_name("https://tilo.coffee/"), "tilo.coffee");
        assert_eq!(
            site_name("https://eu1.tilo.coffee:8443/join?x=1"),
            "eu1.tilo.coffee"
        );
        assert_eq!(site_name("https://someone@tilo.coffee"), "tilo.coffee");
        assert_eq!(site_name("tilo.coffee"), "tilo.coffee");
        assert_eq!(site_name("https://[::1]:443/"), "[::1]");
    }
}
