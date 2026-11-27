//! The actions an app's window publishes and the opening of an app into a workspace, declared
//! once: their ids, operations and arguments, written beside the crate as
//! `app-view-surface.json` for a crate that does not depend on this one to bind.

use serde::{Deserialize, Serialize};
use tilo_actions::remote::InvokeArgument;
use tilo_resource::Effect as _;

/// The app a window shows, as the row the window's actions are offered on.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(deny_unknown_fields)]
pub struct App {}

/// Bringing a window forward.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "tilo.app.activate-window")]
pub enum WindowAction {
    /// Bring this app's window to the front.
    ///
    /// Answers [`ActivationRequested`] once the window was asked to come
    /// forward. An agent brings the window forward; a person looking for it in
    /// the window's own palette already has it.
    #[effect(view, audience = "agents", returns = ActivationRequested, label = "app-activate-window")]
    ActivateWindow,
}

/// What [`WindowAction::ActivateWindow`] answers once the window was asked to
/// come forward.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ActivationRequested {
    pub activation_requested: bool,
}

/// Showing one item in the app's window, where a search hit names it.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "tilo.app.open")]
pub enum AppOpen {
    /// Open the app at one item: the anchor of a search hit.
    #[effect(view, label = "app-open", tag = tilo_actions::OPENS_HIT_TAG)]
    Open {
        #[param("Item to open: the anchor of a search hit")]
        anchor: String,
    },
}

/// Showing what a link names, in the app that claims its scheme.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "tilo.app.open-link")]
pub enum AppLink {
    /// Show what a link names, in the app that claims its scheme.
    #[effect(view, label = "app-open-link")]
    OpenLink {
        /// The link (`geo:52.5,13.4`, `settings://keyboard`).
        #[param("The link to show, of a scheme the app claims (geo:52.5,13.4)")]
        link: String,
    },
}

/// Reading what an app's window shows, as the lines an agent reads.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "tilo.app.outline")]
pub enum AppOutline {
    /// Read what the app's window shows as lines: every label, value and control in reading
    /// order, and the action each control runs, which is called by the address the line
    /// names. With `picture`, also the view as a picture; with an `anchor`, shows that item
    /// first.
    #[effect(view, audience = "agents", label = "app-outline", returns)]
    Outline {
        #[param(
            "An item of the app's to show first: the anchor of a search hit, or an item \
             resource's id"
        )]
        anchor: Option<String>,
        #[param("Also answer the view as a picture of it, where its window can draw one")]
        picture: Option<bool>,
        #[param("Pixels per point of the picture, 0.5 to 2 (default 1)")]
        scale: Option<f64>,
    },
}

impl AppOutline {
    /// The arguments an `outline` invocation travels with, for a caller building one.
    #[must_use]
    pub fn outline_arguments(
        anchor: Option<&str>,
        picture: Option<bool>,
        scale: Option<f64>,
    ) -> serde_json::Map<String, serde_json::Value> {
        Self::Outline {
            anchor: anchor.map(str::to_owned),
            picture,
            scale,
        }
        .arguments()
    }
}

/// The workspace's `open` of an installed app: what it opens, and the name of the file its
/// content is.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "tilo.application.launch")]
pub enum ApplicationOpen {
    /// Open the app
    #[effect(kind = "undoable", label = "app-open")]
    Open {
        /// The content address hash of what it opens; without content or an action
        /// destination, the app opens empty.
        #[param("Content address hash of what the app opens")]
        content_hash: Option<String>,
        /// The name of the file the content is, where the one handing it over knows it
        /// (`plan.txt`).
        #[param("The name of the file it is")]
        name: Option<String>,
    },
}

/// Each action's id and the operation it travels under, as its declaration names them.
pub const ACTIVATE_WINDOW: &str = WindowAction::ACTIVATE_WINDOW.id();
pub const OPEN: &str = AppOpen::OPEN.id();
pub const OPEN_LINK: &str = AppLink::OPEN_LINK.id();
pub const OUTLINE: &str = AppOutline::OUTLINE.id();
pub const OPEN_APP: &str = ApplicationOpen::OPEN.id();
pub const ACTIVATE_OPERATION: &str = <WindowAction as tilo_resource::Effect>::OPERATION;
pub const OPEN_OPERATION: &str = <AppOpen as tilo_resource::Effect>::OPERATION;
pub const OPEN_LINK_OPERATION: &str = <AppLink as tilo_resource::Effect>::OPERATION;
pub const OUTLINE_OPERATION: &str = <AppOutline as tilo_resource::Effect>::OPERATION;
pub const OPEN_APP_OPERATION: &str = <ApplicationOpen as tilo_resource::Effect>::OPERATION;

/// The `outline` parameter naming an item to show before reading; the `render` operation
/// names the same anchor.
pub const OUTLINE_ANCHOR: &str = "anchor";
/// The `outline` parameter asking for the view as a picture.
pub const OUTLINE_PICTURE: &str = "picture";

/// The key of an [`AppOutline::Outline`] answer saying why it has no picture where one was
/// asked for.
pub const OUTLINE_PICTURE_UNAVAILABLE: &str = "picture_unavailable";

/// The spec each window action is published as. The declaration is the error, so these cannot
/// fail; a rename that broke them fails the build.
#[allow(
    clippy::expect_used,
    reason = "the vocabulary declares every id it publishes"
)]
fn spec_of<E: tilo_resource::Effect>(id: &str) -> tilo_actions::ActionSpec {
    E::spec(id).expect("the vocabulary declares the id it is asked for")
}

#[must_use]
pub fn activate_spec() -> tilo_actions::ActionSpec {
    spec_of::<WindowAction>(ACTIVATE_WINDOW)
}

#[must_use]
pub fn open_spec() -> tilo_actions::ActionSpec {
    spec_of::<AppOpen>(OPEN)
}

#[must_use]
pub fn open_link_spec() -> tilo_actions::ActionSpec {
    spec_of::<AppLink>(OPEN_LINK)
}

#[must_use]
pub fn outline_spec() -> tilo_actions::ActionSpec {
    spec_of::<AppOutline>(OUTLINE)
}

/// What an `open` invocation's [`InvokeArgument`] names: the content hash it
/// opens, and the file's name where it gives one. `None` for an argument that
/// names no content, or is not one the `open` action takes. The declaration
/// checks the map; its parameter names then read the values the caller borrowed.
#[must_use]
pub fn opened_content(argument: &InvokeArgument) -> Option<(&str, Option<&str>)> {
    match argument {
        InvokeArgument::Text(hash) => Some((hash, None)),
        InvokeArgument::Json(arguments) => {
            let ApplicationOpen::Open { .. } = ApplicationOpen::decode(OPEN_APP, arguments).ok()?;
            let params = ApplicationOpen::OPEN.params();
            let hash = arguments.get(*params.first()?)?.as_str()?;
            let name = params
                .get(1)
                .and_then(|name| arguments.get(*name))
                .and_then(serde_json::Value::as_str)
                .filter(|name| tilo_actions::remote::valid_file_name(name));
            Some((hash, name))
        }
        InvokeArgument::None => None,
    }
}

/// Opening another installed app into the workspace, optionally with
/// verified content it opens read-only: Photos with a captured picture, a
/// player with a recorded clip. An app offers it as an action on its own
/// resources ([`ApplicationLaunch::spec`]); the provider routes the admitted
/// invocation through the workspace's app host, which stages the bytes and owns
/// the launched process. It can instead deliver a published resource action
/// after launch ([`ApplicationLaunch::with_action`]). The source app never
/// hands over a local path or starts a process itself.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplicationLaunch {
    /// The installed app to open, by its id (`so.tilo.photos`).
    pub app_id: String,
    /// The content address hash of what it opens. Without content or an
    /// action destination, the app opens empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    /// An action to deliver after opening the app, rather than opening it empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<tilo_actions::ActionAddress>,
    /// The name of the file the content is, where the one handing it over knows it
    /// (`plan.txt`): bytes alone say an image is a PNG, never that text is a note or a table.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl ApplicationLaunch {
    /// The operation type the provider routes to the app host.
    pub const TYPE: &'static str = OPEN_APP_OPERATION;

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
    /// The app host resolves and admits the destination contract.
    #[must_use]
    pub fn with_action(
        app_id: impl Into<String>,
        resource: impl Into<String>,
        action: impl Into<String>,
    ) -> Self {
        Self {
            app_id: app_id.into(),
            content_hash: None,
            action: Some(tilo_actions::ActionAddress::new(resource, action)),
            name: None,
        }
    }

    /// Resolve the destination only from the provider's admitted workspace route.
    #[must_use]
    pub fn target(&self, workspace: &str) -> tilo_actions::remote::RemoteActionTarget {
        self.action.as_ref().map_or_else(
            || tilo_actions::remote::application_open_target(workspace, &self.app_id),
            |action| {
                tilo_actions::remote::application_action_target(
                    workspace,
                    &self.app_id,
                    &action.resource,
                    &action.action,
                )
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
            && self.name.as_ref().is_none_or(|name| {
                self.content_hash.is_some() && tilo_actions::remote::valid_file_name(name)
            })
    }

    /// The argument the app host's application `open` takes: the content hash and
    /// the file's name, or nothing.
    #[must_use]
    pub fn argument(&self) -> InvokeArgument {
        match &self.content_hash {
            Some(hash) => InvokeArgument::Json(
                ApplicationOpen::Open {
                    content_hash: Some(hash.clone()),
                    name: self.name.clone(),
                }
                .arguments(),
            ),
            None => InvokeArgument::None,
        }
    }

    /// This launch as the payload of the action it is offered by.
    fn encoded(&self) -> tilo_actions::EncodedAction {
        tilo_actions::EncodedAction::encode_operation(Self::TYPE, self).unwrap_or_else(|_| {
            tilo_actions::EncodedAction {
                type_id: Self::TYPE.to_owned(),
                payload: Vec::new(),
            }
        })
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
    ) -> tilo_actions::ActionSpec {
        let app_id = app_id.to_owned();
        let params =
            ApplicationOpen::declaration(OPEN_APP).map_or_else(Vec::new, |decl| decl.params);
        let spec = tilo_actions::ActionSpec::typed_operation(
            id,
            label,
            description,
            tilo_actions::OperationKind::Effect,
            Self::TYPE,
            params,
            move |args| {
                let launch = match ApplicationOpen::decode(OPEN_APP, &args.to_json()) {
                    Ok(ApplicationOpen::Open { content_hash, name }) => {
                        let launch =
                            Self::with_content(app_id.clone(), content_hash.unwrap_or_default());
                        match name.filter(|name| !name.is_empty()) {
                            Some(name) => launch.named(name),
                            None => launch,
                        }
                    }
                    Err(_) => Self::app(app_id.clone()),
                };
                launch.encoded()
            },
        );
        Self::launched_here(spec)
    }

    /// Opening an app is what the launcher does for whoever is at this
    /// desktop: it opens a window of their own and reaches no one else, so
    /// a workspace's grants have nothing to consent to ([`tilo_actions::OWN_DEVICE_SCOPE`]).
    fn launched_here(mut spec: tilo_actions::ActionSpec) -> tilo_actions::ActionSpec {
        spec.type_mut()
            .contract
            .tags
            .push(tilo_actions::OWN_DEVICE_SCOPE.to_owned());
        spec
    }

    /// This launch as the action `id` an app offers on a resource of its
    /// own, taking no arguments.
    #[must_use]
    pub fn spec(&self, id: &str, label: &str, description: &str) -> tilo_actions::ActionSpec {
        let encoded = self.encoded();
        Self::launched_here(tilo_actions::ActionSpec::typed_operation(
            id,
            label,
            description,
            tilo_actions::OperationKind::Effect,
            Self::TYPE,
            Vec::new(),
            move |_| encoded.clone(),
        ))
    }
}

/// The actions an app's window publishes, as a view of another language reads
/// them: `activate-window`, `open`, `open-link` and `outline` on an app's row.
#[must_use]
pub fn surface() -> tilo_actions::surface::Surface {
    let mut surface = tilo_resource::surface::reading_surface::<App, WindowAction>(
        tilo_actions::remote::APPLICATION_RESOURCE_KIND,
        "Application",
    );
    for mut other in [
        tilo_resource::surface::reading_surface::<App, AppOpen>(
            tilo_actions::remote::APPLICATION_RESOURCE_KIND,
            "Application",
        ),
        tilo_resource::surface::reading_surface::<App, AppLink>(
            tilo_actions::remote::APPLICATION_RESOURCE_KIND,
            "Application",
        ),
        tilo_resource::surface::reading_surface::<App, AppOutline>(
            tilo_actions::remote::APPLICATION_RESOURCE_KIND,
            "Application",
        ),
    ] {
        surface.verbs.append(&mut other.verbs);
        surface.types.append(&mut other.types);
    }
    surface
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::panic,
        reason = "in a test these are the assertion, not a hazard"
    )]

    use super::*;

    /// Camera's "Open Photos" opens a window of the person's own, as the launcher does: a
    /// workspace's grants have nothing to consent to, so a guest is not asked first.
    #[test]
    fn opening_an_app_needs_no_workspace_consent() {
        for spec in [
            ApplicationLaunch::app("so.tilo.photos").spec("open-photos", "Open Photos", ""),
            ApplicationLaunch::content_spec("so.tilo.photos", "open-capture", "Open", ""),
        ] {
            assert!(spec.contract.is_own_device(), "{}", spec.id);
            assert_eq!(
                spec.contract.need().kind,
                tilo_actions::ActionKind::Read,
                "{}",
                spec.id
            );
        }
    }

    /// A handoff reaches the app it names, on the record it names, and never falls back to
    /// opening the app empty.
    #[test]
    fn application_launch_action_preserves_destination_and_never_falls_back_to_open() {
        let launch = ApplicationLaunch::with_action(
            "so.tilo.file-explorer",
            "files.remote-account:616461",
            "open-remote-account",
        );
        let spec = launch.spec("open-files", "Browse files", "Open this account in Files");
        let encoded = spec.build_args([]).unwrap();
        assert_eq!(encoded.type_id, ApplicationLaunch::TYPE);
        let decoded: ApplicationLaunch = serde_json::from_slice(&encoded.payload).unwrap();
        assert!(decoded.valid());
        for workspace in ["workspace-a", "workspace-b"] {
            assert_eq!(
                decoded.target(workspace),
                tilo_actions::remote::application_action_target(
                    workspace,
                    "so.tilo.file-explorer",
                    "files.remote-account:616461",
                    "open-remote-account",
                )
            );
        }
        assert!(matches!(decoded.argument(), InvokeArgument::None));
        let legacy: ApplicationLaunch = serde_json::from_value(serde_json::json!({
            "app_id": "so.tilo.photos"
        }))
        .unwrap();
        assert!(legacy.valid());
        assert_eq!(
            legacy.target("workspace-a"),
            tilo_actions::remote::application_open_target("workspace-a", "so.tilo.photos")
        );
        let mut invalid = decoded;
        invalid.content_hash = Some("a".repeat(64));
        assert!(
            !invalid.valid(),
            "a handoff must not mix content staging and an app action"
        );
        invalid.content_hash = None;
        invalid.action.as_mut().unwrap().resource.clear();
        assert!(
            !invalid.valid(),
            "an invalid destination must fail, not open an empty app"
        );
    }

    /// A launch's `open` names the content it hands over and the file it is, and a name is
    /// one file's, never a path, and only content has one.
    #[test]
    fn a_launch_carries_the_name_of_the_file_it_opens() {
        let hash = "ab".repeat(32);
        let plain = ApplicationLaunch::with_content("so.tilo.notes", hash.clone());
        assert_eq!(
            opened_content(&plain.argument()),
            Some((hash.as_str(), None))
        );
        let named = plain.named("plan.txt");
        assert!(named.valid());
        assert_eq!(
            opened_content(&named.argument()),
            Some((hash.as_str(), Some("plan.txt")))
        );
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

    /// The actions every app's window publishes, as [`surface`] reads them.
    #[test]
    fn every_window_action_is_declared_once() {
        let surface = surface();
        let ids: Vec<&str> = surface.verbs.iter().map(|verb| verb.id.as_str()).collect();
        assert_eq!(ids, ["activate-window", "open", "open-link", "outline"]);
        assert_eq!(
            surface.verbs[1].kind,
            tilo_actions::ActionKind::Read,
            "opening a hit's item reads"
        );
        assert_eq!(AppLink::OPEN_LINK.params(), ["link"]);
        assert_eq!(
            AppOutline::OUTLINE.params(),
            [OUTLINE_ANCHOR, OUTLINE_PICTURE, "scale"],
            "the render operation names the same anchor"
        );
    }

    /// The file a tilolang view compiles the app's window actions against, and a crate that
    /// does not depend on this one binds its calls to, is what this crate declares.
    /// `UPDATE_SNAPSHOTS=1` writes it.
    #[test]
    fn the_app_view_surface_file_is_what_this_crate_publishes() {
        let text =
            tilo_surface_compile::canonical(&serde_json::json!({ "resources": [surface()] }));
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("app-view-surface.json");
        if std::env::var_os("UPDATE_SNAPSHOTS").is_some_and(|value| value == "1") {
            std::fs::write(&path, &text).expect("the surface file is written");
            return;
        }
        assert_eq!(
            std::fs::read_to_string(&path).ok().as_deref(),
            Some(text.as_str()),
            "crates/core/collab-types/app-view-surface.json is not what this crate publishes; \
             regenerate it with UPDATE_SNAPSHOTS=1"
        );
    }
}
