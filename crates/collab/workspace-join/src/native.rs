//! The compact link to an invitation retained by its issuing workspace holder.

use std::net::SocketAddr;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};

use crate::{VerifiedJoin, WorkspaceId};

pub const PREFIX: &str = "tilo://invite#1.";
pub const MAX_LINK_CHARS: usize = 380;
pub const MAX_RELAY_CHARS: usize = 112;
pub const MAX_DIRECT_ADDRESSES: usize = 2;

/// The retained record's id is visible in invitation lists. The separate
/// bearer secret can only be derived by someone who holds the signed bytes,
/// which include a private random nonce. No issuer key is used for lookup.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvitationRef {
    pub workspace: WorkspaceId,
    pub invite_id: [u8; 32],
    bearer: [u8; 32],
}

impl std::fmt::Debug for InvitationRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InvitationRef")
            .field("workspace", &self.workspace)
            .finish_non_exhaustive()
    }
}

impl InvitationRef {
    #[must_use]
    pub fn of(invitation: &VerifiedJoin) -> Self {
        Self {
            workspace: invitation.grant().workspace_id,
            invite_id: invitation.invite_id(),
            bearer: bearer(invitation.signed_invitation()),
        }
    }

    /// Check that the bytes fetched or loaded from disk are the ones this link names.
    #[must_use]
    pub fn matches(&self, signed: &[u8]) -> bool {
        blake3::hash(signed) == blake3::Hash::from_bytes(self.invite_id)
            && blake3::Hash::from_bytes(bearer(signed)) == blake3::Hash::from_bytes(self.bearer)
    }
}

fn bearer(signed: &[u8]) -> [u8; 32] {
    blake3::derive_key("tilo retained workspace invitation link v1", signed)
}

/// Characters of the secret a web link carries for an invitation its
/// workspace keeps ([`bearer_text`]).
pub const BEARER_CHARS: usize = 43;

/// The secret a web link names an invitation by, at the host that keeps it
/// in its workspace: the invitation's bearer as base64url. Only whoever holds
/// the signed bytes can derive it, so the host hands the invitation to
/// whoever names it and to nobody who guesses.
#[must_use]
pub fn bearer_text(signed: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bearer(signed))
}

/// The bearer `text` spells, when it is a web link's secret for a kept
/// invitation rather than an invitation's own text.
#[must_use]
pub fn parse_bearer(text: &str) -> Option<[u8; 32]> {
    let text = text.trim();
    if text.len() != BEARER_CHARS {
        return None;
    }
    URL_SAFE_NO_PAD.decode(text).ok()?.try_into().ok()
}

/// Whether `signed` is the invitation `bearer` names.
#[must_use]
pub fn bears(signed: &[u8], bearer_of: &[u8; 32]) -> bool {
    blake3::Hash::from_bytes(bearer(signed)) == blake3::Hash::from_bytes(*bearer_of)
}

/// Routing information for the read-only lookup. The signed invitation must
/// bind this exact service key to the issuing device and workspace after lookup.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvitationLink {
    pub reference: InvitationRef,
    pub service: [u8; 32],
    pub relay: Option<String>,
    pub direct: Vec<SocketAddr>,
}

impl InvitationLink {
    fn bounded(&self) -> bool {
        !self.reference.workspace.is_nil()
            && self
                .relay
                .as_ref()
                .is_none_or(|relay| !relay.is_empty() && relay.len() <= MAX_RELAY_CHARS)
            && (self.relay.is_some() || !self.direct.is_empty())
            && self.direct.len() <= MAX_DIRECT_ADDRESSES
    }

    #[must_use]
    pub fn to_link(&self) -> Option<String> {
        if !self.bounded() {
            return None;
        }
        let bytes = postcard::to_stdvec(self).ok()?;
        let text = format!("{PREFIX}{}", URL_SAFE_NO_PAD.encode(bytes));
        (text.len() <= MAX_LINK_CHARS).then_some(text)
    }

    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        if text.len() > MAX_LINK_CHARS {
            return None;
        }
        let bytes = URL_SAFE_NO_PAD.decode(text.strip_prefix(PREFIX)?).ok()?;
        let (link, trailing) = postcard::take_from_bytes::<Self>(&bytes).ok()?;
        (trailing.is_empty() && link.to_link().as_deref() == Some(text)).then_some(link)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_web_links_secret_names_one_invitation_and_reads_apart_from_its_text() {
        let signed = b"the signed invitation's own bytes, private nonce and all";
        let secret = bearer_text(signed);
        assert_eq!(secret.len(), BEARER_CHARS);
        let read = parse_bearer(&secret).expect("the secret reads back");
        assert!(bears(signed, &read));
        assert!(
            !bears(b"another invitation", &read),
            "a secret names the one invitation it was derived from"
        );
        assert_eq!(parse_bearer(&format!(" {secret} ")), Some(read));
        assert_eq!(parse_bearer(&format!("{secret}A")), None);
        assert_eq!(
            parse_bearer(&crate::encode_invitation_text(signed)),
            None,
            "an invitation's own text is not a kept invitation's secret"
        );
    }
}
