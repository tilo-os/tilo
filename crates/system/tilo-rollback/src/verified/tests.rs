//! What a boot has to prove before a PIN-less keyslot may open the volume.
//!
//! These build a firmware description on disk: an efivarfs with a `SecureBoot` value and a
//! `db` signature database, plus a lockdown report. That is the whole input — everything
//! below decides from those bytes alone, so the decision is exercised without a TPM, a
//! firmware, or a machine in a particular Secure Boot state.

use super::*;

/// One minimal DER certificate. The parser here walks EFI signature lists, not X.509, so the
/// payload only has to be distinguishable; `describe_certificate` reports a digest for
/// anything that is not a certificate.
fn cert(tag: u8) -> Vec<u8> {
    vec![0x30, 0x03, 0x02, 0x01, tag]
}

/// `EFI_SIGNATURE_LIST` carrying X.509 entries.
fn x509_list(certs: &[Vec<u8>]) -> Vec<u8> {
    let signature_size = certs.first().map_or(16, |c| c.len().saturating_add(16));
    let mut out = Vec::new();
    out.extend_from_slice(&EFI_CERT_X509_GUID);
    let list_size = signature_size
        .saturating_mul(certs.len())
        .saturating_add(28);
    out.extend_from_slice(&u32::try_from(list_size).unwrap().to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&u32::try_from(signature_size).unwrap().to_le_bytes());
    for c in certs {
        out.extend_from_slice(&[0xAA; 16]);
        out.extend_from_slice(c);
    }
    out
}

/// `EFI_SIGNATURE_LIST` carrying an image hash rather than a key.
fn sha256_list(hash: &[u8]) -> Vec<u8> {
    let signature_size = hash.len().saturating_add(16);
    let mut out = Vec::new();
    out.extend_from_slice(&EFI_CERT_SHA256_GUID);
    out.extend_from_slice(
        &u32::try_from(signature_size.saturating_add(28))
            .unwrap()
            .to_le_bytes(),
    );
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&u32::try_from(signature_size).unwrap().to_le_bytes());
    out.extend_from_slice(&[0xBB; 16]);
    out.extend_from_slice(hash);
    out
}

/// An efivarfs holding just what these tests read.
struct Firmware {
    dir: tempfile::TempDir,
    lockdown: std::path::PathBuf,
}

impl Firmware {
    fn new(secure_boot: Option<bool>, db: Option<&[u8]>, level: Option<&str>) -> Self {
        let dir = tempfile::tempdir().expect("scratch efivarfs");
        if let Some(on) = secure_boot {
            let mut bytes = [0x06, 0, 0, 0].to_vec();
            bytes.push(u8::from(on));
            std::fs::write(dir.path().join(format!("SecureBoot-{DB_GUID}")), bytes).unwrap();
        }
        if let Some(db) = db {
            let mut bytes = vec![0x27, 0, 0, 0];
            bytes.extend_from_slice(db);
            std::fs::write(dir.path().join(format!("db-{DB_GUID}")), bytes).unwrap();
        }
        let lockdown = dir.path().join("lockdown");
        std::fs::write(
            &lockdown,
            match level {
                Some(level) => format!("none [{level}] confidentiality ({level})"),
                None => "none integrity confidentiality".to_string(),
            },
        )
        .unwrap();
        Self { dir, lockdown }
    }

    fn facts(&self) -> BootFacts {
        BootFacts::from_efivars(self.dir.path(), &self.lockdown)
    }
}

#[test]
fn a_boot_is_verified_only_with_secure_boot_a_readable_db_and_lockdown() {
    let db = x509_list(&[cert(1)]);
    assert_eq!(
        verified_boot(&Firmware::new(Some(true), Some(&db), Some("integrity")).facts()),
        Ok(anchor_digest(&db).unwrap()),
        "Secure Boot on, authorities in db, lockdown active is a verified boot"
    );

    // Each condition alone is enough to refuse, and each says which one it is.
    assert_eq!(
        verified_boot(&Firmware::new(None, Some(&db), Some("integrity")).facts()),
        Err(Unverified::NotUefi),
        "no SecureBoot variable is not a UEFI boot, however the rest reads"
    );
    assert_eq!(
        verified_boot(&Firmware::new(Some(false), Some(&db), Some("integrity")).facts()),
        Err(Unverified::SecureBootOff),
        "Secure Boot off means nothing checked the boot image"
    );
    assert_eq!(
        verified_boot(&Firmware::new(Some(true), None, Some("integrity")).facts()),
        Err(Unverified::DatabaseUnreadable(
            "there is no db variable".into()
        )),
        "a db that cannot be read cannot be shown to hold only trusted keys"
    );
    assert_eq!(
        verified_boot(&Firmware::new(Some(true), Some(&db), None).facts()),
        Err(Unverified::LockdownInactive),
        "lockdown not active is not a verified boot even with Secure Boot on"
    );

    // An empty db trusts nothing: every image is refused, so it is not evidence of trust.
    assert!(matches!(
        verified_boot(&Firmware::new(Some(true), Some(&[]), Some("integrity")).facts()),
        Err(Unverified::DatabaseUnreadable(_))
    ));
    assert!(matches!(
        verified_boot(&Firmware::new(Some(true), Some(&x509_list(&[])), Some("integrity")).facts()),
        Err(Unverified::DatabaseUnreadable(_))
    ));
}

#[test]
fn the_anchor_digest_covers_authorities_and_ignores_their_order() {
    let a = x509_list(&[cert(1), cert(2)]);
    let b = x509_list(&[cert(2), cert(1)]);
    assert_eq!(
        anchor_digest(&a).unwrap(),
        anchor_digest(&b).unwrap(),
        "two lists holding the same authorities describe the same trust policy"
    );

    let mut reordered = x509_list(&[cert(1)]);
    reordered.extend_from_slice(&x509_list(&[cert(2)]));
    assert_eq!(
        anchor_digest(&reordered).unwrap(),
        anchor_digest(&a).unwrap(),
        "splitting one authority into its own list does not change what is trusted"
    );

    // The third-party CA case: a db that keeps its own authority and adds another refuses to
    // match the recorded one, which is the whole reason the digest is recorded.
    let grown = x509_list(&[cert(1), cert(2), cert(3)]);
    assert_ne!(anchor_digest(&grown).unwrap(), anchor_digest(&a).unwrap());

    // An image hash can admit exactly one binary and names no authority, so it is neither a
    // trusted key nor a way around one.
    let mut with_hash = x509_list(&[cert(1)]);
    with_hash.extend_from_slice(&sha256_list(&[0x11; 32]));
    assert_eq!(
        anchor_digest(&with_hash).unwrap(),
        anchor_digest(&x509_list(&[cert(1)])).unwrap()
    );
}

#[test]
fn a_truncated_or_malformed_database_is_unreadable_rather_than_empty() {
    let good = x509_list(&[cert(1)]);

    let mut truncated = good.clone();
    truncated.truncate(good.len() - 1);
    assert!(
        anchor_digest(&truncated).is_err(),
        "a signature list that stops mid-entry is not a policy that can be compared"
    );
    assert!(anchor_digest(&good[..20]).is_err());

    let mut lying_size = good.clone();
    lying_size[16..20].copy_from_slice(&0u32.to_le_bytes());
    assert!(anchor_digest(&lying_size).is_err(), "a zero list size");

    let mut tiny_signature = good;
    tiny_signature[24..28].copy_from_slice(&4u32.to_le_bytes());
    assert!(
        anchor_digest(&tiny_signature).is_err(),
        "a signature smaller than its owner GUID"
    );
}

#[test]
fn a_pin_less_slot_opens_only_on_the_boot_that_was_approved() {
    let db = x509_list(&[cert(1)]);
    let facts = Firmware::new(Some(true), Some(&db), Some("integrity")).facts();
    let recorded = verified_boot(&facts).unwrap();

    assert!(AutomaticUnlock::decide(&facts, Some(&recorded)).is_yes());

    // Nothing recorded means nothing to compare against, so the slot is not opened.
    let decision = AutomaticUnlock::decide(&facts, None);
    assert!(!decision.is_yes());
    assert!(
        decision
            .require()
            .unwrap_err()
            .to_string()
            .contains("recorded"),
        "{decision:?}"
    );

    // A db that gained an authority after enrollment is a different trust policy.
    let grown_db = x509_list(&[cert(1), cert(2)]);
    let grown = Firmware::new(Some(true), Some(&grown_db), Some("integrity")).facts();
    let decision = AutomaticUnlock::decide(&grown, Some(&recorded));
    assert!(!decision.is_yes());
    assert!(
        decision
            .require()
            .unwrap_err()
            .to_string()
            .contains("changed"),
        "{decision:?}"
    );

    // And a boot that stopped being verified at all never opens it.
    let off = Firmware::new(Some(false), Some(&db), Some("integrity")).facts();
    assert!(!AutomaticUnlock::decide(&off, Some(&recorded)).is_yes());
    let unlocked = Firmware::new(Some(true), Some(&db), None).facts();
    assert!(
        verified_since_enrollment(&unlocked, &recorded)
            .unwrap_err()
            .contains("lockdown"),
        "a boot with lockdown off is refused by name"
    );
}

#[test]
fn lockdown_counts_only_when_a_level_is_selected() {
    assert_eq!(
        active_lockdown("none [integrity] confidentiality (integrity)\n").as_deref(),
        Some("integrity")
    );
    assert_eq!(
        active_lockdown("none integrity [confidentiality]").as_deref(),
        Some("confidentiality")
    );
    assert_eq!(active_lockdown("none integrity confidentiality"), None);
    assert_eq!(active_lockdown(""), None);
    assert_eq!(active_lockdown("unknown format"), None);
}

#[test]
fn the_recorded_policy_survives_a_round_trip_and_is_not_world_readable() {
    let db = x509_list(&[cert(1)]);
    let facts = Firmware::new(Some(true), Some(&db), Some("integrity")).facts();
    let enrollment = require_verified(&facts).expect("a verified boot enrolls");
    assert_eq!(enrollment.authorities.len(), 1);

    let state = tempfile::tempdir().unwrap();
    let state_path = state.path().join("rollback/state.json");
    assert_eq!(load_enrollment(&state_path), None);
    save_enrollment(&state_path, &enrollment).unwrap();
    let recorded = load_enrollment(&state_path).expect("the record reads back");
    assert_eq!(recorded, enrollment.anchors);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(enrollment_path(&state_path))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            mode, 0o600,
            "the record names no secret but is still private"
        );
    }
}
