//! The LUKS2 token that carries this machine's side of a sibling unlock.

use crate::tpm::Sealing;
use anyhow::{Context, Result, bail};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use tilo_unlock_protocol::SiblingContact;
use tilo_user_identity::DeviceCert;

pub const TOKEN_TYPE: &str = "tilo-sibling-unlock";
/// The format a token written now carries: the boot key's certificate is an
/// ordinary `DeviceCert`. A token of an older format is refused rather than
/// migrated; removing it still works.
pub const TOKEN_VERSION: u32 = 2;

/// One of the person's devices this machine asks for the sibling half, as
/// the enrolment recorded it ([`tilo_unlock_protocol::SiblingContact`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sibling {
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Token {
    #[serde(rename = "type")]
    pub kind: String,
    pub keyslots: Vec<String>,
    pub tilo_version: u32,
    /// The boot key's certificate, base64 of its postcard encoding. Public:
    /// it names the boot key, signed by the person's identity, granting
    /// nothing.
    pub cert: String,
    /// The person's devices this machine asks for the half, as the enrolment
    /// recorded them.
    pub siblings: Vec<Sibling>,
    pub sealing: Sealing,
    /// The secret material, base64: a `systemd-creds` blob for
    /// [`Sealing::Tpm2`], the JSON of [`super::material::Material`] for
    /// [`Sealing::Cleartext`].
    pub material: String,
}

impl Token {
    pub fn keyslot(&self) -> Result<u32> {
        match self.keyslots.as_slice() {
            [slot] => slot
                .parse()
                .with_context(|| format!("the token's key slot {slot:?} is not a number")),
            _ => bail!("the token must name exactly one key slot"),
        }
    }

    pub fn cert(&self) -> Result<DeviceCert> {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&self.cert)
            .context("the boot key's certificate is not base64")?;
        postcard::from_bytes(&bytes).context("the boot key's certificate is malformed")
    }

    pub fn encode_cert(cert: &DeviceCert) -> Result<String> {
        Ok(base64::engine::general_purpose::STANDARD.encode(postcard::to_allocvec(cert)?))
    }

    pub fn validate(&self) -> Result<()> {
        if self.kind != TOKEN_TYPE {
            bail!("token type {:?} is not {TOKEN_TYPE}", self.kind);
        }
        if self.tilo_version != TOKEN_VERSION {
            bail!(
                "this volume's sibling unlock was set up by an older Tilo; \
                 remove it and set it up again"
            );
        }
        self.cert()?;
        self.keyslot()?;
        if self.siblings.is_empty() {
            bail!("the token names no device to ask");
        }
        Ok(())
    }
}

/// Finds the sibling-unlock token, and its id, in the LUKS2 header's
/// metadata (`cryptsetup luksDump --dump-json-metadata`). Its format is not
/// checked here, so a token of an older format can still be found and
/// removed; [`Token::validate`] is the caller's check.
///
/// # Errors
/// When the header carries a malformed, slot-ambiguous or more than one
/// sibling-unlock token. `Ok(None)` means the volume has no sibling factor.
pub fn find(metadata: &serde_json::Value) -> Result<Option<(u32, Token)>> {
    let Some(tokens) = metadata
        .get("tokens")
        .and_then(serde_json::Value::as_object)
    else {
        return Ok(None);
    };
    let mut found = None;
    for (id, token) in tokens {
        if token.get("type").and_then(serde_json::Value::as_str) != Some(TOKEN_TYPE) {
            continue;
        }
        if found.is_some() {
            bail!("the volume carries more than one {TOKEN_TYPE} token");
        }
        let id: u32 = id
            .parse()
            .with_context(|| format!("token id {id:?} is not a number"))?;
        let token: Token = serde_json::from_value(token.clone())
            .context("the sibling-unlock token is malformed")?;
        token.keyslot()?;
        found = Some((id, token));
    }
    Ok(found)
}
