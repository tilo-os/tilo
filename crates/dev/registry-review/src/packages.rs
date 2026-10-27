//! Native package review uses the CLI's bounded signed-byte inspection and
//! local moderation transition. No package is launched during review.

use std::path::{Path, PathBuf};

use registry_server::{
    InspectedVersion, Inspection, InspectionPolicy, ModerationService, PendingSubmission,
};
use ui::framework::EffectAnswer;
use ui::prelude::*;

use super::{Draft, PAGE_SIZE, Pane, open_existing, read_field, verbatim};

/// Package actions share the private owner-only operator surface.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.registry-review.packages")]
pub enum PackageReview {
    /// List addressable pending package summaries without downloading packages.
    #[effect(job, returns)]
    RefreshPackages,
    /// Verify the submitted packages and inspect their signed sandbox requests.
    #[effect(job, returns)]
    InspectPackage {
        #[param("Package id")]
        package: String,
    },
    /// Reverify the bytes and approve exactly the inspected request.
    #[effect(job, privileged)]
    ApprovePackage {
        #[param("Package id")]
        package: String,
        #[param("Review id from InspectPackage")]
        review_id: String,
        #[param("Reviewer name")]
        reviewer: String,
        #[param("Reason for the decision")]
        reason: String,
    },
    /// Reject exactly the inspected request, including one with invalid bytes.
    #[effect(job, privileged)]
    RejectPackage {
        #[param("Package id")]
        package: String,
        #[param("Review id from InspectPackage")]
        review_id: String,
        #[param("Reviewer name")]
        reviewer: String,
        #[param("Feedback")]
        reason: String,
    },
    /// List another bounded page of package summaries.
    #[effect(returns)]
    PackagePage {
        #[param("Zero-based page number")]
        page: usize,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Snapshot {
    pending: PendingSubmission,
    inspection: Result<Vec<InspectedVersion>, String>,
}

fn name(entry: &registry_types::RegistryEntry) -> &str {
    entry
        .name
        .resolve(&ui::i18n::I18nManager::current_locale())
        .unwrap_or(&entry.package_id)
}

fn queue_result(pane: &Pane) -> ui::serde_json::Value {
    let packages: Vec<_> = pane
        .packages
        .iter()
        .skip(pane.package_page.saturating_mul(PAGE_SIZE))
        .take(PAGE_SIZE)
        .map(|pending| {
            ui::serde_json::json!({
                "package": pending.submission_id,
                "name": name(&pending.change.entry),
                "action": pending.change.action,
                "received_at_unix": pending.submitted_at_unix,
                "versions": pending.change.entry.versions.len(),
            })
        })
        .collect();
    ui::serde_json::json!({"pending": pane.packages.len(), "page": pane.package_page, "packages": packages})
}

fn load(directory: &Path) -> Result<Vec<PendingSubmission>, String> {
    let mut pending = ModerationService::new(open_existing(directory)?)
        .pending_submissions()
        .map_err(|error| error.to_string())?;
    pending.sort_by(|a, b| {
        a.submitted_at_unix
            .cmp(&b.submitted_at_unix)
            .then(a.submission_id.cmp(&b.submission_id))
    });
    Ok(pending)
}

fn candidate(pane: &Pane, package: &str, review_id: &str) -> Result<Snapshot, String> {
    let snapshot = pane
        .package_selected
        .as_ref()
        .ok_or("Inspect a package first.")?;
    if snapshot.pending.submission_id != package
        || snapshot
            .pending
            .review_id()
            .map_err(|error| error.to_string())?
            != review_id
    {
        return Err("The inspected package changed; inspect it again before deciding.".into());
    }
    Ok(snapshot.clone())
}

pub(super) fn install(
    cx: &ViewCx<Draft>,
    directory: PathBuf,
    pane: &LocalState<Pane>,
    navigation: &LocalState<NavPane>,
) {
    let pane = pane.clone();
    let navigation = navigation.clone();
    cx.effects::<PackageReview>(move |effect, cx| {
        if pane.get().busy.is_some() {
            return Err("A registry operation is running; wait for it to finish.".into());
        }
        if let PackageReview::PackagePage { page } = effect {
            let mut state = pane.get();
            if page > state.packages.len().saturating_sub(1) / PAGE_SIZE {
                return Err("There is no package queue page with that number.".into());
            }
            state.package_page = page;
            let result = queue_result(&state);
            pane.set(state);
            return Ok(Effected::value(result));
        }
        start(cx, &directory, &pane, &navigation, effect)
    });
}

fn selection(pane: &Pane, effect: &PackageReview) -> Result<Option<Snapshot>, String> {
    match effect {
        PackageReview::RefreshPackages => Ok(None),
        PackageReview::InspectPackage { package } => {
            let pending = pane
                .packages
                .iter()
                .find(|pending| pending.submission_id == *package)
                .ok_or("RefreshPackages packages, then inspect a pending request.")?
                .clone();
            Ok(Some(Snapshot {
                pending,
                inspection: Err("Package inspection has not finished.".into()),
            }))
        }
        PackageReview::ApprovePackage {
            package,
            review_id,
            reviewer,
            reason,
        }
        | PackageReview::RejectPackage {
            package,
            review_id,
            reviewer,
            reason,
        } => {
            if reviewer.trim().is_empty() {
                return Err("Name the reviewer before deciding.".into());
            }
            if reason.trim().is_empty() {
                return Err("Write a reason before deciding on a package.".into());
            }
            let snapshot = candidate(pane, package, review_id)?;
            if matches!(effect, PackageReview::ApprovePackage { .. })
                && snapshot.inspection.is_err()
            {
                return Err("Package verification failed; fix the request and inspect it again before approval.".into());
            }
            Ok(Some(snapshot))
        }
        PackageReview::PackagePage { .. } => Err("This operation is not a package job.".into()),
    }
}

enum Outcome {
    Queue(Vec<PendingSubmission>),
    Inspected(Box<Snapshot>),
}

fn run(
    directory: &Path,
    effect: PackageReview,
    snapshot: Option<Snapshot>,
) -> Result<Outcome, String> {
    let storage = open_existing(directory)?;
    match (effect, snapshot) {
        (PackageReview::InspectPackage { .. }, Some(mut snapshot)) => {
            snapshot.inspection = registry_server::submission_review::inspect(
                &storage,
                &snapshot.pending,
                InspectionPolicy::default(),
            )
            .map_err(|error| error.to_string())
            .and_then(|versions| {
                versions
                    .into_iter()
                    .map(|(inspection, _bytes)| match inspection {
                        Inspection::Package(version) => Ok(version),
                        // This window renders package sandboxes only; a bot
                        // program's needs are shown by the operator CLI.
                        Inspection::Program(_) => Err(
                            "Bot programs are reviewed with `tilo-registry-server review show`."
                                .to_owned(),
                        ),
                    })
                    .collect()
            });
            Ok(Outcome::Inspected(Box::new(snapshot)))
        }
        (
            PackageReview::ApprovePackage {
                reviewer, reason, ..
            },
            Some(snapshot),
        ) => {
            registry_server::submission_review::approve(
                &storage,
                &snapshot.pending,
                &reviewer,
                &reason,
                tilo_clock::try_unix_now().ok_or("The system clock is before the Unix epoch.")?,
                InspectionPolicy::default(),
            )
            .map_err(|error| error.to_string())?;
            Ok(Outcome::Queue(load(directory)?))
        }
        (
            PackageReview::RejectPackage {
                reviewer, reason, ..
            },
            Some(snapshot),
        ) => {
            ModerationService::new(storage)
                .reject_reviewed(
                    &snapshot.pending,
                    reviewer,
                    reason,
                    tilo_clock::try_unix_now()
                        .ok_or("The system clock is before the Unix epoch.")?,
                )
                .map_err(|error| error.to_string())?;
            Ok(Outcome::Queue(load(directory)?))
        }
        (PackageReview::RefreshPackages, None) => Ok(Outcome::Queue(load(directory)?)),
        _ => Err("This operation is not a package job.".into()),
    }
}

fn start(
    cx: &mut EffectCx<Draft>,
    directory: &Path,
    pane: &LocalState<Pane>,
    navigation: &LocalState<NavPane>,
    effect: PackageReview,
) -> EffectAnswer {
    let selected = selection(&pane.get(), &effect)?;
    let mut state = pane.get();
    state.busy = Some(ui::time::now());
    state.error = None;
    pane.set(state);
    let directory = directory.to_owned();
    let pane = pane.clone();
    let navigation = navigation.clone();
    Ok(cx.thread(move |_| run(&directory, effect, selected), move |(), result, cx| {
        let mut state = pane.get(); state.busy = None;
        let result = match result {
            Ok(Outcome::Queue(packages)) => {
                state.packages = packages; state.package_selected = None; state.package_page = 0; state.packages_loaded = true;
                navigation.set(NavPane::Overview); queue_result(&state)
            }
            Ok(Outcome::Inspected(snapshot)) => {
                let snapshot = *snapshot;
                let review_id = snapshot.pending.review_id().map_err(|error| error.to_string())?;
                let result = match &snapshot.inspection {
                    Ok(versions) => ui::serde_json::json!({"review_id": review_id, "submission": snapshot.pending, "verified_packages": versions}),
                    Err(error) => ui::serde_json::json!({"review_id": review_id, "submission": snapshot.pending, "inspection_error": error}),
                };
                if state.package_selected.as_ref() != Some(&snapshot) { cx.submit(Draft::paths().feedback.replace("")); }
                state.package_selected = Some(snapshot); state.selected = None; navigation.set(NavPane::Content); result
            }
            Err(error) => { state.error = Some(error.clone()); pane.set(state); return Err(error); }
        };
        pane.set(state); Ok(Effected::value(result))
    }))
}

pub(super) fn queue(cx: &ViewCx<Draft>, pane: &Pane) -> Element {
    let actions = cx.actions();
    let mut body = Column::new()
        .with_intrinsic_height()
        .with_id("package-review-queue")
        .push(Box::new(
            Text::new("package-review-title").with_role(TextRole::Heading),
        ))
        .push(Box::new(Text::new("package-review-help")))
        .push(Box::new(actions.bind_effect_invocation(
            Button::new("refresh-packages", "package-review-refresh"),
            &PackageReview::RefreshPackages,
        )));
    if let Some(since) = pane.busy.filter(|_| !pane.packages_loaded) {
        body = body.push(super::on_its_way(since));
    } else if pane.packages_loaded && pane.packages.is_empty() {
        body = body.push(Box::new(Text::new("package-review-empty")));
    }
    for pending in pane
        .packages
        .iter()
        .skip(pane.package_page.saturating_mul(PAGE_SIZE))
        .take(PAGE_SIZE)
    {
        body = body
            .push(Box::new(verbatim(format!(
                "{} · {}",
                name(&pending.change.entry),
                pending.submission_id
            ))))
            .push(Box::new(actions.bind_effect_invocation(
                Button::new(
                    &format!("inspect-package-{}", pending.submission_id),
                    "package-review-inspect",
                ),
                &PackageReview::InspectPackage {
                    package: pending.submission_id.clone(),
                },
            )));
    }
    for (id, label, page) in [
        (
            "previous-packages",
            "review-previous",
            pane.package_page.checked_sub(1),
        ),
        (
            "next-packages",
            "review-next",
            (pane
                .package_page
                .saturating_add(1)
                .saturating_mul(PAGE_SIZE)
                < pane.packages.len())
            .then_some(pane.package_page.saturating_add(1)),
        ),
    ] {
        if let Some(page) = page {
            body = body.push(Box::new(actions.bind_effect_invocation(
                Button::new(id, label),
                &PackageReview::PackagePage { page },
            )));
        }
    }
    Box::new(body)
}

pub(super) fn detail(cx: &ViewCx<Draft>, snapshot: &Snapshot) -> Element {
    let pending = &snapshot.pending;
    let entry = &pending.change.entry;
    let mut body = Column::new()
        .with_intrinsic_height()
        .with_id("package-review-detail")
        .push(Box::new(verbatim(name(entry)).with_role(TextRole::Heading)))
        .push(read_field("package-review-id", &pending.submission_id))
        .push(read_field(
            "package-review-description",
            entry
                .description
                .resolve(&ui::i18n::I18nManager::current_locale())
                .unwrap_or(""),
        ))
        .push(Box::new(
            verbatim(entry.publisher_pubkey_hex.clone())
                .with_role(TextRole::Code)
                .with_wrap_mode(ui::text_render::TextWrapMode::WordOrGlyph)
                .with_id("package-publisher-key"),
        ));
    if pending.change.action == registry_types::EntryAction::Delete {
        body = body.push(Box::new(Text::new("package-review-deletion")));
    }
    match &snapshot.inspection {
        Err(error) => body = body.push(Box::new(Banner::error(error.clone()))),
        Ok(versions) => {
            for (version, declared) in versions.iter().zip(
                entry
                    .versions
                    .iter()
                    .filter_map(registry_types::RegistryVersion::target),
            ) {
                body = body
                    .push(Box::new(
                        Text::new("package-review-target")
                            .with_arg("platform", declared.platform.clone())
                            .with_arg("architecture", declared.architecture.clone())
                            .with_arg("isa", declared.isa.clone()),
                    ))
                    .push(Box::new(
                        Text::new("package-review-version")
                            .with_arg("version", version.version.clone())
                            .with_arg("bytes", version.size_bytes.to_string())
                            .with_arg("payload", version.payload_bytes.to_string()),
                    ))
                    .push(Box::new(
                        Text::new("package-review-sandbox").with_role(TextRole::Label),
                    ))
                    .push(Box::new(
                        verbatim(
                            ui::serde_json::to_string_pretty(&version.sandbox).unwrap_or_default(),
                        )
                        .with_wrap_mode(ui::text_render::TextWrapMode::WordOrGlyph)
                        .with_id(format!(
                            "sandbox-{}-{}-{}-{}",
                            version.version, declared.platform, declared.architecture, declared.isa
                        )),
                    ));
            }
        }
    }
    body = body
        .push(Box::new(labeled_field(
            "review-reviewer",
            cx.input(Draft::paths().reviewer).with_id("reviewer-input"),
        )))
        .push(Box::new(labeled_field(
            "package-review-reason",
            cx.input(Draft::paths().feedback).with_id("feedback-input"),
        )));
    let snapshot = snapshot.clone();
    let draft = cx.value(Clone::clone);
    body = body.push(cx.switch(&draft, move |cx, draft| {
        decision_controls(cx, &snapshot, draft)
    }));
    Box::new(body)
}

fn decision_controls(cx: &ViewCx<Draft>, snapshot: &Snapshot, draft: &Draft) -> Element {
    let Ok(review_id) = snapshot.pending.review_id() else {
        return Box::new(Text::new("review-snapshot-error"));
    };
    let mut controls = Column::new().with_intrinsic_height();
    let actions = cx.actions();
    if snapshot.inspection.is_ok() {
        controls = controls.push(Box::new(actions.bind_effect_invocation(
            Button::new("approve-package", "package-review-approve"),
            &PackageReview::ApprovePackage {
                package: snapshot.pending.submission_id.clone(),
                review_id: review_id.clone(),
                reviewer: draft.reviewer.clone(),
                reason: draft.feedback.clone(),
            },
        )));
    }
    controls = controls.push(Box::new(actions.bind_effect_invocation(
        Button::new("reject-package", "package-review-reject"),
        &PackageReview::RejectPackage {
            package: snapshot.pending.submission_id.clone(),
            review_id,
            reviewer: draft.reviewer.clone(),
            reason: draft.feedback.clone(),
        },
    )));
    Box::new(controls)
}
