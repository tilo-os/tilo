//! Firewall as a resource: the stored configuration ([`Firewall`]) is the
//! shared document, and firewalld on each device is its projection.
//!
//! A zone's policy, members, rules and the active zone are field edits at
//! their paths; starting, stopping and panic mode are verbs, the switches a
//! running firewall is turned with; deleting a rule is a verb
//! because the deletion must win over a concurrent edit; taking in what
//! firewalld reported is the `import` verb. After every change, each device
//! running the app applies the stored configuration to its own firewalld (a
//! job of its window, the latest configuration winning); what each device
//! applied last is in a record beside it ([`AppliedConfig`]), so an
//! unchanged configuration is not pushed again.

use std::collections::BTreeMap;

use ui::framework::{PersonRecord, ResourceApp, SeedCx, ViewCx};
use ui::prelude::Effect;
use ui::tilo_resource::{self, Availability, Derived, Id, Invoke, Refusal, SearchEntry, resource};
use ui::widget::Element;

use crate::app::{Notice, Tone};
use crate::backend::Backend;
pub use crate::model::Firewall;
use crate::model::{FirewallConfig, ZoneKind};
use ui::capability_door::Door;

#[resource(
    kind = "firewall.config",
    label = "Firewall",
    available = Firewall::available,
    children = Firewall::zone_places,
    search = Firewall::search,
    version = 3,
    migrate = crate::model::upgrade,
)]
impl Firewall {
    /// Start firewall.
    #[action]
    pub fn start(&mut self) {
        self.enabled = true;
    }

    /// Stop firewall.
    ///
    /// Undo starts it again but cannot take back what reached the machine
    /// while it was open, so this is asked first.
    #[action(destructive, irreversible)]
    pub fn stop(&mut self) {
        self.enabled = false;
    }

    /// Enter panic mode: drop all traffic in and out.
    ///
    /// Undo lifts it but cannot restore the connections it cut, so this is
    /// asked first.
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
    pub fn import_of(config: &FirewallConfig) -> Invoke<Self> {
        Self::IMPORT.with(serde_json::to_value(config).unwrap_or_default())
    }

    /// Start, stop and panic mode apply to a configured firewall in the
    /// other state.
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
                SearchEntry::new(zone.kind.title(), format!("{} rules", zone.rules.len()))
                    .anchored(format!("firewall.zone:{}", zone.kind.name()))
            })
            .collect()
    }
}

/// What the firewall does outside the document, for every caller.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "firewall.effect")]
pub enum FirewallEffect {
    /// Apply firewall configuration.
    #[effect(job)]
    ApplyConfig,
    /// Retry firewalld connection.
    #[effect(job)]
    Retry,
    /// Read dropped traffic from the system journal.
    #[effect(kind = "read")]
    ReadBlocked,
}

/// What each device last pushed to (or read from) its firewalld, by device,
/// so the projection applies a configuration once rather than on every
/// launch: a record Firewall keeps beside its own in the workspace, each
/// device reading and writing only its own entry, since another device's
/// firewalld is its own.
#[derive(tilo_resource::Model, Clone, Debug, Default, PartialEq)]
pub struct AppliedConfig {
    /// The applied configuration in canonical JSON, by device.
    pub devices: BTreeMap<String, String>,
}

#[resource(kind = "firewall.applied", label = "Applied configuration")]
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

/// The [`AppliedConfig`] record and this device's id.
pub(crate) type AppliedOnDevice = (PersonRecord<AppliedConfig>, String);

/// The stored configuration still to apply on a device that last applied
/// `applied`, keyed by its canonical form: nothing while there is none,
/// while it is invalid, or once the device applied exactly it.
pub(crate) fn pending_apply(
    config: &FirewallConfig,
    applied: Option<&str>,
) -> Vec<(String, FirewallConfig)> {
    if config.zones.is_empty() || config.validate().is_err() {
        return Vec::new();
    }
    let canonical = config.to_json();
    if applied == Some(canonical.as_str()) {
        return Vec::new();
    }
    vec![(canonical, config.clone())]
}

/// How one apply ended: how many commands ran, or why it failed.
pub(crate) type Applied = Result<usize, String>;

/// Push `config` to this device's firewalld. Only a granted capability may
/// apply: the app passes its door, and no other caller can slip past it.
/// Blocking: runs off the UI thread.
pub(crate) fn apply_on_device(config: &FirewallConfig, door: &Door, allow_host: bool) -> Applied {
    if !door.is_granted() {
        return Err(format!(
            "changing firewall rules needs the {} capability, which is not granted",
            door.capability()
        ));
    }
    let mut backend = Backend::new();
    // A unit test must never change the host it runs on. The plan is the
    // same data the product path would execute.
    if allow_host && !cfg!(test) {
        // A firewalld that holds the configuration already, as the one it
        // was imported from does, is not given it again.
        if crate::backend::import().map(|held| held.canonical().to_json())
            == Some(config.canonical().to_json())
        {
            return Ok(0);
        }
        backend.apply(config)?;
    } else {
        backend.plan_only(config)?;
    }
    Ok(backend.last_plan().len())
}

/// What one apply reports.
pub(crate) fn applied_status(applied: &Applied) -> Notice {
    let changes = |n: usize| if n == 1 { "change" } else { "changes" };
    match applied {
        Ok(n) => Notice {
            text: format!("Applied {n} {}", changes(*n)),
            tone: Tone::Success,
        },
        Err(error) => Notice {
            text: format!("Could not apply the configuration: {error}"),
            tone: Tone::Error,
        },
    }
}

/// Remember that this device's firewalld holds `config`.
pub(crate) fn remember_applied((record, device): &AppliedOnDevice, config: &FirewallConfig) {
    let applied = AppliedConfig::APPLIED.with((device.clone(), config.to_json()));
    if let Err(error) = record.invoke(&applied) {
        tracing::warn!(%error, "could not remember the applied firewall configuration");
    }
}

/// Read this device's firewalld; `Err` says why it cannot be shown.
pub(crate) fn read_device() -> Result<FirewallConfig, String> {
    match crate::backend::import() {
        Some(config) if config.validate().is_ok() => Ok(config),
        Some(_) => {
            Err("Firewalld returned rules this app cannot show safely. Nothing was changed.".into())
        }
        None => Err("Firewalld is still not reachable. Start the service, then try again.".into()),
    }
}

/// Framework app: the firewall widget over [`Firewall`].
pub struct FirewallShell;

/// Every screenshot scenario. All but `service-error` show the sample
/// ruleset; the rest is view state the view stages.
#[cfg(feature = "samples")]
const SCENARIOS: &[&str] = &[
    "empty",
    "add-rule",
    "error",
    "confirmation",
    "active-change",
    "panic",
    "stopped",
    "service-error",
    "services",
    "blocked",
    "selected",
    "switch-confirmation",
    "stop-confirmation",
    "narrow",
    "drawer",
];

impl ResourceApp for FirewallShell {
    type Resource = Firewall;
    const ID: &'static str = "so.tilo.firewall";
    const READS_HOST: &'static [ui::launch::HostSource] = &[ui::launch::HostSource::Network];
    const TITLE: &'static str = "Firewall";

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&crate::locales::CATALOG)
    }

    const ICON: Option<&'static str> = Some("firewall");

    fn view(cx: &mut ViewCx<Firewall>) -> Element {
        crate::app::window(cx)
    }

    #[cfg(feature = "samples")]
    fn scenarios() -> &'static [&'static str] {
        SCENARIOS
    }

    fn shot_viewport(scenario: Option<&str>) -> (u32, u32) {
        scenario
            .and_then(|name| match name {
                "narrow" => Some((800, 600)),
                "drawer" => Some((640, 600)),
                _ => None,
            })
            .unwrap_or((1280, 800))
    }

    /// The bundled sample ruleset, never the capturing machine's; the
    /// active-change scenario has just made `home` the active zone, and
    /// the panic and stopped scenarios have turned those switches.
    #[cfg(feature = "samples")]
    fn scenario(name: Option<&str>) -> Vec<Invoke<Firewall>> {
        if name == Some("service-error") {
            return Vec::new();
        }
        let mut invokes = vec![Firewall::import_of(&crate::sample::sample_config())];
        match name {
            Some("active-change") => invokes
                .extend(Invoke::edit(&Firewall::paths().default_zone.set(ZoneKind::Home)).ok()),
            Some("panic") => invokes.push(Firewall::ENTER_PANIC_MODE.invoke()),
            Some("stopped") => invokes.push(Firewall::STOP.invoke()),
            _ => {}
        }
        invokes
    }

    /// A new record starts with what firewalld on the creating device
    /// already has, which that device therefore does not push again
    /// ([`apply_on_device`]). A device without a readable firewalld starts
    /// empty and offers a retry, and so does a record created without a
    /// window: it is no device's.
    fn seed(cx: &SeedCx<'_>) -> Vec<Invoke<Firewall>> {
        if cx.host().is_none_or(|host| host.capture().is_some()) {
            return Vec::new();
        }
        let Ok(config) = read_device() else {
            return Vec::new();
        };
        vec![Firewall::import_of(&config)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sample::sample_config;
    use ui::prelude::TestApp;

    fn action<'a>(surface: &'a serde_json::Value, id: &str) -> Option<&'a serde_json::Value> {
        surface
            .as_array()?
            .iter()
            .find(|row| row["resource"] == "firewall.config" && row["action"] == id)
    }

    /// A configuration from before a rule's match was stored as its variant
    /// and fields: each match was one JSON string. Opened now, every rule
    /// matches what it matched.
    #[test]
    fn rules_kept_with_json_matches_migrate_to_their_variants() {
        use ui::tilo_resource::{Replica, Slot, evolution, read_key};
        let mut replica = Replica::<Firewall>::new();
        replica
            .apply(Firewall::import_of(&sample_config()))
            .unwrap();
        let expected = replica.state().unwrap();
        let doc = replica.doc_mut();
        let root = ui::automerge::ROOT;
        for (_, zone) in evolution::map_values(doc, &root, "zones").unwrap() {
            for rule in evolution::collection_items(doc, &zone, "rules").unwrap() {
                let matcher: crate::model::Matcher = read_key(doc, &rule, "matcher").unwrap();
                let slot = Slot::Key(&rule, "matcher");
                slot.remove(doc).unwrap();
                slot.put_scalar(
                    doc,
                    ui::automerge::ScalarValue::Str(
                        serde_json::to_string(&matcher).unwrap().into(),
                    ),
                )
                .unwrap();
            }
        }
        Slot::Key(&root, evolution::VERSION_KEY)
            .remove(doc)
            .unwrap();
        doc.commit();
        let mut old = doc.fork();
        assert!(
            evolution::read_checked::<Firewall>(&old).is_err(),
            "unmigrated"
        );
        assert!(evolution::migrate::<Firewall>(&mut old).unwrap());
        assert_eq!(evolution::read_checked::<Firewall>(&old).unwrap(), expected);
    }

    /// A configuration of version 2 stored each rule's match with its fields
    /// flat beside its variant's name. Opened now, every rule matches what it
    /// matched, its fields stored by its variant.
    #[test]
    fn rules_kept_with_flat_matches_migrate_to_fields_stored_by_variant() {
        use ui::automerge::{ObjType, ReadDoc as _, ScalarValue};
        use ui::tilo_resource::{Model as _, Replica, Slot, evolution, read_key};
        let mut replica = Replica::<Firewall>::new();
        replica
            .apply(Firewall::import_of(&sample_config()))
            .unwrap();
        let expected = replica.state().unwrap();
        let doc = replica.doc_mut();
        let root = ui::automerge::ROOT;
        let mut flattened = Vec::new();
        for (_, zone) in evolution::map_values(doc, &root, "zones").unwrap() {
            for rule in evolution::collection_items(doc, &zone, "rules").unwrap() {
                let matcher: crate::model::Matcher = read_key(doc, &rule, "matcher").unwrap();
                Slot::Key(&rule, "matcher").remove(doc).unwrap();
                let map = Slot::Key(&rule, "matcher")
                    .object(doc, ObjType::Map)
                    .unwrap();
                evolution::fixture(doc, &map, &matcher.to_json()).unwrap();
                flattened.push(rule);
            }
        }
        assert!(!flattened.is_empty(), "the sample has rules to migrate");
        Slot::Key(&root, evolution::VERSION_KEY)
            .put_scalar(doc, ScalarValue::Uint(2))
            .unwrap();
        doc.commit();
        let mut old = doc.fork();
        assert!(
            evolution::read_checked::<Firewall>(&old).is_err(),
            "unmigrated"
        );
        assert!(evolution::migrate::<Firewall>(&mut old).unwrap());
        assert_eq!(evolution::read_checked::<Firewall>(&old).unwrap(), expected);
        for rule in flattened {
            let (_, matcher) = old.get(&rule, "matcher").unwrap().unwrap();
            assert!(
                old.keys(&matcher)
                    .all(|key| key == "$variant" || key.starts_with("$field/")),
                "a match's fields are stored by its variant"
            );
        }
    }

    /// The configuration still to apply is the stored one until this device
    /// applied exactly it; an empty or invalid one is never applied.
    #[test]
    fn a_device_applies_each_stored_configuration_once() {
        let config = sample_config().canonical();
        let pending = pending_apply(&config, None);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].1, config);

        let applied = config.to_json();
        assert!(pending_apply(&config, Some(&applied)).is_empty());

        let mut changed = config.clone();
        changed.panic_mode = true;
        assert_eq!(pending_apply(&changed, Some(&applied)).len(), 1);

        assert!(pending_apply(&FirewallConfig::empty(), None).is_empty());
        let mut invalid = config;
        invalid.zones[0].services.push("bad service".into());
        assert!(pending_apply(&invalid, None).is_empty());
    }

    /// The published root offers start/stop and panic as document edits
    /// bound to the stored state, and apply/retry as effects whose
    /// availability follows whether there is a configuration. Once there
    /// is one, this device applies it (planned only in a unit test) and
    /// remembers that it did.
    #[test]
    fn stored_configuration_is_applied_on_the_device_and_toggles_are_edits() {
        let empty = ui::tilo_resource::publish::<Firewall>(
            "firewall.config",
            "Firewall",
            &Firewall::default(),
        );
        assert!(
            empty
                .actions
                .iter()
                .filter(|action| ["start", "stop"].contains(&action.id.as_str()))
                .all(|action| !action.contract.availability.is_available()),
            "nothing to start or stop before there is a configuration"
        );

        let mut app = TestApp::<FirewallShell>::new().unwrap();
        app.invoke(Firewall::import_of(&sample_config())).unwrap();
        let surface = app.agent_surface();
        for id in ["stop", "enter-panic-mode"] {
            assert_eq!(
                action(&surface, id).unwrap()["operation"],
                "Document",
                "{id}"
            );
        }
        assert_eq!(
            action(&surface, "apply-config").unwrap()["operation"],
            "Effect"
        );
        assert_eq!(action(&surface, "retry").unwrap()["operation"], "Effect");
        let retry = app.effect(&FirewallEffect::Retry).unwrap_err();
        assert!(
            matches!(&retry, tilo_actions::DoorError::Unavailable { code, .. } if code == "connected"),
            "{retry:?}"
        );

        let remembered = app.person::<AppliedConfig>().unwrap();
        let stored = app.state().config().to_json();
        let applied = app.harness().wait_until(tilo_test_env::hang_bound(), |_| {
            remembered.with(|applied| applied.on("test-device") == Some(stored.as_str()))
        });
        assert!(applied, "the device remembers the configuration it applied");

        // Stopping the firewall is an edit like any other: confirmed, in
        // the document, and applied by the device in turn.
        app.invoke_confirmed(Firewall::STOP.invoke()).unwrap();
        assert!(!app.state().enabled);
        let stopped = app.state().config().to_json();
        let reapplied = app.harness().wait_until(tilo_test_env::hang_bound(), |_| {
            remembered.with(|applied| applied.on("test-device") == Some(stopped.as_str()))
        });
        assert!(reapplied, "the edit reached this device's firewall");
    }

    /// The palette row and the confirmation's title name the switch by the
    /// first line of its doc alone; why it asks first stays in the
    /// description, never runs on into the name.
    #[test]
    fn the_traffic_cutting_switches_are_named_by_what_they_do() {
        let published = ui::tilo_resource::publish::<Firewall>(
            "firewall.config",
            "Firewall",
            &Firewall::default(),
        );
        let spec = |id: &str| {
            published
                .actions
                .iter()
                .find(|action| action.id == id)
                .unwrap_or_else(|| panic!("{id} is published"))
        };
        for (id, label) in [
            ("stop", "Stop firewall"),
            (
                "enter-panic-mode",
                "Enter panic mode: drop all traffic in and out",
            ),
        ] {
            let spec = spec(id);
            assert_eq!(spec.label, label, "{id}");
            assert!(
                spec.description
                    .as_deref()
                    .is_some_and(|text| text.contains("asked first")),
                "{id}: why it asks stays in the description"
            );
        }
    }

    /// Every screenshot is the sample ruleset staged through actions, with
    /// the view state its scenario names; the service error shows no
    /// configuration at all.
    #[test]
    fn captures_stage_the_sample_through_actions() {
        let mut active = TestApp::<FirewallShell>::capture(Some("active-change")).unwrap();
        assert_eq!(active.harness().inspection().viewport, [1280, 800]);
        assert_eq!(active.state().default_zone, ZoneKind::Home);
        let frame = active.harness().inspection_json().unwrap();
        assert!(frame.contains("Home is now the active zone"), "{frame}");

        let mut narrow = TestApp::<FirewallShell>::capture(Some("narrow")).unwrap();
        assert_eq!(narrow.harness().inspection().viewport, [800, 600]);

        let panic = TestApp::<FirewallShell>::capture(Some("panic")).unwrap();
        assert!(panic.state().panic_mode);
        let stopped = TestApp::<FirewallShell>::capture(Some("stopped")).unwrap();
        assert!(!stopped.state().enabled);

        let mut error = TestApp::<FirewallShell>::capture(Some("service-error")).unwrap();
        assert!(error.state().zones.is_empty());
        let frame = error.harness().inspection_json().unwrap();
        assert!(
            frame.contains("Permission to read firewalld was denied"),
            "{frame}"
        );
    }

    fn imported(config: &FirewallConfig) -> ui::tilo_resource::Replica<Firewall> {
        let mut replica = ui::tilo_resource::Replica::<Firewall>::new();
        replica.apply(Firewall::import_of(config)).unwrap();
        replica
    }

    fn edit(
        replica: &mut ui::tilo_resource::Replica<Firewall>,
        edit: &ui::tilo_resource::Edit<Firewall>,
    ) -> Result<(), String> {
        replica
            .apply(Invoke::edit(edit).map_err(|error| error.to_string())?)
            .map(drop)
            .map_err(|error| error.to_string())
    }

    /// The record reads back as the canonical configuration, and an import
    /// removes the members and rules firewalld no longer reports.
    #[test]
    fn the_record_round_trips_and_an_import_reconciles() {
        let config = sample_config();
        let replica = imported(&config);
        assert_eq!(replica.state().unwrap().config(), config.canonical());

        let zone_kind = config.default_zone;
        let stale_rule = config.zone(zone_kind).unwrap().rules[0].id;
        let mut probe = config.clone();
        let zone = probe.zone_mut(zone_kind).unwrap();
        zone.services.retain(|service| service != "ssh");
        zone.interfaces.retain(|interface| interface != "eth0");
        zone.rules.retain(|rule| rule.id != stale_rule);
        let mut replica = replica;
        replica.apply(Firewall::import_of(&probe)).unwrap();
        let restored = replica.state().unwrap().config();
        let zone = restored.zone(zone_kind).unwrap();
        assert!(!zone.has_service("ssh"));
        assert!(!zone.interfaces.iter().any(|interface| interface == "eth0"));
        assert!(!zone.rules.iter().any(|rule| rule.id == stale_rule));

        // An import of the old configuration does not bring the rule back.
        replica.apply(Firewall::import_of(&config)).unwrap();
        let again = replica.state().unwrap().config();
        assert!(
            !again
                .zone(zone_kind)
                .unwrap()
                .rules
                .iter()
                .any(|rule| rule.id == stale_rule)
        );
    }

    /// Concurrent edits to independent fields, rules and services merge; a
    /// deletion wins over a concurrent edit of the rule.
    #[test]
    fn concurrent_edits_merge_per_field_and_delete_wins() {
        let config = sample_config();
        let z = config.default_zone;
        let ids: Vec<_> = config.zone(z).unwrap().rules.iter().map(|r| r.id).collect();
        let rules = Firewall::paths().zones.key(z.name()).rules;
        let rule = |id| rules.item(&Firewall::rule(id));

        let mut a = imported(&config);
        let mut b = a.fork();
        edit(&mut a, &rule(ids[0]).source.replace("10.0.0.0/8")).unwrap();
        edit(&mut b, &rule(ids[0]).enabled.set(false)).unwrap();
        edit(&mut b, &rule(ids[1]).enabled.set(false)).unwrap();
        let services = Firewall::paths().zones.key(z.name()).services;
        edit(&mut a, &services.insert(&"https".to_owned())).unwrap();
        edit(&mut b, &services.insert(&"dns".to_owned())).unwrap();
        a.sync_with(&mut b).unwrap();
        assert_eq!(a.state().unwrap(), b.state().unwrap());
        let merged = a.state().unwrap().config();
        let zone = merged.zone(z).unwrap();
        let first = zone.rules.iter().find(|r| r.id == ids[0]).unwrap();
        assert_eq!(first.source, "10.0.0.0/8");
        assert!(!first.enabled);
        assert!(!zone.rules.iter().find(|r| r.id == ids[1]).unwrap().enabled);
        assert!(zone.has_service("https") && zone.has_service("dns"));

        let mut a = imported(&config);
        let mut b = a.fork();
        a.apply(Firewall::DELETE_RULE.with((z, Firewall::rule(ids[0]))))
            .unwrap();
        edit(&mut b, &rule(ids[0]).source.replace("192.168.1.0/24")).unwrap();
        a.sync_with(&mut b).unwrap();
        assert!(
            !a.state()
                .unwrap()
                .config()
                .zone(z)
                .unwrap()
                .rules
                .iter()
                .any(|r| r.id == ids[0])
        );
    }

    /// The stored configuration is held to what firewalld accepts, however
    /// it is changed; an agent edits it in one call; history says what
    /// changed.
    #[test]
    fn edits_are_held_to_the_configuration_rules_and_history_names_them() {
        let mut app = TestApp::<FirewallShell>::new().unwrap();
        app.invoke(Firewall::import_of(&sample_config())).unwrap();
        let public = Firewall::paths().zones.key(ZoneKind::Public.name());
        assert!(
            app.invoke(Invoke::edit(&public.services.insert(&"bad service".to_owned())).unwrap())
                .is_err(),
            "a malformed service name is refused"
        );
        let zone = |zone| serde_json::json!({ "edit": Firewall::paths().default_zone.set(zone) });
        assert!(
            matches!(
                app.call("field-edit", zone(ZoneKind::Home)),
                Err(tilo_actions::DoorError::ConfirmationRequired)
            ),
            "a more trusted active zone weakens the firewall, so it asks"
        );
        app.call("field-edit", zone(ZoneKind::Block))
            .expect("a stricter zone asks nothing");
        app.call_confirmed("field-edit", zone(ZoneKind::Home))
            .unwrap();
        let catalogue =
            tilo_history::Catalogue::new("en-US", &[include_str!("../locales/en-US.ftl")]);
        let reader = tilo_history::Reader::new("en-US").with_app(&catalogue);
        let sentences: Vec<String> = app
            .history()
            .into_iter()
            .filter(|entry| entry.what.is_some())
            .map(|entry| reader.sentence(&entry))
            .collect();
        assert_eq!(
            sentences.last().map(String::as_str),
            Some("Made home the active zone")
        );
    }

    /// An agent deletes a rule naming its zone as the zone's JSON name.
    #[test]
    fn an_agent_deletes_a_rule_naming_its_zone() {
        let config = sample_config();
        let mut app = TestApp::<FirewallShell>::new().unwrap();
        app.invoke(Firewall::import_of(&config)).unwrap();
        let rule = config.zone(ZoneKind::Public).unwrap().rules[0].id;
        assert!(
            app.call(
                "delete-rule",
                serde_json::json!({ "zone": "dmz", "rule": Firewall::rule(rule) }),
            )
            .is_err(),
            "the rule is not in that zone"
        );
        app.call(
            "delete-rule",
            serde_json::json!({ "zone": "public", "rule": Firewall::rule(rule) }),
        )
        .unwrap();
        let public = app.state().config();
        let public = public.zone(ZoneKind::Public).unwrap();
        assert!(public.rules.iter().all(|stored| stored.id != rule));
        let catalogue =
            tilo_history::Catalogue::new("en-US", &[include_str!("../locales/en-US.ftl")]);
        let reader = tilo_history::Reader::new("en-US").with_app(&catalogue);
        let last = app.history().pop().map(|entry| reader.sentence(&entry));
        assert_eq!(
            last.as_deref(),
            Some("Deleted a rule from the “public” zone")
        );
    }
}

ui::reach_tests!(FirewallShell);

#[cfg(test)]
mod window_bar_tests {
    /// The window bar's sidebar button flips the sidebar on every click, in a
    /// window wide enough to dock it and in a narrow one.
    #[test]
    fn the_window_bars_sidebar_button_flips_the_sidebar() {
        ui::framework::testing::check_window_sidebar_button(|| {
            ui::framework::testing::TestApp::<super::FirewallShell>::drive("add-rule")
        })
        .unwrap_or_else(|error| panic!("{error}"));
    }
}
