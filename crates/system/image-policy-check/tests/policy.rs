//! The shipped container policy passes; every copy weakened in one Tilo scope
//! while the signed siblings stay intact fails, so only a scope-aware parse
//! passes the suite. The command line reports as the boot audit expects.
#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "a test fixture: any failed fixture call fails the test"
)]

use std::fs;
use std::process::Command;

use serde_json::{Value, json};
use tilo_image_policy_check::{Failure, check};

const SHIPPED: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../os/etc/containers/policy.json"
);
const BINARY: &str = env!("CARGO_BIN_EXE_tilo-image-policy-check");
const TILO: &str = "registry.tilo.so/tilo/tilo";
const CODE: &str = "code.tilo.so/tilo/tilo";

fn shipped() -> Value {
    serde_json::from_str(&fs::read_to_string(SHIPPED).unwrap()).unwrap()
}

fn accept() -> Value {
    json!([{"type": "insecureAcceptAnything"}])
}

fn weakened(mutate: impl Fn(&mut serde_json::Map<String, Value>)) -> String {
    let mut policy = shipped();
    mutate(policy["transports"]["docker"].as_object_mut().unwrap());
    serde_json::to_string_pretty(&policy).unwrap()
}

fn policy_failure(text: &str) -> String {
    match check(text) {
        Err(Failure::Policy(message)) => message,
        other => panic!("expected a policy failure, got {other:?}"),
    }
}

#[test]
fn the_shipped_policy_requires_the_pinned_signature() {
    check(&fs::read_to_string(SHIPPED).unwrap()).unwrap();
}

/// The boot audit's weakened policies, one Tilo scope each.
fn weakened_cases() -> Vec<(&'static str, String)> {
    let docker = shipped()["transports"]["docker"].clone();
    vec![
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
                d.insert("registry.tilo.so/tilo/legacy-guest".into(), accept());
            }),
        ),
        (
            "legacy-guest-scope-removed",
            weakened(|d| {
                d.remove("registry.tilo.so/tilo/legacy-guest");
            }),
        ),
        (
            "dev-scope-removed",
            weakened(|d| {
                d.remove("registry.tilo.so/tilo/tilo-dev");
            }),
        ),
        (
            "code-ci-scope-removed",
            weakened(|d| {
                d.remove("code.tilo.so/tilo/tilo-ci");
            }),
        ),
        (
            "tilo-namespace-accepts-anything",
            weakened(|d| {
                d.insert("registry.tilo.so/tilo".into(), accept());
            }),
        ),
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
        17,
        "plus the missing file: the 18 weakened policies of the boot audit"
    );
    for (name, text) in cases {
        assert!(check(&text).is_err(), "{name} was accepted");
    }
}

#[test]
fn failures_say_which_scope_and_why() {
    assert_eq!(
        policy_failure(&weakened(|d| {
            d.insert(TILO.into(), accept());
        })),
        "registry.tilo.so/tilo/tilo: requirement type is 'insecureAcceptAnything', not 'sigstoreSigned'"
    );
    assert_eq!(
        policy_failure(&weakened(
            |d| d[TILO][0]["keyPath"] = json!("/etc/pki/other.pub")
        )),
        "registry.tilo.so/tilo/tilo: keyPath is '/etc/pki/other.pub', not '/usr/share/tilo/cosign.pub'"
    );
    assert_eq!(
        policy_failure(&weakened(|d| d[TILO][0]["keyData"] = json!("a2V5"))),
        "registry.tilo.so/tilo/tilo: sigstoreSigned carries unexpected fields ['keyData']"
    );
    assert_eq!(
        policy_failure(&weakened(|d| {
            d.remove(TILO);
        })),
        "registry.tilo.so/tilo/tilo: no docker scope, so the default rule applies"
    );
    assert_eq!(
        policy_failure(&weakened(|d| {
            d.remove("registry.tilo.so/tilo/legacy-guest");
        })),
        "registry.tilo.so/tilo/legacy-guest: governed by the default rule: requirement type is 'insecureAcceptAnything', not 'sigstoreSigned'"
    );
    assert_eq!(
        policy_failure(&weakened(
            |d| d[TILO][0]["signedIdentity"] = json!({"type": "exactRepository"})
        )),
        "registry.tilo.so/tilo/tilo: signedIdentity type 'exactRepository' does not bind the image's own repository"
    );
    assert_eq!(
        policy_failure(&weakened(
            |d| d[TILO][0]["signedIdentity"] = json!({"type": 7})
        )),
        "registry.tilo.so/tilo/tilo: signedIdentity type 7 does not bind the image's own repository"
    );
    assert_eq!(
        policy_failure(&weakened(|d| d[CODE][0]["type"] = json!(null))),
        "code.tilo.so/tilo/tilo: requirement type is None, not 'sigstoreSigned'"
    );
    assert_eq!(policy_failure("[]"), "policy is not a JSON object");
    assert_eq!(policy_failure("{}"), "policy has no transports object");
    assert_eq!(
        policy_failure(r#"{"transports": {}}"#),
        "policy has no transports.docker object"
    );
    assert_eq!(
        policy_failure(r#"{"transports": {"docker": {"a": 1, "a": 2}}}"#),
        "duplicate key 'a'; the effective rule is ambiguous"
    );
    let unhashable =
        weakened(|d| d[TILO][0]["signedIdentity"] = json!({"type": ["matchRepository"]}));
    assert!(matches!(check(&unhashable), Err(Failure::Crash(_))));
}

#[test]
fn guarded_repositories_are_judged_by_their_effective_rule() {
    // With no scope of their own under a default reject (the
    // development-trust shape), they pass.
    let mut policy = shipped();
    policy["default"] = json!([{"type": "reject"}]);
    let docker = policy["transports"]["docker"].as_object_mut().unwrap();
    docker.retain(|name, _| {
        !["legacy-guest", "tilo-vm", "tilo-ci", "tilo-dev"]
            .contains(&name.rsplit('/').next().unwrap())
    });
    check(&policy.to_string()).unwrap();
    // A namespace scope that rejects covers them too; one that accepts does not.
    policy["default"] = accept();
    policy["transports"]["docker"]["registry.tilo.so/tilo"] = json!([{"type": "reject"}]);
    policy["transports"]["docker"]["code.tilo.so/tilo"] = json!([{"type": "reject"}]);
    check(&policy.to_string()).unwrap();
    policy["transports"]["docker"]["code.tilo.so/tilo"] = accept();
    assert!(check(&policy.to_string()).is_err());
}

#[test]
fn unrelated_registries_keep_the_default() {
    let mut policy = shipped();
    policy["transports"]["docker"]["quay.io/example"] = accept();
    policy["transports"]["docker"]["registry.tilo.sox/tilo/x"] = accept();
    check(&policy.to_string()).unwrap();
}

fn run(args: &[&str]) -> (Option<i32>, String, String) {
    let output = Command::new(BINARY).args(args).output().unwrap();
    (
        output.status.code(),
        String::from_utf8(output.stdout).unwrap(),
        String::from_utf8(output.stderr).unwrap(),
    )
}

#[test]
fn the_command_line_reports_as_the_audit_reads_it() {
    let (code, out, err) = run(&[SHIPPED]);
    assert_eq!(code, Some(0), "{out}{err}");
    assert_eq!(
        out,
        format!(
            "{SHIPPED}: Tilo image scopes require the /usr/share/tilo/cosign.pub sigstore signature\n"
        )
    );

    let work = tempfile::tempdir().unwrap();
    let missing = work.path().join("missing.json").display().to_string();
    let (code, out, _) = run(&[&missing]);
    assert_eq!(code, Some(1));
    assert_eq!(
        out,
        format!(
            "{missing}: cannot read policy: [Errno 2] No such file or directory: '{missing}'\n"
        )
    );

    let latin = work.path().join("latin.json");
    fs::write(&latin, b"{\"a\": \"\xe9\"}").unwrap();
    let (code, out, _) = run(&[latin.to_str().unwrap()]);
    assert_eq!(code, Some(1));
    assert!(out.ends_with(": cannot read policy: 'utf-8' codec can't decode byte 0xe9 in position 7: invalid continuation byte\n"), "{out}");

    let weak = work.path().join("weak.json");
    fs::write(
        &weak,
        weakened(|d| {
            d.insert(TILO.into(), accept());
        }),
    )
    .unwrap();
    let (code, out, _) = run(&[weak.to_str().unwrap()]);
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
        run(&[crlf.to_str().unwrap()]).0,
        Some(0),
        "universal newlines"
    );

    let (code, out, err) = run(&["a", "b"]);
    assert_eq!(
        (code, out.as_str(), err.as_str()),
        (Some(2), "", "usage: tilo-image-policy-check [POLICY]\n")
    );
}

#[test]
fn the_audit_runs_the_compiled_check() {
    let audit = fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../os/usr/libexec/tilo-security-audit"
    ))
    .unwrap();
    assert!(audit.contains("image_policy_check=/usr/libexec/tilo-image-policy-check\n"));
    assert!(
        audit.contains(r#"if ! policy_report="$("$image_policy_check" "$policy" 2>&1)"; then"#)
    );
}
