//! The device key is the daemon's alone: the other programs of the computer ask the daemon to
//! seal, prove and sign with it, through the client they all use
//! (`tilo_device_identity::login::daemon`), at the native endpoint the daemon listens on. What
//! they get back is what the key made, never the key. A second process taking the key is
//! refused, and with no daemon listening, a program is told the device service isn't running.

use serde_json::{Value, json};
use tilo_device_identity::login::daemon::{DeviceActions, DeviceRefusal};
use tilo_device_identity::login::{
    CHECK_CALLER_PROOF, ENDORSE_IMAGE, OPEN_VAULT, SEAL_VAULT, SEALED_BY_ANOTHER_KEY,
};
use tilo_user_identity::canonical::{canonical_bytes, from_canonical_bytes};
use tilo_user_identity::{ImageEndorsement, ImageSigningAuthorization, StatementSigner as _};

use crate::host;

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
        let cert = tilo_device_identity::persist::read_device_cert(&paths.device_cert).unwrap();
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
