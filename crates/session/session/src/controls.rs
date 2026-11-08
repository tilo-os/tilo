//! The session's own resources: what only the session can change, because it runs the compositor
//! (the touchpad, locking the screen) or holds the questions about the person's apps.
//! They are served as a service beside every other the session runs, so an app reaches them the way
//! it reaches each service's resources: reads what they are now and invokes what changes them
//! through one door. What only the session holds is published here as its reading — whether the
//! screens are locked, the touchpad is on, the night light is on and how far the magnifier zooms —
//! so a resource that only acted would be the one kind of resource a panel could not open.

use std::cell::RefCell;
use std::rc::Rc;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tilo_actions::{
    ActionAddress, ActionContract, ActionParam, ActionSpec, DoorError, EncodedAction,
    ExecutionMode, OperationKind, ParamType, ResourceSpec, ResultSpec,
};
use tilo_compositor::Error;
use tilo_desktop::osd::{Hint, OsdKind};
use tilo_desktop_search::providers::downloaded::{APP_PARAM, APPS_RESOURCE, ASK_UNINSTALL};
use tilo_service::{Service, Started};

use crate::displays::DisplayMode;
use tilo_sheets::Seat as _;

use crate::i18n::t;
use crate::{displays, input_sources};

/// Type with `keyboard` : the compositor's keymap, and the words printed on the keys of what the
/// session draws (the shortcut guide, the desk's chords), both follow it.
pub(crate) fn apply_keyboard(
    compositor: &mut tilo_compositor::Compositor,
    keyboard: &tilo_compositor::KeyboardLayout,
) {
    compositor.set_keyboard_layout(keyboard);
    ui::keycaps::set_active(ui::keycaps::Keycaps::from_xkb(
        &keyboard.layout,
        &keyboard.variant,
    ));
}

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
/// The resource of this device's clipboard, which the clipboard publishes itself:
/// [`crate::mount::clipboard`].
pub const CLIPBOARD_RESOURCE: &str = "session:clipboard";

/// What the session's resources ask of the compositor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "do", rename_all = "kebab-case")]
pub enum SessionCommand {
    /// Turn the touchpads on (`Some(true)`), off, or (`None`) the other way from how they are.
    SetTouchpad { on: Option<bool> },
    /// Lock the screen.
    Lock,
    /// Put the power menu up: locking, logging out, switching user,
    /// suspending, restarting or shutting down.
    OpenPowerMenu,
    /// End the session: the person logs out.
    LogOut,
    /// End this session and nothing else: what shutting down is to a session in a window of another
    /// desktop, which owns no machine.
    End,
    /// Step the magnifier: on or off, stronger or weaker.
    Magnifier(Magnify),
    /// Use the displays together this way.
    Displays { mode: DisplayMode },
    /// Put the display sheet up, where the layouts and the wireless displays are chosen.
    OpenDisplaySheet,
    /// Take the display sheet down.
    CloseDisplaySheet,
    /// Turn the night light on (`Some(true)`), off, or (`None`) the other way from how it is.
    NightLight { on: Option<bool> },
    /// Paste the files copied to the clipboard into this folder, as a copy or (after a cut) a move.
    /// The clipboard's own `paste-files` action is how it is asked now; this is what a command
    /// recorded before that carries.
    PasteFiles { destination: String },
    /// Type in the next of the person's keyboard input sources.
    NextInputSource,
    /// Ask the person whether to uninstall `app` , by its entry's id or the name they know it by:
    /// the question stands on the uninstall sheet until they answer it, as it does when the app
    /// list asks.
    AskUninstall { app: String },
    /// Switch the machine to virtual terminal `vt` , numbered from 1, as `Ctrl+Alt+F1..F12` asks:
    /// the way out to a text console when the desktop cannot give the machine back.
    SwitchVt { vt: i32 },
    /// Stop a remote person driving this machine's seat, as `Super+Escape` asks: everything stays
    /// as it is, and their input stops.
    CutRemoteInput,
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

/// Locking, logging out of and the menu of the session.
fn screen_resource() -> ResourceSpec {
    ResourceSpec::new(SESSION_RESOURCE, "session", t("session-screen"))
        .with_action(action("lock", t("session-lock"), Vec::new(), |_| {
            SessionCommand::Lock
        }))
        .with_action(action("log-out", t("session-log-out"), Vec::new(), |_| {
            SessionCommand::LogOut
        }))
        .with_action(action(
            "power-menu",
            t("session-power-menu"),
            Vec::new(),
            |_| SessionCommand::OpenPowerMenu,
        ))
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

/// The questions about the person's apps: whether to uninstall one. The answer is the downloaded
/// apps registry's own action, as from anywhere.
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

/// The [`SessionCommand`] `arguments` make of `address` , checked against what `published` offers.
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

/// What the session's own resources read: the state of what only the session keeps, told to them by
/// the loop every pass.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlsReading {
    /// The screens are locked.
    pub locked: bool,
    /// The touchpad takes the person's fingers.
    pub touchpad: bool,
    /// The screen is warm.
    pub night_light: bool,
    /// How far the magnifier zooms, `0` while it is off.
    pub zoom: u8,
}

/// The session's own resources and the commands they raise: what only the session can carry out,
/// served beside its services.
pub(crate) struct SessionControls {
    commands: Rc<RefCell<Vec<SessionCommand>>>,
    /// The state the resources read, written by the loop.
    reading: Rc<RefCell<ControlsReading>>,
    /// The reading last reported, so a pass that changed nothing is no revision.
    published: RefCell<Option<ControlsReading>>,
}

impl SessionControls {
    pub(crate) const fn new(
        commands: Rc<RefCell<Vec<SessionCommand>>>,
        reading: Rc<RefCell<ControlsReading>>,
    ) -> Self {
        Self {
            commands,
            reading,
            published: RefCell::new(None),
        }
    }

    /// What `resource` reads: the part of the session's own state that resource changes, or nothing
    /// for the questions about apps.
    fn reading_of(reading: &ControlsReading, resource: &str) -> Option<Value> {
        match resource {
            SESSION_RESOURCE => Some(json!({ "locked": reading.locked })),
            INPUT_RESOURCE => Some(json!({ "touchpad": reading.touchpad })),
            NIGHT_LIGHT_RESOURCE => Some(json!({ "on": reading.night_light })),
            MAGNIFIER_RESOURCE => Some(json!({
                "on": reading.zoom > settings_store::magnifier::OFF,
                "zoom": reading.zoom,
            })),
            APPS_RESOURCE => Some(json!({})),
            _ => None,
        }
    }
}

impl Service for SessionControls {
    fn resources(&self) -> Vec<ResourceSpec> {
        // What the controls change is this device's own state, not anyone's content: a grant to
        // read the session's resources reads it.
        resources()
            .into_iter()
            .map(ResourceSpec::with_granted_reading)
            .collect()
    }

    fn read(&self, resource: &str) -> Option<Value> {
        let published = self.published.borrow().clone();
        let reading = published.unwrap_or_else(|| self.reading.borrow().clone());
        Self::reading_of(&reading, resource)
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, DoorError> {
        let address = ActionAddress::new(resource, action);
        let command = command_of(&resources(), &address, arguments)?;
        self.commands.borrow_mut().push(command);
        Ok(Started::Done(tilo_actions::ActionOutput::default()))
    }

    fn poll(&mut self) -> bool {
        let now = self.reading.borrow().clone();
        let mut published = self.published.borrow_mut();
        if published.as_ref() == Some(&now) {
            return false;
        }
        *published = Some(now);
        true
    }
}

impl crate::Session {
    /// Carry out what the session's own resources asked of it: the compositor, the sheets and the
    /// settings it alone holds.
    pub(crate) fn command(&mut self, command: SessionCommand) -> Result<(), Error> {
        match command {
            SessionCommand::Lock => self.lock_by_person(),
            SessionCommand::SwitchVt { vt } => self.switch_vt(vt),
            SessionCommand::CutRemoteInput => {
                self.compositor.cut_remote_input();
                Ok(())
            }
            SessionCommand::OpenPowerMenu => {
                if let Some(power) = &self.shared.power {
                    power.presentation().open(false);
                }
                Ok(())
            }
            SessionCommand::Displays { mode } => {
                let changes = displays::arrange(mode, &self.compositor.outputs());
                // A refused arrangement leaves the displays as they were.
                if !changes.is_empty()
                    && let Err(error) = self.compositor.configure_outputs(&changes)
                {
                    tracing::warn!(%error, ?mode, "the displays were not arranged");
                }
                Ok(())
            }
            SessionCommand::OpenDisplaySheet => {
                self.display_sheet.open();
                Ok(())
            }
            SessionCommand::CloseDisplaySheet => {
                self.display_sheet.close();
                Ok(())
            }
            SessionCommand::Magnifier(step) => {
                let (zoom, remembered) = self.magnifier;
                self.magnifier = settings_store::magnifier::step(zoom, remembered, step.into());
                self.compositor.set_zoom(f64::from(self.magnifier.0));
                self.hint(
                    OsdKind::Magnifier,
                    self.magnifier.0 > settings_store::magnifier::OFF,
                );
                Ok(())
            }
            SessionCommand::LogOut => {
                // The login wrapper tears the session down once this process ends; without one,
                // ending is the whole of it. A session in a window of another desktop has no
                // wrapper of its own: ending is all logging out is there.
                if !self.shared.hosted
                    && let Err(error) = os_actions::request_logout()
                {
                    tracing::info!(%error, "no login wrapper to tell of the logout");
                }
                // Apps are told the session ends before the loop stops answering them.
                if let Some(desktop) = &self.desktop {
                    desktop.end();
                }
                self.ended = true;
                Ok(())
            }
            SessionCommand::End => {
                self.ended = true;
                Ok(())
            }
            SessionCommand::NightLight { on } => self.set_night_light(on),
            SessionCommand::PasteFiles { destination } => {
                self.paste_files(&destination);
                Ok(())
            }
            SessionCommand::NextInputSource => {
                let Some((sources, name)) = input_sources::next() else {
                    return Ok(());
                };
                self.sources = sources;
                apply_keyboard(&mut self.compositor, &self.sources.compiled());
                self.show_hint(Hint {
                    kind: OsdKind::InputSource,
                    on: true,
                    chord: None,
                    name: Some(name),
                });
                Ok(())
            }
            SessionCommand::AskUninstall { app } => {
                self.shared.ask_uninstall(&app);
                Ok(())
            }
            SessionCommand::SetTouchpad { on } => {
                let enabled = on.unwrap_or(!self.input.touchpad_enabled);
                self.input.touchpad_enabled = enabled;
                self.compositor.set_input_config(self.input.clone());
                self.hint(OsdKind::Touchpad, enabled);
                Ok(())
            }
        }
    }

    /// What the session's own resources read of this device, as this pass left it: a key that
    /// toggled the touchpad and a command that lit the night light are both read here, and the
    /// controls' service publishes them to the apps that asked.
    pub(crate) fn read_controls(&self) {
        let (zoom, _) = self.magnifier;
        self.shared
            .controls
            .borrow_mut()
            .clone_from(&ControlsReading {
                locked: self.compositor.locked(),
                touchpad: self.input.touchpad_enabled,
                night_light: self.night_light.showing(),
                zoom,
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_session_publishes_its_input_and_lock_actions_and_runs_them_as_commands() {
        let commands = Rc::new(RefCell::new(Vec::new()));
        let reading = Rc::new(RefCell::new(ControlsReading::default()));
        let mut controls = SessionControls::new(Rc::clone(&commands), reading);
        let mut arguments = serde_json::Map::new();
        controls
            .invoke(INPUT_RESOURCE, "toggle-touchpad", &arguments)
            .unwrap();
        arguments.insert("on".into(), true.into());
        controls
            .invoke(INPUT_RESOURCE, "toggle-touchpad", &arguments)
            .unwrap();
        controls
            .invoke(SESSION_RESOURCE, "lock", &serde_json::Map::new())
            .unwrap();
        controls
            .invoke(SESSION_RESOURCE, "power-menu", &serde_json::Map::new())
            .unwrap();
        assert_eq!(
            *commands.borrow(),
            [
                SessionCommand::SetTouchpad { on: None },
                SessionCommand::SetTouchpad { on: Some(true) },
                SessionCommand::Lock,
                SessionCommand::OpenPowerMenu,
            ]
        );
        assert!(
            controls
                .invoke(INPUT_RESOURCE, "nothing", &serde_json::Map::new())
                .is_err()
        );
    }

    #[test]
    fn every_resource_reads_the_state_its_actions_change() {
        let commands = Rc::new(RefCell::new(Vec::new()));
        let reading = ControlsReading {
            locked: true,
            touchpad: true,
            night_light: true,
            zoom: 3,
        };
        let mut controls =
            SessionControls::new(Rc::clone(&commands), Rc::new(RefCell::new(reading)));
        // What a reader sees before the loop's first pass is read at once,
        // not left empty until something changes.
        assert_eq!(
            controls.read(SESSION_RESOURCE),
            Some(json!({ "locked": true }))
        );
        assert_eq!(
            controls.read(INPUT_RESOURCE),
            Some(json!({ "touchpad": true }))
        );
        assert_eq!(
            controls.read(NIGHT_LIGHT_RESOURCE),
            Some(json!({ "on": true }))
        );
        assert_eq!(
            controls.read(MAGNIFIER_RESOURCE),
            Some(json!({ "on": true, "zoom": 3 }))
        );
        assert!(controls.read(APPS_RESOURCE).is_some());
        assert_eq!(controls.read("session:nothing"), None);
        assert!(controls.poll(), "a reader is told what it was not told");
        assert!(!controls.poll(), "an unchanged reading is no revision");
    }
}
