use anyhow::{Context as _, ensure};
use ed25519_dalek::SigningKey;
use registry_review::{App, Draft, PackageReview, Review};
use registry_server::applications::{self, ProofSource};
use registry_server::{ServerIdentity, Storage};
use registry_types::application::{
    ApplicationStatus, NamespaceClaim, PublisherApplication, proof_token,
};
use ui::prelude::TestApp;

fn named<'a>(
    node: &'a ui::headless::InspectionNode,
    id: &str,
) -> Option<&'a ui::headless::InspectionNode> {
    if node.test_id.as_deref() == Some(id) {
        return Some(node);
    }
    node.children.iter().find_map(|child| named(child, id))
}

struct Proofs(String);

#[test]
fn drafts_use_system_undo_redo_and_readable_history_without_replaying_decisions()
-> anyhow::Result<()> {
    let (_directory, storage, mut app, publisher) = setup()?;
    inspect(&mut app, &publisher)?;
    app.invoke(Draft::paths().feedback.replace("First feedback").into())?;
    let version = app.versions().pop().context("attributed draft history")?;
    ensure!(version.context.is_some());
    app.invoke(Draft::paths().feedback.replace("Second feedback").into())?;
    ensure!(app.undo());
    ensure!(app.state().feedback == "First feedback");
    ensure!(app.redo());
    ensure!(app.state().feedback == "Second feedback");
    let past = app
        .past_version(&[version.hash])
        .map_err(anyhow::Error::msg)?;
    let texts = past.texts();
    ensure!(
        texts.iter().any(|text| text == "First feedback"),
        "historical drafts render their actual fields: {texts:?}"
    );
    ensure!(
        !texts
            .iter()
            .any(|text| text.contains("Approve namespaces") || text.contains("ada@example.com")),
        "history has neither approval controls nor live queue metadata"
    );
    ensure!(
        storage
            .load_publisher_application(&publisher)?
            .context("pending request")?
            .status
            == ApplicationStatus::Pending
    );
    ensure!(storage.load_allowlist()?.publishers.is_empty());
    Ok(())
}

impl ProofSource for Proofs {
    fn txt_records(&self, name: &str) -> Result<Vec<String>, String> {
        if name == "_tilo-publisher.example.com" {
            Ok(vec![self.0.clone()])
        } else {
            Err("not a fixture domain".into())
        }
    }
    fn https_text(&self, _: &str) -> Result<String, String> {
        Err("no fixture HTTP".into())
    }
}

fn request(note: &str) -> PublisherApplication {
    let mut request = PublisherApplication {
        pubkey_hex: String::new(),
        claims: vec![NamespaceClaim::Domain {
            domain: "example.com".into(),
        }],
        contact: "ada@example.com".into(),
        note: note.into(),
        submitted_at_unix: tilo_clock::unix_now(),
        signature_hex: String::new(),
    };
    request.sign(&SigningKey::from_bytes(&[17; 32]));
    request
}

fn setup() -> anyhow::Result<(tempfile::TempDir, Storage, TestApp<App>, String)> {
    let directory = tempfile::tempdir()?;
    let storage = Storage::open(directory.path())?;
    storage.save_identity(&ServerIdentity::default())?;
    let request = request("A private note for the registry owner");
    let publisher = request.pubkey_hex.clone();
    applications::submit(
        &storage,
        request,
        tilo_clock::unix_now(),
        &Proofs(proof_token(&publisher)),
    )?;
    let path = directory
        .path()
        .to_str()
        .context("temporary path is UTF-8")?;
    let app = TestApp::<App>::launched(&["--private", "--registry-dir", path])?;
    Ok((directory, storage, app, publisher))
}

fn inspect(app: &mut TestApp<App>, publisher: &str) -> anyhow::Result<String> {
    let queue = app
        .effect(&Review::Refresh)?
        .value
        .context("refresh returns addressable publishers")?;
    ensure!(
        queue
            .get("applications")
            .and_then(ui::serde_json::Value::as_array)
            .is_some_and(|rows| rows.iter().any(|row| row
                .get("publisher")
                .and_then(ui::serde_json::Value::as_str)
                == Some(publisher)))
    );
    ensure!(
        !queue.to_string().contains("ada@example.com"),
        "discovery stays compact; inspect returns contact data"
    );
    let result = app.effect(&Review::Inspect {
        publisher: publisher.into(),
    })?;
    result
        .value
        .context("inspection returns its snapshot")?
        .get("review_id")
        .and_then(ui::serde_json::Value::as_str)
        .map(str::to_owned)
        .context("inspection has a review id")
}

#[test]
fn shared_launch_does_not_read_the_registry_or_publish_operator_effects() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let absent = directory.path().join("missing-registry");
    let path = absent.to_str().context("UTF-8 path")?;
    let mut app = TestApp::<App>::launched(&["--registry-dir", path])?;
    ensure!(
        app.harness()
            .semantic_owner_instance("review-locked")
            .is_some()
    );
    ensure!(app.effect(&Review::Refresh).is_err());
    ensure!(app.effect(&PackageReview::RefreshPackages).is_err());
    ensure!(
        !absent.exists(),
        "opening the view must not initialize a registry"
    );
    Ok(())
}

#[test]
fn private_review_rejects_with_exact_feedback_and_requires_confirmation() -> anyhow::Result<()> {
    let (_directory, storage, mut app, publisher) = setup()?;
    ensure!(
        app.harness()
            .semantic_owner_instance("review-detail")
            .is_none()
    );
    let review_id = inspect(&mut app, &publisher)?;
    ensure!(
        app.harness()
            .semantic_owner_instance("review-detail")
            .is_some()
    );
    ensure!(
        !format!("{:?}", app.state()).contains("ada@example.com"),
        "queue metadata stays out of the resource"
    );
    app.invoke(Draft::paths().reviewer.replace("Registry owner").into())?;
    let feedback = "Please provide screenshots; keep the \"notes\" app accessible.";
    app.invoke(Draft::paths().feedback.replace(feedback).into())?;
    app.effect(&Review::Inspect {
        publisher: publisher.clone(),
    })?;
    ensure!(
        app.state().feedback == feedback,
        "re-inspecting the same request preserves the draft"
    );
    let decision = Review::Reject {
        publisher: publisher.clone(),
        review_id,
        reviewer: "Registry owner".into(),
        reason: feedback.into(),
    };
    ensure!(
        app.effect(&decision).is_err(),
        "the owner must confirm the decision"
    );
    ensure!(
        storage
            .load_publisher_application(&publisher)?
            .context("pending request")?
            .status
            == ApplicationStatus::Pending
    );
    app.effect_confirmed(&decision)?;
    let stored = storage
        .load_publisher_application(&publisher)?
        .context("decided request")?;
    ensure!(stored.status == ApplicationStatus::Rejected);
    ensure!(stored.reason.as_deref() == Some(feedback));
    ensure!(stored.reviewer.as_deref() == Some("Registry owner"));
    ensure!(
        app.harness()
            .semantic_owner_instance("empty-queue")
            .is_some()
    );
    ensure!(storage.load_allowlist()?.publishers.is_empty());
    Ok(())
}

#[test]
fn a_replacement_cannot_be_decided_from_the_old_inspection() -> anyhow::Result<()> {
    let (_directory, storage, mut app, publisher) = setup()?;
    let review_id = inspect(&mut app, &publisher)?;
    let replacement = request("The replacement application must remain pending");
    applications::submit(
        &storage,
        replacement.clone(),
        tilo_clock::unix_now(),
        &Proofs(proof_token(&publisher)),
    )?;
    let result = app.effect_confirmed(&Review::Reject {
        publisher: publisher.clone(),
        review_id,
        reviewer: "Owner".into(),
        reason: "Old feedback".into(),
    });
    ensure!(result.is_err(), "a stale review must fail");
    let current = storage
        .load_publisher_application(&publisher)?
        .context("replacement")?;
    ensure!(current.application == replacement);
    ensure!(current.status == ApplicationStatus::Pending);
    ensure!(current.reason.is_none());
    ensure!(storage.load_allowlist()?.publishers.is_empty());
    inspect(&mut app, &publisher)?;
    Ok(())
}

#[test]
fn revoked_owner_access_hides_contact_data_and_refuses_saved_decisions() -> anyhow::Result<()> {
    let (_directory, storage, mut app, publisher) = setup()?;
    let review_id = inspect(&mut app, &publisher)?;
    let unchanged = app.state();
    app.set_role(ui::Role::Editor);
    ensure!(
        app.harness()
            .semantic_owner_instance("review-detail")
            .is_none()
    );
    ensure!(
        app.harness()
            .semantic_owner_instance("review-locked")
            .is_some()
    );
    ensure!(
        app.effect(&Review::Inspect {
            publisher: publisher.clone()
        })
        .is_err()
    );
    ensure!(
        app.effect_confirmed(&Review::Reject {
            publisher: publisher.clone(),
            review_id,
            reviewer: "Owner".into(),
            reason: "Denied".into()
        })
        .is_err()
    );
    ensure!(
        app.state() == unchanged,
        "revocation must not mutate the draft"
    );
    ensure!(
        storage
            .load_publisher_application(&publisher)?
            .context("pending request")?
            .status
            == ApplicationStatus::Pending
    );
    app.set_role(ui::Role::Owner);
    ensure!(
        app.harness()
            .semantic_owner_instance("review-detail")
            .is_some()
    );
    Ok(())
}

#[test]
fn review_controls_pass_native_audits_at_narrow_and_wide_sizes() -> anyhow::Result<()> {
    let (_directory, _storage, mut app, publisher) = setup()?;
    inspect(&mut app, &publisher)?;
    for preset in [
        ui::theme::Preset::Light,
        ui::theme::Preset::Dark,
        ui::theme::Preset::HighContrast,
    ] {
        app.harness().set_theme(ui::theme::Theme::preset(preset));
        let mut narrow_key_height = 0.0;
        for size in [(420, 900), (1280, 900)] {
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
            let inspection = app.harness().inspection();
            let key =
                named(&inspection.root, "publisher-key").context("publisher key is rendered")?;
            let [_, _, _, height] = key.rect;
            if size.0 == 420 {
                narrow_key_height = height;
            } else {
                ensure!(
                    narrow_key_height > height,
                    "long keys wrap on narrow panes without changing their content"
                );
            }
            ensure!(
                key.semantics.as_ref().and_then(|node| node.name.as_deref())
                    == Some(publisher.as_str()),
                "the complete key remains accessible without inserted line breaks"
            );
            let output = std::env::var_os("TILO_REGISTRY_REVIEW_ARTIFACTS").map_or_else(
                || "../../../target/registry-review-captures".into(),
                std::path::PathBuf::from,
            );
            let image = output.join(format!("{preset:?}-{}.png", size.0));
            app.harness().render_png(&image);
            ensure!(
                std::fs::metadata(image)?.len() > 0,
                "offscreen rendering produced no image"
            );
        }
    }
    Ok(())
}

#[test]
fn opening_a_private_window_never_initializes_or_reads_a_queue_automatically() -> anyhow::Result<()>
{
    let directory = tempfile::tempdir()?;
    let absent = directory.path().join("missing-registry");
    let path = absent.to_str().context("UTF-8 path")?;
    let mut app = TestApp::<App>::launched(&["--private", "--registry-dir", path])?;
    ensure!(
        !absent.exists(),
        "view construction creates no registry state"
    );
    let result = app.effect(&Review::Refresh);
    ensure!(result.is_err());
    ensure!(app.effect(&PackageReview::RefreshPackages).is_err());
    ensure!(
        !absent.exists(),
        "refresh refuses an uninitialized registry"
    );
    Ok(())
}

#[test]
fn confirmation_is_bound_to_the_inspected_application_and_explicit_feedback() -> anyhow::Result<()>
{
    let (_directory, storage, mut app, publisher) = setup()?;
    let review_id = inspect(&mut app, &publisher)?;
    let approve = Review::Approve {
        publisher: publisher.clone(),
        review_id: "wrong-id".into(),
        reviewer: "Owner".into(),
    };
    ensure!(
        app.effect(&approve).is_err(),
        "approval requires confirmation too"
    );
    ensure!(
        app.effect_confirmed(&approve).is_err(),
        "wrong snapshots are refused before checking network proofs"
    );
    ensure!(
        app.effect_confirmed(&Review::Reject {
            publisher: publisher.clone(),
            review_id,
            reviewer: "Owner".into(),
            reason: " ".into()
        })
        .is_err()
    );
    ensure!(storage.load_allowlist()?.publishers.is_empty());
    ensure!(
        storage
            .load_publisher_application(&publisher)?
            .context("pending")?
            .status
            == ApplicationStatus::Pending
    );
    Ok(())
}
