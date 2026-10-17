//! Journey presentation over frontend-owned native preparation workers.
//! Native preparation payloads have not yet been extracted into the backend.
use super::native_ownership::{FrontendNativeActor as Actor, FrontendNativeCustody};
use std::sync::Arc;

pub(crate) struct PreparedMedia {
    pub(super) native_payload: FrontendNativeCustody,
    pub(super) api: PreparedApi,
}
pub(super) struct PreparedApi {
    pub projection: fn(&PreparedMedia) -> serde_json::Value,
    pub expectation: fn(&PreparedMedia) -> &imager_backend::imager_model::PreparedSetupExpectation,
    pub account_custody_protocol: fn(&PreparedMedia) -> Option<u32>,
    pub service_session_protocol: fn(&PreparedMedia) -> Option<u32>,
    pub applications_protocol: fn(&PreparedMedia) -> Option<u32>,
    pub home_archive_protocol: fn(&PreparedMedia) -> Option<u32>,
    pub validate_for_write:
        fn(&PreparedMedia, &std::sync::atomic::AtomicBool) -> std::io::Result<()>,
    pub install_profile_draft:
        fn(&PreparedMedia) -> std::io::Result<Option<imager_backend::imager_model::InstallProfile>>,
}
impl PreparedMedia {
    pub(super) fn matches_reviewed_belongings(
        &self,
        identity: &imager_backend::prepared_identity::PreparedIdentity,
        profile: &imager_backend::prepared_setup::PublicInstallProfile,
        migration: Option<&imager_backend::setup::migration_intent::MigrationIntent>,
    ) -> bool {
        #[cfg(target_os = "linux")]
        {
            self.native_matches_reviewed_belongings(identity, profile, migration)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (identity, profile, migration);
            false
        }
    }

    pub(crate) fn bind_recovery(
        &self,
        evidence: Vec<Arc<crate::setup::migration_recovery::Evidence>>,
        plan: &imager_backend::staging::plan::Plan,
    ) -> std::io::Result<()> {
        #[cfg(target_os = "linux")]
        {
            self.native_bind_recovery(evidence, plan)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (evidence, plan);
            Err(imager_backend::journey_media::Unsupported.into())
        }
    }
    /// Amend the authenticated generation using the retained source authority.
    pub(in crate::setup) fn prepare_staging_copy(
        self: &Arc<Self>,
        intent: &imager_backend::setup::migration_intent::MigrationIntent,
        source: &imager_backend::setup::migration_handoff::Plan,
        reservation: u64,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> std::io::Result<Arc<Self>> {
        #[cfg(target_os = "linux")]
        {
            self.native_prepare_staging_copy(intent, source, reservation, cancel)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (intent, source, reservation, cancel);
            Err(imager_backend::journey_media::Unsupported.into())
        }
    }

    /// Bind selected applications through the native preparation authority.
    pub(in crate::setup) fn prepare_computer_applications(
        self: &Arc<Self>,
        intent: &imager_backend::setup::migration_intent::MigrationIntent,
        operation: &str,
        installation: &imager_backend::plan::Plan,
        reservation: u64,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> std::io::Result<Arc<Self>> {
        #[cfg(target_os = "linux")]
        {
            self.native_prepare_computer_applications(
                intent,
                operation,
                installation,
                reservation,
                cancel,
            )
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (intent, operation, installation, reservation, cancel);
            Err(imager_backend::journey_media::Unsupported.into())
        }
    }

    pub fn projection(&self) -> serde_json::Value {
        (self.api.projection)(self)
    }
    pub fn expectation(&self) -> &imager_backend::imager_model::PreparedSetupExpectation {
        (self.api.expectation)(self)
    }
    pub fn account_custody_protocol(&self) -> Option<u32> {
        (self.api.account_custody_protocol)(self)
    }
    pub fn service_session_protocol(&self) -> Option<u32> {
        (self.api.service_session_protocol)(self)
    }
    pub fn applications_protocol(&self) -> Option<u32> {
        (self.api.applications_protocol)(self)
    }
    pub fn home_archive_protocol(&self) -> Option<u32> {
        (self.api.home_archive_protocol)(self)
    }
    pub fn validate_for_write(
        &self,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> std::io::Result<()> {
        (self.api.validate_for_write)(self, cancel)
    }
    /// Convert authenticated public preferences without exposing native custody types.
    pub fn install_profile_draft(
        &self,
    ) -> std::io::Result<Option<imager_backend::imager_model::InstallProfile>> {
        (self.api.install_profile_draft)(self)
    }
}

/// Refresh under native custody before arming a write. Unsupported hosts must
/// refuse rather than treating the existing artifact as a refreshed generation.
pub(crate) fn refresh_for_write(
    prepared: &Arc<PreparedMedia>,
    cancel: &std::sync::atomic::AtomicBool,
) -> std::io::Result<Arc<PreparedMedia>> {
    #[cfg(target_os = "linux")]
    {
        prepared.refreshed_for_transfer(cancel)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (prepared, cancel);
        Err(imager_backend::journey_media::Unsupported.into())
    }
}

/// Translate native source-review failures without exposing native error types
/// to the wizard's portable write callback.
pub(crate) fn source_review_reason(error: &std::io::Error) -> Option<ReturnReason> {
    #[cfg(target_os = "linux")]
    {
        let key = crate::setup::portable_home_media::source_review_error(error)?;
        Some(if key == "installer-source-changed" {
            ReturnReason::ReviewedHomeChanged
        } else {
            ReturnReason::ReviewedHomeUnavailable
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = error;
        None
    }
}

pub(crate) use imager_backend::journey_media::ReturnReason;
#[derive(Clone)]
pub(crate) struct Navigation {
    pub artifact: ui::LocalState<Option<Arc<PreparedMedia>>>,
    pub showing_media: ui::LocalState<bool>,
    pub reason: ui::LocalState<Option<ReturnReason>>,
}
impl Navigation {
    pub fn review_home(&self, reason: ReturnReason) {
        self.reason.set(Some(reason));
        self.showing_media.set(false);
    }
}

pub(super) trait DeliveryState: std::any::Any {
    fn as_any(&self) -> &dyn std::any::Any;
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any;
    fn finish_close(&mut self) -> std::io::Result<()>;
    fn has_artifact(&self, artifact: &Arc<PreparedMedia>) -> bool;
    fn artifact(&self) -> Option<Arc<PreparedMedia>>;
    fn mark_source_changed(&mut self);
    fn mark_source_unavailable(&mut self);
    fn review_reason(&self) -> Option<ReturnReason>;
    fn reserved_data_bytes(&self) -> u64;
    fn expectation(&self) -> Option<&imager_backend::imager_model::PreparedSetupExpectation>;
    fn requires_data_partition(&self) -> bool;
    fn writing(&mut self);
    fn written(&mut self, target: imager_backend::imager_model::TargetDrive);
    fn awaiting_original_boot(&self) -> bool;
    fn original_boot_drive(&self) -> Option<String>;
    fn pending(&self) -> bool;
    fn busy(&self) -> bool;
    fn start(&mut self, root: std::path::PathBuf) -> Result<(), &'static str>;
    fn requires_offline_export(&self) -> bool;
    fn start_automatic(&mut self) -> Result<(), &'static str>;
    fn progress_label(&self) -> Option<String>;
    fn cancel(&self) -> Result<(), &'static str>;
    fn poll(&mut self) -> bool;
    fn needs_source_review(&self) -> bool;
    fn failed(&self) -> bool;
    fn projection(&self) -> serde_json::Value;
}
pub(crate) struct Delivery {
    pub(super) native: Actor<dyn DeliveryState>,
}
impl Default for Delivery {
    fn default() -> Self {
        #[cfg(not(target_os = "linux"))]
        let native = Actor::default();
        #[cfg(target_os = "linux")]
        let native =
            Actor::new(Box::new(super::prepared_media_native::Delivery::default())
                as Box<dyn DeliveryState>);
        Self { native }
    }
}
impl Delivery {
    /// Continue only with the held native media review, never a reconstructed receipt.
    pub(crate) fn start_awaiting_offline_export_automatic(
        &mut self,
        review: Arc<offline::OfflineMediaReview>,
    ) -> Result<(), &'static str> {
        #[cfg(target_os = "linux")]
        {
            self.native
                .get_mut()
                .map_err(|_| "installer-media-delivery-failed")?
                .as_any_mut()
                .downcast_mut::<super::prepared_media_native::Delivery>()
                .ok_or("installer-media-delivery-failed")?
                .start_awaiting_offline_export_automatic(review)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = review;
            Err("installer-media-delivery-failed")
        }
    }

    pub fn finish_close(&mut self) -> std::io::Result<()> {
        match self.native.get_mut() {
            Ok(native) => native.finish_close(),
            Err(_unsupported) => Ok(()),
        }
    }
    pub fn has_artifact(&self, artifact: &Arc<PreparedMedia>) -> bool {
        match self.native.get() {
            Ok(native) => native.has_artifact(artifact),
            Err(_unsupported) => false,
        }
    }
    pub fn artifact(&self) -> Option<Arc<PreparedMedia>> {
        match self.native.get() {
            Ok(native) => native.artifact(),
            Err(_unsupported) => None,
        }
    }
    pub fn mark_source_changed(&mut self) {
        match self.native.get_mut() {
            Ok(native) => native.mark_source_changed(),
            Err(_unsupported) => (),
        }
    }
    pub fn mark_source_unavailable(&mut self) {
        match self.native.get_mut() {
            Ok(native) => native.mark_source_unavailable(),
            Err(_unsupported) => (),
        }
    }
    pub fn review_reason(&self) -> Option<ReturnReason> {
        match self.native.get() {
            Ok(native) => native.review_reason(),
            Err(_unsupported) => None,
        }
    }
    pub fn reserved_data_bytes(&self) -> u64 {
        match self.native.get() {
            Ok(native) => native.reserved_data_bytes(),
            Err(_unsupported) => 0,
        }
    }
    pub fn expectation(&self) -> Option<&imager_backend::imager_model::PreparedSetupExpectation> {
        match self.native.get() {
            Ok(native) => native.expectation(),
            Err(_unsupported) => None,
        }
    }
    pub fn requires_data_partition(&self) -> bool {
        match self.native.get() {
            Ok(native) => native.requires_data_partition(),
            Err(_unsupported) => false,
        }
    }
    pub fn writing(&mut self) {
        match self.native.get_mut() {
            Ok(native) => native.writing(),
            Err(_unsupported) => (),
        }
    }
    pub fn written(&mut self, target: imager_backend::imager_model::TargetDrive) {
        match self.native.get_mut() {
            Ok(native) => native.written(target),
            Err(_unsupported) => (),
        }
    }
    pub fn awaiting_original_boot(&self) -> bool {
        match self.native.get() {
            Ok(native) => native.awaiting_original_boot(),
            Err(_unsupported) => false,
        }
    }
    pub fn original_boot_drive(&self) -> Option<String> {
        match self.native.get() {
            Ok(native) => native.original_boot_drive(),
            Err(_unsupported) => None,
        }
    }
    pub fn pending(&self) -> bool {
        match self.native.get() {
            Ok(native) => native.pending(),
            Err(_unsupported) => false,
        }
    }
    pub fn busy(&self) -> bool {
        match self.native.get() {
            Ok(native) => native.busy(),
            Err(_unsupported) => false,
        }
    }
    pub fn start(&mut self, root: std::path::PathBuf) -> Result<(), &'static str> {
        match self.native.get_mut() {
            Ok(native) => native.start(root),
            Err(_unsupported) => Err("installer-media-delivery-failed"),
        }
    }
    pub fn requires_offline_export(&self) -> bool {
        match self.native.get() {
            Ok(native) => native.requires_offline_export(),
            Err(_unsupported) => false,
        }
    }
    pub fn start_automatic(&mut self) -> Result<(), &'static str> {
        match self.native.get_mut() {
            Ok(native) => native.start_automatic(),
            Err(_unsupported) => Err("installer-media-delivery-failed"),
        }
    }
    pub fn progress_label(&self) -> Option<String> {
        match self.native.get() {
            Ok(native) => native.progress_label(),
            Err(_unsupported) => None,
        }
    }
    pub fn cancel(&self) -> Result<(), &'static str> {
        match self.native.get() {
            Ok(native) => native.cancel(),
            Err(_unsupported) => Err("installer-media-delivery-not-running"),
        }
    }
    pub fn poll(&mut self) -> bool {
        match self.native.get_mut() {
            Ok(native) => native.poll(),
            Err(_unsupported) => false,
        }
    }
    pub fn needs_source_review(&self) -> bool {
        match self.native.get() {
            Ok(native) => native.needs_source_review(),
            Err(_unsupported) => false,
        }
    }
    pub fn failed(&self) -> bool {
        match self.native.get() {
            Ok(native) => native.failed(),
            Err(_unsupported) => false,
        }
    }
    pub fn projection(&self) -> serde_json::Value {
        match self.native.get() {
            Ok(native) => native.projection(),
            Err(_unsupported) => {
                serde_json::json!({"state":"none","next_step":null,"receipt":null,"awaiting_setup_receipt":null,"complete":false,"home_receipt":null,"phase":null,"completed":null,"total":null,"cancelling":false})
            }
        }
    }
}

pub(super) trait ControllerState: std::any::Any {
    fn as_any(&self) -> &dyn std::any::Any;
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any;
    fn busy(&self) -> bool;
    fn cancel(&self) -> Result<(), &'static str>;
    fn phase_key(&self) -> Option<String>;
    fn progress_label(&self) -> Option<String>;
    fn take_progress_changed(&mut self) -> bool;
    fn poll(&mut self) -> Option<Result<Arc<PreparedMedia>, &'static str>>;
    fn require_source_review(&mut self, reason: ReturnReason);
    fn review_reason(&self) -> Option<ReturnReason>;
    fn active_home_projection(&self) -> Option<serde_json::Value>;
    fn projection(&self) -> serde_json::Value;
}
pub(crate) struct Controller {
    pub(super) native: Actor<dyn ControllerState>,
    review: Option<ReturnReason>,
}
impl Default for Controller {
    fn default() -> Self {
        #[cfg(not(target_os = "linux"))]
        let native = Actor::default();
        #[cfg(target_os = "linux")]
        let native = Actor::new(
            Box::new(super::prepared_media_native::Controller::default())
                as Box<dyn ControllerState>,
        );
        Self {
            native,
            review: None,
        }
    }
}
impl Controller {
    #[cfg(target_os = "linux")]
    fn native_controller(
        &mut self,
    ) -> Result<&mut super::prepared_media_native::Controller, &'static str> {
        self.native
            .get_mut()
            .map_err(|_| "installer-media-prepare-failed")?
            .as_any_mut()
            .downcast_mut()
            .ok_or("installer-media-prepare-failed")
    }

    pub(in crate::setup) fn start_with_selections(
        &mut self,
        identity: Arc<imager_backend::prepared_identity::PreparedIdentity>,
        accounts: Option<super::accounts::PreparedAccounts>,
        selections: MediaSelections,
        password: zeroize::Zeroizing<String>,
        profile: Option<imager_backend::prepared_setup::PublicInstallProfile>,
    ) -> Result<(), &'static str> {
        #[cfg(target_os = "linux")]
        {
            self.native_controller()?
                .start_with_selections(identity, accounts, selections, password, profile)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (identity, accounts, selections, password, profile);
            Err("installer-media-prepare-failed")
        }
    }

    pub(in crate::setup) fn review_computer_belongings(
        &mut self,
        prepared: Arc<PreparedMedia>,
        intent: imager_backend::setup::migration_intent::MigrationIntent,
        target_bytes: u64,
    ) -> Result<(), &'static str> {
        #[cfg(target_os = "linux")]
        {
            self.native_controller()?
                .review_computer_belongings(prepared, intent, target_bytes)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (prepared, intent, target_bytes);
            Err("installer-media-prepare-failed")
        }
    }

    pub(in crate::setup) fn start_belongings_export(
        &mut self,
        prepared: Arc<PreparedMedia>,
        destination: std::path::PathBuf,
    ) -> Result<(), &'static str> {
        #[cfg(target_os = "linux")]
        {
            self.native_controller()?
                .start_belongings_export(prepared, destination)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (prepared, destination);
            Err("installer-media-prepare-failed")
        }
    }
    pub fn busy(&self) -> bool {
        match self.native.get() {
            Ok(native) => native.busy(),
            Err(_unsupported) => false,
        }
    }
    pub fn cancel(&self) -> Result<(), &'static str> {
        match self.native.get() {
            Ok(native) => native.cancel(),
            Err(_unsupported) => Err("installer-media-not-preparing"),
        }
    }
    pub fn phase_key(&self) -> Option<String> {
        match self.native.get() {
            Ok(native) => native.phase_key(),
            Err(_unsupported) => None,
        }
    }
    pub fn progress_label(&self) -> Option<String> {
        match self.native.get() {
            Ok(native) => native.progress_label(),
            Err(_unsupported) => None,
        }
    }
    pub fn take_progress_changed(&mut self) -> bool {
        match self.native.get_mut() {
            Ok(native) => native.take_progress_changed(),
            Err(_unsupported) => false,
        }
    }
    pub fn poll(&mut self) -> Option<Result<Arc<PreparedMedia>, &'static str>> {
        match self.native.get_mut() {
            Ok(native) => native.poll(),
            Err(_unsupported) => None,
        }
    }
    pub fn require_source_review(&mut self, reason: ReturnReason) {
        self.review = Some(reason);
        match self.native.get_mut() {
            Ok(native) => native.require_source_review(reason),
            Err(_unsupported) => (),
        }
    }
    pub fn review_reason(&self) -> Option<ReturnReason> {
        match self.native.get() {
            Ok(native) => native.review_reason(),
            Err(_unsupported) => self.review,
        }
    }
    pub fn needs_source_review(&self) -> bool {
        self.review_reason().is_some()
    }
    pub fn active_home_projection(&self) -> Option<serde_json::Value> {
        match self.native.get() {
            Ok(native) => native.active_home_projection(),
            Err(_unsupported) => None,
        }
    }
    pub fn projection(&self) -> serde_json::Value {
        match self.native.get() {
            Ok(native) => native.projection(),
            Err(_unsupported) => {
                serde_json::json!({"state":self.review.map_or("idle", ReturnReason::state),"artifact":null,"exported":null,"phase":null,"completed":null,"total":null,"cancelling":false})
            }
        }
    }
}

impl Delivery {
    pub fn with_artifact(artifact: Arc<PreparedMedia>) -> Self {
        #[cfg(target_os = "linux")]
        {
            return Self {
                native: Actor::new(
                    Box::new(super::prepared_media_native::Delivery::with_artifact(
                        artifact,
                    )) as Box<dyn DeliveryState>,
                ),
            };
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = artifact;
            Self::default()
        }
    }
}
#[cfg(target_os = "linux")]
pub(in crate::setup) fn observe_read_medium(key: String) {
    super::prepared_media_native::delivery_retirement::observe_read_medium(key);
}
/// Retirement is native custody, not a fact reconstructed from a receipt.
pub(crate) fn pending_target(stable_id: &str) -> bool {
    #[cfg(target_os = "linux")]
    {
        super::prepared_media_native::pending_target(stable_id)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = stable_id;
        false
    }
}
pub(crate) fn finish_target(stable_id: &str) -> std::io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        super::prepared_media_native::finish_target(stable_id)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = stable_id;
        Err(imager_backend::journey_media::Unsupported.into())
    }
}

#[cfg(test)]
#[path = "presentation_tests.rs"]
mod tests;

#[cfg(target_os = "linux")]
pub(crate) use super::prepared_media_native::offline;
#[cfg(not(target_os = "linux"))]
pub(crate) mod offline {
    /// Native media admission is unavailable here. No constructor can mint one.
    pub(crate) struct OfflineMediaReview {
        _private: (),
    }
}
pub(in crate::setup) struct MediaSelections {
    pub home_applications: bool,
    pub home: Option<Arc<imager_backend::setup::portable_home_media::HomeSource>>,
    pub retained: Option<imager_backend::setup::migration_intent::MigrationIntent>,
    pub planned_direct: Option<imager_backend::staging::plan::Plan>,
    pub applications: Option<PortableAppsSelection>,
    pub browser: Option<super::browser::SelectedSources>,
    pub sessions: Option<super::service_sessions::SelectedSources>,
}
pub(in crate::setup) struct PortableAppsSelection {
    pub source: Arc<imager_backend::setup::flatpak_transfer::portable::PortableFlatpakSource>,
    pub include_user_state: bool,
}
#[cfg(all(test, target_os = "linux"))]
pub(crate) use super::prepared_media_native::{
    home_media_fixture, native_retained, protected_account_media_fixture,
    selected_applications_media_fixture,
};
