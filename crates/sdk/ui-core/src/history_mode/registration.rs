//! Explicit ownership and presentation registration for managed roots.
use super::{HistoryResource, preview::PreviewFactory, restore::HistoryRestoreSchema};
use crate::widget::Widget;
use automerge::transaction::Transactable as _;

pub struct ManagedRoot {
    pub(crate) widget: Box<dyn Widget>,
    pub(crate) history: HistoryRegistration,
}
impl ManagedRoot {
    pub fn new(widget: Box<dyn Widget>, history: HistoryRegistration) -> Self {
        Self { widget, history }
    }
}

pub struct HistoryDefinition {
    pub app_id: &'static str,
    pub scopes: &'static [HistoryScopeDefinition],
}
pub struct HistoryScopeDefinition {
    pub key: &'static str,
    pub title: &'static str,
    pub presentation: HistoryPresentation,
    pub content_schema: Option<&'static HistoryRestoreSchema>,
}
pub enum HistoryPresentation {
    Domain {
        create: PreviewFactory,
    },
    /// Stateless source factory installed only after scope/custody validation.
    DomainWithAssets {
        create: PreviewFactory,
        assets: fn() -> std::rc::Rc<dyn super::HistoricalAssetSource>,
    },
    CheckedFiles {
        create: super::checked_files::FilePreviewFactory,
    },
    External {
        reason: &'static str,
    },
}

impl HistoryPresentation {
    /// Presentation factory only; does not construct or invoke an asset source.
    pub fn preview_factory(&self) -> Option<PreviewFactory> {
        match self {
            Self::Domain { create } | Self::DomainWithAssets { create, .. } => Some(*create),
            Self::CheckedFiles { .. } | Self::External { .. } => None,
        }
    }
}

/// Immutable identity supplied by the owning runner. This carries no document.
/// Bound runners resolve their actual checkout instead of accepting a foreign identity.
#[derive(Clone)]
#[allow(
    clippy::large_enum_variant,
    reason = "registration keeps an opaque owner witness inline; it is not a per-frame message"
)]
pub enum HistorySourceRegistration {
    CheckedFiles {
        scope: &'static str,
        source: super::checked_files::FileHistorySource,
    },
    HostDocument {
        scope: &'static str,
        resource: HistoryResource,
    },
    OwnedDocument {
        scope: &'static str,
        owner: super::HistoryOwner,
        resource: HistoryResource,
    },
    Unversioned {
        scope: &'static str,
        reason: &'static str,
    },
}
impl HistorySourceRegistration {
    /// Capture the mounted component/tile's actual custody and storage identity.
    /// # Errors
    /// Refuses unavailable custody or an owner belonging to another application.
    pub fn owned(
        scope: &'static str,
        app_id: &str,
        owner: super::HistoryOwner,
    ) -> Result<Self, HistoryRegistrationError> {
        let resource = owner.resource(app_id)?;
        Ok(Self::OwnedDocument {
            scope,
            owner,
            resource,
        })
    }
    pub(crate) fn scope(&self) -> &'static str {
        match self {
            Self::HostDocument { scope, .. }
            | Self::OwnedDocument { scope, .. }
            | Self::Unversioned { scope, .. }
            | Self::CheckedFiles { scope, .. } => scope,
        }
    }
    /// Committed frontier for the refresh predicate, preserving source admission.
    pub(crate) fn committed_heads(
        &self,
        outer: &automerge::AutoCommit,
    ) -> Result<Vec<automerge::ChangeHash>, HistoryRegistrationError> {
        match self {
            Self::OwnedDocument { owner, .. } => owner.committed_heads(outer),
            Self::CheckedFiles { .. } => Err(HistoryRegistrationError::UnsupportedOwner),
            Self::HostDocument { .. } | Self::Unversioned { .. } => Ok(outer.base_heads()),
        }
    }

    pub(crate) fn document(
        &self,
        outer: &automerge::AutoCommit,
    ) -> Result<automerge::AutoCommit, HistoryRegistrationError> {
        match self {
            Self::OwnedDocument { owner, .. } => owner.document(outer),
            Self::CheckedFiles { .. } => Err(HistoryRegistrationError::UnsupportedOwner),
            Self::HostDocument { .. } | Self::Unversioned { .. } => Ok(outer.clone()),
        }
    }
}
pub struct HistoryRegistration {
    pub(crate) definition: &'static HistoryDefinition,
    pub(crate) sources: Vec<HistorySourceRegistration>,
    pub(crate) assets:
        std::collections::HashMap<&'static str, std::rc::Rc<dyn super::HistoricalAssetSource>>,
}
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum HistoryRegistrationError {
    #[error("History registration is required for a managed application")]
    Missing,
    #[error("This managed runner currently accepts one History scope")]
    UnsupportedScopes,
    #[error("This document owner requires the component History runner")]
    UnsupportedOwner,
    #[error("History needs the runner's explicit profile and lifetime namespace")]
    InvalidNamespace,
    #[error("History scopes must be named, unique and have exactly one owning source")]
    InvalidScopes,
    #[error("A versioned resource requires its domain presentation")]
    MissingPresentation,
    #[error("The History source belongs to another application")]
    ForeignSource,
}
impl HistoryRegistration {
    /// A registration over an app-global state document, for tests of
    /// History's presentation. A persistence path identifies storage; it is
    /// not a user-file write target.
    #[cfg(test)]
    #[cfg(feature = "native")]
    pub(crate) fn installed_app_state(
        definition: &'static HistoryDefinition,
        autosave: Option<&crate::persist::DocWriter>,
        lifetime: uuid::Uuid,
    ) -> Result<Self, HistoryRegistrationError> {
        if definition
            .scopes
            .iter()
            .any(|scope| matches!(scope.presentation, HistoryPresentation::CheckedFiles { .. }))
        {
            return Err(HistoryRegistrationError::UnsupportedOwner);
        }
        let resource = if let Some(saver) =
            autosave.filter(|saver| !saver.document_path().as_os_str().is_empty())
        {
            if saver.package_id() != definition.app_id {
                return Err(HistoryRegistrationError::ForeignSource);
            }
            HistoryResource::new(
                saver.document_path().to_string_lossy().into_owned(),
                format!("host:{lifetime}"),
                definition.app_id,
                crate::RecordId::derived(
                    "app-state",
                    &crate::RecordId::for_file(saver.document_path()),
                ),
            )
        } else {
            HistoryResource::new(
                format!("memory:{lifetime}"),
                format!("host:{lifetime}"),
                definition.app_id,
                crate::RecordId::derived("app-state", &crate::RecordId::for_tab(0)),
            )
        };
        let sources = definition
            .scopes
            .iter()
            .map(|scope| match scope.presentation {
                HistoryPresentation::Domain { .. }
                | HistoryPresentation::DomainWithAssets { .. } => {
                    Ok(HistorySourceRegistration::HostDocument {
                        scope: scope.key,
                        resource: resource.clone(),
                    })
                }
                HistoryPresentation::External { reason } => {
                    Ok(HistorySourceRegistration::Unversioned {
                        scope: scope.key,
                        reason,
                    })
                }
                HistoryPresentation::CheckedFiles { .. } => {
                    Err(HistoryRegistrationError::UnsupportedOwner)
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        Self::new(definition, sources)
    }

    /// Build a registration with exactly one source for each declared scope.
    ///
    /// # Errors
    /// Returns an error for invalid scopes, missing presentations or foreign sources.
    pub fn new(
        definition: &'static HistoryDefinition,
        sources: Vec<HistorySourceRegistration>,
    ) -> Result<Self, HistoryRegistrationError> {
        let mut registration = Self {
            definition,
            sources,
            assets: std::collections::HashMap::new(),
        };
        registration.validate()?;
        for scope in definition.scopes {
            if let HistoryPresentation::DomainWithAssets { assets, .. } = scope.presentation {
                registration.assets.insert(scope.key, assets());
            }
        }
        Ok(registration)
    }
    /// Attach the trusted source owner, never a service available to a preview.
    ///
    /// # Errors
    /// Returns an error if the named scope is not installed in this registration.
    pub fn with_asset_source(
        mut self,
        scope: &'static str,
        source: std::rc::Rc<dyn super::HistoricalAssetSource>,
    ) -> Result<Self, HistoryRegistrationError> {
        if !self
            .definition
            .scopes
            .iter()
            .any(|candidate| candidate.key == scope)
        {
            return Err(HistoryRegistrationError::InvalidScopes);
        }
        self.assets.insert(scope, source);
        Ok(self)
    }
    /// Whether every source is a checked file of the mounted filesystem: a
    /// root's witness of the files it names, owning no document.
    #[must_use]
    pub fn is_checked_files_only(&self) -> bool {
        self.sources
            .iter()
            .all(|source| matches!(source, HistorySourceRegistration::CheckedFiles { .. }))
    }
    /// Validate the declared scope and source correspondence.
    ///
    /// # Errors
    /// Returns an error for invalid scopes, missing presentations or foreign sources.
    pub fn validate(&self) -> Result<(), HistoryRegistrationError> {
        use std::collections::HashSet;
        let mut keys = HashSet::new();
        if self.definition.app_id.is_empty()
            || self.definition.scopes.is_empty()
            || self.definition.scopes.len() != self.sources.len()
        {
            return Err(HistoryRegistrationError::InvalidScopes);
        }
        for scope in self.definition.scopes {
            if scope.key.is_empty() || scope.title.is_empty() || !keys.insert(scope.key) {
                return Err(HistoryRegistrationError::InvalidScopes);
            }
            let mut sources = self
                .sources
                .iter()
                .filter(|source| source.scope() == scope.key);
            let source = sources
                .next()
                .ok_or(HistoryRegistrationError::InvalidScopes)?;
            if sources.next().is_some() {
                return Err(HistoryRegistrationError::InvalidScopes);
            }
            match (&scope.presentation, source) {
                (
                    HistoryPresentation::CheckedFiles { .. },
                    HistorySourceRegistration::CheckedFiles { .. },
                ) if scope.content_schema.is_none() => {}
                (
                    HistoryPresentation::Domain { .. }
                    | HistoryPresentation::DomainWithAssets { .. },
                    HistorySourceRegistration::HostDocument { resource, .. }
                    | HistorySourceRegistration::OwnedDocument { resource, .. },
                ) if resource.app_id == self.definition.app_id => {
                    if resource.profile_id.trim().is_empty()
                        || resource.workspace_id.trim().is_empty()
                    {
                        return Err(HistoryRegistrationError::InvalidNamespace);
                    }
                }
                (
                    _,
                    HistorySourceRegistration::HostDocument { resource, .. }
                    | HistorySourceRegistration::OwnedDocument { resource, .. },
                ) if resource.app_id != self.definition.app_id => {
                    return Err(HistoryRegistrationError::ForeignSource);
                }
                (
                    HistoryPresentation::External { reason },
                    HistorySourceRegistration::Unversioned {
                        reason: source_reason,
                        ..
                    },
                ) if !reason.trim().is_empty() && !source_reason.trim().is_empty() => {}
                _ => return Err(HistoryRegistrationError::MissingPresentation),
            }
        }
        for source in &self.sources {
            // A raw path identity cannot establish that the outer State owns
            // that file. Path owners use the actual checkout custody adapter.
            if matches!(source, HistorySourceRegistration::HostDocument { resource, .. } if resource.record_id.file_path().is_some())
            {
                return Err(HistoryRegistrationError::UnsupportedOwner);
            }
            if let HistorySourceRegistration::OwnedDocument {
                owner, resource, ..
            } = source
                && owner.resource(self.definition.app_id)? != *resource
            {
                return Err(HistoryRegistrationError::ForeignSource);
            }
        }
        Ok(())
    }

    pub(crate) fn validate_root(&self, root: &dyn Widget) -> Result<(), HistoryRegistrationError> {
        self.validate_root_with_owner(root, None)
    }
    pub(crate) fn validate_root_with_owner(
        &self,
        root: &dyn Widget,
        enclosing: Option<&super::HistoryOwner>,
    ) -> Result<(), HistoryRegistrationError> {
        fn collect(
            root: &dyn Widget,
            actual: &mut Vec<super::HistoryOwner>,
        ) -> Result<(), HistoryRegistrationError> {
            if root.has_separate_document_owner() {
                let owner = root
                    .history_owner()
                    .ok_or(HistoryRegistrationError::UnsupportedOwner)?;
                owner.validate()?;
                if !actual.iter().any(|other| owner.same_owner(other)) {
                    actual.push(owner);
                }
            }
            for child in root.children() {
                collect(child, actual)?;
            }
            Ok(())
        }
        for source in &self.sources {
            if let HistorySourceRegistration::CheckedFiles { scope, source } = source {
                let root_registration =
                    crate::history_mode::capability::history_registration(root)?
                        .ok_or(HistoryRegistrationError::UnsupportedOwner)?;
                if root_registration.definition.app_id != self.definition.app_id || !root_registration.sources.iter().any(|candidate| matches!(candidate,HistorySourceRegistration::CheckedFiles { scope: other_scope,source: other } if scope==other_scope && source.same_owner(other))) { return Err(HistoryRegistrationError::UnsupportedOwner); }
            }
        }
        let mut actual = Vec::new();
        // An explicitly witnessed Files-only root does not claim its enclosing
        // tab's UI state as file content. Descendant document owners still match
        // their own declared Domain sources under the normal bijection below.
        let external_only = self
            .sources
            .iter()
            .all(|source| matches!(source, HistorySourceRegistration::CheckedFiles { .. }));
        if let Some(owner) = enclosing {
            owner.validate()?;
            if !external_only {
                actual.push(owner.clone());
            }
        }
        collect(root, &mut actual)?;
        let registered: Vec<_> = self
            .sources
            .iter()
            .filter_map(|source| match source {
                HistorySourceRegistration::OwnedDocument { owner, .. } => Some(owner),
                _ => None,
            })
            .collect();
        if actual.iter().any(|owner| {
            !registered
                .iter()
                .any(|registered| owner.same_owner(registered))
        }) || registered
            .iter()
            .any(|owner| !actual.iter().any(|actual| owner.same_owner(actual)))
        {
            return Err(HistoryRegistrationError::UnsupportedOwner);
        }
        Ok(())
    }
}

#[cfg(test)]
mod asset_factory_tests {
    use super::*;
    struct EmptySource;
    impl crate::history_mode::HistoricalAssetSource for EmptySource {
        fn request(
            &self,
            _: crate::history_mode::HistoricalAssetRequest,
            r: crate::history_mode::HistoricalAssetResponse,
        ) {
            r.complete(Ok(Default::default()));
        }
    }
    static DEFINITION: HistoryDefinition = HistoryDefinition {
        app_id: "dev.tilo.asset-factory",
        scopes: &[HistoryScopeDefinition {
            key: "deck",
            title: "Deck",
            content_schema: None,
            presentation: HistoryPresentation::DomainWithAssets {
                create: crate::history_mode::test_preview,
                assets: || std::rc::Rc::new(EmptySource),
            },
        }],
    };
    #[test]
    fn installed_app_registration_automatically_attaches_declared_asset_source() {
        let registration =
            HistoryRegistration::installed_app_state(&DEFINITION, None, uuid::Uuid::new_v4())
                .unwrap();
        assert!(registration.assets.contains_key("deck"));
    }
}
