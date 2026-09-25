//! Protocol v2 scoped grants (parity contract A0, completed by lane D).
//!
//! A [`GrantScope`] authorizes a *class* of operations (app + record(s) +
//! verb + capability classes, optionally narrowed by resource and action
//! lists) instead of one frozen `(app, record, resource, action)` tuple.
//!
//! - A v1 [`GrantAction`] tuple is a degenerate scope ([`scopes_from_v1`]):
//!   exact record, exact resource and an exact action allow-list. It is never
//!   widened: its class list is informational and coverage stays exact.
//! - A v2 scope travels inside the existing v1 container as one *carrier*
//!   tuple ([`scope_carrier`]). The carrier's resource holds the canonical
//!   scope JSON after [`SCOPE_CARRIER_RESOURCE_PREFIX`], so the retained
//!   ledgers freeze the literal scope, and a v1 binary that only knows exact
//!   matching treats the carrier as a tuple no surface publishes: it fails
//!   closed instead of widening (security review T20).
//!
//! Scope coverage is necessary, never sufficient. The host must still run
//! [`crate::authorize_operation`] (workspace policy for the agent *and* the
//! delegating owner, confirmation, delegation manifest, task state).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::{GrantAction, MAX_ACTIONS_PER_GRANT, ValidationError, validate_grant_resource};

/// The protocol version that introduces scoped grants.
pub const GRANT_SCOPE_PROTOCOL_VERSION: u8 = 2;
/// Upper bound on scopes in one v2-authored grant.
pub const MAX_GRANT_SCOPES: usize = 16;
/// Upper bound on an explicit allow or deny action list inside one scope.
pub const MAX_SCOPE_ACTIONS: usize = 64;
/// Upper bound on the records one scope may name (security review MUST-16).
pub const MAX_SCOPE_RECORDS: usize = 32;
/// Canonical grant spelling of an application's package-primary record,
/// whose live route record id is `""`.
pub const PRIMARY_RECORD_ALIAS: &str = "@qos:package-primary";
/// Resource prefix of a v2 scope carried in a v1 [`GrantAction`].
pub const SCOPE_CARRIER_RESOURCE_PREFIX: &str = "@qos:grant-scope/v2:";
/// Action of a v2 scope carrier. No application may publish it.
pub const SCOPE_CARRIER_ACTION: &str = "@qos:grant-scope";

/// Delegation capability classes (security review MUST-4). They replace the
/// bare `OperationKind` for delegation: an app's `Document` default never
/// counts, only an explicit class from a trusted delegation manifest.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum OperationClass {
    /// Edits the record's CRDT document through one registered envelope.
    #[serde(rename = "document")]
    Document,
    /// Changes only the invoker's own view (caret, selection, find).
    #[serde(rename = "view")]
    View,
    /// Read-only: returns data, commits nothing.
    #[serde(rename = "query")]
    Query,
    /// Makes content visible to other people (Chat post).
    #[serde(rename = "disclose")]
    Disclose,
    #[serde(rename = "effect.fs.read")]
    EffectFsRead,
    #[serde(rename = "effect.fs.write")]
    EffectFsWrite,
    #[serde(rename = "effect.net")]
    EffectNet,
    #[serde(rename = "effect.launch")]
    EffectLaunch,
    #[serde(rename = "effect.media")]
    EffectMedia,
    /// Opens an application in a new window with a new record, exactly as a
    /// person opening it from the launcher (`desktop:app:<id>#open`). Not a
    /// host effect: it creates one checked canonical record and its desktop
    /// tab through the workspace owner. A record the task opens this way is
    /// covered for the rest of that task ([`crate::scopes_with_opened_records`]).
    #[serde(rename = "app.open")]
    AppOpen,
}
/// The A0 name of [`OperationClass`].
pub type GrantKind = OperationClass;

impl OperationClass {
    pub const ALL: [Self; 10] = [
        Self::Document,
        Self::View,
        Self::Query,
        Self::Disclose,
        Self::EffectFsRead,
        Self::EffectFsWrite,
        Self::EffectNet,
        Self::EffectLaunch,
        Self::EffectMedia,
        Self::AppOpen,
    ];
    pub fn is_effect(self) -> bool {
        matches!(
            self,
            Self::EffectFsRead
                | Self::EffectFsWrite
                | Self::EffectNet
                | Self::EffectLaunch
                | Self::EffectMedia
        )
    }
    /// The Cloud host filesystem is not the user's: `effect.fs.*` is never
    /// delegable to an agent driving a Cloud host worker (MUST-4).
    pub fn delegable_on_host_worker(self) -> bool {
        !matches!(self, Self::EffectFsRead | Self::EffectFsWrite)
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Document => "document",
            Self::View => "view",
            Self::Query => "query",
            Self::Disclose => "disclose",
            Self::EffectFsRead => "effect.fs.read",
            Self::EffectFsWrite => "effect.fs.write",
            Self::EffectNet => "effect.net",
            Self::EffectLaunch => "effect.launch",
            Self::EffectMedia => "effect.media",
            Self::AppOpen => "app.open",
        }
    }
}

/// Which record(s) a scope covers. Record ids are canonical grant spellings
/// ([`canonical_record`]); the host resolves the live route to that spelling.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub enum RecordScope {
    /// Exactly this record id. The default for "agent edits this document".
    Exact(String),
    /// 2..=[`MAX_SCOPE_RECORDS`] explicit records, sorted and unique.
    AnyOf(Vec<String>),
    /// Any record of `app_id` in the issuing workspace. Covers nothing unless
    /// the scope also carries `explicitWorkspaceConsent` (MUST-3).
    InWorkspace,
}

/// The workspace permission a scope carries. Ordered: `Write` implies
/// `Comment` implies `Read`. Invite and Admin are never delegable.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum GrantVerb {
    Read,
    Comment,
    Write,
}

/// Which resources inside the app a scope covers. This is a narrowing filter
/// only: the record always comes from the live route, never from a resource
/// name (security review T6), so a resource filter can only deny.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub enum ResourceScope {
    /// Every resource of the app.
    #[default]
    Any,
    /// Resource ids starting with this prefix, e.g. `"word-processor."`.
    Prefix(String),
    /// Exactly this resource id (the v1 degenerate form).
    Exact(String),
}

/// The frozen package and delegation-schema identity (MUST-6). A changed
/// delegation schema or package version refuses the scope until re-consent.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PackagePin {
    pub package_id: String,
    pub version: String,
    /// Lowercase hex digest (64 chars) of the app's delegation schema.
    pub schema_digest: String,
}

/// Host-enforced per-task budgets (MUST-8, MUST-9). Defaults follow the
/// security review's recommended scope (§4).
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScopeBudgets {
    pub max_steps: u32,
    pub wall_clock_secs: u32,
    pub removed_bytes_per_step: u32,
    pub removed_bytes_per_task: u32,
    /// Also bounds removal per task to this percentage of the document size
    /// at the task's first step, whichever is smaller.
    pub removed_percent_per_task: u8,
    pub max_delete_count: u32,
    pub invocations_per_sec: u16,
    pub invocation_burst: u16,
}
impl Default for ScopeBudgets {
    fn default() -> Self {
        Self {
            max_steps: 200,
            wall_clock_secs: 15 * 60,
            removed_bytes_per_step: 2 * 1024,
            removed_bytes_per_task: 8 * 1024,
            removed_percent_per_task: 10,
            max_delete_count: 256,
            invocations_per_sec: 5,
            invocation_burst: 20,
        }
    }
}
impl ScopeBudgets {
    /// The pointwise minimum: several covering scopes never add budget.
    #[must_use]
    pub fn tightest(self, other: Self) -> Self {
        Self {
            max_steps: self.max_steps.min(other.max_steps),
            wall_clock_secs: self.wall_clock_secs.min(other.wall_clock_secs),
            removed_bytes_per_step: self
                .removed_bytes_per_step
                .min(other.removed_bytes_per_step),
            removed_bytes_per_task: self
                .removed_bytes_per_task
                .min(other.removed_bytes_per_task),
            removed_percent_per_task: self
                .removed_percent_per_task
                .min(other.removed_percent_per_task),
            max_delete_count: self.max_delete_count.min(other.max_delete_count),
            invocations_per_sec: self.invocations_per_sec.min(other.invocations_per_sec),
            invocation_burst: self.invocation_burst.min(other.invocation_burst),
        }
    }
    fn validate(&self) -> Result<(), ValidationError> {
        let bad = self.max_steps == 0
            || self.max_steps > 10_000
            || self.wall_clock_secs == 0
            || self.wall_clock_secs > 3_600
            || self.removed_bytes_per_step > 64 * 1024
            || self.removed_bytes_per_task > 1024 * 1024
            || self.removed_percent_per_task > 100
            || self.max_delete_count > 4_096
            || self.invocations_per_sec == 0
            || self.invocations_per_sec > 50
            || self.invocation_burst < self.invocations_per_sec
            || self.invocation_burst > 200;
        if bad {
            return Err(ValidationError::InvalidField("grantScope.budgets"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GrantScope {
    pub app_id: String,
    pub record: RecordScope,
    pub verb: GrantVerb,
    /// Non-empty, unique. Typical agent edit scope: `[document, view, query]`.
    #[serde(alias = "kinds")]
    pub classes: Vec<OperationClass>,
    #[serde(default)]
    pub resource: ResourceScope,
    /// Optional allow-list of action ids. `None` = any action. The v1
    /// degenerate form sets it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actions: Option<Vec<String>>,
    /// Action ids never covered, even when everything else matches.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deny_actions: Vec<String>,
    /// Required for a v2-authored scope, absent for the v1 degenerate form.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<PackagePin>,
    /// `None` = [`ScopeBudgets::default`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budgets: Option<ScopeBudgets>,
    /// The separate, explicit consent a [`RecordScope::InWorkspace`] needs.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub explicit_workspace_consent: bool,
}

/// A v2 scoped grant set, version-gated on the wire.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GrantScopeSet {
    /// Must equal [`GRANT_SCOPE_PROTOCOL_VERSION`].
    pub version: u8,
    pub scopes: Vec<GrantScope>,
}

/// One concrete operation a host checks against a scope. `record_id` is the
/// canonical spelling of the broker's live, registered route record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScopeTarget<'a> {
    pub app_id: &'a str,
    pub record_id: &'a str,
    pub resource: &'a str,
    pub action: &'a str,
    /// From the trusted delegation manifest. `None` (unclassified) is never
    /// covered by a v2 scope.
    pub class: Option<OperationClass>,
    /// The verb the operation's contract requires (from its permission).
    pub required: GrantVerb,
}

impl GrantScope {
    /// The v1 degenerate form: exact record, exact resource, exact action
    /// allow-list, no package pin. Its coverage is exact and class-blind.
    pub fn is_degenerate(&self) -> bool {
        matches!(self.record, RecordScope::Exact(_))
            && matches!(self.resource, ResourceScope::Exact(_))
            && self.actions.is_some()
            && self.package.is_none()
    }
    pub fn budgets(&self) -> ScopeBudgets {
        self.budgets.unwrap_or_default()
    }
    /// Canonical record ids named by this scope (empty for `InWorkspace`).
    pub fn records(&self) -> &[String] {
        match &self.record {
            RecordScope::Exact(id) => std::slice::from_ref(id),
            RecordScope::AnyOf(ids) => ids,
            RecordScope::InWorkspace => &[],
        }
    }
    /// Whether the record part alone matches.
    pub fn covers_record(&self, record_id: &str) -> bool {
        match &self.record {
            RecordScope::Exact(id) => id == record_id,
            RecordScope::AnyOf(ids) => ids.iter().any(|id| id == record_id),
            RecordScope::InWorkspace => self.explicit_workspace_consent,
        }
    }
    /// Pure coverage test. `true` means only "inside this scope"; the caller
    /// must still apply workspace policy, confirmation and task-state checks.
    pub fn covers(&self, target: &ScopeTarget<'_>) -> bool {
        let required = target.required;
        if self.app_id != target.app_id
            || required > self.verb
            || self.deny_actions.iter().any(|a| a == target.action)
            || !self.covers_record(target.record_id)
        {
            return false;
        }
        let resource = match &self.resource {
            ResourceScope::Any => true,
            ResourceScope::Prefix(prefix) => target.resource.starts_with(prefix.as_str()),
            ResourceScope::Exact(id) => id == target.resource,
        };
        let action = self
            .actions
            .as_ref()
            .is_none_or(|allowed| allowed.iter().any(|a| a == target.action));
        // A v1 tuple is exact already; a v2 scope needs an explicit class.
        let class = self.is_degenerate()
            || target
                .class
                .is_some_and(|class| self.classes.contains(&class));
        resource && action && class
    }
}

/// Map a live route record id to its one canonical grant spelling (T7):
/// `""` is the package-primary record; reserved spellings (`@qos:*`, `*…`)
/// are never literal record ids; everything else must be canonical ASCII.
///
/// # Errors
///
/// Returns the refusal or validation error that fails the check.
pub fn canonical_record(route_record: &str) -> Result<String, ValidationError> {
    if route_record.is_empty() {
        return Ok(PRIMARY_RECORD_ALIAS.into());
    }
    if is_reserved_record_spelling(route_record) {
        return Err(ValidationError::InvalidField("recordId.reserved"));
    }
    validate_scope_id(route_record, "recordId")?;
    Ok(route_record.into())
}
/// Inverse of [`canonical_record`]: the live route record of a canonical id.
///
/// # Errors
///
/// Returns the refusal or validation error that fails the check.
pub fn route_record(canonical: &str) -> Result<&str, ValidationError> {
    if canonical == PRIMARY_RECORD_ALIAS {
        return Ok("");
    }
    if is_reserved_record_spelling(canonical) {
        return Err(ValidationError::InvalidField("recordId.reserved"));
    }
    validate_scope_id(canonical, "recordId")?;
    Ok(canonical)
}
/// Spellings no document may use as a literal record id.
pub fn is_reserved_record_spelling(value: &str) -> bool {
    value.starts_with("@qos:") || value.starts_with('*')
}
/// A canonical grant record id: the primary alias or a non-reserved id.
fn validate_canonical_record(value: &str, field: &'static str) -> Result<(), ValidationError> {
    if value == PRIMARY_RECORD_ALIAS {
        return Ok(());
    }
    if is_reserved_record_spelling(value) {
        return Err(ValidationError::InvalidField(field));
    }
    validate_scope_id(value, field)
}

/// Convert v1 exact tuples into degenerate scopes: one scope per
/// `(app, record, resource)` with an exact resource and an action allow-list,
/// verb `Write` and classes `[document]` (informational). Order follows first
/// appearance. Carriers are not v1 tuples; use [`scopes_from_grants`].
///
/// A converted v1 set is bounded by `MAX_ACTIONS_PER_GRANT`, not by
/// [`MAX_GRANT_SCOPES`]; validate each scope with [`validate_grant_scope`].
pub fn scopes_from_v1(actions: &[GrantAction]) -> Vec<GrantScope> {
    let mut index: BTreeMap<(&str, &str, &str), usize> = BTreeMap::new();
    let mut scopes: Vec<GrantScope> = Vec::new();
    for action in actions {
        let key = (
            action.app_id.as_str(),
            action.record_id.as_str(),
            action.resource.as_str(),
        );
        let slot = *index.entry(key).or_insert_with(|| {
            scopes.push(GrantScope {
                app_id: action.app_id.clone(),
                record: RecordScope::Exact(action.record_id.clone()),
                verb: GrantVerb::Write,
                classes: vec![OperationClass::Document],
                resource: ResourceScope::Exact(action.resource.clone()),
                actions: Some(Vec::new()),
                deny_actions: Vec::new(),
                package: None,
                budgets: None,
                explicit_workspace_consent: false,
            });
            scopes.len().saturating_sub(1)
        });
        if let Some(allowed) = scopes.get_mut(slot).and_then(|s| s.actions.as_mut())
            && !allowed.contains(&action.action)
        {
            allowed.push(action.action.clone());
        }
    }
    scopes
}

/// Carry one validated v2 scope in a v1 [`GrantAction`]. The carrier binds
/// the app and the first record in its own fields, so every existing
/// per-grant record check still sees a real, current record.
///
/// # Errors
///
/// Returns the refusal or validation error that fails the check.
pub fn scope_carrier(scope: &GrantScope) -> Result<GrantAction, ValidationError> {
    validate_authored_scope(scope)?;
    let record = scope
        .records()
        .first()
        .ok_or(ValidationError::InvalidField("grantScope.record"))?
        .clone();
    let json = serde_json::to_string(scope)
        .map_err(|error| ValidationError::InvalidJson(error.to_string()))?;
    let resource = format!("{SCOPE_CARRIER_RESOURCE_PREFIX}{json}");
    validate_grant_resource(&resource)
        .map_err(|_| ValidationError::InvalidField("grantScope.carrier"))?;
    Ok(GrantAction {
        app_id: scope.app_id.clone(),
        record_id: record,
        resource,
        action: SCOPE_CARRIER_ACTION.into(),
    })
}
/// Whether this tuple is a v2 scope carrier (well-formed or not).
pub fn is_scope_carrier(grant: &GrantAction) -> bool {
    grant.action == SCOPE_CARRIER_ACTION
        || grant.resource.starts_with(SCOPE_CARRIER_RESOURCE_PREFIX)
}
/// Decode a carrier back to its exact scope. Refuses a carrier whose outer
/// app/record disagree with the scope, or whose JSON is not the canonical
/// re-encoding (so one scope has exactly one carrier spelling).
///
/// # Errors
///
/// Returns the refusal or validation error that fails the check.
pub fn decode_scope_carrier(grant: &GrantAction) -> Result<GrantScope, ValidationError> {
    let json = grant
        .resource
        .strip_prefix(SCOPE_CARRIER_RESOURCE_PREFIX)
        .filter(|_| grant.action == SCOPE_CARRIER_ACTION)
        .ok_or(ValidationError::InvalidField("grantScope.carrier"))?;
    let scope: GrantScope = serde_json::from_str(json)
        .map_err(|error| ValidationError::InvalidJson(error.to_string()))?;
    if scope_carrier(&scope)? != *grant {
        return Err(ValidationError::InvalidField("grantScope.carrier"));
    }
    Ok(scope)
}

/// The one scope view of a retained grant list: v2 carriers decode to their
/// scopes, v1 tuples become degenerate scopes. A list mixes neither, and is
/// bounded by [`MAX_GRANT_SCOPES`] carriers or `MAX_ACTIONS_PER_GRANT` tuples.
///
/// # Errors
///
/// Returns the refusal or validation error that fails the check.
pub fn scopes_from_grants(grants: &[GrantAction]) -> Result<Vec<GrantScope>, ValidationError> {
    validate_grant_set(grants)?;
    if grants.first().is_some_and(is_scope_carrier) {
        grants.iter().map(decode_scope_carrier).collect()
    } else {
        Ok(scopes_from_v1(grants))
    }
}
/// Shape and bound check of a retained grant list (v1 tuples or v2 carriers).
/// This replaces the fixed exact-tuple cap as the only count bound for
/// scoped grants: at most [`MAX_GRANT_SCOPES`] carriers.
///
/// # Errors
///
/// Returns the refusal or validation error that fails the check.
pub fn validate_grant_set(grants: &[GrantAction]) -> Result<(), ValidationError> {
    let carriers = grants.iter().filter(|g| is_scope_carrier(g)).count();
    if grants.is_empty() || (carriers != 0 && carriers != grants.len()) {
        return Err(ValidationError::InvalidField("grants"));
    }
    if carriers == 0 {
        if grants.len() > MAX_ACTIONS_PER_GRANT {
            return Err(ValidationError::InvalidField("grants"));
        }
        let mut keys = std::collections::HashSet::new();
        for g in grants {
            validate_scope_id(&g.app_id, "grantAction.appId")?;
            validate_canonical_record(&g.record_id, "grantAction.recordId")?;
            validate_grant_resource(&g.resource)?;
            validate_scope_id(&g.action, "grantAction.action")?;
            if !keys.insert((&g.app_id, &g.record_id, &g.resource, &g.action)) {
                return Err(ValidationError::InvalidField("grants"));
            }
        }
        return Ok(());
    }
    if carriers > MAX_GRANT_SCOPES {
        return Err(ValidationError::InvalidField("grants"));
    }
    let mut seen: Vec<GrantScope> = Vec::with_capacity(carriers);
    for g in grants {
        let scope = decode_scope_carrier(g)?;
        if seen.contains(&scope) {
            return Err(ValidationError::InvalidField("grants"));
        }
        seen.push(scope);
    }
    Ok(())
}

fn validate_scope_id(value: &str, field: &'static str) -> Result<(), ValidationError> {
    if value.is_empty()
        || value.len() > crate::MAX_OPAQUE_ID_BYTES
        || !value.is_ascii()
        || value.trim() != value
        || value.bytes().any(|b| b.is_ascii_control())
    {
        return Err(ValidationError::InvalidField(field));
    }
    Ok(())
}

fn validate_action_list(values: &[String], field: &'static str) -> Result<(), ValidationError> {
    if values.len() > MAX_SCOPE_ACTIONS {
        return Err(ValidationError::InvalidField(field));
    }
    let mut seen = std::collections::HashSet::new();
    for value in values {
        validate_scope_id(value, field)?;
        if value == SCOPE_CARRIER_ACTION || !seen.insert(value.as_str()) {
            return Err(ValidationError::InvalidField(field));
        }
    }
    Ok(())
}

/// Validate one scope's bounds and shape (v1 degenerate or v2).
///
/// # Errors
///
/// Returns the refusal or validation error that fails the check.
pub fn validate_grant_scope(scope: &GrantScope) -> Result<(), ValidationError> {
    validate_scope_id(&scope.app_id, "grantScope.appId")?;
    match &scope.record {
        RecordScope::Exact(id) => validate_canonical_record(id, "grantScope.record")?,
        RecordScope::AnyOf(ids) => {
            if ids.len() < 2 || ids.len() > MAX_SCOPE_RECORDS {
                return Err(ValidationError::InvalidField("grantScope.record"));
            }
            for id in ids {
                validate_canonical_record(id, "grantScope.record")?;
            }
            if ids.windows(2).any(|pair| pair.first() >= pair.get(1)) {
                return Err(ValidationError::InvalidField("grantScope.record"));
            }
        }
        RecordScope::InWorkspace => {}
    }
    if scope.explicit_workspace_consent && scope.record != RecordScope::InWorkspace {
        return Err(ValidationError::InvalidField(
            "grantScope.explicitWorkspaceConsent",
        ));
    }
    if scope.classes.is_empty() || scope.classes.len() > OperationClass::ALL.len() {
        return Err(ValidationError::InvalidField("grantScope.classes"));
    }
    let unique: std::collections::BTreeSet<_> = scope.classes.iter().collect();
    if unique.len() != scope.classes.len() {
        return Err(ValidationError::InvalidField("grantScope.classes"));
    }
    match &scope.resource {
        ResourceScope::Any => {}
        ResourceScope::Prefix(value) | ResourceScope::Exact(value) => {
            validate_grant_resource(value)
                .map_err(|_| ValidationError::InvalidField("grantScope.resource"))?;
            if value.starts_with("@qos:") {
                return Err(ValidationError::InvalidField("grantScope.resource"));
            }
        }
    }
    if let Some(actions) = &scope.actions {
        if actions.is_empty() {
            return Err(ValidationError::InvalidField("grantScope.actions"));
        }
        validate_action_list(actions, "grantScope.actions")?;
    }
    validate_action_list(&scope.deny_actions, "grantScope.denyActions")?;
    if let Some(package) = &scope.package {
        validate_scope_id(&package.package_id, "grantScope.package.packageId")?;
        validate_scope_id(&package.version, "grantScope.package.version")?;
        if package.schema_digest.len() != 64
            || !package
                .schema_digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(ValidationError::InvalidField(
                "grantScope.package.schemaDigest",
            ));
        }
    }
    if let Some(budgets) = &scope.budgets {
        budgets.validate()?;
    }
    Ok(())
}

/// A v2-authored scope: valid, pinned to a package schema, and not the
/// degenerate v1 form.
///
/// # Errors
///
/// Returns the refusal or validation error that fails the check.
pub fn validate_authored_scope(scope: &GrantScope) -> Result<(), ValidationError> {
    validate_grant_scope(scope)?;
    if scope.package.is_none() || scope.is_degenerate() {
        return Err(ValidationError::InvalidField("grantScope.package"));
    }
    Ok(())
}

/// Validate a v2 set: version gate, 1..=[`MAX_GRANT_SCOPES`] scopes, each
/// authored and valid, no duplicates.
///
/// # Errors
///
/// Returns the refusal or validation error that fails the check.
pub fn validate_grant_scope_set(set: &GrantScopeSet) -> Result<(), ValidationError> {
    if set.version != GRANT_SCOPE_PROTOCOL_VERSION {
        return Err(ValidationError::UnsupportedVersion);
    }
    if set.scopes.is_empty() || set.scopes.len() > MAX_GRANT_SCOPES {
        return Err(ValidationError::InvalidField("grantScopes"));
    }
    for (i, scope) in set.scopes.iter().enumerate() {
        validate_authored_scope(scope)?;
        if set.scopes.iter().take(i).any(|prior| prior == scope) {
            return Err(ValidationError::InvalidField("grantScopes"));
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn tuple(resource: &str, action: &str) -> GrantAction {
        GrantAction {
            app_id: "so.tilo.word-processor".into(),
            record_id: "rec-1".into(),
            resource: resource.into(),
            action: action.into(),
        }
    }

    fn target<'a>(resource: &'a str, action: &'a str, class: OperationClass) -> ScopeTarget<'a> {
        ScopeTarget {
            app_id: "so.tilo.word-processor",
            record_id: "rec-1",
            resource,
            action,
            class: Some(class),
            required: GrantVerb::Write,
        }
    }

    pub(crate) fn edit_scope() -> GrantScope {
        GrantScope {
            app_id: "so.tilo.word-processor".into(),
            record: RecordScope::Exact("rec-1".into()),
            verb: GrantVerb::Write,
            classes: vec![
                OperationClass::Document,
                OperationClass::View,
                OperationClass::Query,
            ],
            resource: ResourceScope::Any,
            actions: None,
            deny_actions: vec!["accept-all".into(), "reject-all".into()],
            package: Some(PackagePin {
                package_id: "so.tilo.word-processor".into(),
                version: "0.1.0".into(),
                schema_digest: "a".repeat(64),
            }),
            budgets: None,
            explicit_workspace_consent: false,
        }
    }

    #[test]
    fn v1_tuples_become_exact_degenerate_scopes() {
        let v1 = [
            tuple("word-processor.document", "append-paragraph"),
            tuple("word-processor.document", "continue-paragraph"),
            tuple("word-processor.document", "append-paragraph"),
            tuple("word-processor.section:h1", "align-left"),
        ];
        let scopes = scopes_from_v1(&v1);
        assert_eq!(scopes.len(), 2);
        for scope in &scopes {
            validate_grant_scope(scope).unwrap();
            assert!(scope.is_degenerate());
        }
        for t in &v1 {
            // Class-blind: a v1 tuple keeps today's exact allow set whatever
            // class the manifest now assigns the action.
            for class in OperationClass::ALL {
                assert!(
                    scopes
                        .iter()
                        .any(|s| s.covers(&target(&t.resource, &t.action, class)))
                );
            }
        }
        let outside = [
            target("word-processor.document", "save", OperationClass::Document),
            target(
                "word-processor.section:h1",
                "append-paragraph",
                OperationClass::Document,
            ),
            ScopeTarget {
                record_id: "rec-2",
                ..target(
                    "word-processor.document",
                    "append-paragraph",
                    OperationClass::Document,
                )
            },
        ];
        for t in &outside {
            assert!(!scopes.iter().any(|s| s.covers(t)), "{t:?}");
        }
        // v1 → scopes → same set through the unified reader, never widened.
        let unified = scopes_from_grants(&v1[..2]).unwrap();
        assert_eq!(unified, scopes_from_v1(&v1[..2]));
    }

    #[test]
    fn edit_scope_covers_document_view_query_but_not_effects_or_disclose() {
        let scope = edit_scope();
        validate_authored_scope(&scope).unwrap();
        for (action, class) in [
            ("type-text", OperationClass::Document),
            ("select-range", OperationClass::View),
            ("read-text", OperationClass::Query),
        ] {
            assert!(scope.covers(&target("word-processor.document", action, class)));
        }
        for (action, class) in [
            ("save", OperationClass::EffectFsWrite),
            ("open", OperationClass::EffectFsRead),
            ("post", OperationClass::Disclose),
            ("accept-all", OperationClass::Document),
        ] {
            assert!(!scope.covers(&target("word-processor.document", action, class)));
        }
        // Unclassified never passes a v2 scope.
        let mut unclassified = target(
            "word-processor.document",
            "type-text",
            OperationClass::Document,
        );
        unclassified.class = None;
        assert!(!scope.covers(&unclassified));
        // Another record, another app, a weaker verb.
        let other_record = ScopeTarget {
            record_id: "rec-2",
            ..target(
                "word-processor.document",
                "type-text",
                OperationClass::Document,
            )
        };
        assert!(!scope.covers(&other_record));
        let chat = ScopeTarget {
            app_id: "so.tilo.chat",
            ..target("chat.channel:c", "post", OperationClass::Document)
        };
        assert!(!scope.covers(&chat));
        let read_only = GrantScope {
            verb: GrantVerb::Read,
            ..scope
        };
        assert!(!read_only.covers(&target(
            "word-processor.document",
            "type-text",
            OperationClass::Document
        )));
    }

    #[test]
    fn workspace_wildcard_needs_explicit_consent_and_is_not_degenerate() {
        let mut wildcard = GrantScope {
            record: RecordScope::InWorkspace,
            ..edit_scope()
        };
        validate_authored_scope(&wildcard).unwrap();
        let t = ScopeTarget {
            record_id: "rec-9",
            ..target(
                "word-processor.document",
                "type-text",
                OperationClass::Document,
            )
        };
        assert!(!wildcard.covers(&t));
        wildcard.explicit_workspace_consent = true;
        assert!(wildcard.covers(&t));
        // The consent flag on a non-wildcard scope is malformed.
        let bad = GrantScope {
            explicit_workspace_consent: true,
            ..edit_scope()
        };
        assert!(validate_grant_scope(&bad).is_err());
    }

    #[test]
    fn reserved_record_spellings_are_refused_everywhere() {
        assert_eq!(canonical_record("").unwrap(), PRIMARY_RECORD_ALIAS);
        assert_eq!(canonical_record("rec-1").unwrap(), "rec-1");
        assert_eq!(route_record(PRIMARY_RECORD_ALIAS).unwrap(), "");
        for reserved in [
            "@qos:package-primary",
            "@qos:anything",
            "*in-workspace",
            "*",
        ] {
            assert!(canonical_record(reserved).is_err(), "{reserved}");
        }
        assert!(route_record("*in-workspace").is_err());
        assert!(canonical_record("Récord").is_err());
        let literal = GrantScope {
            record: RecordScope::Exact("*in-workspace".into()),
            ..edit_scope()
        };
        assert!(validate_grant_scope(&literal).is_err());
        let v1 = [GrantAction {
            record_id: "@qos:other".into(),
            ..tuple("word-processor.document", "type-text")
        }];
        assert!(validate_grant_set(&v1).is_err());
    }

    #[test]
    fn carrier_round_trips_exactly_and_fails_closed_for_exact_matchers() {
        let scope = edit_scope();
        let carrier = scope_carrier(&scope).unwrap();
        assert!(is_scope_carrier(&carrier));
        assert_eq!(carrier.record_id, "rec-1");
        assert_eq!(decode_scope_carrier(&carrier).unwrap(), scope);
        assert_eq!(
            scopes_from_grants(std::slice::from_ref(&carrier)).unwrap(),
            vec![scope]
        );
        // A v1 exact matcher sees a tuple no surface publishes.
        assert!(carrier.resource.starts_with("@qos:"));
        // Tampered outer fields or non-canonical JSON are refused.
        let mut moved = carrier.clone();
        moved.record_id = "rec-2".into();
        assert!(decode_scope_carrier(&moved).is_err());
        let mut spaced = carrier.clone();
        spaced.resource = spaced.resource.replacen(":{", ": {", 1);
        assert!(decode_scope_carrier(&spaced).is_err());
        // Mixed sets and duplicates are refused; the carrier bound is 16.
        assert!(
            validate_grant_set(&[carrier.clone(), tuple("word-processor.document", "x")]).is_err()
        );
        assert!(validate_grant_set(&[carrier.clone(), carrier]).is_err());
        let many: Vec<_> = (0..=MAX_GRANT_SCOPES)
            .map(|i| {
                scope_carrier(&GrantScope {
                    record: RecordScope::Exact(format!("rec-{i}")),
                    ..edit_scope()
                })
                .unwrap()
            })
            .collect();
        assert!(validate_grant_set(&many[..MAX_GRANT_SCOPES]).is_ok());
        assert!(validate_grant_set(&many).is_err());
        // A degenerate scope is not an authored carrier.
        assert!(scope_carrier(&scopes_from_v1(&[tuple("r", "a")])[0]).is_err());
    }

    #[test]
    fn scope_set_is_version_gated_and_bounded() {
        let scope = edit_scope();
        let json = serde_json::to_string(&GrantScopeSet {
            version: GRANT_SCOPE_PROTOCOL_VERSION,
            scopes: vec![scope.clone()],
        })
        .unwrap();
        let parsed: GrantScopeSet = serde_json::from_str(&json).unwrap();
        validate_grant_scope_set(&parsed).unwrap();
        assert!(json.contains("\"record\":{\"exact\":\"rec-1\"}"), "{json}");
        assert!(
            json.contains("\"classes\":[\"document\",\"view\",\"query\"]"),
            "{json}"
        );

        let v1 = GrantScopeSet {
            version: 1,
            scopes: vec![scope.clone()],
        };
        assert_eq!(
            validate_grant_scope_set(&v1),
            Err(ValidationError::UnsupportedVersion)
        );
        let many = GrantScopeSet {
            version: GRANT_SCOPE_PROTOCOL_VERSION,
            scopes: (0..=MAX_GRANT_SCOPES)
                .map(|i| GrantScope {
                    app_id: format!("app-{i}"),
                    ..scope.clone()
                })
                .collect(),
        };
        assert!(validate_grant_scope_set(&many).is_err());
        // The A0 spelling `kinds` still parses.
        let legacy: GrantScope = serde_json::from_str(
            r#"{"appId":"so.tilo.chat","record":"inWorkspace","verb":"comment","kinds":["document"]}"#,
        )
        .unwrap();
        assert_eq!(legacy.record, RecordScope::InWorkspace);
        assert_eq!(legacy.resource, ResourceScope::Any);
        validate_grant_scope(&legacy).unwrap();
        assert!(
            serde_json::from_str::<GrantScope>(
                r#"{"appId":"a","record":"inWorkspace","verb":"write","classes":["document"],"extra":1}"#
            )
            .is_err()
        );
        // A bare "effect" class no longer exists: effects are named explicitly.
        assert!(
            serde_json::from_str::<GrantScope>(
                r#"{"appId":"a","record":"inWorkspace","verb":"write","classes":["effect"]}"#
            )
            .is_err()
        );
        // Bounds: records, budgets.
        let too_many = GrantScope {
            record: RecordScope::AnyOf(
                (0..=MAX_SCOPE_RECORDS)
                    .map(|i| format!("r{i:03}"))
                    .collect(),
            ),
            ..scope.clone()
        };
        assert!(validate_grant_scope(&too_many).is_err());
        let unsorted = GrantScope {
            record: RecordScope::AnyOf(vec!["b".into(), "a".into()]),
            ..scope.clone()
        };
        assert!(validate_grant_scope(&unsorted).is_err());
        let wide = GrantScope {
            budgets: Some(ScopeBudgets {
                wall_clock_secs: 86_400,
                ..ScopeBudgets::default()
            }),
            ..scope
        };
        assert!(validate_grant_scope(&wide).is_err());
    }

    #[test]
    fn resource_bounds_agree_across_v1_validation_layers() {
        // MUST-16: one resource bound (4096 bytes) for protocol and retained
        // grant validation; ids other than resource stay 128-byte opaque.
        let long = "r".repeat(crate::MAX_GRANT_RESOURCE_BYTES);
        assert!(validate_grant_set(&[tuple(&long, "a")]).is_ok());
        let too_long = "r".repeat(crate::MAX_GRANT_RESOURCE_BYTES + 1);
        assert!(validate_grant_set(&[tuple(&too_long, "a")]).is_err());
        assert!(validate_grant_set(&[tuple("r", &"a".repeat(129))]).is_err());
    }
}
