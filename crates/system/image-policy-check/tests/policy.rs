//! The shipped container policy passes against the release trust it was
//! generated from; every copy weakened in one Tilo scope while the signed
//! siblings stay intact fails, so only a scope-aware parse passes the suite.
//! The same holds for the chain-of-trust shape an image takes once the
//! offline root is installed. The command line reports as the boot audit
//! expects.
#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "a test fixture: any failed fixture call fails the test"
)]

use std::fs;
use std::path::Path;
use std::process::Command;

use serde_json::{Value, json};
use tilo_image_policy_check::{Failure, Trust, check, check_reference};

const SHIPPED: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../os/etc/containers/policy.json"
);
const RELEASE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../keys/release");
fn binary() -> std::path::PathBuf {
    tilo_test_env::package_binary!("tilo-image-policy-check")
}
const TILO: &str = "registry.tilo.so/tilo/tilo";
const CODE: &str = "code.tilo.so/tilo/tilo";
const GUEST: &str = "registry.tilo.so/tilo/legacy-guest";
const TRUST_ENV: &str = "\
# a comment
TILO_RELEASE_EPOCH=3
TILO_RELEASE_ISSUER_PREFIX=https://signing.example/release/epoch-
TILO_RELEASE_IDENTITY_DOMAIN=signing.example
";

/// The trust the repository's keys/release describes, as an installed image
/// carries it.
fn repository_trust() -> Trust {
    Trust::load(Path::new(RELEASE)).unwrap()
}

fn key_trust() -> Trust {
    Trust::parse(TRUST_ENV, false).unwrap()
}

fn chain_trust() -> Trust {
    Trust::parse(TRUST_ENV, true).unwrap()
}

fn shipped() -> Value {
    serde_json::from_str(&fs::read_to_string(SHIPPED).unwrap()).unwrap()
}

fn accept() -> Value {
    json!([{"type": "insecureAcceptAnything"}])
}

fn key_requirement() -> Value {
    json!([{
        "type": "sigstoreSigned",
        "keyPath": "/usr/share/tilo/cosign.pub",
        "rekorPublicKeyPath": "/usr/share/tilo/release/rekor.pub",
        "signedIdentity": {"type": "matchRepository"},
    }])
}

fn chain_requirement() -> Value {
    json!([{
        "type": "sigstoreSigned",
        "fulcio": {
            "caPath": "/usr/share/tilo/release/root.crt",
            "oidcIssuer": "https://signing.example/release/epoch-3",
            "subjectEmail": "os-images@signing.example",
        },
        "rekorPublicKeyPath": "/usr/share/tilo/release/rekor.pub",
        "signedIdentity": {"type": "matchRepository"},
    }])
}

/// The generator's shape with `requirement` in every signed scope.
fn generated(requirement: &Value) -> Value {
    let mut policy = shipped();
    for scope in [TILO, CODE, GUEST] {
        policy["transports"]["docker"][scope] = requirement.clone();
    }
    policy
}

fn key_policy() -> Value {
    generated(&key_requirement())
}

fn chain_policy() -> Value {
    generated(&chain_requirement())
}

fn weakened(mutate: impl Fn(&mut serde_json::Map<String, Value>)) -> String {
    let mut policy = key_policy();
    mutate(policy["transports"]["docker"].as_object_mut().unwrap());
    serde_json::to_string_pretty(&policy).unwrap()
}

fn policy_failure(text: &str, trust: &Trust) -> String {
    match check(text, trust) {
        Err(Failure::Policy(message)) => message,
        other => panic!("expected a policy failure, got {other:?}"),
    }
}

#[test]
fn the_shipped_policy_proves_the_repository_release_trust() {
    check(&fs::read_to_string(SHIPPED).unwrap(), &repository_trust()).unwrap();
}

#[test]
fn both_trust_modes_have_a_passing_shape() {
    check(&key_policy().to_string(), &key_trust()).unwrap();
    check(&chain_policy().to_string(), &chain_trust()).unwrap();
}

#[test]
fn an_image_with_the_root_may_not_fall_back_to_the_bare_key() {
    assert_eq!(
        policy_failure(&key_policy().to_string(), &chain_trust()),
        "registry.tilo.so/tilo/tilo: pins a bare key, but this image's release trust is the root /usr/share/tilo/release/root.crt"
    );
    assert_eq!(
        policy_failure(&chain_policy().to_string(), &key_trust()),
        "registry.tilo.so/tilo/tilo: chains to a root certificate, but this image installs no /usr/share/tilo/release/root.crt"
    );
}

#[test]
fn chain_requirements_name_the_current_epoch_and_purpose() {
    let chain = |mutate: &dyn Fn(&mut Value)| {
        let mut policy = chain_policy();
        mutate(&mut policy["transports"]["docker"][TILO][0]);
        policy_failure(&policy.to_string(), &chain_trust())
    };
    assert_eq!(
        chain(&|r| r["fulcio"]["oidcIssuer"] = json!("https://signing.example/release/epoch-2")),
        "registry.tilo.so/tilo/tilo: fulcio.oidcIssuer is 'https://signing.example/release/epoch-2', not 'https://signing.example/release/epoch-3'"
    );
    assert_eq!(
        chain(&|r| r["fulcio"]["subjectEmail"] = json!("extension-manifests@signing.example")),
        "registry.tilo.so/tilo/tilo: fulcio.subjectEmail is 'extension-manifests@signing.example', not 'os-images@signing.example'"
    );
    assert_eq!(
        chain(&|r| r["fulcio"]["caPath"] = json!("/etc/pki/other.crt")),
        "registry.tilo.so/tilo/tilo: fulcio.caPath is '/etc/pki/other.crt', not '/usr/share/tilo/release/root.crt'"
    );
    assert_eq!(
        chain(&|r| r["fulcio"]["caData"] = json!("Y2E=")),
        "registry.tilo.so/tilo/tilo: fulcio carries unexpected fields ['caData']"
    );
    assert_eq!(
        chain(&|r| {
            r.as_object_mut().unwrap().remove("rekorPublicKeyPath");
        }),
        "registry.tilo.so/tilo/tilo: no rekorPublicKeyPath, so the signature need not be in the transparency log"
    );
}

/// The boot audit's weakened policies, one scope each.
fn weakened_cases() -> Vec<(&'static str, String)> {
    let mut cases = vec![
        (
            "tilo-scope-accepts-anything",
            weakened(|d| {
                d.insert(TILO.into(), accept());
            }),
        ),
        (
            "code-scope-accepts-anything",
            weakened(|d| {
                d.insert(CODE.into(), accept());
            }),
        ),
        (
            "tilo-scope-other-key",
            weakened(|d| d[TILO][0]["keyPath"] = json!("/etc/pki/other.pub")),
        ),
        (
            "tilo-scope-without-rekor",
            weakened(|d| {
                d[TILO][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("rekorPublicKeyPath");
            }),
        ),
        (
            "tilo-scope-removed",
            weakened(|d| {
                d.remove(TILO);
            }),
        ),
        (
            "tilo-scope-empty",
            weakened(|d| {
                d.insert(TILO.into(), json!([]));
            }),
        ),
        (
            "tilo-scope-gpg-signed-by",
            weakened(|d| {
                d.insert(TILO.into(), json!([{"type": "signedBy", "keyType": "GPGKeys", "keyPath": "/usr/share/tilo/cosign.pub"}]));
            }),
        ),
        (
            "tilo-scope-remapped-identity",
            weakened(|d| {
                d[TILO][0]["signedIdentity"] = json!({"type": "remapIdentity", "prefix": TILO, "signedPrefix": "registry.example/any"});
            }),
        ),
        (
            "tilo-scope-extra-key-data",
            weakened(|d| d[TILO][0]["keyData"] = json!("a2V5")),
        ),
        (
            "tilo-tag-accepts-anything",
            weakened(|d| {
                d.insert(format!("{TILO}:latest"), accept());
            }),
        ),
        (
            "legacy-guest-accepts-anything",
            weakened(|d| {
                d.insert(GUEST.into(), accept());
            }),
        ),
        (
            "tilo-namespace-accepts-anything",
            weakened(|d| {
                d.insert("registry.tilo.so/tilo".into(), accept());
            }),
        ),
        (
            "unlisted-registry-accepts-anything",
            weakened(|d| {
                d.insert("registry.example".into(), accept());
            }),
        ),
    ];
    cases.extend(weakened_documents());
    cases
}

/// Weakened policies that change the document beyond one docker scope.
fn weakened_documents() -> Vec<(&'static str, String)> {
    let docker = key_policy()["transports"]["docker"].clone();
    vec![
        ("default-accepts-anything", {
            let mut policy = key_policy();
            policy["default"] = accept();
            policy.to_string()
        }),
        ("remote-transport-accepts-anything", {
            let mut policy = key_policy();
            policy["transports"]["docker-daemon"] = json!({"": accept()});
            policy.to_string()
        }),
        ("no-docker-scopes", weakened(serde_json::Map::clear)),
        ("malformed", r#"{"transports": {"docker": {"#.to_owned()),
        (
            "duplicate-scope",
            format!(
                r#"{{"transports": {{"docker": {{"{TILO}": {}, "{CODE}": {}, "{TILO}": {}}}}}}}"#,
                docker[TILO],
                docker[CODE],
                accept()
            ),
        ),
    ]
}

#[test]
fn every_weakened_policy_fails() {
    let cases = weakened_cases();
    assert_eq!(
        cases.len(),
        18,
        "plus the missing file: the 19 weakened policies of the boot audit"
    );
    for (name, text) in cases {
        assert!(check(&text, &key_trust()).is_err(), "{name} was accepted");
    }
}

#[test]
fn failures_say_which_scope_and_why() {
    let key = key_trust();
    assert_eq!(
        policy_failure(
            &weakened(|d| {
                d.insert(TILO.into(), accept());
            }),
            &key
        ),
        "registry.tilo.so/tilo/tilo: requirement type is 'insecureAcceptAnything', not 'sigstoreSigned'"
    );
    assert_eq!(
        policy_failure(
            &weakened(|d| d[TILO][0]["keyPath"] = json!("/etc/pki/other.pub")),
            &key
        ),
        "registry.tilo.so/tilo/tilo: keyPath is '/etc/pki/other.pub', not '/usr/share/tilo/cosign.pub'"
    );
    assert_eq!(
        policy_failure(
            &weakened(|d| {
                d[TILO][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("rekorPublicKeyPath");
            }),
            &key
        ),
        "registry.tilo.so/tilo/tilo: no rekorPublicKeyPath, so the signature need not be in the transparency log"
    );
    assert_eq!(
        policy_failure(
            &weakened(|d| d[TILO][0]["rekorPublicKeyPath"] = json!("/tmp/rekor.pub")),
            &key
        ),
        "registry.tilo.so/tilo/tilo: rekorPublicKeyPath is '/tmp/rekor.pub', not '/usr/share/tilo/release/rekor.pub'"
    );
    assert_eq!(
        policy_failure(&weakened(|d| d[TILO][0]["keyData"] = json!("a2V5")), &key),
        "registry.tilo.so/tilo/tilo: sigstoreSigned carries unexpected fields ['keyData']"
    );
    assert_eq!(
        policy_failure(
            &weakened(|d| {
                d.remove(TILO);
            }),
            &key
        ),
        "registry.tilo.so/tilo/tilo: no docker scope, so the default rule applies"
    );
    assert_eq!(
        policy_failure(
            &weakened(|d| d[TILO][0]["signedIdentity"] = json!({"type": "exactRepository"})),
            &key
        ),
        "registry.tilo.so/tilo/tilo: signedIdentity type 'exactRepository' does not bind the image's own repository"
    );
    assert_eq!(
        policy_failure(
            &weakened(|d| d[TILO][0]["signedIdentity"] = json!({"type": 7})),
            &key
        ),
        "registry.tilo.so/tilo/tilo: signedIdentity type 7 does not bind the image's own repository"
    );
    assert_eq!(
        policy_failure(&weakened(|d| d[CODE][0]["type"] = json!(null)), &key),
        "code.tilo.so/tilo/tilo: requirement type is None, not 'sigstoreSigned'"
    );
    assert_eq!(
        policy_failure(
            &weakened(|d| {
                d.insert("registry.example".into(), accept());
            }),
            &key
        ),
        "docker scope 'registry.example': insecureAcceptAnything, which only user-chosen public registries and local transports may use"
    );
    let mut default_accepts = key_policy();
    default_accepts["default"] = accept();
    assert_eq!(
        policy_failure(&default_accepts.to_string(), &key),
        "the default rule is [{'type': 'insecureAcceptAnything'}], not a reject, so an image no scope names is not refused"
    );
    assert_eq!(policy_failure("[]", &key), "policy is not a JSON object");
    assert_eq!(
        policy_failure("{}", &key),
        "policy has no transports object"
    );
    assert_eq!(
        policy_failure(r#"{"transports": {}}"#, &key),
        "policy has no transports.docker object"
    );
    assert_eq!(
        policy_failure(r#"{"transports": {"docker": {"a": 1, "a": 2}}}"#, &key),
        "duplicate key 'a'; the effective rule is ambiguous"
    );
    let unhashable =
        weakened(|d| d[TILO][0]["signedIdentity"] = json!({"type": ["matchRepository"]}));
    assert!(matches!(check(&unhashable, &key), Err(Failure::Crash(_))));
}

#[test]
fn guarded_repositories_are_judged_by_their_effective_rule() {
    // With no scope of their own, the default reject covers them.
    let mut policy = key_policy();
    let docker = policy["transports"]["docker"].as_object_mut().unwrap();
    docker.retain(|name, _| {
        !["legacy-guest", "tilo-vm", "tilo-ci", "tilo-dev"]
            .contains(&name.rsplit('/').next().unwrap())
    });
    check(&policy.to_string(), &key_trust()).unwrap();
    // A namespace scope that rejects covers them too; one that accepts does not.
    policy["transports"]["docker"]["registry.tilo.so/tilo"] = json!([{"type": "reject"}]);
    policy["transports"]["docker"]["code.tilo.so/tilo"] = json!([{"type": "reject"}]);
    check(&policy.to_string(), &key_trust()).unwrap();
    policy["transports"]["docker"]["code.tilo.so/tilo"] = accept();
    assert!(check(&policy.to_string(), &key_trust()).is_err());
}

#[test]
fn only_listed_registries_and_local_transports_accept_anything() {
    let accepts = |mutate: &dyn Fn(&mut Value)| {
        let mut policy = key_policy();
        mutate(&mut policy["transports"]);
        check(&policy.to_string(), &key_trust()).is_ok()
    };
    assert!(accepts(&|t| t["docker"]["quay.io"] = accept()));
    assert!(accepts(&|t| t["docker"]["quay.io/example/app"] = accept()));
    assert!(!accepts(&|t| t["docker"]["quay.io.example"] = accept()));
    assert!(!accepts(
        &|t| t["docker"]["registry.tilo.sox/tilo/x"] = accept()
    ));
    assert!(accepts(&|t| t["oci-archive"] = json!({"": accept()})));
    assert!(!accepts(
        &|t| t["oci-archive"] = json!({"/srv/images": accept()})
    ));
    assert!(!accepts(&|t| t["docker-daemon"] = json!({"": accept()})));
    // Refusing more is always allowed.
    assert!(accepts(
        &|t| t["docker-daemon"] = json!({"": [{"type": "reject"}]})
    ));
}

#[test]
fn release_trust_is_read_strictly() {
    assert_eq!(key_trust(), Trust::Key);
    assert_eq!(
        chain_trust(),
        Trust::Chain {
            issuer: "https://signing.example/release/epoch-3".into(),
            identity: "os-images@signing.example".into(),
        }
    );
    for broken in [
        "TILO_RELEASE_EPOCH=0\nTILO_RELEASE_ISSUER_PREFIX=a\nTILO_RELEASE_IDENTITY_DOMAIN=b\n",
        "TILO_RELEASE_EPOCH=x\nTILO_RELEASE_ISSUER_PREFIX=a\nTILO_RELEASE_IDENTITY_DOMAIN=b\n",
        "TILO_RELEASE_EPOCH=1\nTILO_RELEASE_IDENTITY_DOMAIN=b\n",
        "TILO_RELEASE_EPOCH=1\nexport X=1\n",
        "TILO_RELEASE_EPOCH=1\nnot an assignment\n",
    ] {
        assert!(Trust::parse(broken, true).is_err(), "{broken:?} was read");
    }
}

fn run(args: &[&str]) -> (Option<i32>, String, String) {
    let output = Command::new(binary()).args(args).output().unwrap();
    (
        output.status.code(),
        String::from_utf8(output.stdout).unwrap(),
        String::from_utf8(output.stderr).unwrap(),
    )
}

#[test]
fn the_followed_image_reference_must_require_the_release_signature() {
    let shipped = fs::read_to_string(SHIPPED).unwrap();
    let trust = repository_trust();
    let digest = format!("registry.tilo.so/tilo/tilo@sha256:{}", "a".repeat(64));
    for (transport, image) in [
        ("registry", TILO),
        ("registry", CODE),
        ("registry", digest.as_str()),
        // A tag does not take the reference out of its own scope.
        ("registry", "registry.tilo.so/tilo/tilo:latest"),
        // The image is already on the disk: it fetches nothing, and reached it
        // through the signature-checked installer handoff.
        ("containers-storage", "localhost/tilo-host"),
    ] {
        check_reference(&shipped, transport, image, &trust)
            .unwrap_or_else(|error| panic!("{transport}:{image} was refused: {error:?}"));
    }
    for (transport, image) in [
        // A public registry the policy admits only for a person's own pulls.
        ("registry", "quay.io/x/y"),
        ("registry", "docker.io/library/fedora"),
        // A Tilo repository the policy refuses by name.
        ("registry", "registry.tilo.so/tilo/tilo-dev"),
        ("registry", "registry.tilo.so/tilo/tilo-vm"),
        // A local build no signature accounts for, pulled from a registry.
        ("registry", "localhost/tilo-host"),
        // A transport the policy does not name.
        ("docker-daemon", "anything"),
    ] {
        assert!(
            check_reference(&shipped, transport, image, &trust).is_err(),
            "{transport}:{image} was accepted"
        );
    }
    // An unreadable reference is a failure, never a skip.
    assert!(check_reference(&shipped, "registry", "", &trust).is_err());
    assert!(check_reference(&shipped, "", TILO, &trust).is_err());
    assert!(check_reference("{not json", "registry", TILO, &trust).is_err());
}

#[test]
fn a_registry_pull_is_judged_by_the_docker_transport() {
    // bootc calls it `registry`; containers-policy.json(5) calls it `docker`.
    // A policy whose docker transport is missing must not silently admit a
    // registry image through this name alone.
    let mut policy = key_policy();
    policy["transports"]
        .as_object_mut()
        .unwrap()
        .remove("docker");
    let text = policy.to_string();
    assert_eq!(
        match check_reference(&text, "registry", TILO, &key_trust()) {
            Err(Failure::Policy(message)) => message,
            other => panic!("expected a policy failure, got {other:?}"),
        },
        "the image this machine follows (registry:registry.tilo.so/tilo/tilo) is not a signed Tilo image: \
         the policy has no docker transport, so it says nothing about this image"
    );
}

#[test]
fn a_followed_image_under_a_permissive_default_is_refused() {
    // A Tilo repository with no scope of its own falls to the default rule.
    let mut policy = key_policy();
    policy["default"] = accept();
    policy["transports"]["docker"]
        .as_object_mut()
        .unwrap()
        .remove(TILO);
    let text = policy.to_string();
    assert_eq!(
        match check_reference(&text, "registry", TILO, &key_trust()) {
            Err(Failure::Policy(message)) => message,
            other => panic!("expected a policy failure, got {other:?}"),
        },
        "the image this machine follows (registry:registry.tilo.so/tilo/tilo) is not a signed Tilo image: \
         governed by the default rule: requirement type is 'insecureAcceptAnything', not 'sigstoreSigned'"
    );
    assert!(
        check_reference(&text, "registry", "quay.io/x/y", &key_trust()).is_err(),
        "a permissive default admitted a foreign image"
    );
}

#[test]
fn the_command_line_reports_as_the_audit_reads_it() {
    let (code, out, err) = run(&[SHIPPED, RELEASE]);
    assert_eq!(code, Some(0), "{out}{err}");
    assert!(
        out.starts_with(&format!("{SHIPPED}: Tilo image scopes require ")),
        "{out}"
    );

    let work = tempfile::tempdir().unwrap();
    let trust = work.path().join("release");
    fs::create_dir(&trust).unwrap();
    let trust_dir = trust.to_str().unwrap();

    // No trust.env: fail closed.
    let key = work.path().join("key.json");
    fs::write(&key, key_policy().to_string()).unwrap();
    let (code, out, _) = run(&[key.to_str().unwrap(), trust_dir]);
    assert_eq!(code, Some(1));
    assert!(
        out.contains("cannot read release trust: [Errno 2]"),
        "{out}"
    );

    fs::write(trust.join("trust.env"), TRUST_ENV).unwrap();
    let (code, out, _) = run(&[key.to_str().unwrap(), trust_dir]);
    assert_eq!(code, Some(0), "{out}");
    assert_eq!(
        out,
        format!(
            "{}: Tilo image scopes require the /usr/share/tilo/cosign.pub sigstore signature, logged in Rekor\n",
            key.display()
        )
    );

    // Installing the root switches the requirement: the key policy is now a
    // downgrade and the chain policy passes.
    fs::write(trust.join("root.crt"), "placeholder").unwrap();
    let (code, _, _) = run(&[key.to_str().unwrap(), trust_dir]);
    assert_eq!(code, Some(1));
    let chain = work.path().join("chain.json");
    fs::write(&chain, chain_policy().to_string()).unwrap();
    let (code, out, _) = run(&[chain.to_str().unwrap(), trust_dir]);
    assert_eq!(code, Some(0), "{out}");
    assert_eq!(
        out,
        format!(
            "{}: Tilo image scopes require a Rekor-logged sigstore signature by an operational certificate for os-images@signing.example of https://signing.example/release/epoch-3 under /usr/share/tilo/release/root.crt\n",
            chain.display()
        )
    );

    let missing = work.path().join("missing.json").display().to_string();
    let (code, out, _) = run(&[&missing, trust_dir]);
    assert_eq!(code, Some(1));
    assert_eq!(
        out,
        format!(
            "{missing}: cannot read policy: [Errno 2] No such file or directory: '{missing}'\n"
        )
    );

    let latin = work.path().join("latin.json");
    fs::write(&latin, b"{\"a\": \"\xe9\"}").unwrap();
    let (code, out, _) = run(&[latin.to_str().unwrap(), trust_dir]);
    assert_eq!(code, Some(1));
    assert!(out.ends_with(": cannot read policy: 'utf-8' codec can't decode byte 0xe9 in position 7: invalid continuation byte\n"), "{out}");

    let weak = work.path().join("weak.json");
    let mut weak_policy = chain_policy();
    weak_policy["transports"]["docker"][TILO] = accept();
    fs::write(&weak, weak_policy.to_string()).unwrap();
    let (code, out, _) = run(&[weak.to_str().unwrap(), trust_dir]);
    assert_eq!(code, Some(1));
    assert_eq!(
        out,
        format!(
            "{}: registry.tilo.so/tilo/tilo: requirement type is 'insecureAcceptAnything', not 'sigstoreSigned'\n",
            weak.display()
        )
    );

    let crlf = work.path().join("crlf.json");
    fs::write(
        &crlf,
        fs::read_to_string(SHIPPED).unwrap().replace('\n', "\r\n"),
    )
    .unwrap();
    assert_eq!(
        run(&[crlf.to_str().unwrap(), RELEASE]).0,
        Some(0),
        "universal newlines"
    );

    let usage =
        "usage: tilo-image-policy-check [--reference TRANSPORT IMAGE] [POLICY [TRUST_DIR]]\n";
    let (code, out, err) = run(&["a", "b", "c"]);
    assert_eq!((code, out.as_str(), err.as_str()), (Some(2), "", usage));
    let (code, out, err) = run(&["--reference", "registry"]);
    assert_eq!((code, out.as_str(), err.as_str()), (Some(2), "", usage));
}

#[test]
fn the_command_line_judges_the_followed_image_reference() {
    // The audit's form: the same policy, asked about the image this machine
    // follows instead of about the scopes Tilo publishes.
    let (code, out, err) = run(&["--reference", "registry", TILO, SHIPPED, RELEASE]);
    assert_eq!(code, Some(0), "{out}{err}");
    assert!(
        out.starts_with(&format!(
            "{SHIPPED}: registry:{TILO} requires the /usr/share/tilo/cosign.pub sigstore signature"
        )),
        "{out}"
    );
    let (code, out, _) = run(&["--reference", "registry", "quay.io/x/y", SHIPPED, RELEASE]);
    assert_eq!(code, Some(1), "{out}");
    assert!(
        out.contains(&format!(
            "{SHIPPED}: the image this machine follows (registry:quay.io/x/y) is not a signed Tilo image: governed by quay.io"
        )),
        "{out}"
    );
    let (code, out, _) = run(&[
        "--reference",
        "containers-storage",
        "localhost/tilo-host",
        SHIPPED,
        RELEASE,
    ]);
    assert_eq!(code, Some(0), "{out}");
}

#[test]
fn the_audit_runs_the_compiled_check() {
    let audit = fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../os/usr/libexec/tilo-security-audit"
    ))
    .unwrap();
    assert!(audit.contains(
        "image_policy_check=${TILO_IMAGE_POLICY_CHECK_BIN:-/usr/libexec/tilo-image-policy-check}\n"
    ));
    assert!(
        audit.contains(r#"if ! policy_report="$("$image_policy_check" "$policy" 2>&1)"; then"#)
    );
    // A perfect policy proves nothing about the image bootc actually follows.
    assert!(audit.contains("bootc-status-field booted-image"));
    assert!(audit.contains("bootc-status-field booted-transport"));
    assert!(audit.contains(r#"--reference "$booted_transport" "$booted_image""#));
    // The installed release trust lives at /usr/share/tilo/release on an image
    // and beside the keys in a checkout, so the audit names it.
    assert!(
        audit
            .contains("image_policy_trust=${TILO_IMAGE_POLICY_TRUST_DIR:-/usr/share/tilo/release}")
    );
    assert!(audit.contains(r#""$booted_image" "$policy""#));
    assert!(audit.contains("cannot read the image this machine follows"));
}
