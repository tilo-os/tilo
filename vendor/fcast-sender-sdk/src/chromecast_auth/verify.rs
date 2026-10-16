//! `WebPKI` builds the RFC 5280 path; supplementary Cast checks consume only that
//! verified path, never the receiver's unverified intermediate ordering.
use super::super::wire_proto::Message as _;
use super::super::{auth_crypto, cert_parser, cert_path, pki};
use super::{MAX_CERT, MAX_CHAIN, invalid, proto};
use auth_crypto::{Digest as _, Sha256};
use cert_parser::{
    certificate::X509Certificate, extensions::ParsedExtension, public_key::PublicKey,
};
use pki::{CertificateDer, UnixTime};
use std::io;

fn parse(der: &[u8]) -> io::Result<X509Certificate<'_>> {
    if der.is_empty() || der.len() > MAX_CERT {
        return Err(invalid("Cast certificate size"));
    }
    let (remaining, cert) = cert_parser::parse_x509_certificate(der)
        .map_err(|_| invalid("Malformed Cast certificate"))?;
    if !remaining.is_empty() {
        return Err(invalid("Trailing Cast certificate bytes"));
    }
    cert.extensions_map()
        .map_err(|_| invalid("Duplicate certificate extension"))?;
    Ok(cert)
}
pub(super) fn tls_validity(der: &[u8], now: UnixTime) -> io::Result<UnixTime> {
    let cert = parse(der)?;
    let now = i64::try_from(now.as_secs()).map_err(|_| invalid("Cast clock range"))?;
    if cert.validity().not_before.timestamp() > now
        || cert.validity().not_after.timestamp() < now
        || cert.validity().not_after.timestamp().saturating_sub(now) > 4 * 24 * 60 * 60
    {
        return Err(invalid("Cast TLS leaf validity or lifetime"));
    }
    Ok(UnixTime::since_unix_epoch(std::time::Duration::from_secs(
        u64::try_from(cert.validity().not_after.timestamp())
            .map_err(|_| invalid("Cast TLS expiry range"))?,
    )))
}
pub(super) fn signature(leaf: &[u8], message: &[u8], signature: &[u8]) -> io::Result<()> {
    let der = CertificateDer::from(leaf);
    let cert = cert_path::EndEntityCert::try_from(&der).map_err(io::Error::other)?;
    cert.verify_signature(
        cert_path::ring::RSA_PKCS1_2048_8192_SHA256,
        message,
        signature,
    )
    .map_err(|_| invalid("Invalid Cast RSA/SHA-256 signature"))
}
pub(super) fn chain(
    leaf: &[u8],
    intermediates: &[Vec<u8>],
    roots: &[&[u8]],
    now: UnixTime,
) -> io::Result<Vec<Vec<u8>>> {
    if intermediates.len() > MAX_CHAIN || intermediates.iter().any(|der| der.len() > MAX_CERT) {
        return Err(invalid("Cast certificate chain bound"));
    }
    parse(leaf)?;
    let root_ders: Vec<_> = roots.iter().map(|der| CertificateDer::from(*der)).collect();
    let anchors: Vec<_> = root_ders
        .iter()
        .map(cert_path::anchor_from_trusted_cert)
        .collect::<Result<_, _>>()
        .map_err(io::Error::other)?;
    let intermediates: Vec<_> = intermediates
        .iter()
        .map(|der| CertificateDer::from(der.as_slice()))
        .collect();
    let der = CertificateDer::from(leaf);
    let cert = cert_path::EndEntityCert::try_from(&der).map_err(io::Error::other)?;
    // SHA-1 is absent. PSS certificate signatures are supported with SHA-2;
    // the DeviceAuth proof and version-0 CRL proof remain explicit PKCS1/SHA256.
    let algorithms = [
        cert_path::ring::RSA_PKCS1_2048_8192_SHA256,
        cert_path::ring::RSA_PKCS1_2048_8192_SHA256_ABSENT_PARAMS,
        cert_path::ring::RSA_PKCS1_2048_8192_SHA384,
        cert_path::ring::RSA_PKCS1_2048_8192_SHA384_ABSENT_PARAMS,
        cert_path::ring::RSA_PKCS1_2048_8192_SHA512,
        cert_path::ring::RSA_PKCS1_2048_8192_SHA512_ABSENT_PARAMS,
        cert_path::ring::RSA_PSS_2048_8192_SHA256_LEGACY_KEY,
        cert_path::ring::RSA_PSS_2048_8192_SHA384_LEGACY_KEY,
        cert_path::ring::RSA_PSS_2048_8192_SHA512_LEGACY_KEY,
    ];
    let path = cert
        .verify_for_usage(
            &algorithms,
            &anchors,
            &intermediates,
            now,
            cert_path::KeyUsage::client_auth(),
            None,
            None,
        )
        .map_err(io::Error::other)?;
    let root = anchors
        .iter()
        .position(|anchor| std::ptr::eq(anchor, path.anchor()))
        .ok_or_else(|| invalid("Cast anchor not in private authority set"))?;
    let mut verified = vec![leaf.to_vec()];
    verified.extend(
        path.intermediate_certificates()
            .map(|cert| cert.der().as_ref().to_vec()),
    );
    verified.push(
        roots
            .get(root)
            .ok_or_else(|| invalid("Missing Cast anchor"))?
            .to_vec(),
    );
    for (index, der) in verified.iter().enumerate() {
        certificate_policy(der, index, now)?;
    }
    Ok(verified)
}
fn certificate_policy(der: &[u8], index: usize, now: UnixTime) -> io::Result<()> {
    let cert = parse(der)?;
    let key = cert
        .public_key()
        .parsed()
        .map_err(|_| invalid("Malformed Cast RSA key"))?;
    let PublicKey::RSA(key) = key else {
        return Err(invalid("Cast requires RSA certificate keys"));
    };
    let modulus = key
        .modulus
        .iter()
        .position(|byte| *byte != 0)
        .and_then(|first| key.modulus.get(first..))
        .ok_or_else(|| invalid("Zero RSA modulus"))?;
    let first = modulus
        .first()
        .ok_or_else(|| invalid("Empty RSA modulus"))?;
    let bits = modulus
        .len()
        .checked_mul(8)
        .and_then(|bits| bits.checked_sub(first.leading_zeros() as usize))
        .ok_or_else(|| invalid("RSA modulus size overflow"))?;
    if !(2048..=8192).contains(&bits) {
        return Err(invalid("Cast RSA key size"));
    }
    let time = i64::try_from(now.as_secs()).map_err(|_| invalid("Cast clock range"))?;
    if cert.validity().not_before.timestamp() > time || cert.validity().not_after.timestamp() < time
    {
        return Err(invalid("Cast certificate validity"));
    }
    let usage = cert
        .key_usage()
        .map_err(|_| invalid("Malformed certificate key usage"))?
        .ok_or_else(|| invalid("Missing Cast certificate key usage"))?;
    if (index == 0 && !usage.value.digital_signature())
        || (index != 0 && !usage.value.key_cert_sign())
    {
        return Err(invalid("Cast certificate key usage"));
    }
    if index != 0 {
        let basic = cert
            .basic_constraints()
            .map_err(|_| invalid("Cast CA constraints"))?
            .ok_or_else(|| invalid("Missing Cast CA constraints"))?;
        if !basic.value.ca
            || basic
                .value
                .path_len_constraint
                .is_some_and(|limit| index.saturating_sub(1) > limit as usize)
        {
            return Err(invalid("Cast CA path constraints"));
        }
    }
    for extension in cert.extensions() {
        if matches!(
            extension.parsed_extension(),
            ParsedExtension::ParseError { .. }
        ) || (extension.critical
            && matches!(
                extension.parsed_extension(),
                ParsedExtension::UnsupportedExtension { .. }
            ))
        {
            return Err(invalid("Unsupported or malformed certificate extension"));
        }
    }
    Ok(())
}
pub(super) fn screen_capability(path: &[Vec<u8>]) -> io::Result<()> {
    for der in path {
        let cert = parse(der)?;
        for extension in cert.extensions() {
            if let ParsedExtension::CertificatePolicies(policies) = extension.parsed_extension()
                && policies
                    .iter()
                    .any(|policy| policy.policy_id.to_id_string() == "1.3.6.1.4.1.11129.2.5.2")
            {
                return Err(invalid("Cast receiver identity is audio-only"));
            }
        }
    }
    Ok(())
}
pub(super) fn revocation(
    bundle: Option<&[u8]>,
    roots: &[&[u8]],
    path: &[Vec<u8>],
    now: UnixTime,
) -> io::Result<UnixTime> {
    let bundle = proto::CrlBundle::decode(bundle.ok_or_else(|| invalid("Missing Cast CRL"))?)
        .map_err(|_| invalid("Malformed Cast CRL bundle"))?;
    // An unambiguous current version-0 list is the initial supported profile.
    if bundle.crls.len() != 1 {
        return Err(invalid("Cast CRL bundle version/count"));
    }
    let crl = bundle
        .crls
        .first()
        .ok_or_else(|| invalid("Missing Cast CRL"))?;
    let raw = crl
        .tbs
        .as_deref()
        .ok_or_else(|| invalid("Missing Cast CRL TBS"))?;
    let signer = crl
        .signer_cert
        .as_deref()
        .ok_or_else(|| invalid("Missing Cast CRL signer"))?;
    let proof = crl
        .signature
        .as_deref()
        .ok_or_else(|| invalid("Missing Cast CRL signature"))?;
    let signer_path = chain(signer, &[], roots, now)?;
    signature(signer, raw, proof)?;
    let tbs = proto::TbsCrl::decode(raw).map_err(|_| invalid("Malformed Cast CRL TBS"))?;
    if tbs.version.unwrap_or(0) != 0 {
        return Err(invalid("Unsupported Cast CRL version"));
    }
    let before = tbs
        .not_before_seconds
        .ok_or_else(|| invalid("Missing Cast CRL validity"))?;
    let after = tbs
        .not_after_seconds
        .ok_or_else(|| invalid("Missing Cast CRL validity"))?;
    if before > now.as_secs() || after < now.as_secs() || before > after {
        return Err(invalid("Stale or not-yet-valid Cast CRL"));
    }
    if tbs
        .revoked_public_key_hashes
        .iter()
        .any(|hash| hash.len() != 32)
    {
        return Err(invalid("Cast revoked SPKI hash size"));
    }
    for range in &tbs.revoked_serial_number_ranges {
        if range
            .issuer_public_key_hash
            .as_ref()
            .is_none_or(|hash| hash.len() != 32)
            || range.first_serial_number.is_none_or(|first| first == 0)
            || range
                .last_serial_number
                .zip(range.first_serial_number)
                .is_none_or(|(last, first)| last < first)
        {
            return Err(invalid("Malformed Cast revocation serial range"));
        }
    }
    check_revoked(&tbs, path)?;
    Ok(
        expiry(&signer_path)?.min(UnixTime::since_unix_epoch(std::time::Duration::from_secs(
            after,
        ))),
    )
}
fn check_revoked(tbs: &proto::TbsCrl, path: &[Vec<u8>]) -> io::Result<()> {
    let certs: Vec<_> = path
        .iter()
        .map(|der| parse(der))
        .collect::<Result<_, _>>()?;
    let hashes: Vec<Vec<u8>> = certs
        .iter()
        .map(|cert| Sha256::digest(cert.public_key().raw).to_vec())
        .collect();
    if hashes
        .iter()
        .any(|hash| tbs.revoked_public_key_hashes.contains(hash))
    {
        return Err(invalid("Revoked Cast certificate public key"));
    }
    for (index, hash) in hashes.iter().enumerate().skip(1) {
        for range in &tbs.revoked_serial_number_ranges {
            if range.issuer_public_key_hash.as_ref() == Some(hash) {
                let serial = certs
                    .get(index.saturating_sub(1))
                    .ok_or_else(|| invalid("Missing Cast subordinate"))?
                    .raw_serial();
                let serial = serial
                    .iter()
                    .position(|byte| *byte != 0)
                    .and_then(|first| serial.get(first..))
                    .ok_or_else(|| invalid("Invalid Cast serial"))?;
                if serial.len() > 8 {
                    return Err(invalid("Unrepresentable Cast revoked-range serial"));
                }
                let mut number = [0; 8];
                number
                    .get_mut(8_usize.saturating_sub(serial.len())..)
                    .ok_or_else(|| invalid("Cast serial range"))?
                    .copy_from_slice(serial);
                let number = u64::from_be_bytes(number);
                if range
                    .first_serial_number
                    .is_some_and(|first| number >= first)
                    && range.last_serial_number.is_some_and(|last| number <= last)
                {
                    return Err(invalid("Revoked Cast certificate serial"));
                }
            }
        }
    }
    Ok(())
}

pub(super) fn expiry(path: &[Vec<u8>]) -> io::Result<UnixTime> {
    path.iter()
        .map(|der| {
            let cert = parse(der)?;
            let expiry = u64::try_from(cert.validity().not_after.timestamp())
                .map_err(|_| invalid("Cast certificate expiry range"))?;
            Ok(UnixTime::since_unix_epoch(std::time::Duration::from_secs(
                expiry,
            )))
        })
        .collect::<io::Result<Vec<_>>>()?
        .into_iter()
        .min()
        .ok_or_else(|| invalid("Empty Cast verified path"))
}
