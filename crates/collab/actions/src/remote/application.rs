//! Opening and addressing an app's actions in a workspace: the targets a
//! host routes to a running app or launches one for, and the launch an app
//! offers as an action on its own resources.

use crate::door::ActionAddress;

use super::{
    APPLICATION_OPEN_ACTION, APPLICATION_RESOURCE_PREFIX, InvokeArgument, RemoteActionTarget,
    workspace_provider,
};

/// The target that opens `app_id` into workspace `room`. An invocation may
/// carry the content hash of bytes to open read-only as its text argument.
#[must_use]
pub fn application_open_target(room: &str, app_id: &str) -> RemoteActionTarget {
    RemoteActionTarget {
        provider: workspace_provider(room),
        address: ActionAddress::new(
            format!("{APPLICATION_RESOURCE_PREFIX}{app_id}"),
            APPLICATION_OPEN_ACTION,
        ),
    }
}

/// Opening another installed app into the workspace, optionally with
/// verified content it opens read-only: Photos with a captured picture, a
/// player with a recorded clip. An app offers it as an action on its own
/// resources ([`ApplicationLaunch::spec`]); the host routes the admitted
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
}

impl ApplicationLaunch {
    /// The operation type the host routes to the broker.
    pub const TYPE: &'static str = "tilo.application.launch";

    /// Open `app_id` empty.
    #[must_use]
    pub fn app(app_id: impl Into<String>) -> Self {
        Self {
            app_id: app_id.into(),
            content_hash: None,
            action: None,
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
        }
    }

    /// Resolve the destination only from the host's admitted workspace route.
    #[must_use]
    pub fn target(&self, room: &str) -> RemoteActionTarget {
        self.action.as_ref().map_or_else(
            || application_open_target(room, &self.app_id),
            |action| {
                application_action_target(room, &self.app_id, &action.resource, &action.action)
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
        }
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
    }

    /// The argument the broker's application `open` takes: the content
    /// hash as its text, or nothing.
    #[must_use]
    pub fn argument(&self) -> InvokeArgument {
        self.content_hash
            .clone()
            .map_or(InvokeArgument::None, InvokeArgument::Text)
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
            vec![crate::ActionParam::required(
                "content_hash",
                crate::ParamType::Text,
                "Content address hash of what the app opens",
            )],
            move |args| {
                let launch = Self::with_content(
                    app_id.clone(),
                    args.text("content_hash").unwrap_or_default(),
                );
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
    /// a room's grants have nothing to consent to ([`crate::OWN_DEVICE_SCOPE`]).
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

/// The target of the action `app_id` offers on `resource` in workspace
/// `room`. The host delivers it to the running app, or launches the app into
/// the workspace first and delivers it once the app offers the action; the
/// app's own published contract admits it either way.
#[must_use]
pub fn application_action_target(
    room: &str,
    app_id: &str,
    resource: impl Into<String>,
    action: impl Into<String>,
) -> RemoteActionTarget {
    RemoteActionTarget {
        provider: format!("{APPLICATION_PROVIDER}:{room}:{app_id}"),
        address: ActionAddress::new(resource, action),
    }
}

/// [`application_action_target`] in `app_id`'s record `record`: the host
/// delivers to the app showing that record, or launches the app on it.
/// Opening a reference is the resource's `open` on this target.
#[must_use]
pub fn application_record_target(
    room: &str,
    app_id: &str,
    record: &str,
    resource: impl Into<String>,
    action: impl Into<String>,
) -> RemoteActionTarget {
    let mut target = application_action_target(room, app_id, resource, action);
    target.provider = format!("{}:{record}", target.provider);
    target
}

/// The room and application an [`application_action_target`] provider
/// token names, or `None` for any other provider.
#[must_use]
pub fn application_provider(provider: &str) -> Option<(&str, &str)> {
    application_parts(provider).map(|(room, app_id, _)| (room, app_id))
}

/// The record an [`application_record_target`] provider token names, or
/// `None` when it names none.
#[must_use]
pub fn application_provider_record(provider: &str) -> Option<&str> {
    application_parts(provider).and_then(|(_, _, record)| record)
}

fn application_parts(provider: &str) -> Option<(&str, &str, Option<&str>)> {
    let (room, rest) = provider
        .strip_prefix(APPLICATION_PROVIDER)?
        .strip_prefix(':')?
        .split_once(':')?;
    let (app_id, record) = match rest.split_once(':') {
        Some((app_id, record)) => (app_id, Some(record)),
        None => (rest, None),
    };
    (!room.is_empty() && !app_id.is_empty() && record.is_none_or(|record| !record.is_empty()))
        .then_some((room, app_id, record))
}
