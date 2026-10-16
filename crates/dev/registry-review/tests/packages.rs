//! Real signed fixture packages through the private SDK operator actions.
//! Payload bytes are never executed and requests use local uploaded artifacts.

use anyhow::{Context as _, Result, ensure};
use ed25519_dalek::SigningKey;
use registry_review::{App, Draft, PackageReview};
use registry_server::{
    ModerationService, PendingSubmission, PublisherAllowlist, PublisherRow, ServerIdentity, Storage,
};
use registry_types::{
    EntryAction, RegistryEntry, RegistryTarget, RegistryVersion, sign_change_request,
};
use ui::prelude::TestApp;

fn package(directory: &std::path::Path, key: &SigningKey, isa: &str) -> Result<(Vec<u8>, String)> {
    let payload = directory.join("payload");
    std::fs::create_dir_all(payload.join("bin"))?;
    std::fs::write(
        payload.join("bin/notes"),
        b"review fixture: never executable",
    )?;
    let signer = tilo_app_package::SigningIdentity::from_signing_key(key.clone());
    let manifest = tilo_app_package::Manifest::from_toml_str(&format!(
        r#"[package]
id = "com.example.notes"
version = "1.0.0"
kind = "app"
[package.isa]
{isa}
[publisher]
identity = "ed25519:{}"
signature = ""
[sandbox]
namespaces = ["user"]
seccomp = "tilo-default"
landlock = "strict"
[sandbox.net]
mode = "off"
[entrypoint]
bin = "bin/notes"
type = "gui"
"#,
        hex::encode(signer.public_key_bytes())
    ))?;
    let packed = tilo_app_package::pack(&payload, &manifest, &signer)?;
    Ok((
        tilo_app_package::tapp::to_bytes(&packed.meta, Some(&packed))?,
        packed.meta.id.to_hex(),
    ))
}

fn setup() -> Result<(tempfile::TempDir, Storage, TestApp<App>, PendingSubmission)> {
    let directory = tempfile::tempdir()?;
    let storage = Storage::open(directory.path().join("registry"))?;
    storage.save_identity(&ServerIdentity {
        base_url: "https://apps.example".into(),
        ..ServerIdentity::default()
    })?;
    let key = SigningKey::from_bytes(&[29; 32]);
    let publisher = hex::encode(key.verifying_key().to_bytes());
    storage.save_allowlist(&PublisherAllowlist {
        publishers: vec![PublisherRow {
            pubkey_hex: publisher.clone(),
            can_add_new: true,
            namespaces: vec!["com.example".into()],
            note: String::new(),
        }],
    })?;
    let (bytes, root) = package(directory.path(), &key, "x86_64 = \"v3\"")?;
    let digest = storage.save_upload(&bytes)?;
    let entry = RegistryEntry {
        package_id: "com.example.notes".into(),
        kind: registry_types::EntryKind::App,
        name: "Notes".into(),
        description: "A collaborative notebook".into(),
        tagline: "Write together".into(),
        images: registry_types::LocalizedImages::default(),
        supported_languages: vec!["en".into()],
        categories: vec![],
        source_url: None,
        homepage: None,
        publisher_pubkey_hex: publisher,
        signature_hex: String::new(),
        updated_at_unix: 10,
        versions: vec![RegistryVersion {
            version: "1.0.0".into(),
            released_at_unix: 10,
            artifact: registry_types::VersionArtifact::Tapp(registry_types::TappArtifact {
                tapp_url: format!("https://apps.tilo.so/artifacts/{digest}.tapp"),
                tapp_blake3_hex: digest,
                tapp_root_hash_hex: root,
                tapp_size_bytes: bytes.len() as u64,
                target: RegistryTarget {
                    platform: registry_types::TILO_OS_PLATFORM.into(),
                    architecture: "x86_64".into(),
                    isa: "x86_64-v3".into(),
                    runtime: None,
                    interfaces: vec![],
                },
            }),
        }],
    };
    let pending = ModerationService::new(storage.clone())
        .submit(sign_change_request(EntryAction::Add, entry, &key), 100)?;
    let path = directory.path().join("registry");
    let app = TestApp::<App>::launched(&[
        "--private",
        "--registry-dir",
        path.to_str().context("UTF-8 path")?,
    ])?;
    Ok((directory, storage, app, pending))
}

fn inspect(app: &mut TestApp<App>, package: &str) -> Result<ui::serde_json::Value> {
    let queue = app
        .effect(&PackageReview::RefreshPackages)?
        .value
        .context("compact queue")?;
    ensure!(
        queue
            .get("packages")
            .and_then(ui::serde_json::Value::as_array)
            .is_some_and(|rows| rows.len() == 1)
    );
    ensure!(
        !queue.to_string().contains("signature_hex"),
        "discovery is summaries, not full signed entries"
    );
    app.effect(&PackageReview::InspectPackage {
        package: package.into(),
    })?
    .value
    .context("package inspection")
}

fn review_id(value: &ui::serde_json::Value) -> Result<String> {
    value
        .get("review_id")
        .and_then(ui::serde_json::Value::as_str)
        .map(str::to_owned)
        .context("exact review id")
}

fn decision(package: &str, review_id: &str, approve: bool) -> PackageReview {
    if approve {
        PackageReview::ApprovePackage {
            package: package.into(),
            review_id: review_id.into(),
            reviewer: "Owner".into(),
            reason: "Signed sandbox and journeys reviewed".into(),
        }
    } else {
        PackageReview::RejectPackage {
            package: package.into(),
            review_id: review_id.into(),
            reviewer: "Owner".into(),
            reason: "Please revise the \"notes\" permissions; retain the notes.".into(),
        }
    }
}

#[test]
fn verified_agent_inspection_and_confirmed_approval_publish_only_reviewed_bytes() -> Result<()> {
    let (_directory, storage, mut app, pending) = setup()?;
    let result = inspect(&mut app, &pending.submission_id)?;
    ensure!(result.get("inspection_error").is_none());
    let versions = result
        .get("verified_packages")
        .and_then(ui::serde_json::Value::as_array)
        .context("verified versions")?;
    let version = versions.first().context("native version")?;
    ensure!(
        version
            .get("version")
            .and_then(ui::serde_json::Value::as_str)
            == Some("1.0.0")
    );
    ensure!(
        version
            .get("sandbox")
            .context("signed sandbox")?
            .to_string()
            .contains("strict")
    );
    let digest = pending
        .change
        .entry
        .versions
        .first()
        .and_then(registry_types::RegistryVersion::tapp)
        .context("version")?
        .tapp_blake3_hex
        .clone();
    ensure!(
        storage.load_artifact(&digest)?.is_none(),
        "inspection must not publish a pending upload"
    );
    let approve = decision(&pending.submission_id, &review_id(&result)?, true);
    ensure!(
        app.effect(&approve).is_err(),
        "approval requires confirmation"
    );
    ensure!(storage.load_entry(&pending.submission_id)?.is_none());
    app.effect_confirmed(&approve)?;
    ensure!(storage.load_pending(&pending.submission_id)?.is_none());
    ensure!(storage.load_entry(&pending.submission_id)? == Some(pending.change.entry));
    ensure!(storage.load_artifact(&digest)? == storage.load_upload(&digest)?);
    Ok(())
}

#[test]
fn publisher_replacement_refuses_both_stale_decisions_and_preserves_fresh_feedback() -> Result<()> {
    let (_directory, storage, mut app, pending) = setup()?;
    let old = review_id(&inspect(&mut app, &pending.submission_id)?)?;
    let mut entry = pending.change.entry.clone();
    entry.updated_at_unix = 11;
    entry.description = "A newer notebook description".into();
    let replacement = ModerationService::new(storage.clone()).submit(
        sign_change_request(EntryAction::Add, entry, &SigningKey::from_bytes(&[29; 32])),
        101,
    )?;
    for approve in [true, false] {
        ensure!(
            app.effect_confirmed(&decision(&pending.submission_id, &old, approve))
                .is_err()
        );
        ensure!(storage.load_pending(&pending.submission_id)? == Some(replacement.clone()));
        ensure!(storage.load_entry(&pending.submission_id)?.is_none());
    }
    let fresh = review_id(&inspect(&mut app, &pending.submission_id)?)?;
    ensure!(old != fresh);
    let reject = decision(&pending.submission_id, &fresh, false);
    app.effect_confirmed(&reject)?;
    ensure!(storage.load_pending(&pending.submission_id)?.is_none());
    let reviews = storage.load_reviews()?;
    ensure!(reviews.iter().any(|review| review.reason
        == "Please revise the \"notes\" permissions; retain the notes."
        && review.subject == registry_server::ReviewSubject::Submission(replacement.clone())));
    Ok(())
}

#[test]
fn failed_signed_byte_verification_can_be_rejected_but_never_approved() -> Result<()> {
    let (directory, storage, mut app, pending) = setup()?;
    let version = pending
        .change
        .entry
        .versions
        .first()
        .and_then(registry_types::RegistryVersion::tapp)
        .context("version")?;
    let upload = directory
        .path()
        .join("registry/uploads")
        .join(format!("{}.tapp", version.tapp_blake3_hex));
    let mut bytes = std::fs::read(&upload)?;
    let last = bytes.last_mut().context("signed bytes")?;
    *last ^= 1;
    std::fs::write(upload, bytes)?;
    let result = inspect(&mut app, &pending.submission_id)?;
    ensure!(
        result.get("inspection_error").is_some(),
        "production byte inspection observes corruption"
    );
    let id = review_id(&result)?;
    ensure!(
        app.effect_confirmed(&decision(&pending.submission_id, &id, true))
            .is_err()
    );
    ensure!(
        app.harness()
            .semantic_owner_instance("approve-package")
            .is_none()
    );
    ensure!(storage.load_entry(&pending.submission_id)?.is_none());
    ensure!(storage.load_artifact(&version.tapp_blake3_hex)?.is_none());
    app.effect_confirmed(&decision(&pending.submission_id, &id, false))?;
    ensure!(storage.load_pending(&pending.submission_id)?.is_none());
    ensure!(storage.load_entry(&pending.submission_id)?.is_none());
    Ok(())
}

#[test]
fn revoked_owner_cannot_read_package_details_or_replay_confirmed_decisions() -> Result<()> {
    let (_directory, storage, mut app, pending) = setup()?;
    let id = review_id(&inspect(&mut app, &pending.submission_id)?)?;
    app.set_role(ui::Role::Editor);
    ensure!(
        app.harness()
            .semantic_owner_instance("package-review-detail")
            .is_none()
    );
    ensure!(
        app.effect_confirmed(&decision(&pending.submission_id, &id, true))
            .is_err()
    );
    ensure!(storage.load_entry(&pending.submission_id)?.is_none());
    app.set_role(ui::Role::Owner);
    ensure!(
        app.harness()
            .semantic_owner_instance("package-review-detail")
            .is_some()
    );
    Ok(())
}

#[test]
fn package_details_pass_native_audits_in_three_themes_and_two_sizes() -> Result<()> {
    let (_directory, _storage, mut app, pending) = setup()?;
    inspect(&mut app, &pending.submission_id)?;
    app.invoke(Draft::paths().reviewer.replace("Owner").into())?;
    app.invoke(
        Draft::paths()
            .feedback
            .replace("Signed sandbox and journeys reviewed")
            .into(),
    )?;
    for preset in [
        ui::theme::Preset::Light,
        ui::theme::Preset::Dark,
        ui::theme::Preset::HighContrast,
    ] {
        app.harness().set_theme(ui::theme::Theme::preset(preset));
        for size in [(420, 1000), (1280, 1000)] {
            app.harness().resize(size)?;
            let audit = app.harness().native_app_audit();
            ensure!(
                audit.issues.is_empty(),
                "{preset:?} {size:?}: {:?}",
                audit.issues
            );
            ensure!(
                audit.accessibility.issues.is_empty(),
                "{preset:?} {size:?}: {:?}",
                audit.accessibility.issues
            );
            let path = std::path::PathBuf::from("../../../target/registry-package-captures")
                .join(format!("{preset:?}-{}.png", size.0));
            app.harness().render_png(&path);
            ensure!(std::fs::metadata(path)?.len() > 0);
        }
    }
    Ok(())
}

#[test]
fn package_discovery_is_paginated_and_does_not_fetch_unreviewed_urls() -> Result<()> {
    let (_directory, storage, mut app, pending) = setup()?;
    let key = SigningKey::from_bytes(&[29; 32]);
    storage.save_identity(&ServerIdentity {
        base_url: "http://127.0.0.1:9".into(),
        ..ServerIdentity::default()
    })?;
    // Submission accepts signed metadata; byte verification belongs to Inspect.
    // These extra URLs deliberately cannot be fetched by a refresh operation.
    for number in 0..24 {
        let mut entry = pending.change.entry.clone();
        entry.package_id = format!("com.example.notes{number}");
        for version in &mut entry.versions {
            if let Some(tapp) = version.tapp_mut() {
                tapp.tapp_url = "http://127.0.0.1:9/artifacts/never-fetch.tapp".into();
            }
        }
        ModerationService::new(storage.clone()).submit(
            sign_change_request(EntryAction::Add, entry, &key),
            101_u64.saturating_add(number),
        )?;
    }
    let first = app
        .effect(&PackageReview::RefreshPackages)?
        .value
        .context("first page")?;
    ensure!(first.get("pending").and_then(ui::serde_json::Value::as_u64) == Some(25));
    ensure!(
        first
            .get("packages")
            .and_then(ui::serde_json::Value::as_array)
            .is_some_and(|rows| rows.len() == 20)
    );
    let next = app
        .effect(&PackageReview::PackagePage { page: 1 })?
        .value
        .context("second page")?;
    ensure!(
        next.get("packages")
            .and_then(ui::serde_json::Value::as_array)
            .is_some_and(|rows| rows.len() == 5)
    );
    ensure!(
        app.effect(&PackageReview::PackagePage { page: usize::MAX })
            .is_err()
    );
    ensure!(storage.load_entry(&pending.submission_id)?.is_none());
    Ok(())
}

#[test]
fn each_architecture_is_verified_and_has_a_distinct_sandbox_surface() -> Result<()> {
    let (directory, storage, mut app, pending) = setup()?;
    let key = SigningKey::from_bytes(&[29; 32]);
    let (bytes, root) = package(directory.path(), &key, "aarch64 = \"aarch64\"")?;
    let digest = storage.save_upload(&bytes)?;
    let mut entry = pending.change.entry.clone();
    entry.updated_at_unix = 11;
    entry.versions.push(RegistryVersion {
        version: "1.0.0".into(),
        released_at_unix: 11,
        artifact: registry_types::VersionArtifact::Tapp(registry_types::TappArtifact {
            tapp_url: format!("https://apps.tilo.so/artifacts/{digest}.tapp"),
            tapp_blake3_hex: digest.clone(),
            tapp_root_hash_hex: root,
            tapp_size_bytes: bytes.len() as u64,
            target: RegistryTarget {
                platform: registry_types::TILO_OS_PLATFORM.into(),
                architecture: "aarch64".into(),
                isa: "aarch64".into(),
                runtime: None,
                interfaces: vec![],
            },
        }),
    });
    ModerationService::new(storage.clone())
        .submit(sign_change_request(EntryAction::Add, entry, &key), 101)?;
    let result = inspect(&mut app, &pending.submission_id)?;
    ensure!(
        result
            .get("verified_packages")
            .and_then(ui::serde_json::Value::as_array)
            .is_some_and(|versions| versions.len() == 2)
    );
    for target in ["x86_64-x86_64-v3", "aarch64-aarch64"] {
        ensure!(
            app.harness()
                .semantic_owner_instance(&format!("sandbox-1.0.0-tilo-os-{target}"))
                .is_some()
        );
    }
    app.effect_confirmed(&decision(
        &pending.submission_id,
        &review_id(&result)?,
        true,
    ))?;
    ensure!(storage.load_artifact(&digest)? == Some(bytes));
    Ok(())
}
