//! The configuration's verbs and the places derived from it, and what each
//! device applied: starting, stopping and panic mode, deleting rules, and taking
//! in what a device's firewalld reported.

use std::collections::BTreeMap;

use tilo_resource::{self, Availability, Derived, Id, Invoke, Refusal, SearchEntry, resource};

use crate::model::{Firewall, FirewallConfig, ZoneKind};

impl Firewall {
    /// What the resource holds: the configuration's zones and rules, the active zone.
    ///
    /// # Errors
    /// When the fields cannot be read from the document.
    pub fn read_document(
        &self,
        item: Option<&tilo_resource::Id>,
    ) -> Result<serde_json::Value, String> {
        tilo_resource::read::fields(self, item)
    }
}

#[resource(
    kind = "firewall.config",
    available = Firewall::available,
    children = Firewall::zone_places,
    search = Firewall::search,
    version = 3,
    migrate = crate::model::upgrade,
    read_content = Firewall::read_document
)]
impl Firewall {
    /// Start firewall.
    #[action]
    pub fn start(&mut self) {
        self.enabled = true;
    }

    /// Stop firewall.
    ///
    /// Undo starts it again but cannot take back what reached the machine while it was
    /// open, so this is asked first.
    #[action(destructive, irreversible)]
    pub fn stop(&mut self) {
        self.enabled = false;
    }

    /// Enter panic mode: drop all traffic in and out.
    ///
    /// Undo lifts it but cannot restore the connections it cut, so this is asked first.
    #[action(destructive, irreversible)]
    pub fn enter_panic_mode(&mut self) {
        self.panic_mode = true;
    }

    /// Exit panic mode.
    #[action]
    pub fn exit_panic_mode(&mut self) {
        self.panic_mode = false;
    }

    /// Delete a rule from a zone.
    #[action]
    pub fn delete_rule(
        &mut self,
        #[param("The zone")] zone: ZoneKind,
        #[param("The rule's id")] rule: Id,
    ) -> Result<(), Refusal> {
        let stored = self
            .zones
            .get_mut(zone.name())
            .ok_or("there is no such zone")?;
        stored.rules.remove(&rule).ok_or("there is no such rule")?;
        stored.deleted_rules.insert(rule.as_str().to_owned());
        Ok(())
    }

    /// Take in the configuration firewalld reported on a device.
    #[action(external)]
    pub fn import(
        &mut self,
        #[param("The configuration, as firewalld reported it")] config: serde_json::Value,
    ) -> Result<(), Refusal> {
        let config: FirewallConfig = serde_json::from_value(config)
            .map_err(|error| Refusal::new(format!("not a firewall configuration: {error}")))?;
        self.take_in(&config);
        Ok(())
    }
}

impl Firewall {
    /// The import of `config`, as a device that read it submits it.
    #[must_use]
    pub fn import_of(config: &FirewallConfig) -> Invoke<Self> {
        Self::IMPORT.with(serde_json::to_value(config).unwrap_or_default())
    }

    /// Start, stop and panic mode apply to a configured firewall in the other state.
    fn available(&self, verb: firewall::Verb) -> Availability {
        let unavailable = |code: &str, reason: &str| Availability::Unavailable {
            code: code.into(),
            reason: reason.into(),
        };
        let switch = matches!(
            verb,
            firewall::Verb::Start
                | firewall::Verb::Stop
                | firewall::Verb::EnterPanicMode
                | firewall::Verb::ExitPanicMode
        );
        if switch && self.zones.is_empty() {
            return unavailable("unconfigured", "There is no firewall configuration yet");
        }
        match verb {
            firewall::Verb::Start if self.enabled => {
                unavailable("running", "The firewall is running")
            }
            firewall::Verb::Stop if !self.enabled => {
                unavailable("stopped", "The firewall is stopped")
            }
            firewall::Verb::EnterPanicMode if self.panic_mode => {
                unavailable("panic", "Panic mode is on")
            }
            firewall::Verb::ExitPanicMode if !self.panic_mode => {
                unavailable("calm", "Panic mode is off")
            }
            _ => Availability::Available,
        }
    }

    /// Each configured zone, which deletes its own rules.
    fn zone_places(&self) -> Vec<Derived<Self>> {
        self.config()
            .zones
            .iter()
            .map(|zone| {
                Derived::new(zone.kind.title())
                    .of_kind("zone")
                    .identified(zone.kind.name())
                    .with(Self::DELETE_RULE.bind(firewall::DeleteRule::paths().zone, zone.kind))
            })
            .collect()
    }

    fn search(&self) -> Vec<SearchEntry> {
        self.config()
            .zones
            .iter()
            .map(|zone| {
                SearchEntry::new(
                    zone.kind.title(),
                    tilo_resource::word!("firewall-zone-rules", count = zone.rules.len()).read(),
                )
                .anchored(format!("firewall.zone:{}", zone.kind.name()))
            })
            .collect()
    }
}

/// What each device last pushed to its firewalld, so a launch applies a configuration once
/// rather than again.
#[derive(tilo_resource::Model, Clone, Debug, Default, PartialEq)]
pub struct AppliedConfig {
    /// The applied configuration in canonical JSON, by device.
    pub devices: BTreeMap<String, String>,
}

impl AppliedConfig {
    /// What the resource holds: what each device last pushed to, or read from, its firewalld.
    ///
    /// # Errors
    /// When the fields cannot be read from the document.
    pub fn read_document(
        &self,
        item: Option<&tilo_resource::Id>,
    ) -> Result<serde_json::Value, String> {
        tilo_resource::read::fields(self, item)
    }
}

#[resource(
    kind = "firewall.applied",
    read_content = AppliedConfig::read_document
)]
impl AppliedConfig {
    /// `device`'s firewalld holds `config`, in canonical JSON.
    #[action(housekeeping)]
    pub fn applied(&mut self, device: String, config: String) {
        self.devices.insert(device, config);
    }
}

impl AppliedConfig {
    /// What `device` applied last, in canonical JSON.
    #[must_use]
    pub fn on(&self, device: &str) -> Option<&str> {
        self.devices.get(device).map(String::as_str)
    }
}
