//! A person made from the command line (`tilo login --new-identity`, through
//! [`tilo_identity::provisioning::create_person`]) is the person a session on
//! that account starts as, and a backup of it written on that computer restores in another
//! device's setup as that same person, with a device of its own.

use tilo_device_identity::IdentityPaths;
use tilo_device_identity::persist::read_device_cert;
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

    // The command line's way: the account's own store and the session's device root.
    let holder_dir = store::account_identity_dir_from_env().unwrap();
    assert_eq!(
        holder_dir,
        store::account_identity_dir(&holder.path().join("config/tilo/identity")),
        "the record is the account's own, in the store the session reads"
    );
    let holder_paths = collaboration_identity_paths();
    // This command makes the person and certifies the device, as the device's daemon does, so it
    // makes this computer's key and takes it first: the account's root is sealed with it.
    tilo_device_identity::persist::load_or_create_device_key(&holder_paths).unwrap();
    let _holder_taken = tilo_device_identity::persist::take_device_key(&holder_paths).unwrap();
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

    // A fresh machine's daemon makes and takes its own device key, as `tilo-device-key-init` and
    // the daemon do at boot, and answers what the device is over its endpoint; the daemon restores
    // the backup with that key, as "Restore from a backup" (`devices#restore`) does.
    let fresh = tempfile::tempdir().unwrap();
    let fresh_dir = fresh.path().join("config/tilo/identity");
    let fresh_paths = IdentityPaths::under(fresh.path().join("device"));
    let daemon = tilo_device_identity::fixture::DeviceDaemon::start(&fresh_paths).unwrap();
    let restored = tilo_identity::provisioning::restore_from_backup(
        &fresh_dir,
        &fresh_paths,
        &backup,
        PASSPHRASE,
        IdentityStorage::for_session(),
    )
    .unwrap();
    assert_eq!(restored.id, hex::encode(user), "the same person");
    // The restored device keeps the person as its own active record, with the root the backup
    // carried, which signs as them. A session's startup check of that device is left out here: a
    // restore into an account's store leaves the record in the provider the import wrote it
    // through, which the session's own storage refuses as "a different secret provider".
    let fresh_record = tilo_identity::store::resolve_active_identity(&fresh_dir).unwrap();
    let came_back = tilo_identity::store::load_from_path(fresh_record)
        .expect("the restored root is the person's");
    assert_eq!(came_back.group_public_key(), user);
    let fresh_cert = certificate_of(&fresh_paths, user);
    assert_ne!(
        fresh_cert.device_public_key, holder_device,
        "the restored device has a key of its own"
    );
    assert_eq!(
        fresh_cert.device_public_key,
        daemon.device_key(),
        "the certificate names the key the machine's daemon signs with"
    );
    // The machine's programs read that device over the daemon's endpoint, as an installed
    // machine's do.
    assert_eq!(
        daemon.actions().device_key().unwrap(),
        daemon.device_key(),
        "the daemon answers what the device is"
    );
}
