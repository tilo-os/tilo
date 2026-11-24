//! Opening and addressing an app's actions in a workspace: the targets a
//! provider routes to a running app or launches one for, and the launch an app
//! offers as an action on its own resources.

use crate::api::ActionAddress;

use super::{
    APPLICATION_OPEN_ACTION, APPLICATION_RESOURCE_PREFIX, InvokeArgument, RemoteActionTarget,
    workspace_provider,
};

/// The argument of an application `open` naming the content it opens.
pub const CONTENT_HASH_ARGUMENT: &str = "content_hash";
/// The argument of an application `open` naming the file its content is.
pub const NAME_ARGUMENT: &str = "name";

/// A file's name a launch may carry: one path component of at most 255 bytes, with no
/// control characters.
#[must_use]
pub fn valid_file_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && name != "."
        && name != ".."
        && !name
            .chars()
            .any(|c| c.is_control() || c == '/' || c == '\\')
}

/// The target that opens `app_id` into workspace `workspace`. An invocation may
/// carry the content hash of bytes to open read-only as its text argument.
#[must_use]
pub fn application_open_target(workspace: &str, app_id: &str) -> RemoteActionTarget {
    RemoteActionTarget {
        provider: workspace_provider(workspace),
        address: ActionAddress::new(
            format!("{APPLICATION_RESOURCE_PREFIX}{app_id}"),
            APPLICATION_OPEN_ACTION,
        ),
    }
}

/// Opening another installed app into the workspace, optionally with
/// verified content it opens read-only: Photos with a captured picture, a
/// player with a recorded clip. An app offers it as an action on its own
/// resources ([`ApplicationLaunch::spec`]); the provider routes the admitted
/// invocation through the workspace broker, which stages the bytes and owns
/// the launched process. It can instead deliver a published resource action
/// after launch ([`ApplicationLaunch::with_action`]). The source app never
/// hands over a local path or starts a process itself.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ApplicationLaunch {
    /// The installed app to open, by its id (`so.tilo.photos`).
    pub app_id: String,
    /// The content address hash of what it opens. Without content or an
    /// action destination, the app opens empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    /// An action to deliver after opening the app, rather than opening it empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<crate::ActionAddress>,
    /// The name of the file the content is, where the one handing it over knows it
    /// (`plan.txt`): bytes alone say an image is a PNG, never that text is a note or a table.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl ApplicationLaunch {
    /// The operation type the provider routes to the broker.
    pub const TYPE: &'static str = "tilo.application.launch";

    /// Open `app_id` empty.
    #[must_use]
    pub fn app(app_id: impl Into<String>) -> Self {
        Self {
            app_id: app_id.into(),
            content_hash: None,
            action: None,
            name: None,
        }
    }

    /// Open the app in the current workspace and invoke its published action.
    /// The workspace broker resolves and admits the destination contract.
    #[must_use]
    pub fn with_action(
        app_id: impl Into<String>,
        resource: impl Into<String>,
        action: impl Into<String>,
    ) -> Self {
        Self {
            app_id: app_id.into(),
            content_hash: None,
            action: Some(crate::ActionAddress::new(resource, action)),
            name: None,
        }
    }

    /// Resolve the destination only from the provider's admitted workspace route.
    #[must_use]
    pub fn target(&self, workspace: &str) -> RemoteActionTarget {
        self.action.as_ref().map_or_else(
            || application_open_target(workspace, &self.app_id),
            |action| {
                application_action_target(workspace, &self.app_id, &action.resource, &action.action)
            },
        )
    }

    /// Open `app_id` with the content whose hash is `content_hash`.
    #[must_use]
    pub fn with_content(app_id: impl Into<String>, content_hash: impl Into<String>) -> Self {
        Self {
            app_id: app_id.into(),
            content_hash: Some(content_hash.into()),
            action: None,
            name: None,
        }
    }

    /// This launch's content, as the file called `name`.
    #[must_use]
    pub fn named(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// An app id of letters, digits, dots and dashes, and a content hash
    /// (when given) of 64 lowercase hex digits.
    #[must_use]
    pub fn valid(&self) -> bool {
        self.action.as_ref().is_none_or(|action| {
            self.content_hash.is_none()
                && !action.resource.is_empty()
                && action.resource.len() <= 1024
                && !action.action.is_empty()
                && action.action.len() <= 128
        }) && !self.app_id.is_empty()
            && self.app_id.len() <= 128
            && self
                .app_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
            && self.content_hash.as_ref().is_none_or(|hash| {
                hash.len() == 64
                    && hash
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
            && self
                .name
                .as_ref()
                .is_none_or(|name| self.content_hash.is_some() && valid_file_name(name))
    }

    /// The argument the broker's application `open` takes: the content
    /// hash as its text, with the file's name the map of both, or nothing.
    #[must_use]
    pub fn argument(&self) -> InvokeArgument {
        match (&self.content_hash, &self.name) {
            (Some(hash), Some(name)) => InvokeArgument::Json(serde_json::Map::from_iter([
                (
                    CONTENT_HASH_ARGUMENT.to_owned(),
                    serde_json::Value::String(hash.clone()),
                ),
                (
                    NAME_ARGUMENT.to_owned(),
                    serde_json::Value::String(name.clone()),
                ),
            ])),
            (Some(hash), None) => InvokeArgument::Text(hash.clone()),
            (None, _) => InvokeArgument::None,
        }
    }

    /// The content hash and the file name an `open`'s `argument` carries
    /// ([`Self::argument`]): `None` for an argument that names no content.
    #[must_use]
    pub fn opened_content(argument: &InvokeArgument) -> Option<(&str, Option<&str>)> {
        match argument {
            InvokeArgument::Text(hash) => Some((hash, None)),
            InvokeArgument::Json(map) => {
                let hash = map.get(CONTENT_HASH_ARGUMENT)?.as_str()?;
                let name = map
                    .get(NAME_ARGUMENT)
                    .and_then(serde_json::Value::as_str)
                    .filter(|name| valid_file_name(name));
                Some((hash, name))
            }
            InvokeArgument::None => None,
        }
    }

    /// Opening `app_id` with content the caller names as the action's
    /// `content_hash` argument: a control that hands over whatever it
    /// shows now.
    #[must_use]
    pub fn content_spec(
        app_id: &str,
        id: &str,
        label: &str,
        description: &str,
    ) -> crate::ActionSpec {
        let app_id = app_id.to_owned();
        let spec = crate::ActionSpec::typed_operation(
            id,
            label,
            description,
            crate::OperationKind::Effect,
            Self::TYPE,
            vec![
                crate::ActionParam::required(
                    CONTENT_HASH_ARGUMENT,
                    crate::ParamType::Text,
                    "Content address hash of what the app opens",
                ),
                crate::ActionParam::optional(
                    NAME_ARGUMENT,
                    crate::ParamType::Text,
                    "The name of the file it is",
                ),
            ],
            move |args| {
                let launch = Self::with_content(
                    app_id.clone(),
                    args.text(CONTENT_HASH_ARGUMENT).unwrap_or_default(),
                );
                let launch = match args.text(NAME_ARGUMENT).filter(|name| !name.is_empty()) {
                    Some(name) => launch.named(name),
                    None => launch,
                };
                crate::EncodedAction::encode_operation(Self::TYPE, &launch).unwrap_or_else(|_| {
                    crate::EncodedAction {
                        type_id: Self::TYPE.to_owned(),
                        payload: Vec::new(),
                    }
                })
            },
        );
        Self::launched_here(spec)
    }

    /// Opening an app is what the launcher does for whoever is at this
    /// desktop: it opens a window of their own and reaches no one else, so
    /// a workspace's grants have nothing to consent to ([`crate::OWN_DEVICE_SCOPE`]).
    fn launched_here(mut spec: crate::ActionSpec) -> crate::ActionSpec {
        spec.type_mut()
            .contract
            .tags
            .push(crate::OWN_DEVICE_SCOPE.to_owned());
        spec
    }

    /// This launch as the action `id` an app offers on a resource of its
    /// own, taking no arguments.
    #[must_use]
    pub fn spec(&self, id: &str, label: &str, description: &str) -> crate::ActionSpec {
        let encoded =
            crate::EncodedAction::encode_operation(Self::TYPE, self).unwrap_or_else(|_| {
                crate::EncodedAction {
                    type_id: Self::TYPE.to_owned(),
                    payload: Vec::new(),
                }
            });
        Self::launched_here(crate::ActionSpec::typed_operation(
            id,
            label,
            description,
            crate::OperationKind::Effect,
            Self::TYPE,
            Vec::new(),
            move |_| encoded.clone(),
        ))
    }
}

/// Prefix of the provider token naming an application in a workspace
/// whether or not it is running; see [`application_action_target`].
pub const APPLICATION_PROVIDER: &str = "application";

/// The provider token naming `app_id`'s actions in `workspace`, whether or not the app runs:
/// the token a call reaches its process by ([`application_action_target`]).
#[must_use]
pub fn application_provider_token(workspace: &str, app_id: &str) -> String {
    format!("{APPLICATION_PROVIDER}:{workspace}:{app_id}")
}

/// The target of the action `app_id` offers on `resource` in workspace
/// `workspace`. The provider delivers it to the running app, or launches the app into
/// the workspace first and delivers it once the app offers the action; the
/// app's own published contract admits it either way.
#[must_use]
pub fn application_action_target(
    workspace: &str,
    app_id: &str,
    resource: impl Into<String>,
    action: impl Into<String>,
) -> RemoteActionTarget {
    RemoteActionTarget {
        provider: application_provider_token(workspace, app_id),
        address: ActionAddress::new(resource, action),
    }
}

/// [`application_action_target`] in `app_id`'s record `record`: the provider
/// delivers to the app showing that record, or launches the app on it.
/// Opening a reference is the resource's `open` on this target.
#[must_use]
pub fn application_resource_target(
    workspace: &str,
    app_id: &str,
    record: &str,
    resource: impl Into<String>,
    action: impl Into<String>,
) -> RemoteActionTarget {
    let mut target = application_action_target(workspace, app_id, resource, action);
    target.provider = format!("{}:{record}", target.provider);
    target
}

/// The workspace and application an [`application_action_target`] provider
/// token names, or `None` for any other provider.
#[must_use]
pub fn application_provider(provider: &str) -> Option<(&str, &str)> {
    application_parts(provider).map(|(workspace, app_id, _)| (workspace, app_id))
}

/// The record an [`application_resource_target`] provider token names, or
/// `None` when it names none.
#[must_use]
pub fn application_provider_resource(provider: &str) -> Option<&str> {
    application_parts(provider).and_then(|(_, _, record)| record)
}

fn application_parts(provider: &str) -> Option<(&str, &str, Option<&str>)> {
    let (workspace, rest) = provider
        .strip_prefix(APPLICATION_PROVIDER)?
        .strip_prefix(':')?
        .split_once(':')?;
    let (app_id, record) = match rest.split_once(':') {
        Some((app_id, record)) => (app_id, Some(record)),
        None => (rest, None),
    };
    (!workspace.is_empty() && !app_id.is_empty() && record.is_none_or(|record| !record.is_empty()))
        .then_some((workspace, app_id, record))
}

#[cfg(test)]
mod named_content_tests {
    use super::*;

    #[test]
    fn a_launch_carries_the_name_of_the_file_it_opens() {
        let hash = "ab".repeat(32);
        let plain = ApplicationLaunch::with_content("so.tilo.notes", hash.clone());
        assert_eq!(plain.argument(), InvokeArgument::Text(hash.clone()));
        assert_eq!(
            ApplicationLaunch::opened_content(&plain.argument()),
            Some((hash.as_str(), None))
        );
        let named = plain.named("plan.txt");
        assert!(named.valid());
        assert_eq!(
            ApplicationLaunch::opened_content(&named.argument()),
            Some((hash.as_str(), Some("plan.txt")))
        );
        // A name is one file's, never a path, and only content has one.
        for name in ["", "..", "a/b", "a\\b", "line\nbreak"] {
            assert!(
                !ApplicationLaunch::with_content("so.tilo.notes", hash.clone())
                    .named(name)
                    .valid()
            );
        }
        assert!(
            !ApplicationLaunch::app("so.tilo.notes")
                .named("plan.txt")
                .valid()
        );
    }
}
