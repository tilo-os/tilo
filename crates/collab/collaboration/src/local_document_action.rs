//! A local action observation as the collab client reports it: the
//! published invocation and the exact execution it captured. These are not
//! authorizations; the toolkit's `ui::local_document_action` queues them.

use tilo_actions::{ActionContext, EncodedAction};

/// The longest text a report field may carry.
pub const MAX_REPORT_FIELD: usize = 4096;

/// Original published invocation, before building or wrapping its typed action.
#[derive(Clone)]
pub struct PublishedInvocation {
    pub resource: String,
    pub action: String,
    pub arguments: serde_json::Map<String, serde_json::Value>,
}

/// Exact local execution observation; an empty mutation set means read/no-op.
/// `context` retains the parent record and source invocation independently of
/// the published child address. Neither field establishes caller authority.
#[derive(Clone)]
pub struct CapturedAction {
    /// Original broker-authenticated provenance, separate from app action context.
    pub custody: Option<runtime_collab::committed_document::ConnectionSnapshot>,
    pub invocation: uuid::Uuid,
    /// Random identity generated once, before content binding; retained on retry.
    pub capture_id: uuid::Uuid,
    pub revision: String,
    pub published: Option<PublishedInvocation>,
    pub encoded: EncodedAction,
    pub context: Option<ActionContext>,
    pub original_heads: Vec<[u8; 32]>,
    pub mutation_hashes: Vec<[u8; 32]>,
}

impl CapturedAction {
    /// Compute the content-bound identity after the exact mutations are known.
    /// # Errors
    /// Refuses captures without an original published invocation.
    pub fn bound_invocation(&self) -> Result<uuid::Uuid, String> {
        self.clone().into_report()?.bound_invocation()
    }

    /// Report this change against the published surface `revision`: a
    /// change made while the surface moved under its input, or one the
    /// person retries, is reported against the surface published since.
    /// The revision is part of the content-bound identity, so the
    /// invocation is bound again.
    /// # Errors
    /// Refuses an invalid revision or a capture without a published invocation.
    pub fn report_against(&mut self, revision: &str) -> Result<(), String> {
        validate_report_field(revision)?;
        revision.clone_into(&mut self.revision);
        self.invocation = self.bound_invocation()?;
        Ok(())
    }

    /// The report the collab client sends for this capture.
    ///
    /// # Errors
    /// Refuses a capture without a published invocation.
    pub fn into_report(self) -> Result<runtime_collab::LocalDocumentActionReport, String> {
        let published = self
            .published
            .ok_or("local document action has no published invocation")?;
        let argument = if published.arguments.is_empty() {
            None
        } else {
            Some(format!(
                "{}{}",
                tilo_actions::ARGUMENTS_JSON_PREFIX,
                serde_json::Value::Object(published.arguments)
            ))
        };
        Ok(runtime_collab::LocalDocumentActionReport {
            custody: self.custody,
            version: 1,
            invocation: self.invocation,
            capture_id: self.capture_id,
            revision: self.revision,
            resource: published.resource,
            action: published.action,
            argument,
            encoded: self.encoded,
            group: self
                .context
                .as_ref()
                .and_then(|context| context.group.clone()),
            context: self.context,
            original_heads: self.original_heads,
            mutation_hashes: self.mutation_hashes,
        })
    }
}

impl std::fmt::Debug for PublishedInvocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PublishedInvocation")
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for CapturedAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CapturedAction")
            .field("invocation", &self.invocation)
            .field("mutations", &self.mutation_hashes.len())
            .finish_non_exhaustive()
    }
}

/// Whether `field` can travel in a report: non-empty, at most
/// [`MAX_REPORT_FIELD`] bytes, without control characters.
///
/// # Errors
/// Why the field cannot travel.
pub fn validate_report_field(field: &str) -> Result<(), String> {
    if field.is_empty() || field.len() > MAX_REPORT_FIELD || field.chars().any(char::is_control) {
        return Err("invalid local document report field".into());
    }
    Ok(())
}
