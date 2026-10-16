//! The operator inspects one request and decides that exact displayed snapshot.

use std::path::Path;
use std::process::{Command, Output};

use anyhow::{Result, anyhow, ensure};
use ed25519_dalek::SigningKey;
use registry_server::Storage;
use registry_server::applications::{self, ApplicationStatus, ProofSource};
use registry_types::application::{
    NamespaceClaim, PublisherApplication, dns_txt_name, proof_token,
};

struct Proofs(String);

impl ProofSource for Proofs {
    fn txt_records(&self, name: &str) -> Result<Vec<String>, String> {
        Ok(if name == dns_txt_name("example.com") {
            vec![self.0.clone()]
        } else {
            Vec::new()
        })
    }

    fn https_text(&self, _url: &str) -> Result<String, String> {
        Err("fixture has no HTTPS proof".into())
    }
}

fn fixture() -> Result<(tempfile::TempDir, Storage, PublisherApplication)> {
    let directory = tempfile::tempdir()?;
    let storage = Storage::open(directory.path())?;
    let mut application = PublisherApplication {
        pubkey_hex: String::new(),
        claims: vec![NamespaceClaim::Domain {
            domain: "example.com".into(),
        }],
        contact: "operator-fixture@example.com".into(),
        note: "A native notes app".into(),
        submitted_at_unix: tilo_clock::unix_now(),
        signature_hex: String::new(),
    };
    application.sign(&SigningKey::from_bytes(&[11; 32]));
    applications::submit(
        &storage,
        application.clone(),
        application.submitted_at_unix,
        &Proofs(proof_token(&application.pubkey_hex)),
    )?;
    Ok((directory, storage, application))
}

fn run(directory: &Path, arguments: &[&str]) -> std::io::Result<Output> {
    Command::new(env!("CARGO_BIN_EXE_tilo-registry-server"))
        .arg("--data-dir")
        .arg(directory)
        .args(arguments)
        .output()
}

fn show(directory: &Path, pubkey: &str) -> Result<serde_json::Value> {
    let output = run(directory, &["applications", "show", pubkey, "--json"])?;
    ensure!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(serde_json::from_slice(&output.stdout)?)
}

fn review_id(value: &serde_json::Value) -> Result<&str> {
    value
        .get("review_id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow!("show did not provide a review id"))
}

fn refuse_old_review(
    directory: &Path,
    storage: &Storage,
    candidate: &applications::StoredApplication,
    old_id: &str,
) -> Result<()> {
    for decision in ["approve", "reject"] {
        let mut arguments = vec![
            "applications",
            decision,
            &candidate.application.pubkey_hex,
            "--reviewer",
            "owner",
            "--review-id",
            old_id,
        ];
        if decision == "reject" {
            arguments.extend(["--reason", "Feedback on the previous request"]);
        }
        let refused = run(directory, &arguments)?;
        ensure!(!refused.status.success(), "a stale decision succeeded");
        ensure!(
            String::from_utf8_lossy(&refused.stderr).contains("changed during review"),
            "stale decision did not explain how to recover"
        );
        ensure!(
            storage.load_publisher_application(&candidate.application.pubkey_hex)?
                == Some(candidate.clone()),
            "the stale decision overwrote the replacement"
        );
        ensure!(
            storage.load_allowlist()?.publishers.is_empty(),
            "a stale decision allowlisted the key"
        );
    }

    Ok(())
}

#[test]
fn operator_inspection_and_guarded_feedback_preserve_a_replaced_request() -> Result<()> {
    let (directory, storage, mut application) = fixture()?;
    let old = show(directory.path(), &application.pubkey_hex)?;
    let old_id = review_id(&old)?;
    ensure!(old_id.len() == 64, "review ids are 64 hex characters");
    let list = run(directory.path(), &["applications", "list", "--json"])?;
    ensure!(list.status.success(), "list failed");
    let listed: Vec<applications::StoredApplication> = serde_json::from_slice(&list.stdout)?;
    ensure!(
        listed.len() == 1,
        "the existing list JSON contract is unchanged"
    );

    application.note = "Updated capabilities for review".into();
    application.submitted_at_unix += 1;
    application.sign(&SigningKey::from_bytes(&[11; 32]));
    let replacement = applications::submit(
        &storage,
        application.clone(),
        application.submitted_at_unix,
        &Proofs(proof_token(&application.pubkey_hex)),
    )?;
    refuse_old_review(directory.path(), &storage, &replacement, old_id)?;

    let fresh = show(directory.path(), &application.pubkey_hex)?;
    let fresh_id = review_id(&fresh)?;
    ensure!(
        fresh_id != old_id,
        "a replacement must invalidate the displayed review id"
    );
    let reason = "Describe storage access; keep notes \"literal\".";
    let rejected = run(
        directory.path(),
        &[
            "applications",
            "reject",
            &application.pubkey_hex,
            "--reviewer",
            "owner",
            "--review-id",
            fresh_id,
            "--reason",
            reason,
        ],
    )?;
    ensure!(
        rejected.status.success(),
        "{}",
        String::from_utf8_lossy(&rejected.stderr)
    );
    let decided = storage
        .load_publisher_application(&application.pubkey_hex)?
        .ok_or_else(|| anyhow!("decision did not retain the application"))?;
    ensure!(
        decided.status == ApplicationStatus::Rejected,
        "the fresh decision was not retained"
    );
    ensure!(
        decided.reason.as_deref() == Some(reason),
        "feedback was changed"
    );
    ensure!(
        decided.application == application,
        "the decision changed the signed application"
    );
    let shown = show(directory.path(), &application.pubkey_hex)?;
    let shown_application: applications::StoredApplication = serde_json::from_value(
        shown
            .get("application")
            .cloned()
            .ok_or_else(|| anyhow!("missing application"))?,
    )?;
    ensure!(
        shown_application == decided,
        "show must report the retained decision and feedback"
    );
    Ok(())
}

#[test]
fn the_existing_unguarded_cli_decision_remains_supported() -> Result<()> {
    let (directory, storage, application) = fixture()?;
    let output = run(
        directory.path(),
        &[
            "applications",
            "reject",
            &application.pubkey_hex,
            "--reviewer",
            "owner",
            "--reason",
            "Please add a project URL",
        ],
    )?;
    ensure!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let decided = storage
        .load_publisher_application(&application.pubkey_hex)?
        .ok_or_else(|| anyhow!("decision did not retain the application"))?;
    ensure!(
        decided.reason.as_deref() == Some("Please add a project URL"),
        "legacy feedback was not retained"
    );
    Ok(())
}
