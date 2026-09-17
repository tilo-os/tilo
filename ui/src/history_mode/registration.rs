//! Explicit ownership and presentation registration for managed roots.
use super::{HistoryResource, preview::PreviewFactory, restore::HistoryRestoreSchema};
use crate::widget::Widget;

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
    Domain { create: PreviewFactory },
    External { reason: &'static str },
}

/// Immutable identity supplied by the owning runner. This carries no document.
/// Bound runners resolve their actual checkout instead of accepting a foreign identity.
pub enum HistorySourceRegistration {
    HostDocument {
        scope: &'static str,
        resource: HistoryResource,
    },
    Unversioned {
        scope: &'static str,
        reason: &'static str,
    },
}
impl HistorySourceRegistration {
    pub(crate) fn scope(&self) -> &'static str {
        match self {
            Self::HostDocument { scope, .. } | Self::Unversioned { scope, .. } => scope,
        }
    }
}
pub struct HistoryRegistration {
    pub(crate) definition: &'static HistoryDefinition,
    pub(crate) sources: Vec<HistorySourceRegistration>,
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
    pub fn new(
        definition: &'static HistoryDefinition,
        sources: Vec<HistorySourceRegistration>,
    ) -> Result<Self, HistoryRegistrationError> {
        let registration = Self {
            definition,
            sources,
        };
        registration.validate()?;
        Ok(registration)
    }
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
                    HistoryPresentation::Domain { .. },
                    HistorySourceRegistration::HostDocument { resource, .. },
                ) if resource.app_id == self.definition.app_id => {
                    if resource.profile_id.trim().is_empty()
                        || resource.workspace_id.trim().is_empty()
                    {
                        return Err(HistoryRegistrationError::InvalidNamespace);
                    }
                }
                (_, HistorySourceRegistration::HostDocument { resource, .. })
                    if resource.app_id != self.definition.app_id =>
                {
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
        Ok(())
    }
}

/// This checkpoint supports the runner-owned document only. Reject other
/// owners before mounting, rather than presenting unrelated State history.
pub(crate) fn validate_host_document_root(
    root: &dyn Widget,
) -> Result<(), HistoryRegistrationError> {
    if root.has_separate_document_owner() {
        return Err(HistoryRegistrationError::UnsupportedOwner);
    }
    for child in root.children() {
        validate_host_document_root(child)?;
    }
    Ok(())
}
