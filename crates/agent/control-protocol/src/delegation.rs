//! Delegation manifests: which published operations an agent may be granted,
//! with an explicit capability class each (security review MUST-4, MUST-15).
//!
//! - An operation is delegable only if a *trusted* manifest lists it with a
//!   class. The app's own `OperationKind` (default `Document`) never counts,
//!   and a declared kind that disagrees with the manifest class is refused.
//! - A package manifest is honoured only when its signature verifies against
//!   a publisher key pinned by the host. Today every package manifest has an
//!   all-zero publisher key and an empty signature, so none is honoured.
//! - Until first-party signing exists, the host pins [`DelegationRegistry::first_party`]:
//!   the Writer and Chat tables compiled into the Cloud host, which also runs
//!   those apps in-process as registered host workers. The code identity of
//!   every surface it classifies is therefore the host binary itself.
//! - One authoritative package-id ↔ runtime app-id map lives here.

use serde::{Deserialize, Serialize};

use crate::{GrantScope, GrantVerb, OperationClass, ValidationError};

pub const WRITER_APP_ID: &str = "so.tilo.word-processor";
pub const WRITER_PACKAGE_ID: &str = "so.tilo.word-processor";
pub const CHAT_APP_ID: &str = "so.tilo.chat";
pub const CHAT_PACKAGE_ID: &str = "so.tilo.chat";
pub const NOTES_APP_ID: &str = "so.tilo.notes";
pub const NOTES_PACKAGE_ID: &str = "so.tilo.notes";
/// The workspace desktop: the owner's own "open app" surface, not a package.
pub const DESKTOP_APP_ID: &str = "dev.qos.desktop";
pub const DESKTOP_PACKAGE_ID: &str = "dev.qos.desktop";
/// Resource prefix of the per-application open action (`desktop:app:<app>`).
pub const OPEN_APP_RESOURCE_PREFIX: &str = "desktop:app:";
/// Resource kind and action id of the per-application open action.
pub const OPEN_APP_RESOURCE_KIND: &str = "desktop.application";
pub const OPEN_APP_ACTION: &str = "open";
/// Version of the compiled first-party delegation tables. Bumping it (or
/// changing an entry) changes the schema digest and so refuses every scope
/// frozen against the old tables until the owner consents again.
pub const FIRST_PARTY_DELEGATION_VERSION: &str = "2026-09-24.1";

/// The kind an app declared for one published operation. Mirrors
/// `tilo_actions::OperationKind`; mapped at the call site.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeclaredKind {
    Document,
    View,
    Effect,
}

impl OperationClass {
    /// Declared kinds an app may use for an operation of this class. A
    /// mismatch means the app relabelled the operation (T3) and is refused.
    pub fn accepts_declared(self, declared: DeclaredKind) -> bool {
        match self {
            Self::Document | Self::Disclose => declared == DeclaredKind::Document,
            Self::View => declared == DeclaredKind::View,
            // Writer's read-text is published as an Effect today.
            Self::Query => matches!(declared, DeclaredKind::View | DeclaredKind::Effect),
            Self::AppOpen => declared == DeclaredKind::Effect,
            _ => declared == DeclaredKind::Effect,
        }
    }
}

/// One delegable operation schema entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DelegableOp {
    pub action: String,
    /// `None` = this action on any resource kind of the app.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_kind: Option<String>,
    pub class: OperationClass,
    /// Bulk review/destructive operations are never run unattended (MUST-7/8).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub bulk: bool,
    /// Name of a numeric argument field bounded by `maxDeleteCount` (T5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count_param: Option<String>,
}

/// A package's delegation section: package id, runtime app id, publisher
/// identity and the explicit delegable-operation schema.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DelegationManifest {
    pub package_id: String,
    pub app_id: String,
    pub version: String,
    /// `ed25519:<64 hex>` as in `tilo-app.toml [publisher]`.
    pub publisher: String,
    pub signature: String,
    pub operations: Vec<DelegableOp>,
}

/// How the host established trust in a manifest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManifestTrust {
    /// Compiled into the host and running as its own registered worker.
    FirstPartyPinned,
    /// The signature verified against a publisher key the host pins.
    VerifiedPinnedKey,
    /// Zero key, empty signature, or no signature at all.
    Unsigned,
    /// Signed by a key the host does not pin.
    UntrustedKey,
    /// The signature does not verify.
    BadSignature,
}

/// Classify a package manifest's publisher trust. `verify(key, signature,
/// signed_bytes)` is the caller's ed25519 check (this crate stays
/// dependency-free); it is only consulted for a pinned, non-zero key.
pub fn manifest_trust(
    manifest: &DelegationManifest,
    pinned_keys: &[[u8; 32]],
    verify: impl Fn(&[u8; 32], &str, &[u8]) -> bool,
) -> ManifestTrust {
    let Some(key) = manifest
        .publisher
        .strip_prefix("ed25519:")
        .and_then(decode_key)
    else {
        return ManifestTrust::Unsigned;
    };
    if key == [0; 32] || manifest.signature.is_empty() {
        return ManifestTrust::Unsigned;
    }
    if !pinned_keys.contains(&key) {
        return ManifestTrust::UntrustedKey;
    }
    let mut unsigned = manifest.clone();
    unsigned.signature.clear();
    let Ok(bytes) = serde_json::to_vec(&unsigned) else {
        return ManifestTrust::BadSignature;
    };
    if verify(&key, &manifest.signature, &bytes) {
        ManifestTrust::VerifiedPinnedKey
    } else {
        ManifestTrust::BadSignature
    }
}
fn decode_key(hex: &str) -> Option<[u8; 32]> {
    if hex.len() != 64 {
        return None;
    }
    let mut key = [0; 32];
    for (byte, pair) in key.iter_mut().zip(hex.as_bytes().chunks_exact(2)) {
        *byte = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(key)
}

/// The host's trusted delegation manifests. Only trusted manifests enter.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DelegationRegistry {
    manifests: Vec<DelegationManifest>,
}

impl DelegationRegistry {
    /// No delegable app at all.
    pub fn empty() -> Self {
        Self::default()
    }
    /// The host-pinned first-party tables (Writer, Chat, Notes, and the
    /// workspace desktop's open-application action).
    pub fn first_party() -> Self {
        Self {
            manifests: vec![
                writer_manifest(),
                chat_manifest(),
                notes_manifest(),
                desktop_manifest(),
            ],
        }
    }
    /// Admit a package manifest. Refused unless its trust is pinned or
    /// verified, its ids are canonical, and no manifest already claims the
    /// package or app id (one authoritative package ↔ app map).
    ///
    /// # Errors
    ///
    /// Returns the refusal or validation error that fails the check.
    pub fn admit(
        &mut self,
        manifest: DelegationManifest,
        trust: ManifestTrust,
    ) -> Result<(), ValidationError> {
        if !matches!(
            trust,
            ManifestTrust::FirstPartyPinned | ManifestTrust::VerifiedPinnedKey
        ) {
            return Err(ValidationError::InvalidField("delegation.trust"));
        }
        validate_manifest(&manifest)?;
        if self
            .manifests
            .iter()
            .any(|m| m.package_id == manifest.package_id || m.app_id == manifest.app_id)
        {
            return Err(ValidationError::InvalidField("delegation.packageId"));
        }
        self.manifests.push(manifest);
        Ok(())
    }
    /// A workspace admin may narrow (never widen) what is delegable.
    #[must_use]
    pub fn narrowed(&self, keep: impl Fn(&str, &DelegableOp) -> bool) -> Self {
        Self {
            manifests: self
                .manifests
                .iter()
                .map(|m| DelegationManifest {
                    operations: m
                        .operations
                        .iter()
                        .filter(|op| keep(&m.app_id, op))
                        .cloned()
                        .collect(),
                    ..m.clone()
                })
                .collect(),
        }
    }
    pub fn manifest(&self, app_id: &str) -> Option<&DelegationManifest> {
        self.manifests.iter().find(|m| m.app_id == app_id)
    }
    /// Replaces the hard-coded `so.tilo.word-processor | so.tilo.chat` match.
    pub fn is_delegable_app(&self, app_id: &str) -> bool {
        self.manifest(app_id)
            .is_some_and(|m| !m.operations.is_empty())
    }
    pub fn app_for_package(&self, package_id: &str) -> Option<&str> {
        self.manifests
            .iter()
            .find(|m| m.package_id == package_id)
            .map(|m| m.app_id.as_str())
    }
    pub fn package_for_app(&self, app_id: &str) -> Option<&str> {
        self.manifest(app_id).map(|m| m.package_id.as_str())
    }
    /// The schema entry of one published operation. A resource-kind-specific
    /// entry wins over an any-kind entry for the same action.
    pub fn entry(&self, app_id: &str, resource_kind: &str, action: &str) -> Option<&DelegableOp> {
        let ops = &self.manifest(app_id)?.operations;
        ops.iter()
            .find(|op| op.action == action && op.resource_kind.as_deref() == Some(resource_kind))
            .or_else(|| {
                ops.iter()
                    .find(|op| op.action == action && op.resource_kind.is_none())
            })
    }
    /// Explicit class of a published operation, or `None` when it is not
    /// delegable or the app's declared kind disagrees with the class.
    pub fn classify(
        &self,
        app_id: &str,
        resource_kind: &str,
        action: &str,
        declared: DeclaredKind,
    ) -> Option<OperationClass> {
        self.entry(app_id, resource_kind, action)
            .map(|op| op.class)
            .filter(|class| class.accepts_declared(declared))
    }
    /// Every class the manifest assigns `action` on any resource kind. Used
    /// where only the action id is known (the host step ledger); the broker
    /// classifies precisely with the live descriptor.
    pub fn classes_of_action(&self, app_id: &str, action: &str) -> Vec<OperationClass> {
        let mut classes: Vec<_> = self
            .manifest(app_id)
            .map(|m| {
                m.operations
                    .iter()
                    .filter(|op| op.action == action)
                    .map(|op| op.class)
                    .collect()
            })
            .unwrap_or_default();
        classes.sort();
        classes.dedup();
        classes
    }
    /// Canonical bytes of one app's delegation schema. The host hashes them
    /// into [`crate::PackagePin::schema_digest`].
    pub fn schema_bytes(&self, app_id: &str) -> Option<Vec<u8>> {
        let manifest = self.manifest(app_id)?;
        let mut ops = manifest.operations.clone();
        ops.sort_by(|a, b| (&a.action, &a.resource_kind).cmp(&(&b.action, &b.resource_kind)));
        serde_json::to_vec(&serde_json::json!({
            "delegationSchema": 1,
            "packageId": manifest.package_id,
            "appId": manifest.app_id,
            "version": manifest.version,
            "operations": ops,
        }))
        .ok()
    }
}

fn validate_manifest(manifest: &DelegationManifest) -> Result<(), ValidationError> {
    let id = |v: &str, field| {
        if v.is_empty() || v.len() > crate::MAX_OPAQUE_ID_BYTES || !v.is_ascii() || v.contains('*')
        {
            Err(ValidationError::InvalidField(field))
        } else {
            Ok(())
        }
    };
    id(&manifest.package_id, "delegation.packageId")?;
    id(&manifest.app_id, "delegation.appId")?;
    id(&manifest.version, "delegation.version")?;
    if manifest.operations.len() > 512 {
        return Err(ValidationError::InvalidField("delegation.operations"));
    }
    let mut seen = std::collections::HashSet::new();
    for op in &manifest.operations {
        id(&op.action, "delegation.action")?;
        if let Some(kind) = &op.resource_kind {
            id(kind, "delegation.resourceKind")?;
        }
        if !seen.insert((&op.action, &op.resource_kind)) {
            return Err(ValidationError::InvalidField("delegation.operations"));
        }
    }
    Ok(())
}

fn op(action: &str, class: OperationClass) -> DelegableOp {
    DelegableOp {
        action: action.into(),
        resource_kind: None,
        class,
        bulk: false,
        count_param: None,
    }
}
fn first_party(package: &str, app: &str, operations: Vec<DelegableOp>) -> DelegationManifest {
    DelegationManifest {
        package_id: package.into(),
        app_id: app.into(),
        version: FIRST_PARTY_DELEGATION_VERSION.into(),
        publisher: "host-pinned:first-party".into(),
        signature: String::new(),
        operations,
    }
}

fn writer_manifest() -> DelegationManifest {
    use OperationClass::{Document, EffectFsRead, EffectFsWrite, Query, View};
    let mut ops = vec![];
    // Editing primitives (lane A §4.2) and the legacy position-free ops.
    for action in [
        "type-text",
        "new-paragraph",
        "toggle-mark",
        "set-mark",
        "clear-mark",
        "set-paragraph-style",
        "apply-named-style",
        "paste",
        "apply-spelling-suggestion",
        "append-paragraph",
        "continue-paragraph",
        "replace-selection",
        "toggle-bold",
        "align-left",
        "align-center",
        "align-right",
        "align-justify",
        "insert-page-break",
        "insert-table",
        "append-row",
        "append-column",
        "set-hyperlink",
        "add-comment",
        "reply-to-comment",
        "replace",
        "delete-row",
        "delete-column",
    ] {
        ops.push(op(action, Document));
    }
    ops.push(DelegableOp {
        count_param: Some("count".into()),
        ..op("erase-text", Document)
    });
    // Tracked-change review in bulk is refused unattended (T5).
    for action in ["accept-all", "reject-all"] {
        ops.push(DelegableOp {
            bulk: true,
            ..op(action, Document)
        });
    }
    for action in [
        "move-caret",
        "select-range",
        "select-block",
        "select-all",
        "find-text",
        "go-to",
        "go-to-anchor",
        "find",
        "history",
        "markup",
        "outline",
        "ruler",
        "spellcheck",
        "styles",
        "zoom-in",
        "zoom-out",
        "zoom-reset",
    ] {
        ops.push(op(action, View));
    }
    ops.push(op("read-text", Query));
    // Host filesystem and blob store: classed so they are never delegable
    // on a Cloud host worker, even if Writer mislabels them (T3, T11).
    ops.push(op("open", EffectFsRead));
    ops.push(op("place-image", EffectFsRead));
    ops.push(op("save", EffectFsWrite));
    ops.push(op("save-as", EffectFsWrite));
    first_party(WRITER_PACKAGE_ID, WRITER_APP_ID, ops)
}

fn chat_manifest() -> DelegationManifest {
    use OperationClass::{Disclose, Document, EffectFsRead, EffectMedia, Query, View};
    let mut ops = vec![];
    // Sending content to everyone who can read the channel (T9).
    for action in ["post", "send-message", "edit", "react"] {
        ops.push(op(action, Disclose));
    }
    for action in [
        "mark-read",
        "pin",
        "unpin",
        "rename",
        "archive",
        "unarchive",
        "collapse",
        "expand",
        "new-channel",
        "new-category",
        "delete",
    ] {
        ops.push(op(action, Document));
    }
    ops.push(op("open", View));
    ops.push(op("read-text", Query));
    ops.push(op("read-messages", Query));
    ops.push(op("attach", EffectFsRead));
    for action in [
        "join",
        "leave",
        "select-input",
        "select-output",
        "mute",
        "unmute",
        "status",
    ] {
        ops.push(op(action, EffectMedia));
    }
    first_party(CHAT_PACKAGE_ID, CHAT_APP_ID, ops)
}

fn notes_manifest() -> DelegationManifest {
    use OperationClass::{Document, EffectFsRead, EffectFsWrite, View};
    let mut ops = vec![];
    for action in [
        "new-page",
        "new-child",
        "rename",
        "replace-text",
        "duplicate",
        "move-up",
        "move-down",
        "indent",
        "outdent",
        "daily-note",
        "new-tag",
        "new-and-assign-tag",
        "assign-page",
        "unassign-page",
        // Destructive: confirmation-requiring, so refused unattended (T4).
        "delete",
    ] {
        ops.push(op(action, Document));
    }
    for action in ["open", "back", "forward"] {
        ops.push(op(action, View));
    }
    // Host filesystem: never delegable on a Cloud host worker (T3, T11).
    ops.push(op("import-markdown", EffectFsRead));
    ops.push(op("insert-image", EffectFsRead));
    ops.push(op("export-markdown", EffectFsWrite));
    first_party(NOTES_PACKAGE_ID, NOTES_APP_ID, ops)
}

/// The workspace desktop's per-application open action. Only `open` on
/// `desktop.application` resources; the raw layout command is not delegable.
fn desktop_manifest() -> DelegationManifest {
    first_party(
        DESKTOP_PACKAGE_ID,
        DESKTOP_APP_ID,
        vec![DelegableOp {
            resource_kind: Some(OPEN_APP_RESOURCE_KIND.into()),
            ..op(OPEN_APP_ACTION, OperationClass::AppOpen)
        }],
    )
}

/// A record one task opened through the desktop's open action.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OpenedRecord {
    pub app_id: String,
    /// Live route record id (`""` = package primary).
    pub record_id: String,
}

/// The task's effective scopes: its frozen scopes, plus one scope for each
/// record the task itself opened with an open action its frozen scopes
/// cover. Consent to open an application is consent to work in the record
/// that open creates, within the task's own verb and non-effect classes.
/// Nothing else widens: every step still runs the full authorization (the
/// agent and its spawner on the record, manifest class, confirmation).
///
/// `opened` must come from the host's own durable record of the open (never
/// from the agent). `schema_digest` is the host's live digest of an app's
/// delegation schema.
pub fn scopes_with_opened_records(
    frozen: &[GrantScope],
    opened: &[OpenedRecord],
    registry: &DelegationRegistry,
    schema_digest: &dyn Fn(&str) -> Option<String>,
) -> Vec<GrantScope> {
    let mut scopes = frozen.to_vec();
    let work: Vec<&GrantScope> = frozen
        .iter()
        .filter(|s| s.app_id != DESKTOP_APP_ID && !s.is_degenerate())
        .collect();
    let verb = work
        .iter()
        .map(|s| s.verb)
        .max()
        .unwrap_or(GrantVerb::Write);
    let mut classes: Vec<OperationClass> = work
        .iter()
        .flat_map(|s| s.classes.iter().copied())
        .filter(|c| !c.is_effect() && *c != OperationClass::AppOpen)
        .collect();
    if classes.is_empty() {
        classes = vec![
            OperationClass::Document,
            OperationClass::View,
            OperationClass::Query,
        ];
    }
    classes.sort();
    classes.dedup();
    for record in opened {
        let open_resource = format!("{OPEN_APP_RESOURCE_PREFIX}{}", record.app_id);
        let open = crate::ScopeTarget {
            app_id: DESKTOP_APP_ID,
            record_id: crate::PRIMARY_RECORD_ALIAS,
            resource: &open_resource,
            action: OPEN_APP_ACTION,
            class: Some(OperationClass::AppOpen),
            required: GrantVerb::Write,
        };
        if !frozen.iter().any(|s| s.covers(&open)) {
            continue;
        }
        let (Ok(canonical), Some(manifest), Some(digest)) = (
            crate::canonical_record(&record.record_id),
            registry.manifest(&record.app_id),
            schema_digest(&record.app_id),
        ) else {
            continue;
        };
        scopes.push(GrantScope {
            app_id: record.app_id.clone(),
            record: crate::RecordScope::Exact(canonical),
            verb,
            classes: classes.clone(),
            resource: crate::ResourceScope::Any,
            actions: None,
            deny_actions: vec![],
            package: Some(crate::PackagePin {
                package_id: manifest.package_id.clone(),
                version: manifest.version.clone(),
                schema_digest: digest,
            }),
            budgets: None,
            explicit_workspace_consent: false,
        });
    }
    scopes
}

/// Consent text derived from frozen scopes only (MUST-19). `record_label`
/// resolves a canonical record id to a user-visible name.
pub fn consent_text(
    agent: &str,
    scopes: &[GrantScope],
    record_label: impl Fn(&str, &str) -> String,
) -> String {
    let mut grants = vec![];
    let mut can = std::collections::BTreeSet::new();
    for scope in scopes {
        let names: Vec<String> = if scope.records().is_empty() {
            vec!["every record in this workspace".into()]
        } else {
            scope
                .records()
                .iter()
                .map(|r| record_label(&scope.app_id, r))
                .collect()
        };
        let mut abilities = vec![];
        if scope.classes.contains(&OperationClass::Query) {
            abilities.push("read");
        }
        if scope.classes.contains(&OperationClass::Document) && scope.verb == GrantVerb::Write {
            abilities.push("edit (typing, formatting, comments)");
        } else if scope.classes.contains(&OperationClass::Document) {
            abilities.push("comment");
        }
        if scope.classes.contains(&OperationClass::Disclose) {
            abilities.push("post messages");
        }
        if scope.classes.contains(&OperationClass::AppOpen) {
            // Named without records: it opens applications on the desktop.
            grants.push("open applications and work in the documents it opens".into());
            continue;
        }
        for class in &scope.classes {
            can.insert(*class);
        }
        if abilities.is_empty() && scope.classes.contains(&OperationClass::View) {
            abilities.push("view");
        }
        grants.push(format!("{} {}", abilities.join(", "), names.join(", ")));
    }
    let mut cannot = vec![];
    if !can.contains(&OperationClass::Disclose) {
        cannot.push("post in chat");
    }
    if !can.iter().any(|c| {
        matches!(
            c,
            OperationClass::EffectFsRead | OperationClass::EffectFsWrite
        )
    }) {
        cannot.push("open or save files");
    }
    if !can.contains(&OperationClass::EffectNet) {
        cannot.push("use the network");
    }
    let mut text = format!("Allow {agent} to {}.", grants.join("; "));
    if !cannot.is_empty() {
        text.push_str(" It cannot ");
        text.push_str(&cannot.join(", "));
        text.push('.');
    }
    text.push_str(" It cannot accept or reject tracked changes.");
    let minutes = scopes
        .iter()
        .map(|s| s.budgets().wall_clock_secs)
        .min()
        .unwrap_or(0)
        / 60;
    text.push_str(" Stops after ");
    text.push_str(&minutes.to_string());
    text.push_str(" min or when you revoke it.");
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_party_registry_classifies_explicitly_and_refuses_relabels() {
        let r = DelegationRegistry::first_party();
        assert!(r.is_delegable_app(WRITER_APP_ID) && r.is_delegable_app(CHAT_APP_ID));
        assert!(r.is_delegable_app(NOTES_APP_ID) && r.is_delegable_app(DESKTOP_APP_ID));
        assert!(!r.is_delegable_app("so.tilo.calculator"));
        assert_eq!(
            r.classify(
                DESKTOP_APP_ID,
                OPEN_APP_RESOURCE_KIND,
                OPEN_APP_ACTION,
                DeclaredKind::Effect
            ),
            Some(OperationClass::AppOpen)
        );
        // Only the open action on application resources, never the raw
        // desktop layout command.
        assert_eq!(
            r.classify(DESKTOP_APP_ID, "desktop", "command", DeclaredKind::Effect),
            None
        );
        assert_eq!(
            r.classify(
                NOTES_APP_ID,
                "notes.notebook",
                "new-page",
                DeclaredKind::Document
            ),
            Some(OperationClass::Document)
        );
        assert_eq!(r.app_for_package(WRITER_PACKAGE_ID), Some(WRITER_APP_ID));
        assert_eq!(r.package_for_app(CHAT_APP_ID), Some(CHAT_PACKAGE_ID));
        let k = "word-processor.document";
        assert_eq!(
            r.classify(WRITER_APP_ID, k, "type-text", DeclaredKind::Document),
            Some(OperationClass::Document)
        );
        assert_eq!(
            r.classify(WRITER_APP_ID, k, "read-text", DeclaredKind::Effect),
            Some(OperationClass::Query)
        );
        assert_eq!(
            r.classify(WRITER_APP_ID, k, "move-caret", DeclaredKind::View),
            Some(OperationClass::View)
        );
        // `save` published as Document (T3 relabel) → not delegable.
        assert_eq!(
            r.classify(WRITER_APP_ID, k, "save", DeclaredKind::Document),
            None
        );
        assert_eq!(
            r.classify(WRITER_APP_ID, k, "save", DeclaredKind::Effect),
            Some(OperationClass::EffectFsWrite)
        );
        // Unlisted op, even if the app calls it Document → not delegable.
        assert_eq!(
            r.classify(WRITER_APP_ID, k, "delete-style", DeclaredKind::Document),
            None
        );
        // Chat post is disclose; open is a view only on channels.
        assert_eq!(
            r.classify(CHAT_APP_ID, "chat.channel", "post", DeclaredKind::Document),
            Some(OperationClass::Disclose)
        );
        assert_eq!(
            r.classify(CHAT_APP_ID, "chat.workspace", "open", DeclaredKind::View),
            Some(OperationClass::View)
        );
        assert!(r.entry(WRITER_APP_ID, k, "accept-all").unwrap().bulk);
        assert_eq!(
            r.entry(WRITER_APP_ID, k, "erase-text")
                .unwrap()
                .count_param
                .as_deref(),
            Some("count")
        );
        assert_eq!(
            r.classes_of_action(CHAT_APP_ID, "open"),
            vec![OperationClass::View]
        );
        // Schema bytes are deterministic and differ per app.
        assert_eq!(r.schema_bytes(WRITER_APP_ID), r.schema_bytes(WRITER_APP_ID));
        assert_ne!(r.schema_bytes(WRITER_APP_ID), r.schema_bytes(CHAT_APP_ID));
        // Narrowing never widens.
        let narrowed = r.narrowed(|_, op| op.class != OperationClass::Disclose);
        assert_eq!(
            narrowed.classify(CHAT_APP_ID, "chat.channel", "post", DeclaredKind::Document),
            None
        );
        assert_ne!(
            narrowed.schema_bytes(CHAT_APP_ID),
            r.schema_bytes(CHAT_APP_ID)
        );
    }

    fn package_manifest(publisher: &str, signature: &str) -> DelegationManifest {
        DelegationManifest {
            package_id: "so.tilo.calculator".into(),
            app_id: "so.tilo.calculator".into(),
            version: "0.1.0".into(),
            publisher: publisher.into(),
            signature: signature.into(),
            operations: vec![op("*", OperationClass::Document)],
        }
    }

    #[test]
    fn unsigned_zero_key_and_unpinned_manifests_are_not_delegable() {
        let zero = format!("ed25519:{}", "0".repeat(64));
        let key = [7u8; 32];
        let key_hex = format!("ed25519:{}", "07".repeat(32));
        let always = |_: &[u8; 32], _: &str, _: &[u8]| true;
        assert_eq!(
            manifest_trust(&package_manifest(&zero, ""), &[key], always),
            ManifestTrust::Unsigned
        );
        assert_eq!(
            manifest_trust(&package_manifest(&zero, "sig"), &[key], always),
            ManifestTrust::Unsigned
        );
        assert_eq!(
            manifest_trust(&package_manifest(&key_hex, ""), &[key], always),
            ManifestTrust::Unsigned
        );
        assert_eq!(
            manifest_trust(&package_manifest(&key_hex, "sig"), &[], always),
            ManifestTrust::UntrustedKey
        );
        assert_eq!(
            manifest_trust(&package_manifest(&key_hex, "sig"), &[key], |_, _, _| false),
            ManifestTrust::BadSignature
        );
        assert_eq!(
            manifest_trust(&package_manifest(&key_hex, "sig"), &[key], always),
            ManifestTrust::VerifiedPinnedKey
        );
        let mut registry = DelegationRegistry::first_party();
        for trust in [
            ManifestTrust::Unsigned,
            ManifestTrust::UntrustedKey,
            ManifestTrust::BadSignature,
        ] {
            let mut m = package_manifest(&key_hex, "sig");
            m.operations = vec![op("insert", OperationClass::Document)];
            assert!(registry.admit(m, trust).is_err());
        }
        assert!(!registry.is_delegable_app("so.tilo.calculator"));
        // A wildcard op id is malformed even when trusted.
        assert!(
            registry
                .admit(
                    package_manifest(&key_hex, "sig"),
                    ManifestTrust::VerifiedPinnedKey
                )
                .is_err()
        );
        // A manifest claiming an existing app or package id is refused.
        let mut stolen = package_manifest(&key_hex, "sig");
        stolen.app_id = WRITER_APP_ID.into();
        stolen.operations = vec![op("insert", OperationClass::Document)];
        assert!(
            registry
                .admit(stolen, ManifestTrust::VerifiedPinnedKey)
                .is_err()
        );
        let mut good = package_manifest(&key_hex, "sig");
        good.operations = vec![op("insert", OperationClass::Document)];
        registry
            .admit(good, ManifestTrust::VerifiedPinnedKey)
            .unwrap();
        assert!(registry.is_delegable_app("so.tilo.notes"));
    }

    #[test]
    fn consent_text_names_records_and_what_is_absent() {
        let scope = crate::grant_scope::tests::edit_scope();
        let text = consent_text("Agent", &[scope], |_, r| format!("\u{201c}{r}\u{201d}"));
        assert!(
            text.starts_with(
                "Allow Agent to read, edit (typing, formatting, comments) \u{201c}rec-1\u{201d}."
            ),
            "{text}"
        );
        assert!(
            text.contains("It cannot post in chat, open or save files, use the network."),
            "{text}"
        );
        assert!(text.contains("Stops after 15 min"), "{text}");
    }

    #[test]
    fn an_opened_record_is_covered_only_after_a_covered_open() {
        use crate::{PackagePin, RecordScope, ResourceScope, ScopeTarget};
        let r = DelegationRegistry::first_party();
        let digest = |app: &str| r.schema_bytes(app).map(|b| format!("{:064x}", b.len()));
        let pin = |app: &str| PackagePin {
            package_id: r.package_for_app(app).unwrap().into(),
            version: FIRST_PARTY_DELEGATION_VERSION.into(),
            schema_digest: digest(app).unwrap(),
        };
        let writer = GrantScope {
            app_id: WRITER_APP_ID.into(),
            record: RecordScope::Exact("tab:0000000000000001".into()),
            verb: GrantVerb::Write,
            classes: vec![OperationClass::Document, OperationClass::View],
            resource: ResourceScope::Any,
            actions: None,
            deny_actions: vec![],
            package: Some(pin(WRITER_APP_ID)),
            budgets: None,
            explicit_workspace_consent: false,
        };
        let desktop = GrantScope {
            app_id: DESKTOP_APP_ID.into(),
            record: RecordScope::Exact(crate::PRIMARY_RECORD_ALIAS.into()),
            classes: vec![OperationClass::AppOpen],
            resource: ResourceScope::Prefix(OPEN_APP_RESOURCE_PREFIX.into()),
            actions: Some(vec![OPEN_APP_ACTION.into()]),
            package: Some(pin(DESKTOP_APP_ID)),
            ..writer.clone()
        };
        let opened = [OpenedRecord {
            app_id: NOTES_APP_ID.into(),
            record_id: "tab:00000000000000aa".into(),
        }];
        let target = |class| ScopeTarget {
            app_id: NOTES_APP_ID,
            record_id: "tab:00000000000000aa",
            resource: "notes.notebook",
            action: "new-page",
            class: Some(class),
            required: GrantVerb::Write,
        };
        // Without a covered open, the record is outside the task.
        let without = scopes_with_opened_records(&[writer.clone()], &opened, &r, &digest);
        assert!(
            !without
                .iter()
                .any(|s| s.covers(&target(OperationClass::Document)))
        );
        let with = scopes_with_opened_records(&[writer.clone(), desktop], &opened, &r, &digest);
        assert!(
            with.iter()
                .any(|s| s.covers(&target(OperationClass::Document)))
        );
        // Within the task's own classes only: no disclose, no effect.
        assert!(
            !with
                .iter()
                .any(|s| s.covers(&target(OperationClass::Disclose)))
        );
        assert!(
            !with
                .iter()
                .any(|s| s.covers(&target(OperationClass::EffectFsRead)))
        );
        // Another record of the same app stays outside.
        assert!(!with.iter().any(|s| s.covers(&ScopeTarget {
            record_id: "tab:00000000000000bb",
            ..target(OperationClass::Document)
        })));
    }
}
