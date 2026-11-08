//! A newly installed device enrols its boot key with the person's identity:
//! `tilo-disk-encrypt enrol --sibling` reaches the running session on its socket, the person's root
//! certifies the boot key, and the half that boots the disk is sealed to the person's secret-sync
//! key and published to their other devices.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion"
)]

use std::io::{Read as _, Write as _};
use std::time::{Duration, Instant};

use tilo_compositor::OutputConfig;
use tilo_identity_host::provisioning::{FILE_SECRETS_ENV, IdentityStorage};
use tilo_session::{Options, Session};
use tilo_unlock_protocol::{
    ENROL_SOCKET, EnrolReply, EnrolRequest, Half, PROTOCOL_VERSION, SlotId, decode_frame,
    encode_frame, frame_len,
};
use tilo_user_identity::IdentityPaths;

/// The person's identity in a directory of its own, as the desktop's own start reads it.
fn person() -> (
    std::path::PathBuf,
    tilo_test_env::ScopedEnv,
    tempfile::TempDir,
) {
    let root = tempfile::tempdir().expect("a directory for the identity");
    let xdg = root.path().join("config");
    let system = root.path().join("system");
    let runtime = root.path().join("run");
    std::fs::create_dir_all(&runtime).expect("a runtime directory");
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("XDG_CONFIG_HOME", &xdg)
        .set("TILO_IDENTITY_ROOT", &system)
        .set("XDG_RUNTIME_DIR", &runtime)
        .set(FILE_SECRETS_ENV, "1");
    let personal = xdg.join("tilo/identity");
    let paths = IdentityPaths::under(&system);
    tilo_identity_host::provisioning::ensure_collaboration_identity(
        &personal,
        &paths,
        IdentityStorage::Files,
    )
    .expect("this computer's identity");
    (runtime, env, root)
}

/// This device's fleet key, as the identity store holds it.
fn device_key() -> [u8; 32] {
    let paths = tilo_identity_host::provisioning::collaboration_identity_paths();
    let key = tilo_user_identity::persist::load_device_key(&paths).expect("this device's key");
    key.verifying_key().to_bytes()
}

#[test]
fn a_boot_key_enrols_through_the_sessions_socket() {
    let (runtime, _env, _root) = person();
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            collab: true,
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", 1280, 720)],
    )
    .expect("session");
    let socket = runtime.join(ENROL_SOCKET);
    let boot_key = ed25519_dalek::SigningKey::from_bytes(&[0x77; 32]);
    let half = Half::generate().expect("a fresh half");
    let slot = SlotId {
        volume_uuid: [0x11; 16],
        keyslot: 2,
    };
    let request = EnrolRequest::new(&boot_key, slot, None, half).expect("an enrolment request");
    let frame = encode_frame(&request).expect("a frame");

    // The session serves its socket once it has started.
    let deadline = tilo_test_env::hang_deadline();
    let reply = loop {
        if let Ok(mut stream) = std::os::unix::net::UnixStream::connect(&socket) {
            stream
                .set_read_timeout(Some(tilo_test_env::hang_bound()))
                .expect("a read timeout");
            stream.write_all(&frame).expect("the request is written");
            let mut prefix = [0_u8; 4];
            stream.read_exact(&mut prefix).expect("the reply's prefix");
            let mut body = vec![0_u8; frame_len(prefix).expect("a frame length")];
            stream.read_exact(&mut body).expect("the reply");
            break decode_frame::<EnrolReply>(&body).expect("an enrolment reply");
        }
        session
            .step(Duration::from_millis(20))
            .expect("the session steps");
        assert!(
            Instant::now() < deadline,
            "the session serves its enrolment socket"
        );
    };

    let EnrolReply::Enrolled(response) = reply else {
        panic!("the session enrols the boot key: {reply:?}");
    };
    assert_eq!(response.cert.version, PROTOCOL_VERSION);
    assert_eq!(response.cert.device_public_key, device_key());
    assert_eq!(
        response.cert.boot_public_key,
        boot_key.verifying_key().to_bytes()
    );
    assert_eq!(response.cert.slot, slot);
    response
        .cert
        .verify()
        .expect("the person's root signed the boot key");
    assert!(
        !response.cert.signature.iter().all(|byte| *byte == 0),
        "the certificate carries the root's signature"
    );
}
