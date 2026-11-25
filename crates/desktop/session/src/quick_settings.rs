//! Quick settings, as the session keeps it: the shell draws the panel (`quick.tl`) as a surface of
//! its own over the windows, and reads what it shows from the session's services. The session
//! publishes which controls the panel shows and in what order ([`QUICK_SETTINGS`]), so a press in
//! the panel, an agent and a script change the same setting. Opening Settings from the panel, and
//! from the dictation card that sends the person there, is the session's too: it is the shell's
//! own search.

use settings_store::hardware::{QuickSettingsConfig, QuickSettingsControl};
use tilo_desktop_search::providers::settings::SETTINGS_APP;
use tilo_session_core::desktop::navigation::ChromeEffect;
use tilo_session_core::desktop::overlays::Part;

use crate::Shared;

/// The name the shell's surface of quick settings is opened and closed by.
pub const SURFACE: &str = "quick-settings";

/// Where Settings installs the local speech models dictation needs: the page of the models the
/// person's devices run themselves.
const SPEECH_MODELS_ANCHOR: &str = "settings.section:agent_models";

impl crate::Session {
    /// Carry out what the clipboard was asked. A copy chosen to paste is pasted where the person
    /// was typing: what stands over that field goes down first.
    pub(crate) fn serve_clipboard_asks(&mut self) {
        if self.shared.clipboard.pasting() {
            self.shared.overlays.close(Part::QuickSettings);
        }
        self.shared.clipboard.serve_asks(&mut self.compositor);
    }

    /// Stand the session's layer surface `namespace` where the overlay stack placed it.
    pub(crate) fn stand_layer(
        &mut self,
        namespace: &str,
        placement: &tilo_session_core::desktop::overlays::Placement,
    ) {
        let room = placement.room;
        self.compositor.place_layer_in(
            namespace,
            Some((
                offset(room.x0),
                offset(room.y0),
                offset(room.width()).max(1),
                offset(room.height()).max(1),
            )),
            placement.rests_at_bottom,
        );
    }
}

/// `length` logical pixels as a signed whole number of them.
fn offset(length: f64) -> i32 {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a placed surface is at most a few thousand logical pixels"
    )]
    let offset = length.round() as i32;
    offset
}

/// Close the panel: what a press that takes the person elsewhere does first.
pub(crate) fn close(shared: &Shared) {
    shared.overlays.close(Part::QuickSettings);
    shared.surfaces.close(SURFACE);
}

/// Take the person where the speech models are set up when the dictation card's "Set up" was
/// pressed: Settings at its models page, through the action path search rows use; a plain launch
/// opens Settings too when no action host runs an app's actions.
pub(crate) fn follow_dictation(shared: &Shared) {
    if !shared.dictation.take_model_setup() {
        return;
    }
    open_settings(shared, SPEECH_MODELS_ANCHOR);
}

/// Settings' page of the person's devices, whose "Add a device" prints the ticket a new device
/// joins with.
const ADD_DEVICE_PAGE: &str = "settings://devices_sync";

impl Shared {
    /// Add a device, wherever it was asked for (the person's own card, Settings' Devices and
    /// access, an app's own offer): Settings at its devices page, where the ticket is printed.
    /// The device's daemon issues the ticket itself (`tilo.device`).
    pub(crate) fn open_add_device(&self) {
        open_settings(self, ADD_DEVICE_PAGE);
    }
}

/// Open Settings at the page `link` names — a `settings://` page or a page's anchor — through the
/// action path search rows run; a plain launch opens Settings itself too when no action host runs
/// an app's actions.
pub(crate) fn open_settings(shared: &Shared, link: &str) {
    close(shared);
    let workspace = shared.active_workspace_id();
    if let Some(anchor) = ui::shell::settings_anchor(link) {
        let target = tilo_desktop_search::Target::Invoke(Box::new(
            tilo_desktop_search::providers::settings::opening(&workspace, &anchor),
        ));
        let opened = shared.search.perform(&workspace, target, &mut |own| {
            shared
                .published
                .invoke(
                    &own.address,
                    &own.arguments,
                    &mut tilo_session_core::surfaces::Look::none(),
                )
                .map(drop)
        });
        if opened.is_ok() {
            return;
        }
    }
    let launch = ChromeEffect::OpenApp {
        app: SETTINGS_APP.into(),
        beside: None,
        background: None,
        float: None,
    };
    if let Err(error) = shared.handle.dispatch(launch) {
        // The person asked for Settings and it did not open: they hear why.
        shared.answer_failed(&crate::i18n::t("action-failed-that-settings"), &error);
    }
}

/// Which controls the panel shows, and in what order: the person's own setting of this device,
/// published as an action so that a press in the panel, an agent and a script change it the same
/// way, whether the panel is drawn or not.
pub const QUICK_SETTINGS: &str = "session:quick-settings";
/// The panel's layout as the session publishes it: which controls it shows and in what order.
#[derive(tilo_resource::Row)]
#[row(kind = "session.quick-settings", label = "Quick settings", effects = LayoutAction)]
pub(crate) struct QuickSettings;

pub(crate) use tilo_device_surfaces::quick_settings::{LayoutAction, QuickSettingsView};

/// The Quick Settings resource: which controls the panel shows, read by the panel and by anyone else
/// who asks.
fn quick_settings_resource() -> tilo_actions::ResourceSpec {
    tilo_service::offer::<LayoutAction>(
        tilo_resource::LiveRow::typed::<QuickSettings>(
            QUICK_SETTINGS,
            crate::i18n::t("session-quick-settings"),
        )
        .into_resource()
        .with_personal_reading(),
    )
}

/// Which controls the panel shows, as the panel reads them: the person's choice of controls on this
/// device, kept in its settings document.
#[derive(Default)]
pub(crate) struct QuickSettingsService {
    /// The controls as last read, published as the reading.
    shown: std::cell::RefCell<QuickSettingsConfig>,
    /// Whether the settings document was read at all, and the version that was read: one that did
    /// not move is not read again.
    settings: settings_store::SettingsFollower,
    /// Whether an action this service took changed the controls, so the reading it answers with
    /// and the one it published are told apart once: what a press changed is published too.
    changed: std::cell::Cell<bool>,
}

/// Show the control `control` names on the panel. Whether the controls changed.
pub(crate) fn show_control(config: &mut QuickSettingsConfig, control: &str) -> bool {
    let Some(control) = control_named(control) else {
        return false;
    };
    settled(config, |config| {
        if !config.controls.contains(&control) {
            config.controls.push(control);
        }
    })
}

/// Take the control `control` names off the panel: Wi-Fi stays, as the way a desktop without a
/// network gets online. Whether the controls changed.
pub(crate) fn hide_control(config: &mut QuickSettingsConfig, control: &str) -> bool {
    let Some(control) = control_named(control) else {
        return false;
    };
    settled(config, |config| {
        if !control.required() {
            config.controls.retain(|shown| *shown != control);
        }
    })
}

/// Move the control `control` names to stand before `before`, the last place when none is named.
/// Whether the controls changed.
pub(crate) fn move_control(
    config: &mut QuickSettingsConfig,
    control: &str,
    before: Option<&str>,
) -> bool {
    let Some(control) = control_named(control) else {
        return false;
    };
    let before = before.and_then(control_named);
    settled(config, |config| {
        let Some(from) = config.controls.iter().position(|shown| *shown == control) else {
            return;
        };
        let moved = config.controls.remove(from);
        let at = before
            .filter(|before| *before != control)
            .and_then(|before| config.controls.iter().position(|shown| *shown == before))
            .unwrap_or(config.controls.len());
        config.controls.insert(at.min(config.controls.len()), moved);
    })
}

/// Put the panel's controls back as they were at first. Whether the controls changed.
pub(crate) fn reset_controls(config: &mut QuickSettingsConfig) -> bool {
    let mut fresh = QuickSettingsConfig::default();
    std::mem::swap(config, &mut fresh);
    fresh != *config
}

/// `change` applied to `config`, and the controls kept as a panel reads them: duplicates gone,
/// Wi-Fi where every panel starts. Whether it changed.
fn settled(
    config: &mut QuickSettingsConfig,
    change: impl FnOnce(&mut QuickSettingsConfig),
) -> bool {
    let before = config.clone();
    change(config);
    *config = config.clone().normalised();
    *config != before
}

/// The stable name a control is published and asked for by.
pub(crate) fn control_id(control: QuickSettingsControl) -> &'static str {
    match control {
        QuickSettingsControl::Wifi => "wifi",
        QuickSettingsControl::Bluetooth => "bluetooth",
        QuickSettingsControl::Vpn => "vpn",
        QuickSettingsControl::NightLight => "night_light",
        QuickSettingsControl::AirplaneMode => "airplane_mode",
        QuickSettingsControl::MobileData => "mobile_data",
        QuickSettingsControl::DoNotDisturb => "do_not_disturb",
        QuickSettingsControl::RotationLock => "rotation_lock",
        QuickSettingsControl::PowerMode => "power_mode",
    }
}

/// The control a stable name names.
fn control_named(name: &str) -> Option<QuickSettingsControl> {
    QuickSettingsControl::ALL
        .into_iter()
        .find(|control| control_id(*control) == name)
}

impl QuickSettingsService {
    /// The service, holding the person's controls as their settings document has them.
    pub(crate) fn new() -> Self {
        let mut service = Self::default();
        service.follow();
        service
    }

    /// Read the person's controls, and say the resource changed when they are not what was
    /// published.
    fn follow(&mut self) -> bool {
        if !self.settings.changed(settings_store::stamp()) {
            return false;
        }
        let config = QuickSettingsConfig::load().normalised();
        let changed = *self.shown.borrow() != config;
        *self.shown.borrow_mut() = config;
        changed
    }

    /// Carry `action` out on the person's controls, and keep it: whether they changed.
    fn carry(&mut self, action: &LayoutAction) -> Result<bool, tilo_actions::ApiError> {
        let mut config = self.shown.borrow().clone();
        let changed = match action {
            LayoutAction::ShowControl { control } => show_control(&mut config, control),
            LayoutAction::HideControl { control } => hide_control(&mut config, control),
            LayoutAction::MoveControl { control, before } => {
                move_control(&mut config, control, before.as_deref())
            }
            LayoutAction::ResetControls => reset_controls(&mut config),
        };
        if !changed {
            return Ok(false);
        }
        config.save().map_err(|error| {
            tilo_actions::ApiError::Failed(format!(
                "the Quick Settings config was not kept: {error}"
            ))
        })?;
        *self.shown.borrow_mut() = config;
        self.settings.changed(settings_store::stamp());
        self.changed.set(true);
        Ok(true)
    }
}

impl tilo_service::Service for QuickSettingsService {
    fn resources(&self) -> Vec<tilo_actions::ResourceSpec> {
        vec![quick_settings_resource()]
    }

    fn read(&self, resource: &str) -> Option<serde_json::Value> {
        (resource == QUICK_SETTINGS).then(|| {
            tilo_service::reading(&QuickSettingsView {
                controls: self
                    .shown
                    .borrow()
                    .controls
                    .iter()
                    .map(|control| control_id(*control).to_owned())
                    .collect(),
            })
        })
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<tilo_service::Started, tilo_actions::ApiError> {
        let action =
            tilo_service::decode::<LayoutAction>(&self.resources(), resource, action, arguments)?;
        self.carry(&action)?;
        Ok(tilo_service::Started::Done(
            tilo_actions::ActionOutput::default(),
        ))
    }

    fn poll(&mut self) -> bool {
        let changed = self.changed.replace(false);
        changed | self.follow()
    }
}

#[cfg(test)]
mod tests {
    use tilo_service::Service as _;

    use super::*;

    /// Ask the config resource `action` with `arguments`, as the API decodes it.
    fn asked(service: &mut QuickSettingsService, action: &str, arguments: &serde_json::Value) {
        let arguments = arguments.as_object().cloned().unwrap_or_default();
        service
            .invoke(QUICK_SETTINGS, action, &arguments)
            .expect("the config takes it");
    }

    /// The controls the config publishes now.
    fn shown(service: &QuickSettingsService) -> String {
        service
            .read(QUICK_SETTINGS)
            .expect("the config is published")["controls"]
            .to_string()
    }

    tilo_test_env::scenario! {
        fn the_quick_settings_panel_keeps_its_config;

        phase the_config_is_published_by_the_session_and_kept;
        // The config is the person's own setting, so it is read, changed and kept whether the
        // panel is drawn or not, and a control moves in front of the one it was dropped on.
        {
            let _home = tilo_test_env::TestHome::new().expect("a home");
            let mut service = QuickSettingsService::new();
            assert!(shown(&service).contains("bluetooth"), "{}", shown(&service));

            asked(
                &mut service,
                "hide-control",
                &serde_json::json!({ "control": "bluetooth" }),
            );
            assert!(
                !shown(&service).contains("bluetooth"),
                "{}",
                shown(&service)
            );
            asked(
                &mut service,
                "hide-control",
                &serde_json::json!({ "control": "wifi" }),
            );
            assert!(
                shown(&service).contains("wifi"),
                "Wi-Fi stays: {}",
                shown(&service)
            );
            asked(
                &mut service,
                "move-control",
                &serde_json::json!({ "control": "vpn", "before": "wifi" }),
            );
            assert!(
                shown(&service).starts_with(r#"["vpn","wifi""#),
                "the control stands in front of the one it was dropped on: {}",
                shown(&service)
            );
            asked(&mut service, "reset-controls", &serde_json::json!({}));
            assert!(shown(&service).contains("bluetooth"), "{}", shown(&service));

            // Kept: the session reads it back from the person's settings document, as a panel that
            // starts later does.
            let kept = QuickSettingsService::new();
            assert_eq!(shown(&kept), shown(&service));
        }
    }
}
