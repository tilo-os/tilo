//! A local operator's inspection stays bound to the exact signed request.

use std::path::Path;
use std::process::{Command, Output};

use anyhow::{Context as _, Result, ensure};
use ed25519_dalek::SigningKey;
use registry_server::{
    ModerationService, PendingSubmission, PublisherAllowlist, PublisherRow, Storage,
};
use registry_types::{
    EntryAction, RegistryEntry, RegistryTarget, RegistryVersion, sign_change_request, sign_entry,
};

fn fixture() -> Result<(tempfile::TempDir, Storage, PendingSubmission)> {
    let directory = tempfile::tempdir()?;
    let storage = Storage::open(directory.path())?;
    let key = SigningKey::from_bytes(&[31; 32]);
    let mut entry = RegistryEntry {
        package_id: "com.example.notes".into(),
        kind: registry_types::EntryKind::App,
        name: "Notes".into(),
        description: "Native notes".into(),
        tagline: "Write together".into(),
        images: registry_types::LocalizedImages::default(),
        supported_languages: vec!["en".into()],
        categories: vec![],
        source_url: None,
        homepage: None,
        publisher_pubkey_hex: hex::encode(key.verifying_key().to_bytes()),
        versions: vec![RegistryVersion {
            version: "1.0".into(),
            released_at_unix: 10,
            artifact: registry_types::VersionArtifact::Tapp(registry_types::TappArtifact {
                tapp_url: "https://registry.example/artifacts/notes.tapp".into(),
                tapp_root_hash_hex: "ab".repeat(32),
                tapp_size_bytes: 1,
                tapp_blake3_hex: "cd".repeat(32),
                target: RegistryTarget {
                    platform: registry_types::TILO_OS_PLATFORM.into(),
                    architecture: "x86_64".into(),
                    isa: "x86_64-v3".into(),
                    runtime: None,
                    interfaces: vec![],
                },
            }),
        }],
        updated_at_unix: 10,
        signature_hex: String::new(),
    };
    sign_entry(&mut entry, &key);
    storage.save_entry(&entry)?;
    storage.save_allowlist(&PublisherAllowlist {
        publishers: vec![PublisherRow {
            pubkey_hex: entry.publisher_pubkey_hex.clone(),
            can_add_new: true,
            note: String::new(),
            namespaces: vec!["com.example".into()],
        }],
    })?;
    let pending = ModerationService::new(storage.clone())
        .submit(sign_change_request(EntryAction::Delete, entry, &key), 100)?;
    Ok((directory, storage, pending))
}

fn run(directory: &Path, arguments: &[&str]) -> std::io::Result<Output> {
    Command::new(env!("CARGO_BIN_EXE_tilo-registry-server"))
        .arg("--data-dir")
        .arg(directory)
        .args(arguments)
        .output()
}

fn show(directory: &Path, package: &str) -> Result<serde_json::Value> {
    let output = run(directory, &["review", "show", package, "--json"])?;
    ensure!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(serde_json::from_slice(&output.stdout)?)
}

#[test]
fn stale_package_decisions_refuse_the_replacement_and_fresh_feedback_is_exact() -> Result<()> {
    let (directory, storage, candidate) = fixture()?;
    let inspected = show(directory.path(), &candidate.submission_id)?;
    ensure!(
        serde_json::from_value::<PendingSubmission>(
            inspected.get("submission").context("submission")?.clone()
        )? == candidate
    );
    ensure!(
        inspected
            .get("verified_packages")
            .context("package inspection")?
            .as_array()
            .is_some_and(Vec::is_empty),
        "deleting an entry fetches no packages"
    );
    let review_id = inspected
        .get("review_id")
        .and_then(serde_json::Value::as_str)
        .context("review id")?;
    ensure!(review_id == candidate.review_id()?);
    ensure!(review_id.len() == 64);
    let mut replacement_entry = candidate.change.entry.clone();
    replacement_entry.updated_at_unix = 11;
    let replacement_change = sign_change_request(
        EntryAction::Delete,
        replacement_entry,
        &SigningKey::from_bytes(&[31; 32]),
    );
    let replacement = ModerationService::new(storage.clone()).submit(replacement_change, 101)?;
    for decision in ["approve", "reject"] {
        let output = run(
            directory.path(),
            &[
                "review",
                decision,
                &candidate.submission_id,
                "--reviewer",
                "Owner",
                "--reason",
                "Old feedback",
                "--review-id",
                review_id,
            ],
        )?;
        ensure!(!output.status.success(), "stale {decision} must fail");
        ensure!(String::from_utf8_lossy(&output.stderr).contains("changed after review"));
        ensure!(storage.load_pending(&candidate.submission_id)? == Some(replacement.clone()));
        ensure!(
            storage.load_entry(&candidate.submission_id)?.is_some(),
            "a stale decision never removes the public entry"
        );
    }
    let fresh = show(directory.path(), &candidate.submission_id)?;
    let fresh_id = fresh
        .get("review_id")
        .and_then(serde_json::Value::as_str)
        .context("fresh review id")?;
    ensure!(fresh_id != review_id);
    let feedback = "Keep the \"notes\" app; update the sandbox first.";
    let output = run(
        directory.path(),
        &[
            "review",
            "reject",
            &candidate.submission_id,
            "--reviewer",
            "Owner",
            "--reason",
            feedback,
            "--review-id",
            fresh_id,
        ],
    )?;
    ensure!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let record: registry_server::ReviewRecord = serde_json::from_slice(&output.stdout)?;
    ensure!(record.reason == feedback);
    ensure!(record.subject == registry_server::ReviewSubject::Submission(replacement));
    ensure!(storage.load_pending(&candidate.submission_id)?.is_none());
    ensure!(storage.load_entry(&candidate.submission_id)?.is_some());
    Ok(())
}

#[test]
fn fresh_guarded_approval_and_legacy_rejection_still_use_the_existing_transitions() -> Result<()> {
    let (directory, storage, candidate) = fixture()?;
    let inspected = show(directory.path(), &candidate.submission_id)?;
    let review_id = inspected
        .get("review_id")
        .and_then(serde_json::Value::as_str)
        .context("review id")?;
    let output = run(
        directory.path(),
        &[
            "review",
            "approve",
            &candidate.submission_id,
            "--reviewer",
            "Owner",
            "--reason",
            "Publisher requested removal",
            "--review-id",
            review_id,
        ],
    )?;
    ensure!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let record: registry_server::ReviewRecord = serde_json::from_slice(&output.stdout)?;
    ensure!(record.decision == registry_server::ReviewDecision::Approved);
    ensure!(record.subject == registry_server::ReviewSubject::Submission(candidate.clone()));
    ensure!(storage.load_entry(&candidate.submission_id)?.is_none());
    ensure!(storage.load_pending(&candidate.submission_id)?.is_none());

    let (directory, storage, candidate) = fixture()?;
    let output = run(
        directory.path(),
        &[
            "review",
            "reject",
            &candidate.submission_id,
            "--reviewer",
            "Owner",
            "--reason",
            "Legacy feedback",
        ],
    )?;
    ensure!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    ensure!(storage.load_pending(&candidate.submission_id)?.is_none());
    ensure!(storage.load_entry(&candidate.submission_id)?.is_some());
    Ok(())
}
