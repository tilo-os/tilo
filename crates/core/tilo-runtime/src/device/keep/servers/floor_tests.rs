use tilo_rollback::{counter, exec, policy};

use super::*;

const ATTIC: [u8; 32] = [7; 32];
const CELLAR: [u8; 32] = [8; 32];

fn endpoint() -> String {
    let key = iroh_base::SecretKey::from_bytes(&[7; 32]);
    serde_json::to_string(&iroh_base::EndpointAddr::from_parts(
        key.public(),
        std::iter::empty(),
    ))
    .unwrap()
}

fn root() -> ed25519_dalek::SigningKey {
    ed25519_dalek::SigningKey::from_bytes(&[5; 32])
}

fn secret_sync() -> tilo_seal::SecretKey {
    tilo_seal::SecretKey::from_bytes([3; 32])
}

/// The person's floor key, made as a device holding their root makes it.
fn floor_with_key() -> (Floor, String) {
    let made = make_key(&root(), &secret_sync().public_key().unwrap()).unwrap();
    let mut resource = Replica::<Floor>::new().in_app("so.tilo.model", "Model");
    resource.apply(made.action).unwrap();
    let floor = resource.state().unwrap();
    let private = floor.open_key(&secret_sync()).unwrap().to_string();
    (floor, private)
}

/// The attic, set to follow stable, whose floor takes `key`.
fn servers(key: &str) -> Servers {
    let mut resource = Replica::<Servers>::new().in_app("so.tilo.model", "Model");
    let attic = device_id(&ATTIC);
    for action in [
        Servers::ENROLL_SERVER.with((attic.as_str().to_owned(), "Attic".into(), endpoint())),
        Servers::SET_UPDATES.with((attic.clone(), "stable".into())),
        Servers::SET_FLOOR_KEY.with((attic, key.to_owned())),
    ] {
        resource.apply(action).unwrap();
    }
    resource.state().unwrap()
}

/// What a TPM whose floor is at generation 5, with keyslots up to 8, states.
fn stated(nv_name: &[u8], floor: counter::FloorState) -> String {
    let request = approval::build_request(0x01D9_1100, nv_name, floor, &[6, 7, 8], 3).unwrap();
    serde_json::to_string(&request).unwrap()
}

fn asked(generation: u64, request: String) -> FloorRequest {
    FloorRequest {
        server: device_id(&ATTIC).as_str().to_owned(),
        generation,
        image: "sha256:next".into(),
        updates: "stable".into(),
        request,
        ..FloorRequest::default()
    }
}

/// The key's public half is the root's word, and only the secret-sync key
/// opens its private half.
#[test]
fn the_floor_key_is_signed_by_the_root_and_opens_only_with_secret_sync() {
    let (floor, private) = floor_with_key();
    assert_eq!(
        floor.verified_key(&root().verifying_key().to_bytes()),
        Some(floor.key.as_str())
    );
    let stranger = ed25519_dalek::SigningKey::from_bytes(&[6; 32]);
    assert_eq!(
        floor.verified_key(&stranger.verifying_key().to_bytes()),
        None
    );
    assert!(
        floor
            .open_key(&tilo_seal::SecretKey::from_bytes([4; 32]))
            .is_err()
    );
    assert_eq!(
        tilo_crypto::rsa_public_key_pem(&private).unwrap(),
        floor.key,
        "the sealed half is the public half's key"
    );
}

/// A request is the server's to make and another device's to answer: the
/// server answering its own is refused, and so is a second answer. Each
/// step stays in the resource's history.
#[test]
fn a_server_never_answers_its_own_floor_request() {
    let attic = device_id(&ATTIC).as_str().to_owned();
    let cellar = device_id(&CELLAR).as_str().to_owned();
    let mut resource = Replica::<Floor>::new().in_app("so.tilo.model", "Model");
    resource
        .apply(Floor::REQUEST_FLOOR_ADVANCE.with((
            attic.clone(),
            9,
            "sha256:next".into(),
            "stable".into(),
            "{}".into(),
        )))
        .unwrap();
    let id = request_id(&attic, 9);
    for own in [
        Floor::APPROVE_FLOOR_ADVANCE.with((id.clone(), attic.clone(), "[]".into())),
        Floor::REFUSE_FLOOR_ADVANCE.with((id.clone(), attic.clone(), "no".into())),
    ] {
        let refused = resource.apply(own).unwrap_err().to_string();
        assert!(refused.contains("never answers its own"), "{refused}");
    }
    resource
        .apply(Floor::APPROVE_FLOOR_ADVANCE.with((id.clone(), cellar.clone(), "[]".into())))
        .unwrap();
    let state = resource.state().unwrap();
    let answered = state.requests.get(&id).unwrap();
    assert_eq!(
        (answered.answer.as_str(), answered.answered_by.as_str()),
        (APPROVED, cellar.as_str())
    );
    assert!(
        resource
            .apply(Floor::REFUSE_FLOOR_ADVANCE.with((id, cellar, "late".into())))
            .is_err(),
        "an answered request stays answered"
    );
    assert!(
        resource
            .apply(Floor::REQUEST_FLOOR_ADVANCE.with((
                attic,
                9,
                "sha256:next".into(),
                "stable".into(),
                "{}".into(),
            )))
            .is_err(),
        "and is not asked again"
    );
    assert_eq!(
        resource.log().len(),
        2,
        "the request and its answer are history"
    );
}

/// Approvals never reach past the generation that booted healthy, and a
/// request from a server that is not the person's, whose counter takes
/// another key, or whose image came from updates it does not follow is
/// refused.
#[test]
fn a_request_is_approved_up_to_the_generation_that_booted_and_refused_otherwise() {
    let (floor, private) = floor_with_key();
    let nv_name = [0x00, 0x0B, 1, 2, 3];
    let request = stated(
        &nv_name,
        counter::FloorState {
            counter: 10,
            base: 5,
        },
    );
    let approvals = decide(
        &servers(&floor.key),
        &floor,
        &asked(7, request.clone()),
        &private,
    )
    .unwrap();
    assert_eq!(
        approvals
            .iter()
            .map(|approval| approval.target_floor)
            .collect::<Vec<_>>(),
        [6, 7],
        "the keyslot of generation 8 is not the boot that was judged"
    );

    let stranger = FloorRequest {
        server: device_id(&CELLAR).as_str().to_owned(),
        ..asked(7, request.clone())
    };
    let other_key = servers("another key");
    let dev = FloorRequest {
        updates: "dev".into(),
        ..asked(7, request.clone())
    };
    for (refused, servers, why) in [
        (
            &stranger,
            servers(&floor.key),
            "none of the person's servers",
        ),
        (
            &asked(7, request),
            other_key,
            "does not take the person's floor key",
        ),
        (&dev, servers(&floor.key), "not set to follow"),
    ] {
        let said = decide(&servers, &floor, refused, &private).unwrap_err();
        assert!(said.contains(why), "{said}");
    }
}

/// End to end on an emulated TPM whose floor counter takes the person's
/// floor key: the approvals another device decides for the server's own
/// statement move the counter, one step each, and the TPM takes no other
/// key's.
#[test]
fn approvals_from_the_floor_key_move_an_emulated_tpms_floor() {
    let tpm = match tilo_test_env::swtpm::Swtpm::start() {
        Ok(tpm) => tpm,
        Err(why) => {
            tilo_test_env::capability_unavailable(
                tilo_test_env::Capability::Swtpm,
                format_args!("{why}"),
            );
            return;
        }
    };
    let scratch = tempfile::tempdir().unwrap();
    let runner = exec::Real::new()
        .with_tcti(Some(&tpm.tcti()))
        .with_cwd(scratch.path());
    let ws = policy::Workspace::new(scratch.path());
    let (floor, private) = floor_with_key();
    let authority = scratch.path().join("floor.pub.pem");
    std::fs::write(&authority, &floor.key).unwrap();
    let authority_name = policy::key_name(&runner, &ws, &authority).unwrap();
    let index = counter::DEFAULT_NV_INDEX;
    counter::define(&runner, index, &counter::auth_policy(&authority_name), &ws).unwrap();
    let state =
        counter::materialise(&runner, &ws, index, scratch.path(), 5, &authority_name).unwrap();
    let nv_name = counter::read_name(&runner, index).unwrap();
    let request = approval::build_request(index, &nv_name, state, &[6, 7, 8], 3).unwrap();

    let approvals = decide(
        &servers(&floor.key),
        &floor,
        &asked(7, serde_json::to_string(&request).unwrap()),
        &private,
    )
    .unwrap();
    for approval in &approvals {
        counter::increment_approved(&runner, &ws, index, &authority, &authority_name, approval)
            .unwrap();
    }
    let moved = counter::read(&runner, index, scratch.path())
        .unwrap()
        .unwrap();
    assert_eq!(
        counter::FloorState {
            counter: moved,
            base: state.base
        }
        .floor_generation(),
        7
    );
    // The same approval again is spent, and the TPM refuses it.
    assert!(
        counter::increment_approved(
            &runner,
            &ws,
            index,
            &authority,
            &authority_name,
            &approvals[0]
        )
        .is_err()
    );
}
