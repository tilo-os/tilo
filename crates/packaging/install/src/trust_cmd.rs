//! `tilo-install trust …`, `revoke` and `revoked`: the attestors this
//! person trusts and the publisher keys they refuse, and `check`, the
//! verdict `app install <file>` asks before it installs.

use tilo_app_package::trust::{AttestorScope, RevokedKey, TrustFile, TrustedAttestor};

use super::{
    Candidate, Error, TrustFlags, check, decode_hex_32, load_revocation_list,
    load_trusted_attestors, revoked_keys_path, trusted_attestors_path,
};

pub(super) fn trust_add(id: &str, pubkey_hex: &str, scope: &str) -> Result<(), Error> {
    // Validate scope + pubkey hex up-front, before touching the file.
    let scope_parsed = AttestorScope::parse(scope).map_err(|e| Error::Trust(e.to_string()))?;
    let _ = decode_hex_32(pubkey_hex).map_err(Error::Trust)?;
    let mut attestors = load_trusted_attestors()?;
    attestors.attestor.retain(|a| a.id != id);
    attestors.attestor.push(TrustedAttestor {
        id: id.into(),
        pubkey_hex: pubkey_hex.into(),
        scope: scope_parsed,
    });
    let path = trusted_attestors_path()?;
    attestors
        .save(&path)
        .map_err(|e| Error::Trust(e.to_string()))?;
    println!("trust: added {id} (scope: {scope})");
    println!("  file: {}", path.display());
    Ok(())
}

pub(super) fn trust_list() -> Result<(), Error> {
    let attestors = load_trusted_attestors()?;
    if attestors.attestor.is_empty() {
        println!("(no trusted attestors)");
        return Ok(());
    }
    println!("{:<24}  {:<12}  pubkey", "id", "scope");
    for a in &attestors.attestor {
        println!("{:<24}  {:<12}  {}", a.id, a.scope.render(), a.pubkey_hex);
    }
    Ok(())
}

pub(super) fn trust_remove(id: &str) -> Result<(), Error> {
    let mut attestors = load_trusted_attestors()?;
    let before = attestors.attestor.len();
    attestors.attestor.retain(|a| a.id != id);
    if attestors.attestor.len() == before {
        return Err(Error::Trust(format!("no trusted attestor with id {id:?}")));
    }
    let path = trusted_attestors_path()?;
    attestors
        .save(&path)
        .map_err(|e| Error::Trust(e.to_string()))?;
    println!("trust: removed {id}");
    Ok(())
}

pub(super) fn revoke(pubkey_hex: &str, reason: &str) -> Result<(), Error> {
    let _ = decode_hex_32(pubkey_hex).map_err(Error::Trust)?;
    let mut list = load_revocation_list()?;
    list.revoked.retain(|k| k.pubkey_hex != pubkey_hex);
    list.revoked.push(RevokedKey {
        pubkey_hex: pubkey_hex.into(),
        reason: reason.into(),
    });
    let path = revoked_keys_path()?;
    list.save(&path).map_err(|e| Error::Trust(e.to_string()))?;
    println!("revoked: ed25519:{pubkey_hex}");
    if !reason.is_empty() {
        println!("  reason: {reason}");
    }
    println!("  file: {}", path.display());
    Ok(())
}

pub(super) fn revoked_list() -> Result<(), Error> {
    let list = load_revocation_list()?;
    if list.revoked.is_empty() {
        println!("(no revoked keys)");
        return Ok(());
    }
    println!("{:<66}  reason", "pubkey");
    for k in &list.revoked {
        println!("{:<66}  {}", k.pubkey_hex, k.reason);
    }
    Ok(())
}

/// `tilo-install check <tapp>`: read and verify a `.tapp` as `install`
/// would, store nothing, and print one JSON object saying who signed it.
///
/// `verdict` is `trusted` when a publisher this person trusts signed it
/// (directly, through an attestor, or the publisher `--require-publisher`
/// named) and `unverified` for a valid self-signed or unknown publisher.
/// A revoked publisher or a bad signature is an error, as for `install`.
pub(super) fn check_verdict(tapp: &std::path::Path, trust: &TrustFlags) -> Result<(), Error> {
    // Self-signed is a verdict here, not a refusal: the caller decides.
    let trust = TrustFlags {
        allow_self_signed: true,
        ..trust.clone()
    };
    let checked = check(&Candidate::File(tapp.to_path_buf()), &trust)?;
    let package = &checked.manifest.package;
    let name = package
        .name
        .clone()
        .unwrap_or_else(|| package_store::display_name_for_package_id(&package.id));
    let verdict = serde_json::json!({
        "id": package.id,
        "name": name,
        "version": package.version,
        "publisher": checked.publisher_key_hex,
        "attestor": checked.attested_by,
        "verdict": if checked.verified { "trusted" } else { "unverified" },
    });
    println!("{verdict}");
    Ok(())
}
