//! The setup collected on the Confirm step and written with the image.

use ui::local_state::LocalState;
use ui::widget::{Field, Form, FormSection, TextInput, TextInputKind, Toggle};

use crate::hosting::{Presses, Store};
use crate::model::InstallProfile;
use crate::resource::{Imager, ImagerEffect};

const ENROLL_NOTE: &str = "imager-write-setup-enroll-note";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetupField {
    WifiSsid,
    WifiPsk,
    SshKey,
    DeviceLabel,
}

impl SetupField {
    fn message(self) -> &'static str {
        match self {
            Self::WifiSsid => "imager-write-setup-wifi-ssid",
            Self::WifiPsk => "imager-write-setup-wifi-psk",
            Self::SshKey => "imager-write-setup-ssh-key",
            Self::DeviceLabel => "imager-write-setup-device-label",
        }
    }

    fn label(self) -> String {
        crate::resource::CATALOG.t(self.message())
    }

    fn id(self) -> &'static str {
        match self {
            Self::WifiSsid => "imager.write-setup.wifi-ssid",
            Self::WifiPsk => "imager.write-setup.wifi-psk",
            Self::SshKey => "imager.write-setup.ssh-key",
            Self::DeviceLabel => "imager.write-setup.device-label",
        }
    }

    /// What typing `value` into the field does: edit the profile, or hand the Wi-Fi password to
    /// the window, which keeps it out of the resource.
    fn typed(self, value: &str, bound: &Bound) {
        let profile = Imager::paths().profile;
        let edited = match self {
            Self::WifiSsid => bound
                .store
                .edit(&profile.wifi_ssid.replace(value.to_owned())),
            Self::DeviceLabel => bound
                .store
                .edit(&profile.device_label.replace(value.to_owned())),
            Self::WifiPsk => {
                bound.presses.press(ImagerEffect::SetWifiPsk {
                    value: value.to_owned(),
                });
                Ok(())
            }
            Self::SshKey => Ok(()),
        };
        if let Err(reason) = edited {
            ui::control_notice::say_in_window(&reason);
        }
    }
}

/// What the form's fields change: the resource's profile, and the window's own effects.
#[derive(Clone)]
pub(crate) struct Bound {
    pub store: Store,
    pub presses: Presses<ImagerEffect>,
}

fn text_field(
    field: SetupField,
    value: &str,
    kind: TextInputKind,
    bound: Option<&Bound>,
    ssh_public_key: &LocalState<String>,
) -> Field {
    let mut input = if field == SetupField::SshKey {
        TextInput::from_state(ssh_public_key)
    } else {
        TextInput::local().with_initial_value(value)
    }
    .with_id(field.id())
    .with_kind(kind);
    if let Some(bound) = bound.filter(|_| field != SetupField::SshKey) {
        let bound = bound.clone();
        input = input.on_change(move |value| field.typed(value, &bound));
    }
    if field == SetupField::WifiPsk {
        input = input.with_secret(true);
    }
    Field::new(field.label(), input)
}

pub(crate) fn form(
    profile: &InstallProfile,
    wifi_psk: &str,
    ssh_public_key: &LocalState<String>,
    bound: Option<&Bound>,
) -> Form {
    let mut enroll = Toggle::from_state(&LocalState::new(profile.enroll_local_identity))
        .with_id("imager.write-setup.enroll");
    if let Some(bound) = bound {
        let store = bound.store.clone();
        enroll = enroll.on_choose("imager.write-setup.enroll", move |enrolled| {
            let edit = Imager::paths().profile.enroll_local_identity.set(enrolled);
            if let Err(reason) = store.edit(&edit) {
                ui::control_notice::say_in_window(&reason);
            }
        });
    }
    Form::new().section(
        FormSection::new(crate::resource::CATALOG.t("imager-profile-section-network-identity"))
            .field(text_field(
                SetupField::WifiSsid,
                &profile.wifi_ssid,
                TextInputKind::Name,
                bound,
                ssh_public_key,
            ))
            .field(text_field(
                SetupField::WifiPsk,
                wifi_psk,
                TextInputKind::Password,
                bound,
                ssh_public_key,
            ))
            .field(text_field(
                SetupField::SshKey,
                "",
                TextInputKind::Name,
                bound,
                ssh_public_key,
            ))
            .field(text_field(
                SetupField::DeviceLabel,
                &profile.device_label,
                TextInputKind::Name,
                bound,
                ssh_public_key,
            ))
            .field(
                Field::new(
                    crate::resource::CATALOG.t("imager-write-setup-enroll"),
                    enroll,
                )
                .with_help(crate::resource::CATALOG.t(ENROLL_NOTE)),
            ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_form_stacks_write_setup_fields_below_its_caption() {
        for theme in [
            ui::Theme::light(),
            ui::Theme::dark(),
            ui::Theme::high_contrast(),
        ] {
            for width in [400, 760] {
                let ssh_key = LocalState::new(String::new());
                let profile = InstallProfile::default();
                let panel = form(&profile, "", &ssh_key, None);
                let view = ui::headless::FrameHarness::for_root(
                    ui::State::new(),
                    panel,
                    (width, 900),
                    theme.clone(),
                );
                let targets = view.targets();
                let caption = view
                    .a11y_nodes()
                    .into_iter()
                    .find(|(_, node)| {
                        node.role() == ui::accesskit::Role::Heading
                            && node.label() == Some("Network and identity")
                    })
                    .and_then(|(_, node)| node.bounds())
                    .expect("the production shared-Form caption has actual bounds");
                let ssid = targets["imager.write-setup.wifi-ssid"];
                let password = targets["imager.write-setup.wifi-psk"];
                let ssh_key = targets["imager.write-setup.ssh-key"];
                let device = targets["imager.write-setup.device-label"];
                let enroll = targets["imager.write-setup.enroll"];
                assert!(ssid.y0 > caption.y1, "network field follows the caption");
                assert!(password.y0 > ssid.y1, "password follows the network field");
                assert!(ssh_key.y0 > password.y1, "owner key follows the password");
                assert!(device.y0 > ssh_key.y1, "device name follows the owner key");
                assert!(enroll.y0 > device.y1, "enrollment follows the device name");
                assert!(
                    enroll.x1 <= f64::from(width) + 1e-6,
                    "the real enrollment control stays inside the content width"
                );
            }
        }
    }
}
