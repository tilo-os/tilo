//! Borrowed legacy completion preflight; never an authority or credit witness.
use serde::{Deserialize, Serialize};

// Field order and borrowed byte representation match the native
// onboarding_http/input.rs Complete/Proof/Join preflight and legacy join.rs.
#[derive(Serialize, Deserialize)]
struct Complete<'a> {
    #[serde(borrow)]
    proof: Proof<'a>,
}
#[derive(Serialize, Deserialize)]
struct Proof<'a> {
    #[serde(borrow)]
    challenge: Join<'a>,
    #[serde(borrow)]
    signature: &'a [u8],
}
#[derive(Serialize, Deserialize)]
struct Join<'a> {
    version: u32,
    #[serde(borrow)]
    signed_invitation: &'a [u8],
    #[serde(borrow)]
    room: &'a str,
    workspace_id: u64,
    authority_device_public_key: [u8; 32],
    user_public_key: [u8; 32],
    device_public_key: [u8; 32],
    request_id: [u8; 32],
    nonce: [u8; 32],
    expires_at_ns: u64,
}

struct Exact<'a>(&'a [u8]);
impl postcard::ser_flavors::Flavor for Exact<'_> {
    type Output = ();
    fn try_push(&mut self, byte: u8) -> postcard::Result<()> {
        self.try_extend(&[byte])
    }
    fn try_extend(&mut self, bytes: &[u8]) -> postcard::Result<()> {
        if !self.0.starts_with(bytes) {
            return Err(postcard::Error::SerializeBufferFull);
        }
        self.0 = &self.0[bytes.len()..];
        Ok(())
    }
    fn finalize(self) -> postcard::Result<()> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(postcard::Error::SerializeBufferFull)
        }
    }
}

/// Checks borrowed shape/canonical bytes without allocation. The returned
/// amount must be reserved before the existing owned decode and held through
/// its result's drop; this function supplies neither credit nor authority.
pub(crate) fn preflight_completion(bytes: &[u8]) -> Result<usize, &'static str> {
    if bytes.is_empty() || bytes.len() > 256 * 1024 {
        return Err("retained completion exceeds bound");
    }
    let (value, tail): (Complete<'_>, _) =
        postcard::take_from_bytes(bytes).map_err(|_| "invalid retained completion shape")?;
    let challenge = &value.proof.challenge;
    if !tail.is_empty()
        || challenge.version != 1
        || challenge.signed_invitation.is_empty()
        || challenge.signed_invitation.len() > 65_536
        || challenge.room.is_empty()
        || challenge.room.len() > 128
        || value.proof.signature.len() != 64
    {
        return Err("retained completion nested bound");
    }
    postcard::serialize_with_flavor(&value, Exact(bytes))
        .map_err(|_| "noncanonical retained completion")?;
    // The legacy bytes64 Deserialize temporarily owns a Vec of 64 bytes.
    challenge
        .signed_invitation
        .len()
        .checked_add(challenge.room.len())
        .and_then(|bytes| bytes.checked_add(64))
        .ok_or("retained completion size overflow")
}
