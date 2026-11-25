//! The attestors the workspace trusts, the publisher keys it refuses, and the
//! verdict `check` prints for a package: reads through the workspace's action
//! API. Changing them is [`tilo_control::packages::call`]'s, the workspace's actions.

use super::{Error, TrustFlags, installer};

pub(super) fn trust_list() {
    let (attestors, _) = super::workspace::trust();
    if attestors.is_empty() {
        println!("(no trusted attestors)");
        return;
    }
    println!("{:<24}  {:<12}  pubkey", "id", "scope");
    for a in &attestors {
        println!("{:<24}  {:<12}  {}", a.id, a.scope, a.pubkey_hex);
    }
}

pub(super) fn revoked_list() {
    let (_, revoked) = super::workspace::trust();
    if revoked.is_empty() {
        println!("(no revoked keys)");
        return;
    }
    println!("{:<66}  reason", "pubkey");
    for k in &revoked {
        println!("{:<66}  {}", k.pubkey_hex, k.reason);
    }
}

/// Read and verify a `.tapp` as `install` would, store nothing, and print one JSON object saying who
/// signed it.
///
/// `verdict` is `trusted` when a publisher the workspace trusts signed it (directly, through an
/// attestor, or the publisher `--require-publisher` named) and `unverified` for a valid self-signed
/// or unknown publisher. A revoked publisher or a bad signature is an error, as for `install`.
pub(super) fn check_verdict(tapp: &std::path::Path, trust: &TrustFlags) -> Result<(), Error> {
    // Self-signed is a verdict here, not a refusal: the caller decides.
    let trust = TrustFlags {
        allow_self_signed: true,
        ..trust.clone()
    };
    let checked = installer::check(
        &installer::Candidate::file(tapp),
        &(&trust).into(),
        &mut installer::Report::default(),
    )?;
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
