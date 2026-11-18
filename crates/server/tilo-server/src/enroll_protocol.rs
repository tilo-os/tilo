//! The messages one enrollment carries, sealed for the device they are for.
//!
//! The same bytes travel over the local network and over iroh: the request a
//! machine seals to the device that issued its token, the answer that device
//! seals back to the key in that request, the machine's acknowledgement, and
//! the approver's Finalized. Only the way they are carried differs.

use super::{
    Ack, AckPayload, Approver, Context, Finalized, FinalizedPayload, PAIR_DOMAIN, RESPONSE_DOMAIN,
    Request, RequestV2, RequestV2Owned, ResponseRoute, Signature, Signer, Verifier, VerifyingKey,
    bail, verify_signature,
};

pub(super) const ENVELOPE_MAGIC: &[u8; 7] = b"TLOENRL";
pub(super) const ENVELOPE_V2: u8 = 2;

pub(super) fn v2_header() -> [u8; 8] {
    let [m0, m1, m2, m3, m4, m5, m6] = *ENVELOPE_MAGIC;
    [m0, m1, m2, m3, m4, m5, m6, ENVELOPE_V2]
}

/// The sealed box inside `bytes`, or `None` without the envelope prefix.
pub(super) fn v2_body(bytes: &[u8]) -> Option<&[u8]> {
    bytes.strip_prefix(&v2_header()[..])
}

pub(super) fn wrap_v2(sealed: &tilo_seal::SealedBox) -> Vec<u8> {
    let mut out = v2_header().to_vec();
    out.extend(sealed.to_bytes());
    out
}

/// A device's X25519 sealing secret, derived from its Ed25519 key.
pub(super) fn device_sealing_secret(device: &ed25519_dalek::SigningKey) -> tilo_seal::SecretKey {
    tilo_seal::SecretKey::from_bytes(device.to_scalar_bytes())
}

/// A device's X25519 sealing public key, derived from its Ed25519 key.
pub(super) fn device_sealing_public(
    device_public_key: [u8; 32],
) -> anyhow::Result<tilo_seal::PublicKey> {
    Ok(tilo_seal::PublicKey::from_bytes(
        VerifyingKey::from_bytes(&device_public_key)?
            .to_montgomery()
            .to_bytes(),
    ))
}

pub(super) fn pair_offer_aad(recipient: [u8; 32]) -> Vec<u8> {
    [PAIR_DOMAIN, &recipient].concat()
}

/// Seal an enrollment token to the soliciting device.
pub(super) fn encrypt_pair_offer(token: &str, recipient: [u8; 32]) -> anyhow::Result<Vec<u8>> {
    Ok(wrap_v2(&tilo_seal::hpke::seal(
        &device_sealing_public(recipient)?,
        tilo_seal::Label::ServerEnrollOffer,
        &pair_offer_aad(recipient),
        token.as_bytes(),
    )?))
}

pub(super) fn decrypt_pair_offer(
    bytes: &[u8],
    recipient: &ed25519_dalek::SigningKey,
) -> anyhow::Result<String> {
    let recipient_public = recipient.verifying_key().to_bytes();
    let secret = device_sealing_secret(recipient);
    let aad = pair_offer_aad(recipient_public);
    let plaintext = v2_body(bytes)
        .and_then(|body| {
            tilo_seal::hpke::open_bytes(&secret, tilo_seal::Label::ServerEnrollOffer, &aad, body)
                .ok()
        })
        .ok_or_else(|| anyhow::anyhow!("nearby pairing offer was not encrypted for this server"))?;
    String::from_utf8(plaintext.to_vec()).context("nearby pairing offer is not text")
}

pub(super) fn response_aad(token_nonce: [u8; 16], recipient: [u8; 32]) -> Vec<u8> {
    let mut aad = RESPONSE_DOMAIN.to_vec();
    aad.extend(token_nonce);
    aad.extend(recipient);
    aad
}

/// Seal an enrollment request to the issuing device. Returns
/// the envelope and the secret its response will be sealed to.
pub(super) fn encrypt_request(
    request: &Request,
    issuer_device_public_key: [u8; 32],
) -> anyhow::Result<(Vec<u8>, tilo_seal::SecretKey)> {
    let response_secret = tilo_seal::SecretKey::generate()?;
    let plaintext = zeroize::Zeroizing::new(postcard::to_stdvec(&RequestV2 {
        request,
        response_public_key: response_secret.public_key()?.to_bytes(),
    })?);
    let sealed = tilo_seal::hpke::seal(
        &device_sealing_public(issuer_device_public_key)?,
        tilo_seal::Label::ServerEnrollRequest,
        &v2_header(),
        &plaintext,
    )?;
    Ok((wrap_v2(&sealed), response_secret))
}

/// Open an enrollment request, returning where its response must be sealed
/// to.
pub(super) fn decrypt_request(
    bytes: &[u8],
    issuer_device: &ed25519_dalek::SigningKey,
) -> anyhow::Result<(Request, ResponseRoute)> {
    let secret = device_sealing_secret(issuer_device);
    let not_for_us = || anyhow::anyhow!("enrollment request was not encrypted for this issuer");
    let body = v2_body(bytes).ok_or_else(not_for_us)?;
    let plaintext = tilo_seal::hpke::open_bytes(
        &secret,
        tilo_seal::Label::ServerEnrollRequest,
        &v2_header(),
        body,
    )
    .map_err(|_| not_for_us())?;
    let decoded: RequestV2Owned = postcard::from_bytes(&plaintext)?;
    Ok((
        decoded.request,
        ResponseRoute {
            response_public_key: decoded.response_public_key,
        },
    ))
}

/// Seal the signed response to the requester.
pub(super) fn encrypt_response(
    plaintext: &[u8],
    token_nonce: [u8; 16],
    recipient: [u8; 32],
    route: ResponseRoute,
) -> anyhow::Result<Vec<u8>> {
    let aad = response_aad(token_nonce, recipient);
    Ok(wrap_v2(&tilo_seal::hpke::seal(
        &tilo_seal::PublicKey::from_bytes(route.response_public_key),
        tilo_seal::Label::ServerEnrollResponse,
        &aad,
        plaintext,
    )?))
}

pub(super) fn decrypt_response(
    bytes: &[u8],
    token_nonce: [u8; 16],
    recipient: [u8; 32],
    response_secret: &tilo_seal::SecretKey,
) -> anyhow::Result<zeroize::Zeroizing<Vec<u8>>> {
    let aad = response_aad(token_nonce, recipient);
    v2_body(bytes)
        .and_then(|body| {
            tilo_seal::hpke::open_bytes(
                response_secret,
                tilo_seal::Label::ServerEnrollResponse,
                &aad,
                body,
            )
            .ok()
        })
        .ok_or_else(|| anyhow::anyhow!("enrollment response was not encrypted for this device"))
}

pub(super) fn response_digest(bytes: &[u8]) -> [u8; 32] {
    *blake3::hash(bytes).as_bytes()
}

pub(super) fn make_ack(
    response_bytes: &[u8],
    nonce: [u8; 16],
    device: &ed25519_dalek::SigningKey,
) -> anyhow::Result<Vec<u8>> {
    let payload = AckPayload {
        nonce,
        device_public_key: device.verifying_key().to_bytes(),
        response_digest: response_digest(response_bytes),
    };
    let encoded = postcard::to_stdvec(&payload)?;
    let ack = Ack {
        payload,
        signature: device.sign(&encoded).to_bytes().to_vec(),
    };
    Ok(postcard::to_stdvec(&ack)?)
}

pub(super) fn verify_ack(
    bytes: &[u8],
    response_bytes: &[u8],
    nonce: [u8; 16],
    device_public_key: [u8; 32],
) -> anyhow::Result<()> {
    let ack: Ack = postcard::from_bytes(bytes)?;
    if ack.payload.nonce != nonce
        || ack.payload.device_public_key != device_public_key
        || ack.payload.response_digest != response_digest(response_bytes)
    {
        bail!("enrollment acknowledgement did not match the response");
    }
    let signature: [u8; 64] = ack
        .signature
        .as_slice()
        .try_into()
        .map_err(|_| anyhow::anyhow!("acknowledgement signature has the wrong length"))?;
    VerifyingKey::from_bytes(&device_public_key)?.verify(
        &postcard::to_stdvec(&ack.payload)?,
        &Signature::from_bytes(&signature),
    )?;
    Ok(())
}

pub(super) fn make_finalized(
    response_bytes: &[u8],
    nonce: [u8; 16],
    device_public_key: [u8; 32],
    approver: Approver<'_>,
) -> anyhow::Result<Vec<u8>> {
    let payload = FinalizedPayload {
        nonce,
        device_public_key,
        response_digest: response_digest(response_bytes),
    };
    let encoded = postcard::to_stdvec(&payload)?;
    Ok(postcard::to_stdvec(&Finalized {
        payload,
        signature: approver.sign(&encoded)?,
    })?)
}

pub(super) fn verify_finalized(
    bytes: &[u8],
    response_bytes: &[u8],
    nonce: [u8; 16],
    device_public_key: [u8; 32],
    approver_public_key: [u8; 32],
) -> anyhow::Result<()> {
    let finalized: Finalized = postcard::from_bytes(bytes)?;
    if finalized.payload.nonce != nonce
        || finalized.payload.device_public_key != device_public_key
        || finalized.payload.response_digest != response_digest(response_bytes)
    {
        bail!("enrollment completion did not match this exchange");
    }
    verify_signature(
        approver_public_key,
        &postcard::to_stdvec(&finalized.payload)?,
        &finalized.signature,
    )
}
