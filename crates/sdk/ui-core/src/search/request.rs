//! Opening the platform search from inside an app.
//!
//! Apps never draw a search field of their own. A control that starts a
//! search — the toolbar's magnifier, a "Search everywhere" escalation, `/` or
//! Ctrl+K — asks the platform to open its one search surface, narrowed to
//! this app or to one of its resources:
//!
//! ```
//! use tilo_ui::search::{ScopeDecl, SearchRequest, search_button};
//!
//! // The toolbar's magnifier: search this app.
//! let search = search_button(SearchRequest::this_app());
//! // Search one mailbox, the scope pill reading "Only Inbox".
//! let inbox = search_button(SearchRequest::within(ScopeDecl::new("folder:inbox", "Inbox")));
//! # let _ = (search, inbox);
//! ```
//!
//! The request travels as an ordinary action invocation addressed to the host
//! search resource (`tilo.host:search` / `open`). A standalone window opens
//! its own palette; inside the Tilo shell the framework hands the request to
//! the shell, which opens the system palette scoped to the app's tab. The app
//! does the same thing either way.

use serde::{Deserialize, Serialize};
use tilo_actions::{
    ActionArgs, ActionBinding, ActionInput, ActionParam, ActionSpec, ControlInvocation,
    EncodedAction, OperationKind, ParamType, Resource,
};

use crate::i18n::I18nManager;

/// The operation type the host search resource's `open` action runs.
pub(crate) const SEARCH_OPERATION: &str = "tilo.host.search.view";
/// The host search resource every framework host presents.
pub(crate) const SEARCH_RESOURCE: &str = "tilo.host:search";
/// The action on [`SEARCH_RESOURCE`] that opens search.
pub(crate) const SEARCH_ACTION: &str = "open";

const QUERY_PARAM: &str = "query";
const SCOPE_PARAM: &str = "scope";
const SCOPE_LABEL_PARAM: &str = "scope_label";
const OFFER_PARAM: &str = "offer";

pub use tilo_resource::{ScopeDecl, scope_contains};

/// What a search opened from an app narrows to.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SearchRequestScope {
    /// Everything the app publishes.
    #[default]
    ThisApp,
    /// One part of the app (see [`ScopeDecl`]).
    Resource(ScopeDecl),
}

/// A request to open the platform search from inside an app.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchRequest {
    pub scope: SearchRequestScope,
    /// Text the search field starts with, selected so typing replaces it.
    pub query: Option<String>,
    /// One of the system's own actions the app prepares for the person:
    /// search opens on it with these arguments, and nothing runs until the
    /// person confirms it there. A host that offers no such action opens a
    /// plain search.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offer: Option<SearchOffer>,
}

/// A system action an app prepares for the person ([`SearchRequest::offer`]):
/// the action's id and the text arguments the app fills in. The host decides
/// which of its actions an app may prepare, and asks the person for every
/// other argument, the confirmation among them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchOffer {
    pub action: String,
    pub arguments: std::collections::BTreeMap<String, String>,
}

impl SearchRequest {
    /// Search everything this app publishes.
    #[must_use]
    pub fn this_app() -> Self {
        Self::default()
    }

    /// Search one part of this app.
    #[must_use]
    pub fn within(scope: ScopeDecl) -> Self {
        Self {
            scope: SearchRequestScope::Resource(scope),
            query: None,
            offer: None,
        }
    }

    /// Open search on the system's action `action`, with `arguments` filled
    /// in for the person to confirm (see [`SearchRequest::offer`]).
    #[must_use]
    pub fn offering<K: Into<String>, V: Into<String>>(
        action: impl Into<String>,
        arguments: impl IntoIterator<Item = (K, V)>,
    ) -> Self {
        Self {
            offer: Some(SearchOffer {
                action: action.into(),
                arguments: arguments
                    .into_iter()
                    .map(|(key, value)| (key.into(), value.into()))
                    .collect(),
            }),
            ..Self::default()
        }
    }

    /// Start the search with `query` already typed.
    #[must_use]
    pub fn with_query(mut self, query: impl Into<String>) -> Self {
        let query = query.into();
        self.query = (!query.is_empty()).then_some(query);
        self
    }

    /// The resource scope, when the request names one.
    #[must_use]
    pub fn resource_scope(&self) -> Option<&ScopeDecl> {
        match &self.scope {
            SearchRequestScope::ThisApp => None,
            SearchRequestScope::Resource(scope) => Some(scope),
        }
    }

    /// The request as the `open` action's arguments.
    #[must_use]
    pub fn arguments(&self) -> serde_json::Map<String, serde_json::Value> {
        let mut arguments = serde_json::Map::new();
        if let Some(query) = &self.query {
            arguments.insert(QUERY_PARAM.into(), query.clone().into());
        }
        if let Some(scope) = self.resource_scope() {
            arguments.insert(SCOPE_PARAM.into(), scope.anchor.clone().into());
            arguments.insert(SCOPE_LABEL_PARAM.into(), scope.label.clone().into());
        }
        if let Some(offer) = self
            .offer
            .as_ref()
            .and_then(|offer| serde_json::to_string(offer).ok())
        {
            arguments.insert(OFFER_PARAM.into(), offer.into());
        }
        arguments
    }

    /// The request the `open` action's arguments describe. Unknown or
    /// mistyped arguments read as absent.
    #[must_use]
    pub fn from_arguments(arguments: &serde_json::Map<String, serde_json::Value>) -> Self {
        let text = |name: &str| {
            arguments
                .get(name)
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        };
        Self::from_parts(
            text(QUERY_PARAM),
            text(SCOPE_PARAM),
            text(SCOPE_LABEL_PARAM),
        )
        .with_offer(text(OFFER_PARAM).as_deref())
    }

    /// The offer the `offer` argument carries, as its JSON text; a malformed
    /// one reads as none.
    fn with_offer(mut self, offer: Option<&str>) -> Self {
        self.offer = offer.and_then(|offer| serde_json::from_str(offer).ok());
        self
    }

    fn from_args(args: &ActionArgs) -> Self {
        let text = |name: &str| {
            args.text(name)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        };
        Self::from_parts(
            text(QUERY_PARAM),
            text(SCOPE_PARAM),
            text(SCOPE_LABEL_PARAM),
        )
        .with_offer(text(OFFER_PARAM).as_deref())
    }

    fn from_parts(query: Option<String>, anchor: Option<String>, label: Option<String>) -> Self {
        let scope = anchor.map_or(SearchRequestScope::ThisApp, |anchor| {
            let label = label.unwrap_or_else(|| anchor.clone());
            SearchRequestScope::Resource(ScopeDecl { anchor, label })
        });
        Self {
            scope,
            query,
            offer: None,
        }
    }

    /// Decode the `open` operation's payload. An empty or unit payload is a
    /// plain search of the app.
    pub(crate) fn decode(payload: &[u8]) -> Result<Self, serde_json::Error> {
        match serde_json::from_slice::<Option<Self>>(payload) {
            Ok(request) => Ok(request.unwrap_or_default()),
            Err(_) if payload.is_empty() => Ok(Self::default()),
            Err(error) => Err(error),
        }
    }

    fn encode(&self) -> EncodedAction {
        EncodedAction::encode_operation(SEARCH_OPERATION, self).unwrap_or_else(|_| EncodedAction {
            type_id: SEARCH_OPERATION.into(),
            payload: b"null".to_vec(),
        })
    }

    /// The request a control invocation carries, when it is addressed to the
    /// host search resource.
    pub(crate) fn from_invocation(invocation: &ControlInvocation) -> Option<Self> {
        is_search_address(&invocation.address).then(|| Self::from_arguments(&invocation.arguments))
    }
}

/// Whether `address` names the host search door.
pub(crate) fn is_search_address(address: &tilo_actions::ActionAddress) -> bool {
    address.resource == SEARCH_RESOURCE && address.action == SEARCH_ACTION
}

/// The host search door's address. Every host answers it — a window with its
/// own palette, or the shell — so a control bound to it is always available.
pub(crate) fn search_address() -> tilo_actions::ActionAddress {
    tilo_actions::ActionAddress::new(SEARCH_RESOURCE, SEARCH_ACTION)
}

/// The host search resource: one `open` action that opens search, plainly from
/// a menu or the keyboard, or narrowed and pre-filled by its optional
/// arguments.
pub(crate) fn search_resource() -> Resource {
    let label = I18nManager::lookup_plain("standalone-search-title");
    let mut open = ActionSpec::typed_operation(
        SEARCH_ACTION,
        label.clone(),
        label.clone(),
        OperationKind::View,
        SEARCH_OPERATION,
        vec![
            ActionParam::optional(QUERY_PARAM, ParamType::Text, "Text to search for"),
            ActionParam::optional(
                SCOPE_PARAM,
                ParamType::Text,
                "Resource id of the part of the app to search",
            ),
            ActionParam::optional(
                SCOPE_LABEL_PARAM,
                ParamType::Text,
                "Name of that part, shown on the scope pill",
            ),
            ActionParam::optional(
                OFFER_PARAM,
                ParamType::Text,
                "A system action the app prepares for the person to confirm, as JSON: {\"action\": id, \"arguments\": {name: text}}",
            ),
        ],
        |args| SearchRequest::from_args(args).encode(),
    );
    // Opening search asks nothing first: a menu item, the shortcut and the
    // palette run it at once; a control or an agent passes the arguments.
    open.input = ActionInput::Immediate(SearchRequest::default().encode().into());
    Resource::new(SEARCH_RESOURCE, "tilo.host.search", label).with_action(open)
}

/// Bind a control to open the platform search with `request`
/// ([`search_button`] is the launcher built on it).
///
/// # Panics
///
/// Never: a request's arguments are exactly the search action's optional
/// text parameters, so binding them cannot fail.
#[must_use]
#[expect(
    clippy::expect_used,
    reason = "the request's arguments are exactly the host search action's optional text parameters"
)]
pub fn open_search_binding(request: &SearchRequest) -> ActionBinding<()> {
    ActionBinding::static_json_for(&search_resource(), SEARCH_ACTION, request.arguments())
        .expect("search request arguments match the host search action")
}

/// The search launcher: an icon-only magnifier button that opens the
/// platform search with `request`. This is the only search affordance an app
/// puts in its chrome; it never draws a field.
#[must_use]
pub fn search_button(request: SearchRequest) -> crate::widget::Button {
    let label = I18nManager::lookup_plain("standalone-search-title");
    let id = match request.resource_scope() {
        Some(scope) => format!("search-open-{}", scope.anchor),
        None => "search-open".to_owned(),
    };
    let name = match request.resource_scope() {
        Some(scope) => format!("{label} {}", scope.label),
        None => label,
    };
    crate::widget::Button::icon_only(&id, crate::icons::Icon::Search, &name)
        .on_action(open_search_binding(&request))
}

/// The invocation a painted widget queues (through
/// [`ActionControls`](crate::widget::controls::ActionControls)) to open the
/// platform search with `request`; [`SearchOpener`] keeps one for it.
#[must_use]
pub fn open_search_invocation(request: &SearchRequest) -> ControlInvocation {
    ControlInvocation {
        address: search_address(),
        arguments: request.arguments(),
    }
}

/// A latch a painted widget keeps to open search from its own input handling:
/// call [`open`](Self::open) from the handler and return
/// [`take`](Self::take) from `ActionControls::take_control_invocation`.
#[derive(Debug, Default)]
pub struct SearchOpener {
    pending: Option<SearchRequest>,
}

impl SearchOpener {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Ask the platform to open search with `request`.
    pub fn open(&mut self, request: SearchRequest) {
        self.pending = Some(request);
    }

    /// The queued request as an invocation, once.
    pub fn take(&mut self) -> Option<ControlInvocation> {
        self.pending
            .take()
            .map(|request| open_search_invocation(&request))
    }

    #[must_use]
    pub fn is_pending(&self) -> bool {
        self.pending.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_offered_action_round_trips_and_a_malformed_offer_is_none() {
        let request = SearchRequest::offering(
            "bot.add",
            [("entry", "so.tilo.bots.standup"), ("version", "1.2.0")],
        );
        assert_eq!(SearchRequest::from_arguments(&request.arguments()), request);
        assert_eq!(
            SearchRequest::decode(&request.encode().payload).unwrap(),
            request
        );
        let invocation = open_search_binding(&request).invoke(()).unwrap();
        let carried = SearchRequest::from_invocation(&invocation).unwrap();
        let offer = carried.offer.expect("the offer travels with the request");
        assert_eq!(offer.action, "bot.add");
        assert_eq!(offer.arguments["entry"], "so.tilo.bots.standup");
        assert_eq!(offer.arguments["version"], "1.2.0");

        let mut arguments = SearchRequest::this_app().arguments();
        arguments.insert(OFFER_PARAM.into(), "{not json".into());
        assert_eq!(
            SearchRequest::from_arguments(&arguments),
            SearchRequest::this_app()
        );
    }

    #[test]
    fn a_request_round_trips_through_arguments_payload_and_binding() {
        let request =
            SearchRequest::within(ScopeDecl::new("folder:inbox", "Inbox")).with_query("invoice");
        assert_eq!(SearchRequest::from_arguments(&request.arguments()), request);
        assert_eq!(
            SearchRequest::decode(&request.encode().payload).unwrap(),
            request
        );
        assert_eq!(
            SearchRequest::decode(b"null").unwrap(),
            SearchRequest::this_app()
        );
        assert_eq!(
            SearchRequest::decode(b"").unwrap(),
            SearchRequest::this_app()
        );

        let invocation = open_search_binding(&request).invoke(()).unwrap();
        assert_eq!(
            SearchRequest::from_invocation(&invocation),
            Some(request.clone())
        );
        assert_eq!(open_search_invocation(&request), invocation);

        // The door builds the same request from the arguments.
        let resource = search_resource();
        let open = resource.actions.first().unwrap();
        let built = open.build_json(&request.arguments()).unwrap();
        assert_eq!(SearchRequest::decode(&built.payload).unwrap(), request);
        // A menu or the shortcut opens it without asking anything.
        assert!(matches!(open.input, ActionInput::Immediate(_)));
        assert!(ActionBinding::unit_for(&resource, SEARCH_ACTION).is_ok());
    }

    #[test]
    fn scopes_contain_their_own_path_only() {
        assert!(scope_contains("a", "a/b"));
        let inbox = ScopeDecl::new("folder:inbox", "Inbox");
        assert!(inbox.contains("folder:inbox"));
        assert!(inbox.contains("folder:inbox/msg:12"));
        assert!(!inbox.contains("folder:inboxes"));
        assert!(!inbox.contains("folder:sent/msg:12"));
    }

    #[test]
    fn the_opener_latches_one_request() {
        let mut opener = SearchOpener::new();
        assert!(opener.take().is_none());
        opener.open(SearchRequest::this_app().with_query("x"));
        assert!(opener.is_pending());
        let invocation = opener.take().unwrap();
        assert_eq!(
            SearchRequest::from_invocation(&invocation).and_then(|r| r.query),
            Some("x".into())
        );
        assert!(opener.take().is_none());
    }
}
