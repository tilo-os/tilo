//! This machine's keyboard published as a view draws it: the layouts it installs, and the keys
//! of the layout being inspected — the one the pane previews, or the one the keyboard types —
//! with what each key types at every modifier level.
//!
//! The settings choose what is inspected ([`InputDevicePrefs::keyboard_preview`], and the
//! keyboard's model), so this reading follows the settings document and compiles a layout only
//! when what it draws changes. The compiling is the device's: the layout's own symbols are read
//! by the native XKB compiler, and their legends are spelled the way the layout's language
//! spells them.

use serde_json::{Map, Value};
use settings_sections::prefs::InputDevicePrefs;
use tilo_actions::{ActionAddress, ApiError, ResourceSpec};
use tilo_resource::NoEffect;
use tilo_service::{Service, Started, offer, reading};

pub(crate) const RESOURCE: &str = "session:keyboard";

/// This machine's keyboard as the session publishes it.
#[derive(tilo_resource::Row)]
#[row(kind = "session.keyboard", label = "Keyboard")]
pub(crate) struct KeyboardRoot;

pub(crate) use tilo_device_surfaces::keyboard::{
    Keyboard, KeyboardDrawing, KeyboardKey, KeyboardLayout, KeyboardPoint,
};

/// The keyboard as the session serves it: the layouts installed, read once, and the drawing of
/// the layout the settings name, compiled again only when what it draws changes.
pub(crate) struct Served {
    published: Keyboard,
    /// The drawing's inputs when it was last compiled, so a settings change that leaves the
    /// drawing as it was does not compile a layout again.
    drawn_at: Option<(String, String, String, bool)>,
    /// The settings document when this last read them.
    settings: settings_store::SettingsFollower,
    /// This machine's input devices, kept current by a worker: which keyboard types is read from
    /// them.
    inputs: crate::input_devices::Inputs,
}

impl Served {
    pub(crate) fn new(inputs: crate::input_devices::Inputs) -> Self {
        let (layouts, error) = installed_layouts();
        Self {
            published: Keyboard {
                layouts,
                layouts_error: error,
                drawing: None,
                drawing_error: None,
            },
            drawn_at: None,
            settings: settings_store::SettingsFollower::default(),
            inputs,
        }
    }

    /// The layout the settings inspect: the previewed one, or the one the keyboard types, with
    /// the physical model and the XKB options it compiles with.
    fn wanted(&self) -> (String, String, String, bool) {
        let device: InputDevicePrefs = match serde_json::from_value(Value::Object(
            settings_store::load_section(settings_store::hardware::INPUT_DEVICE_SECTION),
        )) {
            Ok(device) => device,
            Err(error) => {
                tilo_failure::ignored!(
                    "a settings document this reading does not know draws the keyboard as the defaults",
                    tracing::warn!(%error, "the input device settings do not read")
                );
                InputDevicePrefs::default()
            }
        };
        let input = settings_store::hardware::RichInputConfig::load();
        // The keyboard the scope names, as the worker read this machine's input devices: the
        // reading never opens the compositor's file on the loop.
        let current = settings_store::keyboard_sources::effective(&input, &self.inputs.scope());
        let model = if settings_sections::prefs::valid_keyboard_model(&device.keyboard_model) {
            device.keyboard_model.clone()
        } else {
            "pc105".to_owned()
        };
        let source = device
            .keyboard_preview
            .unwrap_or_else(|| current.keyboard_layout.clone());
        (
            source,
            model,
            current.xkb_options().join(","),
            chassis_is_compact(),
        )
    }

    /// Draw the wanted layout, unless the same one is drawn already.
    fn draw(&mut self, wanted: &(String, String, String, bool)) {
        if self.drawn_at.as_ref() == Some(wanted) {
            return;
        }
        let (source, model, options, compact) = wanted;
        let drawn = drawing(source, model, options, *compact);
        self.drawn_at = Some(wanted.clone());
        match drawn {
            Ok(drawing) => {
                self.published.drawing = Some(drawing);
                self.published.drawing_error = None;
            }
            Err(error) => {
                self.published.drawing = None;
                self.published.drawing_error = Some(error);
            }
        }
    }
}

/// The layouts this machine installs, and the input-method engines beside them that are
/// installed too: what a person can type with.
fn installed_layouts() -> (Vec<KeyboardLayout>, Option<String>) {
    use settings_store::hardware::InputMethodEngine;

    let (layouts, error) = tilo_keyboard_map::installed_layouts();
    let mut layouts: Vec<KeyboardLayout> = layouts
        .iter()
        .map(|layout| KeyboardLayout {
            id: layout.id.clone(),
            label: layout.label.clone(),
        })
        .collect();
    layouts.extend(
        InputMethodEngine::ALL
            .into_iter()
            .filter(|engine| engine.is_installed())
            .map(|engine| KeyboardLayout {
                id: engine.source_id(),
                label: engine.label().to_owned(),
            }),
    );
    (layouts, error)
}

/// The keys of `source` as `model` lays them out, as this resource publishes them.
fn drawing(
    source: &str,
    model: &str,
    options: &str,
    compact: bool,
) -> Result<KeyboardDrawing, String> {
    let drawn = tilo_keyboard_map::draw(source, model, options, compact)?;
    Ok(KeyboardDrawing {
        layout: drawn.layout,
        model: drawn.model,
        width: drawn.width,
        height: drawn.height,
        level_count: drawn.level_count,
        keys: drawn
            .keys
            .into_iter()
            .map(|key| KeyboardKey {
                name: key.name,
                outline: key
                    .outline
                    .into_iter()
                    .map(|point| KeyboardPoint {
                        x: point.x,
                        y: point.y,
                    })
                    .collect(),
                levels: key.levels,
            })
            .collect(),
    })
}

/// Whether this machine is a laptop or another compact one, whose drawing leaves out the
/// navigation cluster and the numpad. A machine that does not say is compact.
fn chassis_is_compact() -> bool {
    static COMPACT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *COMPACT.get_or_init(|| {
        let chassis = match std::fs::read_to_string("/sys/class/dmi/id/chassis_type") {
            Ok(text) => match text.trim().parse::<u32>() {
                Ok(chassis) => Some(chassis),
                Err(error) => {
                    tilo_failure::ignored!(
                        "a chassis type that is not a number is drawn as a compact machine's",
                        tracing::debug!(%error, "the chassis type does not read as a number")
                    );
                    None
                }
            },
            Err(error) => {
                tilo_failure::ignored!(
                    "a machine that publishes no chassis type is drawn as a compact one",
                    tracing::debug!(%error, "no chassis type to read")
                );
                None
            }
        };
        compact_chassis(chassis)
    })
}

fn compact_chassis(chassis: Option<u32>) -> bool {
    // 3–7 are desktops, towers and racks; 13, 15–24 are the desktop's other bodies.
    !matches!(chassis, Some(3..=7 | 13 | 15..=24))
}

impl Service for Served {
    fn resources(&self) -> Vec<ResourceSpec> {
        vec![offer::<NoEffect>(
            tilo_resource::LiveRow::typed::<KeyboardRoot>(
                RESOURCE,
                crate::i18n::t("session-keyboard"),
            )
            .into_resource(),
        )]
    }

    fn read(&self, resource: &str) -> Option<Value> {
        (resource == RESOURCE).then(|| reading(&self.published))
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        _arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
        Err(ApiError::Unknown(ActionAddress::new(resource, action)))
    }

    fn poll(&mut self) -> bool {
        let stamp = settings_store::stamp();
        if !self.settings.changed(stamp) {
            return false;
        }
        let before = self.published.drawing.clone();
        let error = self.published.drawing_error.clone();
        let wanted = self.wanted();
        self.draw(&wanted);
        self.published.drawing != before || self.published.drawing_error != error
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A laptop's keyboard is drawn compact and a desktop's full, and a machine that does not
    /// say is a laptop.
    #[test]
    fn the_machines_body_decides_which_drawing_it_gets() {
        for desktop in [
            Some(3),
            Some(5),
            Some(6),
            Some(7),
            Some(13),
            Some(16),
            Some(23),
        ] {
            assert!(!compact_chassis(desktop), "{desktop:?} is a desktop");
        }
        for compact in [Some(8), Some(9), Some(10), Some(14), Some(30), None] {
            assert!(compact_chassis(compact), "{compact:?} is compact");
        }
    }
}
