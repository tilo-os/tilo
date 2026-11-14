//! What a drag that crosses between the person's devices carries: never a byte of content, only
//! what names it in the workspace.

use crate::{valid_identifier, valid_mime};

/// What a drag that crossed between the person's devices carries. No byte of content rides in
/// it: content is kept in the workspace and named, a resource is named by its address.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CarriedDrag {
    /// A drag out of an app: each format's content, kept in `workspace` and named by its sealed
    /// reference.
    Content {
        workspace: String,
        offers: Vec<CarriedOffer>,
    },
    /// A resource the desk carries (a window's record, an item of a list): what its drag names it
    /// by, and what it shows under the pointer.
    Resource {
        kind: String,
        address: String,
        label: String,
    },
}

/// One format a carried drag offers, and the content reference its bytes are kept under.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CarriedOffer {
    pub mime: String,
    pub reference: String,
}

/// The most formats a carried drag offers.
pub const MAX_CARRIED_OFFERS: usize = 8;

impl CarriedDrag {
    /// Whether every name it carries is well formed and bounded.
    #[must_use]
    pub fn valid(&self) -> bool {
        match self {
            Self::Content { workspace, offers } => {
                valid_identifier(workspace, 64)
                    && !offers.is_empty()
                    && offers.len() <= MAX_CARRIED_OFFERS
                    && offers.iter().all(|offer| {
                        valid_mime(&offer.mime) && valid_identifier(&offer.reference, 256)
                    })
            }
            Self::Resource {
                kind,
                address,
                label,
            } => {
                valid_identifier(kind, 64)
                    && !address.is_empty()
                    && address.len() <= 1024
                    && !address.chars().any(char::is_control)
                    && label.len() <= 512
                    && !label.chars().any(char::is_control)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_carried_drag_names_what_it_carries_and_carries_no_bytes() {
        let content = |workspace: &str, offers: Vec<CarriedOffer>| {
            CarriedDrag::Content {
                workspace: workspace.into(),
                offers,
            }
            .valid()
        };
        let text = CarriedOffer {
            mime: "text/plain".into(),
            reference: format!("{}.{}", "a".repeat(64), "B".repeat(43)),
        };
        assert!(content("ws-1", vec![text.clone()]));
        assert!(!content("ws-1", Vec::new()), "a drag carries something");
        assert!(
            !content("ws-1", vec![text.clone(); MAX_CARRIED_OFFERS + 1]),
            "a drag carries a few formats"
        );
        let words = CarriedOffer {
            mime: "text/plain".into(),
            reference: "hello world".into(),
        };
        assert!(!content("ws-1", vec![words]), "a reference, never the text");
        assert!(!content("", vec![text]), "kept in a workspace");
        let resource = |address: &str| {
            CarriedDrag::Resource {
                kind: "resource".into(),
                address: address.into(),
                label: "Notes".into(),
            }
            .valid()
        };
        assert!(resource("notes.page:one"));
        assert!(!resource(""), "a resource is named");
    }
}
