//! A person made from the command line (`tilo login --new-identity`, through
//! [`tilo_identity::provisioning::create_person`]) is the person a session on
//! that account starts as, and a backup of it written on that computer restores in another
//! device's setup as that same person, with a device of its own.

use tilo_device_identity::IdentityPaths;
use tilo_device_identity::persist::{load_device_key, read_device_cert};
use tilo_identity::provisioning::{IdentityStorage, collaboration_identity_paths, create_person};
use tilo_identity::store;

use crate::firstboot::verify_existing_collaboration_identity;

const PASSPHRASE: &[u8] = b"a passphrase of some length";

/// This device's certificate for `user`, checked as signed by them.
fn certificate_of(paths: &IdentityPaths, user: [u8; 32]) -> tilo_user_identity::DeviceCert {
    let cert = read_device_cert(&paths.device_cert_for_user(&user)).unwrap();
    tilo_user_identity::verify_device_cert(&cert).unwrap();
    assert_eq!(cert.user_public_key, user, "the person's root signed it");
    cert
}

#[test]
fn a_person_made_from_the_command_line_is_the_sessions_and_restores_from_its_backup() {
    let mut env = tilo_test_env::ScopedEnv::new();
    let holder = tempfile::tempdir().unwrap();
    env.set("HOME", holder.path())
        .set("XDG_CONFIG_HOME", holder.path().join("config"))
        .set("TILO_IDENTITY_ROOT", holder.path().join("device"))
        .set(tilo_identity::store::FILE_SECRETS_ENV, "1");

    // The command line's way: the session's own identity directory and device root.
    let holder_dir = tilo_identity::store::identity_dir_from_env().unwrap();
    assert_eq!(holder_dir, holder.path().join("config/tilo/identity"));
    let holder_paths = collaboration_identity_paths();
    let user = create_person(
        &holder_dir,
        &holder_paths,
        "Ada",
        "tilo on laptop",
        IdentityStorage::for_session(),
    )
    .unwrap();
    let holder_device = certificate_of(&holder_paths, user).device_public_key;

    // A session on that account starts as them: no setup, and their chain verifies as it is.
    let kept = store::load_identity_file(&store::active_identity_path().unwrap())
        .unwrap()
        .expect("the session finds the person");
    assert_eq!(kept.threshold_identity.group_public_key, user);
    assert_eq!(kept.display_name, "Ada");
    verify_existing_collaboration_identity(&holder_dir, &holder_paths).unwrap();
    assert_eq!(
        certificate_of(&holder_paths, user).device_public_key,
        holder_device,
        "the session keeps the device the command line certified"
    );

    // The holder writes the backup.
    let active = tilo_identity::store::resolve_active_identity(&holder_dir).unwrap();
    let backup = holder.path().join("ada.tilo-backup");
    tilo_identity::store::export_portable_backup(&active, &backup, PASSPHRASE).unwrap();

    // A fresh device's setup restores it: "Use my existing identity", "Restore from a backup".
    let fresh = tempfile::tempdir().unwrap();
    let fresh_dir = fresh.path().join("config/tilo/identity");
    let fresh_paths = IdentityPaths::under(fresh.path().join("device"));
    let restored =
        super::super::import_identity(&fresh_dir, &fresh_paths, &backup, PASSPHRASE).unwrap();
    assert_eq!(restored.fingerprint, hex::encode(user), "the same person");
    let fresh_record = tilo_identity::store::resolve_active_identity(&fresh_dir).unwrap();
    assert!(store::load_identity_file(&fresh_record).unwrap().is_some());
    verify_existing_collaboration_identity(&fresh_dir, &fresh_paths).unwrap();
    let fresh_cert = certificate_of(&fresh_paths, user);
    assert_ne!(
        fresh_cert.device_public_key, holder_device,
        "the restored device has a key of its own"
    );
    assert_eq!(
        fresh_cert.device_public_key,
        load_device_key(&fresh_paths)
            .unwrap()
            .verifying_key()
            .to_bytes()
    );
}
