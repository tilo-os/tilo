//! The LUKS2 token that carries this machine's side of a sibling unlock.
//!
//! One token of type [`TOKEN_TYPE`] per volume, bound to the one key slot the
//! sibling factor opens. It lives in the volume's own header, so it is on the
//! disk exactly as long as the slot is (`systemd-cryptenroll --wipe-slot` and
//! `tilo-sibling-unlock remove` take both away together), and early boot reads
//! it without mounting anything.
//!
//! The header is readable by anyone holding the disk, like the boot
//! partition. What it carries in the clear is public (the device, the boot
//! key's public half and the siblings to ask) plus the [`crate::material`]
//! blob: sealed to this machine's TPM where there is one, otherwise in the
//! clear. That blob alone opens nothing; the sibling half is never stored here.

use anyhow::{Context, Result, bail};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use tilo_unlock_protocol::{BootKeyCert, SiblingContact};

/// The LUKS2 token type. cryptsetup's own token handlers ignore it.
pub const TOKEN_TYPE: &str = "tilo-sibling-unlock";
/// Format of the fields below.
pub const TOKEN_VERSION: u32 = 1;

/// How [`Token::material`] is stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Sealing {
    /// Sealed to this machine's TPM (`systemd-creds --with-key=tpm2`).
    Tpm2,
    /// In the clear: the machine has no TPM Linux can use.
    Cleartext,
}

/// One of the person's devices this machine asks for the sibling half, as
/// the enrolment recorded it ([`tilo_unlock_protocol::SiblingContact`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sibling {
    /// The sibling's fleet key, hex. It answers challenges as this device.
    pub device: String,
    #[serde(default)]
    pub label: String,
    /// Its peer-network address (JSON of an iroh `EndpointAddr`); empty when
    /// it published none, and it is then found by its key.
    #[serde(default)]
    pub endpoint: String,
}

impl From<&SiblingContact> for Sibling {
    fn from(contact: &SiblingContact) -> Self {
        Self {
            device: hex::encode(contact.device_public_key),
            label: contact.label.clone(),
            endpoint: contact.endpoint.clone(),
        }
    }
}

/// The token as it is stored in the LUKS2 header.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Token {
    #[serde(rename = "type")]
    pub kind: String,
    /// The key slot this token opens, as LUKS2 lists slots: decimal strings.
    pub keyslots: Vec<String>,
    pub tilo_version: u32,
    /// The boot key's certificate, base64 of its postcard encoding. Public:
    /// it names this device, the boot key and the slot, signed by the
    /// person's identity.
    pub cert: String,
    /// Who to ask, in order of preference.
    pub siblings: Vec<Sibling>,
    pub sealing: Sealing,
    /// The secret material, base64: a `systemd-creds` blob for
    /// [`Sealing::Tpm2`], the JSON of [`crate::material::Material`] for
    /// [`Sealing::Cleartext`].
    pub material: String,
}

impl Token {
    /// The one key slot this token opens.
    pub fn keyslot(&self) -> Result<u32> {
        match self.keyslots.as_slice() {
            [slot] => slot
                .parse()
                .with_context(|| format!("the token's key slot {slot:?} is not a number")),
            _ => bail!("the token must name exactly one key slot"),
        }
    }

    /// The boot key's certificate.
    pub fn cert(&self) -> Result<BootKeyCert> {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&self.cert)
            .context("the boot key certificate is not base64")?;
        postcard::from_bytes(&bytes).context("the boot key certificate is malformed")
    }

    /// Encodes `cert` as the token stores it.
    ///
    /// # Errors
    /// When the certificate cannot be encoded.
    pub fn encode_cert(cert: &BootKeyCert) -> Result<String> {
        Ok(base64::engine::general_purpose::STANDARD.encode(postcard::to_allocvec(cert)?))
    }

    /// Checks the fields early boot depends on.
    pub fn validate(&self) -> Result<()> {
        if self.kind != TOKEN_TYPE {
            bail!("token type {:?} is not {TOKEN_TYPE}", self.kind);
        }
        if self.tilo_version != TOKEN_VERSION {
            bail!("token version {} is not {TOKEN_VERSION}", self.tilo_version);
        }
        let keyslot = self.keyslot()?;
        if u32::from(self.cert()?.slot.keyslot) != keyslot {
            bail!("the boot key certificate is for another key slot");
        }
        if self.siblings.is_empty() {
            bail!("the token names no device to ask");
        }
        Ok(())
    }
}

/// Finds the sibling-unlock token in `cryptsetup luksDump --dump-json-metadata`
/// output.
///
/// # Errors
/// When the metadata is not LUKS2 JSON, or carries a malformed or more than one
/// sibling-unlock token. `Ok(None)` means the volume has no sibling factor.
pub fn from_metadata(metadata: &str) -> Result<Option<Token>> {
    let metadata: serde_json::Value =
        serde_json::from_str(metadata).context("the LUKS2 metadata is not JSON")?;
    let Some(tokens) = metadata
        .get("tokens")
        .and_then(serde_json::Value::as_object)
    else {
        return Ok(None);
    };
    let mut found = None;
    for token in tokens.values() {
        if token.get("type").and_then(serde_json::Value::as_str) != Some(TOKEN_TYPE) {
            continue;
        }
        if found.is_some() {
            bail!("the volume carries more than one {TOKEN_TYPE} token");
        }
        let token: Token = serde_json::from_value(token.clone())
            .context("the sibling-unlock token is malformed")?;
        token.validate()?;
        found = Some(token);
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_unlock_protocol::SlotId;

    fn cert(keyslot: u16) -> BootKeyCert {
        BootKeyCert {
            version: tilo_unlock_protocol::PROTOCOL_VERSION,
            user_public_key: [1; 32],
            device_public_key: [2; 32],
            boot_public_key: [3; 32],
            slot: SlotId {
                volume_uuid: [4; 16],
                keyslot,
            },
            attestation_key: None,
            issued_at_ns: 5,
            signature: [6; 64],
        }
    }

    fn token() -> Token {
        Token {
            kind: TOKEN_TYPE.into(),
            keyslots: vec!["3".into()],
            tilo_version: TOKEN_VERSION,
            cert: Token::encode_cert(&cert(3)).unwrap(),
            siblings: vec![Sibling {
                device: "cc".repeat(32),
                label: "Desk".into(),
                endpoint: String::new(),
            }],
            sealing: Sealing::Cleartext,
            material: "e30=".into(),
        }
    }

    #[test]
    fn finds_the_one_token_among_others() {
        let metadata = serde_json::json!({
            "keyslots": {},
            "tokens": {
                "0": { "type": "systemd-recovery", "keyslots": ["1"] },
                "1": serde_json::to_value(token()).unwrap(),
            }
        });
        let found = from_metadata(&metadata.to_string()).unwrap().unwrap();
        assert_eq!(found, token());
        assert_eq!(found.cert().unwrap(), cert(3));
        assert_eq!(found.keyslot().unwrap(), 3);
    }

    #[test]
    fn a_volume_without_the_token_has_no_sibling_factor() {
        let metadata =
            serde_json::json!({ "tokens": { "0": { "type": "systemd-tpm2", "keyslots": ["2"] } } });
        assert!(from_metadata(&metadata.to_string()).unwrap().is_none());
        assert!(from_metadata(r#"{"keyslots":{}}"#).unwrap().is_none());
    }

    #[test]
    fn two_tokens_or_a_bad_slot_fail_closed() {
        let metadata = serde_json::json!({ "tokens": {
            "0": serde_json::to_value(token()).unwrap(),
            "1": serde_json::to_value(token()).unwrap(),
        } });
        assert!(from_metadata(&metadata.to_string()).is_err());

        let mut bad = token();
        bad.keyslots = vec!["3".into(), "4".into()];
        let metadata = serde_json::json!({ "tokens": { "0": serde_json::to_value(bad).unwrap() } });
        assert!(from_metadata(&metadata.to_string()).is_err());

        let mut other_slot = token();
        other_slot.cert = Token::encode_cert(&cert(4)).unwrap();
        assert!(other_slot.validate().is_err());

        let mut lonely = token();
        lonely.siblings.clear();
        assert!(lonely.validate().is_err());
    }
}
