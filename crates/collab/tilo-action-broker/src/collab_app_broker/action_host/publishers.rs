//! Which connection of an app may take an action that shows something: only
//! a window of the app, never an instance with no window.

use tilo_actions::remote::{APPLICATION_OPEN_ACTION, InvokeRequest};

use super::super::{AppBrokerState, action_host::application_provider};

impl AppBrokerState {
    pub(crate) fn is_windowless(&self, peer_id: u64) -> bool {
        self.launchable
            .windowless_peers
            .lock()
            .is_ok_and(|windowless| windowless.contains(&peer_id))
    }

    /// Whether `address` in `app_id` shows something, and so is taken only
    /// by a window of the app: its `open`, and any action of the view kind
    /// it declares (an issue's page, its chat), as published in `workspace` or
    /// offered by its manifest. An instance with no window has nothing to
    /// show it in.
    pub(crate) fn action_shows(
        &self,
        app_id: &str,
        workspace: &str,
        address: &tilo_actions::ActionAddress,
    ) -> bool {
        if address.action == APPLICATION_OPEN_ACTION {
            return true;
        }
        let logical = tilo_actions::selection::logical_resource_id(&address.resource);
        self.published_surfaces()
            .into_iter()
            .chain(self.launchable.surfaces())
            .filter(|surface| surface.app_id == app_id && surface.route.cluster_id == workspace)
            .flat_map(|surface| surface.actions)
            .find(|action| {
                action.action == address.action
                    && tilo_actions::selection::logical_resource_id(&action.resource) == logical
            })
            .or_else(|| self.manifest_offer(app_id, address))
            .is_some_and(|action| action.operation_kind == tilo_actions::OperationKind::View)
    }

    /// The root of `app_id` when the session runs the app's model itself, at the resource the
    /// session publishes it at: opening such an app is opening its root
    /// (`docs/designs/app-language/one-app-model.md` §3). `None` for an app a window of starts.
    pub(super) fn served_root(&self, app_id: &str) -> Option<String> {
        self.services
            .launcher
            .as_ref()
            .and_then(|launcher| launcher.served(app_id))
    }

    /// The app `request`'s plain `open` names, when it names one: its application resource
    /// (`app.<app_id>`) under any provider, or an application provider
    /// (`application:<workspace>:<app>`). `None` for any other action, and for an `open` that
    /// names no app this way.
    pub(super) fn opened_app(request: &InvokeRequest) -> Option<String> {
        let address = &request.target.address;
        if address.action != APPLICATION_OPEN_ACTION {
            return None;
        }
        address
            .resource
            .strip_prefix(tilo_actions::remote::APPLICATION_RESOURCE_PREFIX)
            .map(str::to_owned)
            .or_else(|| {
                application_provider(&request.target.provider).map(|(_, app)| app.to_owned())
            })
    }

    /// The root `request` opens, when it is the plain `open` of an app this session serves: the
    /// app it names ([`Self::opened_app`]), when a service of this session runs the app's model.
    /// `None` for every other request, and for an app a window of starts.
    pub(crate) fn served_open_root(&self, request: &InvokeRequest) -> Option<String> {
        let app_id = Self::opened_app(request)?;
        self.served_root(&app_id)
    }
}

/// The system's `open` `request` becomes when it is the plain `open` of an app this session
/// serves: opening such an app *is* opening its root with its default view
/// (`docs/designs/app-language/one-app-model.md` §3, "Opening Tube places its root resource with
/// its default view. … `open(r)` places r with its default view, beside where it was asked, for
/// every app"). The step is the arrangement's own `open`, published by the session on its own
/// door, so the resource's own `open`, an app's `open` for a served app and the arrangement's
/// `open` are one act — never a refusal the caller has to read and retry. The asking request's
/// invocation and context are kept, so a retry reads the answer the first ask got.
pub(crate) fn opened_request(request: &InvokeRequest, root: &str) -> InvokeRequest {
    let mut arguments = serde_json::Map::new();
    arguments.insert("resource".into(), root.into());
    // The place the ask is in (the turn's conversation) is read by the session's own step from
    // the actor's turn, as it is for a step invoked there directly.
    InvokeRequest {
        invocation: request.invocation,
        target: tilo_actions::remote::RemoteActionTarget {
            provider: tilo_actions::remote::SESSION_ACTIONS_PROVIDER.to_owned(),
            address: tilo_actions::ActionAddress::new(
                tilo_actions::remote::session_actions::ARRANGEMENT_RESOURCE,
                tilo_actions::remote::session_actions::OPEN,
            ),
        },
        argument: tilo_actions::remote::InvokeArgument::Json(arguments),
        context: request.context.clone(),
        confirmation: request.confirmation.clone(),
        revision: None,
        on_behalf_of: None,
        appearance: None,
        wait_ms: None,
    }
}
