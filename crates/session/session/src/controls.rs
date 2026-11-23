//! The session's own resources: what only the session can change, because it runs the compositor
//! (the touchpad, locking the screen) or holds the questions about the person's apps.
//! They are served as a service beside every other the session runs, so an app reaches them the way
//! it reaches each service's resources: reads what they are now and invokes what changes them
//! through one action API. What only the session holds is published here as its reading — whether the
//! screens are locked, the touchpad is on, the night light is on and how far the magnifier zooms —
//! so a resource that only acted would be the one kind of resource a panel could not open.

use std::cell::RefCell;
use std::rc::Rc;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tilo_actions::{ActionAddress, ApiError, ResourceSpec};
use tilo_compositor::Error;
use tilo_desktop::osd::{Hint, OsdKind};
use tilo_desktop_search::providers::downloaded::APPS_RESOURCE;
use tilo_resource::Effect as _;
use tilo_service::{Service, Started};

use crate::displays::DisplayMode;
use tilo_sheets::SheetState as _;

use crate::i18n::t;
use crate::{displays, input_sources};

/// Type with `keyboard` : the compositor's keymap, and the words printed on the keys of what the
/// session draws (the shortcut guide, the shell's chords), both follow it.
pub(crate) fn apply_keyboard(
    compositor: &mut tilo_compositor::Compositor,
    keyboard: &tilo_compositor::KeyboardLayout,
) {
    compositor.set_keyboard_layout(keyboard);
    let keycaps = ui::keycaps::Keycaps::from_xkb(&keyboard.layout, &keyboard.variant);
    ui::keycaps::set_active(keycaps);
    // A chord on a mark is shown as the keys this layout presses for it, read from the keymap the
    // chords are matched with.
    ui::keycaps::set_typed_marks(
        compositor
            .typed_marks()
            .into_iter()
            .map(|typed| {
                let key = ui::keycaps::TypedMark {
                    key: keycaps.native_symbol(&typed.key_name, &typed.key_text),
                    shift: typed.shift,
                    alt_gr: typed.level3,
                };
                (typed.mark, key)
            })
            .collect(),
    );
}

/// The resource of this device's input devices.
pub const INPUT_RESOURCE: &str = "session:input";
/// The resource of the screen magnifier.
pub const MAGNIFIER_RESOURCE: &str = "session:magnifier";
/// The resource of the session itself.
pub const SESSION_RESOURCE: &str = "session:screen";
/// The resource of the night light.
pub const NIGHT_LIGHT_RESOURCE: &str = "session:night-light";
/// The resource of this device's clipboard, which the clipboard publishes itself:
/// [`crate::clipboard`].
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
    /// Stop a remote person driving this machine's state, as `Super+Escape` asks: everything stays
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

/// The session's own resources, as declared types.
#[derive(tilo_resource::Row)]
#[row(kind = "session", label = "Input")]
pub struct Input;

#[derive(tilo_resource::Row)]
#[row(kind = "session", label = "Screen")]
pub struct Screen;

#[derive(tilo_resource::Row)]
#[row(kind = "session", label = "Night light")]
pub struct NightLight;

#[derive(tilo_resource::Row)]
#[row(kind = "session", label = "Magnifier")]
pub struct Magnifier;

#[derive(tilo_resource::Row)]
#[row(kind = "session", label = "Apps")]
pub struct AppsRoot;

/// What this device's input devices answer.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "so.tilo.session.input")]
pub enum InputEffect {
    /// Turn the touchpad on or off
    #[effect(kind = "undoable", label = "session-toggle-touchpad")]
    ToggleTouchpad {
        /// Whether it takes the person's fingers
        #[param("Whether it takes the person's fingers")]
        on: Option<bool>,
    },
    /// Switch to the next keyboard layout
    #[effect(kind = "undoable", label = "session-next-input-source")]
    NextSource,
}

/// Locking, logging out of and the menu of the session.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "so.tilo.session.screen")]
pub enum ScreenEffect {
    /// Lock the screens
    #[effect(kind = "undoable", label = "session-lock")]
    Lock,
    /// Log out of the session
    #[effect(
        kind = "undoable",
        label = "session-log-out",
        keywords = "sign out, logout, log off"
    )]
    LogOut,
    /// Open the power menu
    #[effect(kind = "undoable", label = "session-power-menu")]
    PowerMenu,
}

/// The night light as a view of another language reads and changes it: whether the screen is warm,
/// and the verb that switches it.
#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
    tilo_resource::JsonSchema,
)]
pub(crate) struct NightLightReading {
    /// Whether the screen is warm.
    pub on: bool,
}

/// The night light as a view of another language reads it (`device.night_light`).
#[must_use]
pub(crate) fn night_light_surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<NightLightReading, NightLightEffect>(
        "night-light",
        "Night light",
    )
}

/// The screen's night light.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "so.tilo.session.night-light")]
pub enum NightLightEffect {
    /// Turn the night light on or off
    #[effect(kind = "undoable", label = "session-night-light-toggle")]
    Toggle {
        /// Whether the screen is warm
        #[param("Whether the screen is warm")]
        on: Option<bool>,
    },
}

/// The screen magnifier.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "so.tilo.session.magnifier")]
pub enum MagnifierEffect {
    /// Turn the magnifier on or off
    #[effect(kind = "undoable", label = "session-magnifier-toggle")]
    Toggle,
    /// Zoom in
    #[effect(kind = "undoable", label = "session-magnifier-zoom-in")]
    ZoomIn,
    /// Zoom out
    #[effect(kind = "undoable", label = "session-magnifier-zoom-out")]
    ZoomOut,
}

/// The questions about the person's apps: whether to uninstall one.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "so.tilo.session.apps")]
pub enum AppsEffect {
    /// Ask to uninstall an app
    #[effect(kind = "undoable", label = "session-apps-uninstall")]
    Uninstall {
        /// The app's name
        #[param("The app's name")]
        app: String,
    },
}

impl InputEffect {
    fn command(&self) -> SessionCommand {
        match self {
            Self::ToggleTouchpad { on } => SessionCommand::SetTouchpad { on: *on },
            Self::NextSource => SessionCommand::NextInputSource,
        }
    }
}

impl ScreenEffect {
    fn command(&self) -> SessionCommand {
        match self {
            Self::Lock => SessionCommand::Lock,
            Self::LogOut => SessionCommand::LogOut,
            Self::PowerMenu => SessionCommand::OpenPowerMenu,
        }
    }
}

impl NightLightEffect {
    fn command(&self) -> SessionCommand {
        match self {
            Self::Toggle { on } => SessionCommand::NightLight { on: *on },
        }
    }
}

impl MagnifierEffect {
    fn command(&self) -> SessionCommand {
        match self {
            Self::Toggle => SessionCommand::Magnifier(Magnify::Toggle),
            Self::ZoomIn => SessionCommand::Magnifier(Magnify::ZoomIn),
            Self::ZoomOut => SessionCommand::Magnifier(Magnify::ZoomOut),
        }
    }
}

impl AppsEffect {
    fn command(&self) -> SessionCommand {
        match self {
            Self::Uninstall { app } => SessionCommand::AskUninstall { app: app.clone() },
        }
    }
}

/// What the session's own resources and their actions are.
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
    tilo_resource::offer::<InputEffect>(tilo_resource::typed_resource::<Input>(
        INPUT_RESOURCE,
        t("session-input"),
    ))
}

/// Locking, logging out of and the menu of the session.
fn screen_resource() -> ResourceSpec {
    tilo_resource::offer::<ScreenEffect>(tilo_resource::typed_resource::<Screen>(
        SESSION_RESOURCE,
        t("session-screen"),
    ))
}

/// The screen's night light.
fn night_light_resource() -> ResourceSpec {
    tilo_resource::offer::<NightLightEffect>(tilo_resource::typed_resource::<NightLight>(
        NIGHT_LIGHT_RESOURCE,
        t("session-night-light"),
    ))
}

/// The screen magnifier.
fn magnifier_resource() -> ResourceSpec {
    tilo_resource::offer::<MagnifierEffect>(tilo_resource::typed_resource::<Magnifier>(
        MAGNIFIER_RESOURCE,
        t("session-magnifier"),
    ))
}

/// The questions about the person's apps: whether to uninstall one. The answer is the downloaded
/// apps registry's own action, as from anywhere.
fn apps_resource() -> ResourceSpec {
    tilo_resource::offer::<AppsEffect>(tilo_resource::typed_resource::<AppsRoot>(
        APPS_RESOURCE,
        t("session-apps"),
    ))
}

/// The [`SessionCommand`] `arguments` make of `address` , checked against what `published` offers.
pub(crate) fn command_of(
    published: &[ResourceSpec],
    address: &ActionAddress,
    arguments: &serde_json::Map<String, serde_json::Value>,
) -> Result<SessionCommand, ApiError> {
    let encoded = tilo_actions::api::encode_json(published, address, arguments)?;
    let command = match address.resource.as_str() {
        INPUT_RESOURCE => InputEffect::from_payload(&encoded.payload)
            .ok()
            .map(|effect| effect.command()),
        SESSION_RESOURCE => ScreenEffect::from_payload(&encoded.payload)
            .ok()
            .map(|effect| effect.command()),
        NIGHT_LIGHT_RESOURCE => NightLightEffect::from_payload(&encoded.payload)
            .ok()
            .map(|effect| effect.command()),
        MAGNIFIER_RESOURCE => MagnifierEffect::from_payload(&encoded.payload)
            .ok()
            .map(|effect| effect.command()),
        APPS_RESOURCE => AppsEffect::from_payload(&encoded.payload)
            .ok()
            .map(|effect| effect.command()),
        _ => None,
    };
    command.ok_or_else(|| ApiError::Failed("not a session command".into()))
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
    /// The app lists' generation last reported: an app installed or taken away moves it.
    apps: u64,
}

/// The applications installed on this device, as `session:apps` publishes them: what the
/// launcher lists and opens.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
pub(crate) struct Apps {
    pub installed: Vec<App>,
}

/// An application installed on this device: its package id, which `open-app` names, its name in
/// the person's language, its glyph, and its category when it named one.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
pub(crate) struct App {
    pub key: String,
    pub name: String,
    /// The glyph it shows, by the platform icon's kebab-case name.
    pub icon: String,
    /// `work`, `media`, `play` or `system`; empty when it named none.
    pub category: String,
}

impl Apps {
    fn installed() -> Self {
        let installed = crate::app_icons::app_lists_snapshot()
            .map(|(_, lists)| {
                lists
                    .applications
                    .iter()
                    .map(|app| App {
                        key: app.id.clone(),
                        name: app.title.clone(),
                        icon: ui::icons::app_glyph(&app.id, app.category.as_deref())
                            .name()
                            .to_owned(),
                        category: app.category.clone().unwrap_or_default(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self { installed }
    }
}

/// The apps as a view of another language reads them (`device.apps`).
#[must_use]
pub(crate) fn apps_surface() -> tilo_actions::surface::Surface {
    tilo_resource::surface::reading_surface::<Apps, tilo_resource::NoEffect>("apps", "Apps")
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
            apps: 0,
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
            APPS_RESOURCE => Some(tilo_service::reading(&Apps::installed())),
            _ => None,
        }
    }
}

impl Service for SessionControls {
    fn resources(&self) -> Vec<ResourceSpec> {
        // What the controls change is this device's own state, not anyone's content: only a caller
        // whose declared kinds reach the session's reads it.
        resources()
            .into_iter()
            .map(ResourceSpec::with_personal_reading)
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
    ) -> Result<Started, ApiError> {
        let address = ActionAddress::new(resource, action);
        let command = command_of(&resources(), &address, arguments)?;
        self.commands.borrow_mut().push(command);
        Ok(Started::Done(tilo_actions::ActionOutput::default()))
    }

    fn poll(&mut self) -> bool {
        let apps = crate::app_icons::app_lists_snapshot().map_or(0, |(generation, _)| generation);
        let apps_moved = std::mem::replace(&mut self.apps, apps) != apps;
        let now = self.reading.borrow().clone();
        let mut published = self.published.borrow_mut();
        if published.as_ref() == Some(&now) {
            return apps_moved;
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
                // A refused display mode leaves the displays as they were.
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

    tilo_test_env::scenario! {
        fn the_sessions_controls_are_actions_over_the_state_their_resources_read;

        phase the_session_publishes_its_input_and_lock_actions_and_runs_them_as_commands;
        {
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

        phase every_resource_reads_the_state_its_actions_change;
        {
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
}
