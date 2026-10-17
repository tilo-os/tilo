//! Renew a home adoption from completed byte/fidelity and relocation witnesses.
use super::*;
use crate::setup::home_transfer::promotion::PromotionReview;
pub fn recover_promotion(
    request: TransferRequest,
    active_home: &std::path::Path,
    adoption_journal: &std::path::Path,
) -> Result<(TransferReview, PromotionReview), String> {
    recover_promotion_with_runner(
        &flatpak::NativeRunner,
        request,
        active_home,
        adoption_journal,
    )
}
pub fn recover_promotion_with_runner(
    runner: &impl Runner,
    request: TransferRequest,
    active_home: &std::path::Path,
    adoption_journal: &std::path::Path,
) -> Result<(TransferReview, PromotionReview), String> {
    recover_promotion_inner(runner, request, active_home, adoption_journal, None)
}
pub fn recover_portable_promotion_with_runner(
    runner: &impl Runner,
    request: TransferRequest,
    active_home: &std::path::Path,
    adoption_journal: &std::path::Path,
    grant: &super::super::flatpak_transfer::portable::ApplicationStateGrant,
) -> Result<(TransferReview, PromotionReview), String> {
    if request.copy_home || !request.copy_flatpak {
        return Err("Portable application authority cannot authorize home adoption".into());
    }
    recover_promotion_inner(runner, request, active_home, adoption_journal, Some(grant))
}
fn recover_promotion_inner(
    runner: &impl Runner,
    request: TransferRequest,
    active_home: &std::path::Path,
    adoption_journal: &std::path::Path,
    grant: Option<&super::super::flatpak_transfer::portable::ApplicationStateGrant>,
) -> Result<(TransferReview, PromotionReview), String> {
    let protected = grant
        .map(|grant| grant.permits(&request.source_home, active_home))
        .transpose()?
        .unwrap_or(false);
    storage::validate_grants(&request)?;
    let applications_started = durable::validate_recovery(&request)?;
    for path in [active_home, adoption_journal] {
        if !path.is_absolute()
            || path.starts_with(&request.source_home)
            || (request.source_home.starts_with(path) && !(protected && path == active_home))
        {
            return Err("Adoption cannot overlap the original source home".into());
        }
    }
    let promotion = PromotionReview::recover(&request.target_home, active_home, adoption_journal)?
        .with_application_consent(request.copy_flatpak)?;
    let witness = promotion.witness()?;
    let copies = if request.copy_home {
        vec![HomeReview::recover(
            &request.source_home,
            &request.target_home,
            &request.private_journals.join("home-import"),
        )?]
    } else {
        namespace::recover_adoption(&request, &witness)?
    };
    let (remotes, preparation_failures) = if request.copy_flatpak {
        storage::prepare_remotes(runner, &request)?
    } else {
        (Vec::new(), Vec::new())
    };
    if applications_started {
        applications::validate_recovery(&request)?;
        if !preparation_failures.is_empty() {
            return Err("Original signed application repositories are unavailable".into());
        }
        let approved = storage::publish_remotes(&request.private_journals, &remotes)?;
        applications::verify_adopted_output(
            &request,
            witness.application_home()?,
            runner,
            &approved,
        )?;
    }
    let mut phase = Phase::default();
    for (index, copy) in copies.iter().enumerate() {
        let prefix = copy
            .destination()
            .strip_prefix(&request.target_home)
            .map_err(|e| e.to_string())?;
        let scoped = if request.copy_home {
            None
        } else {
            Some(witness.scoped(prefix)?)
        };
        let proof = match scoped.as_ref() {
            Some(scoped)
                if applications_started
                    && prefix == std::path::Path::new(".local/share/flatpak") =>
            {
                copy.verify_flatpak_adoption(scoped)?
            }
            Some(scoped) => copy.verify_adoption(scoped)?,
            None if applications_started => copy.verify_adoption_after_applications(&witness)?,
            None => copy.verify_adoption(&witness)?,
        };
        if !proof.complete() {
            return Err("Completed copy fidelity could not be independently verified".into());
        }
        phase.complete.insert(
            index,
            (
                proof,
                storage::target_identity(&witness.mapped_path(prefix).map_err(|e| e.to_string())?)?,
            ),
        );
        phase.attempted.insert(index);
    }
    phase.applications_complete = applications_started;
    let review = TransferReview {
        request,
        copies,
        remotes,
        preparation_failures,
        recovered: true,
        application_recovery: applications_started,
        verified_applications: applications_started,
        phase: Arc::new(Mutex::new(phase)),
        _source_guard: None,
    };
    Ok((review, promotion))
}
