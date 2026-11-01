//! The session's own resources: what only the session can change, because
//! it runs the compositor (the touchpad, locking the screen) or holds the
//! questions about the person's apps, published on the same door as every
//! hosted surface's.

use std::cell::RefCell;
use std::rc::Rc;

use serde::{Deserialize, Serialize};
use tilo_actions::{
    ActionAddress, ActionContract, ActionParam, ActionSpec, DoorError, EncodedAction,
    ExecutionMode, OperationKind, ParamType, ResourceSpec, ResultSpec,
};
use tilo_desktop_search::providers::downloaded::{APP_PARAM, APPS_RESOURCE, ASK_UNINSTALL};
use tilo_surface_host::Published;

use crate::displays::DisplayMode;
use crate::i18n::t;
use ui::host_operation::ResourceOperationStart;

/// The operation type the session's own actions carry.
const OPERATION: &str = "tilo.session.command";
/// The resource of this device's input devices.
pub const INPUT_RESOURCE: &str = "session:input";
/// The resource of the screen magnifier.
pub const MAGNIFIER_RESOURCE: &str = "session:magnifier";
/// The resource of the session itself.
pub const SESSION_RESOURCE: &str = "session:screen";
/// The resource of the night light.
pub const NIGHT_LIGHT_RESOURCE: &str = "session:night-light";
/// The resource of this device's clipboard, which the clipboard publishes
/// itself: [`crate::mount::clipboard`].
pub const CLIPBOARD_RESOURCE: &str = "session:clipboard";

/// What the session's resources ask of the compositor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "do", rename_all = "kebab-case")]
pub enum SessionCommand {
    /// Turn the touchpads on (`Some(true)`), off, or (`None`) the other
    /// way from how they are.
    SetTouchpad { on: Option<bool> },
    /// Lock the screen.
    Lock,
    /// End the session: the person logs out.
    LogOut,
    /// End this session and nothing else: what shutting down is to a
    /// session in a window of another desktop, which owns no machine.
    End,
    /// Step the magnifier: on or off, stronger or weaker.
    Magnifier(Magnify),
    /// Use the displays together this way.
    Displays { mode: DisplayMode },
    /// Put the display sheet up, where the layouts and the wireless
    /// displays are chosen.
    OpenDisplaySheet,
    /// Take the display sheet down.
    CloseDisplaySheet,
    /// Turn the night light on (`Some(true)`), off, or (`None`) the other
    /// way from how it is.
    NightLight { on: Option<bool> },
    /// Paste the files copied to the clipboard into this folder, as a copy
    /// or (after a cut) a move.
    PasteFiles { destination: String },
    /// Type in the next of the person's keyboard input sources.
    NextInputSource,
    /// Ask the person whether to uninstall `app`, by its entry's id or the
    /// name they know it by: the question stands on the uninstall sheet
    /// until they answer it, as it does when the app list asks.
    AskUninstall { app: String },
}

/// A step of the magnifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Magnify {
    Toggle,
    ZoomIn,
    ZoomOut,
}

impl From<Magnify> for settings_store::magnifier::MagnifierStep {
    fn from(step: Magnify) -> Self {
        match step {
            Magnify::Toggle => Self::Toggle,
            Magnify::ZoomIn => Self::ZoomIn,
            Magnify::ZoomOut => Self::ZoomOut,
        }
    }
}

#[expect(
    clippy::expect_used,
    reason = "a session command always serializes to JSON"
)]
pub(crate) fn action(
    id: &'static str,
    label: String,
    params: Vec<ActionParam>,
    build: impl Fn(&tilo_actions::ActionArgs) -> SessionCommand + Send + Sync + 'static,
) -> ActionSpec {
    let mut spec = ActionSpec::typed_operation(
        id,
        label,
        String::new(),
        OperationKind::Effect,
        OPERATION,
        params,
        move |args| {
            EncodedAction::encode_operation(OPERATION, &build(args))
                .expect("a session command always serializes")
        },
    );
    spec.type_mut().contract = ActionContract {
        execution: ExecutionMode::Immediate,
        result: ResultSpec::None,
        ..ActionContract::default()
    };
    spec
}

/// The session's resources and their actions.
fn resources() -> Vec<ResourceSpec> {
    vec![
        input_resource(),
        screen_resource(),
        night_light_resource(),
        magnifier_resource(),
        apps_resource(),
    ]
}

/// The pointer and keyboard the session acts on.
fn input_resource() -> ResourceSpec {
    ResourceSpec::new(INPUT_RESOURCE, "session", t("session-input"))
        .with_action(action(
            "toggle-touchpad",
            t("session-toggle-touchpad"),
            vec![ActionParam::optional_owned(
                "on",
                ParamType::Boolean,
                t("session-param-touchpad-on"),
            )],
            |args| SessionCommand::SetTouchpad {
                on: args.boolean("on"),
            },
        ))
        .with_action(action(
            "next-source",
            t("session-next-input-source"),
            Vec::new(),
            |_| SessionCommand::NextInputSource,
        ))
}

/// Locking and logging out of the session.
fn screen_resource() -> ResourceSpec {
    ResourceSpec::new(SESSION_RESOURCE, "session", t("session-screen"))
        .with_action(action("lock", t("session-lock"), Vec::new(), |_| {
            SessionCommand::Lock
        }))
        .with_action(action("log-out", t("session-log-out"), Vec::new(), |_| {
            SessionCommand::LogOut
        }))
}

/// The screen's night light.
fn night_light_resource() -> ResourceSpec {
    ResourceSpec::new(NIGHT_LIGHT_RESOURCE, "session", t("session-night-light")).with_action(
        action(
            "toggle",
            t("session-night-light-toggle"),
            vec![ActionParam::optional_owned(
                "on",
                ParamType::Boolean,
                t("session-param-night-light-on"),
            )],
            |args| SessionCommand::NightLight {
                on: args.boolean("on"),
            },
        ),
    )
}

/// The screen magnifier.
fn magnifier_resource() -> ResourceSpec {
    ResourceSpec::new(MAGNIFIER_RESOURCE, "session", t("session-magnifier"))
        .with_action(action(
            "toggle",
            t("session-magnifier-toggle"),
            Vec::new(),
            |_| SessionCommand::Magnifier(Magnify::Toggle),
        ))
        .with_action(action(
            "zoom-in",
            t("session-magnifier-zoom-in"),
            Vec::new(),
            |_| SessionCommand::Magnifier(Magnify::ZoomIn),
        ))
        .with_action(action(
            "zoom-out",
            t("session-magnifier-zoom-out"),
            Vec::new(),
            |_| SessionCommand::Magnifier(Magnify::ZoomOut),
        ))
}

/// The questions about the person's apps: whether to uninstall one. The
/// answer is the downloaded apps registry's own action, as from anywhere.
fn apps_resource() -> ResourceSpec {
    ResourceSpec::new(APPS_RESOURCE, "session", t("session-apps")).with_action(action(
        ASK_UNINSTALL,
        t("session-apps-uninstall"),
        vec![ActionParam::required_owned(
            APP_PARAM,
            ParamType::Text,
            t("session-param-app"),
        )],
        |args| SessionCommand::AskUninstall {
            app: args.text(APP_PARAM).unwrap_or_default().to_owned(),
        },
    ))
}

/// The [`SessionCommand`] `arguments` make of `address`, checked against what
/// `published` offers.
pub(crate) fn command_of(
    published: &[ResourceSpec],
    address: &ActionAddress,
    arguments: &serde_json::Map<String, serde_json::Value>,
) -> Result<SessionCommand, DoorError> {
    let encoded = tilo_actions::door::encode_json(published, address, arguments)?;
    (encoded.type_id == OPERATION)
        .then(|| serde_json::from_slice::<SessionCommand>(&encoded.payload).ok())
        .flatten()
        .ok_or_else(|| DoorError::Failed("not a session command".into()))
}

/// The session's own door: its actions become [`SessionCommand`]s the
/// session carries out with the compositor.
pub(crate) struct SessionControls {
    commands: Rc<RefCell<Vec<SessionCommand>>>,
}

impl SessionControls {
    pub(crate) const fn new(commands: Rc<RefCell<Vec<SessionCommand>>>) -> Self {
        Self { commands }
    }
}

impl Published for SessionControls {
    fn resources(&mut self) -> Vec<ResourceSpec> {
        resources()
    }

    fn revision(&self) -> Option<u64> {
        // Its actions never change.
        Some(0)
    }

    fn invoke(
        &mut self,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<ResourceOperationStart, DoorError> {
        let command = command_of(&resources(), address, arguments)?;
        self.commands.borrow_mut().push(command);
        Ok(ResourceOperationStart::Completed(
            tilo_actions::ActionOutput::default(),
        ))
    }

    fn accessible_labels(&mut self) -> Vec<String> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_session_publishes_its_input_and_lock_actions_and_runs_them_as_commands() {
        let commands = Rc::new(RefCell::new(Vec::new()));
        let mut controls = SessionControls::new(Rc::clone(&commands));
        let mut arguments = serde_json::Map::new();
        controls
            .invoke(
                &ActionAddress::new(INPUT_RESOURCE, "toggle-touchpad"),
                &arguments,
            )
            .unwrap();
        arguments.insert("on".into(), true.into());
        controls
            .invoke(
                &ActionAddress::new(INPUT_RESOURCE, "toggle-touchpad"),
                &arguments,
            )
            .unwrap();
        controls
            .invoke(
                &ActionAddress::new(SESSION_RESOURCE, "lock"),
                &serde_json::Map::new(),
            )
            .unwrap();
        assert_eq!(
            *commands.borrow(),
            [
                SessionCommand::SetTouchpad { on: None },
                SessionCommand::SetTouchpad { on: Some(true) },
                SessionCommand::Lock,
            ]
        );
        assert!(
            controls
                .invoke(
                    &ActionAddress::new(INPUT_RESOURCE, "nothing"),
                    &serde_json::Map::new()
                )
                .is_err()
        );
    }
}
