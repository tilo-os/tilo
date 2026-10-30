use std::path::Path;

use tilo_user_identity::IdentityPaths;

use super::*;
use crate::provisioning::{IdentityStorage, ensure_collaboration_identity};

fn uid() -> u32 {
    rustix::process::geteuid().as_raw()
}

fn account(root: &Path, name: &str, uid: u32) -> Account {
    let home = root.join("home").join(name);
    std::fs::create_dir_all(home.join(IDENTITY_DIR_IN_HOME)).unwrap();
    Account {
        name: name.to_owned(),
        uid,
        gid: uid,
        home,
    }
}

fn proof_path(account: &Account) -> PathBuf {
    account.home.join(IDENTITY_DIR_IN_HOME).join(PROOF_FILE)
}

/// A device on which `ada` (this process's uid) holds the identity the
/// device is bound to, provisioned as a session does it.
fn bound_device(root: &Path) -> (IdentityPaths, Account) {
    let paths = IdentityPaths::under(root.join("system"));
    let ada = account(root, "ada", uid());
    ensure_collaboration_identity(
        &ada.home.join(IDENTITY_DIR_IN_HOME),
        &paths,
        IdentityStorage::Files,
    )
    .unwrap();
    (paths, ada)
}

fn signer(account: &Account) -> runtime_identity::SigningIdentity {
    let dir = account.home.join(IDENTITY_DIR_IN_HOME);
    runtime_identity::load_from_path(runtime_identity::resolve_active_identity(&dir).unwrap())
        .unwrap()
}

/// Of several accounts, the person is the one holding the device's identity
/// key: the one whose proof verifies for its own uid. A proof copied from
/// another account, one signed by another key, and a claim with no proof at
/// all are ignored rather than contested.
#[test]
fn the_account_holding_the_devices_key_is_its_person_and_claims_are_ignored() {
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set(runtime_identity::FILE_SECRETS_ENV, "1");
    let temp = tempfile::tempdir().unwrap();
    let (paths, ada) = bound_device(temp.path());
    assert!(
        proof_path(&ada).is_file(),
        "binding the device wrote ada's proof"
    );

    let copied = account(temp.path(), "bob", uid() + 1);
    std::fs::copy(proof_path(&ada), proof_path(&copied)).unwrap();
    let forged = account(temp.path(), "cy", uid() + 2);
    let (user, device) = device_binding(&paths).unwrap();
    let other = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
    let signature = ed25519_dalek::Signer::sign(&other, &proof_message(&user, &device, forged.uid));
    std::fs::write(proof_path(&forged), signature.to_bytes()).unwrap();
    let claim = account(temp.path(), "dee", uid() + 3);
    std::fs::write(
        claim.home.join(IDENTITY_DIR_IN_HOME).join("registry.json"),
        format!("{{\"default_identity\": \"{}\"}}", hex::encode(user)),
    )
    .unwrap();
    let garbage = account(temp.path(), "eve", uid() + 4);
    std::fs::write(proof_path(&garbage), b"not a signature").unwrap();

    let accounts = [copied, forged, claim, ada.clone(), garbage];
    assert_eq!(find(&paths, &accounts).unwrap(), Person::One(ada));
}

/// A device bound before proofs existed: the person's next sign-in writes
/// the proof, and a stale one (another device's) is replaced.
#[test]
fn a_binding_without_a_proof_gets_one_at_the_next_sign_in() {
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set(runtime_identity::FILE_SECRETS_ENV, "1");
    let temp = tempfile::tempdir().unwrap();
    let (paths, ada) = bound_device(temp.path());
    std::fs::remove_file(proof_path(&ada)).unwrap();
    assert_eq!(
        find(&paths, std::slice::from_ref(&ada)).unwrap(),
        Person::Nobody
    );

    ensure_collaboration_identity(
        &ada.home.join(IDENTITY_DIR_IN_HOME),
        &paths,
        IdentityStorage::Files,
    )
    .unwrap();
    assert_eq!(
        find(&paths, std::slice::from_ref(&ada)).unwrap(),
        Person::One(ada.clone())
    );

    std::fs::write(proof_path(&ada), [0_u8; 64]).unwrap();
    assert!(
        ensure_proof(
            &ada.home.join(IDENTITY_DIR_IN_HOME),
            &signer(&ada),
            &paths,
            ada.uid
        )
        .unwrap()
    );
    assert_eq!(
        find(&paths, std::slice::from_ref(&ada)).unwrap(),
        Person::One(ada)
    );
}

/// Two accounts that both hold the key both prove it: that is for a person
/// to settle, and is answered as such.
#[test]
fn two_accounts_holding_the_key_are_both_answered() {
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set(runtime_identity::FILE_SECRETS_ENV, "1");
    let temp = tempfile::tempdir().unwrap();
    let (paths, ada) = bound_device(temp.path());
    let twin = account(temp.path(), "twin", uid() + 1);
    assert!(
        ensure_proof(
            &twin.home.join(IDENTITY_DIR_IN_HOME),
            &signer(&ada),
            &paths,
            twin.uid
        )
        .unwrap()
    );
    assert_eq!(
        find(&paths, &[ada.clone(), twin.clone()]).unwrap(),
        Person::Several(vec![ada, twin])
    );
}

#[test]
fn regular_accounts_are_read_from_passwd() {
    let text = "root:x:0:0:root:/root:/bin/bash\n\
                ada:x:1000:1000::/home/ada:/bin/bash\n\
                svc:x:990:990::/var/svc:/sbin/nologin\n\
                bob:x:1001:100:Bob:/home/bob:/bin/zsh\n\
                broken line\n\
                nobody:x:65534:65534::/:/sbin/nologin\n";
    let names: Vec<_> = regular_accounts(text)
        .into_iter()
        .map(|a| (a.name, a.uid, a.gid))
        .collect();
    assert_eq!(
        names,
        [
            ("ada".to_owned(), 1000, 1000),
            ("bob".to_owned(), 1001, 100)
        ]
    );
}
