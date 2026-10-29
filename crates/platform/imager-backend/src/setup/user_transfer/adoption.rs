//! Renew a home adoption from the sealed Home unit and relocation witnesses.
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
    if !request.copy_home {
        witness.validate_application_namespace()?;
    }
    let unit = SealedHome::reopen_adopted(
        &request.home_workspace(),
        &request.target_home,
        &witness,
        applications_started,
        &AtomicBool::new(false),
    )
    .map_err(|error| error.to_string())?;
    if applications_started {
        applications::validate_recovery(&request)?;
        applications::verify_adopted_output(&request, witness.application_home()?, runner)?;
    }
    let identity = storage::target_identity(
        &witness
            .mapped_path(std::path::Path::new(""))
            .map_err(|e| e.to_string())?,
    )?;
    let phase = Phase {
        attempted: true,
        applications_complete: applications_started,
        home: Some((unit.clone(), identity)),
        ..Phase::default()
    };
    let review = TransferReview {
        summary: TransferSummary::new(unit.summary(), &request),
        request,
        source: None,
        applications: None,
        recovered: true,
        application_recovery: applications_started,
        verified_applications: applications_started,
        phase: Arc::new(Mutex::new(phase)),
        _source_guard: None,
    };
    Ok((review, promotion))
}
