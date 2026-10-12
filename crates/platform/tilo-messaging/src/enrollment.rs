//! Unpublished first-account setup. This loan grants no account editing,
//! credential decision, message route or storage access to an application.
use crate::{LinkedAccount, Pairing, PairingFuture, SendError, ServiceAccount};
use std::{future::Future, pin::Pin, sync::Arc};
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum EnrollmentPhase {
    Empty,
    Linking,
    Review,
    AwaitingPermission,
    Ready,
    Interrupted,
}
/// Ephemeral self-identity review. Neither provider proof nor credentials are
/// serialized into the app's replicated resource or its action results.
#[derive(Clone, PartialEq, Eq)]
pub struct EnrollmentReview {
    pub account: ServiceAccount,
    pub generation: u64,
    pub scope: String,
    pub number: Option<String>,
}
impl std::fmt::Debug for EnrollmentReview {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("EnrollmentReview(<private>)")
    }
}
pub type EnrollmentReviewFuture =
    Pin<Box<dyn Future<Output = Result<EnrollmentReview, SendError>> + Send + 'static>>;
pub type EnrollmentApprovalFuture =
    Pin<Box<dyn Future<Output = Result<String, SendError>> + Send + 'static>>;
/// Private chooser projection. Selection is an opaque host token, never a bus
/// path, phone number, ICCID, account editor right or modem permission.
#[derive(Clone, PartialEq, Eq)]
pub struct SimChoice {
    pub selection: String,
    pub label: String,
}
impl std::fmt::Debug for SimChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SimChoice(<private>)")
    }
}
pub type SimChoicesFuture =
    Pin<Box<dyn Future<Output = Result<Vec<SimChoice>, SendError>> + Send + 'static>>;
pub trait EnrollmentHost: Send + Sync {
    /// Prepare private storage review before a service can create device keys.
    /// The original protected credential owner still decides its grant.
    fn prepare(&self, _service: String) -> EnrollmentReviewFuture {
        Box::pin(async { Err(SendError::Unavailable) })
    }
    fn begin(&self, service: String) -> PairingFuture;
    fn finish(&self, account: ServiceAccount, attempt: String) -> EnrollmentReviewFuture;
    /// Propose only. The original protected Passwords owner must separately
    /// decide permission before publication, activation or any message write.
    fn review(&self, account: ServiceAccount, display_name: String) -> EnrollmentApprovalFuture;
    fn phase(&self, account: &ServiceAccount) -> EnrollmentPhase;
    fn linked(&self, account: &ServiceAccount) -> Option<LinkedAccount>;
    fn cancel(&self, account: &ServiceAccount);
    fn supports_sim_selection(&self) -> bool {
        false
    }
    fn sim_choices(&self) -> SimChoicesFuture {
        Box::pin(async { Err(SendError::Unavailable) })
    }
    fn select_sim(&self, _selection: String) -> EnrollmentReviewFuture {
        Box::pin(async { Err(SendError::Unavailable) })
    }
    fn cancel_sim_selection(&self) {}
}
#[derive(Clone)]
pub struct EnrollmentClient {
    principal: String,
    host: Arc<dyn EnrollmentHost>,
}
impl EnrollmentClient {
    pub fn new(principal: String, host: Arc<dyn EnrollmentHost>) -> Self {
        Self { principal, host }
    }
    pub fn principal(&self) -> &str {
        &self.principal
    }
    pub fn supports_sim_selection(&self) -> bool {
        self.host.supports_sim_selection()
    }
    /// # Errors
    /// Refuses oversized, duplicate or malformed private chooser results.
    pub async fn sim_choices(&self) -> Result<Vec<SimChoice>, SendError> {
        let choices = self.host.sim_choices().await?;
        let mut seen = std::collections::BTreeSet::new();
        if choices.len() > 32
            || choices.iter().any(|choice| {
                choice.selection.is_empty()
                    || choice.selection.len() > 128
                    || choice
                        .selection
                        .chars()
                        .any(|c| c.is_control() || c.is_whitespace())
                    || choice.label.is_empty()
                    || choice.label.len() > 256
                    || choice.label.chars().any(char::is_control)
                    || !seen.insert(&choice.selection)
            })
        {
            self.host.cancel_sim_selection();
            return Err(SendError::Revoked);
        }
        Ok(choices)
    }
    /// # Errors
    /// Refuses an invalid choice or another service's unpublished identity.
    pub async fn select_sim(&self, selection: String) -> Result<EnrollmentReview, SendError> {
        if selection.is_empty()
            || selection.len() > 128
            || selection
                .chars()
                .any(|c| c.is_control() || c.is_whitespace())
        {
            return Err(SendError::InvalidRequest);
        }
        let review = self.host.select_sim(selection).await?;
        if review.account.service != "sms"
            || review.account.account.is_empty()
            || review.generation == 0
            || review.scope.is_empty()
            || review.scope.len() > 512
        {
            self.host.cancel(&review.account);
            return Err(SendError::Revoked);
        }
        Ok(review)
    }
    pub fn cancel_sim_selection(&self) {
        self.host.cancel_sim_selection();
    }
    /// # Errors
    /// Refuses unsupported storage review or foreign unpublished identity.
    pub async fn prepare(&self, service: String) -> Result<EnrollmentReview, SendError> {
        if !matches!(service.as_str(), "tilo" | "whatsapp") {
            return Err(SendError::InvalidRequest);
        }
        let review = self.host.prepare(service.clone()).await?;
        if review.account.service != service
            || review.account.account.is_empty()
            || review.generation == 0
            || review.scope.is_empty()
            || review.scope.len() > 512
        {
            self.host.cancel(&review.account);
            return Err(SendError::Revoked);
        }
        Ok(review)
    }
    /// # Errors
    /// Refuses unsupported service selections and malformed private results.
    pub async fn begin(&self, service: String) -> Result<Pairing, SendError> {
        if !matches!(service.as_str(), "signal" | "whatsapp" | "sms") {
            return Err(SendError::InvalidRequest);
        }
        let private = self.host.begin(service.clone()).await?;
        if private.account.service != service
            || private.account.account.is_empty()
            || private.generation == 0
            || private.attempt.is_empty()
            || private.attempt.len() > 512
            || private.qr_bytes().is_empty()
            || private.qr_bytes().len() > 4096
        {
            self.host.cancel(&private.account);
            return Err(SendError::Revoked);
        }
        Ok(private)
    }
    /// # Errors
    /// Refuses another unpublished account or malformed self-identity evidence.
    pub async fn finish(
        &self,
        account: ServiceAccount,
        attempt: String,
    ) -> Result<EnrollmentReview, SendError> {
        if attempt.is_empty() || attempt.len() > 512 || attempt.chars().any(char::is_control) {
            return Err(SendError::InvalidRequest);
        }
        let result = self.host.finish(account.clone(), attempt).await?;
        if result.account != account
            || result.generation == 0
            || result.scope.is_empty()
            || result.scope.len() > 512
        {
            self.host.cancel(&account);
            return Err(SendError::Revoked);
        }
        Ok(result)
    }
    /// # Errors
    /// Refuses invalid labels or a retired/changed original review.
    pub async fn review(
        &self,
        account: ServiceAccount,
        display_name: String,
    ) -> Result<String, SendError> {
        if display_name.trim().is_empty()
            || display_name.len() > 256
            || display_name.chars().any(char::is_control)
        {
            return Err(SendError::InvalidRequest);
        }
        self.host.review(account, display_name).await
    }
    pub fn phase(&self, account: &ServiceAccount) -> EnrollmentPhase {
        self.host.phase(account)
    }
    pub fn linked(&self, account: &ServiceAccount) -> Option<LinkedAccount> {
        self.host
            .linked(account)
            .filter(|linked| linked.account == *account && linked.generation > 0)
    }
    pub fn cancel(&self, account: &ServiceAccount) {
        self.host.cancel(account);
    }
}
