//! The device key is the daemon's alone: the other programs of the computer ask the daemon to
//! seal, prove and sign with it, through the client they all use
//! (`tilo_device_identity::login::daemon`), at the native endpoint the daemon listens on. What
//! they get back is what the key made, never the key. A second process taking the key is
//! refused, and with no daemon listening, a program is told the device service isn't running.

use serde_json::{Value, json};
use tilo_device_identity::login::daemon::{DeviceActions, DeviceRefusal};
use tilo_device_identity::login::{
    CHECK_CALLER_PROOF, ENDORSE_IMAGE, OPEN_VAULT, SEAL_VAULT, SEALED_BY_ANOTHER_KEY, SET_UP_UNLOCK,
};
use tilo_identity::daemon_signer::DaemonSigner;
use tilo_user_identity::canonical::{canonical_bytes, from_canonical_bytes};
use tilo_user_identity::{ImageEndorsement, ImageSigningAuthorization, StatementSigner as _};

use crate::language::host;

/// The bytes the hex field `name` of `answer` names.
fn field(answer: &Value, name: &str) -> Vec<u8> {
    hex::decode(
        answer[name]
            .as_str()
            .unwrap_or_else(|| panic!("{name} in {answer}")),
    )
    .unwrap()
}

#[test]
fn the_daemon_seals_proves_and_signs_with_the_key_it_alone_takes() {
    host::in_own_process(|profile| {
        let paths = tilo_identity::provisioning::collaboration_identity_paths();
        let running = tilo_runtime::daemon::Running::start(profile).unwrap();
        // A program asks at the native endpoint, which waits for the person's services to run.
        let actions = DeviceActions::at(running.native_socket().to_owned());
        let ask = |action: &'static str, args: Value| -> Result<Value, DeviceRefusal> {
            actions.ask(action, args)
        };
        let root = tilo_identity::store::load_from_path(
            tilo_runtime::device::services::device_root_record().expect("the person founded here"),
        )
        .unwrap();

        // A person's Secret Service has its vault sealed and opened by the daemon.
        let salt = hex::encode([5_u8; 32]);
        let vault = b"the person's secrets".to_vec();
        let sealed = ask(
            SEAL_VAULT,
            json!({ "salt": salt, "account": "1000", "plain": hex::encode(&vault) }),
        )
        .expect("the daemon seals the vault");
        let sealed = field(&sealed, "sealed");
        assert!(
            !sealed
                .windows(vault.len())
                .any(|window| window == vault.as_slice()),
            "what is sealed does not show the vault"
        );
        let opened = ask(
            OPEN_VAULT,
            json!({ "salt": salt, "account": "1000", "sealed": hex::encode(&sealed) }),
        )
        .expect("the daemon opens the vault it sealed");
        assert_eq!(field(&opened, "plain"), vault);

        // Another account's vault, or one another machine sealed, does not open here.
        let refused = ask(
            OPEN_VAULT,
            json!({ "salt": salt, "account": "1001", "sealed": hex::encode(&sealed) }),
        )
        .expect_err("another account's store key opens nothing");
        assert_eq!(refused.code, SEALED_BY_ANOTHER_KEY, "{refused:?}");
        let elsewhere =
            tilo_seal::secret_service::seal_vault(&[9_u8; 32], &[5; 32], b"1000", &vault).unwrap();
        let refused = ask(
            OPEN_VAULT,
            json!({ "salt": salt, "account": "1000", "sealed": hex::encode(elsewhere) }),
        )
        .expect_err("a vault another device sealed does not open");
        assert_eq!(refused.code, SEALED_BY_ANOTHER_KEY, "{refused:?}");

        // A process that has the device's identity proves it, and the daemon checks the proof
        // for the Secret Service. This test's process is the daemon's: the key it took is the one
        // a prover has.
        let device = tilo_device_identity::persist::load_device_key(&paths).unwrap();
        let prover = tilo_seal::secret_service::CallerKey::derive(&device.to_bytes()).unwrap();
        let (challenge, caller, program) = ([3_u8; 32], ":1.42", b"/usr/bin/tilo-session");
        let proof = prover.prove(&challenge, caller, program).unwrap();
        let checked = |proof: Vec<u8>| {
            ask(
                CHECK_CALLER_PROOF,
                json!({
                    "challenge": hex::encode(challenge),
                    "caller": caller,
                    "program": hex::encode(program),
                    "proof": hex::encode(proof),
                }),
            )
        };
        assert_eq!(checked(proof.to_vec()).unwrap()["proven"], true);
        let mut forged = proof.to_vec();
        forged[0] ^= 1;
        assert_eq!(checked(forged).unwrap()["proven"], false);

        // The device endorses a system image under the grant its person's root gave it.
        let cert = tilo_device_identity::persist::read_owner_cert(&paths)
            .unwrap()
            .unwrap();
        let now = tilo_clock::unix_now_ns();
        let mut grant = ImageSigningAuthorization {
            user_public_key: cert.user_public_key,
            device_public_key: cert.device_public_key,
            issued_at_ns: now,
            expires_at_ns: now + 60 * 60 * 1_000_000_000,
            signature: [0; 64],
        };
        let digest = tilo_user_identity::image_signing_authorization_digest(&grant).unwrap();
        grant.signature = root.sign_digest(&digest).unwrap();
        let unsigned = ImageEndorsement {
            image_manifest_sha256: [8; 32],
            generation: 3,
            issued_at_ns: now,
            device_certificate: cert.clone(),
            signing_authorization: grant,
            signature: [0; 64],
        };
        let signed = ask(
            ENDORSE_IMAGE,
            json!({ "endorsement": hex::encode(canonical_bytes(&unsigned).unwrap()) }),
        )
        .expect("the device endorses the image");
        let signed: ImageEndorsement =
            from_canonical_bytes(&field(&signed, "endorsement")).unwrap();
        tilo_user_identity::verify_image_endorsement_at(&signed, &cert.user_public_key, now)
            .expect("the endorsement verifies under the person's root");

        // A second process taking the device key is refused: this daemon has it.
        match tilo_device_identity::persist::take_device_key(&paths) {
            Err(tilo_device_identity::PersistError::KeyInUse { pid, .. }) => {
                assert_eq!(pid, Some(std::process::id()), "the daemon is named");
            }
            other => panic!("the key is the daemon's alone: {other:?}"),
        }

        // Once the daemon stops, a program asking it is told the device service isn't running, and
        // is answered nothing else.
        let native = running.native_socket().to_owned();
        running.stop().unwrap();
        let refused = DeviceActions::at(native)
            .ask(
                SEAL_VAULT,
                json!({ "salt": salt, "account": "1000", "plain": hex::encode(&vault) }),
            )
            .expect_err("nothing listens");
        assert!(refused.not_running(), "{refused:?}");
        assert!(
            refused.message.contains("the device service isn't running"),
            "{refused}"
        );
    });
}

/// How long a key request on a fresh computer may take: it is answered from the key the daemon
/// took, never after the person's services start or a workspace is admitted (which a request
/// once waited up to five minutes for).
const AT_ONCE: std::time::Duration = std::time::Duration::from_secs(5);

/// A fresh computer's first sign-in: its daemon starts with a key and no person, signs and seals
/// at once, founds its person as `tilo login --new-identity` does, and its key requests go on
/// being answered at once while that person's services and workspace come up.
#[test]
fn a_fresh_computer_signs_at_once_through_its_first_sign_in() {
    host::in_own_unclaimed_process(|profile| {
        let running = tilo_runtime::daemon::Running::start(profile).unwrap();
        let native = running.native_socket().to_owned();
        let actions = DeviceActions::at(native.clone());
        let info = actions.ask("info", json!({})).unwrap();
        assert_eq!(
            info["people"],
            json!([]),
            "a fresh computer has no person: {info}"
        );

        // What every program signing as the device takes, asked before anyone signed in.
        let signer = DaemonSigner::of(actions.clone()).expect("the daemon says its key");
        let device = ed25519_dalek::VerifyingKey::from_bytes(&signer.public_key()).unwrap();
        let signs_at_once = |what: &[u8]| {
            let asked = std::time::Instant::now();
            let signature = collab_types::signer::sign_now(&signer, what.to_vec())
                .expect("the daemon signs as the device");
            assert!(
                asked.elapsed() < AT_ONCE,
                "signing took {:?}",
                asked.elapsed()
            );
            device
                .verify_strict(what, &ed25519_dalek::Signature::from_bytes(&signature))
                .expect("the signature is the device key's");
        };
        let seals_at_once = || {
            let asked = std::time::Instant::now();
            let salt = hex::encode([5_u8; 32]);
            let sealed = actions
                .ask(
                    SEAL_VAULT,
                    json!({ "salt": salt, "account": "1000", "plain": hex::encode(b"secrets") }),
                )
                .expect("the daemon seals");
            let opened = actions
                .ask(
                    OPEN_VAULT,
                    json!({ "salt": salt, "account": "1000", "sealed": sealed["sealed"] }),
                )
                .expect("the daemon opens what it sealed");
            assert_eq!(field(&opened, "plain"), b"secrets");
            assert!(
                asked.elapsed() < AT_ONCE,
                "sealing took {:?}",
                asked.elapsed()
            );
        };
        signs_at_once(b"a program proves it is this device");
        seals_at_once();

        // A disk unlock asked before any person signed in has no person's services to wait for
        // (a server whose join failed): it is told so at once, never left waiting.
        let asked = std::time::Instant::now();
        let refused = actions
            .ask_followed(
                SET_UP_UNLOCK,
                json!({ "request": "00", "half": hex::encode([0_u8; 32]) }),
            )
            .expect_err("a computer with no person sets up no unlock");
        assert!(
            refused.message.contains("this device has no person yet"),
            "{refused:?}"
        );
        assert!(
            asked.elapsed() < AT_ONCE,
            "refusing took {:?}",
            asked.elapsed()
        );

        // The first sign-in: the computer founds its person, through the same call `tilo` makes.
        let place = tilo_api::daemon::DaemonPlace::at(native.parent().unwrap(), profile);
        place
            .found(&tilo_api::daemon::Founding {
                name: Some("Ada".to_owned()),
                tickets: Vec::new(),
            })
            .expect("the computer founds its person")
            .expect("a fresh computer has no person yet");
        let info = actions.ask("info", json!({})).unwrap();
        assert_eq!(
            info["people"].as_array().map(Vec::len),
            Some(1),
            "the person certified this device: {info}"
        );
        // Straight after it, while the person's services and workspace start, the key answers at
        // once, as the same key.
        signs_at_once(b"the first thing signed after signing in");
        seals_at_once();
        // The daemon goes on answering while those services come up and their first work runs
        // (the apps a trigger starts, the content each window reads): none of it waits on the
        // thread every daemon task runs on. A session that signs in meanwhile asks to found its
        // person too, and is told at once that the computer has one.
        let services_start = std::time::Instant::now();
        while services_start.elapsed() < 3 * AT_ONCE {
            let asked = std::time::Instant::now();
            let refounded = place
                .found(&tilo_api::daemon::Founding::default())
                .expect("a second founding is answered");
            assert!(refounded.is_none(), "the computer has its person already");
            actions.ask("info", json!({})).expect("the daemon answers");
            assert!(
                asked.elapsed() < AT_ONCE,
                "the daemon answered after {:?}, {:?} after its person was founded",
                asked.elapsed(),
                services_start.elapsed()
            );
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
        assert_eq!(
            DaemonSigner::of(actions.clone()).unwrap().public_key(),
            signer.public_key(),
            "signing in keeps the device's key"
        );

        // The key file is the daemon's: a second process taking it is refused.
        let paths = tilo_identity::provisioning::collaboration_identity_paths();
        assert!(matches!(
            tilo_device_identity::persist::take_device_key(&paths),
            Err(tilo_device_identity::PersistError::KeyInUse { .. })
        ));

        running.stop().unwrap();
        let refused = DaemonSigner::of(DeviceActions::at(native))
            .expect_err("nothing listens once the daemon stopped");
        assert!(refused.not_running(), "{refused:?}");
        assert!(
            refused
                .message
                .starts_with("the device service isn't running on this computer"),
            "{refused}"
        );
    });
}

/// A session whose certificate is due asks the daemon to renew it, through the client every
/// program uses: the person's root is sealed with the device key, so the daemon, which alone
/// took the key, opens it and reissues the certificate, and names the person when asked. The
/// session reads only the person's public record and their certificates.
#[test]
fn the_daemon_renews_a_due_certificate_a_session_asks_it_to() {
    host::in_own_process(|profile| {
        const HOUR: u64 = 3_600_000_000_000;
        let paths = tilo_identity::provisioning::collaboration_identity_paths();
        let running = tilo_runtime::daemon::Running::start(profile).unwrap();
        let actions = DeviceActions::at(running.native_socket().to_owned());
        let record =
            tilo_runtime::device::services::device_root_record().expect("the person founded here");
        let person = tilo_identity::store::load_metadata_from_path(&record)
            .unwrap()
            .group_public_key;
        let path = paths.device_cert_for_user(&person);
        let held = tilo_device_identity::persist::read_device_cert(&path).unwrap();

        // Nothing is due yet: asking renews none.
        assert_eq!(actions.renew_certificates().unwrap(), 0);

        // The certificate falls due: it was issued two hours ago for three.
        let now = tilo_clock::unix_now_ns();
        let due = tilo_user_identity::DeviceCert {
            issued_at_ns: now - 2 * HOUR,
            expires_at_ns: Some(now + HOUR),
            ..held.clone()
        };
        assert!(
            tilo_identity::provisioning::renewal_due_at(&due).is_some_and(|at| at <= now),
            "the certificate is due"
        );
        tilo_device_identity::persist::write_device_cert(&path, &due).unwrap();

        assert_eq!(
            actions.renew_certificates().expect("the daemon renews it"),
            1,
            "the due certificate is renewed"
        );
        let renewed = tilo_device_identity::persist::read_device_cert(&path).unwrap();
        assert!(renewed.issued_at_ns >= now, "{renewed:?}");
        assert_eq!(
            renewed
                .expires_at_ns
                .map(|expires| expires - renewed.issued_at_ns),
            Some(3 * HOUR),
            "a renewal keeps the certificate's lifetime"
        );
        assert_eq!(renewed.user_public_key, person);
        assert_eq!(renewed.device_public_key, held.device_public_key);
        tilo_user_identity::verify_device_cert_at(&renewed, tilo_clock::unix_now_ns())
            .expect("the person's root signed the renewal");
        assert_eq!(
            actions.renew_certificates().unwrap(),
            0,
            "nothing is due once renewed"
        );
        // The person's name and picture are the daemon's to write too: their record is sealed with
        // the device key.
        actions
            .act(
                tilo_device_identity::login::SET_PROFILE,
                serde_json::json!({"name": "Ada", "photo": null}),
            )
            .expect("the daemon names the person");
        let named = tilo_identity::store::load_metadata_from_path(&record).unwrap();
        assert_eq!(named.display_name, "Ada");
        running.stop().unwrap();
    });
}
