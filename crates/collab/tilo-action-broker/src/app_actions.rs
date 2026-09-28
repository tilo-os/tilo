//! Applications on the action door under their own identity.
//!
//! An application declares in its signed manifest what it may do on the
//! door beyond its own actions ([`AppActionAccess`]): read the catalogue,
//! invoke other apps' actions. The declaration is a ceiling, never a grant:
//! invoking another app's action still needs the workspace to grant it to
//! the app, exactly as it grants a collaborator.
//!
//! When an app reaches for an action it holds no grant for, the person who
//! admitted its connection is asked first — on a sheet the host draws
//! ([`AppActionConsentPrompter`]) — instead of refusing silently. The
//! answer becomes an ordinary capability in the workspace's records,
//! granted by that person and recorded by the host
//! ([`AppActionGrants`](crate::services::AppActionGrants)). Nobody present
//! means nobody to ask: the request is refused without a sheet.

use std::sync::Arc;

use collab_types::caps::Expiry;
use tokio::sync::oneshot;

/// What an application declared it may do on the door under its own
/// identity.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AppActionAccess {
    /// Read the catalogue of actions the person using the app is offered.
    pub query: bool,
    /// Invoke actions other apps publish, as this app.
    pub invoke: bool,
}

/// Where declared access is read from, by package id.
pub type AppActionAccessResolver = Arc<dyn Fn(&str) -> AppActionAccess + Send + Sync>;

/// A resolver under which no application declared anything.
#[must_use]
pub fn nothing_declared() -> AppActionAccessResolver {
    Arc::new(|_| AppActionAccess::default())
}

/// What the person is asked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppActionConsentRequest {
    /// Names this one question; the sheet's handle.
    pub handle: String,
    /// The workspace the grant would be recorded in.
    pub room: String,
    /// Who admitted the asking application's connection, and would grant.
    pub person: String,
    /// The application asking.
    pub app_id: String,
    /// The application whose action it wants to use.
    pub target_app: String,
    /// The action's id within `target_app`, as a grant scopes it.
    pub action: String,
    /// The action as its publisher labels it.
    pub action_label: String,
    pub risk: tilo_actions::RiskLevel,
    /// Whether each use still asks for confirmation, whatever is granted.
    pub needs_confirmation: bool,
}

/// The person's answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppActionConsent {
    /// This one call.
    Once,
    /// Until this session ends.
    Session,
    /// In this workspace until revoked.
    Always,
    Deny,
}

impl AppActionConsent {
    /// The allowing answers, in the order the sheet offers them: the
    /// narrowest first, and selected by default.
    pub const ALLOWING: [Self; 3] = [Self::Once, Self::Session, Self::Always];

    /// How long the grant this answer records lives; `None` records
    /// nothing.
    #[must_use]
    pub const fn expiry(self) -> Option<Expiry> {
        match self {
            Self::Once => Some(Expiry::AfterUse),
            Self::Session => Some(Expiry::EndOfSession),
            Self::Always => Some(Expiry::Never),
            Self::Deny => None,
        }
    }

    /// How long the grant lives, in words for the workspace's history.
    #[must_use]
    pub const fn lifetime(self) -> &'static str {
        match self {
            Self::Once => "once",
            Self::Session => "for this session",
            Self::Always | Self::Deny => "in this workspace",
        }
    }
}

/// Whoever can answer for the person using an application.
pub trait AppActionConsentPrompter: Send + Sync {
    /// Put `request` to the person. `None` when nobody is there to answer,
    /// which refuses the request without asking.
    fn ask(&self, request: AppActionConsentRequest) -> Option<oneshot::Receiver<AppActionConsent>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_answer_records_the_matching_lifetime() {
        assert_eq!(AppActionConsent::Once.expiry(), Some(Expiry::AfterUse));
        assert_eq!(
            AppActionConsent::Session.expiry(),
            Some(Expiry::EndOfSession)
        );
        assert_eq!(AppActionConsent::Always.expiry(), Some(Expiry::Never));
        assert_eq!(AppActionConsent::Deny.expiry(), None);
        assert_eq!(AppActionConsent::ALLOWING[0], AppActionConsent::Once);
    }
}
