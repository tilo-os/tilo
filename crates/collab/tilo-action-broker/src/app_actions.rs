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

use tilo_action_policy::Lifetime;
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
    /// The resource of `target_app` that publishes the action, as a grant
    /// scopes it.
    pub resource: String,
    /// The action's id on `resource`, as a grant scopes it.
    pub action: String,
    /// The resource as its publisher labels it.
    pub resource_label: String,
    /// The action as its publisher labels it.
    pub action_label: String,
    pub risk: tilo_actions::RiskLevel,
    /// Whether each use still asks for confirmation, whatever is granted.
    pub needs_confirmation: bool,
    /// The person confirms this one invocation, which `app_id` (an agent or
    /// an application) makes for them, rather than granting anything.
    pub confirming: bool,
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
    pub fn grant_lifetime(self) -> Option<Lifetime> {
        match self {
            Self::Once => Some(Lifetime::Once),
            Self::Session => Some(Lifetime::Session(
                tilo_desktop_model::session_id().to_owned(),
            )),
            Self::Always => Some(Lifetime::UntilRevoked),
            Self::Deny => None,
        }
    }

    /// How long the grant lives, as the workspace's history records it:
    /// `once`, `session` or `workspace`.
    #[must_use]
    pub const fn lifetime(self) -> &'static str {
        match self {
            Self::Once => "once",
            Self::Session => "session",
            Self::Always | Self::Deny => "workspace",
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
        assert_eq!(
            AppActionConsent::Once.grant_lifetime(),
            Some(Lifetime::Once)
        );
        assert_eq!(
            AppActionConsent::Session.grant_lifetime(),
            Some(Lifetime::Session(
                tilo_desktop_model::session_id().to_owned()
            ))
        );
        assert_eq!(
            AppActionConsent::Always.grant_lifetime(),
            Some(Lifetime::UntilRevoked)
        );
        assert_eq!(AppActionConsent::Deny.grant_lifetime(), None);
        assert_eq!(AppActionConsent::ALLOWING[0], AppActionConsent::Once);
    }
}
