//! The guided setup's own choices, kept privately on this computer so closing
//! the window and opening it again keeps what the person decided.
//!
//! Only choices live here. A password, a confirmation, a ticket or anything
//! else secret is never written: the format has nowhere to put one.

use imager_backend::record::read_bounded;
use imager_backend::support_report::SupportReport;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use super::machine;
use super::model::{Destination, Journey};
use super::target::Target;

/// What this file's format is. A file of another version is not read.
const SCHEMA: u32 = 1;
/// The whole draft's bound; it is a handful of names and choices.
const MAX_BYTES: usize = 32 * 1024;
/// A hardware report larger than this is left out, as it was before: the
/// person imports the report again rather than the draft carrying a megabyte.
const MAX_REPORT_BYTES: usize = 16 * 1024;

/// The choices written down: none of them authorizes anything, and the
/// installers on either side still validate what they are given.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Choices {
    #[serde(default)]
    schema: u32,
    /// `computer`, `another` or `desktop`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    destination: Option<String>,
    /// Empty when the person kept the suggested name.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    device_name: String,
    /// Empty when the person kept the default purpose.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    purpose: String,
    /// Absent when every plan choice is still the default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    plan: Option<Plan>,
    /// Absent when the other computer is neither named nor described.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    target: Option<TargetChoices>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Plan {
    /// `open` is the choice left for the computer to answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    os: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    name_open: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    disk: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    network: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    unlock: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    updates: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    hosting: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    delivery: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TargetChoices {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    manufacturer: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    model: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    variant: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    board: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    sku: String,
    /// Absent when the person kept this computer's architecture.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    architecture: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    report: Option<SupportReport>,
}

impl Choices {
    /// The choices `journey` and `target` hold now, with the suggestions they
    /// start from left out so a later default change reaches them.
    pub(super) fn current(journey: &Journey, target: &Target, default_name: &str) -> Self {
        let mut choices = Self {
            schema: SCHEMA,
            destination: None,
            device_name: String::new(),
            purpose: String::new(),
            plan: None,
            target: None,
        };
        if let Some(destination) = journey.destination {
            choices.destination = Some(destination.key().to_owned());
        }
        if !journey.device_name.is_empty() && journey.device_name != default_name {
            choices.device_name.clone_from(&journey.device_name);
        }
        if journey.purpose != install_profile_model::default_purpose() {
            choices.purpose.clone_from(&journey.purpose);
        }
        if journey.plan != machine::Choices::default() {
            choices.plan = Some(Plan {
                os: journey.plan.os.map(|os| os.key().to_owned()),
                name_open: journey.plan.name_open,
                disk: journey.plan.disk.map(|disk| disk.key().to_owned()),
                network: journey.plan.network.map(|network| network.key().to_owned()),
                unlock: journey.plan.unlock.map(|unlock| unlock.key().to_owned()),
                updates: journey.plan.updates.map(|updates| updates.key().to_owned()),
                hosting: journey.plan.hosting.map(|hosting| hosting.key().to_owned()),
                delivery: Some(journey.plan.delivery.key().to_owned()),
            });
        }
        let target_choices = TargetChoices::current(target);
        if target_choices != TargetChoices::default() {
            choices.target = Some(target_choices);
        }
        choices
    }

    /// Nothing here differs from what a first opening shows.
    fn is_empty(&self) -> bool {
        self.destination.is_none()
            && self.device_name.is_empty()
            && self.purpose.is_empty()
            && self.plan.is_none()
            && self.target.is_none()
    }

    /// Write this opening's choices onto `journey` and `target`.
    pub(super) fn restore(&self, journey: &mut Journey, target: &mut Target) {
        if let Some(destination) = self.destination.as_deref().and_then(destination) {
            journey.destination = Some(destination);
        }
        if !self.device_name.is_empty() {
            journey.device_name.clone_from(&self.device_name);
        }
        if !self.purpose.is_empty() {
            journey.purpose.clone_from(&self.purpose);
        }
        if let Some(plan) = &self.plan {
            plan.restore(&mut journey.plan, self.device_name.trim().is_empty());
        }
        if let Some(target_choices) = &self.target {
            target.restore(target_choices);
        }
    }
}

impl Plan {
    fn restore(&self, plan: &mut machine::Choices, name_open: bool) {
        plan.os = choice(self.os.as_deref(), machine::Os::from_key);
        plan.disk = choice(self.disk.as_deref(), machine::Disks::from_key);
        plan.network = choice(self.network.as_deref(), machine::Networks::from_key);
        plan.unlock = choice(self.unlock.as_deref(), machine::Unlock::from_key);
        plan.updates = choice(self.updates.as_deref(), machine::Updates::from_key);
        plan.hosting = choice(self.hosting.as_deref(), machine::Hosting::from_key);
        plan.delivery = match self
            .delivery
            .as_deref()
            .and_then(machine::Delivery::from_key)
        {
            Some(delivery) => delivery,
            None => machine::Delivery::Usb,
        };
        plan.name_open = name_open || self.name_open;
    }
}

impl TargetChoices {
    fn current(target: &Target) -> Self {
        let mut choices = Self {
            manufacturer: target.manufacturer(),
            model: target.model(),
            variant: target.variant(),
            board: target.board(),
            sku: target.sku(),
            architecture: String::new(),
            report: target.report().cloned(),
        };
        if target.report().is_none() && target.architecture() != super::target::host_architecture()
        {
            choices.architecture = target.architecture().to_owned();
        }
        if let Some(report) = &choices.report
            && !serde_json::to_vec(report).is_ok_and(|bytes| bytes.len() <= MAX_REPORT_BYTES)
        {
            // The names the report supplied stay; the report itself is
            // imported again when it is needed.
            choices.report = None;
        }
        choices
    }
}

impl Target {
    fn restore(&mut self, choices: &TargetChoices) {
        self.set_fields(
            &choices.manufacturer,
            &choices.model,
            &choices.variant,
            &choices.board,
            &choices.sku,
        );
        if !choices.architecture.is_empty() {
            let _ = self.set_architecture(choices.architecture.clone());
        }
        self.set_report(
            choices
                .report
                .clone()
                .filter(|report| report.validate().is_ok()),
        );
    }
}

const fn is_false(value: &bool) -> bool {
    !*value
}

/// `key`'s choice, absent or `open` meaning the computer answers it later.
fn choice<T>(key: Option<&str>, from_key: impl Fn(&str) -> Option<T>) -> Option<T> {
    match key {
        None | Some("open" | "") => None,
        Some(key) => from_key(key),
    }
}

fn destination(key: &str) -> Option<Destination> {
    match key {
        "computer" => Some(Destination::Computer),
        "another" => Some(Destination::Another),
        "desktop" => Some(Destination::Desktop),
        _ => None,
    }
}

fn path() -> Option<PathBuf> {
    tilo_paths::data_dir().map(|dir| dir.join(crate::resource::APP_ID).join("setup-choices.json"))
}

/// The choices an earlier opening left, if this computer holds readable ones.
pub(super) fn load() -> Option<Choices> {
    let file = std::fs::File::open(path()?).ok()?;
    let bytes = read_bounded(file, MAX_BYTES as u64, || {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "setup choices are too large",
        )
    })
    .ok()?;
    decode(&bytes)
}

/// The choices `bytes` holds, when they are this version's.
fn decode(bytes: &[u8]) -> Option<Choices> {
    let choices: Choices = serde_json::from_slice(bytes).ok()?;
    (choices.schema == SCHEMA).then_some(choices)
}

/// Keep `choices` for the next opening, or remove what an earlier one kept.
pub(super) fn save(choices: &Choices) {
    let Some(path) = path() else {
        return;
    };
    if choices.is_empty() {
        let _ = std::fs::remove_file(path);
        return;
    }
    let Ok(bytes) = serde_json::to_vec(choices) else {
        return;
    };
    let written = path
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| tilo_files::native::atomic::write(&path, &bytes));
    if let Err(error) = written {
        eprintln!("imager: could not keep {}: {error}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_named_computer_and_its_plan_come_back() {
        let mut journey = Journey::new();
        journey.destination = Some(Destination::Another);
        journey.device_name = "cupboard".into();
        journey.plan.os = Some(machine::Os::TiloServer);
        journey.plan.disk = None;
        journey.plan.delivery = machine::Delivery::Netboot;
        let mut target = Target::default();
        target.set_fields("Test Vendor", "Test Laptop", "BOARD", "", "");

        let choices = Choices::current(&journey, &target, "This computer");
        let encoded = serde_json::to_vec(&choices).unwrap();
        let decoded: Choices = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded, choices);

        let mut restored = Journey::new();
        let mut restored_target = Target::default();
        decoded.restore(&mut restored, &mut restored_target);
        assert_eq!(restored.destination, Some(Destination::Another));
        assert_eq!(restored.device_name, "cupboard");
        assert_eq!(restored.plan.os, Some(machine::Os::TiloServer));
        assert_eq!(restored.plan.disk, None, "an open choice stays open");
        assert_eq!(restored.plan.delivery, machine::Delivery::Netboot);
        assert_eq!(restored_target.model(), "Test Laptop");
    }

    #[test]
    fn the_suggested_name_and_default_plan_are_not_written() {
        let mut journey = Journey::new();
        journey.destination = Some(Destination::Computer);
        journey.device_name = "This computer".into();
        let choices = Choices::current(&journey, &Target::default(), "This computer");
        assert!(choices.device_name.is_empty());
        assert!(choices.plan.is_none());
        assert!(choices.target.is_none());
        assert_eq!(choices.destination.as_deref(), Some("computer"));
    }

    #[test]
    fn closing_and_opening_the_setup_keeps_its_choices() {
        use crate::setup::journey::InstallerPreview;
        use crate::setup::journey::InstallerPreviewEffect as Effect;

        let home = tilo_test_env::TestHome::new().unwrap();
        let store = crate::hosting::Store::open();
        let mut preview = InstallerPreview {
            live: true,
            capabilities: guided(),
            port: crate::hosting::Port::new(store, ui::WakeHandle::new()),
            ..InstallerPreview::default()
        };
        preview.restore_choices();
        preview
            .apply(Effect::ChoosePreview {
                choice: "another".into(),
            })
            .unwrap();
        preview.apply(Effect::ContinuePreview).unwrap();
        for (field, value) in [
            ("manufacturer", "Test Vendor"),
            ("model", "Test Laptop"),
            ("variant", "BOARD"),
        ] {
            preview
                .apply(Effect::SetTargetIdentityField {
                    field: field.into(),
                    value: value.into(),
                })
                .unwrap();
        }
        preview.journey.device_name = "cupboard".into();
        preview.persist_choices();
        assert!(draft_file(&home).is_file(), "the choices are kept");

        let mut reopened = InstallerPreview {
            live: true,
            capabilities: guided(),
            port: crate::hosting::Port::new(crate::hosting::Store::open(), ui::WakeHandle::new()),
            ..InstallerPreview::default()
        };
        reopened.restore_choices();
        assert_eq!(reopened.journey.destination, Some(Destination::Another));
        assert_eq!(reopened.journey.device_name, "cupboard");
        assert_eq!(reopened.target.model(), "Test Laptop");
        assert_eq!(
            reopened.journey.page,
            crate::setup::journey::Page::Destination,
            "a reopened setup starts at its first page with the choices filled in"
        );

        // A password and a ticket are never part of what is kept.
        let kept = std::fs::read_to_string(draft_file(&home)).unwrap();
        assert!(!kept.contains("password"), "{kept}");
        assert!(!kept.contains("ticket"), "{kept}");

        // Starting over forgets them.
        reopened.apply(Effect::RestartPreview).unwrap();
        assert!(load().is_none(), "a new start keeps nothing");
    }

    #[test]
    fn a_window_over_memory_keeps_nothing() {
        use crate::setup::journey::InstallerPreview;
        use crate::setup::journey::InstallerPreviewEffect as Effect;

        let _home = tilo_test_env::TestHome::new().unwrap();
        let mut preview = InstallerPreview {
            live: true,
            capabilities: guided(),
            ..InstallerPreview::default()
        };
        preview
            .apply(Effect::ChoosePreview {
                choice: "computer".into(),
            })
            .unwrap();
        assert!(
            load().is_none(),
            "a window whose resource is not kept writes no choices"
        );
    }

    /// A host that offers the guided pages, whatever it is built for.
    fn guided() -> crate::setup::journey::capabilities::GuidedCapabilities {
        crate::setup::journey::capabilities::GuidedCapabilities {
            prepared_installation: true,
        }
    }

    fn draft_file(home: &tilo_test_env::TestHome) -> std::path::PathBuf {
        home.data()
            .join(crate::resource::APP_ID)
            .join("setup-choices.json")
    }

    #[test]
    fn a_file_of_another_version_or_with_unknown_fields_is_not_read() {
        assert!(decode(br#"{"schema":1,"destination":"computer"}"#).is_some());
        assert!(
            decode(br#"{"schema":2,"destination":"computer"}"#).is_none(),
            "another version's file is not read"
        );
        assert!(
            decode(br#"{"schema":1,"destination":"computer","password":"secret"}"#).is_none(),
            "a file with a field the choices do not have is not read"
        );
    }
}
