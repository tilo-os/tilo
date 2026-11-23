//! A text a person's vault released for one site's page, on its way to the device that holds
//! that page's window.
//!
//! The server that keeps the vault releases the text for one page action of one window. Whatever
//! carries the answer to that window's device reads none of it: the text is sealed to the
//! device's identity key, as its media keys are ([`crate::SenderMediaKeys::seal_to`]), under a
//! label of its own. What it was released for is authenticated with it ([`ReleasedFor`]): the
//! workspace, the page action that asked, the device and the site. A box sealed for one of them
//! opens for no other, so an answer that is kept and sent again fills no other page.

use ed25519_dalek::SigningKey;
use tilo_seal::{Label, SealedBox, SecretKey};
use zeroize::Zeroizing;

use crate::endpoint::box_recipient;
use crate::{Error, Result};

/// What the associated data of such a box begins with.
const DOMAIN: &[u8] = b"tilo/released-page-text/v1";
/// The longest text sealed this way, in bytes: a password, a code or a kept value.
pub const MAX_RELEASED_TEXT: usize = 4096;

/// What a text is released for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReleasedFor<'a> {
    /// The workspace the window is in, as its UUID is written.
    pub workspace: &'a str,
    /// The page action that asked: the 16 bytes of its invocation.
    pub invocation: [u8; 16],
    /// The site the window's engine says its page is at: an origin.
    pub origin: &'a str,
}

impl ReleasedFor<'_> {
    /// What a box for `device` is authenticated with.
    fn aad(&self, device: &[u8; 32]) -> Vec<u8> {
        let mut aad = Vec::new();
        aad.extend_from_slice(DOMAIN);
        measured(&mut aad, self.workspace.as_bytes());
        aad.extend_from_slice(&self.invocation);
        aad.extend_from_slice(device);
        measured(&mut aad, self.origin.as_bytes());
        aad
    }
}

/// `bytes` behind their length, so two parts never read as one.
fn measured(aad: &mut Vec<u8>, bytes: &[u8]) {
    aad.extend_from_slice(&u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_le_bytes());
    aad.extend_from_slice(bytes);
}

/// Seal `text` to `device`, a device's Ed25519 identity key, for `bound`.
///
/// # Errors
///
/// [`Error::InvalidState`] for a text that is empty or longer than [`MAX_RELEASED_TEXT`], or
/// for bytes that are no Ed25519 public key, and [`Error::ContentCrypto`] if the box cannot be
/// sealed.
pub fn seal_released_text(
    device: [u8; 32],
    bound: &ReleasedFor<'_>,
    text: &str,
) -> Result<Vec<u8>> {
    if text.is_empty() || text.len() > MAX_RELEASED_TEXT {
        return Err(Error::InvalidState(
            "a released text is empty or too long".into(),
        ));
    }
    let recipient = box_recipient(device)?;
    tilo_seal::hpke::seal(
        &recipient,
        Label::ReleasedPageText,
        &bound.aad(&device),
        text.as_bytes(),
    )
    .map(|sealed| sealed.to_bytes())
    .map_err(Error::ContentCrypto)
}

/// Open a box [`seal_released_text`] sealed to `device` for `bound`.
///
/// # Errors
///
/// [`Error::ContentCrypto`] for a box sealed to another device, or for another workspace, page
/// action or site, and [`Error::Serialization`] for bytes that are no text.
pub fn open_released_text(
    device: &SigningKey,
    bound: &ReleasedFor<'_>,
    sealed: &[u8],
) -> Result<Zeroizing<String>> {
    let own = device.verifying_key().to_bytes();
    let opened = tilo_seal::hpke::open(
        &SecretKey::from_bytes(device.to_scalar_bytes()),
        Label::ReleasedPageText,
        &bound.aad(&own),
        &SealedBox::from_bytes(sealed).map_err(Error::ContentCrypto)?,
    )
    .map_err(Error::ContentCrypto)?;
    std::str::from_utf8(&opened)
        .map(|text| Zeroizing::new(text.to_owned()))
        .map_err(|_| Error::Serialization("a released text is not text".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORKSPACE: &str = "0f8fad5b-d9cb-469f-a165-70867728950e";
    /// A made-up text: no password of anyone.
    const TEXT: &str = "a made-up password";

    fn bound(origin: &str) -> ReleasedFor<'_> {
        ReleasedFor {
            workspace: WORKSPACE,
            invocation: [7; 16],
            origin,
        }
    }

    #[test]
    fn a_released_text_opens_for_its_device_its_page_action_and_its_site_alone() {
        let device = SigningKey::from_bytes(&[5; 32]);
        let key = device.verifying_key().to_bytes();
        let site = bound("https://example.test");
        let sealed = seal_released_text(key, &site, TEXT).unwrap();
        assert!(
            !sealed.windows(7).any(|window| window == b"made-up"),
            "the box holds nothing of the text in the clear"
        );
        assert_eq!(
            open_released_text(&device, &site, &sealed)
                .unwrap()
                .as_str(),
            TEXT
        );
        let other = SigningKey::from_bytes(&[6; 32]);
        assert!(
            open_released_text(&other, &site, &sealed).is_err(),
            "another device opens nothing"
        );
        assert!(
            open_released_text(&device, &bound("https://example.test:8443"), &sealed).is_err(),
            "nor does it open for another site"
        );
        let later = ReleasedFor {
            invocation: [8; 16],
            ..site
        };
        assert!(
            open_released_text(&device, &later, &sealed).is_err(),
            "nor for another page action"
        );
        let elsewhere = ReleasedFor {
            workspace: "another-workspace",
            ..site
        };
        assert!(
            open_released_text(&device, &elsewhere, &sealed).is_err(),
            "nor in another workspace"
        );
    }

    #[test]
    fn nothing_empty_or_too_long_is_sealed() {
        let key = SigningKey::from_bytes(&[5; 32]).verifying_key().to_bytes();
        let site = bound("https://example.test");
        assert!(seal_released_text(key, &site, "").is_err());
        let long = "x".repeat(MAX_RELEASED_TEXT.saturating_add(1));
        assert!(seal_released_text(key, &site, &long).is_err());
    }
}
