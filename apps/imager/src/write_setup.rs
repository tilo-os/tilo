//! On-write setup collected on the Confirm step.
//!
//! Installable media embeds a secret-free media profile on Customize. Separately,
//! this machine can join the target to Wi-Fi and certify a new device for the
//! person already here. The passphrase stays host-local; the collaborative
//! profile only records the network name, device label, and whether to enroll.

use std::rc::Rc;

use ui::local_state::LocalState;
use ui::widget::{Field, Form, FormSection, TextInput, TextInputKind, Toggle};

use crate::actions::ImagerAction;
use crate::app::{Command, Commands};
use crate::model::InstallProfile;

/// What enrolling does, under its switch.
const ENROLL_NOTE: &str = "Certifies the new device. The root secret stays on this computer.";

/// One text field of the network and identity section.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetupField {
    WifiSsid,
    WifiPsk,
    SshKey,
    DeviceLabel,
}

impl SetupField {
    fn label(self) -> &'static str {
        match self {
            Self::WifiSsid => "Wi-Fi network",
            Self::WifiPsk => "Wi-Fi password",
            Self::SshKey => "Owner SSH public key",
            Self::DeviceLabel => "Device name",
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::WifiSsid => "imager.write-setup.wifi-ssid",
            Self::WifiPsk => "imager.write-setup.wifi-psk",
            Self::SshKey => "imager.write-setup.ssh-key",
            Self::DeviceLabel => "imager.write-setup.device-label",
        }
    }
}

/// The profile action a changed field dispatches; `None` for the host-local
/// passphrase, which never enters the document.
pub(crate) fn field_action(field: SetupField, value: String) -> Option<ImagerAction> {
    match field {
        SetupField::WifiSsid => Some(ImagerAction::SetWifiSsid { value }),
        SetupField::DeviceLabel => Some(ImagerAction::SetDeviceLabel { value }),
        SetupField::WifiPsk | SetupField::SshKey => None,
    }
}

fn text_field(field: SetupField, value: &str, kind: TextInputKind, commands: &Commands) -> Field {
    let sink = Rc::clone(commands);
    let mut input = TextInput::local()
        .with_id(field.id())
        .with_kind(kind)
        .with_initial_value(value)
        .on_change(move |value| {
            sink.borrow_mut()
                .push(Command::WriteSetup(field, value.to_string()));
        });
    if field == SetupField::WifiPsk {
        input = input.with_secret(true);
    }
    Field::new(field.label(), input)
}

/// The network and identity section for `profile`, with the host-local
/// passphrase `wifi_psk`.
pub(crate) fn form(
    profile: &InstallProfile,
    wifi_psk: &str,
    ssh_public_key: &str,
    commands: &Commands,
) -> Form {
    let sink = Rc::clone(commands);
    let enroll = Toggle::from_state(&LocalState::new(profile.enroll_local_identity))
        .with_id("imager.write-setup.enroll")
        .on_change(move |on| sink.borrow_mut().push(Command::Enroll(on)));
    Form::new().section(
        FormSection::new("Network and identity")
            .field(text_field(
                SetupField::WifiSsid,
                &profile.wifi_ssid,
                TextInputKind::Name,
                commands,
            ))
            .field(text_field(
                SetupField::WifiPsk,
                wifi_psk,
                TextInputKind::Password,
                commands,
            ))
            .field(text_field(
                SetupField::SshKey,
                ssh_public_key,
                TextInputKind::Name,
                commands,
            ))
            .field(text_field(
                SetupField::DeviceLabel,
                &profile.device_label,
                TextInputKind::Name,
                commands,
            ))
            .field(Field::new("Enroll as a new device", enroll).with_help(ENROLL_NOTE)),
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
                let commands = Rc::new(std::cell::RefCell::new(Vec::new()));
                let profile = InstallProfile::default();
                let panel = form(&profile, "", "", &commands);
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
