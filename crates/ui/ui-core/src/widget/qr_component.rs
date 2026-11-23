//! [`QrWidget`] as a described component.

use crate::component::{Component, Described};

use super::QrWidget;

/// A QR code of a payload for a phone's camera to read (a pairing handshake, an invite); a
/// private payload is left out of screenshots and screens and never spoken.
#[derive(Component, Default)]
#[component(name = "QrCode", role = "Image", label = "alt")]
pub struct QrProps {
    /// What the code carries, as the scanner reads it.
    #[component(required, positional)]
    #[component(data)]
    pub payload: String,
    /// What scanning it means, as assistive technology announces it: "QR code" unless said.
    pub alt: Option<String>,
    /// The payload is private pairing material: leave the code out of screenshots and
    /// recordings, and never speak it.
    pub secret: bool,
}

impl Described for QrWidget {
    type Props = QrProps;

    fn from_props(_key: &str, props: QrProps) -> Self {
        let QrProps {
            payload,
            alt,
            secret,
        } = props;
        let mut code = Self::new(payload.into_bytes());
        if let Some(alt) = alt {
            code = code.with_alt(alt);
        }
        if secret {
            code = code.with_secret();
        }
        code
    }

    /// A changed payload re-encodes in place; a changed name or secrecy too.
    fn set_props(&mut self, props: QrProps) -> bool {
        let QrProps {
            payload,
            alt,
            secret,
        } = props;
        self.set_alt(alt.unwrap_or_default());
        self.set_secret(secret);
        self.set_payload(payload.into_bytes());
        true
    }
}
