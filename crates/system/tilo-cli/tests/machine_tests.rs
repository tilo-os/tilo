use super::*;

/// `tilo machine add` makes an install plan, mints the single-use token the
/// installation enrolls with, and writes it to a file: the plan names the
/// token by its nonce only, and the token is in no output.
#[test]
fn a_planned_machine_gets_a_plan_and_a_token_in_a_file() {
    let scratch = tempfile::tempdir().unwrap();
    let identity = scratch.path().join("identity");
    let tilo = |args: &[&str]| {
        let out = Command::new(tilo_test_env::package_binary!("tilo"))
            .args(args)
            .env("TILO_IDENTITY_DIR", &identity)
            .output_locked()
            .unwrap();
        assert!(out.status.success(), "tilo {args:?}: {out:?}");
        (
            String::from_utf8(out.stdout).unwrap(),
            String::from_utf8(out.stderr).unwrap(),
        )
    };
    tilo(&["login", "--new-identity", "--name", "Ada"]);
    let (said, _) = tilo(&[
        "machine",
        "add",
        "--name",
        "cupboard",
        "--disk",
        "serial:WD-ABC123",
        "--unlock",
        "passphrase",
        "--hold",
        "0b7e7dfa-3ad5-4b43-9d6c-5f0d7c1f6a11:key-holder",
    ]);
    let plan_id = said
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("plan "))
        .and_then(|rest| rest.split_whitespace().next())
        .expect("the plan is named")
        .to_owned();
    assert_eq!(plan_id.len(), 32, "{said}");
    assert!(said.contains("single use"), "{said}");
    assert!(!said.contains("waiting"), "every choice was made: {said}");

    let plans = identity.join("install-plans");
    let plan: install_profile_model::plan::Plan =
        serde_json::from_slice(&std::fs::read(plans.join(format!("{plan_id}.json"))).unwrap())
            .unwrap();
    assert_eq!(plan.name, "cupboard");
    assert_eq!(plan.destination, "tilo-server");
    assert_eq!(plan.disk.encode(), "serial:WD-ABC123");
    assert!(plan.complete());

    let token_file = plans.join(format!("{plan_id}.token"));
    let token = tilo_user_identity::decode_enrollment_token(
        std::fs::read_to_string(&token_file).unwrap().trim(),
    )
    .unwrap();
    assert_eq!(hex::encode(token.nonce), plan.token.nonce);
    assert!(token.expires_at_ns > token.issued_at_ns);
    assert!(
        !said.contains(&hex::encode(token.nonce)),
        "the token's nonce is not the token, and the token is not printed: {said}"
    );

    let (listed, _) = tilo(&["machine", "list"]);
    assert!(listed.contains(&plan_id), "{listed}");
    assert!(listed.contains("cupboard"), "{listed}");
}

/// A choice left out is open, and the plan says so.
#[test]
fn a_plan_left_open_says_what_its_machine_answers() {
    let scratch = tempfile::tempdir().unwrap();
    let identity = scratch.path().join("identity");
    let tilo = |args: &[&str]| {
        let out = Command::new(tilo_test_env::package_binary!("tilo"))
            .args(args)
            .env("TILO_IDENTITY_DIR", &identity)
            .output_locked()
            .unwrap();
        assert!(out.status.success(), "tilo {args:?}: {out:?}");
        String::from_utf8(out.stdout).unwrap()
    };
    tilo(&["login", "--new-identity", "--name", "Ada"]);
    let said = tilo(&[
        "machine",
        "add",
        "--os",
        "os",
        "--disk",
        "open",
        "--network",
        "open",
        "--unlock",
        "open",
        "--updates",
        "open",
    ]);
    assert!(said.contains("waiting"), "{said}");
    for open in ["name", "disk", "network", "unlock", "updates"] {
        assert!(said.contains(open), "{open} is open: {said}");
    }
}
