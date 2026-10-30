//! Which players may be handed to another device, and how a handoff names
//! the source the destination resumes.
//!
//! A player becomes a [`PlaybackReport`] only when its owner was verified to
//! be a launch of an installed app that claims the source's scheme or file
//! type, and the source is portable: a public `http(s)` stream without
//! credentials in it, or a local file the transfer carries privately.

use std::path::PathBuf;

use media::handoff::{HandoffSource, PlaybackReport, PlaybackSession};

use crate::{Player, ResumeSource, Status};

/// The sources an installed app opens, for matching a player against it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaApp {
    pub package_id: String,
    pub uri_schemes: Vec<String>,
    pub extensions: Vec<String>,
}

/// `player` as a handoff report, its playhead at `playhead_us`. `None`
/// unless it is verified, playing or paused, and its source portable and
/// claimed by exactly one of `apps` that is its own package.
#[must_use]
pub fn report(player: &Player, playhead_us: i64, apps: &[MediaApp]) -> Option<PlaybackReport> {
    let playing = match player.status {
        Status::Playing => true,
        Status::Paused => false,
        Status::Stopped => return None,
    };
    let verified = player.package_id.as_deref()?;
    let locator = player.url.as_deref()?;
    let source = portable_source(locator)?;
    let scheme = match &source {
        HandoffSource::RemoteStream { .. } => locator.split_once(':')?.0,
        HandoffSource::LocalFile { .. } => "file",
        HandoffSource::Workspace { .. } | HandoffSource::AppResource { .. } => return None,
    };
    let package_id = resolve_package(verified, apps, scheme, &source)?;
    let title = player
        .title
        .clone()
        .or_else(|| player.identity.clone())
        .unwrap_or_else(|| "Media".to_string());
    let duration_ms = player
        .length_us
        .filter(|value| *value >= 0)
        .map(|value| u64::try_from(value / 1_000).unwrap_or(u64::MAX));
    let position_ms = u64::try_from(playhead_us.max(0) / 1_000).unwrap_or(0);
    if duration_ms.is_some_and(|duration| position_ms > duration) {
        return None;
    }
    let content_id = format!("blake3:{}", blake3::hash(locator.as_bytes()).to_hex());
    let session_suffix = blake3::hash(format!("{}\0{locator}", player.bus_name).as_bytes());
    let session_suffix = session_suffix.to_hex();
    let session_suffix = session_suffix.as_str().get(..16)?;
    let report = PlaybackReport {
        session: PlaybackSession {
            id: format!("{package_id}:mpris:{session_suffix}"),
            app_id: package_id.clone(),
            title,
            content_id,
            position_ms,
            duration_ms,
            playing,
            has_video: locator_has_video(locator),
        },
        workspace_id: String::new(),
        source,
        controller_bus_name: Some(player.bus_name.clone()),
        accepted_handoff_request: None,
    };
    report.validate_route(&package_id, "").then_some(report)
}

fn resolve_package(
    verified_package_id: &str,
    apps: &[MediaApp],
    scheme: &str,
    source: &HandoffSource,
) -> Option<String> {
    let extension = match source {
        HandoffSource::LocalFile { path } => path
            .extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase),
        _ => None,
    };
    let matches = apps
        .iter()
        .filter(|app| {
            app.package_id == verified_package_id
                && if scheme == "file" {
                    extension
                        .as_ref()
                        .is_some_and(|extension| app.extensions.contains(extension))
                } else {
                    app.uri_schemes.iter().any(|claimed| claimed == scheme)
                }
        })
        .map(|app| app.package_id.as_str())
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [package_id] => Some((*package_id).to_string()),
        _ => None,
    }
}

/// The exact URI a resumed player must report: a canonical `file://` URI
/// for a local file, a credential-free `http(s)` stream as given.
#[must_use]
pub fn resume_uri(source: &ResumeSource) -> Option<String> {
    match source {
        ResumeSource::Uri(uri) => safe_remote_locator(uri).then(|| uri.clone()),
        ResumeSource::LocalFile(path) if path.is_absolute() => {
            let path = path.to_str()?;
            if path.chars().any(char::is_control) {
                return None;
            }
            url::Url::from_file_path(path).ok().map(String::from)
        }
        ResumeSource::LocalFile(_) => None,
    }
}

fn portable_source(locator: &str) -> Option<HandoffSource> {
    if safe_remote_locator(locator) {
        return Some(HandoffSource::RemoteStream {
            url: locator.to_string(),
        });
    }
    let encoded = locator.strip_prefix("file://")?;
    if !encoded.starts_with('/') || encoded.contains(['?', '#']) {
        return None;
    }
    let path = PathBuf::from(percent_decode(encoded)?);
    path.is_absolute()
        .then_some(HandoffSource::LocalFile { path })
}

fn safe_remote_locator(locator: &str) -> bool {
    let Some(rest) = locator
        .strip_prefix("https://")
        .or_else(|| locator.strip_prefix("http://"))
    else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.is_empty()
        || authority.contains('@')
        || locator.len() > 8192
        || locator
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte == b'\\')
    {
        return false;
    }
    let Some(query) = locator.split_once('?').map(|(_, query)| query) else {
        return true;
    };
    let query = query.split('#').next().unwrap_or(query);
    !query.split('&').any(|pair| {
        let key = pair.split_once('=').map_or(pair, |(key, _)| key);
        let key = percent_decode(key)
            .unwrap_or_else(|| key.to_string())
            .to_ascii_lowercase();
        matches!(
            key.as_str(),
            "access_token"
                | "auth"
                | "authorization"
                | "expire"
                | "expires"
                | "key"
                | "sig"
                | "signature"
                | "token"
        ) || key.starts_with("x-amz-")
    })
}

/// `value` percent-decoded, unless it decodes to invalid UTF-8 or a control
/// character.
#[must_use]
pub fn percent_decode(value: &str) -> Option<String> {
    let decoded = percent_encoding::percent_decode_str(value)
        .decode_utf8()
        .ok()?;
    (!decoded.chars().any(char::is_control)).then(|| decoded.into_owned())
}

fn locator_has_video(locator: &str) -> bool {
    let path = locator.split(['?', '#']).next().unwrap_or(locator);
    matches!(
        path.rsplit('.')
            .next()
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("avi" | "m4v" | "mkv" | "mov" | "mp4" | "webm")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn player(locator: &str) -> Player {
        Player {
            bus_name: "org.mpris.MediaPlayer2.browser".into(),
            unique_name: ":1.4".into(),
            pid: Some(4),
            package_id: Some("so.tilo.browser".into()),
            desktop_entry: None,
            identity: Some("Browser".into()),
            status: Status::Playing,
            title: Some("A film".into()),
            artist: None,
            url: Some(locator.into()),
            art_url: None,
            track_id: None,
            length_us: Some(9_000_000),
            position_us: 4_000_000,
            rate: 1.0,
            can_play: true,
            can_pause: true,
            can_seek: false,
            can_go_next: false,
            can_go_previous: false,
        }
    }

    fn browser() -> MediaApp {
        MediaApp {
            package_id: "so.tilo.browser".into(),
            uri_schemes: vec!["http".into(), "https".into()],
            extensions: vec!["mp4".into()],
        }
    }

    fn report_of(locator: &str) -> Option<PlaybackReport> {
        report(&player(locator), 4_000_000, &[browser()])
    }

    #[test]
    fn a_verified_player_becomes_a_portable_stable_app_report() {
        let first = report_of("https://media.example/film.mp4").unwrap();
        assert_eq!(
            Some(&first),
            report_of("https://media.example/film.mp4").as_ref(),
            "session identity must be stable"
        );
        assert_eq!(first.session.app_id, "so.tilo.browser");
        assert_eq!(first.session.position_ms, 4_000);
        assert_eq!(first.session.duration_ms, Some(9_000));
        assert!(first.session.has_video);
        assert!(matches!(
            first.source,
            HandoffSource::RemoteStream { ref url } if url == "https://media.example/film.mp4"
        ));
        assert_eq!(
            first.controller_bus_name.as_deref(),
            Some("org.mpris.MediaPlayer2.browser")
        );
    }

    #[test]
    fn unverified_ambiguous_or_stopped_players_are_not_advertised() {
        let observed = player("https://media.example/film.mp4");
        assert!(report(&observed, 0, &[browser(), browser()]).is_none());
        let stopped = Player {
            status: Status::Stopped,
            ..observed.clone()
        };
        assert!(report(&stopped, 0, &[browser()]).is_none());
        let unverified = Player {
            package_id: None,
            ..observed
        };
        assert!(report(&unverified, 0, &[browser()]).is_none());
    }

    #[test]
    fn local_paths_stay_in_the_private_transfer_source() {
        let report = report_of("file:///home/alice/Videos/My%20Film.mp4").unwrap();
        assert!(matches!(
            report.source,
            HandoffSource::LocalFile { ref path }
                if path == &PathBuf::from("/home/alice/Videos/My Film.mp4")
        ));
        assert!(!format!("{:?}", report.session).contains("/home/alice"));
    }

    #[test]
    fn unsafe_or_nonportable_urls_are_rejected() {
        for locator in [
            "https://alice:secret@media.example/film.mp4",
            "https://media.example/film.mp4?access_token=secret",
            "blob:https://media.example/ephemeral",
            "file://server/share/film.mp4",
        ] {
            assert!(report_of(locator).is_none(), "{locator}");
        }
    }

    #[test]
    fn destination_resume_sources_are_canonical_and_bearer_free() {
        assert_eq!(
            resume_uri(&ResumeSource::LocalFile(PathBuf::from(
                "/home/alice/My Film.mp4"
            ))),
            Some("file:///home/alice/My%20Film.mp4".into())
        );
        assert_eq!(
            resume_uri(&ResumeSource::Uri(
                "https://media.example/film.mp4?v=42".into()
            )),
            Some("https://media.example/film.mp4?v=42".into())
        );
        assert!(
            resume_uri(&ResumeSource::Uri(
                "https://media.example/film.mp4?%61ccess_token=secret".into()
            ))
            .is_none()
        );
    }
}
