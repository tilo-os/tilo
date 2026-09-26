//! How far a piece of presented content may travel.
//!
//! Declared by whoever presents the content — a shared widget for its kind,
//! an app for the rest — never guessed downstream. Every consumer that copies
//! presented content out of the process that shows it (AccessKit, inspection,
//! a recorded trace) applies the same rule through [`Sensitivity::present`],
//! so a password field is redacted identically whichever door it is read
//! through.

use serde::{Deserialize, Serialize};

/// Glyph that stands in for each character of redacted personal content.
///
/// One per character, whitespace kept, so a placeholder has the shape of what
/// it replaces: a truncated name still looks truncated in a shared report.
pub const PLACEHOLDER_GLYPH: char = '\u{25AE}';

/// Classification of presented content, ordered from least to most
/// restricted.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Sensitivity {
    /// Chrome and content anyone in the room may read: button labels,
    /// headings, document text the room already shares.
    #[default]
    Public,
    /// About a person: a contact's name or address, a message body. Shown
    /// in the local session; anything that leaves the device carries a
    /// shape-preserving placeholder instead.
    Personal,
    /// A credential or one-time code. Never leaves the widget that shows it:
    /// not to AccessKit, not to inspection, not to a trace.
    Secret,
}

/// Where presented content is going, which decides what of it survives.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Audience {
    /// This machine's own session: its compositor chrome, its screen reader,
    /// its local recorder before review.
    #[default]
    LocalSession,
    /// Anything beyond this device: a peer, a relay, a posted report.
    BeyondDevice,
}

impl Sensitivity {
    pub fn is_secret(self) -> bool {
        self == Self::Secret
    }

    /// The form of `content` that `audience` may receive, or `None` when it
    /// may receive none of it.
    pub fn present(self, content: &str, audience: Audience) -> Option<String> {
        match (self, audience) {
            (Self::Secret, _) => None,
            (Self::Public, _) | (Self::Personal, Audience::LocalSession) => {
                Some(content.to_owned())
            }
            (Self::Personal, Audience::BeyondDevice) => Some(placeholder(content)),
        }
    }
}

/// A shape-preserving stand-in for `content`: every non-whitespace character
/// becomes [`PLACEHOLDER_GLYPH`], whitespace is kept.
pub fn placeholder(content: &str) -> String {
    content
        .chars()
        .map(|c| {
            if c.is_whitespace() {
                c
            } else {
                PLACEHOLDER_GLYPH
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_content_never_reaches_any_audience() {
        assert_eq!(
            Sensitivity::Secret.present("hunter2", Audience::LocalSession),
            None
        );
        assert_eq!(
            Sensitivity::Secret.present("hunter2", Audience::BeyondDevice),
            None
        );
    }

    #[test]
    fn personal_content_stays_local_and_keeps_its_shape_beyond() {
        assert_eq!(
            Sensitivity::Personal
                .present("Ada Lovelace", Audience::LocalSession)
                .as_deref(),
            Some("Ada Lovelace")
        );
        let shared = Sensitivity::Personal
            .present("Ada Lovelace", Audience::BeyondDevice)
            .unwrap_or_default();
        assert_eq!(shared, "▮▮▮ ▮▮▮▮▮▮▮▮");
        assert_eq!(shared.chars().count(), "Ada Lovelace".chars().count());
    }

    #[test]
    fn restriction_is_ordered() {
        assert!(Sensitivity::Public < Sensitivity::Personal);
        assert!(Sensitivity::Personal < Sensitivity::Secret);
        assert_eq!(Sensitivity::default(), Sensitivity::Public);
    }
}
