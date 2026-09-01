//! Dynamic provider backed by the resource/action surfaces live qOS apps
//! publish through the session collaboration broker.

use std::time::Duration;

use crate::shell::collab_broker::AppActionBroker;
use crate::shell::search::action_api::{ActionError, ActionParam, ParamType};
use crate::shell::search::{ResourceKind, SearchContext, SearchProvider, SearchResult};

use crate::shell::collab_broker_capnp::{PublishedAction, PublishedInput, PublishedSurface};

pub const PROVIDER_ID: &str = "app-actions";
const ID_SEPARATOR: char = '\u{1f}';

pub struct LiveAppActionsProvider {
    broker: AppActionBroker,
}

impl LiveAppActionsProvider {
    pub fn new(broker: AppActionBroker) -> Self {
        Self { broker }
    }

    pub fn capability_target(id: &str) -> Option<(&str, qos_actions::OperationKind, &str)> {
        let mut parts = id.split(ID_SEPARATOR);
        let _peer = parts.next()?.parse::<u64>().ok()?;
        let app = parts.next()?;
        let kind = match parts.next()? {
            "document" => qos_actions::OperationKind::Document,
            "effect" => qos_actions::OperationKind::Effect,
            "view" => qos_actions::OperationKind::View,
            _ => return None,
        };
        let _resource = parts.next()?;
        let action = parts.next()?;
        parts.next().is_none().then_some((app, kind, action))
    }

    fn result_id(surface: &PublishedSurface, action: &PublishedAction) -> String {
        format!(
            "{}{}{}{}{}{}{}{}{}",
            surface.peer_id,
            ID_SEPARATOR,
            surface.app_id,
            ID_SEPARATOR,
            action.operation_kind.as_str(),
            ID_SEPARATOR,
            action.resource,
            ID_SEPARATOR,
            action.action
        )
    }

    fn matches_scope(surface: &PublishedSurface, ctx: &SearchContext<'_>) -> bool {
        let active_room = ctx
            .chrome
            .tree()
            .active_workspace()
            .map(|workspace| workspace.room.to_string());
        match ctx.chrome.search_scope {
            crate::shell::chrome_state::SearchScope::All => true,
            crate::shell::chrome_state::SearchScope::Workspace => {
                active_room.as_deref() == Some(surface.route.cluster_id.as_str())
            }
            crate::shell::chrome_state::SearchScope::ActiveApp => {
                active_room.as_deref() == Some(surface.route.cluster_id.as_str())
                    && ctx
                        .chrome
                        .active_app_id()
                        .is_some_and(|app| app.eq_ignore_ascii_case(&surface.app_id))
            }
        }
    }

    fn action_params(action: &PublishedAction) -> Vec<ActionParam> {
        if !action.params.is_empty() {
            return action.params.clone();
        }
        match action.input {
            PublishedInput::None => Vec::new(),
            PublishedInput::Text {
                wants_content: true,
                ..
            } => vec![ActionParam::required(
                "content",
                ParamType::Text,
                "Content supplied to the application action",
            )],
            PublishedInput::Text { .. } => vec![ActionParam::required(
                "argument",
                ParamType::Text,
                "Text supplied to the application action",
            )],
        }
    }
}

impl SearchProvider for LiveAppActionsProvider {
    fn id(&self) -> &str {
        PROVIDER_ID
    }

    fn name(&self) -> &'static str {
        "Application actions"
    }

    fn resource_kinds(&self) -> &[ResourceKind] {
        &[ResourceKind::Action]
    }

    fn owns_tab(&self, tab_id: u64, chrome: &crate::shell::chrome_state::ChromeState) -> bool {
        chrome.active_tab_id() == Some(tab_id)
            && chrome.active_app_id().is_some_and(|active| {
                self.broker
                    .surfaces()
                    .iter()
                    .any(|surface| active.eq_ignore_ascii_case(&surface.app_id))
            })
    }

    fn query(&self, ctx: SearchContext<'_>) -> Vec<SearchResult> {
        let needle = ctx.needle.trim().to_lowercase();
        let actor = ctx.chrome.active_local_user();
        let mut results = Vec::new();
        for surface in self.broker.surfaces() {
            if !Self::matches_scope(&surface, &ctx) {
                continue;
            }
            for action in &surface.actions {
                let searchable = format!(
                    "{} {} {} {} {}",
                    action.action_label,
                    action.resource_label,
                    action.action,
                    action.resource,
                    surface.app_id
                )
                .to_lowercase();
                let score = if needle.is_empty() {
                    0
                } else if action.action_label.to_lowercase().starts_with(&needle) {
                    100
                } else if searchable.contains(&needle) {
                    50
                } else {
                    continue;
                };
                let params = Self::action_params(action);
                let broker = self.broker.clone();
                let peer_id = surface.peer_id;
                let resource = action.resource.clone();
                let action_id = action.action.clone();
                let actor = actor.clone();
                let wants_content = matches!(
                    action.input,
                    PublishedInput::Text {
                        wants_content: true,
                        ..
                    }
                );
                let typed_arguments = !action.params.is_empty();
                results.push(SearchResult {
                    kind: ResourceKind::Action,
                    provider_id: PROVIDER_ID.to_string(),
                    id: Self::result_id(&surface, action),
                    title: action.action_label.clone(),
                    subtitle: Some(action.description.clone().unwrap_or_else(|| {
                        format!("{} · {}", action.resource_label, surface.app_id)
                    })),
                    score,
                    params,
                    on_open: std::rc::Rc::new(move |_doc, _root, _chrome, args| {
                        let encoded_arguments = typed_arguments.then(|| {
                            format!(
                                "{}{}",
                                qos_actions::ARGUMENTS_JSON_PREFIX,
                                serde_json::Value::Object(args.to_json())
                            )
                        });
                        let argument = if let Some(arguments) = encoded_arguments.as_deref() {
                            Some(arguments)
                        } else if wants_content {
                            args.text("content")
                        } else {
                            args.text("argument")
                        };
                        let completion = broker
                            .invoke(peer_id, &resource, &action_id, argument, &actor)
                            .map_err(ActionError::Refused)?
                            .recv_timeout(Duration::from_secs(5))
                            .map_err(|_| {
                                ActionError::Refused(
                                    "application did not complete the action within five seconds"
                                        .into(),
                                )
                            })?;
                        if completion.code.is_empty() {
                            Ok(())
                        } else {
                            Err(ActionError::Refused(format!(
                                "{}: {}",
                                completion.code, completion.message
                            )))
                        }
                    }),
                });
            }
        }
        results
    }
}
