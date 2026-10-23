//! `GET /metrics`: when the published catalogue and the authority behind it
//! expire, in the Prometheus text format.
//!
//! The owner authorizes the online signing channel for at most 31 days, and
//! renewing it is a deliberate offline operation; once it lapses, nothing can
//! be signed and the 24-hour snapshot runs out a day later. On 2026-10-21 that
//! happened unnoticed and every device lost the catalogue. These gauges let
//! alerts ask for the renewal days ahead, and for the signer well before the
//! snapshot expires. They are read from the catalogue the server already
//! verified for `/entries`, so a scrape costs no extra file reads.

use std::fmt::Write as _;

use bytes::Bytes;
use http_body_util::Full;
use hyper::{Response, header};

use crate::catalogue::{ChannelTerm, FullSnapshot, Published};

/// The `/metrics` response for `published`.
pub(crate) fn response(published: &Published) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(Bytes::from(render(published))));
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("text/plain; version=0.0.4"),
    );
    headers.insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response
}

pub(crate) fn render(published: &Published) -> String {
    let snapshot = match &published.full {
        FullSnapshot::Verified { encoded, .. } => SnapshotState::Verified {
            expires_at_unix: encoded.expires_at_unix(),
        },
        FullSnapshot::Refused => SnapshotState::Refused,
        FullSnapshot::Missing => SnapshotState::Missing,
    };
    render_parts(published.channel, snapshot)
}

/// What `/metrics` reports about the published snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SnapshotState {
    Missing,
    Refused,
    Verified { expires_at_unix: u64 },
}

fn render_parts(channel: Option<ChannelTerm>, snapshot: SnapshotState) -> String {
    let mut out = String::new();
    if let Some(channel) = channel {
        gauge(
            &mut out,
            "tilo_registry_channel_authorization_expires_at_seconds",
            "When the owner's authorization of the signing channel expires, in Unix seconds. Renew it before then.",
            channel.expires_at_unix,
        );
        gauge(
            &mut out,
            "tilo_registry_channel_authorization_generation",
            "The generation of the active channel authorization; a renewal takes the next one.",
            channel.generation,
        );
    }
    gauge(
        &mut out,
        "tilo_registry_snapshot_verified",
        "1 while the published snapshot verifies against the owner's key and the active channel.",
        u64::from(matches!(snapshot, SnapshotState::Verified { .. })),
    );
    if let SnapshotState::Verified { expires_at_unix } = snapshot {
        gauge(
            &mut out,
            "tilo_registry_snapshot_expires_at_seconds",
            "When the published snapshot expires, in Unix seconds; devices refuse it afterwards.",
            expires_at_unix,
        );
    }
    out
}

fn gauge(out: &mut String, name: &str, help: &str, value: u64) {
    // Writing to a String cannot fail.
    let _ = writeln!(out, "# HELP {name} {help}");
    let _ = writeln!(out, "# TYPE {name} gauge");
    let _ = writeln!(out, "{name} {value}");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The value of the one sample named `name`, if the body has it.
    fn sample(body: &str, name: &str) -> Option<u64> {
        body.lines()
            .filter(|line| !line.starts_with('#'))
            .find_map(|line| line.strip_prefix(name)?.strip_prefix(' ')?.parse().ok())
    }

    #[test]
    fn both_expiries_and_the_generation_are_gauges_alerts_can_read() {
        let body = render_parts(
            Some(ChannelTerm {
                generation: 2,
                expires_at_unix: 1_795_266_000,
            }),
            SnapshotState::Verified {
                expires_at_unix: 1_792_700_000,
            },
        );
        assert_eq!(
            sample(
                &body,
                "tilo_registry_channel_authorization_expires_at_seconds"
            ),
            Some(1_795_266_000)
        );
        assert_eq!(
            sample(&body, "tilo_registry_channel_authorization_generation"),
            Some(2)
        );
        assert_eq!(
            sample(&body, "tilo_registry_snapshot_expires_at_seconds"),
            Some(1_792_700_000)
        );
        assert_eq!(sample(&body, "tilo_registry_snapshot_verified"), Some(1));
        for name in [
            "tilo_registry_channel_authorization_expires_at_seconds",
            "tilo_registry_channel_authorization_generation",
            "tilo_registry_snapshot_expires_at_seconds",
            "tilo_registry_snapshot_verified",
        ] {
            assert_eq!(
                body.matches(&format!("# TYPE {name} gauge\n")).count(),
                1,
                "{name}"
            );
        }
    }

    #[test]
    fn the_authorization_is_reported_even_when_no_snapshot_verifies() {
        // When the channel lapsed and nothing verifies, the renewal is what
        // the operator needs to see.
        let body = render_parts(
            Some(ChannelTerm {
                generation: 1,
                expires_at_unix: 1_792_587_897,
            }),
            SnapshotState::Refused,
        );
        assert_eq!(
            sample(
                &body,
                "tilo_registry_channel_authorization_expires_at_seconds"
            ),
            Some(1_792_587_897)
        );
        assert_eq!(sample(&body, "tilo_registry_snapshot_verified"), Some(0));
        assert_eq!(
            sample(&body, "tilo_registry_snapshot_expires_at_seconds"),
            None
        );
    }

    #[test]
    fn an_owner_signed_registry_reports_no_channel() {
        let body = render_parts(None, SnapshotState::Missing);
        assert!(!body.contains("channel_authorization"));
        assert_eq!(sample(&body, "tilo_registry_snapshot_verified"), Some(0));
    }
}
